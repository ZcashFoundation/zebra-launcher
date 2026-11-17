//! The `ZingodRunner` (docs/PUBLIC-SERVICES.md, "How Zeal runs zingod"): one actor that owns
//! zingod's supervisor and brings it in line with the plan it is given.
//!
//! ```text
//!   watch<Option<ZingodPlan>> ──▶ runner ──▶ watch<ZingodFacts>
//!   mpsc<Cmd> (Rebuild, Delete) ─▶   │   ──▶ the supervisor's log subscription
//!                                    ▼
//!         supervisor (Launch::Zingod) ── zeal-zingod -c <app data>/public/zingod-<network>.toml start
//! ```
//!
//! The plan names the configuration, its port and the zebrad run zingod reads. A new zebrad
//! run, or a new attempt (the user's Try again, or a restart Z3's rules decide), restarts a
//! running zingod; after a failure only a new attempt starts it again; no plan stops it. The
//! runner writes the configuration before every start, moves to the next free port once per
//! attempt after `PortInUse`, stops zingod while the index's disk is low and starts it again
//! once it isn't ([`disk_allows`]), and restarts it after `DiskFull` only on evidence that
//! covers the failure ([`disk_full_recovery`]). It reads `/status` every 5 s, the free space
//! every minute and the index's size every 10 minutes, and turns the process tree's figures
//! into integers. There are no locks: the plan comes on a `watch`, the commands on an `mpsc`
//! with `oneshot` replies, and every slow read runs in a task that reports back on a channel.

use std::{
    io,
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};

use bytes::Bytes;
use http_body_util::{BodyExt, Empty, Limited};
use launcher_core::{
    firstrun::{dir_size, free_inodes, free_space},
    logfile::RotatingLogConfig,
    paths::Paths,
    policy::RestartPolicy,
    resources::ResourceSampler,
    supervisor::{
        self, Listener, LogSubscription, StartOptions, SupervisorConfig, SupervisorError,
        SupervisorHandle,
    },
    zingod::{ZingodCommand, ZingodConfig, ZingodNetwork, logs::disk_full_kind},
};
use launcher_herd::public::zingod::{
    Cookie, DiskFullEvidence, DiskFullKind, DiskReading, DiskRecovery, ZebradFacts, ZebradRun,
    ZingodFacts, ZingodResources, answering_zero_since, disk_allows, disk_full_recovery,
};
use launcher_types::{
    ExitClass, NodeResources, Phase, PortRole, Status,
    zingod::{IndexerStatus, ZingodStatus},
};
use tokio::{
    net::TcpStream,
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
};

/// How long a stop may take before zingod is killed: its drain is up to 10 s.
pub const STOP_GRACE: Duration = Duration::from_secs(30);

/// How often `/status` is read while zingod runs, each on a new connection.
pub const STATUS_EVERY: Duration = Duration::from_secs(5);

/// How long one read of `/status` may take.
pub const STATUS_TIMEOUT: Duration = Duration::from_secs(2);

/// How often the index's disk is read.
pub const DISK_EVERY: Duration = Duration::from_secs(60);

/// How often the index's size is walked.
pub const SIZE_EVERY: Duration = Duration::from_secs(10 * 60);

/// The most entries one walk of the index visits.
pub const SIZE_LIMIT: usize = 50_000;

/// How far above its port zingod's one move looks for a free one.
const PORT_SEARCH: u16 = 100;

/// The largest `/status` answer read.
const MAX_STATUS: usize = 64 * 1024;

/// What zingod should run with. `None` on the runner's `watch` stops it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZingodPlan {
    /// The configuration; its `listen` port is the one Z3 keeps in the service's spec.
    pub config: ZingodConfig,
    /// The zebrad run zingod reads: a new one restarts a running zingod, and doesn't start a
    /// failed one.
    pub zebrad: ZebradRun,
    /// Bumped for the user's Try again and for a restart the rules decide: a new attempt
    /// starts zingod again after a failure.
    pub attempt: u64,
}

impl ZingodPlan {
    fn key(&self) -> (ZebradRun, u64) {
        (self.zebrad, self.attempt)
    }
}

/// Reads the index's disk.
pub type DiskReader = Arc<dyn Fn(&Path) -> io::Result<DiskReading> + Send + Sync>;

/// The free bytes and inodes of the volume holding `path`.
pub fn read_disk(path: &Path) -> io::Result<DiskReading> {
    Ok(DiskReading {
        free_bytes: free_space(path)?,
        free_inodes: free_inodes(path)?,
    })
}

/// How the runner runs zingod.
#[derive(Clone)]
pub struct RunnerOptions {
    /// `zeal-zingod`.
    pub binary: PathBuf,
    /// The configuration the runner writes before every start.
    pub config_file: PathBuf,
    pub pidfile: PathBuf,
    pub log: RotatingLogConfig,
    pub stop_grace: Duration,
    pub restart: RestartPolicy,
    pub status_every: Duration,
    pub status_timeout: Duration,
    pub disk_every: Duration,
    pub size_every: Duration,
    pub disk: DiskReader,
}

impl std::fmt::Debug for RunnerOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunnerOptions")
            .field("binary", &self.binary)
            .field("config_file", &self.config_file)
            .field("pidfile", &self.pidfile)
            .finish_non_exhaustive()
    }
}

impl RunnerOptions {
    /// Zeal's files for zingod on `network`: `<app data>/public/zingod-<network>.toml` and
    /// `.pid`, and `<log dir>/zingod-<network>.log`, rotated like `zebrad.log`.
    pub fn new(binary: impl Into<PathBuf>, paths: &Paths, network: ZingodNetwork) -> Self {
        let name = format!("zingod-{}", network.name());
        let public = paths.data_dir().join("public");
        RunnerOptions {
            binary: binary.into(),
            config_file: public.join(format!("{name}.toml")),
            pidfile: public.join(format!("{name}.pid")),
            log: RotatingLogConfig::new(paths.log_dir().join(format!("{name}.log"))),
            stop_grace: STOP_GRACE,
            restart: RestartPolicy::default(),
            status_every: STATUS_EVERY,
            status_timeout: STATUS_TIMEOUT,
            disk_every: DISK_EVERY,
            size_every: SIZE_EVERY,
            disk: Arc::new(read_disk),
        }
    }
}

/// What zingod's start waits for about zebrad, from one status of zebrad's supervisor and its
/// cookie. The RPC address in that status is the barrier: the supervisor clears it in the
/// update that records the run, and zebrad logs it just after it writes the run's cookie.
/// The cookie's time isn't compared with the run's start, which the supervisor records only
/// once the spawn has returned.
pub fn zebrad_facts(
    status: &Status,
    on_network: bool,
    cookie: &Path,
    at_tip_once: bool,
) -> Option<ZebradFacts> {
    let cookie = match std::fs::metadata(cookie) {
        Ok(meta) if meta.is_file() && meta.len() > 0 => Cookie::Present,
        Ok(meta) if meta.is_file() => Cookie::Empty,
        // A folder or an unreadable entry: zingod's own start says why.
        Ok(_) => Cookie::Present,
        Err(_) => Cookie::Missing,
    };
    Some(ZebradFacts {
        run: ZebradRun::of(status)?,
        on_network,
        rpc_addr: status.rpc_addr,
        cookie,
        at_tip_once,
    })
}

/// Removes zingod's index and state from `index_dir`, the folder the spec recorded, and
/// nothing else: `indexes/` and `state/` inside it, a symbolic link among them as the link
/// alone, and only when the folder is `…/zingod/<network>`. zingod must be stopped: its
/// `LOCK` files hold the folders. A missing folder has nothing to remove. Names whatever it
/// couldn't remove.
pub fn remove_index(index_dir: &Path) -> io::Result<()> {
    let network = index_dir.file_name().and_then(|name| name.to_str());
    let parent = index_dir
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str());
    if !index_dir.is_absolute()
        || parent != Some("zingod")
        || !matches!(network, Some("mainnet" | "testnet" | "regtest"))
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "refusing to delete in {}: not a zingod index folder",
                index_dir.display()
            ),
        ));
    }
    let root = match index_dir.canonicalize() {
        Ok(root) => root,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    let mut failed = Vec::new();
    for name in ["indexes", "state"] {
        let path = root.join(name);
        let removed = match std::fs::symlink_metadata(&path) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => Err(err),
            // `remove_dir_all` doesn't follow the links inside, and a link is removed alone.
            Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(&path),
            Ok(_) => std::fs::remove_file(&path),
        };
        if let Err(err) = removed {
            failed.push(format!("{}: {err}", path.display()));
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "could not remove {}",
            failed.join("; ")
        )))
    }
}

/// One read of zingod's `/status` on a new HTTP/1.1 connection, within `limit`.
pub async fn read_status(addr: SocketAddr, limit: Duration) -> Option<IndexerStatus> {
    let read = async {
        let stream = TcpStream::connect(addr).await.ok()?;
        let (mut sender, connection) =
            hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(stream))
                .await
                .ok()?;
        let driver = tokio::spawn(async move {
            let _ = connection.await;
        });
        let request = http::Request::get("/status")
            .header(http::header::HOST, addr.to_string())
            .body(Empty::<Bytes>::new())
            .ok()?;
        let response = sender.send_request(request).await.ok();
        let body = match response {
            Some(response) if response.status() == http::StatusCode::OK => {
                Limited::new(response.into_body(), MAX_STATUS)
                    .collect()
                    .await
                    .ok()
                    .map(|body| body.to_bytes())
            }
            _ => None,
        };
        driver.abort();
        let status: ZingodStatus = serde_json::from_slice(&body?).ok()?;
        Some(IndexerStatus::from(status))
    };
    tokio::time::timeout(limit, read).await.ok().flatten()
}

/// The process tree's figures, in integers.
fn resources(sample: &NodeResources) -> ZingodResources {
    ZingodResources {
        rss_bytes: sample.memory_bytes,
        cpu_millicores: sample.cpu_cores.map_or(0, |cores| {
            (cores * 1_000.0).round().clamp(0.0, f64::from(u32::MAX)) as u32
        }),
        read_bytes: sample.read_bytes.unwrap_or(0),
        written_bytes: sample.written_bytes.unwrap_or(0),
    }
}

enum Cmd {
    Rebuild {
        reply: oneshot::Sender<Result<(), String>>,
    },
    Delete {
        reply: oneshot::Sender<Result<(), String>>,
    },
    Subscribe {
        reply: oneshot::Sender<Option<LogSubscription>>,
    },
    Shutdown {
        reply: oneshot::Sender<()>,
    },
}

/// What the runner's tasks report back.
enum Internal {
    Started(Result<(), SupervisorError>),
    Status {
        pid: Option<u32>,
        status: Option<IndexerStatus>,
    },
    /// The disk of the index folder `dir`.
    Disk {
        dir: PathBuf,
        reading: Option<DiskReading>,
    },
    /// The disk just after the `DiskFull` exit at `at`.
    FailedDisk {
        at: SystemTime,
        kind: DiskFullKind,
        reading: Option<DiskReading>,
    },
    Size(u64, bool),
}

/// The running runner. Dropping it closes the actor's commands, which stops zingod and ends
/// the actor.
#[derive(Debug)]
pub struct ZingodRunner {
    cmd: mpsc::Sender<Cmd>,
    facts: watch::Receiver<ZingodFacts>,
    shutdown_cap: Duration,
    _task: JoinHandle<()>,
}

impl ZingodRunner {
    /// Starts the runner. zingod runs while `plan` holds a plan.
    pub fn spawn(options: RunnerOptions, plan: watch::Receiver<Option<ZingodPlan>>) -> Self {
        let (cmd, cmds) = mpsc::channel(8);
        // `disk_allows` is false until the index's disk is first read.
        let (facts_tx, facts) = watch::channel(ZingodFacts::default());
        let (internal_tx, internal) = mpsc::channel(64);
        let shutdown_cap = supervisor::shutdown_cap(options.stop_grace);
        let runner = Runner {
            options,
            plan,
            cmds,
            facts: facts_tx,
            internal_tx,
            internal,
            zingod: None,
            started: None,
            stale_for: None,
            starting: false,
            port: None,
            moved: false,
            held: false,
            last_config: None,
            last_exit_at: None,
            run_pid: None,
            disk_dir: None,
            disk_read: false,
        };
        let task = tokio::spawn(runner.run());
        ZingodRunner {
            cmd,
            facts,
            shutdown_cap,
            _task: task,
        }
    }

    /// The runner's facts, published at every change.
    pub fn facts(&self) -> watch::Receiver<ZingodFacts> {
        self.facts.clone()
    }

    /// How long a caller should wait for [`Self::shutdown`].
    pub fn shutdown_cap(&self) -> Duration {
        self.shutdown_cap
    }

    /// Stops zingod, removes its index and state, and starts it again under the plan.
    pub async fn rebuild(&self) -> Result<(), String> {
        self.ask(|reply| Cmd::Rebuild { reply }).await?
    }

    /// Stops zingod and removes its index and state; it stays stopped until the plan changes.
    pub async fn delete(&self) -> Result<(), String> {
        self.ask(|reply| Cmd::Delete { reply }).await?
    }

    /// zingod's log: what was logged so far, then a live feed. `None` before zingod first ran.
    pub async fn subscribe(&self) -> Option<LogSubscription> {
        self.ask(|reply| Cmd::Subscribe { reply })
            .await
            .ok()
            .flatten()
    }

    /// Stops zingod and ends the runner.
    pub async fn shutdown(&self) {
        let _ = self.ask(|reply| Cmd::Shutdown { reply }).await;
    }

    async fn ask<T>(&self, cmd: impl FnOnce(oneshot::Sender<T>) -> Cmd) -> Result<T, String> {
        let (reply, answer) = oneshot::channel();
        let closed = || "zingod's runner has stopped".to_string();
        self.cmd.send(cmd(reply)).await.map_err(|_| closed())?;
        answer.await.map_err(|_| closed())
    }
}

/// zingod's supervisor, for one home folder.
struct Zingod {
    handle: SupervisorHandle,
    listeners: watch::Sender<Vec<Listener>>,
    status: watch::Receiver<Status>,
    resources: watch::Receiver<Option<NodeResources>>,
    home: PathBuf,
}

struct Runner {
    options: RunnerOptions,
    plan: watch::Receiver<Option<ZingodPlan>>,
    cmds: mpsc::Receiver<Cmd>,
    facts: watch::Sender<ZingodFacts>,
    internal_tx: mpsc::Sender<Internal>,
    internal: mpsc::Receiver<Internal>,
    zingod: Option<Zingod>,
    /// The plan's key the last start was for.
    started: Option<(ZebradRun, u64)>,
    /// The plan's key whose start a stale zingod failed (one Zeal couldn't end): no start
    /// again until the key changes, so a process that outlives `SIGKILL` can't make the runner
    /// spin.
    stale_for: Option<(ZebradRun, u64)>,
    /// A start is under way.
    starting: bool,
    /// The port zingod gets, after a move.
    port: Option<u16>,
    /// The one move after `PortInUse` was made for this attempt.
    moved: bool,
    /// After Delete: stopped until the plan changes.
    held: bool,
    /// The configuration of the last plan, for Delete without one.
    last_config: Option<ZingodConfig>,
    /// The last exit seen, so each is handled once.
    last_exit_at: Option<SystemTime>,
    /// The pid of the run the facts describe.
    run_pid: Option<u32>,
    /// The index folder the disk facts describe.
    disk_dir: Option<PathBuf>,
    /// A reading of `disk_dir`'s disk has come back, failed or not: zingod doesn't start on a
    /// disk not yet read.
    disk_read: bool,
}

/// Waits for a change of `rx`, or forever without one.
async fn changed<T>(rx: Option<&mut watch::Receiver<T>>) -> bool {
    match rx {
        Some(rx) => rx.changed().await.is_ok(),
        None => std::future::pending().await,
    }
}

impl Runner {
    async fn run(mut self) {
        let mut status_tick = tokio::time::interval(self.options.status_every);
        let mut disk_tick = tokio::time::interval(self.options.disk_every);
        let mut size_tick = tokio::time::interval(self.options.size_every);
        for tick in [&mut status_tick, &mut disk_tick, &mut size_tick] {
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        }
        let mut plan_open = true;
        // A plan given before the runner started counts as a change.
        self.on_plan();
        loop {
            let (status_rx, resources_rx) = match self.zingod.as_mut() {
                Some(zingod) => (Some(&mut zingod.status), Some(&mut zingod.resources)),
                None => (None, None),
            };
            tokio::select! {
                changed = self.plan.changed(), if plan_open => {
                    if changed.is_err() {
                        plan_open = false;
                    }
                    self.held = false;
                    self.on_plan();
                }
                cmd = self.cmds.recv() => match cmd {
                    Some(Cmd::Shutdown { reply }) => {
                        self.shutdown().await;
                        let _ = reply.send(());
                        return;
                    }
                    Some(cmd) => self.on_cmd(cmd).await,
                    None => {
                        self.shutdown().await;
                        return;
                    }
                },
                Some(event) = self.internal.recv() => self.on_internal(event),
                alive = changed(status_rx) => {
                    if alive {
                        self.on_zingod_status();
                    }
                }
                alive = changed(resources_rx) => {
                    if alive {
                        self.on_resources();
                    }
                }
                _ = status_tick.tick() => self.read_status(),
                _ = disk_tick.tick() => self.read_disk(),
                _ = size_tick.tick() => self.read_size(),
            }
            self.reconcile().await;
        }
    }

    fn plan(&self) -> Option<ZingodPlan> {
        self.plan.borrow().clone()
    }

    fn index_dir(&self) -> Option<PathBuf> {
        self.plan()
            .map(|plan| plan.config.index_dir)
            .or_else(|| self.last_config.as_ref().map(|c| c.index_dir.clone()))
    }

    fn on_plan(&mut self) {
        if let Some(plan) = self.plan() {
            self.last_config = Some(plan.config.clone());
            // The port move is per attempt: a new zebrad run keeps the moved port.
            if self.started.map(|(_, attempt)| attempt) != Some(plan.attempt) {
                self.port = None;
                self.moved = false;
                self.facts
                    .send_modify(|facts| facts.ports_tried = vec![plan.config.listen.port()]);
            }
        }
        // A new index folder needs its first readings, and its disk starts from none.
        let dir = self.index_dir();
        if dir != self.disk_dir {
            self.disk_dir = dir;
            self.disk_read = false;
            self.facts.send_modify(|facts| {
                facts.disk = None;
                facts.disk_allows = false;
            });
        }
        self.read_disk();
        self.read_size();
    }

    fn phase(&self) -> Phase {
        self.zingod.as_ref().map_or(Phase::Stopped, |zingod| {
            zingod.status.borrow().phase.clone()
        })
    }

    /// Brings zingod in line with the plan, the disk and the last exit.
    async fn reconcile(&mut self) {
        if self.starting {
            return;
        }
        let plan = self.plan().filter(|_| !self.held);
        let phase = self.phase();
        let facts = self.facts.borrow().clone();
        let Some(plan) = plan else {
            if phase.is_active() {
                self.stop().await;
            }
            return;
        };
        let home = plan.config.home_dir();
        if self
            .zingod
            .as_ref()
            .is_some_and(|zingod| zingod.home != home)
        {
            self.stop().await;
            if let Some(old) = self.zingod.take() {
                let _ = old.handle.shutdown().await;
            }
        }
        let current = self.started == Some(plan.key());
        // A failure waits for a new attempt: a new zebrad run doesn't start zingod again.
        let same_attempt = self
            .started
            .is_some_and(|(_, attempt)| attempt == plan.attempt);
        if phase.is_active() {
            if !current || !facts.disk_allows {
                self.stop().await;
            }
            return;
        }
        if !self.disk_read || !facts.disk_allows || self.stale_for == Some(plan.key()) {
            return;
        }
        match phase {
            Phase::Failed { class } if same_attempt => match class {
                ExitClass::PortInUse if !self.moved => {
                    self.moved = true;
                    let from = self.port.unwrap_or(plan.config.listen.port());
                    match next_free_port(from) {
                        Some(port) => {
                            self.port = Some(port);
                            self.facts.send_modify(|facts| facts.ports_tried.push(port));
                            self.start(plan).await;
                        }
                        None => tracing::warn!(from, "no free port for zingod above its own"),
                    }
                }
                ExitClass::DiskFull => {
                    let recovery = facts
                        .disk_full
                        .as_ref()
                        .map_or(DiskRecovery::NeedsUser, |e| {
                            disk_full_recovery(e, facts.disk)
                        });
                    if recovery == DiskRecovery::Restart {
                        self.start(plan).await;
                    }
                }
                _ => {}
            },
            Phase::Stopping => {}
            _ => self.start(plan).await,
        }
    }

    fn ensure_supervisor(&mut self, home: &Path) -> io::Result<()> {
        if self.zingod.is_some() {
            return Ok(());
        }
        let (listeners, listeners_rx) = watch::channel(Vec::new());
        let mut config = SupervisorConfig::new(ZingodCommand::new(
            &self.options.binary,
            &self.options.config_file,
            home,
        ));
        config.stop_grace = self.options.stop_grace;
        config.restart = self.options.restart.clone();
        config.pidfile = Some(self.options.pidfile.clone());
        config.log_file = Some(self.options.log.clone());
        config.listeners = listeners_rx;
        if let Some(parent) = self.options.pidfile.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if let Some(parent) = self.options.log.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let handle = supervisor::spawn(config)?;
        let resources = ResourceSampler::spawn(handle.watch_probe());
        self.zingod = Some(Zingod {
            status: handle.watch_status(),
            handle,
            listeners,
            resources,
            home: home.to_path_buf(),
        });
        Ok(())
    }

    /// Writes the configuration with the current port and starts zingod in a task.
    async fn start(&mut self, plan: ZingodPlan) {
        let mut config = plan.config.clone();
        if let Some(port) = self.port {
            config.listen.set_port(port);
        }
        let written = config
            .ensure_dirs()
            .and_then(|()| config.write(&self.options.config_file));
        if let Err(err) = written {
            tracing::warn!(%err, "could not write zingod's configuration");
            return;
        }
        if let Err(err) = self.ensure_supervisor(&config.home_dir()) {
            tracing::warn!(%err, "could not start zingod's supervisor");
            return;
        }
        let Some(zingod) = &self.zingod else {
            return;
        };
        zingod.listeners.send_replace(vec![Listener {
            role: PortRole::Grpc,
            addr: SocketAddr::from((Ipv4Addr::LOCALHOST, config.listen.port())),
        }]);
        self.started = Some(plan.key());
        self.starting = true;
        self.facts.send_modify(|facts| {
            facts.port = Some(config.listen.port());
            facts.running_for = Some(plan.zebrad);
        });
        let handle = zingod.handle.clone();
        // Sent from the actor, so a stop it sends next is handled after this start.
        let answer = handle.send_start(StartOptions { kill_stale: true }).await;
        let tx = self.internal_tx.clone();
        tokio::spawn(async move {
            let started = match answer {
                Ok(answer) => answer.await,
                Err(err) => Err(err),
            };
            let _ = tx.send(Internal::Started(started)).await;
        });
    }

    async fn stop(&mut self) {
        if let Some(zingod) = &self.zingod {
            let _ = zingod.handle.stop().await;
        }
        self.on_zingod_status();
    }

    async fn shutdown(&mut self) {
        if let Some(zingod) = self.zingod.take() {
            let _ = zingod.handle.shutdown().await;
        }
    }

    async fn on_cmd(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Rebuild { reply } => {
                let removed = self.remove().await;
                let _ = reply.send(removed);
            }
            Cmd::Delete { reply } => {
                self.held = true;
                let removed = self.remove().await;
                let _ = reply.send(removed);
            }
            Cmd::Subscribe { reply } => {
                let subscription = match &self.zingod {
                    Some(zingod) => zingod.handle.subscribe().await.ok(),
                    None => None,
                };
                let _ = reply.send(subscription);
            }
            Cmd::Shutdown { .. } => {}
        }
    }

    /// Stops zingod and removes its index and state, off the actor's thread.
    async fn remove(&mut self) -> Result<(), String> {
        self.stop().await;
        // A start under way can still launch zingod: one that waits for a stale zingod to go
        // goes ahead past a stop. Wait for it to end, stopping whatever it launches.
        while self.starting {
            let status = self.zingod.as_mut().map(|zingod| &mut zingod.status);
            tokio::select! {
                Some(event) = self.internal.recv() => self.on_internal(event),
                alive = changed(status) => {
                    if alive {
                        self.on_zingod_status();
                    }
                }
            }
            if self.phase().is_active() {
                self.stop().await;
            }
        }
        let Some(dir) = self.index_dir() else {
            return Ok(());
        };
        let removed = tokio::task::spawn_blocking(move || remove_index(&dir))
            .await
            .map_err(|err| err.to_string())?
            .map_err(|err| err.to_string());
        self.facts.send_modify(|facts| facts.index_bytes = None);
        self.read_size();
        // A rebuilt index starts from a clean history.
        self.started = None;
        self.stale_for = None;
        removed
    }

    fn on_internal(&mut self, event: Internal) {
        match event {
            Internal::Started(result) => {
                self.starting = false;
                if let Err(SupervisorError::StaleProcess(_)) = &result {
                    self.stale_for = self.started;
                }
                if let Err(err) = result {
                    tracing::debug!(%err, "zingod's start ended");
                }
                self.on_zingod_status();
            }
            Internal::Status { pid, status } => {
                if pid != self.run_pid {
                    return;
                }
                let now = SystemTime::now();
                self.facts.send_modify(|facts| match status {
                    Some(status) => {
                        facts.misses = 0;
                        facts.answering_zero_since =
                            answering_zero_since(facts.answering_zero_since, Some(&status), now);
                        facts.status = Some(status);
                    }
                    None => facts.misses = facts.misses.saturating_add(1),
                });
            }
            Internal::Disk { dir, reading } => {
                // A reading of the folder before a plan moved the index.
                if self.disk_dir.as_ref() != Some(&dir) {
                    return;
                }
                // The first reading of a disk starts the hysteresis: below the low mark blocks,
                // anything else (no reading included) allows.
                let first = !self.disk_read;
                self.disk_read = true;
                self.facts.send_modify(|facts| {
                    facts.disk = reading.or(facts.disk);
                    facts.disk_allows = disk_allows(
                        first || facts.disk_allows,
                        reading.map(|reading| reading.free_bytes),
                    );
                });
            }
            Internal::FailedDisk { at, kind, reading } => {
                // A later exit has come since: this reading isn't its evidence.
                if Some(at) != self.last_exit_at {
                    return;
                }
                self.facts.send_modify(|facts| {
                    facts.disk_full = Some(DiskFullEvidence {
                        kind,
                        at_failure: reading,
                    });
                    if let Some(reading) = reading {
                        facts.disk = Some(reading);
                    }
                });
            }
            Internal::Size(bytes, partial) => {
                self.facts
                    .send_modify(|facts| facts.index_bytes = Some((bytes, partial)));
            }
        }
    }

    /// Takes zingod's supervisor status into the facts: a new run starts its readings over,
    /// and a `DiskFull` exit reads the disk at once, as the evidence of what ran out.
    fn on_zingod_status(&mut self) {
        let Some(zingod) = &self.zingod else {
            return;
        };
        let status = zingod.status.borrow().clone();
        if status.pid.is_some() && status.pid != self.run_pid {
            self.run_pid = status.pid;
            self.facts.send_modify(|facts| {
                facts.status = None;
                facts.misses = 0;
                facts.answering_zero_since = None;
            });
        }
        let new_exit = status
            .last_exit
            .as_ref()
            .filter(|exit| Some(exit.at) != self.last_exit_at);
        if let Some(exit) = new_exit {
            self.last_exit_at = Some(exit.at);
            // An earlier exit's evidence doesn't cover this one: until this exit's reading
            // arrives, the recovery waits for the user.
            self.facts
                .send_if_modified(|facts| facts.disk_full.take().is_some());
            if exit.class == ExitClass::DiskFull {
                let kind = exit
                    .detail
                    .as_deref()
                    .and_then(disk_full_kind)
                    .unwrap_or(DiskFullKind::Unstated);
                let at = exit.at;
                let dir = self.index_dir();
                let disk = self.options.disk.clone();
                let tx = self.internal_tx.clone();
                tokio::spawn(async move {
                    let reading = match dir {
                        Some(dir) => tokio::task::spawn_blocking(move || disk(&dir).ok())
                            .await
                            .ok()
                            .flatten(),
                        None => None,
                    };
                    let _ = tx.send(Internal::FailedDisk { at, kind, reading }).await;
                });
            }
        }
        self.facts.send_if_modified(|facts| {
            let changed = facts.phase != status.phase
                || facts.last_exit != status.last_exit
                || facts.stale != status.stale;
            facts.phase = status.phase.clone();
            facts.last_exit = status.last_exit.clone();
            facts.stale = status.stale.clone();
            changed
        });
    }

    fn on_resources(&mut self) {
        let Some(zingod) = &self.zingod else {
            return;
        };
        let sample = zingod.resources.borrow().as_ref().map(resources);
        self.facts
            .send_if_modified(|facts| std::mem::replace(&mut facts.resources, sample) != sample);
    }

    fn read_status(&mut self) {
        if self.phase() != Phase::Running {
            return;
        }
        let Some(port) = self.facts.borrow().port else {
            return;
        };
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let pid = self.run_pid;
        let limit = self.options.status_timeout;
        let tx = self.internal_tx.clone();
        tokio::spawn(async move {
            let status = read_status(addr, limit).await;
            let _ = tx.send(Internal::Status { pid, status }).await;
        });
    }

    fn read_disk(&mut self) {
        let Some(dir) = self.index_dir() else {
            return;
        };
        let disk = self.options.disk.clone();
        let tx = self.internal_tx.clone();
        tokio::spawn(async move {
            let read = dir.clone();
            let reading = tokio::task::spawn_blocking(move || disk(&read).ok())
                .await
                .ok()
                .flatten();
            let _ = tx.send(Internal::Disk { dir, reading }).await;
        });
    }

    fn read_size(&mut self) {
        let Some(dir) = self.index_dir() else {
            return;
        };
        let tx = self.internal_tx.clone();
        tokio::spawn(async move {
            let walked =
                tokio::task::spawn_blocking(move || dir_size(&dir.join("indexes"), SIZE_LIMIT))
                    .await;
            if let Ok((bytes, partial)) = walked {
                let _ = tx.send(Internal::Size(bytes, partial)).await;
            }
        });
    }
}

/// The first port above `from` that no other program holds.
fn next_free_port(from: u16) -> Option<u16> {
    (from.saturating_add(1)..=from.saturating_add(PORT_SEARCH))
        .find(|port| !supervisor::addr_in_use(SocketAddr::from((Ipv4Addr::LOCALHOST, *port))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_files_are_named_for_the_network() {
        let paths = Paths::under("/zeal");
        let options = RunnerOptions::new("/app/zeal-zingod", &paths, ZingodNetwork::Testnet);
        assert_eq!(
            options.config_file,
            PathBuf::from("/zeal/data/public/zingod-testnet.toml")
        );
        assert_eq!(
            options.pidfile,
            PathBuf::from("/zeal/data/public/zingod-testnet.pid")
        );
        assert_eq!(
            options.log.path,
            PathBuf::from("/zeal/logs/zingod-testnet.log")
        );
        assert_eq!(options.stop_grace, Duration::from_secs(30));
    }

    #[test]
    fn resources_become_integers() {
        let sample = NodeResources {
            run: 1,
            at: SystemTime::UNIX_EPOCH,
            run_secs: 1.0,
            platform: launcher_types::Platform::Linux,
            cpu_secs: 1.0,
            cpu_cores: Some(1.2345),
            memory_bytes: 1_500_000_000,
            peak_memory_bytes: 1_600_000_000,
            committed_bytes: None,
            read_bytes: Some(10),
            written_bytes: None,
            processes: 1,
            threads: Some(4),
            descriptors: Some(9),
            members: Vec::new(),
        };
        assert_eq!(
            resources(&sample),
            ZingodResources {
                rss_bytes: 1_500_000_000,
                cpu_millicores: 1_235,
                read_bytes: 10,
                written_bytes: 0,
            }
        );
    }

    #[test]
    fn only_a_zingod_folder_is_emptied() {
        for dir in [
            "/zeal/zingod/other",
            "/zeal/state/testnet",
            "zingod/testnet",
            "/",
        ] {
            let refused = remove_index(Path::new(dir)).unwrap_err();
            assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied, "{dir}");
        }
        let dir = tempfile::tempdir().unwrap();
        assert!(
            remove_index(&dir.path().join("zingod/testnet")).is_ok(),
            "missing"
        );
    }
}
