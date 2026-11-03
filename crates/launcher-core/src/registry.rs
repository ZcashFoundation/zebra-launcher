//! The node registry: one actor owns the node list and a runtime per node.
//!
//! ```text
//!   RegistryHandle ──Cmd──▶ registry actor ──▶ watch<Arc<Vec<NodeHandle>>>  (lookups by id)
//!        │                    │   ▲        ──▶ watch<Vec<NodeSummary>>      (the list, phases)
//!        │                    │   └── phase changes, finished removals
//!        │                    ▼
//!        │          SettingsHandle::save_nodes (launcher.json)
//!        ▼
//!   NodeHandle ──▶ the node's supervisor, RPC monitor, metrics poller and gates
//! ```
//!
//! Each node runs on its own actors, as the single node did: its supervisor
//! (with its log ring, log file, pid file and restart policy), its RPC
//! monitor and its metrics poller, created from the node's spec. The
//! registry actor only changes the list and saves it; starting, stopping and
//! the node's files go straight to the node's actors through its
//! [`NodeHandle`], which callers look up by id in the published list without
//! a round trip. Long work (a removal that stops the node and deletes its
//! chain state, the shutdown at quit) runs in a task that reports back, so
//! one of them never holds up the others. No locks.

use std::{
    io,
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicU64},
    time::Duration,
};

use launcher_types::{
    MetricsSnapshot, NodeDetails, NodeDraft, NodeId, NodeInfo, NodeStats, NodeUpdate, Phase,
    PortRole, Settings,
};
use tokio::{
    sync::{mpsc, oneshot, watch},
    task::JoinSet,
};

use crate::{
    config,
    firstrun::{self, SizeCache},
    gates::NodeGates,
    metrics::poller,
    monitor,
    nodes::{self, NodeError, NodeLayout, NodeSpec},
    paths::{NodePaths, Paths},
    runtime::{self, NodeActors},
    settings::SettingsHandle,
    supervisor::{self, Listener, Note, StartOptions, SupervisorConfig, SupervisorError},
};

/// Whether another program holds a node's port: `(role, port)`.
pub type PortProbe = Arc<dyn Fn(PortRole, u16) -> bool + Send + Sync>;

/// A last word on each node's supervisor configuration before it starts: the
/// app sets the stdin-close mode, tests a fake zebrad's mode and fast timings.
pub type Customize = Arc<dyn Fn(&NodeSpec, &mut SupervisorConfig) + Send + Sync>;

/// How the registry runs its nodes.
#[derive(Clone)]
pub struct RegistryConfig {
    /// The launcher's directories.
    pub paths: Paths,
    /// The bundled zebrad, which every zebrad node runs.
    pub zebrad: PathBuf,
    /// Applied to every node's supervisor configuration.
    pub customize: Option<Customize>,
    /// How often each node's RPC is polled.
    pub monitor_interval: Duration,
    /// How often each node's metrics page is read.
    pub metrics_interval: Duration,
    /// Whether another program holds a port, for a new node's ports.
    pub port_in_use: PortProbe,
    /// The sizes measured this session, so a deleted state reports what it held.
    pub sizes: Arc<SizeCache>,
}

impl RegistryConfig {
    /// The defaults: zebrad's polling intervals, and a port counts as in use
    /// when binding it fails ([`port_in_use`]).
    pub fn new(paths: Paths, zebrad: impl Into<PathBuf>) -> Self {
        Self {
            paths,
            zebrad: zebrad.into(),
            customize: None,
            monitor_interval: monitor::DEFAULT_INTERVAL,
            metrics_interval: poller::DEFAULT_INTERVAL,
            port_in_use: Arc::new(port_in_use),
            sizes: Arc::new(SizeCache::default()),
        }
    }
}

/// Where zebrad listens on `port` for `role`, for a check that binds it:
/// every address for P2P, loopback for the rest.
pub fn listen_addr(role: PortRole, port: u16) -> SocketAddr {
    let ip = match role {
        PortRole::P2p => Ipv4Addr::UNSPECIFIED,
        _ => Ipv4Addr::LOCALHOST,
    };
    SocketAddr::from((ip, port))
}

/// Whether another program holds `port`: binding [`listen_addr`] fails with
/// `AddrInUse`.
pub fn port_in_use(role: PortRole, port: u16) -> bool {
    matches!(
        std::net::TcpListener::bind(listen_addr(role, port)),
        Err(err) if err.kind() == io::ErrorKind::AddrInUse
    )
}

/// The addresses a node's supervisor checks before every start.
fn listeners(spec: &NodeSpec) -> Vec<Listener> {
    spec.ports
        .iter()
        .map(|port| Listener {
            role: port.role,
            addr: listen_addr(port.role, port.port),
        })
        .collect()
}

/// A node and what it is doing: the tray's and the UI's list.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeSummary {
    pub info: NodeInfo,
    pub phase: Phase,
}

/// The nodes' phases as one, for the updater, which stops and restarts them
/// together: `Failed { EosHalt }` when a node halted at its end of support
/// (they all run the bundled zebrad), else `Running` while any node runs or
/// will without the user ([`Phase::is_active`]), else `Stopped`.
pub fn herd_phase<'a>(phases: impl IntoIterator<Item = &'a Phase>) -> Phase {
    let mut active = false;
    for phase in phases {
        if matches!(
            phase,
            Phase::Failed {
                class: launcher_types::ExitClass::EosHalt
            }
        ) {
            return phase.clone();
        }
        active |= phase.is_active();
    }
    if active {
        Phase::Running
    } else {
        Phase::Stopped
    }
}

/// One node's runtime: cheap to clone, every part a handle to the node's own
/// actors or a channel.
#[derive(Clone, Debug)]
pub struct NodeHandle {
    /// The node's id when this handle was published. A node's id changes only
    /// while the first run is not done (its network then gives it its id),
    /// and then the registry publishes a new handle.
    pub id: NodeId,
    /// A number no other handle of this registry has had, so a node removed
    /// and added again under the same id is told apart.
    pub serial: u64,
    spec: watch::Receiver<NodeSpec>,
    launcher: Paths,
    /// The node's files.
    pub paths: NodePaths,
    /// The node's zebrad supervisor.
    pub supervisor: supervisor::SupervisorHandle,
    /// The latest RPC poll, `None` while the node is not running.
    pub stats: watch::Receiver<Option<NodeStats>>,
    /// The details behind [`NodeHandle::stats`], from the same poll.
    pub details: watch::Receiver<Option<NodeDetails>>,
    /// The latest read of the node's metrics page.
    pub metrics: watch::Receiver<Option<MetricsSnapshot>>,
    /// Keeps the node's starts, data changes and config writes apart.
    pub gates: NodeGates,
    /// Bumped by every log subscription to the node; an older forwarder
    /// stops when it changes, so a reloaded UI leaves none behind.
    pub log_generation: Arc<AtomicU64>,
}

/// Why a node did not start.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    /// The first run is not done, or the node's data is being changed.
    #[error("{0}")]
    Refused(String),
    /// The supervisor could not start zebrad; a stale zebrad from an earlier
    /// launcher run is [`SupervisorError::StaleProcess`].
    #[error(transparent)]
    Supervisor(#[from] SupervisorError),
}

impl NodeHandle {
    /// The node's spec as it is now.
    pub fn spec(&self) -> NodeSpec {
        self.spec.borrow().clone()
    }

    /// The node as the UI sees it.
    pub fn info(&self) -> NodeInfo {
        self.spec.borrow().info(&self.launcher)
    }

    /// The node's data folder.
    pub fn data_dir(&self) -> PathBuf {
        self.spec.borrow().data_dir(&self.launcher)
    }

    /// The node's chain state folder.
    pub fn state_dir(&self) -> PathBuf {
        self.spec.borrow().state_dir(&self.launcher)
    }

    /// The `zebrad.toml` keys the launcher manages for the node now.
    pub fn managed_keys(&self) -> config::ManagedKeys {
        self.spec.borrow().managed_keys(&self.launcher)
    }

    /// The folders a deletion of the node's state may happen in: the
    /// launcher's data directory and the node's data folder.
    pub fn data_roots(&self) -> Vec<PathBuf> {
        vec![self.launcher.data_dir().to_path_buf(), self.data_dir()]
    }

    /// Starts the node the way every start does: not before the first run is
    /// done ([`firstrun::start_allowed`]) and not while its data is being
    /// changed, holding a start permit until zebrad accepted its
    /// configuration.
    pub async fn start(
        &self,
        settings: &Settings,
        options: StartOptions,
    ) -> Result<(), StartError> {
        firstrun::start_allowed(settings).map_err(StartError::Refused)?;
        let _permit = self.gates.start_permit().map_err(StartError::Refused)?;
        self.supervisor.start_with(options).await?;
        Ok(())
    }

    /// Stops the node, then starts it again, under the same rules as
    /// [`NodeHandle::start`].
    pub async fn restart(&self, settings: &Settings) -> Result<(), StartError> {
        firstrun::start_allowed(settings).map_err(StartError::Refused)?;
        let _permit = self.gates.start_permit().map_err(StartError::Refused)?;
        self.supervisor.restart().await?;
        Ok(())
    }
}

type Reply<T> = oneshot::Sender<Result<T, NodeError>>;

enum Cmd {
    Add {
        draft: NodeDraft,
        reply: Reply<NodeInfo>,
    },
    Update {
        id: NodeId,
        update: NodeUpdate,
        reply: Reply<NodeInfo>,
    },
    SetDataDir {
        id: NodeId,
        dir: Option<String>,
        reply: Reply<NodeInfo>,
    },
    Remove {
        id: NodeId,
        delete_data: bool,
        reply: Reply<u64>,
    },
    StopForUpdate {
        reply: oneshot::Sender<Vec<NodeHandle>>,
    },
    TakeStoppedForUpdate {
        reply: oneshot::Sender<Vec<NodeId>>,
    },
    Shutdown {
        reply: oneshot::Sender<()>,
    },
}

enum Event {
    /// A node's phase changed.
    Phase,
    /// A removal stopped its node and dealt with its data.
    Removed {
        serial: u64,
        delete_data: bool,
        result: Result<u64, NodeError>,
        reply: Reply<u64>,
    },
}

/// A node as the actor keeps it: the published handle and the senders behind it.
struct Runtime {
    handle: NodeHandle,
    spec: watch::Sender<NodeSpec>,
    listeners: watch::Sender<Vec<Listener>>,
    /// A removal is under way.
    removing: bool,
}

/// A cloneable handle to the registry actor.
#[derive(Clone, Debug)]
pub struct RegistryHandle {
    cmd: mpsc::Sender<Cmd>,
    nodes: watch::Receiver<Arc<Vec<NodeHandle>>>,
    summaries: watch::Receiver<Vec<NodeSummary>>,
}

/// Starts a runtime for every node in `settings` and the registry actor.
/// Must be called inside a Tokio runtime.
///
/// Before a node's runtime starts, a configuration change that a launcher
/// exit interrupted is rolled back ([`config::recover`]) and the managed keys
/// of its `zebrad.toml` are brought up to date ([`config::ensure`]); a
/// failure of either is logged, not fatal, as zebrad reports a broken file
/// and the editor can fix it. No node is started.
pub fn spawn(config: RegistryConfig, settings: SettingsHandle) -> io::Result<RegistryHandle> {
    let (cmd_tx, cmd_rx) = mpsc::channel(32);
    let (events_tx, events_rx) = mpsc::channel(64);
    let mut actor = Actor {
        config,
        settings,
        nodes: Vec::new(),
        next_serial: 1,
        stopped_for_update: Vec::new(),
        closed: false,
        cmd_rx,
        events_tx,
        events_rx,
        nodes_tx: watch::channel(Arc::new(Vec::new())).0,
        summaries_tx: watch::channel(Vec::new()).0,
    };
    for spec in actor.settings.nodes() {
        runtime::prepare_files(&actor.config.paths, &spec);
        let runtime = actor.new_runtime(spec)?;
        actor.nodes.push(runtime);
    }
    actor.publish();
    let handle = RegistryHandle {
        cmd: cmd_tx,
        nodes: actor.nodes_tx.subscribe(),
        summaries: actor.summaries_tx.subscribe(),
    };
    tokio::spawn(actor.run());
    Ok(handle)
}

impl RegistryHandle {
    /// The nodes, in the list's order. Never empty while the registry runs.
    pub fn nodes(&self) -> Arc<Vec<NodeHandle>> {
        self.nodes.borrow().clone()
    }

    /// A receiver that sees every change of the node set: a node added,
    /// removed, or given a new id.
    pub fn watch_nodes(&self) -> watch::Receiver<Arc<Vec<NodeHandle>>> {
        self.nodes.clone()
    }

    /// The node with `id`.
    pub fn node(&self, id: &NodeId) -> Result<NodeHandle, NodeError> {
        self.nodes
            .borrow()
            .iter()
            .find(|node| &node.id == id)
            .cloned()
            .ok_or_else(|| NodeError::NotFound(id.clone()))
    }

    /// The first node in the list: the one the first run sets up.
    pub fn first(&self) -> Option<NodeHandle> {
        self.nodes.borrow().first().cloned()
    }

    /// Every node as the UI sees it, in the list's order.
    pub fn infos(&self) -> Vec<NodeInfo> {
        self.summaries
            .borrow()
            .iter()
            .map(|summary| summary.info.clone())
            .collect()
    }

    /// A receiver that sees every change of a node's spec or phase and of
    /// the list.
    pub fn watch_summaries(&self) -> watch::Receiver<Vec<NodeSummary>> {
        self.summaries.clone()
    }

    /// How long a caller should wait for [`Self::shutdown`]: the longest of
    /// the nodes' shutdown caps, as they stop side by side.
    pub fn shutdown_cap(&self) -> Duration {
        self.nodes
            .borrow()
            .iter()
            .map(|node| node.supervisor.shutdown_cap())
            .max()
            .unwrap_or_else(|| supervisor::shutdown_cap(Duration::ZERO))
    }

    /// Adds the node `draft` describes, stopped, after the checks of
    /// [`nodes::plan_new_node`]: its folder is created with a minimal
    /// `zebrad.toml`, and it is saved to the node list.
    pub async fn add(&self, draft: NodeDraft) -> Result<NodeInfo, NodeError> {
        self.ask(|reply| Cmd::Add { draft, reply }).await
    }

    /// Changes a node ([`nodes::plan_update`]). A new network moves its ports
    /// and rewrites the managed keys of its `zebrad.toml` right away (refused
    /// while a configuration change is being applied); like a new stop grace
    /// period it takes effect on the node's next start, the stop grace on the
    /// next launch. While the first run is not done, a node in the launcher's
    /// directories also takes its new network's id.
    pub async fn update(&self, id: &NodeId, update: NodeUpdate) -> Result<NodeInfo, NodeError> {
        let id = id.clone();
        self.ask(|reply| Cmd::Update { id, update, reply }).await
    }

    /// Makes `path` the node's data folder: its chain state then lives in
    /// `<path>/state`. The folder is created if needed and must be writable,
    /// and may not be another node's; choosing the node's default folder
    /// restores the default. Only while the node is stopped or failed: it
    /// holds the node's data and config permits, and rewrites the managed
    /// `state.cache_dir`. Returns the node and the free space at `path`.
    pub async fn choose_data_dir(
        &self,
        id: &NodeId,
        path: &str,
    ) -> Result<(NodeInfo, Option<u64>), NodeError> {
        let node = self.node(id)?;
        let _data = node.gates.data_change_permit().map_err(NodeError::Failed)?;
        let _config = node.gates.try_config_permit().map_err(NodeError::Failed)?;
        let dir = PathBuf::from(path.trim());
        let default = node.paths.data_dir().to_path_buf();
        let (free, is_default) = tokio::task::spawn_blocking(move || {
            firstrun::check_chosen_dir(&dir, &default).map_err(NodeError::Failed)
        })
        .await
        .map_err(|err| NodeError::Failed(err.to_string()))??;
        let dir = (!is_default).then(|| path.trim().to_string());
        let id = id.clone();
        let info = self.ask(|reply| Cmd::SetDataDir { id, dir, reply }).await?;
        Ok((info, free))
    }

    /// Removes a node: it is stopped (killed after its grace period), its
    /// chain state deleted with [`firstrun::delete_node_data`]'s rules when
    /// `delete_data` is set (with the rest of its folder, for a node in a
    /// folder of its own), its runtime shut down, and it leaves the node
    /// list. Kept data stays where it is, and its id is not given to a new
    /// node while its folder exists. Refused for the last node, and while the
    /// node is starting or its data or config is being changed. Returns the
    /// bytes freed as far as they are known (0 for unknown or kept).
    pub async fn remove(&self, id: &NodeId, delete_data: bool) -> Result<u64, NodeError> {
        let id = id.clone();
        self.ask(|reply| Cmd::Remove {
            id,
            delete_data,
            reply,
        })
        .await
    }

    /// Stops every node that is not stopped, side by side, and remembers the
    /// ones that were running, for [`Self::take_stopped_for_update`]. Each
    /// stop waits for its node's exit (a kill after its grace period).
    pub async fn stop_for_update(&self) -> Result<(), NodeError> {
        let (reply, response) = oneshot::channel();
        self.cmd
            .send(Cmd::StopForUpdate { reply })
            .await
            .map_err(|_| NodeError::Closed)?;
        let nodes = response.await.map_err(|_| NodeError::Closed)?;
        stop_each(nodes).await
    }

    /// The nodes the last [`Self::stop_for_update`] found running, once: a
    /// second call returns none.
    pub async fn take_stopped_for_update(&self) -> Vec<NodeId> {
        let (reply, response) = oneshot::channel();
        if self
            .cmd
            .send(Cmd::TakeStoppedForUpdate { reply })
            .await
            .is_err()
        {
            return Vec::new();
        }
        response.await.unwrap_or_default()
    }

    /// Stops every node that is not stopped, side by side.
    pub async fn stop_all(&self) -> Result<(), NodeError> {
        let nodes = self.nodes().iter().cloned().collect();
        stop_each(nodes).await
    }

    /// Stops every node and ends every runtime, side by side, each within its
    /// own stop grace; then the registry refuses further changes.
    pub async fn shutdown(&self) {
        let (reply, response) = oneshot::channel();
        if self.cmd.send(Cmd::Shutdown { reply }).await.is_ok() {
            let _ = response.await;
        }
    }

    /// The nodes' status as the updater follows it: [`herd_phase`] of their
    /// phases, kept up to date by a task until the receiver or the registry
    /// is gone.
    pub fn watch_herd(&self) -> watch::Receiver<launcher_types::Status> {
        let fold = |summaries: &[NodeSummary]| launcher_types::Status {
            phase: herd_phase(summaries.iter().map(|summary| &summary.phase)),
            ..launcher_types::Status::default()
        };
        let mut summaries = self.summaries.clone();
        let (tx, rx) = watch::channel(fold(&summaries.borrow_and_update()));
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    changed = summaries.changed() => if changed.is_err() {
                        return;
                    },
                    () = tx.closed() => return,
                }
                let status = fold(&summaries.borrow_and_update());
                tx.send_if_modified(|current| {
                    let changed = *current != status;
                    *current = status;
                    changed
                });
            }
        });
        rx
    }

    /// The statistics the updater reads the end of support from: those of
    /// the first zebrad node on Mainnet, the only network with one (every
    /// node runs the same zebrad, so one is enough), and `None` while there
    /// is no such node. Kept up to date by a task, which follows the node
    /// list too, until the receiver or the registry is gone.
    pub fn eos_stats(&self) -> watch::Receiver<Option<NodeStats>> {
        let registry = self.clone();
        let (tx, rx) = watch::channel(None);
        tokio::spawn(async move {
            let mut summaries = registry.summaries.clone();
            loop {
                summaries.borrow_and_update();
                let source = registry
                    .nodes()
                    .iter()
                    .find(|node| {
                        let spec = node.spec();
                        spec.kind == launcher_types::NodeKind::Zebrad
                            && spec.network == launcher_types::Network::Mainnet
                    })
                    .cloned();
                let Some(node) = source else {
                    tx.send_if_modified(|current| current.take().is_some());
                    tokio::select! {
                        changed = summaries.changed() => if changed.is_err() {
                            return;
                        },
                        () = tx.closed() => return,
                    }
                    continue;
                };
                let mut stats = node.stats.clone();
                loop {
                    tx.send_replace(stats.borrow_and_update().clone());
                    tokio::select! {
                        changed = stats.changed() => if changed.is_err() {
                            break;
                        },
                        changed = summaries.changed() => {
                            if changed.is_err() {
                                return;
                            }
                            break;
                        }
                        () = tx.closed() => return,
                    }
                }
            }
        });
        rx
    }

    /// Adds a launcher line to the log of `node`, or of every node: what the
    /// launcher did or decided. Never waits ([`supervisor::SupervisorHandle::note_at`]).
    pub fn note(&self, node: Option<&NodeId>, note: Note) {
        for handle in self.nodes.borrow().iter() {
            if node.is_none_or(|id| &handle.id == id) {
                handle.supervisor.note_at(note.clone());
            }
        }
    }

    async fn ask<T>(&self, cmd: impl FnOnce(Reply<T>) -> Cmd) -> Result<T, NodeError> {
        let (reply, response) = oneshot::channel();
        self.cmd
            .send(cmd(reply))
            .await
            .map_err(|_| NodeError::Closed)?;
        response.await.map_err(|_| NodeError::Closed)?
    }
}

/// Stops each node that is not settled, side by side.
async fn stop_each(nodes: Vec<NodeHandle>) -> Result<(), NodeError> {
    let mut stops = JoinSet::new();
    for node in nodes {
        if !node.supervisor.status().phase.is_settled() {
            stops.spawn(async move { node.supervisor.stop().await });
        }
    }
    let mut result = Ok(());
    while let Some(stopped) = stops.join_next().await {
        if let Err(err) = stopped
            .map_err(|err| err.to_string())
            .and_then(|stopped| stopped.map_err(|err| err.to_string()))
        {
            result = Err(NodeError::Failed(err));
        }
    }
    result
}

struct Actor {
    config: RegistryConfig,
    settings: SettingsHandle,
    nodes: Vec<Runtime>,
    next_serial: u64,
    stopped_for_update: Vec<NodeId>,
    /// A shutdown was asked for: no more changes.
    closed: bool,
    cmd_rx: mpsc::Receiver<Cmd>,
    events_tx: mpsc::Sender<Event>,
    events_rx: mpsc::Receiver<Event>,
    nodes_tx: watch::Sender<Arc<Vec<NodeHandle>>>,
    summaries_tx: watch::Sender<Vec<NodeSummary>>,
}

impl Actor {
    async fn run(mut self) {
        loop {
            tokio::select! {
                cmd = self.cmd_rx.recv() => match cmd {
                    Some(cmd) => self.on_cmd(cmd).await,
                    // Every handle is gone: stop the nodes on the way out.
                    None => {
                        self.shutdown_all(None);
                        return;
                    }
                },
                Some(event) = self.events_rx.recv() => self.on_event(event).await,
            }
        }
    }

    /// The runtime for `spec`: its supervisor, monitor, poller and gates.
    fn new_runtime(&mut self, spec: NodeSpec) -> io::Result<Runtime> {
        let paths = spec.paths(&self.config.paths);
        let (listeners_tx, listeners_rx) = watch::channel(listeners(&spec));
        let NodeActors {
            supervisor,
            stats,
            details,
            metrics,
        } = runtime::spawn(&self.config, &spec, listeners_rx)?;
        // Phase changes reach the summaries through the actor.
        let mut status = supervisor.watch_status();
        let events = self.events_tx.clone();
        tokio::spawn(async move {
            let mut last = status.borrow_and_update().phase.clone();
            while status.changed().await.is_ok() {
                let phase = status.borrow_and_update().phase.clone();
                if phase != last {
                    last = phase;
                    if events.send(Event::Phase).await.is_err() {
                        return;
                    }
                }
            }
        });
        let (spec_tx, spec_rx) = watch::channel(spec.clone());
        let serial = self.next_serial;
        self.next_serial += 1;
        Ok(Runtime {
            handle: NodeHandle {
                id: spec.id,
                serial,
                spec: spec_rx,
                launcher: self.config.paths.clone(),
                gates: NodeGates::new(supervisor.watch_status()),
                paths,
                supervisor,
                stats,
                details,
                metrics,
                log_generation: Arc::new(AtomicU64::new(0)),
            },
            spec: spec_tx,
            listeners: listeners_tx,
            removing: false,
        })
    }

    fn specs(&self) -> Vec<NodeSpec> {
        self.nodes
            .iter()
            .map(|runtime| runtime.spec.borrow().clone())
            .collect()
    }

    fn index_of(&self, id: &NodeId) -> Result<usize, NodeError> {
        self.nodes
            .iter()
            .position(|runtime| &runtime.handle.id == id)
            .ok_or_else(|| NodeError::NotFound(id.clone()))
    }

    /// Publishes the node set (when it changed) and the summaries.
    fn publish(&self) {
        let handles: Vec<NodeHandle> = self.nodes.iter().map(|r| r.handle.clone()).collect();
        self.nodes_tx.send_if_modified(|current| {
            let same = current.len() == handles.len()
                && current
                    .iter()
                    .zip(&handles)
                    .all(|(a, b)| a.serial == b.serial);
            if !same {
                *current = Arc::new(handles);
            }
            !same
        });
        self.publish_summaries();
    }

    fn publish_summaries(&self) {
        let summaries: Vec<NodeSummary> = self
            .nodes
            .iter()
            .map(|runtime| NodeSummary {
                info: runtime.handle.info(),
                phase: runtime.handle.supervisor.status().phase,
            })
            .collect();
        self.summaries_tx.send_if_modified(|current| {
            let changed = *current != summaries;
            if changed {
                *current = summaries;
            }
            changed
        });
    }

    async fn save(&self, specs: Vec<NodeSpec>) -> Result<(), NodeError> {
        self.settings
            .save_nodes(specs)
            .await
            .map_err(|err| NodeError::Failed(err.to_string()))
    }

    async fn on_cmd(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Add { draft, reply } => {
                let result = self.add(draft).await;
                let _ = reply.send(result);
            }
            Cmd::Update { id, update, reply } => {
                let result = self.update(&id, update).await;
                let _ = reply.send(result);
            }
            Cmd::SetDataDir { id, dir, reply } => {
                let result = self.set_data_dir(&id, dir).await;
                let _ = reply.send(result);
            }
            Cmd::Remove {
                id,
                delete_data,
                reply,
            } => self.begin_remove(&id, delete_data, reply),
            Cmd::StopForUpdate { reply } => {
                self.stopped_for_update = self
                    .nodes
                    .iter()
                    .filter(|runtime| runtime.handle.supervisor.status().phase.is_active())
                    .map(|runtime| runtime.handle.id.clone())
                    .collect();
                let _ = reply.send(self.nodes.iter().map(|r| r.handle.clone()).collect());
            }
            Cmd::TakeStoppedForUpdate { reply } => {
                let _ = reply.send(std::mem::take(&mut self.stopped_for_update));
            }
            Cmd::Shutdown { reply } => self.shutdown_all(Some(reply)),
        }
    }

    async fn on_event(&mut self, event: Event) {
        match event {
            Event::Phase => self.publish_summaries(),
            Event::Removed {
                serial,
                delete_data,
                result,
                reply,
            } => self.finish_remove(serial, delete_data, result, reply).await,
        }
    }

    async fn add(&mut self, draft: NodeDraft) -> Result<NodeInfo, NodeError> {
        if self.closed {
            return Err(NodeError::Closed);
        }
        let specs = self.specs();
        let in_use = self.config.port_in_use.clone();
        let spec = nodes::plan_new_node(&draft, &specs, &self.config.paths, &|role, port| {
            in_use(role, port)
        })?;
        // The folder is new: a removed node's folder keeps its id from being
        // planned. Created before the list is saved, and removed again if the
        // save fails.
        let files = spec.paths(&self.config.paths);
        let keys = spec.managed_keys(&self.config.paths);
        let created = files.clone();
        blocking(move || {
            created.ensure_dirs()?;
            config::ensure(&created, &keys).map_err(io::Error::other)
        })
        .await
        .map_err(|err| NodeError::Failed(format!("could not create the node's files: {err}")))?;
        let mut next = specs;
        next.push(spec.clone());
        if let Err(err) = self.save(next).await {
            let folder = files.data_dir().to_path_buf();
            let _ = blocking(move || std::fs::remove_dir_all(folder)).await;
            return Err(err);
        }
        let runtime = self.new_runtime(spec).map_err(|err| {
            NodeError::Failed(format!("could not start the node's supervisor: {err}"))
        })?;
        let info = runtime.handle.info();
        tracing::info!(node = %info.id, name = %info.name, "added a node");
        self.nodes.push(runtime);
        self.publish();
        Ok(info)
    }

    async fn update(&mut self, id: &NodeId, update: NodeUpdate) -> Result<NodeInfo, NodeError> {
        if self.closed {
            return Err(NodeError::Closed);
        }
        let index = self.index_of(id)?;
        let runtime = &self.nodes[index];
        if runtime.removing {
            return Err(NodeError::Failed("the node is being removed".into()));
        }
        let current = runtime.spec.borrow().clone();
        let others: Vec<NodeSpec> = self
            .specs()
            .into_iter()
            .filter(|spec| &spec.id != id)
            .collect();
        let rename = !self.settings.current().first_run_done;
        let next = nodes::plan_update(&current, &update, &others, rename)?;
        if next == current {
            return Ok(runtime.handle.info());
        }
        // A new network changes the managed keys: written now, under the
        // node's config permit, so no apply interleaves.
        let _config =
            if next.managed_keys(&self.config.paths) != current.managed_keys(&self.config.paths) {
                let permit = runtime
                    .handle
                    .gates
                    .try_config_permit()
                    .map_err(NodeError::Failed)?;
                let files = next.paths(&self.config.paths);
                let keys = next.managed_keys(&self.config.paths);
                blocking(move || config::ensure(&files, &keys).map_err(io::Error::other))
                    .await
                    .map_err(|err| {
                        NodeError::Failed(format!("zebrad.toml could not be updated: {err}"))
                    })?;
                Some(permit)
            } else {
                None
            };
        let mut specs = self.specs();
        specs[index] = next.clone();
        self.save(specs).await?;
        let runtime = &mut self.nodes[index];
        runtime.listeners.send_replace(listeners(&next));
        runtime.spec.send_replace(next.clone());
        if next.id != current.id {
            // The first run gave the node its network's id: a new handle.
            runtime.handle.id = next.id.clone();
            runtime.handle.serial = self.next_serial;
            self.next_serial += 1;
            tracing::info!(from = %current.id, to = %next.id, "the first node takes its network's id");
        }
        let info = self.nodes[index].handle.info();
        tracing::info!(node = %info.id, "changed a node");
        self.publish();
        Ok(info)
    }

    async fn set_data_dir(
        &mut self,
        id: &NodeId,
        dir: Option<String>,
    ) -> Result<NodeInfo, NodeError> {
        if self.closed {
            return Err(NodeError::Closed);
        }
        let index = self.index_of(id)?;
        let current = self.nodes[index].spec.borrow().clone();
        let next = NodeSpec {
            data_dir: dir,
            ..current
        };
        let specs = self.specs();
        nodes::check_data_dir(&next, &specs, &self.config.paths)?;
        let files = next.paths(&self.config.paths);
        let keys = next.managed_keys(&self.config.paths);
        blocking(move || config::ensure(&files, &keys).map_err(io::Error::other))
            .await
            .map_err(|err| {
                NodeError::Failed(format!(
                    "the data folder was not saved: zebrad.toml could not be updated: {err}"
                ))
            })?;
        let mut specs = specs;
        specs[index] = next.clone();
        self.save(specs).await?;
        self.nodes[index].spec.send_replace(next);
        let info = self.nodes[index].handle.info();
        tracing::info!(node = %info.id, data_dir = %info.data_dir, "data folder chosen");
        self.publish();
        Ok(info)
    }

    fn begin_remove(&mut self, id: &NodeId, delete_data: bool, reply: Reply<u64>) {
        let index = match self.index_of(id) {
            Ok(index) if !self.closed => index,
            Ok(_) => {
                let _ = reply.send(Err(NodeError::Closed));
                return;
            }
            Err(err) => {
                let _ = reply.send(Err(err));
                return;
            }
        };
        if self.nodes.len() == 1 {
            let name = self.nodes[index].spec.borrow().name.clone();
            let _ = reply.send(Err(NodeError::LastNode(name)));
            return;
        }
        let runtime = &mut self.nodes[index];
        if runtime.removing {
            let _ = reply.send(Err(NodeError::Failed("the node is being removed".into())));
            return;
        }
        // No start may begin from here on and no configuration change be
        // applied: the permits go with the removal.
        let permits = runtime
            .handle
            .gates
            .block_starts()
            .and_then(|starts| Ok((starts, runtime.handle.gates.try_config_permit()?)));
        let permits = match permits {
            Ok(permits) => permits,
            Err(err) => {
                let _ = reply.send(Err(NodeError::Failed(err)));
                return;
            }
        };
        runtime.removing = true;
        let handle = runtime.handle.clone();
        let sizes = self.config.sizes.clone();
        let events = self.events_tx.clone();
        tokio::spawn(async move {
            let serial = handle.serial;
            let result = stop_and_delete(&handle, delete_data, &sizes).await;
            drop(permits);
            let _ = events
                .send(Event::Removed {
                    serial,
                    delete_data,
                    result,
                    reply,
                })
                .await;
        });
    }

    async fn finish_remove(
        &mut self,
        serial: u64,
        delete_data: bool,
        result: Result<u64, NodeError>,
        reply: Reply<u64>,
    ) {
        let Some(index) = self.nodes.iter().position(|r| r.handle.serial == serial) else {
            let _ = reply.send(Err(NodeError::Closed));
            return;
        };
        let freed = match result {
            Ok(freed) => freed,
            Err(err) => {
                // The node stays, stopped, with whatever is left of its data.
                self.nodes[index].removing = false;
                let _ = reply.send(Err(err));
                return;
            }
        };
        let mut specs = self.specs();
        let spec = specs.remove(index);
        if let Err(err) = self.save(specs).await {
            self.nodes[index].removing = false;
            let _ = reply.send(Err(err));
            return;
        }
        let runtime = self.nodes.remove(index);
        tracing::info!(node = %spec.id, delete_data, "removed a node");
        self.publish();
        // The supervisor closes the node's log file when it ends; only then
        // can the rest of its own folder go (Windows keeps open files).
        let folder = (delete_data && spec.layout == NodeLayout::Folder)
            .then(|| runtime.handle.paths.data_dir().to_path_buf())
            .filter(|folder| folder.starts_with(self.config.paths.nodes_dir()));
        tokio::spawn(async move {
            let _ = runtime.handle.supervisor.shutdown().await;
            if let Some(folder) = folder {
                remove_folder(folder).await;
            }
            let _ = reply.send(Ok(freed));
        });
    }

    /// Stops every node and ends its runtime, in a task; `reply` hears when
    /// all are done.
    fn shutdown_all(&mut self, reply: Option<oneshot::Sender<()>>) {
        self.closed = true;
        let nodes: Vec<NodeHandle> = self.nodes.iter().map(|r| r.handle.clone()).collect();
        tokio::spawn(async move {
            let mut shutdowns = JoinSet::new();
            for node in nodes {
                shutdowns.spawn(async move { node.supervisor.shutdown().await });
            }
            while shutdowns.join_next().await.is_some() {}
            if let Some(reply) = reply {
                let _ = reply.send(());
            }
        });
    }
}

/// Stops `node` and, with `delete_data`, deletes its chain state with
/// [`firstrun::delete_node_data`]'s rules: only a `state` folder inside the
/// launcher's data directory or the node's data folder.
async fn stop_and_delete(
    node: &NodeHandle,
    delete_data: bool,
    sizes: &Arc<SizeCache>,
) -> Result<u64, NodeError> {
    node.supervisor
        .stop()
        .await
        .map_err(|err| NodeError::Failed(err.to_string()))?;
    if !delete_data {
        return Ok(0);
    }
    let state = node.state_dir();
    let roots = node.data_roots();
    let sizes = sizes.clone();
    blocking(move || {
        let roots: Vec<&Path> = roots.iter().map(PathBuf::as_path).collect();
        firstrun::delete_node_data(&state, &roots, &sizes)
    })
    .await
    .map_err(|err| NodeError::Failed(format!("could not delete the node data: {err}")))
}

/// Removes a removed node's own folder, retrying for a moment while its log
/// file is still being closed. A failure is logged: the chain state is gone
/// already, and what is left is small.
async fn remove_folder(folder: PathBuf) {
    for attempt in 0..10 {
        let target = folder.clone();
        match blocking(move || match std::fs::remove_dir_all(&target) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        })
        .await
        {
            Ok(()) => return,
            Err(err) if attempt == 9 => {
                tracing::warn!(%err, folder = %folder.display(), "could not remove a removed node's folder");
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
        }
    }
}

/// Runs blocking file work off the async runtime.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> io::Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(io::Error::other)?
}

#[cfg(test)]
mod tests {
    use launcher_types::ExitClass;

    use super::*;

    #[test]
    fn the_herds_phase_is_running_while_any_node_is_and_halted_when_one_is() {
        let backoff = Phase::Backoff {
            until: std::time::SystemTime::UNIX_EPOCH,
            crashes: 1,
        };
        let halted = Phase::Failed {
            class: ExitClass::EosHalt,
        };
        let failed = Phase::Failed {
            class: ExitClass::ConfigError,
        };
        assert_eq!(herd_phase([]), Phase::Stopped);
        assert_eq!(herd_phase([&Phase::Stopped, &failed]), Phase::Stopped);
        assert_eq!(
            herd_phase([&Phase::Stopped, &Phase::Starting]),
            Phase::Running
        );
        assert_eq!(herd_phase([&backoff]), Phase::Running);
        assert_eq!(herd_phase([&Phase::Stopping]), Phase::Stopped);
        assert_eq!(herd_phase([&Phase::Running, &halted]), halted);
    }
}
