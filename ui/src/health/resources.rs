//! What the node's processes and the computer use, in the verdict's input and in the copied
//! summary (docs/RESOURCES.md): the rules for a full disk (19), low memory (20) and a chain
//! that may not fit (21), and the summary's lines, which come from the Resources card's own
//! helpers so they cannot disagree with the card.
//!
//! Only these facts raise a verdict, each with a sustain time and a leave margin
//! ([`Episode`]); CPU, I/O and the node's own memory never do.

use std::time::{Duration, SystemTime};

use launcher_types::{
    ExitClass, ExitInfo, MachineResources, MemoryPressure, Network, NodeId, NodeInfo, NodeKind,
    NodeResources, VolumeUsage,
};

use super::{Action, Facts, Level, STALE_AFTER, Verdict, for_text};
use crate::{
    charts::byte_rate,
    format::{LOW_SPACE_BYTES, bytes, cores, mem_bytes, mem_of, share, thousands},
    machine_history::{
        Episode, MachineHistory, VOLUME_GAP, available_share, commit_share, volume_fresh,
    },
    metrics_history::names,
    pages::dashboard::resources::{last_run_text, machine_text},
    resource_history::{RATE_WINDOW, ResourceHistory},
};

/// Rule 19's problem: under this much free on the node's data volume, on one reading, since
/// the next write may fail; it clears above [`DISK_FULL_CLEARS`].
pub const DISK_FULL_BYTES: u64 = 2_000_000_000;
pub const DISK_FULL_CLEARS: u64 = 3_000_000_000;
/// Rule 19's warning: under [`DISK_LOW_SHARE`] of the volume or [`DISK_LOW_BYTES`], whichever
/// is less, for [`DISK_LOW_FOR`]; it clears [`DISK_LOW_MARGIN`] above that.
pub const DISK_LOW_SHARE: f64 = 0.05;
pub const DISK_LOW_BYTES: u64 = 20_000_000_000;
pub const DISK_LOW_FOR: Duration = Duration::from_secs(60);
pub const DISK_LOW_MARGIN: u64 = 1_000_000_000;
/// Rule 20's problem (Linux, Windows): under this share of the memory available for
/// [`MEMORY_OUT_FOR`]; it clears above [`MEMORY_OUT_CLEARS`].
pub const MEMORY_OUT_SHARE: f64 = 0.05;
pub const MEMORY_OUT_CLEARS: f64 = 0.07;
pub const MEMORY_OUT_FOR: Duration = Duration::from_secs(2 * 60);
/// Rule 20's warning (Linux, Windows): under this share for [`MEMORY_LOW_FOR`]; it clears above
/// [`MEMORY_LOW_CLEARS`].
pub const MEMORY_LOW_SHARE: f64 = 0.10;
pub const MEMORY_LOW_CLEARS: f64 = 0.12;
pub const MEMORY_LOW_FOR: Duration = Duration::from_secs(5 * 60);
/// Rule 20 on macOS: the pressure level for this long.
pub const PRESSURE_FOR: Duration = Duration::from_secs(60);
/// Rule 20 on Windows too: the commit charge above this share of its limit for
/// [`MEMORY_OUT_FOR`]; it clears under [`COMMIT_FULL_CLEARS`].
pub const COMMIT_FULL_SHARE: f64 = 0.95;
pub const COMMIT_FULL_CLEARS: f64 = 0.93;
/// Rule 21: a syncing Mainnet zebrad's free space and state together under the chain's
/// expected size ([`LOW_SPACE_BYTES`]) times this for [`FIT_FOR`]; it clears above
/// [`FIT_CLEARS`] times it.
pub const FIT_MARGIN: f64 = 1.05;
pub const FIT_CLEARS: f64 = 1.10;
pub const FIT_FOR: Duration = Duration::from_secs(60);
/// The exit sentences look this far back from an exit.
pub const BEFORE_EXIT: Duration = Duration::from_secs(60);

/// A reading for [`Episode::current`]: its time and its value, if it has one.
type Reading = (SystemTime, Option<f64>);

/// How much more memory than the node that carries the memory verdict another node must use to
/// take it: this share of the carrier's, and at least [`CARRIER_MARGIN_BYTES`].
pub const CARRIER_MARGIN_SHARE: f64 = 0.05;
pub const CARRIER_MARGIN_BYTES: u64 = 256 << 20;

/// The node that carries the memory verdict, of either network: the one that uses the most
/// memory (the first of equals). `memory` lists each node in the node list's order with its
/// mean over the last minute ([`ResourceHistory::memory_mean`]), `None` without a run.
/// `previous`, the carrier so far, keeps it while it has a run unless another node uses
/// [`CARRIER_MARGIN_SHARE`] more (at least [`CARRIER_MARGIN_BYTES`]), so two nodes that use
/// about as much do not pass the verdict back and forth.
pub fn memory_carrier(
    memory: &[(NodeId, Option<u64>)],
    previous: Option<&NodeId>,
) -> Option<NodeId> {
    let running = || {
        memory
            .iter()
            .filter_map(|(id, bytes)| Some((id, (*bytes)?)))
    };
    let (largest, most) =
        running().fold(
            None,
            |best: Option<(&NodeId, u64)>, (id, bytes)| match best {
                Some((_, top)) if top >= bytes => best,
                _ => Some((id, bytes)),
            },
        )?;
    let held = previous.and_then(|previous| running().find(|(id, _)| *id == previous));
    if let Some((id, bytes)) = held {
        let margin = ((bytes as f64 * CARRIER_MARGIN_SHARE) as u64).max(CARRIER_MARGIN_BYTES);
        if most <= bytes.saturating_add(margin) {
            return Some(id.clone());
        }
    }
    Some(largest.clone())
}

/// The resource facts the verdict and the summary read, borrowed.
#[derive(Clone, Copy)]
pub struct ResourceInput<'a> {
    /// The node: its id names its volume, its name the reasons'.
    pub info: &'a NodeInfo,
    /// It carries the memory verdict ([`memory_carrier`]).
    pub carries_memory: bool,
    /// The node's newest sample, `None` while it has no run.
    pub node: Option<&'a NodeResources>,
    /// The node's samples since the launcher opened.
    pub history: &'a ResourceHistory,
    /// The computer's newest sample.
    pub machine: Option<&'a MachineResources>,
    /// The computer's samples since the launcher opened.
    pub machine_history: &'a MachineHistory,
}

impl ResourceInput<'_> {
    /// The copied summary's lines, after the state on disk: the node's processes over the last
    /// minute (its last run once it stopped), and the computer as the card's footer has it.
    ///
    /// `Resources: 5.6 cores (70 % of 8) · 6.1 GiB, peak 6.4 GiB · 1 process · Disk 35.0 MB/s
    /// read, 95.0 MB/s written`
    pub fn summary_lines(&self, now: SystemTime) -> Vec<String> {
        let mut lines = Vec::with_capacity(2);
        match self.node {
            Some(sample) => lines.push(format!("Resources: {}", self.node_text(sample, now))),
            None => {
                if let Some(summary) = self.history.last_run() {
                    lines.push(format!("Resources: {}", last_run_text(&summary)));
                }
            }
        }
        if let Some(machine) = self.machine {
            let busy = self.machine_history.busy_cores(RATE_WINDOW, now);
            lines.push(machine_text(machine, busy, &self.info.id, now));
        }
        lines
    }

    /// The live sample's figures, with the one-minute rates.
    fn node_text(&self, sample: &NodeResources, now: SystemTime) -> String {
        let mut parts = Vec::with_capacity(4);
        let cpus = self
            .machine
            .map(|machine| machine.logical_cpus)
            .filter(|&cpus| cpus > 0);
        parts.push(match (self.history.cpu_cores(RATE_WINDOW, now), cpus) {
            (Some(busy), Some(cpus)) => format!(
                "{} ({} of {cpus})",
                cores(busy),
                share(busy / f64::from(cpus))
            ),
            (Some(busy), None) => cores(busy),
            (None, _) => "CPU not available".to_owned(),
        });
        let mut memory = format!(
            "{}, peak {}",
            mem_bytes(sample.memory_bytes),
            mem_bytes(sample.peak_memory_bytes)
        );
        if let Some(committed) = sample.committed_bytes {
            memory.push_str(&format!(", {} committed", mem_bytes(committed)));
        }
        parts.push(memory);
        parts.push(match sample.processes {
            1 => "1 process".to_owned(),
            n => format!("{} processes", thousands(u64::from(n))),
        });
        let rate = |rate: Option<f64>| rate.map_or_else(|| "—".to_owned(), byte_rate);
        match self.history.io_rates(RATE_WINDOW, now) {
            (None, None) => {}
            (read, written) => parts.push(format!(
                "{} {} read, {} written",
                sample.platform.io_label(),
                rate(read),
                rate(written)
            )),
        }
        parts.join(" · ")
    }
}

impl Facts<'_> {
    fn resources(&self) -> &ResourceInput<'_> {
        &self.input.resources
    }

    /// The readings of the node's volume, oldest first, while the newest still says how the
    /// volume is ([`volume_fresh`]); none once its folder's read hangs.
    fn volume_readings(&self) -> Vec<&VolumeUsage> {
        let resources = self.resources();
        let readings = resources
            .machine_history
            .volume_readings(&resources.info.id);
        match readings.last() {
            Some(newest) if volume_fresh(newest, self.now()) => readings,
            _ => Vec::new(),
        }
    }

    /// The free space readings of the node's volume as an episode's readings, with the newest;
    /// `None` without a fresh one.
    fn free_readings(&self) -> Option<(&VolumeUsage, Vec<Reading>)> {
        let readings = self.volume_readings();
        let newest = *readings.last()?;
        let free = readings
            .iter()
            .map(|volume| (volume.measured_at, Some(volume.free_bytes as f64)))
            .collect();
        Some((newest, free))
    }

    // Rule 19, its problem: under 2 GB free on one reading. It runs before rule 7, as a full
    // disk is what stops zebrad's writes, its RPC and its sync.
    pub(super) fn disk_full(&self) -> Option<Verdict> {
        let (newest, free) = self.free_readings()?;
        Episode::current(
            free,
            |bytes| bytes < DISK_FULL_BYTES as f64,
            |bytes| bytes > DISK_FULL_CLEARS as f64,
            Duration::ZERO,
            VOLUME_GAP,
        )?;
        Some(Verdict::new(
            Level::Problem,
            19,
            "Disk almost full",
            format!(
                "{} free on {}. zebrad stops when a write fails; free some space or move the data folder.",
                bytes(newest.free_bytes),
                disk_place(newest)
            ),
            vec![Action::OpenDataFolder],
        ))
    }

    // Rule 19, its warning: under min(5 %, 20 GB) free for a minute. It stops nothing yet, so
    // it runs after rule 15 and leaves the running problems their verdict.
    pub(super) fn disk_space_low(&self) -> Option<Verdict> {
        let (newest, free) = self.free_readings()?;
        let threshold = ((newest.total_bytes as f64 * DISK_LOW_SHARE) as u64).min(DISK_LOW_BYTES);
        let episode = Episode::current(
            free,
            |bytes| bytes < threshold as f64,
            |bytes| bytes > (threshold + DISK_LOW_MARGIN) as f64,
            DISK_LOW_FOR,
            VOLUME_GAP,
        )?;
        Some(Verdict::new(
            Level::Attention,
            19,
            "Disk space low",
            format!(
                "{} free of {} on {} {}. zebrad stops when a write fails.",
                bytes(newest.free_bytes),
                bytes(newest.total_bytes),
                disk_place(newest),
                for_text(self.ago(episode.since))
            ),
            vec![Action::OpenDataFolder],
        ))
    }

    // Rule 20, its problem: the computer's memory nearly exhausted. It runs before rule 7, as
    // exhausted memory is what makes the RPC go quiet and the sync stall.
    pub(super) fn memory_exhausted(&self) -> Option<Verdict> {
        self.memory(Level::Problem)
    }

    // Rule 20, its warning: the computer's memory low. Under 10 % for the whole of a Mainnet
    // sync is usual on an 8 or 16 GiB computer (zebra#9699), so it runs after rule 15 and
    // leaves the running problems their verdict.
    pub(super) fn memory_low(&self) -> Option<Verdict> {
        self.memory(Level::Attention)
    }

    /// Rule 20 at `level`, the problem or the warning, on the node that carries it.
    fn memory(&self, level: Level) -> Option<Verdict> {
        let resources = self.resources();
        if !resources.carries_memory {
            return None;
        }
        let history = resources.machine_history;
        let machine = history.latest()?;
        if self.ago(machine.at) > STALE_AFTER {
            return None;
        }
        let total = machine.memory_total_bytes;
        let name = &resources.info.name;
        let node_memory = resources.node.map(|node| node.memory_bytes);
        let uses = node_memory.map_or_else(String::new, |memory| {
            format!("; {name} uses {} of it", mem_bytes(memory))
        });
        let available = |episode: Episode| {
            let available = machine.memory_available_bytes.unwrap_or(0);
            format!(
                "This computer has {} available ({}) {}{uses}",
                mem_of(available, total),
                share(available_share(machine).unwrap_or(0.0)),
                for_text(self.ago(episode.since))
            )
        };
        let pressure = |episode: Episode| {
            let level = machine.pressure.unwrap_or(MemoryPressure::Normal);
            format!(
                "macOS reports {} memory pressure {}{uses}",
                level.as_str(),
                for_text(self.ago(episode.since))
            )
        };
        let commit = |episode: Episode| {
            let committed = resources.node.and_then(|node| node.committed_bytes);
            let commits = committed.map_or_else(String::new, |bytes| {
                format!("; {name} commits {} of it", mem_bytes(bytes))
            });
            format!(
                "Windows has committed {} ({}) {}{commits}",
                mem_of(
                    machine.commit_bytes.unwrap_or(0),
                    machine.commit_limit_bytes.unwrap_or(0)
                ),
                share(commit_share(machine).unwrap_or(0.0)),
                for_text(self.ago(episode.since))
            )
        };
        // macOS's available figure is an estimate, so its verdicts follow the pressure level.
        let macos = machine.pressure.is_some();
        let (headline, reason) = if level == Level::Problem {
            let reason = if macos {
                history
                    .pressure_at_least_for(MemoryPressure::Critical, PRESSURE_FOR)
                    .map(pressure)
            } else {
                history
                    .available_below_for(MEMORY_OUT_SHARE, MEMORY_OUT_FOR, MEMORY_OUT_CLEARS)
                    .map(available)
                    .or_else(|| {
                        history
                            .commit_above_for(COMMIT_FULL_SHARE, MEMORY_OUT_FOR, COMMIT_FULL_CLEARS)
                            .map(commit)
                    })
            };
            ("Memory nearly exhausted", reason?)
        } else {
            let reason = if macos {
                history
                    .pressure_at_least_for(MemoryPressure::Warning, PRESSURE_FOR)
                    .map(pressure)
            } else {
                history
                    .available_below_for(MEMORY_LOW_SHARE, MEMORY_LOW_FOR, MEMORY_LOW_CLEARS)
                    .map(available)
            };
            ("Memory low", reason?)
        };
        // zebra#9699: a zebrad that grew to most of a computer's memory while syncing needs
        // much less after a restart.
        let restart = node_memory.is_some_and(|memory| memory.saturating_mul(2) >= total);
        let actions = if restart {
            vec![Action::Restart]
        } else {
            Vec::new()
        };
        Some(Verdict::new(level, 20, headline, reason, actions))
    }

    // Rule 21.
    pub(super) fn chain_may_not_fit(&self) -> Option<Verdict> {
        let resources = self.resources();
        let info = resources.info;
        if info.kind != NodeKind::Zebrad || info.network != Network::Mainnet {
            return None;
        }
        if self.latest?.at_tip() {
            return None;
        }
        let state = self.metric_count(names::DISK_BYTES)?;
        let history = self.input.metrics_history;
        let volumes = self.volume_readings();
        let readings = volumes
            .iter()
            .map(|volume| {
                let state = history
                    .value_at(names::DISK_BYTES, volume.measured_at)
                    .unwrap_or(state as f64);
                (volume.measured_at, Some(volume.free_bytes as f64 + state))
            })
            .collect::<Vec<_>>();
        let expected = LOW_SPACE_BYTES as f64;
        Episode::current(
            readings,
            |room| room < expected * FIT_MARGIN,
            |room| room > expected * FIT_CLEARS,
            FIT_FOR,
            VOLUME_GAP,
        )?;
        let volume = volumes.last()?;
        Some(Verdict::new(
            Level::Attention,
            21,
            "The chain may not fit",
            format!(
                "Mainnet needs about {}. This node holds {} and its disk has {} free.",
                bytes_round(LOW_SPACE_BYTES),
                bytes(state),
                bytes(volume.free_bytes)
            ),
            vec![Action::OpenDataFolder],
        ))
    }
}

impl Facts<'_> {
    /// What the computer's history says about a crash the user did not ask for, from the
    /// minute before it (rules 2, 3 and 15): memory almost out when a signal ended the node
    /// (the Linux OOM killer and macOS jetsam send `SIGKILL`, which is classed a crash),
    /// Windows' commit charge near its limit, and under 2 GB free on the node's disk. The
    /// other failures (a port in use, a configuration error, a locked database, a failed
    /// spawn, the end of support) have causes of their own, so they get none.
    pub(super) fn exit_context(&self, exit: &ExitInfo) -> Option<String> {
        if exit.requested || exit.class != ExitClass::Crash {
            return None;
        }
        let resources = self.resources();
        let history = resources.machine_history;
        let from = exit.at.checked_sub(BEFORE_EXIT).unwrap_or(exit.at);
        let mut sentences = Vec::new();
        let memory_out = |machine: &MachineResources| {
            available_share(machine).is_some_and(|share| share < MEMORY_OUT_SHARE)
                || machine.pressure == Some(MemoryPressure::Critical)
        };
        if sigkill(exit) && history.any_between(from, exit.at, memory_out) {
            sentences.push(
                "the computer was almost out of memory just before; the system may have stopped it"
                    .to_owned(),
            );
        }
        let commit_full = |machine: &MachineResources| {
            commit_share(machine).is_some_and(|share| share > COMMIT_FULL_SHARE)
        };
        if history.any_between(from, exit.at, commit_full) {
            sentences.push("Windows was almost out of memory to commit just before".to_owned());
        }
        let disk = history
            .volume_readings(&resources.info.id)
            .into_iter()
            .rev()
            .find(|volume| volume.measured_at <= exit.at)
            .filter(|volume| volume_fresh(volume, exit.at));
        if let Some(volume) = disk.filter(|volume| volume.free_bytes < DISK_FULL_BYTES) {
            sentences.push(format!(
                "only {} was free on its disk",
                bytes(volume.free_bytes)
            ));
        }
        (!sentences.is_empty()).then(|| sentences.join(" · "))
    }
}

/// Whether `SIGKILL` ended the run, as the OOM killer and jetsam send it: no exit code, and the
/// supervisor's detail names signal 9 (it names the signal when zebrad logged no reason).
fn sigkill(exit: &ExitInfo) -> bool {
    exit.code.is_none()
        && exit
            .detail
            .as_deref()
            .is_none_or(|detail| detail == "killed by signal 9")
}

/// Where rule 19 says the node's data is: `the disk with this node's data (/mnt/zcash)`.
fn disk_place(volume: &VolumeUsage) -> String {
    format!("the disk with this node's data ({})", volume.label)
}

/// A size in whole decimal units: `300 GB`.
fn bytes_round(value: u64) -> String {
    bytes(value).replace(".0 ", " ")
}
