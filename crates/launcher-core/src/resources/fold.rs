//! The pure part of measuring a process tree: which processes belong to it, and how the
//! counters of its live and departed processes add up to totals that stay continuous across
//! exits. Tested on literal process tables.
//!
//! When a parent reaps a child, Linux moves the child's whole CPU time into the parent's
//! rollup (`cutime` and `cstime`) and its I/O into the parent's counts. That is also the moment
//! the child stops being a live member, so the total stays continuous when each live member
//! counts its own time plus its rollup:
//!
//! ```text
//! total = Σ over live members (own + rollup) + lost
//! ```
//!
//! A departed member whose time is not in a live member's rollup moves its last-seen counters
//! to `lost`:
//!
//! - A member with no live parent in the tree (the root, reaped by Zeal, or an orphan reaped
//!   by init) goes to `lost` at once; at most its last interval is missed.
//! - A member whose parent is live was reaped by it. The scan is not atomic, so the parent may
//!   have been read just before the reap: the member waits one more reading, and goes to
//!   `lost` only if the parent's rollup still has not grown by its total. That happens when
//!   the parent ignores `SIGCHLD`, so the kernel reaps the child without folding it in.
//! - A member that left together with its parent, which reaped it before being reaped itself
//!   (a shell that runs a command, both done within one interval), is checked against the
//!   nearest live ancestor's rollup, shallowest first: that rollup holds the parent's total,
//!   and the parent's holds the child's.
//! - A member waiting on a parent that then leaves moves up the same way, to the parent's
//!   live ancestor, and settles after the parent's own departure, waiting one more reading if
//!   it must: the parent's final total holds the member's.
//!
//! A member missing from readings and back within [`RETURN_READINGS`] had not left: XNU may
//! refuse to answer for a process while it tears it down, before it is a zombie, and that can
//! outlast a reading. What it lost is taken back, and a departure waiting for it is dropped. A
//! departure beneath it that moved up past it while it was missing, or went to `lost` then,
//! anchors on it again, with a fresh grace, and claims what its rollup gained since it was
//! last seen: its final total holds the departure's. What it claimed of an ancestor's growth
//! while it was missing was another member's, and is given back to that ancestor for the
//! departures waiting there. Until then the reader looks for it by its identity, which names
//! one process, so what is kept can only be matched by that process and costs only memory.
//!
//! Members that waited claim an ancestor's growth before those that just left, since their
//! reap may be what the reading shows. A member that could not claim although its parent's
//! departure did, in the same reading, claims last: the parent's total reached the rollup
//! without it, so the parent did not reap it (an orphan, reaped by init), and the growth left
//! belongs to its siblings.
//!
//! macOS rolls a reaped child's CPU time into its parent the same way (`ri_child_user_time`
//! and `ri_child_system_time`), but not its I/O: a member that leaves moves the bytes it was
//! last seen with to `lost` at once. On Windows the job keeps the CPU and I/O totals of every
//! process ever in it, so the fold takes those and tracks no departures.
//!
//! The published totals never decrease within a run, which hides the one-reading dip of a torn
//! read.

use std::{
    collections::{HashMap, HashSet},
    time::Instant,
};

use launcher_types::{MAX_MEMBERS, NodeResources, Platform, ProcessUsage};

use super::{Identity, JobTotals, MemberReading, TreeReading};
use crate::process::TreeProbe;

/// CPU seconds below which two figures count as equal: under one clock tick.
const EPSILON: f64 = 0.001;

/// A run's first rate is averaged over at least this long, so a reading taken right at the
/// spawn does not turn one clock tick of CPU into many cores.
const MIN_FIRST_RATE_SECS: f64 = 1.0;

/// How far up a chain of departed members the fold looks for a live ancestor.
const MAX_CHAIN: u32 = 64;

/// For how many readings what the fold did with a member that left is kept, in case the member
/// is back: a minute at the 5 s interval.
const RETURN_READINGS: u64 = 12;

/// One process of a scan of the computer, as membership needs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcEntry {
    pub pid: u32,
    pub ppid: u32,
    pub pgrp: u32,
    /// The start time in the OS's unit.
    pub start: u64,
}

impl ProcEntry {
    fn id(&self) -> Identity {
        Identity {
            pid: self.pid,
            start: self.start,
        }
    }
}

/// The members of a tree among `entries`, in tree order: each member before its children,
/// children by start time, and the root first. A process is a member when it is
///
/// - the `root`, when its identity is known;
/// - in the process group `group` (the root's pid);
/// - a child of a member that started no earlier than that member, which rejects a parent
///   link to a pid that was reused;
/// - `sticky`: seen as a member before and still alive with the same start, so it stays one
///   after it is orphaned and re-parented to init or a subreaper.
///
/// Kernel threads are in group 0 and are children of pid 2, so they never qualify.
#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
pub fn select(
    entries: &[ProcEntry],
    group: u32,
    root: Option<Identity>,
    sticky: &[Identity],
) -> Vec<Identity> {
    let mut members: Vec<ProcEntry> = entries
        .iter()
        .filter(|entry| entry.pid != 0)
        .filter(|entry| {
            root == Some(entry.id())
                || (group != 0 && entry.pgrp == group)
                || sticky.contains(&entry.id())
        })
        .copied()
        .collect();
    let mut seen: HashSet<Identity> = members.iter().map(ProcEntry::id).collect();
    let mut children: HashMap<u32, Vec<&ProcEntry>> = HashMap::new();
    for entry in entries.iter().filter(|entry| entry.pid != 0) {
        children.entry(entry.ppid).or_default().push(entry);
    }
    let mut queue: Vec<ProcEntry> = members.clone();
    while let Some(parent) = queue.pop() {
        for child in children.get(&parent.pid).into_iter().flatten() {
            if child.start >= parent.start && seen.insert(child.id()) {
                members.push(**child);
                queue.push(**child);
            }
        }
    }
    tree_order(&members, root)
}

/// The parent of `member` among `members`: the one with its parent pid that started no later.
fn parent_entry<'a>(members: &'a [ProcEntry], member: &ProcEntry) -> Option<&'a ProcEntry> {
    members.iter().find(|parent| {
        parent.pid == member.ppid && parent.pid != member.pid && parent.start <= member.start
    })
}

/// `members` in tree order: the processes without a member parent (the root first, then by
/// start), each followed by its children, by start.
fn tree_order(members: &[ProcEntry], root: Option<Identity>) -> Vec<Identity> {
    let key = |entry: &ProcEntry| (Some(entry.id()) != root, entry.start, entry.pid);
    let mut tops: Vec<&ProcEntry> = members
        .iter()
        .filter(|member| parent_entry(members, member).is_none())
        .collect();
    tops.sort_by_key(|entry| key(entry));
    let mut order = Vec::with_capacity(members.len());
    let mut visited = HashSet::new();
    let mut stack: Vec<&ProcEntry> = tops.into_iter().rev().collect();
    while let Some(entry) = stack.pop() {
        if !visited.insert(entry.id()) {
            continue;
        }
        order.push(entry.id());
        let mut kids: Vec<&ProcEntry> = members
            .iter()
            .filter(|child| parent_entry(members, child).is_some_and(|p| p.id() == entry.id()))
            .collect();
        kids.sort_by_key(|entry| key(entry));
        stack.extend(kids.into_iter().rev());
    }
    // A parent loop, which only a torn scan could show, is listed rather than dropped.
    order.extend(
        members
            .iter()
            .map(ProcEntry::id)
            .filter(|id| !visited.contains(id)),
    );
    order
}

/// Where the counters of a member that left go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rollup {
    /// Into the parent that reaps it, whose rollup or counts then hold its total: Linux's CPU
    /// and I/O, macOS's CPU.
    IntoParent,
    /// Into `lost` at once, as last seen: macOS's I/O, which XNU does not roll up.
    LostOnExit,
    /// Nowhere: the job's totals hold every process ever in it (Windows).
    JobTotal,
}

impl Rollup {
    /// The rules for CPU and for I/O on `platform`.
    fn of(platform: Platform) -> (Rollup, Rollup) {
        match platform {
            Platform::Linux => (Rollup::IntoParent, Rollup::IntoParent),
            Platform::Macos => (Rollup::IntoParent, Rollup::LostOnExit),
            Platform::Windows => (Rollup::JobTotal, Rollup::JobTotal),
        }
    }
}

/// Counters that add up across members.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Counters {
    cpu: f64,
    read: u64,
    written: u64,
}

impl Counters {
    /// Takes `counters` back out of these, which hold them.
    fn take_back(&mut self, counters: &Counters) {
        self.cpu = (self.cpu - counters.cpu).max(0.0);
        self.read = self.read.saturating_sub(counters.read);
        self.written = self.written.saturating_sub(counters.written);
    }
}

/// A member as the previous reading left it.
#[derive(Clone, Debug, PartialEq)]
struct Seen {
    parent: Option<Identity>,
    /// Own CPU plus rollup.
    cpu: f64,
    rollup: f64,
    read: Option<u64>,
    written: Option<u64>,
    memory: Option<u64>,
    committed: Option<u64>,
}

/// A member that left, whose time should be in a live ancestor's rollup.
#[derive(Clone, Debug, PartialEq)]
struct Departed {
    id: Identity,
    /// Its parent when it was last seen.
    parent: Option<Identity>,
    /// The nearest live ancestor when it left.
    anchor: Identity,
    /// 1 for a child of the anchor, 2 for a grandchild, and so on.
    depth: u32,
    cpu: f64,
    read: Option<u64>,
    written: Option<u64>,
    /// It has already waited one reading.
    waited: bool,
    /// It could not claim although its parent's departure claimed in the same reading, so
    /// its parent did not reap it: an orphan, reaped outside the tree.
    suspect: bool,
}

/// What went to `lost` for a member, at which reading, in case the member or an ancestor is
/// back.
#[derive(Clone, Debug, PartialEq)]
struct Loss {
    id: Identity,
    at: u64,
    /// The member's parent as last seen, when an ancestor's rollup may hold what was lost:
    /// `None` for macOS bytes, which no rollup holds.
    parent: Option<Identity>,
    counters: Counters,
}

/// CPU time a departure claimed of its anchor's rollup growth, at which reading, in case the
/// member is back: then it was not reaped, and the growth was another member's.
#[derive(Clone, Debug, PartialEq)]
struct Claim {
    id: Identity,
    anchor: Identity,
    at: u64,
    cpu: f64,
}

/// One run's measurement: the members last seen, the counters of the departed, and the
/// published totals.
#[derive(Debug)]
pub(crate) struct Fold {
    run: u64,
    root_pid: u32,
    spawned: Instant,
    platform: Platform,
    /// Where a departed member's CPU time and I/O go.
    cpu_rollup: Rollup,
    io_rollup: Rollup,
    /// The root's identity, once a reading found it: the root is known by it from then on.
    root: Option<Identity>,
    last: HashMap<Identity, Seen>,
    pending: Vec<Departed>,
    /// Rollup growth of an anchor that its waiting departures may still claim.
    leftover: HashMap<Identity, f64>,
    lost: Counters,
    /// The readings folded, which date what is kept for a member that may be back.
    readings: u64,
    /// What went to `lost` in the last [`RETURN_READINGS`] readings, by member.
    losses: Vec<Loss>,
    /// What departures claimed in the last [`RETURN_READINGS`] readings, by member.
    claims: Vec<Claim>,
    /// The members that left in the last [`RETURN_READINGS`] readings, with the reading they
    /// left at and as last seen.
    absent: HashMap<Identity, (u64, Seen)>,
    published: Counters,
    /// Whether any member's I/O was readable this run.
    io_seen: bool,
    peak: u64,
    /// The previous reading's time and CPU total, for the rate.
    previous: Option<(Instant, f64)>,
    rate: Option<f64>,
}

impl Fold {
    pub(crate) fn new(probe: &TreeProbe, platform: Platform) -> Self {
        let (cpu_rollup, io_rollup) = Rollup::of(platform);
        Fold {
            run: probe.run,
            root_pid: probe.pid,
            spawned: probe.spawned,
            platform,
            cpu_rollup,
            io_rollup,
            root: None,
            last: HashMap::new(),
            pending: Vec::new(),
            leftover: HashMap::new(),
            lost: Counters::default(),
            readings: 0,
            losses: Vec::new(),
            claims: Vec::new(),
            absent: HashMap::new(),
            published: Counters::default(),
            io_seen: false,
            peak: 0,
            previous: None,
            rate: None,
        }
    }

    /// The identities seen as members before, for the reader: the last reading's members, those
    /// that left in the last [`RETURN_READINGS`] readings, and the root, which stays known by
    /// its identity after it exits.
    pub(crate) fn sticky(&self) -> Vec<Identity> {
        let mut sticky: Vec<Identity> = self
            .last
            .keys()
            .chain(self.absent.keys())
            .chain(&self.root)
            .copied()
            .collect();
        sticky.sort_by_key(|id| (id.pid, id.start));
        sticky.dedup();
        sticky
    }

    /// Folds a reading into the run's totals and returns the sample to publish.
    pub(crate) fn fold(&mut self, reading: TreeReading) -> NodeResources {
        let TreeReading {
            at,
            mono,
            members,
            denied,
            job,
        } = reading;
        if self.root.is_none() {
            self.root = members
                .iter()
                .find(|member| member.id.pid == self.root_pid)
                .map(|member| member.id);
        }
        let index: HashMap<Identity, usize> = members
            .iter()
            .enumerate()
            .map(|(i, member)| (member.id, i))
            .collect();
        let parents: Vec<Option<Identity>> = members
            .iter()
            .map(|member| parent_of(&members, member))
            .collect();
        self.readings += 1;
        if self.cpu_rollup != Rollup::JobTotal {
            self.settle_departures(&members, &index);
        }

        let mut totals = self.lost;
        let mut memory = 0u64;
        let mut committed: Option<u64> = None;
        let mut member_peak = 0u64;
        let mut threads: Option<u32> = None;
        let mut descriptors: Option<u32> = None;
        let mut last = HashMap::with_capacity(members.len());
        for (member, parent) in members.iter().zip(&parents) {
            // A field the OS refused this time keeps its last value.
            let before = self.last.get(&member.id);
            let read = member.read.or_else(|| before.and_then(|seen| seen.read));
            let written = member
                .written
                .or_else(|| before.and_then(|seen| seen.written));
            let member_memory = member
                .memory
                .or_else(|| before.and_then(|seen| seen.memory));
            let member_committed = member
                .committed
                .or_else(|| before.and_then(|seen| seen.committed));
            let cpu = member.cpu_own + member.cpu_rollup;
            totals.cpu += cpu;
            totals.read += read.unwrap_or(0);
            totals.written += written.unwrap_or(0);
            self.io_seen |= read.is_some() || written.is_some();
            memory = memory.saturating_add(member_memory.unwrap_or(0));
            if let Some(bytes) = member_committed {
                committed = Some(committed.unwrap_or(0).saturating_add(bytes));
            }
            member_peak = member_peak.max(member.peak.unwrap_or(0));
            if let Some(count) = member.threads {
                threads = Some(threads.unwrap_or(0).saturating_add(count));
            }
            if let Some(count) = member.descriptors {
                descriptors = Some(descriptors.unwrap_or(0).saturating_add(count));
            }
            last.insert(
                member.id,
                Seen {
                    parent: *parent,
                    cpu,
                    rollup: member.cpu_rollup,
                    read,
                    written,
                    memory: member_memory,
                    committed: member_committed,
                },
            );
        }
        self.last = last;
        if self.cpu_rollup == Rollup::JobTotal {
            // The members' sum misses the processes that exited; a reading without the job's
            // totals keeps the last ones.
            totals = job.map_or(self.published, |job: JobTotals| Counters {
                cpu: job.cpu,
                read: job.read,
                written: job.written,
            });
            self.io_seen |= job.is_some();
        }
        self.published.cpu = self.published.cpu.max(totals.cpu);
        self.published.read = self.published.read.max(totals.read);
        self.published.written = self.published.written.max(totals.written);
        self.peak = self.peak.max(memory).max(member_peak);

        let run_secs = mono.saturating_duration_since(self.spawned).as_secs_f64();
        if denied.cpu {
            // No rate from times that were not read.
            self.rate = None;
            self.previous = None;
        } else {
            self.rate = match self.previous {
                Some((then, cpu)) => {
                    let secs = mono.saturating_duration_since(then).as_secs_f64();
                    if secs > 0.0 {
                        Some((self.published.cpu - cpu) / secs)
                    } else {
                        self.rate
                    }
                }
                None => Some(self.published.cpu / run_secs.max(MIN_FIRST_RATE_SECS)),
            };
            self.previous = Some((mono, self.published.cpu));
        }

        let usage = members
            .iter()
            .zip(&parents)
            .take(MAX_MEMBERS)
            .map(|(member, parent)| ProcessUsage {
                pid: member.id.pid,
                parent: parent.map(|parent| parent.pid),
                name: member.name.clone(),
                started_at: member.started_at,
                cpu_secs: member.cpu_own,
                memory_bytes: member.memory,
                read_bytes: member.read,
                written_bytes: member.written,
                threads: member.threads,
            })
            .collect();
        NodeResources {
            run: self.run,
            at,
            run_secs,
            platform: self.platform,
            cpu_secs: self.published.cpu,
            cpu_cores: self.rate,
            memory_bytes: memory,
            peak_memory_bytes: self.peak,
            committed_bytes: committed,
            read_bytes: self.io_seen.then_some(self.published.read),
            written_bytes: self.io_seen.then_some(self.published.written),
            processes: u32::try_from(members.len()).unwrap_or(u32::MAX),
            threads,
            descriptors,
            members: usage,
        }
    }

    /// Moves the counters of the members that left since the last reading, and of those that
    /// waited a reading, to `lost` unless a live ancestor's rollup holds them.
    fn settle_departures(&mut self, members: &[MemberReading], index: &HashMap<Identity, usize>) {
        let live = |id: &Identity| index.contains_key(id);
        let now = self.readings;
        let kept = |at: u64| now - at < RETURN_READINGS;
        // A member that left and is back had not left.
        let lost = &mut self.lost;
        self.losses.retain(|loss| {
            if !live(&loss.id) {
                return kept(loss.at);
            }
            lost.take_back(&loss.counters);
            false
        });
        // What it claimed was another member's growth: the departures waiting on that anchor
        // may claim it.
        let leftover = &mut self.leftover;
        self.claims.retain(|claim| {
            if !live(&claim.id) {
                return kept(claim.at);
            }
            *leftover.entry(claim.anchor).or_insert(0.0) += claim.cpu;
            false
        });
        let mut pending = std::mem::take(&mut self.pending);
        pending.retain(|departure| !live(&departure.id));
        // A waiting departure whose anchor has left since: its time is in the anchor's final
        // total, so it moves up to the anchor's live ancestor and settles behind the anchor's
        // own departure, with the same one-reading grace.
        for departure in &mut pending {
            while !live(&departure.anchor) && departure.depth < MAX_CHAIN {
                match self
                    .last
                    .get(&departure.anchor)
                    .and_then(|seen| seen.parent)
                {
                    Some(up) => {
                        departure.anchor = up;
                        departure.depth += 1;
                        departure.waited = false;
                    }
                    None => break,
                }
            }
        }
        // A waiting departure that moved up past a member that is back: that member's final
        // total holds its time, so it anchors there again, with a fresh grace.
        let mut absent = std::mem::take(&mut self.absent);
        for departure in &mut pending {
            let mut up = departure.parent;
            let mut depth = 1;
            while let Some(id) = up.filter(|_| depth < departure.depth) {
                if live(&id) {
                    departure.anchor = id;
                    departure.depth = depth;
                    departure.waited = false;
                    break;
                }
                up = self
                    .last
                    .get(&id)
                    .or_else(|| absent.get(&id).map(|(_, seen)| seen))
                    .and_then(|seen| seen.parent);
                depth += 1;
            }
        }
        // A departure that went to `lost` while an ancestor was missing, and that ancestor is
        // back: the ancestor may have reaped it, so its final total may hold the departure's.
        // The departure waits on it again, with a fresh grace.
        for loss in std::mem::take(&mut self.losses) {
            let mut up = loss.parent;
            let mut depth = 1;
            let anchor = loop {
                match up {
                    Some(id) if live(&id) => {
                        break absent
                            .get(&id)
                            .filter(|(left, _)| loss.at >= *left)
                            .map(|_| id);
                    }
                    Some(id) if depth < MAX_CHAIN => {
                        up = self
                            .last
                            .get(&id)
                            .or_else(|| absent.get(&id).map(|(_, seen)| seen))
                            .and_then(|seen| seen.parent);
                        depth += 1;
                    }
                    _ => break None,
                }
            };
            let Some(anchor) = anchor else {
                self.losses.push(loss);
                continue;
            };
            self.lost.take_back(&loss.counters);
            pending.push(Departed {
                id: loss.id,
                parent: loss.parent,
                anchor,
                depth,
                cpu: loss.counters.cpu,
                read: Some(loss.counters.read),
                written: Some(loss.counters.written),
                waited: false,
                suspect: false,
            });
        }
        let mut departed: Vec<(&Identity, &Seen)> =
            self.last.iter().filter(|(id, _)| !live(id)).collect();
        departed.sort_by_key(|(id, _)| (id.pid, id.start));
        let gone: Vec<(Identity, (u64, Seen))> = departed
            .iter()
            .map(|&(id, seen)| (*id, (now, seen.clone())))
            .collect();
        let mut orphans = Vec::new();
        for (id, seen) in departed {
            // Where no parent's count holds a member's bytes, they are lost as it leaves, and
            // only its CPU time waits for a rollup.
            let (read, written) = if self.io_rollup == Rollup::LostOnExit {
                orphans.push((
                    *id,
                    None,
                    Counters {
                        cpu: 0.0,
                        read: seen.read.unwrap_or(0),
                        written: seen.written.unwrap_or(0),
                    },
                ));
                (None, None)
            } else {
                (seen.read, seen.written)
            };
            let mut parent = seen.parent;
            let mut depth = 1;
            let anchor = loop {
                match parent {
                    Some(up) if live(&up) => break Some(up),
                    Some(up) if depth < MAX_CHAIN => match self.last.get(&up) {
                        Some(above) => {
                            parent = above.parent;
                            depth += 1;
                        }
                        None => break None,
                    },
                    _ => break None,
                }
            };
            match anchor {
                Some(anchor) => pending.push(Departed {
                    id: *id,
                    parent: seen.parent,
                    anchor,
                    depth,
                    cpu: seen.cpu,
                    read,
                    written,
                    waited: false,
                    suspect: false,
                }),
                None => orphans.push((
                    *id,
                    seen.parent,
                    Counters {
                        cpu: seen.cpu,
                        read: read.unwrap_or(0),
                        written: written.unwrap_or(0),
                    },
                )),
            }
        }
        for (id, parent, counters) in orphans {
            self.lose(id, parent, counters);
        }

        // What each anchor's rollup gained since it was last seen, plus what its waiting
        // departures left unclaimed then.
        let mut credit: HashMap<Identity, f64> = HashMap::new();
        for departure in &pending {
            credit.entry(departure.anchor).or_insert_with(|| {
                let now = index.get(&departure.anchor).map(|&i| members[i].cpu_rollup);
                let before = self
                    .last
                    .get(&departure.anchor)
                    .or_else(|| absent.get(&departure.anchor).map(|(_, seen)| seen))
                    .map(|seen| seen.rollup);
                let grown = match (now, before) {
                    (Some(now), Some(before)) => (now - before).max(0.0),
                    _ => 0.0,
                };
                grown + self.leftover.get(&departure.anchor).copied().unwrap_or(0.0)
            });
        }
        // Waiting departures first, since their reap may be what this reading shows; then the
        // shallowest, whose fold carries the time of the children it reaped. Suspects last:
        // their time is in no rollup, so the growth is the others'.
        pending.sort_by_key(|departure| {
            (
                departure.suspect,
                !departure.waited,
                departure.depth,
                departure.id.pid,
                departure.id.start,
            )
        });
        let mut waiting = Vec::new();
        let mut claimed = HashSet::new();
        for departure in pending {
            let anchored = live(&departure.anchor);
            let credit = credit.entry(departure.anchor).or_insert(0.0);
            if anchored && departure.cpu <= *credit + EPSILON {
                let cpu = departure.cpu.min(*credit);
                *credit -= cpu;
                claimed.insert(departure.id);
                self.claims.push(Claim {
                    id: departure.id,
                    anchor: departure.anchor,
                    at: now,
                    cpu,
                });
            } else if anchored && !departure.waited {
                // The parent's total reached the anchor and left too little for this one: the
                // parent did not reap it. A suspect stays one after a move up the tree.
                let suspect = departure.suspect
                    || departure
                        .parent
                        .is_some_and(|parent| claimed.contains(&parent));
                waiting.push(Departed {
                    waited: true,
                    suspect,
                    ..departure
                });
            } else {
                self.lose(
                    departure.id,
                    departure.parent,
                    Counters {
                        cpu: departure.cpu,
                        read: departure.read.unwrap_or(0),
                        written: departure.written.unwrap_or(0),
                    },
                );
            }
        }
        self.leftover = waiting
            .iter()
            .map(|departure| {
                (
                    departure.anchor,
                    credit.get(&departure.anchor).copied().unwrap_or(0.0),
                )
            })
            .collect();
        self.pending = waiting;
        absent.retain(|id, (at, _)| !live(id) && kept(*at));
        absent.extend(gone);
        self.absent = absent;
    }

    fn lose(&mut self, id: Identity, parent: Option<Identity>, counters: Counters) {
        self.lost.cpu += counters.cpu;
        self.lost.read = self.lost.read.saturating_add(counters.read);
        self.lost.written = self.lost.written.saturating_add(counters.written);
        self.losses.push(Loss {
            id,
            at: self.readings,
            parent,
            counters,
        });
    }
}

/// The parent of `member` within a reading: the member with its parent pid that started no
/// later than it.
fn parent_of(members: &[MemberReading], member: &MemberReading) -> Option<Identity> {
    members
        .iter()
        .find(|parent| {
            parent.id.pid == member.ppid
                && parent.id != member.id
                && parent.id.start <= member.id.start
        })
        .map(|parent| parent.id)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::resources::Denied;

    fn id(pid: u32, start: u64) -> Identity {
        Identity { pid, start }
    }

    fn entry(pid: u32, ppid: u32, pgrp: u32, start: u64) -> ProcEntry {
        ProcEntry {
            pid,
            ppid,
            pgrp,
            start,
        }
    }

    #[test]
    fn members_come_from_the_group_parent_links_and_sticky_ids() {
        let root = id(100, 1000);
        let entries = [
            entry(1, 0, 1, 1),
            entry(2, 0, 0, 1),
            // A kernel thread.
            entry(50, 2, 0, 5),
            entry(100, 1, 100, 1000),
            // In the root's group.
            entry(101, 100, 100, 1100),
            // Left the group (a session of its own), found by its parent link.
            entry(102, 100, 102, 1200),
            // Its child, also outside the group.
            entry(103, 102, 102, 1300),
            // An unrelated process.
            entry(200, 1, 200, 900),
            // Orphaned and re-parented to init, but seen before: sticky.
            entry(104, 1, 104, 1400),
            // Re-parented too, never seen: missed.
            entry(105, 1, 105, 1500),
        ];
        let members = select(&entries, 100, Some(root), &[id(104, 1400)]);
        assert_eq!(
            members,
            vec![
                root,
                id(101, 1100),
                id(102, 1200),
                id(103, 1300),
                id(104, 1400)
            ],
            "tree order: the root, its children by start, then other tops"
        );
    }

    #[test]
    fn a_parent_link_to_a_reused_pid_is_rejected() {
        // 300 started before member 102, which now has the pid 300's parent had: the link
        // was read across a pid reuse.
        let entries = [
            entry(100, 1, 100, 1000),
            entry(102, 100, 102, 5000),
            entry(300, 102, 300, 4000),
        ];
        let members = select(&entries, 100, Some(id(100, 1000)), &[]);
        assert_eq!(members, vec![id(100, 1000), id(102, 5000)]);
        // A sticky id whose pid now names another process is not a member either.
        let members = select(&entries, 100, Some(id(100, 1000)), &[id(300, 3999)]);
        assert!(!members.contains(&id(300, 4000)));
    }

    #[test]
    fn a_missing_root_still_finds_its_group_and_sticky_members() {
        let entries = [entry(101, 1, 100, 1100), entry(104, 1, 104, 1400)];
        let members = select(&entries, 100, None, &[id(104, 1400)]);
        assert_eq!(members, vec![id(101, 1100), id(104, 1400)]);
        assert!(
            select(&entries, 0, None, &[]).is_empty(),
            "group 0 is nobody's"
        );
    }

    // The fold, on literal readings 5 s apart.

    const SPAWN_SECS: u64 = 1_800_000_000;

    struct Clock {
        spawned: Instant,
    }

    impl Clock {
        fn probe(&self, run: u64) -> TreeProbe {
            TreeProbe {
                run,
                pid: 100,
                spawned_at: UNIX_EPOCH + Duration::from_secs(SPAWN_SECS),
                spawned: self.spawned,
                #[cfg(windows)]
                job: std::sync::Weak::new(),
            }
        }

        fn reading(&self, tick: u64, members: Vec<MemberReading>) -> TreeReading {
            TreeReading {
                at: UNIX_EPOCH + Duration::from_secs(SPAWN_SECS + 5 * tick),
                mono: self.spawned + Duration::from_secs(5 * tick),
                members,
                denied: Default::default(),
                job: None,
            }
        }
    }

    fn clock() -> Clock {
        Clock {
            spawned: Instant::now(),
        }
    }

    /// A member with CPU `own` and `rollup` seconds and `read` bytes read.
    fn member(pid: u32, ppid: u32, own: f64, rollup: f64, read: u64) -> MemberReading {
        MemberReading {
            id: id(pid, u64::from(pid) * 10),
            ppid,
            name: format!("p{pid}"),
            started_at: SystemTime::UNIX_EPOCH,
            cpu_own: own,
            cpu_rollup: rollup,
            memory: Some(1000),
            peak: Some(1500),
            committed: None,
            read: Some(read),
            written: Some(read * 2),
            threads: Some(2),
            descriptors: Some(10),
        }
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    /// Folds `readings`, 5 s apart, into a new run: the last sample, and the fold.
    fn fold_all(readings: Vec<Vec<MemberReading>>) -> (NodeResources, Fold) {
        fold_on(Platform::Linux, readings)
    }

    /// Like [`fold_all`], with `platform`'s rules.
    fn fold_on(platform: Platform, readings: Vec<Vec<MemberReading>>) -> (NodeResources, Fold) {
        let clock = clock();
        let mut fold = Fold::new(&clock.probe(1), platform);
        let mut last = None;
        for (tick, members) in (1..).zip(readings) {
            last = Some(fold.fold(clock.reading(tick, members)));
        }
        (last.expect("a reading"), fold)
    }

    #[test]
    fn a_reap_by_a_member_leaves_the_total_continuous() {
        // The worked example: a child uses 10 s before reading 1 and 1 s after it, then its
        // parent reaps it.
        let clock = clock();
        let mut fold = Fold::new(&clock.probe(1), Platform::Linux);
        let first = fold.fold(clock.reading(
            1,
            vec![
                member(100, 1, 100.0, 0.0, 0),
                member(101, 100, 10.0, 0.0, 70),
            ],
        ));
        assert!(close(first.cpu_secs, 110.0));
        assert_eq!(first.processes, 2);
        assert_eq!(first.members[1].parent, Some(100));
        assert_eq!(first.read_bytes, Some(70));
        // The root's own 5 s, and the child's last second in the root's rollup.
        let second = fold.fold(clock.reading(2, vec![member(100, 1, 105.0, 11.0, 80)]));
        assert!(close(second.cpu_secs, 116.0), "{}", second.cpu_secs);
        assert!(close(second.cpu_cores.unwrap(), 6.0 / 5.0));
        assert_eq!(second.processes, 1);
        assert_eq!(
            second.read_bytes,
            Some(80),
            "the child's I/O went into the parent's count"
        );
        let third = fold.fold(clock.reading(3, vec![member(100, 1, 110.0, 11.0, 80)]));
        assert!(
            close(third.cpu_secs, 121.0),
            "nothing counted twice: {}",
            third.cpu_secs
        );
    }

    #[test]
    fn a_torn_read_neither_dips_nor_counts_twice() {
        let clock = clock();
        let mut fold = Fold::new(&clock.probe(1), Platform::Linux);
        fold.fold(clock.reading(
            1,
            vec![
                member(100, 1, 100.0, 0.0, 0),
                member(101, 100, 10.0, 0.0, 0),
            ],
        ));
        // The child is gone, but the parent was read before the reap.
        let torn = fold.fold(clock.reading(2, vec![member(100, 1, 105.0, 0.0, 0)]));
        assert!(
            close(torn.cpu_secs, 110.0),
            "held, not dipped: {}",
            torn.cpu_secs
        );
        let after = fold.fold(clock.reading(3, vec![member(100, 1, 110.0, 11.0, 0)]));
        assert!(close(after.cpu_secs, 121.0), "{}", after.cpu_secs);
        assert!(close(fold.lost.cpu, 0.0), "nothing lost");
    }

    #[test]
    fn an_auto_reaped_child_waits_one_reading_then_is_lost() {
        let clock = clock();
        let mut fold = Fold::new(&clock.probe(1), Platform::Linux);
        fold.fold(clock.reading(
            1,
            vec![
                member(100, 1, 100.0, 0.0, 0),
                member(101, 100, 10.0, 0.0, 30),
            ],
        ));
        fold.fold(clock.reading(2, vec![member(100, 1, 105.0, 0.0, 0)]));
        assert!(close(fold.lost.cpu, 0.0), "still waiting");
        let third = fold.fold(clock.reading(3, vec![member(100, 1, 110.0, 0.0, 0)]));
        assert!(close(third.cpu_secs, 120.0), "{}", third.cpu_secs);
        assert_eq!(fold.lost.read, 30);
        assert_eq!(third.read_bytes, Some(30));
    }

    #[test]
    fn an_orphan_reaped_outside_the_tree_keeps_its_seen_part() {
        let clock = clock();
        let mut fold = Fold::new(&clock.probe(1), Platform::Linux);
        // 104 was re-parented to init: no live parent in the tree.
        fold.fold(clock.reading(
            1,
            vec![member(100, 1, 100.0, 0.0, 0), member(104, 1, 7.0, 1.0, 5)],
        ));
        let second = fold.fold(clock.reading(2, vec![member(100, 1, 105.0, 0.0, 0)]));
        assert!(close(second.cpu_secs, 113.0), "{}", second.cpu_secs);
        assert!(close(fold.lost.cpu, 8.0));
        assert_eq!(fold.lost.read, 5);
        // The root, reaped by Zeal, is lost the same way, and the total holds.
        let gone = fold.fold(clock.reading(3, Vec::new()));
        assert!(close(gone.cpu_secs, 113.0), "{}", gone.cpu_secs);
        assert_eq!(gone.processes, 0);
        assert_eq!(gone.memory_bytes, 0);
    }

    #[test]
    fn a_shell_and_its_command_leaving_together_count_once() {
        // zebrad (100) runs `sh -c cmd` (101), which forks cmd (102); within one interval sh
        // reaps cmd and zebrad reaps sh, so zebrad's rollup holds both.
        let clock = clock();
        let mut fold = Fold::new(&clock.probe(1), Platform::Linux);
        fold.fold(clock.reading(
            1,
            vec![
                member(100, 1, 100.0, 0.0, 0),
                member(101, 100, 1.0, 0.0, 0),
                member(102, 101, 2.0, 0.0, 0),
            ],
        ));
        // sh's final total: 1.5 own and cmd's 2.5 in its rollup.
        let second = fold.fold(clock.reading(2, vec![member(100, 1, 100.0, 4.0, 0)]));
        assert!(close(second.cpu_secs, 104.0), "{}", second.cpu_secs);
        assert!(close(fold.lost.cpu, 0.0));
        let third = fold.fold(clock.reading(3, vec![member(100, 1, 100.0, 4.0, 0)]));
        assert!(close(third.cpu_secs, 104.0), "{}", third.cpu_secs);

        // When sh did not reap cmd (it was orphaned, then reaped by init), only sh's part
        // reaches zebrad's rollup, and cmd's seen part is lost.
        let mut fold = Fold::new(&clock.probe(2), Platform::Linux);
        fold.fold(clock.reading(
            1,
            vec![
                member(100, 1, 100.0, 0.0, 0),
                member(101, 100, 1.0, 0.0, 0),
                member(102, 101, 2.0, 0.0, 0),
            ],
        ));
        fold.fold(clock.reading(2, vec![member(100, 1, 100.0, 1.5, 0)]));
        let settled = fold.fold(clock.reading(3, vec![member(100, 1, 100.0, 1.5, 0)]));
        assert!(close(settled.cpu_secs, 103.5), "{}", settled.cpu_secs);
        assert!(close(fold.lost.cpu, 2.0));
    }

    #[test]
    fn a_member_waiting_on_a_parent_that_leaves_moves_up() {
        // A torn read: sh (101) was read just before it reaped cmd (102), so cmd is gone and
        // waits on sh. Before the next reading zebrad reaps sh, whose final total holds cmd's:
        // sh's own 1.3 s and cmd's 2.3 s, and cmd's 50 bytes.
        let clock = clock();
        let mut fold = Fold::new(&clock.probe(1), Platform::Linux);
        fold.fold(clock.reading(
            1,
            vec![
                member(100, 1, 100.0, 0.0, 0),
                member(101, 100, 1.0, 0.0, 0),
                member(102, 101, 2.0, 0.0, 50),
            ],
        ));
        fold.fold(clock.reading(
            2,
            vec![member(100, 1, 100.0, 0.0, 0), member(101, 100, 1.2, 0.0, 0)],
        ));
        let third = fold.fold(clock.reading(3, vec![member(100, 1, 100.0, 3.6, 50)]));
        assert!(close(third.cpu_secs, 103.6), "{}", third.cpu_secs);
        assert_eq!(third.read_bytes, Some(50), "cmd's bytes once");
        assert!(close(fold.lost.cpu, 0.0));

        // The same with I/O on every member: sh's 500 bytes and cmd's 2,000 reach zebrad's
        // count with sh.
        let mut fold = Fold::new(&clock.probe(2), Platform::Linux);
        fold.fold(clock.reading(
            1,
            vec![
                member(100, 1, 100.0, 0.0, 100),
                member(101, 100, 1.0, 0.0, 500),
                member(102, 101, 2.0, 0.0, 2000),
            ],
        ));
        fold.fold(clock.reading(
            2,
            vec![
                member(100, 1, 100.0, 0.0, 100),
                member(101, 100, 1.5, 0.0, 500),
            ],
        ));
        for tick in 3..=4 {
            let sample = fold.fold(clock.reading(tick, vec![member(100, 1, 100.0, 4.0, 3100)]));
            assert!(close(sample.cpu_secs, 104.0), "{}", sample.cpu_secs);
            assert_eq!(sample.read_bytes, Some(3100));
        }
    }

    #[test]
    fn an_auto_reaped_child_whose_parent_leaves_is_lost_once() {
        // sh (101) ignores SIGCHLD, so the kernel reaps cmd (102) without folding it in. Then
        // zebrad reaps sh: its rollup grows by sh's 1.5 s and 600 bytes only. cmd moves up to
        // zebrad with a fresh grace, settles after sh, and its seen part is lost.
        let clock = clock();
        let mut fold = Fold::new(&clock.probe(1), Platform::Linux);
        fold.fold(clock.reading(
            1,
            vec![
                member(100, 1, 100.0, 0.0, 100),
                member(101, 100, 1.0, 0.0, 500),
                member(102, 101, 0.8, 0.0, 2000),
            ],
        ));
        fold.fold(clock.reading(
            2,
            vec![
                member(100, 1, 100.0, 0.0, 100),
                member(101, 100, 1.0, 0.0, 500),
            ],
        ));
        let mut last = None;
        for tick in 3..=5 {
            last = Some(fold.fold(clock.reading(tick, vec![member(100, 1, 100.0, 1.5, 700)])));
        }
        let settled = last.unwrap();
        assert!(close(settled.cpu_secs, 102.3), "{}", settled.cpu_secs);
        assert_eq!(settled.read_bytes, Some(2700));
        assert!(close(fold.lost.cpu, 0.8));
    }

    #[test]
    fn an_orphan_claims_after_a_sibling_its_ancestor_reaped() {
        // zebrad reaps sh (101), whose cmd (102) was orphaned and reaped by init: the rollup
        // grows by sh's 1.5 s only, and cmd waits. Then zebrad reaps a helper (103) seen at
        // 3.0 s, which ended at 3.2 s: that growth is the helper's, and cmd's seen 2.0 s is
        // lost.
        let (settled, fold) = fold_all(vec![
            vec![
                member(100, 1, 100.0, 0.0, 0),
                member(101, 100, 1.0, 0.0, 0),
                member(102, 101, 2.0, 0.0, 0),
            ],
            vec![member(100, 1, 100.0, 1.5, 0), member(103, 100, 3.0, 0.0, 0)],
            vec![member(100, 1, 100.0, 4.7, 0)],
            vec![member(100, 1, 100.0, 4.7, 0)],
        ]);
        assert!(close(settled.cpu_secs, 106.7), "{}", settled.cpu_secs);
        assert!(close(fold.lost.cpu, 2.0), "{}", fold.lost.cpu);
    }

    #[test]
    fn a_member_from_a_torn_read_claims_before_one_that_just_left() {
        // 101 leaves while zebrad was read before reaping it, and 102 leaves the same way one
        // reading later: each reap shows a reading late, and the waiting one claims first.
        let (settled, fold) = fold_all(vec![
            vec![
                member(100, 1, 100.0, 0.0, 0),
                member(101, 100, 10.0, 0.0, 0),
            ],
            vec![member(100, 1, 105.0, 0.0, 0), member(102, 100, 3.0, 0.0, 0)],
            vec![member(100, 1, 110.0, 11.0, 0)],
            vec![member(100, 1, 115.0, 14.5, 0)],
            vec![member(100, 1, 115.0, 14.5, 0)],
        ]);
        assert!(close(settled.cpu_secs, 129.5), "{}", settled.cpu_secs);
        assert!(close(fold.lost.cpu, 0.0), "{}", fold.lost.cpu);
    }

    #[test]
    fn an_orphan_claims_after_the_next_shell_and_its_command() {
        // zebrad reaps sh (101), and cmd (102), orphaned and reaped by init, waits. Then a
        // second sh (103) and its command (104) leave together, each reaped by its parent:
        // that growth is theirs, and cmd's seen 2.0 s is lost.
        let (settled, fold) = fold_all(vec![
            vec![
                member(100, 1, 100.0, 0.0, 0),
                member(101, 100, 1.0, 0.0, 0),
                member(102, 101, 2.0, 0.0, 0),
            ],
            vec![
                member(100, 1, 100.0, 1.5, 0),
                member(103, 100, 1.0, 0.0, 0),
                member(104, 103, 1.0, 0.0, 0),
            ],
            vec![member(100, 1, 100.0, 4.2, 0)],
            vec![member(100, 1, 100.0, 4.2, 0)],
        ]);
        assert!(close(settled.cpu_secs, 106.2), "{}", settled.cpu_secs);
        assert!(close(fold.lost.cpu, 2.0), "{}", fold.lost.cpu);
    }

    #[test]
    fn a_torn_read_and_an_orphan_at_the_same_depth() {
        // The first sh (101) and its command leave while zebrad was read before reaping sh, so
        // both wait; the second sh (103) leaves too, and its command (104) was orphaned. The
        // next reading holds both shells' totals: the first with its command, the second
        // without, so only the orphan's seen 1.5 s is lost.
        let (settled, fold) = fold_all(vec![
            vec![
                member(100, 1, 100.0, 0.0, 0),
                member(101, 100, 1.0, 0.0, 0),
                member(102, 101, 2.0, 0.0, 0),
            ],
            vec![
                member(100, 1, 100.0, 0.0, 0),
                member(103, 100, 1.0, 0.0, 0),
                member(104, 103, 1.5, 0.0, 0),
            ],
            vec![member(100, 1, 100.0, 5.2, 0)],
            vec![member(100, 1, 100.0, 5.2, 0)],
        ]);
        assert!(close(settled.cpu_secs, 106.7), "{}", settled.cpu_secs);
        assert!(close(fold.lost.cpu, 1.5), "{}", fold.lost.cpu);
    }

    #[test]
    fn an_orphan_stays_last_after_it_moves_up() {
        // 101 reaps 102, whose child 103 was orphaned and reaped by init: 103 waits on 101 as
        // an orphan. Then zebrad reaps 101, and a reading later a helper (105) seen at 3.0 s,
        // which ended at 3.2 s. 103 moves up to zebrad and still claims after the helper.
        let (settled, fold) = fold_all(vec![
            vec![
                member(100, 1, 100.0, 0.0, 0),
                member(101, 100, 1.0, 0.0, 0),
                member(102, 101, 1.0, 0.0, 0),
                member(103, 102, 2.0, 0.0, 0),
            ],
            vec![member(100, 1, 100.0, 0.0, 0), member(101, 100, 1.0, 1.2, 0)],
            vec![member(100, 1, 100.0, 2.3, 0), member(105, 100, 3.0, 0.0, 0)],
            vec![member(100, 1, 100.0, 5.5, 0)],
            vec![member(100, 1, 100.0, 5.5, 0)],
        ]);
        assert!(close(settled.cpu_secs, 107.5), "{}", settled.cpu_secs);
        assert!(close(fold.lost.cpu, 2.0), "{}", fold.lost.cpu);
    }

    #[test]
    fn on_macos_a_members_bytes_are_lost_as_it_leaves() {
        // The worked example on macOS: the child's CPU reaches its parent's rollup, but XNU
        // rolls up no I/O, so the child's 70 bytes stay as last seen.
        let clock = clock();
        let mut fold = Fold::new(&clock.probe(1), Platform::Macos);
        let first = fold.fold(clock.reading(
            1,
            vec![
                member(100, 1, 100.0, 0.0, 0),
                member(101, 100, 10.0, 0.0, 70),
            ],
        ));
        assert_eq!(first.read_bytes, Some(70));
        let second = fold.fold(clock.reading(2, vec![member(100, 1, 105.0, 11.0, 0)]));
        assert!(close(second.cpu_secs, 116.0), "{}", second.cpu_secs);
        assert_eq!(second.read_bytes, Some(70), "the child's bytes, once");
        assert_eq!(second.written_bytes, Some(140));
        assert!(close(fold.lost.cpu, 0.0), "its CPU is in the rollup");
        let third = fold.fold(clock.reading(3, vec![member(100, 1, 110.0, 11.0, 0)]));
        assert!(close(third.cpu_secs, 121.0), "{}", third.cpu_secs);
        assert_eq!(third.read_bytes, Some(70));

        // A torn read: the child is gone before its parent's rollup grew. Its bytes are lost at
        // once, and its CPU waits for the rollup as on Linux.
        let mut fold = Fold::new(&clock.probe(2), Platform::Macos);
        fold.fold(clock.reading(
            1,
            vec![
                member(100, 1, 100.0, 0.0, 10),
                member(101, 100, 10.0, 0.0, 70),
            ],
        ));
        let torn = fold.fold(clock.reading(2, vec![member(100, 1, 105.0, 0.0, 10)]));
        assert!(close(torn.cpu_secs, 110.0), "{}", torn.cpu_secs);
        assert_eq!(torn.read_bytes, Some(80));
        let after = fold.fold(clock.reading(3, vec![member(100, 1, 110.0, 11.0, 10)]));
        assert!(close(after.cpu_secs, 121.0), "{}", after.cpu_secs);
        assert_eq!(after.read_bytes, Some(80));

        // An orphan reaped outside the tree loses its CPU and its bytes, once.
        let (settled, fold) = {
            let mut fold = Fold::new(&clock.probe(3), Platform::Macos);
            fold.fold(clock.reading(
                1,
                vec![member(100, 1, 100.0, 0.0, 10), member(104, 1, 2.0, 0.0, 5)],
            ));
            let mut last = None;
            for tick in 2..=3 {
                last = Some(fold.fold(clock.reading(tick, vec![member(100, 1, 100.0, 0.0, 10)])));
            }
            (last.unwrap(), fold)
        };
        assert!(close(settled.cpu_secs, 102.0), "{}", settled.cpu_secs);
        assert_eq!(settled.read_bytes, Some(15));
        assert!(close(fold.lost.cpu, 2.0));
    }

    #[test]
    fn a_member_missing_from_one_reading_had_not_left() {
        // XNU may refuse to answer for a process while it tears it down, before it is a
        // zombie: the child misses one reading, and is back as a zombie with its final times.
        for platform in [Platform::Linux, Platform::Macos] {
            let (settled, fold) = fold_on(
                platform,
                vec![
                    vec![
                        member(100, 1, 100.0, 0.0, 10),
                        member(101, 100, 10.0, 0.0, 20),
                    ],
                    vec![member(100, 1, 105.0, 0.0, 10)],
                    vec![
                        member(100, 1, 110.0, 0.0, 10),
                        member(101, 100, 11.0, 0.0, 20),
                    ],
                    vec![
                        member(100, 1, 110.0, 0.0, 10),
                        member(101, 100, 11.0, 0.0, 20),
                    ],
                ],
            );
            assert!(close(settled.cpu_secs, 121.0), "{}", settled.cpu_secs);
            assert_eq!(settled.read_bytes, Some(30), "{platform:?}");
            assert!(close(fold.lost.cpu, 0.0), "{platform:?}");
            assert_eq!(fold.lost.read, 0, "{platform:?}");
        }
        // The root, which has no parent to wait on, the same way.
        let (settled, fold) = fold_on(
            Platform::Macos,
            vec![
                vec![member(100, 1, 100.0, 0.0, 10)],
                Vec::new(),
                vec![member(100, 1, 101.0, 0.0, 10)],
            ],
        );
        assert!(close(settled.cpu_secs, 101.0), "{}", settled.cpu_secs);
        assert_eq!(settled.read_bytes, Some(10));
        assert!(close(fold.lost.cpu, 0.0));
    }

    #[test]
    fn a_member_missing_from_several_readings_had_not_left() {
        // XNU's teardown of a process can outlast a reading: the child misses two readings,
        // is back as a zombie with the same counters, and then its parent reaps it.
        for platform in [Platform::Linux, Platform::Macos] {
            let clock = clock();
            let mut fold = Fold::new(&clock.probe(1), platform);
            let root = |rollup| member(100, 1, 100.0, rollup, 10);
            let child = member(101, 100, 10.0, 0.0, 20);
            fold.fold(clock.reading(1, vec![root(0.0), child.clone()]));
            fold.fold(clock.reading(2, vec![root(0.0)]));
            fold.fold(clock.reading(3, vec![root(0.0)]));
            let back = fold.fold(clock.reading(4, vec![root(0.0), child]));
            assert!(
                close(back.cpu_secs, 110.0),
                "{platform:?}: {}",
                back.cpu_secs
            );
            assert_eq!(back.read_bytes, Some(30), "{platform:?}");
            assert_eq!(fold.lost.read, 0, "{platform:?}");
            assert!(close(fold.lost.cpu, 0.0), "{platform:?}: {}", fold.lost.cpu);
            let reaped = fold.fold(clock.reading(5, vec![root(10.0)]));
            assert!(
                close(reaped.cpu_secs, 110.0),
                "{platform:?}: {}",
                reaped.cpu_secs
            );
            assert_eq!(reaped.read_bytes, Some(30), "{platform:?}");
        }
        // Three readings: by then its CPU was lost too, and is taken back as well.
        for platform in [Platform::Linux, Platform::Macos] {
            let (settled, fold) = fold_on(
                platform,
                vec![
                    vec![
                        member(100, 1, 100.0, 0.0, 10),
                        member(101, 100, 10.0, 0.0, 20),
                    ],
                    vec![member(100, 1, 100.0, 0.0, 10)],
                    vec![member(100, 1, 100.0, 0.0, 10)],
                    vec![member(100, 1, 100.0, 0.0, 10)],
                    vec![
                        member(100, 1, 100.0, 0.0, 10),
                        member(101, 100, 10.0, 0.0, 20),
                    ],
                ],
            );
            assert!(
                close(settled.cpu_secs, 110.0),
                "{platform:?}: {}",
                settled.cpu_secs
            );
            assert_eq!(settled.read_bytes, Some(30), "{platform:?}");
            assert!(close(fold.lost.cpu, 0.0), "{platform:?}");
        }
        // The root, missing from two readings.
        for platform in [Platform::Linux, Platform::Macos] {
            let (settled, fold) = fold_on(
                platform,
                vec![
                    vec![member(100, 1, 100.0, 0.0, 10)],
                    Vec::new(),
                    Vec::new(),
                    vec![member(100, 1, 101.0, 0.0, 10)],
                ],
            );
            assert!(
                close(settled.cpu_secs, 101.0),
                "{platform:?}: {}",
                settled.cpu_secs
            );
            assert_eq!(settled.read_bytes, Some(10), "{platform:?}");
            assert!(close(fold.lost.cpu, 0.0), "{platform:?}");
        }
    }

    #[test]
    fn a_member_that_is_back_gives_back_what_it_claimed() {
        // zebrad reaps 102 while XNU refuses 101 in its exit window: both leave, and 101, the
        // lower pid, claims 1 s of the 10 s that are 102's, so 102 waits. 101 is back as a
        // zombie: what it claimed is given back, 102 claims its 10 s, and zebrad reaps 101.
        let readings = |missing: u32, other: u32, cpu: f64| {
            vec![
                vec![
                    member(100, 1, 100.0, 0.0, 0),
                    member(missing, 100, 1.0, 0.0, 0),
                    member(other, 100, cpu, 0.0, 0),
                ],
                vec![member(100, 1, 100.0, cpu, 0)],
                vec![
                    member(100, 1, 100.0, cpu, 0),
                    member(missing, 100, 1.0, 0.0, 0),
                ],
                vec![member(100, 1, 100.0, cpu + 1.0, 0)],
                vec![member(100, 1, 100.0, cpu + 1.0, 0)],
            ]
        };
        for platform in [Platform::Linux, Platform::Macos] {
            let (settled, fold) = fold_on(platform, readings(101, 102, 10.0));
            assert!(
                close(settled.cpu_secs, 111.0),
                "{platform:?}: {}",
                settled.cpu_secs
            );
            assert!(close(fold.lost.cpu, 0.0), "{platform:?}: {}", fold.lost.cpu);
            // 101's claim from its missing reading was given back; the ones after the reaps stay.
            let kept: Vec<(u32, u64)> = fold
                .claims
                .iter()
                .map(|claim| (claim.id.pid, claim.at))
                .collect();
            assert_eq!(kept, vec![(102, 3), (101, 4)], "{platform:?}");
            // With the reaped one first by pid, it claims its own growth.
            let (settled, fold) = fold_on(platform, readings(102, 101, 10.0));
            assert!(
                close(settled.cpu_secs, 111.0),
                "{platform:?}: {}",
                settled.cpu_secs
            );
            assert!(close(fold.lost.cpu, 0.0), "{platform:?}: {}", fold.lost.cpu);
        }
        // A reaped sibling's whole time was at stake: 3,600 s against the refused one's 1 s.
        let (settled, fold) = fold_on(Platform::Macos, readings(101, 102, 3600.0));
        assert!(close(settled.cpu_secs, 3701.0), "{}", settled.cpu_secs);
        assert!(close(fold.lost.cpu, 0.0), "{}", fold.lost.cpu);
    }

    #[test]
    fn what_is_kept_for_a_member_that_may_be_back_expires() {
        let clock = clock();
        let mut fold = Fold::new(&clock.probe(1), Platform::Macos);
        fold.fold(clock.reading(
            1,
            vec![member(100, 1, 100.0, 0.0, 10), member(104, 1, 2.0, 0.0, 5)],
        ));
        // The orphan is missing: the reader looks for it again by its identity.
        fold.fold(clock.reading(2, vec![member(100, 1, 100.0, 0.0, 10)]));
        assert_eq!(fold.sticky(), vec![id(100, 1000), id(104, 1040)]);
        assert!(!fold.losses.is_empty());
        for tick in 3..3 + RETURN_READINGS {
            fold.fold(clock.reading(tick, vec![member(100, 1, 100.0, 0.0, 10)]));
        }
        assert_eq!(fold.sticky(), vec![id(100, 1000)]);
        assert!(fold.losses.is_empty(), "{:?}", fold.losses);
        assert!(fold.absent.is_empty(), "{:?}", fold.absent);
        assert!(close(fold.lost.cpu, 2.0));
        assert_eq!(fold.lost.read, 5);
    }

    #[test]
    fn a_departure_anchors_again_on_a_parent_that_is_back() {
        // A torn read: sh (101) was read just before it reaped cmd (102), so cmd waits on sh.
        // XNU then refuses sh while it exits, and cmd moves up to zebrad. sh is back as a
        // zombie whose rollup holds cmd's 2.5 s: cmd claims it there, and zebrad's rollup
        // later holds sh's final 4.0 s.
        let (settled, fold) = fold_on(
            Platform::Macos,
            vec![
                vec![
                    member(100, 1, 100.0, 0.0, 0),
                    member(101, 100, 1.0, 0.0, 0),
                    member(102, 101, 2.0, 0.0, 0),
                ],
                vec![member(100, 1, 100.0, 0.0, 0), member(101, 100, 1.0, 0.0, 0)],
                vec![member(100, 1, 100.0, 0.0, 0)],
                vec![member(100, 1, 100.0, 0.0, 0), member(101, 100, 1.5, 2.5, 0)],
                vec![member(100, 1, 100.0, 4.0, 0)],
                vec![member(100, 1, 100.0, 4.0, 0)],
            ],
        );
        assert!(close(settled.cpu_secs, 104.0), "{}", settled.cpu_secs);
        assert!(close(fold.lost.cpu, 0.0), "{}", fold.lost.cpu);
    }

    #[test]
    fn a_command_that_leaves_as_its_shell_is_refused_waits_for_the_shell() {
        // sh (101) reaps cmd (102) and starts to exit, so XNU refuses it in the reading where
        // cmd is gone: both wait on zebrad. sh is back as a zombie whose rollup holds cmd's
        // 2.5 s, and cmd claims it there.
        let (settled, fold) = fold_on(
            Platform::Macos,
            vec![
                vec![
                    member(100, 1, 100.0, 0.0, 0),
                    member(101, 100, 1.0, 0.0, 0),
                    member(102, 101, 2.0, 0.0, 0),
                ],
                vec![member(100, 1, 100.0, 0.0, 0)],
                vec![member(100, 1, 100.0, 0.0, 0), member(101, 100, 1.5, 2.5, 0)],
                vec![member(100, 1, 100.0, 4.0, 0)],
                vec![member(100, 1, 100.0, 4.0, 0)],
            ],
        );
        assert!(close(settled.cpu_secs, 104.0), "{}", settled.cpu_secs);
        assert!(close(fold.lost.cpu, 0.0), "{}", fold.lost.cpu);
    }

    #[test]
    fn a_departure_lost_while_its_parent_was_missing_waits_for_it_again() {
        // As above, but XNU refuses sh for two readings, so cmd waits on zebrad in vain and goes
        // to `lost`. sh is back with cmd's 2.5 s in its rollup: cmd waits on sh again and
        // claims it there. Linux rolls cmd's 7 bytes into sh too; macOS loses them as cmd
        // leaves.
        for platform in [Platform::Linux, Platform::Macos] {
            let rolled = if platform == Platform::Linux { 7 } else { 0 };
            let (settled, fold) = fold_on(
                platform,
                vec![
                    vec![
                        member(100, 1, 100.0, 0.0, 0),
                        member(101, 100, 1.0, 0.0, 0),
                        member(102, 101, 2.0, 0.0, 7),
                    ],
                    vec![member(100, 1, 100.0, 0.0, 0)],
                    vec![member(100, 1, 100.0, 0.0, 0)],
                    vec![
                        member(100, 1, 100.0, 0.0, 0),
                        member(101, 100, 1.5, 2.5, rolled),
                    ],
                    vec![member(100, 1, 100.0, 4.0, rolled)],
                    vec![member(100, 1, 100.0, 4.0, rolled)],
                ],
            );
            assert!(
                close(settled.cpu_secs, 104.0),
                "{platform:?}: {}",
                settled.cpu_secs
            );
            assert!(close(fold.lost.cpu, 0.0), "{platform:?}: {}", fold.lost.cpu);
            assert_eq!(settled.read_bytes, Some(7), "{platform:?}");
            assert_eq!(fold.lost.read, 7 - rolled, "{platform:?}");
        }
        // The root, refused for one reading or two after it reaped its child: the child, lost
        // at once with no live ancestor, is in the root's rollup.
        for platform in [Platform::Linux, Platform::Macos] {
            for missing in 1..=2 {
                let mut readings = vec![vec![
                    member(100, 1, 100.0, 0.0, 0),
                    member(101, 100, 5.0, 0.0, 0),
                ]];
                readings.extend((0..missing).map(|_| Vec::new()));
                readings.push(vec![member(100, 1, 100.0, 5.0, 0)]);
                let (settled, fold) = fold_on(platform, readings);
                assert!(
                    close(settled.cpu_secs, 105.0),
                    "{platform:?}, {missing}: {}",
                    settled.cpu_secs
                );
                assert!(close(fold.lost.cpu, 0.0), "{platform:?}, {missing}");
            }
        }
        // A departure lost before its parent went missing stays lost: sh ignores SIGCHLD, so
        // cmd is lost, and the 2 s sh's rollup gained while it was missing are another child's.
        let (settled, fold) = fold_on(
            Platform::Macos,
            vec![
                vec![
                    member(100, 1, 100.0, 0.0, 0),
                    member(101, 100, 1.0, 0.0, 0),
                    member(102, 101, 2.0, 0.0, 0),
                ],
                vec![member(100, 1, 100.0, 0.0, 0), member(101, 100, 1.0, 0.0, 0)],
                vec![member(100, 1, 100.0, 0.0, 0), member(101, 100, 1.0, 0.0, 0)],
                vec![member(100, 1, 100.0, 0.0, 0)],
                vec![member(100, 1, 100.0, 0.0, 0), member(101, 100, 1.5, 2.0, 0)],
                vec![member(100, 1, 100.0, 3.5, 0)],
            ],
        );
        assert!(close(settled.cpu_secs, 105.5), "{}", settled.cpu_secs);
        assert!(close(fold.lost.cpu, 2.0), "{}", fold.lost.cpu);
    }

    #[test]
    fn windows_takes_the_jobs_totals() {
        let clock = clock();
        let mut fold = Fold::new(&clock.probe(1), Platform::Windows);
        let job = |cpu, read| {
            Some(JobTotals {
                cpu,
                read,
                written: read * 2,
            })
        };
        // No parent links on Windows: ppid 0.
        let mut root = member(100, 0, 3.0, 0.0, 10);
        root.committed = Some(2000);
        let mut child = member(101, 0, 1.0, 0.0, 20);
        child.committed = Some(500);
        let first = fold.fold(TreeReading {
            job: job(4.5, 40),
            ..clock.reading(1, vec![root.clone(), child])
        });
        assert!(
            close(first.cpu_secs, 4.5),
            "the job's, which holds a process already gone: {}",
            first.cpu_secs
        );
        assert_eq!(first.read_bytes, Some(40));
        assert_eq!(first.written_bytes, Some(80));
        assert_eq!(first.committed_bytes, Some(2500));
        assert_eq!(first.memory_bytes, 2000);
        assert_eq!(first.members[1].parent, None);
        assert_eq!(first.platform, Platform::Windows);
        // The child exits: the job still holds its time, and nothing is lost or counted twice.
        let second = fold.fold(TreeReading {
            job: job(9.5, 45),
            ..clock.reading(2, vec![root.clone()])
        });
        assert!(close(second.cpu_secs, 9.5), "{}", second.cpu_secs);
        assert!(close(second.cpu_cores.unwrap(), 1.0));
        assert_eq!(second.processes, 1);
        assert_eq!(second.committed_bytes, Some(2000));
        assert!(close(fold.lost.cpu, 0.0));
        // A reading without the job's totals keeps the last ones.
        let third = fold.fold(clock.reading(3, vec![root]));
        assert!(close(third.cpu_secs, 9.5), "{}", third.cpu_secs);
        assert_eq!(third.read_bytes, Some(45));
    }

    #[test]
    fn cpu_that_cannot_be_read_has_no_rate() {
        // An x86-64 build under Rosetta: the reader reads no times, and says so.
        let clock = clock();
        let mut fold = Fold::new(&clock.probe(1), Platform::Macos);
        let blind = |tick| TreeReading {
            denied: Denied {
                cpu: true,
                ..Denied::default()
            },
            ..clock.reading(tick, vec![member(100, 1, 0.0, 0.0, 10)])
        };
        let first = fold.fold(blind(1));
        assert_eq!(first.cpu_cores, None);
        assert_eq!(first.cpu_secs, 0.0);
        assert_eq!(first.read_bytes, Some(10), "the other figures are read");
        assert_eq!(first.memory_bytes, 1000);
        assert_eq!(fold.fold(blind(2)).cpu_cores, None);
    }

    #[test]
    fn rates_start_from_the_spawn() {
        let clock = clock();
        let mut fold = Fold::new(&clock.probe(1), Platform::Linux);
        let first = fold.fold(clock.reading(2, vec![member(100, 1, 20.0, 0.0, 0)]));
        assert!(close(first.run_secs, 10.0));
        assert!(
            close(first.cpu_cores.unwrap(), 2.0),
            "the average since the spawn"
        );
        let second = fold.fold(clock.reading(3, vec![member(100, 1, 35.0, 0.0, 0)]));
        assert!(close(second.cpu_cores.unwrap(), 3.0));
        // A reading right at the spawn is averaged over a second, not a few milliseconds.
        let mut fold = Fold::new(&clock.probe(2), Platform::Linux);
        let at_spawn = fold.fold(TreeReading {
            mono: clock.spawned + Duration::from_millis(2),
            ..clock.reading(0, vec![member(100, 1, 0.01, 0.0, 0)])
        });
        assert!(
            close(at_spawn.cpu_cores.unwrap(), 0.01),
            "{:?}",
            at_spawn.cpu_cores
        );
    }

    #[test]
    fn an_unreadable_field_keeps_its_last_value() {
        let clock = clock();
        let mut fold = Fold::new(&clock.probe(1), Platform::Linux);
        fold.fold(clock.reading(1, vec![member(100, 1, 1.0, 0.0, 500)]));
        let mut denied = member(100, 1, 2.0, 0.0, 0);
        denied.read = None;
        denied.written = None;
        denied.memory = None;
        let second = fold.fold(clock.reading(2, vec![denied]));
        assert_eq!(second.read_bytes, Some(500));
        assert_eq!(second.written_bytes, Some(1000));
        assert_eq!(second.memory_bytes, 1000);
        assert_eq!(
            second.members[0].read_bytes, None,
            "the process row says what was read"
        );

        // A tree whose I/O was never readable reports none.
        let mut fold = Fold::new(&clock.probe(2), Platform::Linux);
        let mut blind = member(100, 1, 1.0, 0.0, 0);
        blind.read = None;
        blind.written = None;
        assert_eq!(fold.fold(clock.reading(1, vec![blind])).read_bytes, None);
    }

    #[test]
    fn the_peak_is_the_largest_sum_or_member_peak() {
        let clock = clock();
        let mut fold = Fold::new(&clock.probe(1), Platform::Linux);
        let mut a = member(100, 1, 1.0, 0.0, 0);
        a.memory = Some(3000);
        a.peak = Some(3500);
        let mut b = member(101, 100, 1.0, 0.0, 0);
        b.memory = Some(1000);
        b.peak = Some(1000);
        let first = fold.fold(clock.reading(1, vec![a.clone(), b]));
        assert_eq!(first.memory_bytes, 4000);
        assert_eq!(
            first.peak_memory_bytes, 4000,
            "the sum beats one member's peak"
        );
        a.memory = Some(2000);
        a.peak = Some(5000);
        let second = fold.fold(clock.reading(2, vec![a]));
        assert_eq!(second.memory_bytes, 2000);
        assert_eq!(
            second.peak_memory_bytes, 5000,
            "a member's own peak beats the sums"
        );
        assert_eq!(second.threads, Some(2));
        assert_eq!(second.descriptors, Some(10));
    }

    #[test]
    fn the_root_stays_known_by_its_identity() {
        let clock = clock();
        let mut fold = Fold::new(&clock.probe(1), Platform::Linux);
        assert!(fold.sticky().is_empty());
        fold.fold(clock.reading(
            1,
            vec![member(100, 1, 1.0, 0.0, 0), member(101, 100, 1.0, 0.0, 0)],
        ));
        assert_eq!(fold.sticky(), vec![id(100, 1000), id(101, 1010)]);
        // After the root exits, its identity stays, so a reused pid is never taken for it.
        fold.fold(clock.reading(2, vec![member(101, 100, 1.0, 0.0, 0)]));
        assert_eq!(fold.sticky(), vec![id(100, 1000), id(101, 1010)]);
    }

    #[test]
    fn the_process_list_is_capped() {
        let clock = clock();
        let mut fold = Fold::new(&clock.probe(1), Platform::Linux);
        let mut members = vec![member(100, 1, 1.0, 0.0, 0)];
        members.extend((0..40).map(|i| member(200 + i, 100, 0.1, 0.0, 0)));
        let sample = fold.fold(clock.reading(1, members));
        assert_eq!(sample.processes, 41);
        assert_eq!(sample.members.len(), MAX_MEMBERS);
        assert_eq!(sample.members[0].pid, 100);
        assert_eq!(sample.run, 1);
        assert_eq!(sample.platform, Platform::Linux);
    }
}
