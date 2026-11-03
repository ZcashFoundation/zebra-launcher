//! `?herd`: five more nodes beside the mock's zebrad, for the overview and the node pages of a
//! launcher that runs several nodes (docs/MULTI-NODE.md), and the nodes the add-node dialog
//! adds. [`super::nodes`] routes the node-scoped API here for every node but the main one.
//!
//! The nodes are light simulations, not the zebrad mock: a status, statistics that move
//! every 5 s while running, a log line now and then, and for a zebrad node a `zebrad.toml` of
//! its own.
//!
//! | Node | State |
//! | --- | --- |
//! | Zebra · Testnet | running for hours, at the tip |
//! | Zakura · Testnet | running, pruned, at the tip |
//! | Zakura · Mainnet | running, catching up from a pruned snapshot |
//! | Zaino · Mainnet | running, indexing the mock's Zebra · Mainnet |
//! | zcashd · Mainnet | the compatibility build following Zebra · Mainnet, stopped two days ago |

use std::{
    cell::RefCell,
    sync::{
        Arc,
        atomic::{AtomicU32, AtomicU64, Ordering},
    },
    time::{Duration, SystemTime},
};

use launcher_types::{
    ApplyOutcome, ConfigDocument, DEFAULT_STOP_GRACE_SECS, ExitClass, ExitInfo, LogLevel, LogLine,
    LogStream, Network, NodeId, NodeInfo, NodeKind, NodePort, NodeProgram, NodeStats, Phase,
    PortRole, Status,
};
use leptos::prelude::*;

use super::{DEFAULT_DATA_DIR, FIRST_TIP, config::minimal_config, mock, nodes};
use crate::{clock, format};

/// The block interval after NU7, for the nodes at the tip.
const BLOCK_INTERVAL: Duration = Duration::from_secs(25);

/// How often the running nodes publish statistics, like the RPC monitor.
const STATS_INTERVAL: Duration = Duration::from_secs(5);

/// Testnet's tip in the mock: past its NU7 activation at 4,465,026.
const TESTNET_TIP: u64 = 4_471_230;

/// Where Zakura's pruned snapshot left it, and how fast it catches up.
const ZAKURA_SNAPSHOT_HEIGHT: u64 = 3_052_000;
const ZAKURA_BLOCKS_PER_TICK: u64 = 60;

/// One extra node: who it is and the signals its events come from.
#[derive(Clone)]
struct HerdNode {
    info: ArcRwSignal<NodeInfo>,
    /// A zebrad node's `zebrad.toml`, written the first time it is read.
    config: ArcRwSignal<Option<ConfigDocument>>,
    status: ArcRwSignal<Status>,
    stats: ArcRwSignal<Option<NodeStats>>,
    /// Lines waiting for the next log batch.
    pending: ArcRwSignal<Vec<LogLine>>,
    next_seq: Arc<AtomicU64>,
    height: Arc<AtomicU64>,
    next_pid: Arc<AtomicU32>,
    /// Bumped by every command, so a delayed transition from an older one is skipped.
    generation: Arc<AtomicU64>,
}

thread_local! {
    static HERD: RefCell<Vec<HerdNode>> = RefCell::new(herd_from_url());
}

fn herd_from_url() -> Vec<HerdNode> {
    let wanted = web_sys::window()
        .and_then(|window| window.location().search().ok())
        .is_some_and(|search| {
            search
                .trim_start_matches('?')
                .split('&')
                .any(|flag| flag == "herd")
        });
    if wanted { herd() } else { Vec::new() }
}

fn id(text: &str) -> NodeId {
    NodeId::new(text).expect("the mock's ids are valid")
}

fn ports(list: &[(PortRole, u16)]) -> Vec<NodePort> {
    list.iter()
        .map(|&(role, port)| NodePort { role, port })
        .collect()
}

fn info(
    slug: &str,
    kind: NodeKind,
    network: Network,
    depends_on: Option<&str>,
    port_list: &[(PortRole, u16)],
) -> NodeInfo {
    NodeInfo {
        id: id(slug),
        name: NodeInfo::default_name(kind, network),
        kind,
        network,
        depends_on: depends_on.map(id),
        data_dir: format!("/mnt/zcash/{slug}"),
        own_folder: Some(format!("{DEFAULT_DATA_DIR}/nodes/{slug}")),
        ports: ports(port_list),
        program: NodeProgram::Bundled,
        autostart: true,
        stop_grace_secs: DEFAULT_STOP_GRACE_SECS,
    }
}

/// The five nodes and their starting states.
fn herd() -> Vec<HerdNode> {
    let now = clock::now();
    let hours = |h: u64| now - Duration::from_secs(h * 3600);
    let nodes = vec![
        HerdNode::running(
            info(
                "zebrad-testnet",
                NodeKind::Zebrad,
                Network::Testnet,
                None,
                &[
                    (PortRole::P2p, 18233),
                    (PortRole::Rpc, 18232),
                    (PortRole::Metrics, 10_000),
                ],
            ),
            TESTNET_TIP,
            hours(3) - Duration::from_secs(12 * 60),
            4300,
        ),
        HerdNode::running(
            info(
                "zakura-testnet",
                NodeKind::Zakura,
                Network::Testnet,
                None,
                &[
                    (PortRole::P2p, 48233),
                    (PortRole::Rpc, 48232),
                    (PortRole::Metrics, 10_002),
                ],
            ),
            TESTNET_TIP,
            hours(26),
            4340,
        ),
        HerdNode::running(
            info(
                "zakura-mainnet",
                NodeKind::Zakura,
                Network::Mainnet,
                None,
                &[
                    (PortRole::P2p, 28233),
                    (PortRole::Rpc, 28232),
                    (PortRole::Metrics, 10_001),
                ],
            ),
            ZAKURA_SNAPSHOT_HEIGHT,
            now - Duration::from_secs(19 * 60),
            4310,
        ),
        HerdNode::running(
            info(
                "zaino-mainnet",
                NodeKind::Zaino,
                Network::Mainnet,
                Some("zebrad-mainnet"),
                &[(PortRole::Grpc, 8137), (PortRole::Rpc, 8237)],
            ),
            0,
            hours(3) - Duration::from_secs(10 * 60),
            4320,
        ),
        HerdNode::stopped(info(
            "zcashd-mainnet",
            NodeKind::Zcashd,
            Network::Mainnet,
            Some("zebrad-mainnet"),
            &[(PortRole::Rpc, 38232)],
        )),
    ];
    for node in &nodes {
        node.clone().simulate();
    }
    nodes
}

impl HerdNode {
    fn new(info: NodeInfo, height: u64, status: Status, pid: u32) -> Self {
        HerdNode {
            info: ArcRwSignal::new(info),
            config: ArcRwSignal::new(None),
            status: ArcRwSignal::new(status),
            stats: ArcRwSignal::new(None),
            pending: ArcRwSignal::new(Vec::new()),
            next_seq: Arc::new(AtomicU64::new(0)),
            height: Arc::new(AtomicU64::new(height)),
            next_pid: Arc::new(AtomicU32::new(pid)),
            generation: Arc::new(AtomicU64::new(0)),
        }
    }

    fn running(info: NodeInfo, height: u64, started_at: SystemTime, pid: u32) -> Self {
        let status = Status {
            phase: Phase::Running,
            pid: Some(pid),
            started_at: Some(started_at),
            ..Status::default()
        };
        HerdNode::new(info, height, status, pid + 1)
    }

    fn stopped(info: NodeInfo) -> Self {
        let exited_at = clock::now() - Duration::from_secs(2 * 24 * 3600 + 5 * 3600);
        let status = Status {
            phase: Phase::Stopped,
            last_exit: Some(ExitInfo {
                code: Some(0),
                class: ExitClass::Clean,
                detail: None,
                requested: true,
                at: exited_at,
            }),
            ..Status::default()
        };
        HerdNode::new(info, FIRST_TIP - 40_000, status, 4330)
    }

    fn phase(&self) -> Phase {
        self.status.with_untracked(|status| status.phase.clone())
    }

    fn id(&self) -> NodeId {
        self.info.with_untracked(|info| info.id.clone())
    }

    fn kind(&self) -> NodeKind {
        self.info.with_untracked(|info| info.kind)
    }

    fn network(&self) -> Network {
        self.info.with_untracked(|info| info.network)
    }

    /// Sends every change of the node's status and statistics to the UI's handlers.
    fn report(&self) {
        let (node, status) = (self.clone(), self.status.clone());
        Effect::new(move |_| nodes::emit_status(&node.id(), status.get()));
        let (node, stats) = (self.clone(), self.stats.clone());
        Effect::new(move |_| nodes::emit_stats(&node.id(), stats.get()));
    }

    /// The tip this node's network is at: the mock's Mainnet tip moves with the main node.
    fn tip(&self) -> u64 {
        match self.network() {
            Network::Testnet => TESTNET_TIP + blocks_since_load(),
            Network::Mainnet => mock()
                .stats
                .with_untracked(|stats| stats.as_ref().map(|stats| stats.estimated_height))
                .unwrap_or(FIRST_TIP),
        }
    }

    /// Statistics every 5 s and a log line now and then, while running.
    fn simulate(self) {
        self.report();
        let node = self.clone();
        set_interval(
            move || {
                if node.phase() != Phase::Running {
                    return;
                }
                node.tick();
            },
            STATS_INTERVAL,
        );
        self.tick();
    }

    fn tick(&self) {
        if self.phase() != Phase::Running {
            return;
        }
        let tip = self.tip();
        let height = match self.kind() {
            // Catching up from the snapshot.
            NodeKind::Zakura => {
                let next = self.height.load(Ordering::Relaxed) + ZAKURA_BLOCKS_PER_TICK;
                next.min(tip)
            }
            // An indexer follows the node it reads, a block or two behind.
            NodeKind::Zaino => mock()
                .stats
                .with_untracked(|stats| stats.as_ref().map(|stats| stats.height))
                .map_or(0, |height| height.saturating_sub(1)),
            _ => tip.saturating_sub(u64::from(random_below(2))),
        };
        self.height.store(height, Ordering::Relaxed);
        let indexer = self.kind() == NodeKind::Zaino;
        let stats = NodeStats {
            height,
            estimated_height: tip,
            verification_progress: if tip == 0 {
                0.0
            } else {
                height as f64 / tip as f64
            },
            peers: if indexer { 0 } else { 6 + random_below(5) },
            mempool_transactions: if indexer {
                0
            } else {
                u64::from(random_below(6))
            },
            chain: match self.network() {
                Network::Mainnet => "main".into(),
                Network::Testnet => "test".into(),
            },
            zebrad_version: version(self.kind()).into(),
            last_error: None,
            eos: None,
            updated_at: clock::now(),
        };
        self.stats.set(Some(stats));
        if random_below(3) == 0 {
            self.log(LogLevel::Info, &self.sample_message(height));
        }
    }

    fn sample_message(&self, height: u64) -> String {
        match self.kind() {
            NodeKind::Zaino => format!(
                "indexed block height={height} · serving lightwalletd gRPC on 127.0.0.1:8137"
            ),
            NodeKind::Zakura => format!(
                "estimated progress to chain tip sync_percent={:.3}% current_height=Height({height})",
                self.stats
                    .with_untracked(|stats| stats.as_ref().map(|s| s.verification_progress))
                    .unwrap_or(0.0)
                    * 100.0
            ),
            _ => format!("committed block to the non-finalized state height=Height({height})"),
        }
    }

    fn log(&self, level: LogLevel, message: &str) {
        let line = LogLine {
            seq: self.next_seq.fetch_add(1, Ordering::Relaxed),
            stream: LogStream::Stdout,
            timestamp: Some(clock::now_iso()),
            level: Some(level),
            spans: None,
            target: Some(target(self.kind()).into()),
            message: message.to_owned(),
            dropped: None,
        };
        self.pending.update(|pending| pending.push(line));
    }

    fn note(&self, message: &str) {
        let line = LogLine {
            seq: self.next_seq.fetch_add(1, Ordering::Relaxed),
            stream: LogStream::Launcher,
            timestamp: Some(clock::now_iso()),
            level: Some(LogLevel::Info),
            spans: None,
            target: Some("launcher_core::supervisor".into()),
            message: message.to_owned(),
            dropped: None,
        };
        self.pending.update(|pending| pending.push(line));
    }

    async fn start(self) {
        if !matches!(self.phase(), Phase::Stopped | Phase::Failed { .. }) {
            return;
        }
        let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let pid = self.next_pid.fetch_add(1, Ordering::Relaxed);
        self.status.update(|status| {
            status.phase = Phase::Starting;
            status.pid = Some(pid);
            status.started_at = Some(clock::now());
        });
        self.note(&format!("{} started (pid {pid})", program(self.kind())));
        super::sleep(1500).await;
        if self.generation.load(Ordering::Relaxed) == generation {
            self.status.update(|status| status.phase = Phase::Running);
            self.tick();
        }
    }

    async fn stop(self) {
        if !matches!(self.phase(), Phase::Starting | Phase::Running) {
            return;
        }
        let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        self.status.update(|status| status.phase = Phase::Stopping);
        self.note("stop requested");
        super::sleep(1200).await;
        if self.generation.load(Ordering::Relaxed) == generation {
            self.status.update(|status| {
                status.phase = Phase::Stopped;
                status.pid = None;
                // As the supervisor does: no start time or listeners once it exited.
                status.started_at = None;
                status.rpc_addr = None;
                status.metrics_addr = None;
                status.last_exit = Some(ExitInfo {
                    code: Some(0),
                    class: ExitClass::Clean,
                    detail: None,
                    requested: true,
                    at: clock::now(),
                });
            });
            self.stats.set(None);
            self.note(&format!(
                "{} exited (exit code 0, Clean, stop requested)",
                program(self.kind())
            ));
        }
    }
}

fn program(kind: NodeKind) -> &'static str {
    kind.program().unwrap_or("program")
}

fn target(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Zaino => "zainod::indexer",
        NodeKind::Zakura => "zakurad::components::sync::progress",
        _ => "zebra_state::service::non_finalized_state",
    }
}

fn version(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Zebrad => "7.0.0",
        NodeKind::Zakura => "1.8.0",
        NodeKind::Zcashd => "zebra-compat 1.3.0",
        NodeKind::Zaino => "0.10.2",
        _ => "0.1.0",
    }
}

/// Blocks Testnet has gained since the page loaded, one per [`BLOCK_INTERVAL`].
fn blocks_since_load() -> u64 {
    thread_local! {
        static LOADED: SystemTime = clock::now();
    }
    let loaded = LOADED.with(|loaded| *loaded);
    clock::now()
        .duration_since(loaded)
        .map_or(0, |since| since.as_secs() / BLOCK_INTERVAL.as_secs())
}

fn random_below(n: u32) -> u32 {
    (js_sys::Math::random() * f64::from(n)) as u32
}

fn find(node: &NodeId) -> Option<HerdNode> {
    HERD.with_borrow(|herd| {
        herd.iter()
            .find(|candidate| &candidate.id() == node)
            .cloned()
    })
}

/// The extra nodes, in the order they were added.
pub fn infos() -> Vec<NodeInfo> {
    HERD.with_borrow(|herd| herd.iter().map(|node| node.info.get_untracked()).collect())
}

/// Adds a node, stopped, as the add-node dialog does. Its simulation starts with it.
pub fn add_node(info: NodeInfo) {
    let pid = 4400 + 10 * HERD.with_borrow(|herd| herd.len() as u32);
    let node = HerdNode::new(info, FIRST_TIP - 120_000, Status::default(), pid);
    node.clone().simulate();
    HERD.with_borrow_mut(|herd| herd.push(node));
}

/// Takes a node off the list, once stopped; `delete_data` frees its chain. The bytes freed.
pub fn remove(node: &NodeId, delete_data: bool) -> Option<u64> {
    let removed = HERD.with_borrow_mut(|herd| {
        let index = herd.iter().position(|candidate| &candidate.id() == node)?;
        Some(herd.remove(index))
    })?;
    // The simulation stops with the node: it ticks only while running.
    removed.generation.fetch_add(1, Ordering::Relaxed);
    Some(if delete_data {
        removed.height.load(Ordering::Relaxed) * 50_000
    } else {
        0
    })
}

/// Replaces a node's info, as `update_node` does.
pub fn set_info(node: &NodeId, info: NodeInfo) {
    if let Some(found) = find(node) {
        found.info.set(info);
    }
}

pub fn status(node: &NodeId) -> Option<Status> {
    find(node).map(|node| node.status.get_untracked())
}

pub fn stats(node: &NodeId) -> Option<Option<NodeStats>> {
    find(node).map(|node| node.stats.get_untracked())
}

/// Sends `node`'s log lines in batches, as the bridge does.
pub fn subscribe_node_logs(node: &NodeId, on_batch: impl Fn(Vec<LogLine>) + 'static) {
    if let Some(node) = find(node) {
        set_interval(
            move || {
                let batch = node.pending.try_update(std::mem::take).unwrap_or_default();
                if !batch.is_empty() {
                    on_batch(batch);
                }
            },
            Duration::from_millis(400),
        );
    }
}

pub async fn start_node(node: &NodeId) -> Option<()> {
    find(node)?.start().await;
    Some(())
}

pub async fn stop_node(node: &NodeId) -> Option<()> {
    find(node)?.stop().await;
    Some(())
}

pub async fn restart_node(node: &NodeId) -> Option<()> {
    let node = find(node)?;
    node.clone().stop().await;
    node.start().await;
    Some(())
}

/// A zebrad node's `zebrad.toml`: the minimal managed file for its network, ports and folder
/// until it is saved.
pub fn read_config(node: &NodeId) -> Result<ConfigDocument, String> {
    let found = find(node).ok_or_else(|| format!("there is no node {node}"))?;
    let info = found.info.get_untracked();
    if info.kind != NodeKind::Zebrad {
        return Err(format!("(mock) {} has no zebrad.toml", info.name));
    }
    if found.config.with_untracked(Option::is_none) {
        found.config.set(Some(fresh_config(&info)));
    }
    Ok(found
        .config
        .get_untracked()
        .unwrap_or_else(|| fresh_config(&info)))
}

fn fresh_config(info: &NodeInfo) -> ConfigDocument {
    ConfigDocument {
        text: minimal_config(info, &format!("{}/rpc", info.data_dir)),
        token: format!("mock-{}-{}", info.id, js_sys::Date::now()),
        path: format!("{}/zebrad.toml", info.data_dir),
    }
}

/// Saves a zebrad node's `zebrad.toml` and restarts the node if it runs. A herd node accepts
/// any text; only the main node simulates a rollback.
pub async fn apply_config(
    node: &NodeId,
    text: String,
    token: String,
) -> Result<ApplyOutcome, String> {
    let current = read_config(node)?;
    if token != current.token {
        return Err(format!(
            "{} changed on disk since it was read",
            current.path
        ));
    }
    let found = find(node).ok_or_else(|| format!("there is no node {node}"))?;
    found.config.set(Some(ConfigDocument {
        text,
        token: format!("mock-{node}-{}", js_sys::Date::now()),
        path: current.path,
    }));
    if matches!(found.phase(), Phase::Starting | Phase::Running) {
        found.clone().stop().await;
        found.start().await;
    }
    Ok(ApplyOutcome::Applied)
}

/// Replaces a zebrad node's `zebrad.toml` with the minimal file, while it is stopped.
pub fn reset_config(node: &NodeId) -> Result<ConfigDocument, String> {
    let found = find(node).ok_or_else(|| format!("there is no node {node}"))?;
    if !format::data_change_allowed(&found.phase()) {
        return Err("stop zebrad first".into());
    }
    let info = found.info.get_untracked();
    let fresh = fresh_config(&info);
    found.config.set(Some(fresh.clone()));
    Ok(fresh)
}

/// Deletes a node's chain state, while it is stopped: the bytes freed.
pub fn delete_data(node: &NodeId) -> Result<u64, String> {
    let found = find(node).ok_or_else(|| format!("there is no node {node}"))?;
    if !format::data_change_allowed(&found.phase()) {
        return Err("stop zebrad first".into());
    }
    Ok(found.height.swap(0, Ordering::Relaxed) * 50_000)
}
