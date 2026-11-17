//! Resource monitoring (docs/RESOURCES.md): what each node's process tree uses, and what the
//! computer has.
//!
//! - [`ResourceSampler`] follows the supervisor's [`TreeProbe`] watch: from the spawn until
//!   the run is finalized it reads the tree every [`INTERVAL`] and publishes
//!   `watch<Option<NodeResources>>`; between runs it publishes `None`.
//! - [`MachineSampler`] reads the computer's CPU and memory every [`INTERVAL`], and the volumes
//!   that hold the nodes' data folders every [`VOLUME_INTERVAL`], and publishes
//!   `watch<Option<MachineResources>>`.
//!
//! The reads are blocking system calls, so each one runs in `spawn_blocking`, taking its inputs
//! by value and returning an owned reading; each actor keeps its own state, and nothing is
//! shared. The per-OS readers implement [`Reader`] and [`MachineReader`]: the Linux one reads
//! `/proc`, the macOS one libproc and Mach's host statistics, and the Windows one the node's
//! job and a few calls per process. On other platforms reading is not supported, and the
//! samplers publish `None` and stop.
//!
//! ```text
//!   watch<Option<TreeProbe>> ──▶ ResourceSampler ──spawn_blocking──▶ Reader::tree
//!                                     │ Fold
//!                                     ▼
//!                            watch<Option<NodeResources>>
//!
//!   watch<Vec<DataDir>> ──▶ MachineSampler ──spawn_blocking──▶ MachineReader::machine
//!                                │          ──spawn_blocking──▶ MachineReader::volume
//!                                ▼          (one read per folder at a time; one may hang)
//!                       watch<Option<MachineResources>>
//! ```

use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

use launcher_types::{
    MachineResources, MemoryPressure, NodeId, NodeResources, Platform, VolumeUsage,
};
use tokio::{
    sync::watch,
    task::{self, JoinError, JoinSet},
    time::MissedTickBehavior,
};
use tracing::Instrument;

use crate::{
    firstrun::{self, Volume},
    process::TreeProbe,
};

mod fold;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod procfs;
#[cfg(windows)]
mod windows;

use fold::Fold;

/// How often both samplers read: the RPC monitor's cadence.
pub const INTERVAL: Duration = Duration::from_secs(5);

/// How often the machine sampler reads the volumes that hold the nodes' data folders.
pub const VOLUME_INTERVAL: Duration = Duration::from_secs(30);

/// A node's data folder, whose volume the machine sampler reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataDir {
    pub node: NodeId,
    pub path: PathBuf,
}

/// A process as the OS has it: its pid and its start time in the OS's unit (clock ticks since
/// boot on Linux, microseconds since 1970 on macOS, 100 ns since 1601 on Windows), which tells
/// it apart from a later process with the same pid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Identity {
    pub pid: u32,
    pub start: u64,
}

/// One member of a tree as a reader saw it. A field the OS refused is `None`.
#[derive(Clone, Debug, PartialEq)]
pub struct MemberReading {
    pub id: Identity,
    /// 0 on Windows, where parent pids are not read.
    pub ppid: u32,
    /// Cleaned to one line of plain text ([`clean_name`]).
    pub name: String,
    pub started_at: SystemTime,
    /// Its own user and system time, its threads' included; 0 when [`Denied::cpu`] is set.
    pub cpu_own: f64,
    /// The time of the children it reaped (Linux `cutime` + `cstime`, macOS `ri_child_*`); 0 on
    /// Windows.
    pub cpu_rollup: f64,
    /// Its memory as the OS's monitor shows it (resident memory, the physical footprint or the
    /// working set); 0 for a zombie.
    pub memory: Option<u64>,
    /// The highest memory it reached.
    pub peak: Option<u64>,
    /// Windows only: its commit charge (`PrivateUsage`).
    pub committed: Option<u64>,
    /// Bytes read and written: from storage on Linux, the children it reaped included; from
    /// storage on macOS; all I/O on Windows.
    pub read: Option<u64>,
    pub written: Option<u64>,
    /// `None` on Windows.
    pub threads: Option<u32>,
    /// Open files, or handles on Windows.
    pub descriptors: Option<u32>,
}

/// The fields not read for at least one member of a reading: refused by the OS, or for `cpu`,
/// not read under Rosetta.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Denied {
    /// CPU times are not read for any member: an x86-64 build under Rosetta, whose Mach
    /// timebase is unchecked (docs/RESOURCES.md). The OS refuses nothing there.
    pub cpu: bool,
    pub memory: bool,
    pub io: bool,
    pub descriptors: bool,
}

/// Windows: what the job counted for every process ever in it, the exited ones included.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct JobTotals {
    /// User plus kernel time, in seconds.
    pub cpu: f64,
    /// Bytes of all I/O: files, network and devices.
    pub read: u64,
    pub written: u64,
}

/// One reading of a tree, before the fold.
#[derive(Clone, Debug, PartialEq)]
pub struct TreeReading {
    pub at: SystemTime,
    /// When the members were read, on the monotonic clock.
    pub mono: Instant,
    /// In tree order: each member before its children. On Windows, where parents are not
    /// read, the root and then the others by start time.
    pub members: Vec<MemberReading>,
    pub denied: Denied,
    /// Windows: the job's totals, which replace the sum over the members.
    pub job: Option<JobTotals>,
}

/// Reads a node's process tree.
pub trait Reader: Send + Sync + 'static {
    /// Reads the tree of `probe`. `sticky` holds the identities seen as members before, the
    /// root's included once it was found. Malformed OS text or an unexpected answer is an
    /// error, never a panic; [`io::ErrorKind::Unsupported`] means this platform has no reader.
    fn tree(&self, probe: &TreeProbe, sticky: &[Identity]) -> io::Result<TreeReading>;
}

/// The computer's cumulative CPU time spent busy and idle, in the OS's unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MachineCpu {
    pub busy: u64,
    pub idle: u64,
}

/// One reading of the computer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MachineReading {
    pub at: SystemTime,
    pub logical_cpus: u32,
    /// `None` where the OS gives no times for all `logical_cpus`: Windows with more than one
    /// processor group.
    pub cpu: Option<MachineCpu>,
    pub memory_total: u64,
    pub memory_available: Option<u64>,
    /// Linux and macOS.
    pub swap_total: Option<u64>,
    pub swap_used: Option<u64>,
    /// Windows: the commit charge and its limit.
    pub commit: Option<u64>,
    pub commit_limit: Option<u64>,
    /// macOS: the kernel's memory pressure level.
    pub pressure: Option<MemoryPressure>,
}

/// Reads the computer's CPU and memory, and the volumes that hold folders.
pub trait MachineReader: Send + Sync + 'static {
    /// [`io::ErrorKind::Unsupported`] means this platform has no reader.
    fn machine(&self) -> io::Result<MachineReading>;

    /// The volume that holds `dir` ([`firstrun::volume`]). It can hang on a dead network
    /// mount, so the sampler reads each folder's volume on a task of its own.
    fn volume(&self, dir: &Path) -> io::Result<Volume> {
        firstrun::volume(dir)
    }
}

/// The platform the readers of this build report.
pub fn platform() -> Platform {
    if cfg!(target_os = "macos") {
        Platform::Macos
    } else if cfg!(windows) {
        Platform::Windows
    } else {
        Platform::Linux
    }
}

/// This platform's tree reader: Linux, macOS and Windows have one. On other platforms every
/// read fails with [`io::ErrorKind::Unsupported`].
pub fn platform_reader() -> Arc<dyn Reader> {
    #[cfg(target_os = "linux")]
    {
        Arc::new(linux::LinuxReader::new())
    }
    #[cfg(target_os = "macos")]
    {
        Arc::new(macos::MacReader::new())
    }
    #[cfg(windows)]
    {
        Arc::new(windows::WindowsReader)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        Arc::new(Unsupported)
    }
}

/// This platform's machine reader, like [`platform_reader`].
pub fn platform_machine_reader() -> Arc<dyn MachineReader> {
    #[cfg(target_os = "linux")]
    {
        Arc::new(linux::LinuxReader::new())
    }
    #[cfg(target_os = "macos")]
    {
        Arc::new(macos::MacMachineReader::new())
    }
    #[cfg(windows)]
    {
        Arc::new(windows::WindowsReader)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        Arc::new(Unsupported)
    }
}

/// The reader of a platform that has none.
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
struct Unsupported;

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
impl Unsupported {
    fn error() -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "resource monitoring is not supported on this platform",
        )
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
impl Reader for Unsupported {
    fn tree(&self, _probe: &TreeProbe, _sticky: &[Identity]) -> io::Result<TreeReading> {
        Err(Self::error())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
impl MachineReader for Unsupported {
    fn machine(&self) -> io::Result<MachineReading> {
        Err(Self::error())
    }
}

/// A process name as the OS reports it, as one line of plain text: invalid UTF-8 becomes
/// U+FFFD, and so does every control character.
pub fn clean_name(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw)
        .chars()
        .map(|c| if c.is_control() { '\u{FFFD}' } else { c })
        .collect()
}

/// Starts the sampling of a node's process tree.
pub struct ResourceSampler;

impl ResourceSampler {
    /// Samples the probe's tree every [`INTERVAL`] while the probe is `Some`, with this
    /// platform's reader. Must be called inside a Tokio runtime. The sampler logs inside the
    /// span that is current here, so a node's sampler started in the node's span logs to that
    /// node.
    pub fn spawn(
        probe: watch::Receiver<Option<TreeProbe>>,
    ) -> watch::Receiver<Option<NodeResources>> {
        Self::spawn_with(probe, INTERVAL, platform_reader())
    }

    /// Like [`ResourceSampler::spawn`], with another interval or reader (tests).
    pub fn spawn_with(
        probe: watch::Receiver<Option<TreeProbe>>,
        interval: Duration,
        reader: Arc<dyn Reader>,
    ) -> watch::Receiver<Option<NodeResources>> {
        let (tx, rx) = watch::channel(None);
        tokio::spawn(sample_trees(probe, interval, reader, tx).in_current_span());
        rx
    }
}

/// How sampling one run ended.
enum RunEnd {
    /// The probe changed: a new run, or none.
    ProbeChanged,
    /// The reader panicked: nothing more is read until the next run.
    Stopped,
    /// This platform has no reader: nothing more is read at all.
    Unsupported,
    /// The probe's sender or every receiver is gone.
    Closed,
}

/// The node sampler's task. It ends when the probe's sender (the supervisor) or every
/// receiver is dropped, or when the platform has no reader.
async fn sample_trees(
    mut probe: watch::Receiver<Option<TreeProbe>>,
    interval: Duration,
    reader: Arc<dyn Reader>,
    tx: watch::Sender<Option<NodeResources>>,
) {
    let mut logged = Denied::default();
    loop {
        let current = probe.borrow_and_update().clone();
        let Some(current) = current else {
            tx.send_if_modified(|sample| sample.take().is_some());
            tokio::select! {
                changed = probe.changed() => if changed.is_err() { return },
                () = tx.closed() => return,
            }
            continue;
        };
        match sample_run(&mut probe, &current, interval, &reader, &tx, &mut logged).await {
            RunEnd::ProbeChanged => {}
            RunEnd::Closed => return,
            RunEnd::Unsupported => {
                tx.send_if_modified(|sample| sample.take().is_some());
                return;
            }
            RunEnd::Stopped => {
                tx.send_if_modified(|sample| sample.take().is_some());
                // Wait for the next run.
                loop {
                    tokio::select! {
                        changed = probe.changed() => if changed.is_err() { return },
                        () = tx.closed() => return,
                    }
                    let run = probe.borrow().as_ref().map(|probe| probe.run);
                    if run != Some(current.run) {
                        break;
                    }
                }
            }
        }
    }
}

/// Samples one run until its probe changes: a reading at once, then every `interval`.
async fn sample_run(
    probe_rx: &mut watch::Receiver<Option<TreeProbe>>,
    probe: &TreeProbe,
    interval: Duration,
    reader: &Arc<dyn Reader>,
    tx: &watch::Sender<Option<NodeResources>>,
    logged: &mut Denied,
) -> RunEnd {
    let mut fold = Fold::new(probe, platform());
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut warned = false;
    loop {
        tokio::select! {
            _ = ticker.tick() => {}
            changed = probe_rx.changed() => {
                return if changed.is_err() { RunEnd::Closed } else { RunEnd::ProbeChanged };
            }
            () = tx.closed() => return RunEnd::Closed,
        }
        let job = {
            let probe = probe.clone();
            let sticky = fold.sticky();
            let reader = Arc::clone(reader);
            tokio::task::spawn_blocking(move || {
                let started = Instant::now();
                let reading = reader.tree(&probe, &sticky);
                (reading, started.elapsed())
            })
        };
        let joined = job.await;
        // A read that spans a probe change belongs to a run that is over.
        match probe_rx.has_changed() {
            Ok(false) => {}
            Ok(true) => return RunEnd::ProbeChanged,
            Err(_) => return RunEnd::Closed,
        }
        match joined {
            Ok((Ok(reading), took)) => {
                tracing::debug!(
                    run = probe.run,
                    members = reading.members.len(),
                    ms = took.as_secs_f64() * 1000.0,
                    "read the node's process tree"
                );
                log_denied(logged, reading.denied);
                tx.send_replace(Some(fold.fold(reading)));
            }
            Ok((Err(err), _)) if err.kind() == io::ErrorKind::Unsupported => {
                tracing::debug!(%err, "the node's resources are not read on this platform");
                return RunEnd::Unsupported;
            }
            Ok((Err(err), _)) => {
                if warned {
                    tracing::debug!(
                        %err,
                        run = probe.run,
                        "reading the node's process tree failed"
                    );
                } else {
                    warned = true;
                    tracing::warn!(
                        %err,
                        run = probe.run,
                        "reading the node's process tree failed; trying again every interval"
                    );
                }
            }
            Err(err) => {
                // A panic in the reader is a bug; the panic hook has logged it already.
                tracing::warn!(
                    %err,
                    run = probe.run,
                    "the resource reader failed; the node's resources are not read until its next run"
                );
                return RunEnd::Stopped;
            }
        }
    }
}

/// Logs each field the OS refused, and CPU not read under Rosetta, once per sampler.
fn log_denied(logged: &mut Denied, denied: Denied) {
    if denied.cpu && !logged.cpu {
        logged.cpu = true;
        tracing::debug!("CPU is not read under Rosetta until its timebase is checked");
    }
    let platform = platform();
    for (now, before, field) in [
        (denied.memory, &mut logged.memory, "memory"),
        (denied.io, &mut logged.io, platform.io_label()),
        (
            denied.descriptors,
            &mut logged.descriptors,
            platform.descriptors_label(),
        ),
    ] {
        if now && !*before {
            *before = true;
            tracing::debug!(field, "the OS refused a field of the node's processes");
        }
    }
}

/// Starts the sampling of the computer.
pub struct MachineSampler;

impl MachineSampler {
    /// The computer's CPU and memory every [`INTERVAL`], and the volumes that hold `data_dirs`
    /// every [`VOLUME_INTERVAL`] and whenever the folders change, with this platform's reader.
    /// Must be called inside a Tokio runtime.
    pub fn spawn(
        data_dirs: watch::Receiver<Vec<DataDir>>,
    ) -> watch::Receiver<Option<MachineResources>> {
        Self::spawn_with(
            data_dirs,
            INTERVAL,
            VOLUME_INTERVAL,
            platform_machine_reader(),
        )
    }

    /// Like [`MachineSampler::spawn`], with other intervals or another reader (tests).
    pub fn spawn_with(
        data_dirs: watch::Receiver<Vec<DataDir>>,
        interval: Duration,
        volume_interval: Duration,
        reader: Arc<dyn MachineReader>,
    ) -> watch::Receiver<Option<MachineResources>> {
        let (tx, rx) = watch::channel(None);
        tokio::spawn(sample_machine(
            data_dirs,
            interval,
            volume_interval,
            reader,
            tx,
        ));
        rx
    }
}

/// One folder's volume read as it ended: the folder, what the read gave, and when.
type FolderRead = (PathBuf, io::Result<Volume>, SystemTime);

/// The machine sampler's volume reads: at most one at a time for each folder, each on its own
/// blocking task, so a volume that hangs delays only the figures of the folders on it. The
/// actor owns it.
struct VolumeReads {
    reader: Arc<dyn MachineReader>,
    /// The folders, in the list's order.
    dirs: Vec<DataDir>,
    /// The reads in flight.
    jobs: JoinSet<FolderRead>,
    /// The folder of each read in flight. A folder that left the list keeps its read here
    /// until the read ends, so a folder never has two.
    reading: HashMap<task::Id, PathBuf>,
    /// Each listed folder's newest volume, with when it was read. A failed read removes the
    /// folder's; a read that hangs leaves the one before.
    read: HashMap<PathBuf, (Volume, SystemTime)>,
    /// The listed folders' volumes, as the samples carry them ([`VolumeReads::group`]).
    volumes: Vec<VolumeUsage>,
    /// A failure was logged as a warning; later ones log at debug.
    warned: bool,
}

impl VolumeReads {
    fn new(reader: Arc<dyn MachineReader>) -> Self {
        VolumeReads {
            reader,
            dirs: Vec::new(),
            jobs: JoinSet::new(),
            reading: HashMap::new(),
            read: HashMap::new(),
            volumes: Vec::new(),
            warned: false,
        }
    }

    /// Follows `dirs`: forgets the folders that left it, and reads each listed folder that has
    /// no read in flight.
    fn start(&mut self, dirs: Vec<DataDir>) {
        self.read
            .retain(|path, _| dirs.iter().any(|dir| dir.path == *path));
        self.dirs = dirs;
        for dir in &self.dirs {
            if self.reading.values().any(|path| *path == dir.path) {
                continue;
            }
            let reader = Arc::clone(&self.reader);
            let path = dir.path.clone();
            let job = self.jobs.spawn_blocking(move || {
                let volume = reader.volume(&path);
                (path, volume, SystemTime::now())
            });
            self.reading.insert(job.id(), dir.path.clone());
        }
        self.group();
    }

    /// The next read to end; pending while none is in flight.
    async fn finished(&mut self) -> Result<(task::Id, FolderRead), JoinError> {
        match self.jobs.join_next_with_id().await {
            Some(joined) => joined,
            None => std::future::pending().await,
        }
    }

    /// Takes a read that ended: the folder's volume while it is listed, or none after a
    /// failure.
    fn take(&mut self, joined: Result<(task::Id, FolderRead), JoinError>) {
        let (id, failure) = match joined {
            Ok((id, (path, Ok(volume), at))) => {
                if self.dirs.iter().any(|dir| dir.path == path) {
                    self.read.insert(path, (volume, at));
                }
                (id, None)
            }
            Ok((id, (path, Err(err), _))) => {
                self.read.remove(&path);
                (id, Some(err.to_string()))
            }
            // A panic in the reader is a bug; the panic hook has logged it already.
            Err(err) => {
                if let Some(path) = self.reading.get(&err.id()) {
                    self.read.remove(path);
                }
                (err.id(), Some(err.to_string()))
            }
        };
        self.reading.remove(&id);
        self.group();
        if let Some(err) = failure {
            if self.warned {
                tracing::debug!(%err, "reading a data folder's volume failed");
            } else {
                self.warned = true;
                tracing::warn!(%err, "reading a data folder's volume failed; trying again every 30 s");
            }
        }
    }

    /// Groups the listed folders' newest volumes by device into [`VolumeReads::volumes`]: one
    /// entry per volume, in the order of its first folder, listing its nodes in the folders'
    /// order, with the newest reading among its folders.
    fn group(&mut self) {
        let mut volumes: Vec<(&str, VolumeUsage)> = Vec::new();
        for dir in &self.dirs {
            let Some((volume, at)) = self.read.get(&dir.path) else {
                continue;
            };
            match volumes.iter_mut().find(|(key, _)| *key == volume.key) {
                Some((_, usage)) => {
                    if !usage.nodes.contains(&dir.node) {
                        usage.nodes.push(dir.node.clone());
                    }
                    if *at > usage.measured_at {
                        usage.total_bytes = volume.total;
                        usage.free_bytes = volume.available;
                        usage.measured_at = *at;
                    }
                }
                None => volumes.push((
                    &volume.key,
                    VolumeUsage {
                        label: volume.label.clone(),
                        total_bytes: volume.total,
                        free_bytes: volume.available,
                        nodes: vec![dir.node.clone()],
                        measured_at: *at,
                    },
                )),
            }
        }
        self.volumes = volumes.into_iter().map(|(_, usage)| usage).collect();
    }
}

/// Puts the newest volumes into the published sample, if they changed.
fn publish_volumes(tx: &watch::Sender<Option<MachineResources>>, volumes: &[VolumeUsage]) {
    tx.send_if_modified(|sample| match sample {
        Some(sample) if sample.volumes != volumes => {
            sample.volumes = volumes.to_vec();
            true
        }
        _ => false,
    });
}

/// The busy logical CPUs between two readings: the busy share of the CPU time that passed,
/// times the CPUs. `None` when no time passed or a reading has no CPU times.
fn busy_cores(before: &MachineReading, now: &MachineReading) -> Option<f64> {
    let (before_cpu, now_cpu) = (before.cpu?, now.cpu?);
    let busy = now_cpu.busy.checked_sub(before_cpu.busy)?;
    let idle = now_cpu.idle.checked_sub(before_cpu.idle)?;
    let total = busy + idle;
    (total > 0).then(|| busy as f64 / total as f64 * f64::from(now.logical_cpus))
}

/// The machine sampler's task. It ends when every receiver is dropped, when the platform has
/// no reader, or when the reader panics while reading the CPU and memory.
async fn sample_machine(
    mut data_dirs: watch::Receiver<Vec<DataDir>>,
    interval: Duration,
    volume_interval: Duration,
    reader: Arc<dyn MachineReader>,
    tx: watch::Sender<Option<MachineResources>>,
) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut volume_ticker = tokio::time::interval(volume_interval);
    volume_ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut volumes = VolumeReads::new(Arc::clone(&reader));
    // The folders' sender is gone: the folders stay as they are.
    let mut dirs_open = true;
    let mut previous: Option<MachineReading> = None;
    let mut warned = false;
    loop {
        tokio::select! {
            _ = ticker.tick() => {}
            _ = volume_ticker.tick() => {
                volumes.start(data_dirs.borrow_and_update().clone());
                publish_volumes(&tx, &volumes.volumes);
                continue;
            }
            changed = data_dirs.changed(), if dirs_open => {
                match changed {
                    Ok(()) => {
                        volumes.start(data_dirs.borrow_and_update().clone());
                        publish_volumes(&tx, &volumes.volumes);
                    }
                    Err(_) => dirs_open = false,
                }
                continue;
            }
            joined = volumes.finished() => {
                volumes.take(joined);
                publish_volumes(&tx, &volumes.volumes);
                continue;
            }
            () = tx.closed() => return,
        }
        let reader = Arc::clone(&reader);
        match tokio::task::spawn_blocking(move || reader.machine()).await {
            Ok(Ok(reading)) => {
                let busy = previous
                    .as_ref()
                    .and_then(|before| busy_cores(before, &reading));
                previous = Some(reading);
                tx.send_replace(Some(MachineResources {
                    at: reading.at,
                    logical_cpus: reading.logical_cpus,
                    cpu_busy_cores: busy,
                    memory_total_bytes: reading.memory_total,
                    memory_available_bytes: reading.memory_available,
                    swap_total_bytes: reading.swap_total,
                    swap_used_bytes: reading.swap_used,
                    commit_bytes: reading.commit,
                    commit_limit_bytes: reading.commit_limit,
                    pressure: reading.pressure,
                    volumes: volumes.volumes.clone(),
                }));
            }
            Ok(Err(err)) if err.kind() == io::ErrorKind::Unsupported => {
                tracing::debug!(%err, "the computer's resources are not read on this platform");
                return;
            }
            Ok(Err(err)) => {
                if warned {
                    tracing::debug!(%err, "reading the computer's resources failed");
                } else {
                    warned = true;
                    tracing::warn!(
                        %err,
                        "reading the computer's resources failed; trying again every interval"
                    );
                }
            }
            Err(err) => {
                tracing::warn!(%err, "the computer's resource reader failed; it is not read again");
                tx.send_replace(None);
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

    use super::*;

    const LIMIT: Duration = Duration::from_secs(10);

    type Script = dyn Fn(u32, &TreeProbe) -> io::Result<TreeReading> + Send + Sync;

    /// A reader that answers from a script, counting its calls.
    struct Scripted {
        calls: AtomicU32,
        script: Box<Script>,
    }

    impl Scripted {
        fn new(
            script: impl Fn(u32, &TreeProbe) -> io::Result<TreeReading> + Send + Sync + 'static,
        ) -> Arc<Self> {
            Arc::new(Scripted {
                calls: AtomicU32::new(0),
                script: Box::new(script),
            })
        }

        fn calls(&self) -> u32 {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl Reader for Scripted {
        fn tree(&self, probe: &TreeProbe, _sticky: &[Identity]) -> io::Result<TreeReading> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            (self.script)(call, probe)
        }
    }

    fn probe(run: u64, spawned: Instant) -> TreeProbe {
        TreeProbe {
            run,
            pid: 100,
            spawned_at: SystemTime::now(),
            spawned,
            #[cfg(windows)]
            job: std::sync::Weak::new(),
        }
    }

    /// The root alone, `5 × (call + 1)` seconds into the run with 2 cores' worth of CPU. The
    /// job's totals match it, since the fold takes those on Windows and ignores them elsewhere.
    fn steady(call: u32, probe: &TreeProbe) -> io::Result<TreeReading> {
        let secs = 5 * (u64::from(call) + 1);
        let cpu = 2.0 * secs as f64;
        Ok(TreeReading {
            at: SystemTime::now(),
            mono: probe.spawned + Duration::from_secs(secs),
            members: vec![MemberReading {
                id: Identity {
                    pid: probe.pid,
                    start: 7,
                },
                ppid: 1,
                name: "zebrad".into(),
                started_at: probe.spawned_at,
                cpu_own: cpu,
                cpu_rollup: 0.0,
                memory: Some(1 << 30),
                peak: Some(1 << 30),
                committed: None,
                read: Some(0),
                written: Some(0),
                threads: Some(68),
                descriptors: Some(412),
            }],
            denied: Denied::default(),
            job: Some(JobTotals {
                cpu,
                read: 0,
                written: 0,
            }),
        })
    }

    async fn next(rx: &mut watch::Receiver<Option<NodeResources>>) -> Option<NodeResources> {
        tokio::time::timeout(LIMIT, rx.changed())
            .await
            .expect("a sample in time")
            .expect("the sampler is alive");
        rx.borrow_and_update().clone()
    }

    #[test]
    fn names_are_one_line_of_plain_text() {
        assert_eq!(clean_name(b"zebrad"), "zebrad");
        assert_eq!(clean_name(b"a\nb\tc"), "a\u{FFFD}b\u{FFFD}c");
        assert_eq!(clean_name(b"cut\xe2\x82"), "cut\u{FFFD}");
        assert_eq!(clean_name("zéro".as_bytes()), "zéro");
    }

    #[tokio::test]
    async fn no_probe_means_no_sample_and_no_read() {
        let (_probe_tx, probe_rx) = watch::channel(None);
        let reader = Scripted::new(steady);
        let rx = ResourceSampler::spawn_with(probe_rx, Duration::from_millis(10), reader.clone());
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert_eq!(*rx.borrow(), None);
        assert_eq!(reader.calls(), 0);
    }

    #[tokio::test]
    async fn samples_come_at_once_then_every_interval() {
        let spawned = Instant::now();
        let (probe_tx, probe_rx) = watch::channel(Some(probe(1, spawned)));
        let reader = Scripted::new(steady);
        let mut rx =
            ResourceSampler::spawn_with(probe_rx, Duration::from_millis(40), reader.clone());
        let first = next(&mut rx).await.expect("a sample");
        assert_eq!(first.run, 1);
        assert_eq!(first.cpu_cores, Some(2.0), "since the spawn");
        assert_eq!(first.members[0].name, "zebrad");
        let second = next(&mut rx).await.expect("a sample");
        assert_eq!(second.cpu_cores, Some(2.0), "over the interval");
        assert!(second.cpu_secs > first.cpu_secs);
        drop(probe_tx);
        // Dropping the probe's sender ends the task.
        tokio::time::timeout(LIMIT, async { while rx.changed().await.is_ok() {} })
            .await
            .expect("the sampler ended");
    }

    #[tokio::test]
    async fn a_new_run_starts_a_new_fold() {
        let spawned = Instant::now();
        let (probe_tx, probe_rx) = watch::channel(Some(probe(1, spawned)));
        let reader = Scripted::new(steady);
        let mut rx =
            ResourceSampler::spawn_with(probe_rx, Duration::from_millis(20), reader.clone());
        let first = next(&mut rx).await.expect("run 1");
        next(&mut rx).await.expect("run 1 again");
        probe_tx.send_replace(None);
        // The run is over: `None`, then the next run from zero.
        loop {
            if next(&mut rx).await.is_none() {
                break;
            }
        }
        probe_tx.send_replace(Some(probe(2, Instant::now())));
        let second = loop {
            if let Some(sample) = next(&mut rx).await {
                break sample;
            }
        };
        assert_eq!(second.run, 2);
        assert!(second.cpu_secs <= first.cpu_secs * 20.0);
        assert_eq!(
            second.cpu_cores,
            Some(2.0),
            "its first rate is the average since the spawn"
        );
        assert_eq!(second.peak_memory_bytes, 1 << 30);
    }

    #[tokio::test]
    async fn a_slow_reader_does_not_pile_up_reads() {
        let (_probe_tx, probe_rx) = watch::channel(Some(probe(1, Instant::now())));
        let reader = Scripted::new(|call, probe| {
            std::thread::sleep(Duration::from_millis(100));
            steady(call, probe)
        });
        let _rx = ResourceSampler::spawn_with(probe_rx, Duration::from_millis(10), reader.clone());
        tokio::time::sleep(Duration::from_millis(450)).await;
        let calls = reader.calls();
        assert!((2..=6).contains(&calls), "one read at a time: {calls}");
    }

    #[tokio::test]
    async fn a_read_that_spans_a_probe_change_is_dropped() {
        let (probe_tx, probe_rx) = watch::channel(Some(probe(1, Instant::now())));
        let reader = Scripted::new(|call, probe| {
            if probe.run == 1 {
                std::thread::sleep(Duration::from_millis(200));
            }
            steady(call, probe)
        });
        let mut rx = ResourceSampler::spawn_with(probe_rx, Duration::from_millis(20), reader);
        tokio::time::sleep(Duration::from_millis(50)).await;
        probe_tx.send_replace(Some(probe(2, Instant::now())));
        let sample = loop {
            if let Some(sample) = next(&mut rx).await {
                break sample;
            }
        };
        assert_eq!(
            sample.run, 2,
            "run 1's reading finished after its run ended"
        );
    }

    #[tokio::test]
    async fn errors_do_not_end_sampling_and_a_panic_waits_for_the_next_run() {
        let (probe_tx, probe_rx) = watch::channel(Some(probe(1, Instant::now())));
        let reader = Scripted::new(|call, probe| match call {
            0..3 => Err(io::Error::other("malformed /proc text")),
            3 => steady(call, probe),
            4 => panic!("a reader bug"),
            _ => steady(call, probe),
        });
        let mut rx =
            ResourceSampler::spawn_with(probe_rx, Duration::from_millis(10), reader.clone());
        let sample = next(&mut rx).await.expect("a sample after the errors");
        assert_eq!(sample.run, 1);
        // The panic withdraws the sample, and nothing more is read in this run.
        assert_eq!(next(&mut rx).await, None);
        let calls = reader.calls();
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(reader.calls(), calls, "no reads until the next run");
        probe_tx.send_replace(Some(probe(2, Instant::now())));
        let sample = next(&mut rx).await.expect("the next run is read");
        assert_eq!(sample.run, 2);
    }

    #[tokio::test]
    async fn an_unsupported_platform_stops_sampling() {
        let (_probe_tx, probe_rx) = watch::channel(Some(probe(1, Instant::now())));
        let reader = Scripted::new(|_, _| Err(io::Error::from(io::ErrorKind::Unsupported)));
        let mut rx =
            ResourceSampler::spawn_with(probe_rx, Duration::from_millis(10), reader.clone());
        tokio::time::timeout(LIMIT, async { while rx.changed().await.is_ok() {} })
            .await
            .expect("the sampler ended");
        assert_eq!(*rx.borrow(), None);
        assert_eq!(reader.calls(), 1);
    }

    struct Machine {
        calls: AtomicU32,
    }

    impl MachineReader for Machine {
        fn machine(&self) -> io::Result<MachineReading> {
            let call = u64::from(self.calls.fetch_add(1, Ordering::SeqCst));
            // 8 CPUs, a quarter busy; Windows' commit and macOS's pressure, as one reader
            // gives none of both.
            Ok(MachineReading {
                at: SystemTime::now(),
                logical_cpus: 8,
                cpu: Some(MachineCpu {
                    busy: 100 * call,
                    idle: 300 * call,
                }),
                memory_total: 16 << 30,
                memory_available: Some(4 << 30),
                swap_total: Some(2 << 30),
                swap_used: Some(0),
                commit: Some(9 << 30),
                commit_limit: Some(24 << 30),
                pressure: Some(MemoryPressure::Warning),
            })
        }
    }

    #[tokio::test]
    async fn the_computers_busy_cores_come_from_two_readings() {
        let reader = Arc::new(Machine {
            calls: AtomicU32::new(0),
        });
        let (_dirs_tx, dirs) = watch::channel(Vec::new());
        let mut rx = MachineSampler::spawn_with(
            dirs,
            Duration::from_millis(20),
            Duration::from_secs(3600),
            reader,
        );
        let first = loop {
            tokio::time::timeout(LIMIT, rx.changed())
                .await
                .unwrap()
                .unwrap();
            if let Some(first) = rx.borrow_and_update().clone() {
                break first;
            }
        };
        assert_eq!(first.cpu_busy_cores, None, "one reading has no rate");
        assert_eq!(first.logical_cpus, 8);
        assert_eq!(first.memory_available_bytes, Some(4 << 30));
        tokio::time::timeout(LIMIT, rx.changed())
            .await
            .unwrap()
            .unwrap();
        let second = rx.borrow_and_update().clone().unwrap();
        assert_eq!(second.cpu_busy_cores, Some(2.0));
        assert_eq!(second.commit_bytes, Some(9 << 30));
        assert_eq!(second.commit_limit_bytes, Some(24 << 30));
        assert_eq!(second.pressure, Some(MemoryPressure::Warning));
    }

    /// A computer whose folders' volumes are their paths' first parts (`/a/x` is on `a`, with
    /// `free` of 1,000 bytes free, 100 at first), whose reads of `/missing` fail, whose volume
    /// reads wait while `hold` is set, and whose reads of `/hung` wait while `hung` is set, as
    /// on a dead network mount.
    struct Volumes {
        machine: Machine,
        free: AtomicU64,
        hold: AtomicBool,
        hung: AtomicBool,
        reads: AtomicU32,
        hung_reads: AtomicU32,
    }

    impl Volumes {
        fn new(hold: bool) -> Arc<Self> {
            Arc::new(Volumes {
                machine: Machine {
                    calls: AtomicU32::new(0),
                },
                free: AtomicU64::new(100),
                hold: AtomicBool::new(hold),
                hung: AtomicBool::new(true),
                reads: AtomicU32::new(0),
                hung_reads: AtomicU32::new(0),
            })
        }
    }

    impl MachineReader for Volumes {
        fn machine(&self) -> io::Result<MachineReading> {
            self.machine.machine()
        }

        fn volume(&self, dir: &Path) -> io::Result<Volume> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            while self.hold.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(10));
            }
            let key = dir
                .components()
                .nth(1)
                .map(|part| part.as_os_str().to_string_lossy().into_owned())
                .unwrap_or_default();
            if key == "hung" {
                self.hung_reads.fetch_add(1, Ordering::SeqCst);
                while self.hung.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            if key == "missing" {
                return Err(io::Error::from(io::ErrorKind::NotFound));
            }
            Ok(Volume {
                label: format!("/{key}"),
                key,
                available: self.free.load(Ordering::SeqCst),
                total: 1000,
            })
        }
    }

    fn dir(node: &str, path: &str) -> DataDir {
        DataDir {
            node: NodeId::new(node).expect("an id"),
            path: PathBuf::from(path),
        }
    }

    /// The volumes of the next sample that `pick` accepts, as labels and their nodes.
    async fn volumes_until(
        rx: &mut watch::Receiver<Option<MachineResources>>,
        pick: impl Fn(&[VolumeUsage]) -> bool,
    ) -> Vec<(String, Vec<String>)> {
        let sample = tokio::time::timeout(
            LIMIT,
            rx.wait_for(|sample| sample.as_ref().is_some_and(|s| pick(&s.volumes))),
        )
        .await
        .expect("volumes in time")
        .expect("the sampler is alive")
        .clone()
        .expect("a sample");
        sample
            .volumes
            .iter()
            .map(|volume| {
                (
                    volume.label.clone(),
                    volume.nodes.iter().map(ToString::to_string).collect(),
                )
            })
            .collect()
    }

    #[tokio::test]
    async fn volumes_are_grouped_by_device_and_follow_the_folders() {
        let reader = Volumes::new(false);
        let (dirs_tx, dirs) = watch::channel(vec![
            dir("zebrad-mainnet", "/a/x"),
            dir("zebrad-testnet", "/b/z"),
            dir("zakura-mainnet", "/a/y"),
        ]);
        let mut rx = MachineSampler::spawn_with(
            dirs,
            Duration::from_millis(20),
            Duration::from_secs(3600),
            reader,
        );
        let owned = |rows: &[(&str, &[&str])]| -> Vec<(String, Vec<String>)> {
            rows.iter()
                .map(|(label, nodes)| {
                    (
                        (*label).to_owned(),
                        nodes.iter().map(|node| (*node).to_owned()).collect(),
                    )
                })
                .collect()
        };
        // Each folder's read is published as it ends, so wait for all three.
        let read = |volumes: &[VolumeUsage]| {
            volumes
                .iter()
                .map(|volume| volume.nodes.len())
                .sum::<usize>()
        };
        assert_eq!(
            volumes_until(&mut rx, |volumes| read(volumes) == 3).await,
            owned(&[
                ("/a", &["zebrad-mainnet", "zakura-mainnet"]),
                ("/b", &["zebrad-testnet"])
            ])
        );
        let sample = rx.borrow().clone().expect("a sample");
        assert_eq!(
            (sample.volumes[0].free_bytes, sample.volumes[0].total_bytes),
            (100, 1000)
        );
        // A change of the folders is read at once; a folder whose volume cannot be read is
        // left out.
        dirs_tx.send_replace(vec![
            dir("zebrad-testnet", "/b/z"),
            dir("zebrad-mainnet", "/missing/q"),
        ]);
        assert_eq!(
            volumes_until(&mut rx, |volumes| volumes.len() == 1).await,
            owned(&[("/b", &["zebrad-testnet"])])
        );
    }

    #[tokio::test]
    async fn a_hung_volume_read_does_not_hold_up_the_computers_readings() {
        let reader = Volumes::new(true);
        let (dirs_tx, dirs) = watch::channel(vec![dir("zebrad-mainnet", "/a/x")]);
        let mut rx = MachineSampler::spawn_with(
            dirs,
            Duration::from_millis(20),
            Duration::from_millis(20),
            reader.clone(),
        );
        for _ in 0..5 {
            tokio::time::timeout(LIMIT, rx.changed())
                .await
                .expect("a sample in time")
                .expect("the sampler is alive");
            assert!(
                rx.borrow_and_update()
                    .as_ref()
                    .is_some_and(|s| s.volumes.is_empty())
            );
        }
        dirs_tx.send_replace(vec![dir("zebrad-mainnet", "/c/x")]);
        tokio::time::sleep(Duration::from_millis(60)).await;
        // The new folder is read beside the hung one, whose read is not started again though
        // it left the list, and neither is read twice at a time.
        assert_eq!(
            reader.reads.load(Ordering::SeqCst),
            2,
            "one read per folder at a time"
        );
        reader.hold.store(false, Ordering::SeqCst);
        // The reads end; the folder that left the list is not published.
        let volumes = volumes_until(&mut rx, |volumes| {
            volumes.first().is_some_and(|volume| volume.label == "/c")
        })
        .await;
        assert_eq!(
            volumes,
            vec![("/c".to_owned(), vec!["zebrad-mainnet".to_owned()])]
        );
    }

    /// The volume of `node`'s folder in the next sample that `pick` accepts.
    async fn volume_until(
        rx: &mut watch::Receiver<Option<MachineResources>>,
        node: &str,
        pick: impl Fn(&VolumeUsage) -> bool,
    ) -> VolumeUsage {
        let node = NodeId::new(node).expect("an id");
        let sample = tokio::time::timeout(
            LIMIT,
            rx.wait_for(|sample| {
                sample
                    .as_ref()
                    .and_then(|sample| sample.volume_of(&node))
                    .is_some_and(&pick)
            }),
        )
        .await
        .expect("the volume in time")
        .expect("the sampler is alive")
        .clone()
        .expect("a sample");
        sample.volume_of(&node).expect("its volume").clone()
    }

    #[tokio::test]
    async fn a_hung_folder_holds_up_only_its_own_volume() {
        let reader = Volumes::new(false);
        let (dirs_tx, dirs) = watch::channel(vec![
            dir("zebrad-mainnet", "/a/x"),
            dir("zebrad-testnet", "/hung/z"),
        ]);
        let mut rx = MachineSampler::spawn_with(
            dirs,
            Duration::from_millis(20),
            Duration::from_millis(20),
            reader.clone(),
        );
        // While the read of `/hung` waits, the other folder's volume fills up, and the samples
        // say so.
        let first = volume_until(&mut rx, "zebrad-mainnet", |_| true).await;
        assert_eq!(first.label, "/a");
        reader.free.store(40, Ordering::SeqCst);
        let fuller =
            volume_until(&mut rx, "zebrad-mainnet", |volume| volume.free_bytes == 40).await;
        assert!(fuller.measured_at > first.measured_at);
        let sample = rx.borrow().clone().expect("a sample");
        let labels: Vec<_> = sample.volumes.iter().map(|v| v.label.as_str()).collect();
        assert_eq!(labels, ["/a"], "nothing for the hung folder yet");
        assert_eq!(
            reader.hung_reads.load(Ordering::SeqCst),
            1,
            "one read at a time"
        );
        // Taking the hung folder off the list leaves the other one's reads going.
        dirs_tx.send_replace(vec![dir("zebrad-mainnet", "/a/x")]);
        reader.free.store(30, Ordering::SeqCst);
        volume_until(&mut rx, "zebrad-mainnet", |volume| volume.free_bytes == 30).await;
        assert_eq!(reader.hung_reads.load(Ordering::SeqCst), 1);
        // The mount comes back after its folder left the list: its reading is not published.
        reader.hung.store(false, Ordering::SeqCst);
        reader.free.store(20, Ordering::SeqCst);
        volume_until(&mut rx, "zebrad-mainnet", |volume| volume.free_bytes == 20).await;
        let now = SystemTime::now();
        volume_until(&mut rx, "zebrad-mainnet", |volume| volume.measured_at > now).await;
        let sample = rx.borrow().clone().expect("a sample");
        let labels: Vec<_> = sample.volumes.iter().map(|v| v.label.as_str()).collect();
        assert_eq!(labels, ["/a"]);
    }

    #[test]
    fn busy_cores_need_time_to_pass() {
        let reading = |busy, idle| MachineReading {
            at: SystemTime::UNIX_EPOCH,
            logical_cpus: 4,
            cpu: Some(MachineCpu { busy, idle }),
            memory_total: 0,
            memory_available: None,
            swap_total: None,
            swap_used: None,
            commit: None,
            commit_limit: None,
            pressure: None,
        };
        assert_eq!(busy_cores(&reading(10, 10), &reading(10, 10)), None);
        assert_eq!(busy_cores(&reading(10, 10), &reading(30, 10)), Some(4.0));
        // Counters that went backwards (a CPU taken offline) give no figure.
        assert_eq!(busy_cores(&reading(10, 10), &reading(5, 20)), None);
        // Nor does a reading without times for every CPU (Windows above one processor group).
        let blind = MachineReading {
            cpu: None,
            ..reading(30, 10)
        };
        assert_eq!(busy_cores(&reading(10, 10), &blind), None);
        assert_eq!(busy_cores(&blind, &reading(50, 10)), None);
    }
}
