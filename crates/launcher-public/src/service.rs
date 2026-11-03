//! `PublicServices`: the one actor that owns the light client server. It reads the stored
//! spec, gathers the facts (zebrad's status and statistics, the light monitor, the network
//! checks, the name, the certificate, the front door), runs `launcher-herd`'s rules over them,
//! carries out what they decide and publishes the status on a `watch`.
//!
//! It never writes `zebrad.toml` or restarts zebrad itself: it sets or clears the spec's
//! `backend_port` through the settings actor, the only writer of `launcher.json`, and asks the
//! [`EndpointHost`] (the app) to apply the managed keys behind the app's permits.

use std::{
    future::Future,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    pin::Pin,
    sync::Arc,
    time::{Duration, SystemTime},
};

use launcher_core::settings::SettingsHandle;
use launcher_herd::public::{
    self as rules, Checked, Facts, evidence,
    firewall::firewall_info,
    light::{at_tip, pick_endpoint_port, track_since, wants_endpoint},
    names::{keychain_account, parse_name, plausible_duckdns_token},
};
use launcher_types::{
    ExitClass, Network, NodeId, NodeStats, Phase, Settings, Status,
    public::{
        CONSENT_VERSION, CheckId, ConnectionReport, Consent, DnsProvider, Evidence, EvidenceSource,
        FirewallInfo, GatewayMode, GatewayStats, LightdInfo, LocalTestProblem, NetFacts, OpenError,
        PublicHostname, PublicService, PublicServiceSpec, PublicState, PublicStatus, StepAnswer,
        StepId, StillPublic, WanKind,
    },
};
use tokio::sync::{mpsc, oneshot, watch};

use crate::{
    acme::{AcmeCmd, AcmeConfig, AcmeHandle, LETS_ENCRYPT_PRODUCTION, LETS_ENCRYPT_STAGING},
    awake::KeepAwake,
    ddns::{DUCKDNS_API, DdnsCmd, DdnsConfig, DdnsHandle, Fetch},
    gateway::FrontDoor,
    light::LightMonitor,
    netcheck,
    secrets::SecretStore,
    selftest,
    store::{self, PublicPaths, State},
};

/// How often the actor looks again without a change.
const TICK: Duration = Duration::from_secs(10);

/// How often the network checks run while they matter.
const NET_EVERY: Duration = Duration::from_secs(5 * 60);

/// How often the month's upload is written down.
const SAVE_EVERY: Duration = Duration::from_secs(10 * 60);

/// What the app does for the actor: write the managed keys from the stored settings and, with
/// `restart`, restart zebrad if it runs, behind the app's permits. Without `restart` the file
/// changes for zebrad's next start only, as a network change does.
pub trait EndpointHost: Send + Sync + 'static {
    fn apply(&self, restart: bool) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;
}

/// Where the actor gets certificates and names, and what it may do here.
#[derive(Clone, Debug)]
pub struct Options {
    pub paths: PublicPaths,
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
    /// Let's Encrypt and DuckDNS, with the public services' files under `data_dir`.
    pub fn new(data_dir: &std::path::Path, bundled_version: &str) -> Self {
        Options {
            paths: PublicPaths::new(data_dir),
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
    pub status: watch::Receiver<Status>,
    pub stats: watch::Receiver<Option<NodeStats>>,
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
    if !spec.switch_on || held {
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
        reply: Reply<Result<StillPublic, String>>,
    },
    CheckConnection {
        reply: Reply<ConnectionReport>,
    },
    Shutdown {
        reply: Reply<()>,
    },
}

enum Internal {
    /// A child's watch changed.
    Poke,
    Net(NetFacts),
    LocalTest(Checked<Result<LightdInfo, LocalTestProblem>>),
    Hairpin(Checked<bool>),
    Applied {
        target: Option<u16>,
        result: Result<(), String>,
    },
    Door(launcher_gateway::Event),
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

    /// Turns the service off and forgets it, returning what Zeal could not undo.
    pub async fn forget(&self, remove_token: bool) -> Result<StillPublic, String> {
        ask!(self, Forget { remove_token })?
    }

    /// "Check my connection": the read-only network checks, without setting anything up.
    pub async fn check_connection(&self) -> Result<ConnectionReport, String> {
        ask!(self, CheckConnection {})
    }

    /// Closes the front door and lets go of everything, for Zeal's exit.
    pub async fn shutdown(&self) {
        let (reply, answer) = oneshot::channel();
        if self.tx.send(Msg::Shutdown { reply }).await.is_ok() {
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
    status_rx: watch::Receiver<Status>,
    stats_rx: watch::Receiver<Option<NodeStats>>,
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
    name_elsewhere_since: Option<SystemTime>,
    net: Option<NetFacts>,
    net_due: Option<tokio::time::Instant>,
    net_running: bool,
    firewall: Option<FirewallInfo>,
    port_free: Option<bool>,
    local_test: Option<Checked<Result<LightdInfo, LocalTestProblem>>>,
    local_running: bool,
    hairpin: Option<Checked<bool>>,
    hairpin_running: bool,
    keychain_unavailable: bool,
    /// The `backend_port` the app last applied to `zebrad.toml`.
    applied: Option<Option<u16>>,
    applying: bool,
    endpoint_error: Option<String>,
    acme_asked: bool,
    state: State,
    state_saved: tokio::time::Instant,
    /// The month's upload before the current front door started.
    door_base: u64,

    light: Option<Child<LightMonitor>>,
    ddns: Option<Child<DdnsHandle>>,
    ddns_wanted: watch::Sender<Option<IpAddr>>,
    acme: Option<Child<AcmeHandle>>,
    door: Option<Child<FrontDoor>>,
    challenge: watch::Sender<Option<Arc<launcher_gateway::Challenge>>>,
    mode: watch::Sender<launcher_gateway::Mode>,
    can_validate: watch::Sender<bool>,
    awake: Option<KeepAwake>,
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
        let applied = wiring
            .settings
            .current()
            .public
            .light_server()
            .map(|spec| spec.backend_port);
        Actor {
            options,
            settings: wiring.settings,
            status_rx: wiring.status,
            stats_rx: wiring.stats,
            host: wiring.host,
            secrets: wiring.secrets,
            fetch: wiring.fetch,
            out,
            internal_tx,
            internal_rx: Some(internal_rx),
            held: held_at_launch.then_some(launcher_types::public::PauseReason::NotResumed),
            paused: None,
            at_tip_since: None,
            name_elsewhere_since: None,
            net: None,
            net_due: None,
            net_running: false,
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
            door_base: 0,
            light: None,
            ddns: None,
            ddns_wanted: watch::channel(None).0,
            acme: None,
            door: None,
            challenge: watch::channel(None).0,
            mode: watch::channel(launcher_gateway::Mode::ChallengeOnly).0,
            can_validate: watch::channel(false).0,
            awake: None,
        }
    }

    async fn run(mut self, mut rx: mpsc::Receiver<Msg>) {
        let mut internal = self.internal_rx.take().expect("taken once");
        let mut status = self.status_rx.clone();
        let mut stats = self.stats_rx.clone();
        let mut settings = self.settings.watch();
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            self.step().await;
            tokio::select! {
                msg = rx.recv() => match msg {
                    Some(Msg::Shutdown { reply }) => {
                        self.shutdown();
                        let _ = reply.send(());
                        return;
                    }
                    Some(msg) => self.handle(msg).await,
                    None => {
                        self.shutdown();
                        return;
                    }
                },
                Some(event) = internal.recv() => self.internal(event),
                _ = status.changed() => {}
                _ = stats.changed() => {}
                _ = settings.changed() => {}
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

    /// The facts at `now` for `spec`.
    fn facts(&self, spec: &PublicServiceSpec, settings: &Settings, now: SystemTime) -> Facts {
        let status = self.status_rx.borrow().clone();
        let stats = self.stats_rx.borrow().clone();
        let mut facts = Facts::new(now, settings.network);
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
        facts.phase = status.phase;
        facts.endpoint = status.lightwalletd_addr;
        facts.endpoint_error = self.endpoint_error.clone();
        facts.height = stats.as_ref().map(|stats| stats.height);
        facts.tip = stats.as_ref().map(|stats| stats.estimated_height);
        facts.at_tip_since = self.at_tip_since;
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
        facts.evidence = self.state.evidence.clone();
        facts.name_elsewhere_since = self.name_elsewhere_since;
        let stats = self
            .door
            .as_ref()
            .map(|door| door.handle.stats.borrow().clone())
            .unwrap_or_default();
        facts.month_bytes_up = self.state.month_bytes_up + self.door_base + stats.bytes_up;
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
        facts
    }

    /// One look: the facts, the rules, the children, the status.
    async fn step(&mut self) {
        let now = SystemTime::now();
        let settings = self.settings.current();
        let Some(spec) = settings.public.light_server().cloned() else {
            self.stop_children();
            self.out.send_if_modified(|statuses| {
                let changed = !statuses.is_empty();
                statuses.clear();
                changed
            });
            return;
        };
        self.roll_month(now);
        if spec.network != settings.network {
            // Switching back does not resume by itself.
            self.held = Some(launcher_types::public::PauseReason::NetworkChanged);
        }
        let facts = self.facts(&spec, &settings, now);
        self.sync_endpoint(&spec, &facts, settings.network).await;
        let spec = self.spec().unwrap_or(spec);

        let mut facts = self.facts(&spec, &settings, now);
        self.at_tip_since = track_since(self.at_tip_since, at_tip(facts.height, facts.tip), now);
        facts.at_tip_since = self.at_tip_since;
        let expected = facts
            .net
            .as_ref()
            .filter(|net| net.wan == WanKind::Public)
            .and_then(|net| net.wan_addr);
        let elsewhere = match (&facts.dns, expected) {
            (Some(dns), Some(expected)) if dns.checked_at.is_some() => {
                !dns.resolved.contains(&expected)
            }
            _ => false,
        };
        self.name_elsewhere_since =
            track_since(self.name_elsewhere_since, elsewhere && spec.opened, now);
        facts.name_elsewhere_since = self.name_elsewhere_since;

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

        let facts = self.facts(&spec, &settings, now);
        let mut facts = facts;
        facts.at_tip_since = self.at_tip_since;
        facts.name_elsewhere_since = self.name_elsewhere_since;
        let status = rules::status(&spec, &facts, since);
        self.out.send_if_modified(|statuses| {
            let changed = statuses.len() != 1 || statuses[0] != status;
            *statuses = vec![status];
            changed
        });
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
            let _ = self.update_spec(|spec| spec.backend_port = None).await;
        }
        let target = self.spec().and_then(|spec| spec.backend_port);
        if !self.applying && self.applied != Some(target) {
            self.applying = true;
            // zebrad on another network than the service's keeps running as it is: the key
            // goes from the file before its next start.
            let restart = spec.network == network;
            let host = self.host.clone();
            let tx = self.internal_tx.clone();
            tokio::spawn(async move {
                let result = host.apply(restart).await;
                let _ = tx.send(Internal::Applied { target, result }).await;
            });
        }
    }

    fn internal(&mut self, event: Internal) {
        match event {
            Internal::Poke => {}
            Internal::Net(net) => {
                self.net_running = false;
                self.ddns_wanted.send_if_modified(|wanted| {
                    let next = (net.wan == WanKind::Public)
                        .then_some(net.wan_addr)
                        .flatten();
                    let changed = *wanted != next;
                    *wanted = next;
                    changed
                });
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
                self.record(Evidence {
                    source: EvidenceSource::OutsideTlsConnection,
                    at,
                });
            }
            Internal::Door(_) => {}
        }
    }

    fn record(&mut self, new: Evidence) {
        let before = self.state.evidence.clone();
        evidence::record(&mut self.state.evidence, new);
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

    fn save_state(&mut self) {
        let mut state = self.state.clone();
        if let Some(door) = &self.door {
            state.month_bytes_up += self.door_base + door.handle.stats.borrow().bytes_up;
        }
        let _ = store::save(&self.options.paths.state(), &state, false);
        self.state_saved = tokio::time::Instant::now();
    }

    fn roll_month(&mut self, now: SystemTime) {
        let month = month_of(now);
        if self.state.month != month {
            self.state.month = month;
            self.state.month_bytes_up = 0;
            self.door_base = 0;
            self.save_state();
        } else if self.state_saved.elapsed() >= SAVE_EVERY {
            self.save_state();
        }
    }

    /// Ends the front door and moves its upload into the month's total.
    fn stop_door(&mut self) {
        if let Some(door) = self.door.take() {
            self.door_base += door.handle.stats.borrow().bytes_up;
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

    fn shutdown(&mut self) {
        self.stop_door();
        self.awake = None;
        self.state.month_bytes_up += std::mem::take(&mut self.door_base);
        let _ = store::save(&self.options.paths.state(), &self.state, false);
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
        let running = !matches!(
            state,
            PublicState::Paused { .. } | PublicState::NotAvailable { .. } | PublicState::Off
        );

        // The light monitor follows zebrad's endpoint.
        if self
            .light
            .as_ref()
            .is_none_or(|light| light.key != spec.network.as_str())
        {
            let monitor =
                LightMonitor::spawn(spec.network, self.status_rx.clone(), self.stats_rx.clone());
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
        if running
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

        // The name.
        match &spec.name {
            Some(name) if running => {
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
                        self.state.staging_validated_at,
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
            let validated = *acme.handle.validated.borrow();
            let staging = acme.handle.status.borrow().staging_validated_at;
            if let Some(at) = validated {
                self.record(Evidence {
                    source: EvidenceSource::TlsAlpn01Validation,
                    at,
                });
            }
            if staging.is_some() && self.state.staging_validated_at != staging {
                self.state.staging_validated_at = staging;
                self.save_state();
            }
        }

        // The front door, in the mode the rules give.
        let upstream = spec
            .backend_port
            .map(|port| SocketAddr::from((Ipv4Addr::LOCALHOST, port)));
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

        // The test on this computer, once there is a certificate and the front door serves
        // this computer.
        let cert_valid = facts
            .cert
            .as_ref()
            .and_then(|cert| cert.not_after)
            .is_some_and(|not_after| not_after > facts.now);
        if current == Some(StepId::LocalTest)
            && cert_valid
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
        tokio::spawn(async move {
            let net = netcheck::check(SystemTime::now()).await;
            let _ = tx.send(Internal::Net(net)).await;
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
                    // Less exposure acts at once: the front door closes before anything else.
                    self.stop_door();
                    self.awake = None;
                }
                if on {
                    self.held = None;
                }
                self.endpoint_error = None;
                let result = self.update_spec(move |spec| spec.switch_on = on).await;
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
                reply,
            } => {
                let result = self.forget(remove_token).await;
                let _ = reply.send(result);
            }
            Msg::CheckConnection { reply } => {
                let report = self.check_connection().await;
                let _ = reply.send(report);
            }
            Msg::Shutdown { .. } => unreachable!("handled by the loop"),
        }
    }

    async fn begin_setup(&mut self, network: Network) -> Result<NodeId, String> {
        if let Some(spec) = self.spec() {
            return Ok(spec.node);
        }
        if !self.options.supported {
            return Err("public services run on Linux only for now".into());
        }
        if network != self.settings.current().network {
            return Err(format!(
                "Zeal runs zebrad on the other network; switch to {network} first"
            ));
        }
        let node = NodeId::new(format!("zebrad-{}", network.as_str().to_ascii_lowercase()))
            .ok_or("no node id")?;
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
                    })
                })
                .await
            }
            StepAnswer::Name { name, provider } => {
                let name = parse_name(&name, provider).map_err(|err| err.to_string())?;
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
                    spec.secret = None;
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
                let account = keychain_account(spec.provider, &name);
                match self
                    .secrets
                    .set(account.clone(), token.trim().to_string())
                    .await
                {
                    Ok(()) => {
                        self.keychain_unavailable = false;
                        let provider = spec.provider;
                        self.update_spec(move |spec| {
                            spec.secret =
                                Some(launcher_types::public::SecretRef { provider, account })
                        })
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
        let settings = self.settings.current();
        let mut facts = self.facts(&spec, &settings, SystemTime::now());
        facts.at_tip_since = self.at_tip_since;
        rules::open_gates(&spec, &facts)?;
        self.update_spec(|spec| spec.opened = true)
            .await
            .map_err(|err| refuse(&err))?;
        self.hairpin = None;
        tracing::info!(target: "zeal::public", "light client server opened to the internet");
        Ok(())
    }

    async fn forget(&mut self, remove_token: bool) -> Result<StillPublic, String> {
        let spec = self.spec().ok_or("no light client server is set up")?;
        self.stop_children();
        self.update_spec(|spec| {
            spec.switch_on = false;
            spec.backend_port = None;
        })
        .await?;
        // The endpoint must be gone before the spec is: once it is, the key is the user's.
        self.host
            .apply(spec.network == self.settings.current().network)
            .await?;
        self.applied = Some(None);
        if remove_token && let Some(name) = &spec.name {
            let _ = self
                .secrets
                .delete(keychain_account(spec.provider, name))
                .await;
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
        })
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
    }

    #[test]
    fn months_are_utc_year_and_month() {
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_793_700_000);
        assert_eq!(month_of(at), "2026-11");
    }
}
