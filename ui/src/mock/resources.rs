//! The mock's resource samples: each node's process tree every 5 s while it has a pid
//! (Starting, Running and Stopping, like the backend's samplers, which do not wait for
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
//! (memory pressure on macOS; commit, handles, and no thread counts or parent links on
//! Windows, whose reader takes the tree from the job).
//! `?children` gives zebrad a `block_notify_command` that runs in cycles of three samples
//! ([`block_notify`]), `?unreadable` reports CPU and I/O as refused by the OS, and
//! `?readerpanic` stops the samples 30 s into the first run while zebrad runs on, as the
//! backend does after a reader panic ([`reader_panicked`]). `?backfill` starts zebrad's run
//! 45 minutes before the page loaded and hands each new listener the samples of the 40
//! minutes before the load first ([`backfill`]).
//!
//! The herd's nodes ([`super::herd`]: `?herd` and the nodes the add-node dialog adds) have
//! samples of their own ([`NodeSamples`]), with docs/RESOURCES.md's figures where it gives
//! them ([`herd_load`]):
//!
//! | Node | Uses |
//! | --- | --- |
//! | Zebra · Testnet, at the tip | 0.15 cores, 1.9 GiB |
//! | Zakura · Testnet, pruned, at the tip | 0.1 cores, 1.2 GiB |
//! | Zakura · Mainnet, catching up | 3.2 cores, 4.4 GiB; once at the tip, as zebrad there |
//! | Zaino · Mainnet | 0.3 cores, 650 MiB |
//! | zcashd · Mainnet | stopped, so no sample; started, 0.2 cores and 2.4 GiB |
//! | A zebrad the dialog added, at the tip | as the main zebrad at the tip on Mainnet, as Zebra · Testnet on Testnet |
//!
//! With `?herd` the computer has 16 logical CPUs and 32 GiB, so the herd fits without a memory
//! warning; its figures count every node's newest sample.
//!
//! The computer's samples list the volumes that hold the nodes' data folders ([`volumes`]),
//! read every 30 s: `/` with the launcher's data folder (212.4 GB free of 1,000.2 GB),
//! `/mnt/storage` with the folder picker's (1,834.5 GB of 4,000.8 GB), `/mnt/zcash` with the
//! herd's (3,412.7 GB of 8,001.6 GB), and for any other folder its first two parts (120 GB of
//! 500.1 GB). `?lowdisk` leaves 1.4 GB on the main node's volume, `?lowdisk=low` 12 GB and
//! `?nofit` 40 GB; `?lowmem` leaves 4 % of the memory available, with critical pressure on
//! macOS and the commit charge at 96 % of its limit on Windows, `?lowmem=low` 8 %,
//! `?lowmem=commit` only the commit charge at 96 %, and `?lowmem=restart` 4 % of an 8 GiB
//! computer ([`LowMemory`], [`short_of_memory`]).

use std::{
    cell::RefCell,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime},
};

use launcher_types::{
    MAX_MEMBERS, MachineResources, MemoryPressure, Network, NodeId, NodeKind, NodeResources, Phase,
    Platform, ProcessUsage, Status, VolumeUsage,
};
use leptos::prelude::*;

use super::{
    DEFAULT_DATA_DIR, FREE_AT_DEFAULT, FREE_AT_PICKED, FREE_ELSEWHERE, Flags, PICKED_DATA_DIR,
    Scenario, mock, nodes, sleep,
};
use crate::clock;

/// `?os=`: the OS whose figures the mock reports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum OsFlag {
    #[default]
    Linux,
    Macos,
    Windows,
}

/// `?lowdisk=…`: how little the main node's volume has free ([`volumes`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LowDisk {
    /// `?lowdisk`: [`LOW_DISK_FREE`], rule 19's problem at once.
    Full,
    /// `?lowdisk=low`: [`LOW_DISK_SPACE`], rule 19's warning after a minute.
    Low,
}

impl LowDisk {
    /// The value of `?lowdisk=`; anything else is `Full`.
    pub(super) fn parse(value: Option<&str>) -> Self {
        match value {
            Some("low") => LowDisk::Low,
            _ => LowDisk::Full,
        }
    }
}

/// `?lowmem=…`: what the computer is short of ([`short_of_memory`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LowMemory {
    /// `?lowmem`: 4 % of the memory available, critical pressure on macOS and the commit
    /// charge at 96 % of its limit on Windows: rule 20's problem.
    Exhausted,
    /// `?lowmem=low`: 8 % available, warning pressure on macOS: rule 20's warning.
    Low,
    /// `?lowmem=commit`: Windows' commit charge at 96 % of its limit with the memory available
    /// as usual: rule 20's problem for the commit charge. It brings `?os=windows` along.
    Commit,
    /// `?lowmem=restart`: as `?lowmem`, on an 8 GiB computer ([`SMALL_COMPUTER`]), where the
    /// syncing zebrad holds most of the memory, as in zebra#9699: the verdict offers Restart.
    Restart,
}

impl LowMemory {
    /// The value of `?lowmem=`; anything else is `Exhausted`.
    pub(super) fn parse(value: Option<&str>) -> Self {
        match value {
            Some("low") => LowMemory::Low,
            Some("commit") => LowMemory::Commit,
            Some("restart") => LowMemory::Restart,
            _ => LowMemory::Exhausted,
        }
    }

    /// Whether the system kills zebrad for want of memory, as the OOM killer and jetsam do.
    pub(super) fn kills(self) -> bool {
        matches!(self, LowMemory::Exhausted | LowMemory::Restart)
    }
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

/// `?backfill`: how long before the page loaded zebrad's run started, and how much of it the
/// samples that reach the page at once cover.
const BACKFILL_RUN: Duration = Duration::from_secs(45 * 60);
const BACKFILL: Duration = Duration::from_secs(40 * 60);

/// How long memory climbs while syncing, and the period of the sawtooth after that.
const CLIMB_SECS: f64 = 30.0 * 60.0;
const SAWTOOTH_SECS: f64 = 4.0 * 60.0;

/// The computer the mock runs on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Computer {
    pub logical_cpus: u32,
    pub memory_gib: f64,
    /// Windows: the commit limit.
    pub commit_limit_gib: f64,
}

/// One node's computer: 8 logical CPUs and 16 GiB.
pub(super) const ONE_NODE: Computer = Computer {
    logical_cpus: 8,
    memory_gib: 16.0,
    commit_limit_gib: 24.0,
};

/// `?herd`'s computer: 16 logical CPUs and 32 GiB, so the herd fits without a memory warning.
pub(super) const HERD_COMPUTER: Computer = Computer {
    logical_cpus: 16,
    memory_gib: 32.0,
    commit_limit_gib: 48.0,
};

/// `?lowmem=restart`'s computer: 8 logical CPUs and 8 GiB, the size zebra#9699 ran out on.
pub(super) const SMALL_COMPUTER: Computer = Computer {
    logical_cpus: 8,
    memory_gib: 8.0,
    commit_limit_gib: 12.0,
};

/// The computer `flags` ask for: [`SMALL_COMPUTER`] with `?lowmem=restart`, [`HERD_COMPUTER`]
/// with `?herd`, [`ONE_NODE`] otherwise.
fn computer(flags: Flags) -> Computer {
    if flags.low_memory == Some(LowMemory::Restart) {
        SMALL_COMPUTER
    } else if flags.herd {
        HERD_COMPUTER
    } else {
        ONE_NODE
    }
}

/// What the other programs on the computer use.
const OTHER_MEMORY_GIB: f64 = 5.2;
const SWAP_TOTAL_GIB: f64 = 2.0;
const SWAP_USED_GIB: f64 = 0.3;
/// Windows: what the other programs commit.
const OTHER_COMMIT_GIB: f64 = 7.0;
/// Windows: zebrad's commit charge for its working set.
const COMMIT_RATIO: f64 = 1.15;

/// zebrad's threads and open files (handles on Windows), always.
const THREADS: u32 = 68;
const OPEN_FILES: u32 = 412;
const HANDLES: u32 = 1204;

/// The first process of a node's tree, with its threads and open files (handles on Windows).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Root {
    /// As Linux and macOS name it; Windows adds `.exe`.
    pub name: &'static str,
    pub threads: u32,
    pub files: u32,
    pub handles: u32,
}

/// The mock's zebrad.
const ZEBRAD: Root = Root {
    name: "zebrad",
    threads: THREADS,
    files: OPEN_FILES,
    handles: HANDLES,
};

/// The first process of a `kind` node's tree.
pub(super) fn root_of(kind: NodeKind) -> Root {
    let program = kind.program().unwrap_or("program");
    let (threads, files, handles) = match kind {
        NodeKind::Zebrad => return ZEBRAD,
        NodeKind::Zakura => (64, 380, 1100),
        NodeKind::Zcashd => (40, 210, 620),
        NodeKind::Zaino => (24, 96, 260),
        NodeKind::Zebend | NodeKind::Zallet | NodeKind::Generic => (12, 48, 180),
    };
    Root {
        name: program,
        threads,
        files,
        handles,
    }
}

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

/// One run of a mock node, as the backend's fold would see it: counters since the spawn and
/// the peak.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct MockTree {
    pub run: u64,
    pub pid: u32,
    pub started_at: SystemTime,
    /// The process the run started: zebrad, or a herd node's program.
    root: Root,
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
    /// A run of the main zebrad.
    pub(super) fn new(run: u64, pid: u32, started_at: SystemTime) -> Self {
        Self::with_root(ZEBRAD, run, pid, started_at)
    }

    /// A run that starts `root`.
    pub(super) fn with_root(root: Root, run: u64, pid: u32, started_at: SystemTime) -> Self {
        MockTree {
            run,
            pid,
            started_at,
            root,
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
        let root = ProcessUsage {
            pid: self.pid,
            parent: None,
            name: if windows {
                format!("{}.exe", self.root.name)
            } else {
                self.root.name.to_owned()
            },
            started_at: self.started_at,
            cpu_secs: self.cpu_secs,
            memory_bytes: Some(load.memory),
            read_bytes: Some(read),
            written_bytes: Some(written),
            threads: (!windows).then_some(self.root.threads),
        };
        let members = std::iter::once(root)
            .chain(self.children.iter().map(|child| {
                let age = at.duration_since(child.started_at).unwrap_or_default();
                ProcessUsage {
                    pid: child.pid,
                    // Windows reads no parent pids: the job's list is the tree.
                    parent: child.parent.filter(|_| !windows),
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
            threads: (!windows).then_some(self.root.threads + children),
            descriptors: Some(if windows {
                self.root.handles + CHILD_HANDLES * children
            } else {
                self.root.files + CHILD_FILES * children
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

/// What a herd node uses over one interval ([`super::herd`]), from docs/RESOURCES.md's mock
/// data where it gives figures: `catching_up` says its sync is behind the tip.
pub(super) fn herd_load(kind: NodeKind, network: Network, catching_up: bool, count: u64) -> Load {
    let j = |series| jitter(count, series);
    // Cores, GiB, MB/s read and written.
    let (cores, memory, read, written) = match (kind, network) {
        (NodeKind::Zakura, _) if catching_up => (3.2, 4.4, 20.0, 60.0),
        (NodeKind::Zakura, Network::Testnet) => (0.1, 1.2, 0.05, 0.2),
        (NodeKind::Zebrad, Network::Testnet) => (0.15, 1.9, 0.05, 0.2),
        // At the tip on Mainnet, like the main zebrad there.
        (NodeKind::Zebrad | NodeKind::Zakura, Network::Mainnet) => (0.25, 3.2, 0.1, 0.4),
        (NodeKind::Zaino, _) => (0.3, 650.0 / 1024.0, 1.0, 0.5),
        (NodeKind::Zcashd, _) => (0.2, 2.4, 0.1, 0.3),
        (NodeKind::Zebend | NodeKind::Zallet | NodeKind::Generic, _) => (0.1, 0.5, 0.05, 0.1),
    };
    Load {
        cores: (cores * (1.0 + 0.1 * j(2))).max(0.0),
        read_per_sec: (read * (1.0 + 0.2 * j(3))).max(0.0) * MB,
        written_per_sec: (written * (1.0 + 0.2 * j(4))).max(0.0) * MB,
        memory: gib(memory * (1.0 + 0.01 * j(1))),
    }
}

thread_local! {
    /// The herd's nodes' newest samples, for the computer's figures: each node's signal, `None`
    /// while it has no run. Kept here rather than read from the herd, so the main zebrad's
    /// sampler never starts the herd's simulation.
    static HERD_SAMPLES: RefCell<Vec<ArcRwSignal<Option<NodeResources>>>> =
        const { RefCell::new(Vec::new()) };
    /// `?backfill`: the main zebrad's samples and the computer's from before the page loaded,
    /// oldest first.
    static BACKFILLED: RefCell<(Vec<NodeResources>, Vec<MachineResources>)> =
        const { RefCell::new((Vec::new(), Vec::new())) };
}

/// What `?backfill` hands each new listener first: zebrad's run, `pid` since `started_at`,
/// sampled every [`INTERVAL`] over the [`BACKFILL`] before `now` while it synced, with the
/// computer beside it; and the run as its sampler goes on from there. The run's counters count
/// from the spawn, so the first sample already holds its first minutes.
pub(super) fn backfill(
    flags: Flags,
    pid: u32,
    started_at: SystemTime,
    now: SystemTime,
    computer: Computer,
) -> (MockTree, Vec<NodeResources>, Vec<MachineResources>) {
    let mut tree = MockTree::new(1, pid, started_at);
    let from = now
        .checked_sub(BACKFILL)
        .and_then(|from| from.duration_since(started_at).ok())
        .unwrap_or_default()
        .as_secs_f64();
    let to = now
        .duration_since(started_at)
        .unwrap_or_default()
        .as_secs_f64();
    let step = INTERVAL.as_secs_f64();
    let (mut nodes, mut machines) = (Vec::new(), Vec::new());
    let mut run_secs = 0.0;
    // Up to the last interval before `now`, where the live sampler takes over.
    while run_secs + step <= to {
        let state = RunState {
            run_secs,
            stopping: false,
            at_tip: false,
            scenario: flags.scenario(),
            count: tree.count,
        };
        let at = started_at + Duration::from_secs_f64(run_secs);
        let sample = tree.sample(at, run_secs, &load(&state), flags);
        if run_secs >= from {
            let count = machines.len() as u64 + 1;
            machines.push(machine(at, count, flags.os, computer, &[&sample]));
            nodes.push(sample);
        }
        run_secs += step;
    }
    (tree, nodes, machines)
}

/// The main zebrad's samples from before the page loaded (`?backfill`), oldest first.
pub(super) fn backfilled_nodes() -> Vec<NodeResources> {
    BACKFILLED.with_borrow(|(nodes, _)| nodes.clone())
}

/// A herd node's resource samples: its runs and its newest sample.
#[derive(Clone)]
pub(super) struct NodeSamples {
    /// `None` while the node has no run.
    pub resources: ArcRwSignal<Option<NodeResources>>,
    /// The current or last run behind `resources`.
    tree: ArcRwSignal<Option<MockTree>>,
    /// Seconds of ticks so far, for the 5 s cadence.
    seconds: Arc<AtomicU64>,
}

impl NodeSamples {
    /// The samples of a node that has not run yet, counted in the computer's figures.
    pub(super) fn new() -> Self {
        let samples = NodeSamples {
            resources: ArcRwSignal::new(None),
            tree: ArcRwSignal::new(None),
            seconds: Arc::new(AtomicU64::new(0)),
        };
        HERD_SAMPLES.with_borrow_mut(|all| all.push(samples.resources.clone()));
        samples
    }

    /// Every second, like the main zebrad's sampler ([`super::Mock::tick_resources`]): a new run
    /// publishes its first sample at once, a running one every [`INTERVAL`], and a node without
    /// a pid publishes `None`. `?children` and `?readerpanic` are about the main zebrad only.
    pub(super) fn tick(
        &self,
        kind: NodeKind,
        network: Network,
        status: &Status,
        catching_up: bool,
        flags: Flags,
    ) {
        let second = self.seconds.fetch_add(1, Ordering::Relaxed);
        let tree = self.tree.get_untracked();
        let (Some(pid), Some(started_at)) = (status.pid, status.started_at) else {
            if self.resources.with_untracked(Option::is_some) {
                self.resources.set(None);
            }
            // The next run starts its own counters; its id follows this run's.
            if let Some(tree) = tree.filter(|tree| tree.pid != 0) {
                self.tree.set(Some(MockTree { pid: 0, ..tree }));
            }
            return;
        };
        let new_run = tree.as_ref().is_none_or(|tree| tree.pid != pid);
        if !new_run && !second.is_multiple_of(INTERVAL.as_secs()) {
            return;
        }
        let mut tree = match tree {
            Some(tree) if !new_run => tree,
            previous => MockTree::with_root(
                root_of(kind),
                previous.map_or(1, |tree| tree.run + 1),
                pid,
                started_at,
            ),
        };
        let now = clock::now();
        let run_secs = now
            .duration_since(started_at)
            .map_or(0.0, |since| since.as_secs_f64());
        let load = herd_load(kind, network, catching_up, tree.count);
        let flags = Flags {
            children: false,
            ..flags
        };
        let sample = tree.sample(now, run_secs, &load, flags);
        self.tree.set(Some(tree));
        self.resources.set(Some(sample));
    }
}

/// The volume a mock data folder is on: its label, size and free space.
fn volume_of_dir(dir: &str) -> (String, u64, u64) {
    let path = std::path::Path::new(dir);
    if path.starts_with(DEFAULT_DATA_DIR) {
        ("/".to_owned(), 1_000_200_000_000, FREE_AT_DEFAULT)
    } else if path.starts_with(PICKED_DATA_DIR) {
        ("/mnt/storage".to_owned(), 4_000_800_000_000, FREE_AT_PICKED)
    } else if path.starts_with("/mnt/zcash") {
        (
            "/mnt/zcash".to_owned(),
            8_001_600_000_000,
            3_412_700_000_000,
        )
    } else {
        // The mock's folders are Unix paths on every host, so the label is
        // split on `/` rather than by `Path`, which joins with `\` on Windows.
        let label = format!(
            "/{}",
            dir.split('/')
                .filter(|part| !part.is_empty())
                .take(2)
                .collect::<Vec<_>>()
                .join("/")
        );
        (label, 500_100_000_000, FREE_ELSEWHERE)
    }
}

/// `?lowdisk`: what the main node's volume has free.
pub(super) const LOW_DISK_FREE: u64 = 1_400_000_000;
/// `?lowdisk=low`: what it has free, under rule 19's warning threshold of 20 GB on its
/// 1,000.2 GB and above the problem's 3 GB.
pub(super) const LOW_DISK_SPACE: u64 = 12_000_000_000;
/// `?nofit`: what the main node's volume has free while zebrad syncs Mainnet beside its
/// 248.3 GB of state.
const NO_FIT_FREE: u64 = 40_000_000_000;

/// The volumes that hold `dirs` (each node and its data folder, in the list's order), read at
/// the newest 30 s mark at or before `at`; the one that holds `main_dir` has what `?lowdisk`,
/// `?lowdisk=low` and `?nofit` leave.
pub(super) fn volumes(
    dirs: &[(NodeId, String)],
    main_dir: &str,
    flags: Flags,
    at: SystemTime,
) -> Vec<VolumeUsage> {
    let since_epoch = at
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let measured_at = at - Duration::from_secs(since_epoch % 30);
    let main_label = volume_of_dir(main_dir).0;
    let mut volumes: Vec<VolumeUsage> = Vec::new();
    for (node, dir) in dirs {
        let (label, total, free) = volume_of_dir(dir);
        if let Some(volume) = volumes.iter_mut().find(|volume| volume.label == label) {
            volume.nodes.push(node.clone());
            continue;
        }
        let free = if label != main_label {
            free
        } else if let Some(low) = flags.low_disk {
            match low {
                LowDisk::Full => LOW_DISK_FREE,
                LowDisk::Low => LOW_DISK_SPACE,
            }
        } else if flags.no_fit {
            NO_FIT_FREE
        } else {
            free
        };
        volumes.push(VolumeUsage {
            label,
            total_bytes: total,
            free_bytes: free,
            nodes: vec![node.clone()],
            measured_at,
        });
    }
    volumes
}

/// `?lowmem=…`: what other programs leave ([`LowMemory`]). `?lowmem` and `?lowmem=restart`
/// leave 4 % of the memory available, with critical pressure on macOS and a commit charge at
/// 96 % of its limit on Windows; `?lowmem=low` leaves 8 %, with warning pressure on macOS;
/// `?lowmem=commit` leaves the memory as it is and puts the commit charge at 96 %.
pub(super) fn short_of_memory(machine: MachineResources, low: LowMemory) -> MachineResources {
    let total = machine.memory_total_bytes;
    let committed = machine.commit_limit_bytes.map(|limit| limit / 100 * 96);
    match low {
        LowMemory::Exhausted | LowMemory::Restart => MachineResources {
            memory_available_bytes: Some(total / 25),
            pressure: machine.pressure.map(|_| MemoryPressure::Critical),
            commit_bytes: committed,
            ..machine
        },
        LowMemory::Low => MachineResources {
            memory_available_bytes: Some(total / 100 * 8),
            pressure: machine.pressure.map(|_| MemoryPressure::Warning),
            ..machine
        },
        LowMemory::Commit => MachineResources {
            commit_bytes: committed,
            ..machine
        },
    }
}

/// The computer as the page gets it: with `?lowmem` short of memory, and with the volumes of
/// the nodes' data folders now.
fn as_published(machine: MachineResources) -> MachineResources {
    let mock = mock();
    let machine = match mock.flags.low_memory {
        Some(low) => short_of_memory(machine, low),
        None => machine,
    };
    let dirs: Vec<(NodeId, String)> = nodes::list()
        .into_iter()
        .map(|info| (info.id, info.data_dir))
        .collect();
    let main_dir = mock.info.with_untracked(|info| info.data_dir.clone());
    MachineResources {
        volumes: volumes(&dirs, &main_dir, mock.flags, machine.at),
        ..machine
    }
}

/// The herd's nodes' newest samples, of those that have a run.
fn herd_samples() -> Vec<NodeResources> {
    HERD_SAMPLES.with_borrow(|all| all.iter().filter_map(ArcRwSignal::get_untracked).collect())
}

/// The computer at sample `count`, with the newest sample of each node that has a run. The
/// first sample has no busy figure, like the backend's.
pub(super) fn machine(
    at: SystemTime,
    count: u64,
    os: OsFlag,
    computer: Computer,
    nodes: &[&NodeResources],
) -> MachineResources {
    let others = 0.75 + 0.15 * jitter(count, 5);
    let node_cores: f64 = nodes.iter().filter_map(|node| node.cpu_cores).sum();
    let node_memory: u64 = nodes.iter().map(|node| node.memory_bytes).sum();
    let node_commit: u64 = nodes.iter().filter_map(|node| node.committed_bytes).sum();
    let total = gib(computer.memory_gib);
    let used = node_memory + gib(OTHER_MEMORY_GIB);
    let windows = os == OsFlag::Windows;
    let swap = (!windows).then_some((gib(SWAP_TOTAL_GIB), gib(SWAP_USED_GIB)));
    let cpus = computer.logical_cpus;
    MachineResources {
        at,
        logical_cpus: cpus,
        cpu_busy_cores: (count > 0).then(|| (node_cores + others).clamp(0.0, f64::from(cpus))),
        memory_total_bytes: total,
        memory_available_bytes: Some(total.saturating_sub(used)),
        swap_total_bytes: swap.map(|(total, _)| total),
        swap_used_bytes: swap.map(|(_, used)| used),
        commit_bytes: windows.then(|| gib(OTHER_COMMIT_GIB) + node_commit),
        commit_limit_bytes: windows.then(|| gib(computer.commit_limit_gib)),
        pressure: (os == OsFlag::Macos).then_some(MemoryPressure::Normal),
        volumes: Vec::new(),
    }
}

impl super::Mock {
    /// `?backfill`: starts the run [`BACKFILL_RUN`] ago ([`super::Mock::start_at`], so the
    /// launcher's note is stamped then too) and has it running; its samples and the
    /// computer's of the [`BACKFILL`] before now wait for the listeners ([`backfill`]), and
    /// the samplers go on from there.
    pub(super) fn backfill(&self) {
        let now = clock::now();
        let started_at = now.checked_sub(BACKFILL_RUN).unwrap_or(now);
        self.start_at(started_at);
        let Some(pid) = self.status.with_untracked(|status| status.pid) else {
            return;
        };
        self.status.update(|status| {
            status.phase = Phase::Running;
            status.rpc_addr = "127.0.0.1:8232".parse().ok();
            status.metrics_addr = "127.0.0.1:9999".parse().ok();
        });
        let (tree, nodes, machines) =
            backfill(self.flags, pid, started_at, now, computer(self.flags));
        // The computer's count goes on, so its first live sample has a busy figure.
        self.resource_seconds.store(
            machines.len() as u64 * INTERVAL.as_secs(),
            Ordering::Relaxed,
        );
        self.tree.set(Some(tree));
        BACKFILLED.with_borrow_mut(|backfilled| *backfilled = (nodes, machines));
        // The RPC answers at once, as it would have for 45 minutes: as soon as the mock is set
        // up, since the statistics read the mock's other parts.
        let mock = self.clone();
        set_timeout(move || mock.tick_stats(), Duration::ZERO);
    }

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
            let main = unpublished.or_else(|| self.resources.get_untracked());
            let herd = herd_samples();
            let nodes: Vec<&NodeResources> = main.iter().chain(&herd).collect();
            self.machine.set(Some(machine(
                now,
                count,
                self.flags.os,
                computer(self.flags),
                &nodes,
            )));
        }
    }
}

/// The main zebrad's newest sample; [`super::nodes`] sends its changes to the UI.
pub async fn get_node_resources() -> Option<NodeResources> {
    sleep(50).await;
    mock().resources.get_untracked()
}

/// Calls `handler` with every change of the computer's resources, starting with the current
/// value; with `?backfill`, its samples from before the page loaded come first.
pub fn on_machine_resources(handler: impl Fn(Option<MachineResources>) + 'static) {
    let machine = mock().machine;
    for sample in BACKFILLED.with_borrow(|(_, machines)| machines.clone()) {
        handler(Some(as_published(sample)));
    }
    Effect::new(move |_| handler(machine.get().map(as_published)));
}

pub async fn get_machine_resources() -> Option<MachineResources> {
    sleep(50).await;
    mock().machine.get_untracked().map(as_published)
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

        // Windows lists the job's processes without parent links, by start time.
        let mut tree = MockTree::new(1, 4242, start);
        let flags = Flags {
            os: OsFlag::Windows,
            children: true,
            ..Flags::default()
        };
        tree.sample(start, 0.0, &syncing, flags);
        let children = tree.sample(start + Duration::from_secs(5), 5.0, &syncing, flags);
        let rows: Vec<(&str, Option<u32>)> = children
            .members
            .iter()
            .map(|member| (member.name.as_str(), member.parent))
            .collect();
        assert_eq!(
            rows,
            vec![
                ("zebrad.exe", None),
                ("cmd.exe", None),
                ("notify-block.exe", None)
            ]
        );
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
        let first = machine(at, 0, OsFlag::Linux, ONE_NODE, &[]);
        assert_eq!(
            first.cpu_busy_cores, None,
            "no busy figure before a second reading"
        );
        assert_eq!(first.logical_cpus, ONE_NODE.logical_cpus);
        let start = at - Duration::from_secs(600);
        let mut tree = MockTree::new(1, 7, start);
        let node = tree.sample(
            at,
            600.0,
            &load(&state(600.0, false, Scenario::Normal)),
            Flags::default(),
        );
        let linux = machine(at, 3, OsFlag::Linux, ONE_NODE, &[&node]);
        let busy = linux.cpu_busy_cores.unwrap();
        assert!(busy > node.cpu_cores.unwrap() && busy <= f64::from(ONE_NODE.logical_cpus));
        let available = linux.memory_available_bytes.unwrap();
        assert_eq!(
            available,
            linux.memory_total_bytes - node.memory_bytes - gib(OTHER_MEMORY_GIB)
        );
        assert_eq!(linux.swap_total_bytes, Some(gib(SWAP_TOTAL_GIB)));
        assert_eq!(linux.pressure, None);
        assert_eq!(linux.commit_bytes, None);

        let mac = machine(at, 3, OsFlag::Macos, ONE_NODE, &[&node]);
        assert_eq!(mac.pressure, Some(MemoryPressure::Normal));
        let windows = machine(at, 3, OsFlag::Windows, ONE_NODE, &[&node]);
        assert_eq!(windows.swap_total_bytes, None);
        assert_eq!(
            windows.commit_limit_bytes,
            Some(gib(ONE_NODE.commit_limit_gib))
        );
        assert!(windows.commit_bytes.unwrap() >= gib(OTHER_COMMIT_GIB));
    }

    #[test]
    fn the_herds_nodes_use_the_designs_figures() {
        let gib_of = |load: &Load| load.memory as f64 / GIB;
        for count in 0..200 {
            let zebra = herd_load(NodeKind::Zebrad, Network::Testnet, false, count);
            assert!((0.13..0.17).contains(&zebra.cores), "{zebra:?}");
            assert!((1.85..1.95).contains(&gib_of(&zebra)), "{zebra:?}");
            let pruned = herd_load(NodeKind::Zakura, Network::Testnet, false, count);
            assert!((0.09..0.11).contains(&pruned.cores), "{pruned:?}");
            assert!((1.18..1.22).contains(&gib_of(&pruned)), "{pruned:?}");
            let catching_up = herd_load(NodeKind::Zakura, Network::Mainnet, true, count);
            assert!((2.8..3.6).contains(&catching_up.cores), "{catching_up:?}");
            assert!(
                (4.3..4.5).contains(&gib_of(&catching_up)),
                "{catching_up:?}"
            );
            let indexer = herd_load(NodeKind::Zaino, Network::Mainnet, false, count);
            assert!((0.27..0.33).contains(&indexer.cores), "{indexer:?}");
            let mib = indexer.memory as f64 / MIB;
            assert!((640.0..660.0).contains(&mib), "{indexer:?}");
        }
        // Caught up, Zakura uses what zebrad does at the tip.
        let caught_up = herd_load(NodeKind::Zakura, Network::Mainnet, false, 0);
        assert!(caught_up.cores < 0.3, "{caught_up:?}");
    }

    #[test]
    fn a_herd_nodes_tree_starts_with_its_program() {
        let start = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let mut tree = MockTree::with_root(root_of(NodeKind::Zakura), 1, 4310, start);
        let load = herd_load(NodeKind::Zakura, Network::Mainnet, true, 0);
        let sample = tree.sample(start + Duration::from_secs(5), 5.0, &load, Flags::default());
        assert_eq!(sample.members[0].name, "zakurad");
        assert_eq!(sample.members[0].pid, 4310);
        assert_eq!(sample.processes, 1);
        assert_eq!(sample.threads, Some(64));
        assert!(sample.cpu_secs > 10.0, "{sample:?}");
        let windows = tree.sample(
            start + Duration::from_secs(10),
            10.0,
            &load,
            Flags {
                os: OsFlag::Windows,
                ..Flags::default()
            },
        );
        assert_eq!(windows.members[0].name, "zakurad.exe");
        assert_eq!(windows.descriptors, Some(1100));
        assert_eq!(root_of(NodeKind::Zebrad), ZEBRAD);
        assert_eq!(root_of(NodeKind::Zaino).name, "zainod");
    }

    #[test]
    fn the_herd_fits_its_computer() {
        let at = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let start = at - Duration::from_secs(3600);
        // The main zebrad at its largest, syncing, beside the herd's running nodes.
        let main = MockTree::new(1, 7, start).sample(
            at,
            3600.0,
            &load(&state(3600.0, false, Scenario::Normal)),
            Flags::default(),
        );
        let herd: Vec<NodeResources> = [
            (NodeKind::Zebrad, Network::Testnet, false),
            (NodeKind::Zakura, Network::Testnet, false),
            (NodeKind::Zakura, Network::Mainnet, true),
            (NodeKind::Zaino, Network::Mainnet, false),
        ]
        .into_iter()
        .enumerate()
        .map(|(i, (kind, network, catching_up))| {
            let mut tree = MockTree::with_root(root_of(kind), 1, 4300 + i as u32, start);
            tree.sample(
                at,
                3600.0,
                &herd_load(kind, network, catching_up, 3),
                Flags::default(),
            )
        })
        .collect();
        let nodes: Vec<&NodeResources> = std::iter::once(&main).chain(&herd).collect();
        let computer = machine(at, 3, OsFlag::Linux, HERD_COMPUTER, &nodes);
        assert_eq!(computer.logical_cpus, 16);
        let busy = computer.cpu_busy_cores.unwrap();
        assert!((8.0..16.0).contains(&busy), "{busy}");
        let available = computer.memory_available_bytes.unwrap() as f64;
        let share = available / computer.memory_total_bytes as f64;
        assert!(share > 0.25, "no memory warning: {share}");
        // One node's computer could not hold them.
        let small = machine(at, 3, OsFlag::Linux, ONE_NODE, &nodes);
        assert!(small.memory_available_bytes.unwrap() < gib(1.0));
    }

    #[test]
    fn the_backfill_covers_the_forty_minutes_before_the_load() {
        let now = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let started_at = now - BACKFILL_RUN;
        let (tree, nodes, machines) = backfill(Flags::default(), 4242, started_at, now, ONE_NODE);
        assert_eq!(nodes.len(), 480, "every 5 s for 40 minutes");
        assert_eq!(machines.len(), nodes.len());
        assert_eq!(nodes[0].at, now - BACKFILL);
        assert_eq!(nodes[479].at, now - INTERVAL);
        assert!(nodes.iter().all(|sample| sample.run == 1));
        assert!(nodes.windows(2).all(
            |pair| pair[0].run_secs < pair[1].run_secs && pair[0].cpu_secs <= pair[1].cpu_secs
        ));
        // The counters hold the run's first five minutes, and the memory climbs.
        assert!(nodes[0].cpu_secs > 4.0 * 300.0, "{}", nodes[0].cpu_secs);
        assert!(nodes[0].memory_bytes < nodes[479].memory_bytes);
        assert!(
            machines
                .iter()
                .all(|machine| machine.cpu_busy_cores.is_some())
        );
        assert_eq!(machines[10].at, nodes[10].at);
        // The live sampler goes on with the same run.
        assert_eq!((tree.run, tree.pid, tree.count), (1, 4242, 540));
    }

    #[test]
    fn the_volumes_hold_the_nodes_folders() {
        let at = UNIX_EPOCH + Duration::from_secs(1_800_000_017);
        let id = |text: &str| NodeId::new(text).unwrap();
        let dirs = vec![
            (id("zebrad-mainnet"), DEFAULT_DATA_DIR.to_owned()),
            (id("zebrad-testnet"), "/mnt/zcash/zebrad-testnet".to_owned()),
            (id("zakura-mainnet"), "/mnt/zcash/zakura-mainnet".to_owned()),
            (
                id("zebrad-mainnet-2"),
                format!("{DEFAULT_DATA_DIR}/nodes/zebrad-mainnet-2"),
            ),
            (id("zebrad-testnet-2"), "/media/small-disk/zebra".to_owned()),
        ];
        let read = volumes(&dirs, DEFAULT_DATA_DIR, Flags::default(), at);
        let listed: Vec<(&str, usize, u64)> = read
            .iter()
            .map(|v| (v.label.as_str(), v.nodes.len(), v.free_bytes))
            .collect();
        assert_eq!(
            listed,
            vec![
                ("/", 2, FREE_AT_DEFAULT),
                ("/mnt/zcash", 2, 3_412_700_000_000),
                ("/media/small-disk", 1, FREE_ELSEWHERE)
            ]
        );
        assert_eq!(
            read[0].measured_at,
            UNIX_EPOCH + Duration::from_secs(1_800_000_000),
            "read at the 30 s mark"
        );
        // `?lowdisk` and `?nofit` are about the main node's volume only.
        let low = volumes(
            &dirs,
            DEFAULT_DATA_DIR,
            Flags {
                low_disk: Some(LowDisk::Full),
                ..Flags::default()
            },
            at,
        );
        assert_eq!(low[0].free_bytes, LOW_DISK_FREE);
        assert!(
            low[0].free_bytes < crate::health::resources::DISK_FULL_BYTES,
            "the verdict's problem"
        );
        assert_eq!(low[1].free_bytes, 3_412_700_000_000);
        // `?lowdisk=low`: under the warning's threshold, min(5 % of the volume, 20 GB), and
        // above where the problem clears.
        let tight = volumes(
            &dirs,
            DEFAULT_DATA_DIR,
            Flags {
                low_disk: Some(LowDisk::Low),
                ..Flags::default()
            },
            at,
        );
        let threshold = ((tight[0].total_bytes as f64 * crate::health::resources::DISK_LOW_SHARE)
            as u64)
            .min(crate::health::resources::DISK_LOW_BYTES);
        assert!(tight[0].free_bytes < threshold, "{threshold}");
        assert!(tight[0].free_bytes > crate::health::resources::DISK_FULL_CLEARS);
        let tight = volumes(
            &dirs,
            "/mnt/zcash/zebrad-testnet",
            Flags {
                no_fit: true,
                ..Flags::default()
            },
            at,
        );
        assert_eq!(tight[1].free_bytes, NO_FIT_FREE);
        assert_eq!(tight[0].free_bytes, FREE_AT_DEFAULT);
    }

    #[test]
    fn short_of_memory_leaves_what_each_value_asks() {
        use crate::health::resources::{
            COMMIT_FULL_SHARE, MEMORY_LOW_SHARE, MEMORY_OUT_CLEARS, MEMORY_OUT_SHARE,
        };
        let at = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let short = |os, low| short_of_memory(machine(at, 3, os, ONE_NODE, &[]), low);
        let available = |machine: &MachineResources| {
            machine.memory_available_bytes.unwrap() as f64 / machine.memory_total_bytes as f64
        };
        let commit = |machine: &MachineResources| {
            machine.commit_bytes.unwrap() as f64 / machine.commit_limit_bytes.unwrap() as f64
        };
        // `?lowmem`: 4 %, critical pressure, the commit charge at 96 %.
        let linux = short(OsFlag::Linux, LowMemory::Exhausted);
        assert!((0.039..0.041).contains(&available(&linux)));
        assert!(available(&linux) < MEMORY_OUT_SHARE);
        assert_eq!(linux.pressure, None);
        let mac = short(OsFlag::Macos, LowMemory::Exhausted);
        assert_eq!(mac.pressure, Some(MemoryPressure::Critical));
        let windows = short(OsFlag::Windows, LowMemory::Exhausted);
        assert!(commit(&windows) > COMMIT_FULL_SHARE);
        // `?lowmem=low`: between the problem's leave threshold and the warning's.
        let low = short(OsFlag::Linux, LowMemory::Low);
        assert!(available(&low) > MEMORY_OUT_CLEARS && available(&low) < MEMORY_LOW_SHARE);
        let mac = short(OsFlag::Macos, LowMemory::Low);
        assert_eq!(mac.pressure, Some(MemoryPressure::Warning));
        let windows = short(OsFlag::Windows, LowMemory::Low);
        assert!(commit(&windows) < 0.93, "the commit charge as usual");
        // `?lowmem=commit`: the memory as usual, the commit charge at 96 %.
        let usual = machine(at, 3, OsFlag::Windows, ONE_NODE, &[]);
        let windows = short(OsFlag::Windows, LowMemory::Commit);
        assert_eq!(windows.memory_available_bytes, usual.memory_available_bytes);
        assert!(available(&windows) > MEMORY_LOW_SHARE);
        assert!(commit(&windows) > COMMIT_FULL_SHARE);
        // `?lowmem=restart`: the small computer, where the syncing zebrad has more than half.
        let flags = Flags::parse("?lowmem=restart");
        assert_eq!(computer(flags), SMALL_COMPUTER);
        assert_eq!(computer(Flags::parse("?lowmem&herd")), HERD_COMPUTER);
        let zebrad = load(&RunState {
            run_secs: 45.0 * 60.0,
            stopping: false,
            at_tip: false,
            scenario: Scenario::Normal,
            count: 540,
        });
        assert!(zebrad.memory * 2 >= gib(SMALL_COMPUTER.memory_gib));
        assert!(LowMemory::Restart.kills() && !LowMemory::Low.kills());
    }

    #[test]
    fn os_values() {
        assert_eq!(OsFlag::parse(None), OsFlag::Linux);
        assert_eq!(OsFlag::parse(Some("macos")), OsFlag::Macos);
        assert_eq!(OsFlag::parse(Some("windows")), OsFlag::Windows);
        assert_eq!(OsFlag::parse(Some("beos")), OsFlag::Linux);
    }
}
