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
//! The published totals never decrease within a run, which hides the one-reading dip of a torn
//! read.

use std::{
    collections::{HashMap, HashSet},
    time::Instant,
};

use launcher_types::{MAX_MEMBERS, NodeResources, Platform, ProcessUsage};

use super::{Identity, MemberReading, TreeReading};
use crate::process::TreeProbe;

/// CPU seconds below which two figures count as equal: under one clock tick.
const EPSILON: f64 = 0.001;

/// A run's first rate is averaged over at least this long, so a reading taken right at the
/// spawn does not turn one clock tick of CPU into many cores.
const MIN_FIRST_RATE_SECS: f64 = 1.0;

/// How far up a chain of departed members the fold looks for a live ancestor.
const MAX_CHAIN: u32 = 64;

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
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
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

/// Counters that add up across members.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Counters {
    cpu: f64,
    read: u64,
    written: u64,
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
}

/// A member that left, whose time should be in a live ancestor's rollup.
#[derive(Clone, Debug, PartialEq)]
struct Departed {
    id: Identity,
    /// The nearest live ancestor when it left.
    anchor: Identity,
    /// 1 for a child of the anchor, 2 for a grandchild, and so on.
    depth: u32,
    cpu: f64,
    read: Option<u64>,
    written: Option<u64>,
    /// It has already waited one reading.
    waited: bool,
}

/// One run's measurement: the members last seen, the counters of the departed, and the
/// published totals.
#[derive(Debug)]
pub(crate) struct Fold {
    run: u64,
    root_pid: u32,
    spawned: Instant,
    platform: Platform,
    /// The root's identity, once a reading found it: the root is known by it from then on.
    root: Option<Identity>,
    last: HashMap<Identity, Seen>,
    pending: Vec<Departed>,
    /// Rollup growth of an anchor that its waiting departures may still claim.
    leftover: HashMap<Identity, f64>,
    lost: Counters,
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
        Fold {
            run: probe.run,
            root_pid: probe.pid,
            spawned: probe.spawned,
            platform,
            root: None,
            last: HashMap::new(),
            pending: Vec::new(),
            leftover: HashMap::new(),
            lost: Counters::default(),
            published: Counters::default(),
            io_seen: false,
            peak: 0,
            previous: None,
            rate: None,
        }
    }

    /// The identities seen as members before, for the reader: the last reading's members and
    /// the root, which stays known by its identity after it exits.
    pub(crate) fn sticky(&self) -> Vec<Identity> {
        let mut sticky: Vec<Identity> = self.last.keys().copied().collect();
        if let Some(root) = self.root.filter(|root| !self.last.contains_key(root)) {
            sticky.push(root);
        }
        sticky.sort_by_key(|id| (id.pid, id.start));
        sticky
    }

    /// Folds a reading into the run's totals and returns the sample to publish.
    pub(crate) fn fold(&mut self, reading: TreeReading) -> NodeResources {
        let TreeReading {
            at, mono, members, ..
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
        self.settle_departures(&members, &index);

        let mut totals = self.lost;
        let mut memory = 0u64;
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
            let cpu = member.cpu_own + member.cpu_rollup;
            totals.cpu += cpu;
            totals.read += read.unwrap_or(0);
            totals.written += written.unwrap_or(0);
            self.io_seen |= read.is_some() || written.is_some();
            memory = memory.saturating_add(member_memory.unwrap_or(0));
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
                },
            );
        }
        self.last = last;
        self.published.cpu = self.published.cpu.max(totals.cpu);
        self.published.read = self.published.read.max(totals.read);
        self.published.written = self.published.written.max(totals.written);
        self.peak = self.peak.max(memory).max(member_peak);

        let run_secs = mono.saturating_duration_since(self.spawned).as_secs_f64();
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
            committed_bytes: None,
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
        let mut pending = std::mem::take(&mut self.pending);
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
        let mut departed: Vec<(&Identity, &Seen)> =
            self.last.iter().filter(|(id, _)| !live(id)).collect();
        departed.sort_by_key(|(id, _)| (id.pid, id.start));
        let mut orphans = Vec::new();
        for (id, seen) in departed {
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
                    anchor,
                    depth,
                    cpu: seen.cpu,
                    read: seen.read,
                    written: seen.written,
                    waited: false,
                }),
                None => orphans.push(Counters {
                    cpu: seen.cpu,
                    read: seen.read.unwrap_or(0),
                    written: seen.written.unwrap_or(0),
                }),
            }
        }
        for orphan in orphans {
            self.lose(orphan);
        }

        // What each anchor's rollup gained since the last reading, plus what its waiting
        // departures left unclaimed then.
        let mut credit: HashMap<Identity, f64> = HashMap::new();
        for departure in &pending {
            credit.entry(departure.anchor).or_insert_with(|| {
                let now = index.get(&departure.anchor).map(|&i| members[i].cpu_rollup);
                let before = self.last.get(&departure.anchor).map(|seen| seen.rollup);
                let grown = match (now, before) {
                    (Some(now), Some(before)) => (now - before).max(0.0),
                    _ => 0.0,
                };
                grown + self.leftover.get(&departure.anchor).copied().unwrap_or(0.0)
            });
        }
        // Waiting departures first, since their reap may be what this reading shows; then the
        // shallowest, whose fold carries the time of the children it reaped.
        pending.sort_by_key(|departure| {
            (
                !departure.waited,
                departure.depth,
                departure.id.pid,
                departure.id.start,
            )
        });
        let mut waiting = Vec::new();
        for departure in pending {
            let anchored = live(&departure.anchor);
            let credit = credit.entry(departure.anchor).or_insert(0.0);
            if anchored && departure.cpu <= *credit + EPSILON {
                *credit = (*credit - departure.cpu).max(0.0);
            } else if anchored && !departure.waited {
                waiting.push(Departed {
                    waited: true,
                    ..departure
                });
            } else {
                self.lose(Counters {
                    cpu: departure.cpu,
                    read: departure.read.unwrap_or(0),
                    written: departure.written.unwrap_or(0),
                });
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
    }

    fn lose(&mut self, counters: Counters) {
        self.lost.cpu += counters.cpu;
        self.lost.read = self.lost.read.saturating_add(counters.read);
        self.lost.written = self.lost.written.saturating_add(counters.written);
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
            }
        }

        fn reading(&self, tick: u64, members: Vec<MemberReading>) -> TreeReading {
            TreeReading {
                at: UNIX_EPOCH + Duration::from_secs(SPAWN_SECS + 5 * tick),
                mono: self.spawned + Duration::from_secs(5 * tick),
                members,
                denied: Default::default(),
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
            read: Some(read),
            written: Some(read * 2),
            threads: Some(2),
            descriptors: Some(10),
        }
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
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
