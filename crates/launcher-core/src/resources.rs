//! Resource monitoring (docs/RESOURCES.md): what each node's process tree uses, and what the
//! computer has.
//!
//! - [`ResourceSampler`] follows the supervisor's [`TreeProbe`] watch: from the spawn until
//!   the run is finalized it reads the tree every [`INTERVAL`] and publishes
//!   `watch<Option<NodeResources>>`; between runs it publishes `None`.
//! - [`MachineSampler`] reads the computer's CPU and memory every [`INTERVAL`] and publishes
//!   `watch<Option<MachineResources>>`.
//!
//! The reads are blocking system calls, so each one runs in `spawn_blocking`, taking its inputs
//! by value and returning an owned reading; each actor keeps its own state, and nothing is
//! shared. The per-OS readers implement [`Reader`] and [`MachineReader`]: the Linux one reads
//! `/proc`; on other platforms reading is not supported yet, the samplers publish `None` and
//! stop.
//!
//! ```text
//!   watch<Option<TreeProbe>> ──▶ ResourceSampler ──spawn_blocking──▶ Reader::tree
//!                                     │ Fold
//!                                     ▼
//!                            watch<Option<NodeResources>>
//! ```

use std::{
    io,
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

use launcher_types::{MachineResources, NodeResources, Platform};
use tokio::{sync::watch, time::MissedTickBehavior};

use crate::process::TreeProbe;

mod fold;
#[cfg(target_os = "linux")]
mod linux;
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod procfs;

use fold::Fold;

/// How often both samplers read: the RPC monitor's cadence.
pub const INTERVAL: Duration = Duration::from_secs(5);

/// A process as the OS has it: its pid and its start time in the OS's unit (clock ticks since
/// boot on Linux), which tells it apart from a later process with the same pid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Identity {
    pub pid: u32,
    pub start: u64,
}

/// One member of a tree as a reader saw it. A field the OS refused is `None`.
#[derive(Clone, Debug, PartialEq)]
pub struct MemberReading {
    pub id: Identity,
    pub ppid: u32,
    /// Cleaned to one line of plain text ([`clean_name`]).
    pub name: String,
    pub started_at: SystemTime,
    /// Its own user and system time, its threads' included.
    pub cpu_own: f64,
    /// The time of the children it reaped (Linux `cutime` + `cstime`).
    pub cpu_rollup: f64,
    /// Resident memory; 0 for a zombie.
    pub memory: Option<u64>,
    /// The highest resident memory it reached.
    pub peak: Option<u64>,
    /// Bytes read from and written to storage, the children it reaped included.
    pub read: Option<u64>,
    pub written: Option<u64>,
    pub threads: Option<u32>,
    /// Open files.
    pub descriptors: Option<u32>,
}

/// The fields the OS refused for at least one member of a reading.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Denied {
    pub memory: bool,
    pub io: bool,
    pub descriptors: bool,
}

/// One reading of a tree, before the fold.
#[derive(Clone, Debug, PartialEq)]
pub struct TreeReading {
    pub at: SystemTime,
    /// When the members were read, on the monotonic clock.
    pub mono: Instant,
    /// In tree order: each member before its children.
    pub members: Vec<MemberReading>,
    pub denied: Denied,
}

/// Reads a node's process tree.
pub trait Reader: Send + Sync + 'static {
    /// Reads the tree of `probe`. `sticky` holds the identities seen as members before, the
    /// root's included once it was found. Malformed OS text or an unexpected answer is an
    /// error, never a panic; [`io::ErrorKind::Unsupported`] means this platform has no reader.
    fn tree(&self, probe: &TreeProbe, sticky: &[Identity]) -> io::Result<TreeReading>;
}

/// One reading of the computer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MachineReading {
    pub at: SystemTime,
    pub logical_cpus: u32,
    /// Cumulative CPU time spent busy and idle across the computer, in the OS's unit.
    pub cpu_busy: u64,
    pub cpu_idle: u64,
    pub memory_total: u64,
    pub memory_available: Option<u64>,
    pub swap_total: Option<u64>,
    pub swap_used: Option<u64>,
}

/// Reads the computer's CPU and memory.
pub trait MachineReader: Send + Sync + 'static {
    /// [`io::ErrorKind::Unsupported`] means this platform has no reader.
    fn machine(&self) -> io::Result<MachineReading>;
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

/// This platform's tree reader. On platforms without one yet, every read fails with
/// [`io::ErrorKind::Unsupported`].
pub fn platform_reader() -> Arc<dyn Reader> {
    #[cfg(target_os = "linux")]
    {
        Arc::new(linux::LinuxReader::new())
    }
    #[cfg(not(target_os = "linux"))]
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
    #[cfg(not(target_os = "linux"))]
    {
        Arc::new(Unsupported)
    }
}

/// The reader of a platform that has none yet (macOS and Windows come with phase 3).
#[cfg(not(target_os = "linux"))]
struct Unsupported;

#[cfg(not(target_os = "linux"))]
impl Unsupported {
    fn error() -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "resource monitoring is not supported on this platform yet",
        )
    }
}

#[cfg(not(target_os = "linux"))]
impl Reader for Unsupported {
    fn tree(&self, _probe: &TreeProbe, _sticky: &[Identity]) -> io::Result<TreeReading> {
        Err(Self::error())
    }
}

#[cfg(not(target_os = "linux"))]
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
    /// platform's reader. Must be called inside a Tokio runtime.
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
        tokio::spawn(sample_trees(probe, interval, reader, tx));
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

/// Logs each field the OS refused, once per sampler.
fn log_denied(logged: &mut Denied, denied: Denied) {
    for (now, before, field) in [
        (denied.memory, &mut logged.memory, "memory"),
        (denied.io, &mut logged.io, "disk I/O"),
        (denied.descriptors, &mut logged.descriptors, "open files"),
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
    /// The computer's CPU and memory every [`INTERVAL`], with this platform's reader. Must be
    /// called inside a Tokio runtime.
    pub fn spawn() -> watch::Receiver<Option<MachineResources>> {
        Self::spawn_with(INTERVAL, platform_machine_reader())
    }

    /// Like [`MachineSampler::spawn`], with another interval or reader (tests).
    pub fn spawn_with(
        interval: Duration,
        reader: Arc<dyn MachineReader>,
    ) -> watch::Receiver<Option<MachineResources>> {
        let (tx, rx) = watch::channel(None);
        tokio::spawn(sample_machine(interval, reader, tx));
        rx
    }
}

/// The busy logical CPUs between two readings: the busy share of the CPU time that passed,
/// times the CPUs. `None` when no time passed.
fn busy_cores(before: &MachineReading, now: &MachineReading) -> Option<f64> {
    let busy = now.cpu_busy.checked_sub(before.cpu_busy)?;
    let idle = now.cpu_idle.checked_sub(before.cpu_idle)?;
    let total = busy + idle;
    (total > 0).then(|| busy as f64 / total as f64 * f64::from(now.logical_cpus))
}

/// The machine sampler's task. It ends when every receiver is dropped, when the platform has
/// no reader, or when the reader panics.
async fn sample_machine(
    interval: Duration,
    reader: Arc<dyn MachineReader>,
    tx: watch::Sender<Option<MachineResources>>,
) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut previous: Option<MachineReading> = None;
    let mut warned = false;
    loop {
        tokio::select! {
            _ = ticker.tick() => {}
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
                    commit_bytes: None,
                    commit_limit_bytes: None,
                    pressure: None,
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
    use std::sync::atomic::{AtomicU32, Ordering};

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
        }
    }

    /// The root alone, `5 × (call + 1)` seconds into the run with 2 cores' worth of CPU.
    fn steady(call: u32, probe: &TreeProbe) -> io::Result<TreeReading> {
        let secs = 5 * (u64::from(call) + 1);
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
                cpu_own: 2.0 * secs as f64,
                cpu_rollup: 0.0,
                memory: Some(1 << 30),
                peak: Some(1 << 30),
                read: Some(0),
                written: Some(0),
                threads: Some(68),
                descriptors: Some(412),
            }],
            denied: Denied::default(),
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
            // 8 CPUs, a quarter busy.
            Ok(MachineReading {
                at: SystemTime::now(),
                logical_cpus: 8,
                cpu_busy: 100 * call,
                cpu_idle: 300 * call,
                memory_total: 16 << 30,
                memory_available: Some(4 << 30),
                swap_total: Some(2 << 30),
                swap_used: Some(0),
            })
        }
    }

    #[tokio::test]
    async fn the_computers_busy_cores_come_from_two_readings() {
        let reader = Arc::new(Machine {
            calls: AtomicU32::new(0),
        });
        let mut rx = MachineSampler::spawn_with(Duration::from_millis(20), reader);
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
        assert_eq!(second.pressure, None);
    }

    #[test]
    fn busy_cores_need_time_to_pass() {
        let reading = |busy, idle| MachineReading {
            at: SystemTime::UNIX_EPOCH,
            logical_cpus: 4,
            cpu_busy: busy,
            cpu_idle: idle,
            memory_total: 0,
            memory_available: None,
            swap_total: None,
            swap_used: None,
        };
        assert_eq!(busy_cores(&reading(10, 10), &reading(10, 10)), None);
        assert_eq!(busy_cores(&reading(10, 10), &reading(30, 10)), Some(4.0));
        // Counters that went backwards (a CPU taken offline) give no figure.
        assert_eq!(busy_cores(&reading(10, 10), &reading(5, 20)), None);
    }
}
