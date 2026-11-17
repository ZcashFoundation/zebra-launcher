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

/// Whether `node` uses the most memory among the nodes with a run (`memory`, the newest
/// sample of each, `None` without a run; the first of equals). The memory verdict goes only
/// on that node, of either network.
pub fn largest_consumer(node: &NodeId, memory: &[(NodeId, Option<u64>)]) -> bool {
    memory
        .iter()
        .filter_map(|(id, bytes)| Some((id, (*bytes)?)))
        .fold(
            None,
            |best: Option<(&NodeId, u64)>, (id, bytes)| match best {
                Some((_, top)) if top >= bytes => best,
                _ => Some((id, bytes)),
            },
        )
        .is_some_and(|(id, _)| id == node)
}

/// The resource facts the verdict and the summary read, borrowed.
#[derive(Clone, Copy)]
pub struct ResourceInput<'a> {
    /// The node: its id names its volume, its name the reasons'.
    pub info: &'a NodeInfo,
    /// It uses the most memory of the nodes with a run ([`largest_consumer`]).
    pub largest_consumer: bool,
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

    // Rule 19.
    pub(super) fn disk_low(&self) -> Option<Verdict> {
        let readings = self.volume_readings();
        let newest = *readings.last()?;
        let free: Vec<_> = readings
            .iter()
            .map(|volume| (volume.measured_at, Some(volume.free_bytes as f64)))
            .collect();
        let place = format!("the disk with this node's data ({})", newest.label);
        if Episode::current(
            free.iter().copied(),
            |bytes| bytes < DISK_FULL_BYTES as f64,
            |bytes| bytes > DISK_FULL_CLEARS as f64,
            Duration::ZERO,
            VOLUME_GAP,
        )
        .is_some()
        {
            return Some(Verdict::new(
                Level::Problem,
                19,
                "Disk almost full",
                format!(
                    "{} free on {place}. zebrad stops when a write fails; free some space or move the data folder.",
                    bytes(newest.free_bytes)
                ),
                vec![Action::OpenDataFolder],
            ));
        }
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
                "{} free of {} on {place} {}. zebrad stops when a write fails.",
                bytes(newest.free_bytes),
                bytes(newest.total_bytes),
                for_text(self.ago(episode.since))
            ),
            vec![Action::OpenDataFolder],
        ))
    }

    // Rule 20.
    pub(super) fn memory_low(&self) -> Option<Verdict> {
        let resources = self.resources();
        if !resources.largest_consumer {
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
        let problem = if macos {
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
        let (level, headline, reason) = match problem {
            Some(reason) => (Level::Problem, "Memory nearly exhausted", reason),
            None => {
                let low = if macos {
                    history
                        .pressure_at_least_for(MemoryPressure::Warning, PRESSURE_FOR)
                        .map(pressure)
                } else {
                    history
                        .available_below_for(MEMORY_LOW_SHARE, MEMORY_LOW_FOR, MEMORY_LOW_CLEARS)
                        .map(available)
                };
                (Level::Attention, "Memory low", low?)
            }
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
    /// What the computer's history says about an exit the user did not ask for, from the
    /// minute before it (rules 2, 3 and 15): memory almost out when a signal ended the node
    /// (the Linux OOM killer and macOS jetsam send `SIGKILL`), Windows' commit charge near its
    /// limit at a crash, and under 2 GB free on the node's disk at a crash.
    pub(super) fn exit_context(&self, exit: &ExitInfo) -> Option<String> {
        if exit.requested || exit.class == ExitClass::Clean {
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

/// A size in whole decimal units: `300 GB`.
fn bytes_round(value: u64) -> String {
    bytes(value).replace(".0 ", " ")
}
