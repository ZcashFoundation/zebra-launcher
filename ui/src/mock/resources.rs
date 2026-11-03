//! The mock's resource samples: the primary zebrad's process tree every 5 s while it has a
//! pid (Starting, Running and Stopping, like the backend's sampler, which does not wait for
//! Running), and the computer every 5 s. The numbers are pure functions of the run, the phase
//! and the sample count, with jitter from a hash of the count rather than `Math.random`, so
//! they are tested natively; they reach the page through `api::mock_handler`, like every mock
//! payload.
//!
//! | State | zebrad |
//! | --- | --- |
//! | The first 10 s of a run | 1.1 cores and 180 MB/s read: the database opens |
//! | Syncing | 5.6 ± 0.8 cores, 35 MB/s read, 95 MB/s written; memory climbs from 2.4 to 6.4 GiB over 30 min, then a ±0.3 GiB sawtooth |
//! | At the tip | 0.25 cores, 3.2 GiB, 0.4 MB/s written |
//! | Stopping | 0.6 cores |
//! | `?stalled`, `?nopeers` | 0.1 cores and almost no I/O |
//!
//! zebrad always has 68 threads and 412 open files (1,204 handles on Windows). The computer
//! has 8 logical CPUs and 16 GiB; other programs use 0.6 to 0.9 cores and 5.2 GiB, and swap
//! holds 0.3 of 2 GiB. `?os=macos` and `?os=windows` switch the figures to those platforms'
//! (memory pressure on macOS; commit, handles and no thread counts on Windows).
//! `?children` gives zebrad a `block_notify_command` that runs in cycles of three samples
//! ([`block_notify`]), `?unreadable` reports CPU and I/O as refused by the OS, and
//! `?readerpanic` stops the samples 30 s into the first run while zebrad runs on, as the
//! backend does after a reader panic ([`reader_panicked`]).

use std::time::{Duration, SystemTime};

use launcher_types::{
    MAX_MEMBERS, MachineResources, MemoryPressure, NodeResources, Phase, Platform, ProcessUsage,
};
use leptos::prelude::*;

use super::{Flags, Scenario, mock, sleep};
use crate::clock;

/// `?os=`: the OS whose figures the mock reports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum OsFlag {
    #[default]
    Linux,
    Macos,
    Windows,
}

impl OsFlag {
    /// The value of `?os=`; anything else is Linux.
    pub(super) fn parse(value: Option<&str>) -> Self {
        match value {
            Some("macos") => OsFlag::Macos,
            Some("windows") => OsFlag::Windows,
            _ => OsFlag::Linux,
        }
    }

    fn platform(self) -> Platform {
        match self {
            OsFlag::Linux => Platform::Linux,
            OsFlag::Macos => Platform::Macos,
            OsFlag::Windows => Platform::Windows,
        }
    }
}

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
const MIB: f64 = 1024.0 * 1024.0;
const MB: f64 = 1_000_000.0;

/// How often the samplers read, like the backend's.
pub(super) const INTERVAL: Duration = Duration::from_secs(5);

/// How long the database open takes at the start of every run.
const OPENING_SECS: f64 = 10.0;

/// `?readerpanic`: how far into the first run the resource reader panics.
const READER_PANIC_SECS: f64 = 30.0;

/// How long memory climbs while syncing, and the period of the sawtooth after that.
const CLIMB_SECS: f64 = 30.0 * 60.0;
const SAWTOOTH_SECS: f64 = 4.0 * 60.0;

const LOGICAL_CPUS: u32 = 8;
const MEMORY_TOTAL_GIB: f64 = 16.0;
/// What the other programs on the computer use.
const OTHER_MEMORY_GIB: f64 = 5.2;
const SWAP_TOTAL_GIB: f64 = 2.0;
const SWAP_USED_GIB: f64 = 0.3;
/// Windows: the commit limit, and what the other programs commit.
const COMMIT_LIMIT_GIB: f64 = 24.0;
const OTHER_COMMIT_GIB: f64 = 7.0;
/// Windows: zebrad's commit charge for its working set.
const COMMIT_RATIO: f64 = 1.15;

/// zebrad's threads and open files (handles on Windows), always.
const THREADS: u32 = 68;
const OPEN_FILES: u32 = 412;
const HANDLES: u32 = 1204;

/// `?children`: the `curl`s `notify-block` starts, more than a sample lists, and the open
/// files (handles on Windows) of each child.
const CURLS: u32 = 40;
const CHILD_FILES: u32 = 3;
const CHILD_HANDLES: u32 = 30;

fn gib(value: f64) -> u64 {
    (value * GIB).round() as u64
}

/// A value from -1 to 1 for sample `count` of the series numbered `series`: a hash of the two
/// (splitmix64), so a test sees the same numbers as the page. Not cryptographic.
pub(super) fn jitter(count: u64, series: u64) -> f64 {
    let mut z = (count ^ series.rotate_left(32)).wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    (z >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
}

/// Where the run is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct RunState {
    /// Seconds since the spawn.
    pub run_secs: f64,
    /// A stop was requested.
    pub stopping: bool,
    /// The statistics put the node at the chain tip.
    pub at_tip: bool,
    pub scenario: Scenario,
    /// The run's sample count, for the jitter.
    pub count: u64,
}

/// What zebrad does over one interval.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Load {
    pub cores: f64,
    pub read_per_sec: f64,
    pub written_per_sec: f64,
    pub memory: u64,
}

/// zebrad's memory: the sync's climb and sawtooth, or the tip's steady size. Always above
/// the mock's state memory (the metrics' memtables and block cache, 1.62 to 1.73 GB).
fn memory_gib(state: &RunState) -> f64 {
    let wobble = 0.05 * jitter(state.count, 1);
    if state.at_tip {
        return 3.2 + wobble;
    }
    if state.run_secs < CLIMB_SECS {
        return 2.4 + 4.0 * (state.run_secs.max(0.0) / CLIMB_SECS) + wobble;
    }
    // From the middle of the tooth, so the climb runs on into it.
    let phase = ((state.run_secs - CLIMB_SECS) / SAWTOOTH_SECS + 0.5) % 1.0;
    6.4 + 0.3 * (2.0 * phase - 1.0)
}

/// What zebrad uses in `state`.
pub(super) fn load(state: &RunState) -> Load {
    let j = |series| jitter(state.count, series);
    let (cores, read_mb, written_mb) = if state.stopping {
        (0.6 + 0.05 * j(2), 2.0 + j(3), 20.0 + 5.0 * j(4))
    } else if state.run_secs < OPENING_SECS {
        (1.1 + 0.1 * j(2), 180.0 + 10.0 * j(3), 1.5 + 0.5 * j(4))
    } else if state.scenario != Scenario::Normal {
        (0.1 + 0.02 * j(2), 0.05 + 0.02 * j(3), 0.1 + 0.05 * j(4))
    } else if state.at_tip {
        (0.25 + 0.05 * j(2), 0.1 + 0.05 * j(3), 0.4 + 0.1 * j(4))
    } else {
        (5.6 + 0.8 * j(2), 35.0 + 5.0 * j(3), 95.0 + 10.0 * j(4))
    };
    Load {
        cores: cores.max(0.0),
        read_per_sec: read_mb.max(0.0) * MB,
        written_per_sec: written_mb.max(0.0) * MB,
        memory: gib(memory_gib(state)),
    }
}

/// `?readerpanic`: whether the resource reader has panicked, `run_secs` into `run`. It panics
/// once, in the first run, and the backend then publishes `None` until the next run while
/// zebrad keeps running.
pub(super) fn reader_panicked(flags: Flags, run: u64, run_secs: f64) -> bool {
    flags.reader_panic && run == 1 && run_secs >= READER_PANIC_SECS
}

/// A process under the mock's zebrad, for `?children`. It does no disk I/O.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct MockChild {
    pub pid: u32,
    /// Its parent, when that one is in the tree.
    pub parent: Option<u32>,
    pub name: &'static str,
    pub started_at: SystemTime,
    pub cores: f64,
    pub memory: u64,
}

/// `?children`: what zebrad's `block_notify_command`, `sh -c notify-block`, has running at a
/// run's sample `count`, taken `at`, given what it had at the sample before; in tree order.
/// Each cycle of three samples: a new `sh` with `notify-block` under it; then the 40 `curl`s
/// `notify-block` started, so the tree has more processes than a sample lists; then
/// `notify-block` alone, orphaned when `sh` exited, so it has no parent in the tree. A run's
/// first sample has none.
pub(super) fn block_notify(
    previous: &[MockChild],
    zebrad: u32,
    count: u64,
    at: SystemTime,
    os: OsFlag,
) -> Vec<MockChild> {
    let [shell, notify, curl] = if os == OsFlag::Windows {
        ["cmd.exe", "notify-block.exe", "curl.exe"]
    } else {
        ["sh", "notify-block", "curl"]
    };
    let started_at = at.checked_sub(Duration::from_secs(1)).unwrap_or(at);
    match count % 3 {
        1 => {
            // Pids from the cycle, so a new cycle never reuses the last one's.
            let sh = zebrad.wrapping_add(1 + 50 * (count / 3 % 1000) as u32);
            vec![
                MockChild {
                    pid: sh,
                    parent: Some(zebrad),
                    name: shell,
                    started_at,
                    cores: 0.0,
                    memory: (1.5 * MIB) as u64,
                },
                MockChild {
                    pid: sh.wrapping_add(1),
                    parent: Some(sh),
                    name: notify,
                    started_at,
                    cores: 0.05,
                    memory: (9.0 * MIB) as u64,
                },
            ]
        }
        2 => {
            let mut children = previous.to_vec();
            if let Some(parent) = previous.iter().find(|child| child.name == notify) {
                children.extend((1..=CURLS).map(|i| MockChild {
                    pid: parent.pid.wrapping_add(i),
                    parent: Some(parent.pid),
                    name: curl,
                    started_at,
                    cores: 0.01,
                    memory: (6.0 * MIB) as u64,
                }));
            }
            children
        }
        _ => previous
            .iter()
            .filter(|child| child.name == notify)
            .map(|child| MockChild {
                parent: None,
                ..child.clone()
            })
            .collect(),
    }
}

/// One run of the mock's zebrad, as the backend's fold would see it: counters since the
/// spawn and the peak.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct MockTree {
    pub run: u64,
    pub pid: u32,
    pub started_at: SystemTime,
    run_secs: f64,
    /// zebrad's own CPU.
    cpu_secs: f64,
    /// The CPU of its children, those that exited included (`?children`).
    children_cpu_secs: f64,
    read: f64,
    written: f64,
    peak: u64,
    /// The processes under zebrad at the last sample (`?children`).
    children: Vec<MockChild>,
    /// Samples taken so far.
    pub count: u64,
}

impl MockTree {
    pub(super) fn new(run: u64, pid: u32, started_at: SystemTime) -> Self {
        MockTree {
            run,
            pid,
            started_at,
            run_secs: 0.0,
            cpu_secs: 0.0,
            children_cpu_secs: 0.0,
            read: 0.0,
            written: 0.0,
            peak: 0,
            children: Vec::new(),
            count: 0,
        }
    }

    /// Advances the run to `run_secs` under `load` and returns the sample taken `at` then,
    /// with what `?os=`, `?children` and `?unreadable` ask for.
    pub(super) fn sample(
        &mut self,
        at: SystemTime,
        run_secs: f64,
        load: &Load,
        flags: Flags,
    ) -> NodeResources {
        let os = flags.os;
        if flags.children {
            self.children = block_notify(&self.children, self.pid, self.count, at, os);
        }
        let children_cores: f64 = self.children.iter().map(|child| child.cores).sum();
        let children_memory: u64 = self.children.iter().map(|child| child.memory).sum();
        let secs = (run_secs - self.run_secs).max(0.0);
        self.run_secs = self.run_secs.max(run_secs);
        self.cpu_secs += load.cores * secs;
        self.children_cpu_secs += children_cores * secs;
        self.read += load.read_per_sec * secs;
        self.written += load.written_per_sec * secs;
        let memory = load.memory + children_memory;
        self.peak = self.peak.max(memory);
        self.count += 1;
        let windows = os == OsFlag::Windows;
        let children = u32::try_from(self.children.len()).unwrap_or(u32::MAX);
        let read = self.read.round() as u64;
        let written = self.written.round() as u64;
        let zebrad = ProcessUsage {
            pid: self.pid,
            parent: None,
            name: if windows { "zebrad.exe" } else { "zebrad" }.to_owned(),
            started_at: self.started_at,
            cpu_secs: self.cpu_secs,
            memory_bytes: Some(load.memory),
            read_bytes: Some(read),
            written_bytes: Some(written),
            threads: (!windows).then_some(THREADS),
        };
        let members = std::iter::once(zebrad)
            .chain(self.children.iter().map(|child| {
                let age = at.duration_since(child.started_at).unwrap_or_default();
                ProcessUsage {
                    pid: child.pid,
                    parent: child.parent,
                    name: child.name.to_owned(),
                    started_at: child.started_at,
                    cpu_secs: child.cores * age.as_secs_f64(),
                    memory_bytes: Some(child.memory),
                    read_bytes: Some(0),
                    written_bytes: Some(0),
                    threads: (!windows).then_some(1),
                }
            }))
            .take(MAX_MEMBERS)
            .collect();
        let mut sample = NodeResources {
            run: self.run,
            at,
            run_secs: self.run_secs,
            platform: os.platform(),
            cpu_secs: self.cpu_secs + self.children_cpu_secs,
            cpu_cores: Some(load.cores + children_cores),
            memory_bytes: memory,
            peak_memory_bytes: self.peak,
            committed_bytes: windows.then(|| (memory as f64 * COMMIT_RATIO).round() as u64),
            read_bytes: Some(read),
            written_bytes: Some(written),
            processes: 1 + children,
            threads: (!windows).then_some(THREADS + children),
            descriptors: Some(if windows {
                HANDLES + CHILD_HANDLES * children
            } else {
                OPEN_FILES + CHILD_FILES * children
            }),
            members,
        };
        if flags.unreadable {
            // As the backend reports a tree whose CPU and I/O the OS refuses to read.
            sample.cpu_secs = 0.0;
            sample.cpu_cores = None;
            sample.read_bytes = None;
            sample.written_bytes = None;
            for member in &mut sample.members {
                member.cpu_secs = 0.0;
                member.read_bytes = None;
                member.written_bytes = None;
            }
        }
        sample
    }
}

/// The computer at sample `count`, with the node's newest sample (`None` while it has no
/// run). The first sample has no busy figure, like the backend's.
pub(super) fn machine(
    at: SystemTime,
    count: u64,
    os: OsFlag,
    node: Option<&NodeResources>,
) -> MachineResources {
    let others = 0.75 + 0.15 * jitter(count, 5);
    let node_cores = node.and_then(|node| node.cpu_cores).unwrap_or(0.0);
    let node_memory = node.map_or(0, |node| node.memory_bytes);
    let total = gib(MEMORY_TOTAL_GIB);
    let used = node_memory + gib(OTHER_MEMORY_GIB);
    let windows = os == OsFlag::Windows;
    let swap = (!windows).then_some((gib(SWAP_TOTAL_GIB), gib(SWAP_USED_GIB)));
    MachineResources {
        at,
        logical_cpus: LOGICAL_CPUS,
        cpu_busy_cores: (count > 0)
            .then(|| (node_cores + others).clamp(0.0, f64::from(LOGICAL_CPUS))),
        memory_total_bytes: total,
        memory_available_bytes: Some(total.saturating_sub(used)),
        swap_total_bytes: swap.map(|(total, _)| total),
        swap_used_bytes: swap.map(|(_, used)| used),
        commit_bytes: windows.then(|| {
            gib(OTHER_COMMIT_GIB) + node.and_then(|node| node.committed_bytes).unwrap_or(0)
        }),
        commit_limit_bytes: windows.then(|| gib(COMMIT_LIMIT_GIB)),
        pressure: (os == OsFlag::Macos).then_some(MemoryPressure::Normal),
    }
}

impl super::Mock {
    /// Every second: a new run publishes its first sample at once, a running one every
    /// [`INTERVAL`], and a node without a pid publishes `None`, as does a run whose reader
    /// panicked (`?readerpanic`); the computer publishes every [`INTERVAL`], from the page's
    /// load, and sees the node's use either way.
    pub(super) fn tick_resources(&self) {
        let now = clock::now();
        let second = self
            .resource_seconds
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (pid, started_at, stopping) = self.status.with_untracked(|status| {
            (
                status.pid,
                status.started_at,
                status.phase == Phase::Stopping,
            )
        });
        let at_tip = self.stats.with_untracked(|stats| {
            stats.as_ref().is_some_and(|stats| {
                stats.estimated_height.saturating_sub(stats.height) <= 1
                    || stats.verification_progress >= 0.9999
            })
        });
        let tree = self.tree.get_untracked();
        // A sample the node's sampler did not publish, which the computer still sees.
        let mut unpublished = None;
        match (pid, started_at) {
            (Some(pid), Some(started_at)) => {
                let new_run = tree.as_ref().is_none_or(|tree| tree.pid != pid);
                let due = second.is_multiple_of(INTERVAL.as_secs());
                if new_run || due {
                    let mut tree = match tree {
                        Some(tree) if !new_run => tree,
                        previous => {
                            MockTree::new(previous.map_or(1, |tree| tree.run + 1), pid, started_at)
                        }
                    };
                    let run_secs = now
                        .duration_since(started_at)
                        .map_or(0.0, |since| since.as_secs_f64());
                    let state = RunState {
                        run_secs,
                        stopping,
                        at_tip,
                        scenario: self.flags.scenario(),
                        count: tree.count,
                    };
                    let sample = tree.sample(now, run_secs, &load(&state), self.flags);
                    let panicked = reader_panicked(self.flags, tree.run, run_secs);
                    self.tree.set(Some(tree));
                    if !panicked {
                        self.resources.set(Some(sample));
                    } else {
                        if self.resources.with_untracked(Option::is_some) {
                            self.resources.set(None);
                        }
                        unpublished = Some(sample);
                    }
                }
            }
            _ => {
                if self.resources.with_untracked(Option::is_some) {
                    self.resources.set(None);
                }
                // The next run starts its own counters; its id follows this run's.
                if let Some(tree) = tree.filter(|tree| tree.pid != 0) {
                    self.tree.set(Some(MockTree { pid: 0, ..tree }));
                }
            }
        }
        if second.is_multiple_of(INTERVAL.as_secs()) {
            let count = second / INTERVAL.as_secs();
            let sample = match &unpublished {
                Some(node) => machine(now, count, self.flags.os, Some(node)),
                None => self
                    .resources
                    .with_untracked(|node| machine(now, count, self.flags.os, node.as_ref())),
            };
            self.machine.set(Some(sample));
        }
    }
}

/// Calls `handler` with every change of the node's resources, starting with the current value.
pub fn on_node_resources(handler: impl Fn(Option<NodeResources>) + 'static) {
    let resources = mock().resources;
    Effect::new(move |_| handler(resources.get()));
}

pub async fn get_node_resources() -> Option<NodeResources> {
    sleep(50).await;
    mock().resources.get_untracked()
}

/// Calls `handler` with every change of the computer's resources, starting with the current
/// value.
pub fn on_machine_resources(handler: impl Fn(Option<MachineResources>) + 'static) {
    let machine = mock().machine;
    Effect::new(move |_| handler(machine.get()));
}

pub async fn get_machine_resources() -> Option<MachineResources> {
    sleep(50).await;
    mock().machine.get_untracked()
}

#[cfg(test)]
mod tests {
    use std::time::UNIX_EPOCH;

    use super::*;

    /// The most state memory the mock's metrics report (memtables and block cache).
    const STATE_MEMORY_MAX: u64 = 1_200_000_000 + 11 * 8_000_000 + 420_000_000 + 8 * 3_000_000;

    fn state(run_secs: f64, at_tip: bool, scenario: Scenario) -> RunState {
        RunState {
            run_secs,
            stopping: false,
            at_tip,
            scenario,
            count: run_secs as u64 / 5,
        }
    }

    #[test]
    fn jitter_is_repeatable_and_bounded() {
        assert_eq!(jitter(7, 2), jitter(7, 2));
        assert_ne!(jitter(7, 2), jitter(8, 2));
        assert_ne!(jitter(7, 2), jitter(7, 3));
        for count in 0..10_000 {
            let j = jitter(count, 4);
            assert!((-1.0..1.0).contains(&j), "{j}");
        }
    }

    #[test]
    fn memory_stays_above_state_memory() {
        for scenario in [Scenario::Normal, Scenario::Stalled, Scenario::NoPeers] {
            for at_tip in [false, true] {
                for secs in (0..4 * 3600).step_by(5) {
                    let load = load(&state(f64::from(secs), at_tip, scenario));
                    assert!(
                        load.memory > STATE_MEMORY_MAX,
                        "{scenario:?} at_tip={at_tip} {secs} s: {}",
                        load.memory
                    );
                }
            }
        }
        // The climb reaches 6.4 GiB in 30 min, then stays within the sawtooth.
        let peak = load(&state(CLIMB_SECS - 1.0, false, Scenario::Normal)).memory as f64 / GIB;
        assert!((6.3..6.5).contains(&peak), "{peak}");
        for secs in (1800..7200).step_by(5) {
            let memory = load(&state(f64::from(secs), false, Scenario::Normal)).memory as f64;
            assert!(
                (6.09..6.71).contains(&(memory / GIB)),
                "{secs} s: {}",
                memory / GIB
            );
        }
    }

    #[test]
    fn loads_follow_the_run() {
        let opening = load(&state(3.0, false, Scenario::Normal));
        assert!((1.0..1.2).contains(&opening.cores), "{opening:?}");
        assert!(opening.read_per_sec >= 170.0 * MB, "{opening:?}");
        let syncing = load(&state(600.0, false, Scenario::Normal));
        assert!((4.8..=6.4).contains(&syncing.cores), "{syncing:?}");
        assert!(syncing.written_per_sec >= 85.0 * MB, "{syncing:?}");
        let tip = load(&state(600.0, true, Scenario::Normal));
        assert!((0.2..0.3).contains(&tip.cores), "{tip:?}");
        assert!((3.1..3.3).contains(&(tip.memory as f64 / GIB)), "{tip:?}");
        for scenario in [Scenario::Stalled, Scenario::NoPeers] {
            let stuck = load(&state(600.0, false, scenario));
            assert!(stuck.cores < 0.15, "{scenario:?}: {stuck:?}");
            assert!(stuck.written_per_sec < MB, "{scenario:?}: {stuck:?}");
        }
        let stopping = load(&RunState {
            stopping: true,
            ..state(600.0, false, Scenario::Normal)
        });
        assert!((0.5..0.7).contains(&stopping.cores), "{stopping:?}");
    }

    #[test]
    fn a_run_counts_from_its_spawn() {
        let start = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let mut tree = MockTree::new(3, 4242, start);
        let opening = load(&state(0.5, false, Scenario::Normal));
        let first = tree.sample(start, 0.5, &opening, Flags::default());
        assert_eq!(first.run, 3);
        assert_eq!(first.platform, Platform::Linux);
        assert_eq!(first.processes, 1);
        assert_eq!(first.members[0].pid, 4242);
        assert_eq!(first.members[0].name, "zebrad");
        assert_eq!(first.threads, Some(THREADS));
        assert_eq!(first.committed_bytes, None);
        let syncing = load(&state(5.5, false, Scenario::Normal));
        let second = tree.sample(
            start + Duration::from_secs(5),
            5.5,
            &syncing,
            Flags::default(),
        );
        assert!(second.cpu_secs > first.cpu_secs);
        assert!(second.written_bytes > first.written_bytes);
        assert!(second.peak_memory_bytes >= second.memory_bytes);
        assert_eq!(second.members[0].cpu_secs, second.cpu_secs);

        let windows = tree.sample(
            start + Duration::from_secs(10),
            10.5,
            &syncing,
            Flags {
                os: OsFlag::Windows,
                ..Flags::default()
            },
        );
        assert_eq!(windows.platform, Platform::Windows);
        assert_eq!(windows.threads, None);
        assert_eq!(windows.descriptors, Some(HANDLES));
        assert_eq!(windows.members[0].name, "zebrad.exe");
        assert!(windows.committed_bytes.unwrap() > windows.memory_bytes);
    }

    #[test]
    fn children_come_and_go_in_cycles_of_three() {
        let start = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let flags = Flags {
            children: true,
            ..Flags::default()
        };
        let syncing = load(&state(600.0, false, Scenario::Normal));
        let mut tree = MockTree::new(1, 4242, start);
        let samples: Vec<NodeResources> = (0..5u32)
            .map(|i| {
                let at = start + Duration::from_secs(5 * u64::from(i));
                tree.sample(at, 5.0 * f64::from(i), &syncing, flags)
            })
            .collect();
        let tree_of = |sample: &NodeResources| -> Vec<(String, Option<u32>)> {
            sample
                .members
                .iter()
                .map(|member| (member.name.clone(), member.parent))
                .collect()
        };
        let owned = |rows: &[(&str, Option<u32>)]| -> Vec<(String, Option<u32>)> {
            rows.iter()
                .map(|(name, parent)| ((*name).to_owned(), *parent))
                .collect()
        };
        assert_eq!(samples[0].processes, 1, "none at the run's first sample");
        assert_eq!(
            tree_of(&samples[1]),
            owned(&[
                ("zebrad", None),
                ("sh", Some(4242)),
                ("notify-block", Some(4243))
            ])
        );
        // notify-block's curls: more than a sample lists.
        assert_eq!(samples[2].processes, 3 + CURLS);
        assert_eq!(samples[2].members.len(), MAX_MEMBERS);
        assert!(
            samples[2].members[3..]
                .iter()
                .all(|member| member.name == "curl" && member.parent == Some(4244))
        );
        // sh exited: notify-block stays, with no parent in the tree.
        assert_eq!(
            tree_of(&samples[3]),
            owned(&[("zebrad", None), ("notify-block", None)])
        );
        assert_eq!(samples[3].members[1].pid, samples[1].members[2].pid);
        assert_eq!(
            samples[3].members[1].started_at,
            samples[1].members[2].started_at
        );
        // The next cycle starts new processes.
        assert_eq!(samples[4].members[1].name, "sh");
        assert_ne!(samples[4].members[1].pid, samples[1].members[1].pid);

        // The tree's figures hold the children's; zebrad's row holds its own.
        let busy = &samples[2];
        assert!(busy.cpu_cores.unwrap() > syncing.cores);
        assert!(busy.memory_bytes > syncing.memory);
        assert_eq!(busy.members[0].memory_bytes, Some(syncing.memory));
        assert!(busy.cpu_secs > busy.members[0].cpu_secs);
        assert_eq!(busy.threads, Some(THREADS + 2 + CURLS));
        assert_eq!(
            busy.descriptors,
            Some(OPEN_FILES + CHILD_FILES * (2 + CURLS))
        );
        // Windows names its shell and programs as Windows does.
        let windows = block_notify(&[], 4242, 1, start, OsFlag::Windows);
        let names: Vec<&str> = windows.iter().map(|child| child.name).collect();
        assert_eq!(names, vec!["cmd.exe", "notify-block.exe"]);
    }

    #[test]
    fn the_reader_panics_once_in_the_first_run() {
        let flags = Flags {
            reader_panic: true,
            ..Flags::default()
        };
        assert!(!reader_panicked(flags, 1, 29.0));
        assert!(reader_panicked(flags, 1, 30.0));
        assert!(reader_panicked(flags, 1, 3600.0), "until the run ends");
        assert!(
            !reader_panicked(flags, 2, 3600.0),
            "the next run is measured"
        );
        assert!(!reader_panicked(Flags::default(), 1, 3600.0));
    }

    #[test]
    fn unreadable_counters_are_reported_as_none() {
        let start = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let flags = Flags {
            unreadable: true,
            ..Flags::default()
        };
        let mut tree = MockTree::new(1, 4242, start);
        let syncing = load(&state(600.0, false, Scenario::Normal));
        let sample = tree.sample(start + Duration::from_secs(5), 5.0, &syncing, flags);
        assert_eq!(sample.cpu_cores, None);
        assert_eq!(sample.cpu_secs, 0.0);
        assert_eq!((sample.read_bytes, sample.written_bytes), (None, None));
        let zebrad = &sample.members[0];
        assert_eq!(zebrad.cpu_secs, 0.0);
        assert_eq!((zebrad.read_bytes, zebrad.written_bytes), (None, None));
        assert_eq!(sample.memory_bytes, syncing.memory, "memory stays readable");
    }

    #[test]
    fn the_computer_holds_the_node() {
        let at = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let first = machine(at, 0, OsFlag::Linux, None);
        assert_eq!(
            first.cpu_busy_cores, None,
            "no busy figure before a second reading"
        );
        assert_eq!(first.logical_cpus, LOGICAL_CPUS);
        let start = at - Duration::from_secs(600);
        let mut tree = MockTree::new(1, 7, start);
        let node = tree.sample(
            at,
            600.0,
            &load(&state(600.0, false, Scenario::Normal)),
            Flags::default(),
        );
        let linux = machine(at, 3, OsFlag::Linux, Some(&node));
        let busy = linux.cpu_busy_cores.unwrap();
        assert!(busy > node.cpu_cores.unwrap() && busy <= f64::from(LOGICAL_CPUS));
        let available = linux.memory_available_bytes.unwrap();
        assert_eq!(
            available,
            linux.memory_total_bytes - node.memory_bytes - gib(OTHER_MEMORY_GIB)
        );
        assert_eq!(linux.swap_total_bytes, Some(gib(SWAP_TOTAL_GIB)));
        assert_eq!(linux.pressure, None);
        assert_eq!(linux.commit_bytes, None);

        let mac = machine(at, 3, OsFlag::Macos, Some(&node));
        assert_eq!(mac.pressure, Some(MemoryPressure::Normal));
        let windows = machine(at, 3, OsFlag::Windows, Some(&node));
        assert_eq!(windows.swap_total_bytes, None);
        assert_eq!(windows.commit_limit_bytes, Some(gib(COMMIT_LIMIT_GIB)));
        assert!(windows.commit_bytes.unwrap() >= gib(OTHER_COMMIT_GIB));
    }

    #[test]
    fn os_values() {
        assert_eq!(OsFlag::parse(None), OsFlag::Linux);
        assert_eq!(OsFlag::parse(Some("macos")), OsFlag::Macos);
        assert_eq!(OsFlag::parse(Some("windows")), OsFlag::Windows);
        assert_eq!(OsFlag::parse(Some("beos")), OsFlag::Linux);
    }
}
