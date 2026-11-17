//! `PublicServices`: the one actor that owns the light client server. It reads the stored
//! spec, gathers the facts (zebrad's status and statistics, the light monitor, the network
//! checks, the name, the certificate, the front door), runs `launcher-herd`'s rules over them,
//! carries out what they decide and publishes the status on a `watch`.
//!
//! It never writes `zebrad.toml` or restarts zebrad itself: it sets or clears the spec's
//! `backend_port` through the settings actor, the only writer of `launcher.json`, and asks the
//! [`EndpointHost`] (the app) to apply the managed keys behind the app's permits.
//!
//! With zingod as the backend (docs/PUBLIC-SERVICES.md, "zingod as the backend") it also owns
//! the [`ZingodRunner`]: it records the [`ZingodSpec`] when zingod is first chosen, gives the
//! runner a plan while launcher-herd's `plan_run` allows one, restarts zingod for a stall, for
//! no validator and for the user's Try again, runs the tip cross-check once a minute, and puts
//! the front door in `Unavailable` before it stops zingod itself. zebrad's endpoint goes while
//! zingod is the backend.

use std::{
    future::Future,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    pin::Pin,
    sync::Arc,
    time::{Duration, SystemTime},
};

use launcher_core::{paths::Paths, settings::SettingsHandle, zingod::ZingodNetwork};
use launcher_herd::public::{
    self as rules, Checked, Facts, evidence,
    firewall::firewall_info,
    light::{at_tip, behind, pick_endpoint_port, track_since, wants_endpoint},
    names::{DUCKDNS_ACCOUNT, legacy_duckdns_account, parse_name, plausible_duckdns_token},
    zingod::{self as zingod_rules, CrossCheck, IndexerFacts, Stall, StallVerdict, ZebradRun},
};
use launcher_types::{
    ExitClass, MachineResources, Network, NodeDetails, NodeId, NodeStats, Phase, Settings, Status,
    public::{
        CONSENT_VERSION, CheckId, ConnectionReport, Consent, DnsProvider, Evidence, EvidenceSource,
        FirewallInfo, GatewayMode, GatewayStats, LightBackend, LightdInfo, LocalTestProblem,
        NetFacts, OpenError, PublicHostname, PublicService, PublicServiceSpec, PublicState,
        PublicStatus, SecretRef, StepAnswer, StepId, StillPublic, WanKind, ZingodChoice,
        ZingodSpec,
    },
};
use tokio::sync::{mpsc, oneshot, watch};

use crate::{
    acme::{AcmeCmd, AcmeConfig, AcmeHandle, LETS_ENCRYPT_PRODUCTION, LETS_ENCRYPT_STAGING},
    awake::KeepAwake,
    ddns::{DUCKDNS_API, DdnsCmd, DdnsConfig, DdnsHandle, Fetch, Wanted},
    gateway::FrontDoor,
    light::LightMonitor,
    netcheck,
    secrets::{SecretError, SecretStore},
    selftest,
    store::{self, PublicPaths, State},
    zingod::{
        IndexDisk, RunnerOptions, STOP_GRACE, ZingodPlan, ZingodRunner, cross_check,
        read_index_disk, remove_index, zebrad_facts,
    },
};

/// How often the actor looks again without a change.
const TICK: Duration = Duration::from_secs(10);

/// How often the network checks run while they matter.
const NET_EVERY: Duration = Duration::from_secs(5 * 60);

/// How often the month's upload is written down.
const SAVE_EVERY: Duration = Duration::from_secs(10 * 60);

/// How often the tip cross-check runs while zingod serves: at most once a minute.
pub const CROSS_CHECK_EVERY: Duration = Duration::from_secs(60);

/// How often the actor reads the index folder's disk itself, for the setup and for an index
/// zingod doesn't run on.
pub const INDEX_DISK_EVERY: Duration = Duration::from_secs(60);

/// How many lines of zingod's log the diagnostics carry.
pub const DIAGNOSTIC_LINES: usize = 200;

/// What the app does for the actor: write `node`'s managed keys with the light wallet endpoint
/// on loopback at `port`, or without the endpoint for `None`, and, with `restart`, restart its
/// zebrad if it runs, behind the app's permits. Without `restart` the file changes for zebrad's
/// next start only, as a network change does. `node` is the node the actor's facts come from
/// ([`Wiring::node`]), never another. `port` is the actor's decision, not the stored spec's,
/// so an off that `launcher.json` could not save still removes the endpoint. A node that runs
/// another network than the service's by the time the app holds its permit gets none.
pub trait EndpointHost: Send + Sync + 'static {
    fn apply(
        &self,
        node: NodeId,
        port: Option<u16>,
        restart: bool,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;
}

/// Where the actor gets certificates and names, and what it may do here.
#[derive(Clone, Debug)]
pub struct Options {
    pub paths: PublicPaths,
    /// The launcher's folders: the serving node's data folder and zingod's files.
    pub launcher: Paths,
    /// `zeal-zingod`, where one is bundled (Linux); `None` disables the choice.
    pub zingod_binary: Option<std::path::PathBuf>,
    /// Changes to the runner's options before it starts, for tests.
    pub tune_zingod: Option<fn(&mut RunnerOptions)>,
    /// [`CROSS_CHECK_EVERY`], shorter in tests.
    pub cross_check_every: Duration,
    /// [`INDEX_DISK_EVERY`], shorter in tests.
    pub index_disk_every: Duration,
    /// How the actor reads the index folder's disk: [`read_index_disk`], scripted in tests.
    pub read_index_disk: fn(&std::path::Path) -> IndexDisk,
    /// The span three mismatching cross-checks must cover: launcher-herd's `MISMATCH_SPAN`,
    /// shorter in tests, since the actor's clock is the wall clock.
    pub mismatch_span: Duration,
    /// Public services run on this platform: Linux in PS1.
    pub supported: bool,
    /// The bundled zebrad's version, until zebrad reports its own.
    pub bundled_version: String,
    /// The address the front door listens on: `::`, which takes IPv4 too.
    pub listen_ip: IpAddr,
    pub acme_staging: String,
    pub acme_production: String,
    pub acme_server_root: Option<std::path::PathBuf>,
    pub extra_roots: Vec<rustls::pki_types::CertificateDer<'static>>,
    pub duckdns_api: String,
}

impl Options {
    /// Let's Encrypt and DuckDNS, with the public services' files under the launcher's data
    /// folder, and no zingod.
    pub fn new(launcher: &Paths, bundled_version: &str) -> Self {
        Options {
            paths: PublicPaths::new(launcher.data_dir()),
            launcher: launcher.clone(),
            zingod_binary: None,
            tune_zingod: None,
            cross_check_every: CROSS_CHECK_EVERY,
            index_disk_every: INDEX_DISK_EVERY,
            read_index_disk,
            mismatch_span: zingod_rules::MISMATCH_SPAN,
            supported: cfg!(target_os = "linux"),
            bundled_version: bundled_version.to_string(),
            listen_ip: IpAddr::from(std::net::Ipv6Addr::UNSPECIFIED),
            acme_staging: LETS_ENCRYPT_STAGING.to_string(),
            acme_production: LETS_ENCRYPT_PRODUCTION.to_string(),
            acme_server_root: None,
            extra_roots: Vec::new(),
            duckdns_api: DUCKDNS_API.to_string(),
        }
    }
}

/// What the actor needs from the rest of the app.
pub struct Wiring {
    pub settings: SettingsHandle,
    /// The serving node's spec: its id and the network it runs on. With `status` and `stats`,
    /// it follows the first node (`RegistryHandle::follow_first`).
    pub node: watch::Receiver<launcher_core::nodes::NodeSpec>,
    pub status: watch::Receiver<Status>,
    pub stats: watch::Receiver<Option<NodeStats>>,
    /// The serving node's details, for the network upgrades zingod's gate reads.
    pub details: watch::Receiver<Option<NodeDetails>>,
    /// This computer's newest sample, for its memory.
    pub machine: watch::Receiver<Option<MachineResources>>,
    pub host: Arc<dyn EndpointHost>,
    pub secrets: Arc<dyn SecretStore>,
    pub fetch: Arc<dyn Fetch>,
}

/// Applies "resume when Zeal starts" to the stored settings, before the first `ensure` at
/// launch writes the managed keys: a service that does not resume, or is switched off, loses
/// its endpoint before zebrad's first start. Returns whether an open service is held for the
/// user's switch.
pub fn prepare_launch(settings: &mut Settings) -> bool {
    let Some(spec) = settings.public.light_server_mut() else {
        return false;
    };
    let held = spec.switch_on && spec.opened && !spec.resume_on_launch;
    // A consent this release no longer covers waits for the user too.
    if !spec.switch_on || held || !spec.consent_current() {
        spec.backend_port = None;
    }
    held
}

type Reply<T> = oneshot::Sender<T>;

enum Msg {
    BeginSetup {
        network: Network,
        reply: Reply<Result<NodeId, String>>,
    },
    Switch {
        on: bool,
        reply: Reply<Result<(), String>>,
    },
    Answer {
        answer: StepAnswer,
        reply: Reply<Result<(), String>>,
    },
    RunChecks {
        step: StepId,
        reply: Reply<Result<(), String>>,
    },
    Open {
        reply: Reply<Result<(), OpenError>>,
    },
    Forget {
        remove_token: bool,
        remove_index: bool,
        reply: Reply<Result<StillPublic, String>>,
    },
    CheckConnection {
        reply: Reply<ConnectionReport>,
    },
    Rebuild {
        reply: Reply<Result<(), String>>,
    },
    DeleteIndex {
        reply: Reply<Result<(), String>>,
    },
    Diagnostics {
        reply: Reply<Option<String>>,
    },
    Shutdown {
        reply: Reply<()>,
    },
    StopIndexer {
        reply: Reply<()>,
    },
}

enum Internal {
    /// A child's watch changed.
    Poke,
    /// A network check's facts, from the run of the service numbered `epoch`.
    Net {
        net: NetFacts,
        epoch: u64,
    },
    LocalTest(Checked<Result<LightdInfo, LocalTestProblem>>),
    Hairpin(Checked<bool>),
    Applied {
        target: Option<u16>,
        result: Result<(), String>,
    },
    Door(launcher_gateway::Event),
    /// The actor's own reading of the index folder `dir`.
    IndexDisk {
        dir: std::path::PathBuf,
        read: IndexDisk,
    },
    /// A tip cross-check: the height zingod served and what zebrad said at it.
    TipCheck(Option<(u64, zingod_rules::TipCheck)>),
}

/// The public services. Cheap to clone; the actor stops when the last handle is dropped.
#[derive(Clone, Debug)]
pub struct PublicServices {
    tx: mpsc::Sender<Msg>,
    status: watch::Receiver<Vec<PublicStatus>>,
}

macro_rules! ask {
    ($self:ident, $variant:ident { $($field:ident),* }) => {{
        let (reply, answer) = oneshot::channel();
        $self
            .tx
            .send(Msg::$variant { $($field,)* reply })
            .await
            .map_err(|_| "the public services have stopped".to_string())?;
        answer
            .await
            .map_err(|_| "the public services have stopped".to_string())
    }};
}

impl PublicServices {
    /// Starts the actor. `held_at_launch` comes from [`prepare_launch`].
    pub fn spawn(options: Options, wiring: Wiring, held_at_launch: bool) -> Self {
        let (tx, rx) = mpsc::channel(16);
        let (status_tx, status) = watch::channel(Vec::new());
        let actor = Actor::new(options, wiring, status_tx, held_at_launch);
        tokio::spawn(actor.run(rx));
        PublicServices { tx, status }
    }

    /// Every service's status; empty while nothing is set up.
    pub fn statuses(&self) -> Vec<PublicStatus> {
        self.status.borrow().clone()
    }

    pub fn watch(&self) -> watch::Receiver<Vec<PublicStatus>> {
        self.status.clone()
    }

    /// Sets up the light client server for zebrad on `network`, switched on and waiting for
    /// consent, and returns the serving node.
    pub async fn begin_setup(&self, network: Network) -> Result<NodeId, String> {
        ask!(self, BeginSetup { network })?
    }

    /// Off pauses at once; on resumes the setup or the service.
    pub async fn set_switch(&self, on: bool) -> Result<(), String> {
        ask!(self, Switch { on })?
    }

    pub async fn answer(&self, answer: StepAnswer) -> Result<(), String> {
        ask!(self, Answer { answer })?
    }

    pub async fn run_checks(&self, step: StepId) -> Result<(), String> {
        ask!(self, RunChecks { step })?
    }

    /// "Open to the internet": refused unless every gate passes now.
    pub async fn open(&self) -> Result<(), OpenError> {
        let (reply, answer) = oneshot::channel();
        let stopped = || OpenError {
            check: None,
            message: "the public services have stopped".into(),
        };
        self.tx
            .send(Msg::Open { reply })
            .await
            .map_err(|_| stopped())?;
        answer.await.map_err(|_| stopped())?
    }

    /// Turns the service off and forgets it, returning what Zeal could not undo. With
    /// `remove_index`, zingod's index goes too, once zingod has stopped.
    pub async fn forget(
        &self,
        remove_token: bool,
        remove_index: bool,
    ) -> Result<StillPublic, String> {
        ask!(
            self,
            Forget {
                remove_token,
                remove_index
            }
        )?
    }

    /// Stops zingod, removes its index and state in the recorded folder, and starts it again:
    /// hours of "try again" for wallets.
    pub async fn rebuild_index(&self) -> Result<(), String> {
        ask!(self, Rebuild {})?
    }

    /// Stops zingod and removes its index and state in the recorded folder, after switching
    /// back to zebrad's endpoint.
    pub async fn delete_index(&self) -> Result<(), String> {
        ask!(self, DeleteIndex {})?
    }

    /// zingod's part of the diagnostics: its status, its configuration's paths and the last
    /// [`DIAGNOSTIC_LINES`] of its log, redacted. `None` while zingod isn't set up.
    pub async fn indexer_diagnostics(&self) -> Option<String> {
        let asked: Result<Option<String>, String> = async { ask!(self, Diagnostics {}) }.await;
        asked.ok().flatten()
    }

    /// How long [`Self::stop_indexer`] may take: zingod's stop grace and its kill.
    pub fn indexer_shutdown_cap(&self) -> Duration {
        launcher_core::supervisor::shutdown_cap(STOP_GRACE)
    }

    /// "Check my connection": the read-only network checks, without setting anything up.
    pub async fn check_connection(&self) -> Result<ConnectionReport, String> {
        ask!(self, CheckConnection {})
    }

    /// Closes the front door and lets go of everything but zingod, for Zeal's exit:
    /// [`Self::stop_indexer`] stops zingod after it, beside the nodes.
    pub async fn shutdown(&self) {
        let (reply, answer) = oneshot::channel();
        if self.tx.send(Msg::Shutdown { reply }).await.is_ok() {
            let _ = answer.await;
        }
    }

    /// Stops zingod and ends the actor, for Zeal's exit after [`Self::shutdown`].
    pub async fn stop_indexer(&self) {
        let (reply, answer) = oneshot::channel();
        if self.tx.send(Msg::StopIndexer { reply }).await.is_ok() {
            let _ = answer.await;
        }
    }
}

/// Forwards every change of `rx` as a poke, so the actor looks again.
fn forward_changes<T: Send + Sync + 'static>(
    mut rx: watch::Receiver<T>,
    tx: mpsc::Sender<Internal>,
) -> tokio::task::AbortHandle {
    tokio::spawn(async move {
        while rx.changed().await.is_ok() {
            let _ = tx.try_send(Internal::Poke);
        }
    })
    .abort_handle()
}

/// A child with the tasks that forward its changes, stopped together.
struct Child<T> {
    key: String,
    handle: T,
    forwards: Vec<tokio::task::AbortHandle>,
}

impl<T> Drop for Child<T> {
    fn drop(&mut self) {
        for forward in &self.forwards {
            forward.abort();
        }
    }
}

struct Actor {
    options: Options,
    settings: SettingsHandle,
    node_rx: watch::Receiver<launcher_core::nodes::NodeSpec>,
    status_rx: watch::Receiver<Status>,
    stats_rx: watch::Receiver<Option<NodeStats>>,
    details_rx: watch::Receiver<Option<NodeDetails>>,
    machine_rx: watch::Receiver<Option<MachineResources>>,
    host: Arc<dyn EndpointHost>,
    secrets: Arc<dyn SecretStore>,
    fetch: Arc<dyn Fetch>,
    out: watch::Sender<Vec<PublicStatus>>,
    internal_tx: mpsc::Sender<Internal>,
    internal_rx: Option<mpsc::Receiver<Internal>>,

    /// A pause only the user's switch ends.
    held: Option<launcher_types::public::PauseReason>,
    paused: Option<(String, SystemTime)>,
    at_tip_since: Option<SystemTime>,
    behind_since: Option<SystemTime>,
    name_elsewhere_since: Option<SystemTime>,
    net: Option<NetFacts>,
    net_due: Option<tokio::time::Instant>,
    net_running: bool,
    /// Counts the times the service stopped watching its surroundings
    /// ([`rules::watches_surroundings`]), so a network check from before is told apart.
    net_epoch: u64,
    /// A network check made since the service last started watching has come back.
    net_fresh: bool,
    was_watching: bool,
    firewall: Option<FirewallInfo>,
    port_free: Option<bool>,
    local_test: Option<Checked<Result<LightdInfo, LocalTestProblem>>>,
    local_running: bool,
    hairpin: Option<Checked<bool>>,
    hairpin_running: bool,
    keychain_unavailable: bool,
    /// The endpoint port the app last applied to `zebrad.toml`.
    applied: Option<Option<u16>>,
    applying: bool,
    endpoint_error: Option<String>,
    acme_asked: bool,
    state: State,
    state_saved: tokio::time::Instant,
    /// This run's upload this month.
    upload: Upload,

    light: Option<Child<LightMonitor>>,
    ddns: Option<Child<DdnsHandle>>,
    ddns_wanted: watch::Sender<Wanted>,
    acme: Option<Child<AcmeHandle>>,
    door: Option<Child<FrontDoor>>,
    challenge: watch::Sender<Option<Arc<launcher_gateway::Challenge>>>,
    mode: watch::Sender<launcher_gateway::Mode>,
    can_validate: watch::Sender<bool>,
    awake: Option<KeepAwake>,
    /// The front door has closed for Zeal's exit: nothing starts again.
    closed: bool,

    /// The backend's endpoint the light monitor checks.
    endpoint: watch::Sender<Option<SocketAddr>>,
    zingod: Zingod,
}

/// What the actor keeps about zingod between looks.
struct Zingod {
    mismatch_span: Duration,
    runner: Option<Child<ZingodRunner>>,
    plan: watch::Sender<Option<ZingodPlan>>,
    /// Bumped for the user's Try again and for a restart the rules decide.
    attempt: u64,
    /// zebrad's run, and whether it has been at the tip in it.
    tip_run: Option<(ZebradRun, bool)>,
    behind_since: Option<SystemTime>,
    stall: Stall,
    stuck: Option<(u64, u64)>,
    /// The zebrad run zingod was restarted for when no validator answered: once per run.
    no_validator_restart: Option<ZebradRun>,
    cross: CrossCheck,
    mismatch: Option<u64>,
    tip: Option<Checked<zingod_rules::TipCheck>>,
    tip_due: Option<tokio::time::Instant>,
    tip_running: bool,
    /// The actor's own reading of the index folder.
    disk: Option<(std::path::PathBuf, IndexDisk)>,
    disk_due: Option<tokio::time::Instant>,
    disk_running: bool,
}

impl Zingod {
    fn new(mismatch_span: Duration) -> Self {
        Zingod {
            mismatch_span,
            runner: None,
            plan: watch::channel(None).0,
            attempt: 0,
            tip_run: None,
            behind_since: None,
            stall: Stall::default(),
            stuck: None,
            no_validator_restart: None,
            cross: CrossCheck::with_span(mismatch_span),
            mismatch: None,
            tip: None,
            tip_due: None,
            tip_running: false,
            disk: None,
            disk_due: None,
            disk_running: false,
        }
    }

    /// Starts over the rules' memory: after a rebuild, a delete, a new backend or a forget.
    fn forget_history(&mut self) {
        self.behind_since = None;
        self.stall = Stall::default();
        self.stuck = None;
        self.cross = CrossCheck::with_span(self.mismatch_span);
        self.mismatch = None;
        self.tip = None;
    }

    fn facts(&self) -> launcher_herd::public::zingod::ZingodFacts {
        self.runner
            .as_ref()
            .map(|runner| runner.handle.facts().borrow().clone())
            .unwrap_or_default()
    }
}

fn door_mode(mode: GatewayMode) -> launcher_gateway::Mode {
    match mode {
        GatewayMode::ChallengeOnly => launcher_gateway::Mode::ChallengeOnly,
        GatewayMode::LocalOnly => launcher_gateway::Mode::LocalOnly,
        GatewayMode::Open => launcher_gateway::Mode::Open,
        GatewayMode::Unavailable => launcher_gateway::Mode::Unavailable,
    }
}

fn door_limits(limits: &launcher_types::public::PublicLimits) -> launcher_gateway::Limits {
    launcher_gateway::Limits {
        max_connections: limits.max_connections,
        per_address: limits.per_address,
        new_per_second: limits.new_per_second,
        new_burst: limits.new_burst,
        upload_kbit: limits.upload_kbit,
    }
}

/// This run's upload this month: what its stopped front doors sent, and the running one's
/// count, which goes on from before the month began.
#[derive(Debug, Default)]
struct Upload {
    /// Sent this month by front doors that stopped.
    stopped: u64,
    /// The running front door's count when the month began.
    offset: u64,
}

impl Upload {
    /// This run's upload this month, with the running front door's count `door`.
    fn this_run(&self, door: Option<u64>) -> u64 {
        self.stopped + door.map_or(0, |door| door.saturating_sub(self.offset))
    }

    fn door_stopped(&mut self, door: u64) {
        self.stopped += door.saturating_sub(self.offset);
        self.offset = 0;
    }

    fn new_month(&mut self, door: Option<u64>) {
        self.stopped = 0;
        self.offset = door.unwrap_or(0);
    }
}

/// Where the DuckDNS token is kept.
fn duckdns_secret() -> SecretRef {
    SecretRef {
        provider: DnsProvider::DuckDns,
        account: DUCKDNS_ACCOUNT.to_string(),
    }
}

/// `YYYY-MM` of `at`, in UTC.
fn month_of(at: SystemTime) -> String {
    launcher_core::clock::rfc3339(at).chars().take(7).collect()
}

impl Actor {
    fn new(
        options: Options,
        wiring: Wiring,
        out: watch::Sender<Vec<PublicStatus>>,
        held_at_launch: bool,
    ) -> Self {
        let (internal_tx, internal_rx) = mpsc::channel(64);
        let state: State = store::load(&options.paths.state());
        let mismatch_span = options.mismatch_span;
        let applied = wiring
            .settings
            .current()
            .public
            .light_server()
            .map(|spec| spec.backend_port);
        Actor {
            options,
            settings: wiring.settings,
            node_rx: wiring.node,
            status_rx: wiring.status,
            stats_rx: wiring.stats,
            details_rx: wiring.details,
            machine_rx: wiring.machine,
            host: wiring.host,
            secrets: wiring.secrets,
            fetch: wiring.fetch,
            out,
            internal_tx,
            internal_rx: Some(internal_rx),
            held: held_at_launch.then_some(launcher_types::public::PauseReason::NotResumed),
            paused: None,
            at_tip_since: None,
            behind_since: None,
            name_elsewhere_since: None,
            net: None,
            net_due: None,
            net_running: false,
            net_epoch: 0,
            net_fresh: false,
            was_watching: false,
            firewall: None,
            port_free: None,
            local_test: None,
            local_running: false,
            hairpin: None,
            hairpin_running: false,
            keychain_unavailable: false,
            applied,
            applying: false,
            endpoint_error: None,
            acme_asked: false,
            state,
            state_saved: tokio::time::Instant::now(),
            upload: Upload::default(),
            light: None,
            ddns: None,
            ddns_wanted: watch::channel(Wanted::default()).0,
            acme: None,
            door: None,
            challenge: watch::channel(None).0,
            mode: watch::channel(launcher_gateway::Mode::ChallengeOnly).0,
            can_validate: watch::channel(false).0,
            awake: None,
            closed: false,
            endpoint: watch::channel(None).0,
            zingod: Zingod::new(mismatch_span),
        }
    }

    async fn run(mut self, mut rx: mpsc::Receiver<Msg>) {
        let mut internal = self.internal_rx.take().expect("taken once");
        let mut status = self.status_rx.clone();
        let mut stats = self.stats_rx.clone();
        let mut settings = self.settings.watch();
        let mut node = self.node_rx.clone();
        let mut details = self.details_rx.clone();
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            if !self.closed {
                self.step().await;
            }
            tokio::select! {
                msg = rx.recv() => match msg {
                    Some(Msg::Shutdown { reply }) => {
                        self.shutdown();
                        let _ = reply.send(());
                    }
                    Some(Msg::StopIndexer { reply }) => {
                        self.shutdown();
                        self.stop_indexer().await;
                        let _ = reply.send(());
                        return;
                    }
                    Some(_) if self.closed => {}
                    Some(msg) => self.handle(msg).await,
                    None => {
                        self.shutdown();
                        self.stop_indexer().await;
                        return;
                    }
                },
                Some(event) = internal.recv() => self.internal(event),
                // A closed watch turns its arm off instead of waking the actor at once forever.
                Ok(()) = status.changed() => {}
                Ok(()) = stats.changed() => {}
                Ok(()) = settings.changed() => {}
                // A network change holds the service at once.
                Ok(()) = node.changed() => {}
                Ok(()) = details.changed() => {}
                _ = tick.tick() => {}
            }
        }
    }

    fn spec(&self) -> Option<PublicServiceSpec> {
        self.settings.current().public.light_server().cloned()
    }

    async fn update_spec(
        &self,
        f: impl FnOnce(&mut PublicServiceSpec) + Send + 'static,
    ) -> Result<(), String> {
        self.settings
            .update(move |settings| {
                if let Some(spec) = settings.public.light_server_mut() {
                    f(spec);
                }
            })
            .await
            .map(|_| ())
            .map_err(|err| err.to_string())
    }

    /// The network the serving node runs on.
    fn network(&self) -> launcher_types::Network {
        self.node_rx.borrow().network
    }

    /// The facts at `now` for `spec`.
    fn facts(&self, spec: &PublicServiceSpec, now: SystemTime) -> Facts {
        let status = self.status_rx.borrow().clone();
        let stats = self.stats_rx.borrow().clone();
        let mut facts = Facts::new(now, self.network());
        facts.supported = self.options.supported;
        facts.zebrad_version = stats
            .as_ref()
            .map(|stats| stats.zebrad_version.clone())
            .filter(|version| !version.is_empty())
            .or_else(|| Some(self.options.bundled_version.clone()));
        facts.endpoint_port_taken = status.phase
            == (Phase::Failed {
                class: ExitClass::PortInUse,
            })
            && status
                .last_exit
                .as_ref()
                .and_then(|exit| exit.detail.as_deref())
                .is_some_and(|detail| detail.contains("lightwalletd"));
        facts.phase = status.phase.clone();
        facts.endpoint = status.lightwalletd_addr;
        facts.upgrades = self
            .details_rx
            .borrow()
            .as_ref()
            .map(|details| details.upgrades.clone())
            .unwrap_or_default();
        facts.endpoint_error = self.endpoint_error.clone();
        facts.height = stats.as_ref().map(|stats| stats.height);
        facts.tip = stats.as_ref().map(|stats| stats.estimated_height);
        facts.at_tip_since = self.at_tip_since;
        facts.behind_since = self.behind_since;
        facts.wallet = self
            .light
            .as_ref()
            .and_then(|light| light.handle.result.borrow().clone());
        facts.net = self.net.clone();
        facts.dns = self.ddns.as_ref().and_then(|ddns| {
            let status = ddns.handle.status.borrow().clone();
            (Some(&status.name) == spec.name.as_ref()).then_some(status)
        });
        facts.keychain_unavailable = self.keychain_unavailable;
        facts.port_free = self.port_free;
        facts.firewall = self.firewall.clone();
        facts.cert = self
            .acme
            .as_ref()
            .filter(|acme| {
                spec.name
                    .as_ref()
                    .is_some_and(|name| acme.key == name.as_str())
            })
            .map(|acme| acme.handle.status.borrow().clone());
        facts.local_test = self.local_test.clone();
        facts.hairpin = self.hairpin.clone();
        facts.evidence = spec
            .name
            .as_ref()
            .and_then(|name| self.state.names.get(name.as_str()))
            .map(|proven| proven.evidence.clone())
            .unwrap_or_default();
        facts.name_elsewhere_since = self.name_elsewhere_since;
        let stats = self
            .door
            .as_ref()
            .map(|door| door.handle.stats.borrow().clone())
            .unwrap_or_default();
        facts.month_bytes_up = self.state.month_bytes_up + self.upload.this_run(self.door_up());
        facts.gateway = GatewayStats {
            open_connections: stats.open_connections,
            client_networks: stats.client_networks,
            handshakes: stats.handshakes,
            handshakes_failed: stats.handshakes_failed,
            refused_sni: stats.refused_sni,
            refused_limits: stats.refused_limits,
            bytes_up: stats.bytes_up,
            bytes_down: stats.bytes_down,
        };
        facts.awake_held = self.awake.is_some();
        facts.held = self.held.clone();
        facts.zingod = self.indexer_facts(spec, &status);
        facts
    }

    /// The serving node's data folder.
    fn node_data(&self) -> std::path::PathBuf {
        self.node_rx.borrow().data_dir(&self.options.launcher)
    }

    /// zingod's index folder for `spec`: the recorded one, or where a first choice puts it.
    fn index_dir(&self, spec: &PublicServiceSpec) -> std::path::PathBuf {
        spec.zingod.as_ref().map_or_else(
            || zingod_rules::index_dir(&self.node_data(), spec.network),
            |zingod| zingod.dir.clone(),
        )
    }

    /// What the rules read about zingod, while it is bundled or was chosen.
    fn indexer_facts(&self, spec: &PublicServiceSpec, status: &Status) -> Option<IndexerFacts> {
        let bundled = self.options.zingod_binary.is_some();
        if !bundled && spec.backend != LightBackend::Zingod && spec.zingod.is_none() {
            return None;
        }
        let dir = self.index_dir(spec);
        let runner = self.zingod.facts();
        let own = self
            .zingod
            .disk
            .as_ref()
            .filter(|(read, _)| *read == dir)
            .map(|(_, read)| *read)
            .unwrap_or_default();
        let node = self.node_rx.borrow().clone();
        let cookie = node.paths(&self.options.launcher).rpc_cookie_file();
        let run = ZebradRun::of(status);
        let at_tip_once = self
            .zingod
            .tip_run
            .is_some_and(|(tip_run, at_tip)| Some(tip_run) == run && at_tip);
        // The rules see zingod as starting through a restart asked for, so the front door
        // stays `Unavailable` until it serves again.
        let restarting = runner.restarting(self.zingod.attempt);
        Some(IndexerFacts {
            bundled,
            version: launcher_types::zingod::version().to_owned(),
            disk: runner.disk.or(own.disk),
            network_fs: own.network_fs,
            total_memory: self
                .machine_rx
                .borrow()
                .as_ref()
                .map(|machine| machine.memory_total_bytes),
            zebrad: zebrad_facts(status, node.network == spec.network, &cookie, at_tip_once),
            runner: launcher_herd::public::zingod::ZingodFacts {
                index_bytes: runner.index_bytes.or(own.index_bytes),
                ..runner
            },
            behind_since: self.zingod.behind_since,
            stuck: self.zingod.stuck,
            mismatch: self.zingod.mismatch,
            tip: self.zingod.tip.clone(),
            restarting,
            dir,
        })
    }

    /// One look: the facts, the rules, the children, the status.
    async fn step(&mut self) {
        let now = SystemTime::now();
        let settings = self.settings.current();
        let Some(spec) = settings.public.light_server().cloned() else {
            self.stop_children();
            self.zingod.plan.send_replace(None);
            self.zingod.runner = None;
            self.out.send_if_modified(|statuses| {
                let changed = !statuses.is_empty();
                statuses.clear();
                changed
            });
            return;
        };
        self.roll_month(now);
        if spec.network != self.network() {
            // Switching back does not resume by itself.
            self.held = Some(launcher_types::public::PauseReason::NetworkChanged);
        }
        let facts = self.facts(&spec, now);
        self.sync_endpoint(&spec, &facts, self.network()).await;
        let spec = self.spec().unwrap_or(spec);

        let mut facts = self.facts(&spec, now);
        self.at_tip_since = track_since(self.at_tip_since, at_tip(facts.height, facts.tip), now);
        facts.at_tip_since = self.at_tip_since;
        self.behind_since = track_since(self.behind_since, behind(facts.height, facts.tip), now);
        facts.behind_since = self.behind_since;
        self.name_elsewhere_since = rules::name_elsewhere_since(&spec, &facts);
        facts.name_elsewhere_since = self.name_elsewhere_since;
        if self.track_zingod(&spec, &facts, now).await {
            facts = self.facts(&spec, now);
            facts.at_tip_since = self.at_tip_since;
            facts.behind_since = self.behind_since;
            facts.name_elsewhere_since = self.name_elsewhere_since;
        }

        let steps = rules::plan(&spec, &facts);
        let pause = rules::pause(&spec, &facts);
        let key = pause.as_ref().map(|(why, _)| format!("{why:?}"));
        match (&self.paused, key) {
            (Some((old, _)), Some(new)) if *old == new => {}
            (_, Some(new)) => self.paused = Some((new, now)),
            (_, None) => self.paused = None,
        }
        let since = self.paused.as_ref().map_or(now, |(_, since)| *since);
        let state = rules::state(&spec, &facts, &steps, since);
        let mode = rules::gateway_mode(&state, &facts);
        self.drive(&spec, &facts, &steps, &state, mode).await;

        let facts = self.facts(&spec, now);
        let mut facts = facts;
        facts.at_tip_since = self.at_tip_since;
        facts.behind_since = self.behind_since;
        facts.name_elsewhere_since = self.name_elsewhere_since;
        let status = rules::status(&spec, &facts, since);
        self.out.send_if_modified(|statuses| {
            let changed = statuses.len() != 1 || statuses[0] != status;
            *statuses = vec![status];
            changed
        });
    }

    /// Follows zingod between looks: zebrad at the tip in its run, zingod behind, the stall
    /// rule, the restart when no validator answers, a moved port into the spec. Returns whether
    /// the facts changed, a restart's new attempt included, so the look doesn't open the front
    /// door again for a zingod it is restarting.
    async fn track_zingod(
        &mut self,
        spec: &PublicServiceSpec,
        facts: &Facts,
        now: SystemTime,
    ) -> bool {
        let Some(zingod) = &facts.zingod else {
            return false;
        };
        let run = ZebradRun::of(&self.status_rx.borrow());
        let at_tip = at_tip(facts.height, facts.tip);
        let mut changed = false;
        let next = run.map(|run| match self.zingod.tip_run {
            Some((known, was)) if known == run => (run, was || at_tip),
            _ => (run, at_tip),
        });
        if next != self.zingod.tip_run {
            self.zingod.tip_run = next;
            changed = true;
        }
        if spec.backend != LightBackend::Zingod {
            return changed;
        }
        let runner = &zingod.runner;
        let status = runner
            .status
            .as_ref()
            .filter(|_| runner.phase == Phase::Running);
        let behind_since = track_since(
            self.zingod.behind_since,
            zingod_rules::behind(status, facts.height),
            now,
        );
        changed |= behind_since != self.zingod.behind_since;
        self.zingod.behind_since = behind_since;
        let health = zingod_rules::health(
            status,
            runner.misses,
            runner.answering_zero_since,
            facts.height,
            now,
        );
        let building = matches!(health, zingod_rules::IndexerHealth::Building { .. });
        let stuck = match self.zingod.stall.observe(
            status.and_then(|status| status.served),
            facts.height,
            building,
            now,
        ) {
            StallVerdict::Moving => None,
            StallVerdict::Restart => {
                tracing::warn!(target: "zeal::public", "zingod is stalled; restarting it once");
                self.restart_zingod();
                changed = true;
                None
            }
            StallVerdict::Stuck { at, zebrad_at } => Some((at, zebrad_at)),
        };
        changed |= stuck != self.zingod.stuck;
        self.zingod.stuck = stuck;
        // A missed cookie change looks like no validator: restart zingod once per zebrad run.
        if health == zingod_rules::IndexerHealth::NoValidator
            && run.is_some()
            && self.zingod.no_validator_restart != run
        {
            tracing::warn!(target: "zeal::public", "zingod can't reach zebrad; restarting it once");
            self.zingod.no_validator_restart = run;
            self.restart_zingod();
            changed = true;
        }
        // The runner's one move after `PortInUse` goes into the spec.
        if let (Some(port), Some(recorded)) = (runner.port, &spec.zingod)
            && port != recorded.port
            && runner.phase == Phase::Running
        {
            let _ = self
                .update_spec(move |spec| {
                    if let Some(zingod) = &mut spec.zingod {
                        zingod.port = port;
                    }
                })
                .await;
        }
        changed
    }

    /// A new attempt for zingod: the runner restarts it, or starts it again after a failure
    /// that waited for the user. The front door goes `Unavailable` first, and stays so while
    /// zingod runs for an earlier attempt (`IndexerFacts.restarting`).
    fn restart_zingod(&mut self) {
        self.door_unavailable();
        self.zingod.attempt += 1;
    }

    /// Puts a running front door in `Unavailable`, before Zeal stops zingod itself.
    fn door_unavailable(&mut self) {
        if self.door.is_some() {
            self.mode.send_if_modified(|current| {
                std::mem::replace(current, launcher_gateway::Mode::Unavailable)
                    != launcher_gateway::Mode::Unavailable
            });
        }
    }

    /// Sets or clears the spec's `backend_port` and has the app apply it.
    async fn sync_endpoint(&mut self, spec: &PublicServiceSpec, facts: &Facts, network: Network) {
        let wanted = wants_endpoint(spec, facts);
        if wanted && spec.backend_port.is_none() && self.endpoint_error.is_none() && !self.applying
        {
            let free =
                |port: u16| std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_err();
            if let Some(port) = pick_endpoint_port(network, free) {
                let _ = self
                    .update_spec(move |spec| spec.backend_port = Some(port))
                    .await;
            }
        } else if !wanted && spec.backend_port.is_some() {
            // Retried at every look while launcher.json cannot be written.
            let _ = self.update_spec(|spec| spec.backend_port = None).await;
        }
        // What zebrad should have, saved or not: an unwanted endpoint goes even while the
        // stored spec still has its port.
        let target = if wanted {
            self.spec().and_then(|spec| spec.backend_port)
        } else {
            None
        };
        if !self.applying && self.applied != Some(target) {
            self.applying = true;
            // zebrad on another network than the service's keeps running as it is: the key
            // goes from the file before its next start.
            let restart = spec.network == network;
            let host = self.host.clone();
            let node = self.node_rx.borrow().id.clone();
            let tx = self.internal_tx.clone();
            tokio::spawn(async move {
                let result = host.apply(node, target, restart).await;
                let _ = tx.send(Internal::Applied { target, result }).await;
            });
        }
    }

    fn internal(&mut self, event: Internal) {
        match event {
            Internal::Poke => {}
            Internal::Net { net, epoch } => {
                self.net_running = false;
                // The name's updates wait for the next look, after the rules saw these facts.
                self.net_fresh |= epoch == self.net_epoch;
                self.net = Some(net);
            }
            Internal::LocalTest(result) => {
                self.local_running = false;
                self.local_test = Some(result);
            }
            Internal::Hairpin(result) => {
                self.hairpin_running = false;
                self.hairpin = Some(result);
            }
            Internal::Applied { target, result } => {
                self.applying = false;
                match result {
                    Ok(()) => {
                        self.applied = Some(target);
                        self.endpoint_error = None;
                    }
                    Err(detail) => {
                        tracing::warn!(target: "zeal::public", %detail, "the light wallet endpoint was not applied");
                        if target.is_some() {
                            // Turning on failed fast: clear the port again, so no later writer
                            // adds the key, and wait for the user to try again.
                            self.endpoint_error = Some(detail);
                            let settings = self.settings.clone();
                            tokio::spawn(async move {
                                let _ = settings
                                    .update(|settings| {
                                        if let Some(spec) = settings.public.light_server_mut() {
                                            spec.backend_port = None;
                                        }
                                    })
                                    .await;
                            });
                        }
                        // Turning off is retried at the next look.
                    }
                }
            }
            Internal::Door(launcher_gateway::Event::Outside { at }) => {
                // The front door serves the spec's name.
                if let Some(name) = self.spec().and_then(|spec| spec.name) {
                    self.record(
                        name.as_str(),
                        Evidence {
                            source: EvidenceSource::OutsideTlsConnection,
                            at,
                        },
                    );
                }
            }
            Internal::Door(_) => {}
            Internal::IndexDisk { dir, read } => {
                self.zingod.disk_running = false;
                self.zingod.disk = Some((dir, read));
            }
            Internal::TipCheck(checked) => {
                self.zingod.tip_running = false;
                let Some((height, check)) = checked else {
                    return;
                };
                let now = SystemTime::now();
                self.zingod.tip =
                    Some(zingod_rules::keep_tip(self.zingod.tip.as_ref(), check, now));
                if self.zingod.cross.observe(check, now) && self.zingod.mismatch.is_none() {
                    tracing::warn!(target: "zeal::public", height, "zingod's chain doesn't match zebrad's; stopping zingod");
                    self.door_unavailable();
                    self.zingod.mismatch = Some(height);
                }
            }
        }
    }

    /// Records evidence for `name`.
    fn record(&mut self, name: &str, new: Evidence) {
        let proven = self.state.names.entry(name.to_string()).or_default();
        let before = proven.evidence.clone();
        evidence::record(&mut proven.evidence, new);
        // An outside connection is written down at most hourly.
        let worth_saving = before.iter().all(|old| old.source != new.source)
            || before.iter().any(|old| {
                old.source == new.source
                    && new.at.duration_since(old.at).unwrap_or_default() > Duration::from_secs(3600)
            });
        if worth_saving {
            self.save_state();
        }
    }

    /// The running front door's upload since it started.
    fn door_up(&self) -> Option<u64> {
        self.door
            .as_ref()
            .map(|door| door.handle.stats.borrow().bytes_up)
    }

    fn save_state(&mut self) {
        let mut state = self.state.clone();
        state.month_bytes_up += self.upload.this_run(self.door_up());
        let _ = store::save(&self.options.paths.state(), &state, false);
        self.state_saved = tokio::time::Instant::now();
    }

    fn roll_month(&mut self, now: SystemTime) {
        let month = month_of(now);
        if self.state.month != month {
            self.state.month = month;
            self.state.month_bytes_up = 0;
            self.upload.new_month(self.door_up());
            self.save_state();
        } else if self.state_saved.elapsed() >= SAVE_EVERY {
            self.save_state();
        }
    }

    /// Ends the front door and moves its upload into the month's total.
    fn stop_door(&mut self) {
        if let Some(door) = self.door.take() {
            self.upload
                .door_stopped(door.handle.stats.borrow().bytes_up);
            door.handle.stop();
        }
        self.can_validate
            .send_if_modified(|can| std::mem::replace(can, false));
    }

    fn stop_children(&mut self) {
        self.stop_door();
        self.ddns = None;
        self.acme = None;
        self.light = None;
        self.awake = None;
    }

    /// Closes the front door for Zeal's exit and saves the month's upload. zingod keeps running
    /// until [`Self::stop_indexer`], so its stop runs beside the nodes'.
    fn shutdown(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        self.stop_door();
        self.awake = None;
        self.state.month_bytes_up += std::mem::take(&mut self.upload).this_run(None);
        let _ = store::save(&self.options.paths.state(), &self.state, false);
    }

    /// Stops zingod, after the front door closed.
    async fn stop_indexer(&mut self) {
        self.zingod.plan.send_replace(None);
        if let Some(runner) = self.zingod.runner.take() {
            runner.handle.shutdown().await;
        }
    }

    /// Starts, stops and pokes the children for the current state.
    async fn drive(
        &mut self,
        spec: &PublicServiceSpec,
        facts: &Facts,
        steps: &[launcher_types::public::Step],
        state: &PublicState,
        mode: Option<GatewayMode>,
    ) {
        let current = rules::current_step(steps);
        let reached = |step: StepId| spec.opened || current.is_none_or(|current| current >= step);
        // The network checks and the name go on through a pause that ends by itself, so it can.
        let watching = rules::watches_surroundings(state);
        // A network check from before they stopped says nothing about where this computer is
        // now.
        if !watching {
            if self.was_watching {
                self.net_epoch += 1;
                self.net_due = None;
            }
            self.net_fresh = false;
        }
        self.was_watching = watching;

        // The light monitor follows the backend's endpoint: zebrad's, or zingod's once it
        // serves.
        let endpoint = match spec.backend {
            LightBackend::ZebradEndpoint => facts.endpoint,
            LightBackend::Zingod => zingod_rules::ready(spec, facts)
                .then(|| {
                    facts
                        .zingod
                        .as_ref()
                        .and_then(|zingod| zingod.runner.listening)
                })
                .flatten(),
        };
        self.endpoint
            .send_if_modified(|current| std::mem::replace(current, endpoint) != endpoint);
        if self
            .light
            .as_ref()
            .is_none_or(|light| light.key != spec.network.as_str())
        {
            let monitor = LightMonitor::spawn(
                spec.network,
                self.endpoint.subscribe(),
                self.stats_rx.clone(),
            );
            let forwards = vec![forward_changes(
                monitor.result.clone(),
                self.internal_tx.clone(),
            )];
            self.light = Some(Child {
                key: spec.network.as_str().to_string(),
                handle: monitor,
                forwards,
            });
        }

        // The read-only network checks, from the network step on.
        if watching
            && reached(StepId::Network)
            && !self.net_running
            && self
                .net_due
                .is_none_or(|due| tokio::time::Instant::now() >= due)
        {
            self.spawn_net_check();
        }
        if reached(StepId::Port) && self.firewall.is_none() {
            let tool = crate::firewall::detect(None);
            self.firewall = Some(firewall_info(tool, spec.internal_port));
        }

        // The name: no update until this run's network check found the network fit to expose.
        let wanted = Wanted {
            update: self.net_fresh
                && self
                    .net
                    .as_ref()
                    .is_some_and(|net| rules::may_point_name_here(spec, net)),
            address: self
                .net
                .as_ref()
                .filter(|net| net.wan == WanKind::Public)
                .and_then(|net| net.wan_addr),
        };
        self.ddns_wanted.send_if_modified(|current| {
            let changed = *current != wanted;
            *current = wanted;
            changed
        });
        match &spec.name {
            Some(name) if watching => {
                let key = format!("{name} {:?}", spec.provider);
                if self.ddns.as_ref().is_none_or(|ddns| ddns.key != key) {
                    let handle = DdnsHandle::spawn(DdnsConfig {
                        name: name.clone(),
                        provider: spec.provider,
                        api: self.options.duckdns_api.clone(),
                        fetch: self.fetch.clone(),
                        secrets: self.secrets.clone(),
                        wanted: self.ddns_wanted.subscribe(),
                    });
                    let forwards = vec![forward_changes(
                        handle.status.clone(),
                        self.internal_tx.clone(),
                    )];
                    self.ddns = Some(Child {
                        key,
                        handle,
                        forwards,
                    });
                }
            }
            _ => self.ddns = None,
        }

        // The certificate, from the certificate step on.
        match &spec.name {
            Some(name) if reached(StepId::Certificate) || self.acme.is_some() => {
                if self
                    .acme
                    .as_ref()
                    .is_none_or(|acme| acme.key != name.as_str())
                {
                    let config = AcmeConfig {
                        paths: self.options.paths.clone(),
                        name: name.to_string(),
                        staging: self.options.acme_staging.clone(),
                        production: self.options.acme_production.clone(),
                        server_root: self.options.acme_server_root.clone(),
                        extra_roots: self.options.extra_roots.clone(),
                        order_timeout: Duration::from_secs(120),
                    };
                    let handle = AcmeHandle::spawn(
                        config,
                        self.challenge.clone(),
                        self.can_validate.subscribe(),
                        self.state
                            .names
                            .get(name.as_str())
                            .and_then(|proven| proven.staging_validated_at),
                    );
                    let forwards = vec![
                        forward_changes(handle.status.clone(), self.internal_tx.clone()),
                        forward_changes(handle.validated.clone(), self.internal_tx.clone()),
                    ];
                    self.acme = Some(Child {
                        key: name.to_string(),
                        handle,
                        forwards,
                    });
                    self.acme_asked = false;
                }
            }
            _ => self.acme = None,
        }
        if let Some(acme) = &self.acme {
            if current == Some(StepId::Certificate) && !self.acme_asked {
                let _ = acme
                    .handle
                    .commands
                    .try_send(AcmeCmd::Order { asked: false });
                self.acme_asked = true;
            }
            let name = acme.key.clone();
            let validated = *acme.handle.validated.borrow();
            let staging = acme.handle.status.borrow().staging_validated_at;
            if let Some(at) = validated {
                self.record(
                    &name,
                    Evidence {
                        source: EvidenceSource::TlsAlpn01Validation,
                        at,
                    },
                );
            }
            let kept = self
                .state
                .names
                .get(&name)
                .and_then(|proven| proven.staging_validated_at);
            if staging.is_some() && kept != staging {
                self.state
                    .names
                    .entry(name)
                    .or_default()
                    .staging_validated_at = staging;
                self.save_state();
            }
        }

        // The front door, in the mode the rules give, before zingod's plan changes: it goes
        // `Unavailable` before Zeal stops zingod.
        let upstream = match spec.backend {
            LightBackend::ZebradEndpoint => spec
                .backend_port
                .map(|port| SocketAddr::from((Ipv4Addr::LOCALHOST, port))),
            LightBackend::Zingod => zingod_rules::endpoint(spec, facts),
        };
        match (mode, upstream, &spec.name) {
            (Some(mode), Some(upstream), Some(name)) => {
                self.mode.send_if_modified(|current| {
                    let next = door_mode(mode);
                    std::mem::replace(current, next) != next
                });
                let key = format!("{upstream} {name} {}", spec.internal_port);
                if self.door.as_ref().is_none_or(|door| door.key != key) {
                    self.stop_door();
                    self.start_door(spec, name, upstream, facts, key).await;
                }
            }
            _ => self.stop_door(),
        }

        self.drive_zingod(spec, facts);

        // The test on this computer, once there is a certificate and the front door serves
        // this computer, and with zingod once it serves.
        let cert_valid = facts
            .cert
            .as_ref()
            .and_then(|cert| cert.not_after)
            .is_some_and(|not_after| not_after > facts.now);
        let backend_serves =
            spec.backend == LightBackend::ZebradEndpoint || zingod_rules::ready(spec, facts);
        if current == Some(StepId::LocalTest)
            && cert_valid
            && backend_serves
            && mode == Some(GatewayMode::LocalOnly)
            && self.local_test.is_none()
            && !self.local_running
        {
            self.spawn_local_test(spec, facts);
        }

        // The hairpin test, once open.
        let wan = facts
            .net
            .as_ref()
            .filter(|net| net.wan == WanKind::Public)
            .and_then(|net| net.wan_addr);
        if spec.opened
            && matches!(state, PublicState::Serving)
            && self.hairpin.is_none()
            && !self.hairpin_running
            && let (Some(wan), Some(name)) = (wan, &spec.name)
        {
            self.hairpin_running = true;
            let (port, name, roots, tx) = (
                spec.external_port,
                name.to_string(),
                self.options.extra_roots.clone(),
                self.internal_tx.clone(),
            );
            tokio::spawn(async move {
                let result = selftest::hairpin(wan, port, &name, &roots).await;
                let _ = tx
                    .send(Internal::Hairpin(Checked {
                        at: SystemTime::now(),
                        result,
                    }))
                    .await;
            });
        }

        // Keep this computer awake while serving.
        let serving = matches!(state, PublicState::Serving | PublicState::Degraded { .. });
        if serving && spec.keep_awake {
            if self.awake.as_mut().is_none_or(|awake| !awake.held()) {
                self.awake = KeepAwake::hold();
            }
        } else {
            self.awake = None;
        }
    }

    /// zingod's runner and plan, the tip cross-check and the actor's reading of the index
    /// folder, for the current facts.
    fn drive_zingod(&mut self, spec: &PublicServiceSpec, facts: &Facts) {
        let Some(zingod) = &facts.zingod else {
            self.zingod.plan.send_replace(None);
            self.zingod.runner = None;
            return;
        };
        // The index folder's disk, for the setup's room and an index zingod doesn't run on.
        let now = tokio::time::Instant::now();
        if !self.zingod.disk_running
            && self.zingod.disk_due.is_none_or(|due| now >= due)
            && (spec.backend == LightBackend::Zingod || spec.zingod.is_some())
        {
            self.zingod.disk_running = true;
            self.zingod.disk_due = Some(now + self.options.index_disk_every);
            let (dir, tx) = (zingod.dir.clone(), self.internal_tx.clone());
            let reader = self.options.read_index_disk;
            tokio::spawn(async move {
                let read = {
                    let dir = dir.clone();
                    tokio::task::spawn_blocking(move || reader(&dir))
                        .await
                        .unwrap_or_default()
                };
                let _ = tx.send(Internal::IndexDisk { dir, read }).await;
            });
        }
        // The runner lives while zingod has a recorded folder here, so a delete after
        // switching back finds it stopped.
        let Some(binary) = self.options.zingod_binary.clone() else {
            self.zingod.plan.send_replace(None);
            self.zingod.runner = None;
            return;
        };
        let Some(recorded) = spec.zingod.clone() else {
            self.zingod.plan.send_replace(None);
            self.zingod.runner = None;
            return;
        };
        let key = format!("{} {}", spec.network.as_str(), recorded.dir.display());
        if self
            .zingod
            .runner
            .as_ref()
            .is_none_or(|runner| runner.key != key)
        {
            self.zingod.plan.send_replace(None);
            let mut options = RunnerOptions::new(
                binary,
                &self.options.launcher,
                ZingodNetwork::of(spec.network),
            );
            if let Some(tune) = self.options.tune_zingod {
                tune(&mut options);
            }
            let runner = ZingodRunner::spawn(options, self.zingod.plan.subscribe());
            let forwards = vec![forward_changes(runner.facts(), self.internal_tx.clone())];
            self.zingod.runner = Some(Child {
                key,
                handle: runner,
                forwards,
            });
        }
        // The plan, while the conditions outside the runner hold.
        let start = zingod_rules::start_facts(spec, facts);
        let rpc = self.status_rx.borrow().rpc_addr;
        let plan = match (zingod_rules::plan_run(&start), rpc) {
            (Ok(run), Some(validator)) => Some(ZingodPlan {
                config: launcher_core::zingod::ZingodConfig {
                    network: ZingodNetwork::of(spec.network),
                    validator,
                    cookie: self
                        .node_rx
                        .borrow()
                        .paths(&self.options.launcher)
                        .rpc_cookie_file(),
                    listen: SocketAddr::from((Ipv4Addr::LOCALHOST, recorded.port)),
                    index_dir: recorded.dir.clone(),
                },
                zebrad: run,
                attempt: self.zingod.attempt,
            }),
            _ => None,
        };
        if plan.is_none() && self.zingod.plan.borrow().is_some() {
            self.door_unavailable();
        }
        self.zingod.plan.send_if_modified(|current| {
            let changed = *current != plan;
            *current = plan;
            changed
        });
        // The tip cross-check, at most once a minute while zingod serves.
        if zingod_rules::ready(spec, facts)
            && !self.zingod.tip_running
            && self.zingod.tip_due.is_none_or(|due| now >= due)
            && let (Some(listening), Some(rpc)) = (zingod.runner.listening, rpc)
        {
            self.zingod.tip_running = true;
            self.zingod.tip_due = Some(now + self.options.cross_check_every);
            let cookie = self
                .node_rx
                .borrow()
                .paths(&self.options.launcher)
                .rpc_cookie_file();
            let (height, tx) = (facts.height, self.internal_tx.clone());
            tokio::spawn(async move {
                let checked = cross_check(listening, rpc, cookie, height).await;
                let _ = tx.send(Internal::TipCheck(checked)).await;
            });
        }
    }

    async fn start_door(
        &mut self,
        spec: &PublicServiceSpec,
        name: &PublicHostname,
        upstream: SocketAddr,
        facts: &Facts,
        key: String,
    ) {
        let not_outside: Vec<IpAddr> = facts
            .net
            .as_ref()
            .and_then(|net| net.wan_addr)
            .into_iter()
            .collect();
        let config = launcher_gateway::Config {
            listen: SocketAddr::from((self.options.listen_ip, spec.internal_port)),
            upstream,
            name: name.to_string(),
            limits: door_limits(&spec.limits),
            timers: launcher_gateway::Timers::default(),
            not_outside,
        };
        let cert = self
            .acme
            .as_ref()
            .map(|acme| acme.handle.cert.clone())
            .unwrap_or_else(|| watch::channel(None).1);
        let mut started = FrontDoor::start(
            config.clone(),
            cert.clone(),
            self.challenge.subscribe(),
            self.mode.subscribe(),
        )
        .await;
        // A system without IPv6 cannot bind `[::]`: IPv4 alone, then.
        if matches!(started, Err(launcher_gateway::GatewayError::Listen { .. }))
            && config.listen.ip().is_ipv6()
            && config.listen.ip().is_unspecified()
        {
            let ipv4 = launcher_gateway::Config {
                listen: SocketAddr::from((Ipv4Addr::UNSPECIFIED, config.listen.port())),
                ..config
            };
            started = FrontDoor::start(
                ipv4,
                cert,
                self.challenge.subscribe(),
                self.mode.subscribe(),
            )
            .await;
        }
        match started {
            Ok(mut door) => {
                self.port_free = Some(true);
                self.can_validate.send_replace(true);
                let mut events = std::mem::replace(&mut door.events, mpsc::channel(1).1);
                let tx = self.internal_tx.clone();
                let forward_events = tokio::spawn(async move {
                    while let Some(event) = events.recv().await {
                        let _ = tx.try_send(Internal::Door(event));
                    }
                })
                .abort_handle();
                let forwards = vec![
                    forward_changes(door.stats.clone(), self.internal_tx.clone()),
                    forward_events,
                ];
                self.door = Some(Child {
                    key,
                    handle: door,
                    forwards,
                });
            }
            Err(launcher_gateway::GatewayError::PortInUse(_)) => self.port_free = Some(false),
            Err(err) => {
                tracing::warn!(target: "zeal::public", %err, "the front door did not start");
                self.port_free = Some(false);
            }
        }
    }

    fn spawn_net_check(&mut self) {
        self.net_running = true;
        self.net_due = Some(tokio::time::Instant::now() + NET_EVERY);
        let tx = self.internal_tx.clone();
        let epoch = self.net_epoch;
        tokio::spawn(async move {
            let net = netcheck::check(SystemTime::now()).await;
            let _ = tx.send(Internal::Net { net, epoch }).await;
        });
    }

    fn spawn_local_test(&mut self, spec: &PublicServiceSpec, facts: &Facts) {
        let Some(name) = spec.name.clone() else {
            return;
        };
        self.local_running = true;
        let (port, network, height, roots, tx) = (
            spec.internal_port,
            spec.network,
            facts.height,
            self.options.extra_roots.clone(),
            self.internal_tx.clone(),
        );
        tokio::spawn(async move {
            let result = selftest::local_test(port, name.as_str(), &roots, network, height).await;
            let _ = tx
                .send(Internal::LocalTest(Checked {
                    at: SystemTime::now(),
                    result,
                }))
                .await;
        });
    }

    /// Forgets what depended on the name.
    fn name_changed(&mut self) {
        self.local_test = None;
        self.hairpin = None;
        self.acme_asked = false;
        self.name_elsewhere_since = None;
    }

    async fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::BeginSetup { network, reply } => {
                let result = self.begin_setup(network).await;
                let _ = reply.send(result);
            }
            Msg::Switch { on, reply } => {
                if !on {
                    // Less exposure acts at once: the front door closes before anything else,
                    // and stays closed even if the off cannot be saved.
                    self.held = Some(launcher_types::public::PauseReason::ByYou);
                    self.stop_door();
                    self.awake = None;
                }
                self.endpoint_error = None;
                let result = self.update_spec(move |spec| spec.switch_on = on).await;
                if on && result.is_ok() {
                    self.held = None;
                }
                let _ = reply.send(result);
            }
            Msg::Answer { answer, reply } => {
                let result = self.answer(answer).await;
                let _ = reply.send(result);
            }
            Msg::RunChecks { step, reply } => {
                self.run_checks(step).await;
                let _ = reply.send(Ok(()));
            }
            Msg::Open { reply } => {
                let result = self.open().await;
                let _ = reply.send(result);
            }
            Msg::Forget {
                remove_token,
                remove_index,
                reply,
            } => {
                let result = self.forget(remove_token, remove_index).await;
                let _ = reply.send(result);
            }
            Msg::CheckConnection { reply } => {
                let report = self.check_connection().await;
                let _ = reply.send(report);
            }
            Msg::Rebuild { reply } => {
                let result = self.remove_zingod_index(true).await;
                let _ = reply.send(result);
            }
            Msg::DeleteIndex { reply } => {
                let result = self.remove_zingod_index(false).await;
                let _ = reply.send(result);
            }
            Msg::Diagnostics { reply } => {
                let text = self.indexer_diagnostics().await;
                let _ = reply.send(text);
            }
            Msg::Shutdown { .. } | Msg::StopIndexer { .. } => unreachable!("handled by the loop"),
        }
    }

    async fn begin_setup(&mut self, network: Network) -> Result<NodeId, String> {
        if let Some(spec) = self.spec() {
            return Ok(spec.node);
        }
        if !self.options.supported {
            return Err("public services run on Linux only for now".into());
        }
        if network != self.network() {
            return Err(format!(
                "Zeal runs zebrad on the other network; switch to {network} first"
            ));
        }
        let node = self.node_rx.borrow().id.clone();
        let spec = PublicServiceSpec::new_light_server(node.clone(), network);
        self.settings
            .update(move |settings| settings.public.services.push(spec))
            .await
            .map_err(|err| err.to_string())?;
        tracing::info!(target: "zeal::public", %network, "light client server set up");
        Ok(node)
    }

    async fn answer(&mut self, answer: StepAnswer) -> Result<(), String> {
        let spec = self.spec().ok_or("no light client server is set up")?;
        match answer {
            StepAnswer::Consent { version } => {
                if version != CONSENT_VERSION {
                    return Err("the consent text changed; read it again".into());
                }
                let hostname = spec.name.map(|name| name.to_string()).unwrap_or_default();
                let at = SystemTime::now();
                self.update_spec(move |spec| {
                    spec.consent = Some(Consent {
                        version,
                        at,
                        hostname,
                        backend: spec.backend,
                    })
                })
                .await
            }
            StepAnswer::Name { name, provider } => {
                let name = parse_name(&name, provider).map_err(|err| err.to_string())?;
                // The token is kept for DuckDNS's zone, so it serves the new name too.
                let secret = match &spec.name {
                    Some(old) if *old != name => {
                        let secret = self.move_legacy_token(old, spec.secret.clone()).await?;
                        // What was proven for the old name is not the new one's.
                        if self.state.names.remove(old.as_str()).is_some() {
                            self.save_state();
                        }
                        secret
                    }
                    _ => spec.secret.clone(),
                };
                self.name_changed();
                self.update_spec(move |spec| {
                    // Consent given before a name binds to the first one; another name asks again.
                    if let Some(consent) = &mut spec.consent
                        && consent.hostname.is_empty()
                    {
                        consent.hostname = name.to_string();
                    }
                    spec.name = Some(name);
                    spec.provider = provider;
                    spec.secret = secret;
                })
                .await
            }
            StepAnswer::Token { token } => {
                let name = spec.name.ok_or("choose the name first")?;
                if spec.provider != DnsProvider::DuckDns {
                    return Err("a name kept by hand needs no token".into());
                }
                if !plausible_duckdns_token(&token) {
                    return Err(
                        "a DuckDNS token looks like a7c4d0ad-114e-40ef-ba1d-d217904a50f2".into(),
                    );
                }
                match self
                    .secrets
                    .set(DUCKDNS_ACCOUNT.into(), token.trim().to_string())
                    .await
                {
                    Ok(()) => {
                        self.keychain_unavailable = false;
                        // An older Zeal's entry for this name would only be left behind.
                        let _ = self.secrets.delete(legacy_duckdns_account(&name)).await;
                        self.update_spec(move |spec| spec.secret = Some(duckdns_secret()))
                            .await?;
                        if let Some(ddns) = &self.ddns {
                            let _ = ddns.handle.commands.try_send(DdnsCmd::Now);
                        }
                        Ok(())
                    }
                    Err(err) => {
                        self.keychain_unavailable = true;
                        Err(err.to_string())
                    }
                }
            }
            StepAnswer::LanReserved => {
                let lan = self
                    .net
                    .as_ref()
                    .and_then(|net| net.lan_addr)
                    .ok_or("this computer's LAN address is not known yet")?;
                self.update_spec(move |spec| spec.lan_addr = Some(lan))
                    .await
            }
            StepAnswer::ForwardedByHand => {
                self.acme_asked = false;
                self.update_spec(|spec| spec.forwarded = true).await
            }
            StepAnswer::FirewallDone => self.update_spec(|spec| spec.firewall_done = true).await,
            StepAnswer::KeepAwake { on } => {
                self.update_spec(move |spec| spec.keep_awake = on).await
            }
            StepAnswer::ResumeOnLaunch { on } => {
                self.update_spec(move |spec| spec.resume_on_launch = on)
                    .await
            }
            StepAnswer::Backend { backend } => self.choose_backend(&spec, backend).await,
            StepAnswer::Upload { preset } => {
                let kbit = preset.kbit();
                if let Some(door) = &self.door {
                    door.handle.limits(launcher_gateway::Limits {
                        upload_kbit: kbit,
                        ..door_limits(&spec.limits)
                    });
                }
                self.update_spec(move |spec| spec.limits.upload_kbit = kbit)
                    .await
            }
        }
    }

    async fn run_checks(&mut self, step: StepId) {
        match step {
            StepId::Service => {
                self.endpoint_error = None;
                // zingod's Try again: a new attempt starts it again after a failure that waits
                // for the user, or restarts one that is stuck. A zingod that runs well keeps
                // running.
                let runner = self.zingod.facts();
                let waits = !runner.phase.is_active()
                    || runner.stale.is_some()
                    || self.zingod.stuck.is_some();
                if waits
                    && self
                        .spec()
                        .is_some_and(|spec| spec.backend == LightBackend::Zingod)
                {
                    self.restart_zingod();
                    self.zingod.no_validator_restart = None;
                }
                // zebrad could not bind the endpoint's port: the next look picks a free one.
                let taken = {
                    let status = self.status_rx.borrow();
                    status.phase
                        == (Phase::Failed {
                            class: ExitClass::PortInUse,
                        })
                        && status
                            .last_exit
                            .as_ref()
                            .and_then(|exit| exit.detail.as_deref())
                            .is_some_and(|detail| detail.contains("lightwalletd"))
                };
                if taken {
                    let _ = self.update_spec(|spec| spec.backend_port = None).await;
                }
                if let Some(light) = &self.light {
                    let _ = light.handle.now.try_send(());
                }
            }
            StepId::Network | StepId::Port | StepId::Firewall => {
                self.net_due = None;
                self.firewall = None;
                if step == StepId::Port && self.port_free == Some(false) {
                    self.port_free = None;
                    self.stop_door();
                }
            }
            StepId::Name => {
                if let Some(ddns) = &self.ddns {
                    let _ = ddns.handle.commands.try_send(DdnsCmd::Now);
                }
            }
            StepId::Certificate => {
                if let Some(acme) = &self.acme {
                    let _ = acme
                        .handle
                        .commands
                        .try_send(AcmeCmd::Order { asked: true });
                }
            }
            StepId::LocalTest => self.local_test = None,
            StepId::Open => self.hairpin = None,
            StepId::Consent | StepId::KeepRunning | StepId::Share => {}
        }
    }

    async fn open(&mut self) -> Result<(), OpenError> {
        let refuse = |message: &str| OpenError {
            check: None,
            message: message.to_string(),
        };
        let spec = self
            .spec()
            .ok_or_else(|| refuse("no light client server is set up"))?;
        let mut facts = self.facts(&spec, SystemTime::now());
        facts.at_tip_since = self.at_tip_since;
        rules::open_gates(&spec, &facts)?;
        self.update_spec(|spec| spec.opened = true)
            .await
            .map_err(|err| refuse(&err))?;
        self.hairpin = None;
        tracing::info!(target: "zeal::public", "light client server opened to the internet");
        Ok(())
    }

    async fn forget(
        &mut self,
        remove_token: bool,
        remove_index: bool,
    ) -> Result<StillPublic, String> {
        let spec = self.spec().ok_or("no light client server is set up")?;
        // Closed until the service is gone, even if a write below fails.
        self.held = Some(launcher_types::public::PauseReason::ByYou);
        self.stop_children();
        // zingod stops once the front door is closed.
        self.zingod.plan.send_replace(None);
        // The switch-off is saved before anything else can fail, so the service can't resume
        // at the next launch.
        self.update_spec(|spec| {
            spec.switch_on = false;
            spec.backend_port = None;
        })
        .await?;
        // The index goes once zingod has stopped. What can't be removed is reported, not a
        // reason to keep the service.
        let mut index_left = None;
        if remove_index
            && let Some(recorded) = &spec.zingod
            && let Err(err) = self.delete_index_in(&recorded.dir).await
        {
            tracing::warn!(target: "zeal::public", %err, "zingod's index was not removed");
            index_left = Some(err);
        }
        if let Some(runner) = self.zingod.runner.take() {
            runner.handle.shutdown().await;
        }
        self.zingod.forget_history();
        self.zingod.disk = None;
        // The endpoint must be gone before the spec is: once it is, the key is the user's.
        let node = self.node_rx.borrow().id.clone();
        self.host
            .apply(node, None, spec.network == self.network())
            .await?;
        self.applied = Some(None);
        // Every entry the token may be under: the zone's, the spec's, and an older Zeal's for
        // the name, whatever the provider is now. A missing entry is no failure.
        let mut token_left = false;
        if remove_token {
            let mut accounts = vec![DUCKDNS_ACCOUNT.to_string()];
            accounts.extend(spec.secret.as_ref().map(|secret| secret.account.clone()));
            accounts.extend(spec.name.as_ref().map(legacy_duckdns_account));
            accounts.sort();
            accounts.dedup();
            for account in accounts {
                if let Err(err) = self.secrets.delete(account).await {
                    tracing::warn!(target: "zeal::public", %err, "the DuckDNS token was not deleted");
                    token_left = true;
                }
            }
        }
        self.settings
            .update(|settings| {
                settings
                    .public
                    .services
                    .retain(|spec| spec.service != PublicService::LightServer)
            })
            .await
            .map_err(|err| err.to_string())?;
        self.held = None;
        // A new setup proves itself again, whatever its name.
        self.state.names.clear();
        self.save_state();
        self.name_changed();
        self.net = None;
        self.port_free = None;
        let firewall = firewall_info(crate::firewall::detect(None), spec.internal_port);
        tracing::info!(target: "zeal::public", "light client server forgotten");
        Ok(StillPublic {
            forward: spec.forwarded.then_some((
                spec.external_port,
                spec.lan_addr,
                spec.internal_port,
            )),
            firewall_remove: if spec.firewall_done {
                firewall.remove
            } else {
                Vec::new()
            },
            name: spec.name,
            token_left,
            index_left,
        })
    }

    /// Chooses the server that answers wallets. zingod must be one Zeal offers here; the first
    /// choice records its port, the next free one from 8137 or 18137, and its folder beside the
    /// serving node's data, so a later data-folder change doesn't orphan the index. The consent
    /// covers one backend, so the other one asks for it again.
    async fn choose_backend(
        &mut self,
        spec: &PublicServiceSpec,
        backend: LightBackend,
    ) -> Result<(), String> {
        if backend == spec.backend {
            return Ok(());
        }
        let mut recorded = spec.zingod.clone();
        if backend == LightBackend::Zingod {
            let facts = self.facts(spec, SystemTime::now());
            match zingod_rules::choice(&facts, spec.network) {
                ZingodChoice::Offered => {}
                ZingodChoice::NotBundled => {
                    return Err("zingod runs on Linux only for now".into());
                }
                ZingodChoice::NotOnNetwork { network } => {
                    return Err(format!(
                        "zingod is being tested on Testnet first, not {network}"
                    ));
                }
                ZingodChoice::Untested {
                    version,
                    zebrad,
                    network,
                } => {
                    return Err(format!(
                        "zingod {version} isn't tested with Zebra {zebrad} on {network} yet"
                    ));
                }
            }
            if recorded.is_none() {
                let first = launcher_types::public::zingod_port(spec.network);
                let port = (first..=first.saturating_add(100))
                    .find(|port| {
                        !launcher_core::supervisor::addr_in_use(SocketAddr::from((
                            Ipv4Addr::LOCALHOST,
                            *port,
                        )))
                    })
                    .ok_or("no free port for zingod")?;
                recorded = Some(ZingodSpec {
                    port,
                    dir: zingod_rules::index_dir(&self.node_data(), spec.network),
                });
            }
        }
        self.door_unavailable();
        self.zingod.forget_history();
        self.local_test = None;
        tracing::info!(target: "zeal::public", ?backend, "the light client server's backend changes");
        self.update_spec(move |spec| {
            spec.backend = backend;
            spec.zingod = recorded;
        })
        .await
    }

    /// Rebuild (`restart`) or Delete: zingod stops, and its index and state go from the recorded
    /// folder; a rebuild puts the front door in `Unavailable` first, and starts zingod again
    /// under a new attempt. Delete comes after switching back to zebrad's endpoint, which the
    /// front door serves meanwhile: its mode stays the rules'.
    async fn remove_zingod_index(&mut self, restart: bool) -> Result<(), String> {
        let spec = self.spec().ok_or("no light client server is set up")?;
        let recorded = spec.zingod.ok_or("zingod has no index here")?;
        if restart && spec.backend != LightBackend::Zingod {
            return Err("zingod isn't the light client server's backend".into());
        }
        if !restart && spec.backend == LightBackend::Zingod {
            return Err("zingod serves the light client server: switch back to Zebra's built-in server first".into());
        }
        if restart {
            self.door_unavailable();
        }
        let removed = self.delete_index_in(&recorded.dir).await;
        self.zingod.forget_history();
        self.zingod.disk_due = None;
        // The runner holds zingod stopped after its removal until the plan changes: a rebuild's
        // new attempt is that change, whether or not the removal went through, so a half
        // removed index shows as zingod's own failure rather than a start that never comes.
        if restart {
            self.zingod.attempt += 1;
        }
        removed?;
        if restart {
            tracing::info!(target: "zeal::public", "zingod's index is rebuilt");
        } else {
            tracing::info!(target: "zeal::public", "zingod's index is deleted");
        }
        Ok(())
    }

    /// Stops zingod and removes the index and state in `dir`, the recorded folder: the
    /// runner's removal, which waits for zingod's stop, then the folder's again, for an index
    /// the runner never ran on in this run.
    async fn delete_index_in(&mut self, dir: &std::path::Path) -> Result<(), String> {
        if let Some(runner) = &self.zingod.runner {
            runner.handle.delete().await?;
        }
        let dir = dir.to_path_buf();
        tokio::task::spawn_blocking(move || remove_index(&dir))
            .await
            .map_err(|err| err.to_string())?
            .map_err(|err| err.to_string())
    }

    /// zingod's part of the diagnostics: its supervisor's phase and its last `/status`, the
    /// paths of its files (the cookie is a path) and its last log lines, which the reader
    /// redacted before they reached the ring.
    async fn indexer_diagnostics(&mut self) -> Option<String> {
        let spec = self.spec()?;
        let recorded = spec.zingod.clone()?;
        let runner = self.zingod.facts();
        let mut text = String::new();
        text.push_str(&format!("backend: {:?}\n", spec.backend));
        text.push_str(&format!("index folder: {}\n", recorded.dir.display()));
        text.push_str(&format!("port: {}\n", recorded.port));
        let options = RunnerOptions::new(
            self.options.zingod_binary.clone().unwrap_or_default(),
            &self.options.launcher,
            ZingodNetwork::of(spec.network),
        );
        text.push_str(&format!("binary: {}\n", options.binary.display()));
        text.push_str(&format!(
            "configuration: {}\n",
            options.config_file.display()
        ));
        text.push_str(&format!("log: {}\n", options.log.path.display()));
        text.push_str(&format!(
            "cookie: {}\n",
            self.node_rx
                .borrow()
                .paths(&self.options.launcher)
                .rpc_cookie_file()
                .display()
        ));
        text.push_str(&format!("phase: {:?}\n", runner.phase));
        text.push_str(&format!(
            "status: {}\n",
            serde_json::to_string_pretty(&runner.status).unwrap_or_default()
        ));
        let lines = match &self.zingod.runner {
            Some(runner) => runner.handle.subscribe().await,
            None => None,
        };
        text.push_str(&format!("last {DIAGNOSTIC_LINES} lines:\n"));
        if let Some(lines) = lines {
            let skip = lines.history.len().saturating_sub(DIAGNOSTIC_LINES);
            for line in lines.history.iter().skip(skip) {
                let level = line
                    .level
                    .map(|level| format!("{level:?} "))
                    .unwrap_or_default();
                let target = line
                    .target
                    .as_deref()
                    .map(|target| format!("{target}: "))
                    .unwrap_or_default();
                text.push_str(&format!(
                    "{} {level}{target}{}\n",
                    line.timestamp.as_deref().unwrap_or("-"),
                    line.message
                ));
            }
        }
        Some(text)
    }

    /// Moves a token an older Zeal kept for the name `old` to the zone's entry, before the name
    /// or provider changes. Returns where the token is kept now, or why the name must stay:
    /// the token could not move, and an entry for a name Zeal no longer has would be lost to
    /// the name's updates and to "Turn off and forget". With the name kept, both still find it,
    /// and the next change tries again.
    async fn move_legacy_token(
        &self,
        old: &PublicHostname,
        secret: Option<SecretRef>,
    ) -> Result<Option<SecretRef>, String> {
        let refused = || {
            "the keychain did not move the DuckDNS token kept for the old name; try again"
                .to_string()
        };
        let legacy = legacy_duckdns_account(old);
        match self.secrets.get(legacy.clone()).await {
            Ok(token) => {
                let kept = self.secrets.get(DUCKDNS_ACCOUNT.into()).await.is_ok()
                    || self
                        .secrets
                        .set(DUCKDNS_ACCOUNT.into(), token)
                        .await
                        .is_ok();
                if kept && self.secrets.delete(legacy).await.is_ok() {
                    Ok(Some(duckdns_secret()))
                } else {
                    Err(refused())
                }
            }
            Err(SecretError::Missing) => Ok(secret.map(|_| duckdns_secret())),
            // The keychain did not answer, and the spec keeps the token under the old name.
            Err(_)
                if secret
                    .as_ref()
                    .is_some_and(|secret| secret.account != DUCKDNS_ACCOUNT) =>
            {
                Err(refused())
            }
            // Nothing says a token is under the old name: a name kept by hand, or no token.
            Err(_) => Ok(secret),
        }
    }

    async fn check_connection(&mut self) -> ConnectionReport {
        let net = netcheck::check(SystemTime::now()).await;
        let firewall = firewall_info(
            crate::firewall::detect(None),
            launcher_types::public::FRONT_DOOR_PORT,
        );
        self.net = Some(net.clone());
        let mut spec = PublicServiceSpec::new_light_server(
            NodeId::new("zebrad-mainnet").expect("a valid id"),
            Network::Mainnet,
        );
        // The reservation is the user's answer, not a fact of the network.
        spec.lan_addr = net.lan_addr;
        let mut facts = Facts::new(SystemTime::now(), Network::Mainnet);
        facts.net = Some(net.clone());
        let checks = rules::plan(&spec, &facts)
            .into_iter()
            .find(|step| step.id == StepId::Network)
            .map(|step| {
                step.checks
                    .into_iter()
                    .filter(|check| check.id != CheckId::LanReserved)
                    .collect()
            })
            .unwrap_or_default();
        ConnectionReport {
            net,
            checks,
            firewall,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_without_resume_drops_the_endpoint() {
        let mut settings = Settings::default();
        assert!(!prepare_launch(&mut settings));
        let mut spec = PublicServiceSpec::new_light_server(
            NodeId::new("zebrad-mainnet").unwrap(),
            Network::Mainnet,
        );
        spec.backend_port = Some(9067);
        spec.opened = true;
        spec.consent = Some(launcher_types::public::Consent {
            version: CONSENT_VERSION,
            at: SystemTime::UNIX_EPOCH,
            hostname: String::new(),
            backend: Default::default(),
        });
        settings.public.services.push(spec);
        // Resumes: the key stays, so zebrad's first start opens the endpoint.
        assert!(!prepare_launch(&mut settings));
        assert_eq!(
            settings.public.light_server().unwrap().backend_port,
            Some(9067)
        );
        // Does not resume: held for the switch, and the key goes before zebrad starts.
        settings.public.light_server_mut().unwrap().resume_on_launch = false;
        assert!(prepare_launch(&mut settings));
        assert_eq!(settings.public.light_server().unwrap().backend_port, None);
        // Switched off: nothing is held, and the key is gone.
        let spec = settings.public.light_server_mut().unwrap();
        spec.switch_on = false;
        spec.backend_port = Some(9067);
        assert!(!prepare_launch(&mut settings));
        assert_eq!(settings.public.light_server().unwrap().backend_port, None);
        // A consent this release does not cover: the key goes before zebrad starts.
        let spec = settings.public.light_server_mut().unwrap();
        spec.switch_on = true;
        spec.resume_on_launch = true;
        spec.backend_port = Some(9067);
        spec.consent = Some(launcher_types::public::Consent {
            version: CONSENT_VERSION - 1,
            at: SystemTime::UNIX_EPOCH,
            hostname: String::new(),
            backend: Default::default(),
        });
        assert!(!prepare_launch(&mut settings));
        assert_eq!(settings.public.light_server().unwrap().backend_port, None);
    }

    #[test]
    fn a_new_month_starts_at_zero_with_a_front_door_running() {
        let mut upload = Upload::default();
        // A front door that started last month has sent 250 so far.
        assert_eq!(upload.this_run(Some(250)), 250);
        upload.new_month(Some(250));
        assert_eq!(upload.this_run(Some(250)), 0);
        assert_eq!(upload.this_run(Some(260)), 10);
        upload.door_stopped(260);
        assert_eq!(upload.this_run(None), 10);
        // The next front door counts from zero.
        assert_eq!(upload.this_run(Some(5)), 15);
        upload.new_month(None);
        assert_eq!(upload.this_run(Some(7)), 7);
    }

    #[test]
    fn months_are_utc_year_and_month() {
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_793_700_000);
        assert_eq!(month_of(at), "2026-11");
    }
}
