//! The mock's node-scoped API, the same as the backend's: every node command and event takes a
//! node's id, routed to the mock's main zebrad (the full simulation, the first node) or to a
//! node of `?herd` or one the add-node dialog added. Adding, changing and removing nodes keep
//! the backend's rules (`launcher_core::nodes`), and every node's changes go to the handlers the
//! UI registered, also those of nodes added later.

use std::{cell::RefCell, rc::Rc};

use launcher_types::{
    ApplyError, ApplyErrorKind, ApplyOutcome, ConfigDocument, DEFAULT_STOP_GRACE_SECS, LogLine,
    MAX_NODES, MetricsSnapshot, NodeDataDirChoice, NodeDetails, NodeDraft, NodeEvent, NodeId,
    NodeInfo, NodeKind, NodeProgram, NodeStats, NodeUpdate, STOP_GRACE_SECS, Status, assign_ports,
};
use leptos::prelude::*;

use super::{DEFAULT_DATA_DIR, herd, mock, sleep};

type Handlers<T> = RefCell<Vec<Rc<dyn Fn(T)>>>;

thread_local! {
    static NODES: Handlers<Vec<NodeInfo>> = RefCell::new(Vec::new());
    static STATUS: Handlers<NodeEvent<Status>> = RefCell::new(Vec::new());
    static STATS: Handlers<NodeEvent<Option<NodeStats>>> = RefCell::new(Vec::new());
    static DETAILS: Handlers<NodeEvent<Option<NodeDetails>>> = RefCell::new(Vec::new());
    static METRICS: Handlers<NodeEvent<Option<MetricsSnapshot>>> = RefCell::new(Vec::new());
    /// Whether the main node's signals already feed the handlers.
    static WATCHING_MAIN: RefCell<bool> = const { RefCell::new(false) };
    /// The ids of removed nodes whose data was kept: like the backend, a new node does not get
    /// one while its folder is there.
    static KEPT: RefCell<Vec<NodeId>> = const { RefCell::new(Vec::new()) };
}

/// Calls every handler in `list` with `value`. The handlers are copied out first, so one may
/// register another.
fn emit<T: Clone>(list: &'static std::thread::LocalKey<Handlers<T>>, value: T) {
    let handlers: Vec<Rc<dyn Fn(T)>> = list.with_borrow(Clone::clone);
    for handler in handlers {
        handler(value.clone());
    }
}

fn register<T>(list: &'static std::thread::LocalKey<Handlers<T>>, handler: impl Fn(T) + 'static) {
    list.with_borrow_mut(|handlers| handlers.push(Rc::new(handler)));
    watch_main();
}

/// Feeds the main node's status, statistics, details and metrics to the handlers, once.
fn watch_main() {
    if WATCHING_MAIN.with_borrow_mut(|watching| std::mem::replace(watching, true)) {
        return;
    }
    let mock = mock();
    let info = mock.info.clone();
    let id = move || info.with_untracked(|info| info.id.clone());
    let status = mock.status.clone();
    let main = id.clone();
    Effect::new(move |_| emit_status(&main(), status.get()));
    let stats = mock.stats.clone();
    let main = id.clone();
    Effect::new(move |_| emit_stats(&main(), stats.get()));
    let details = mock.details.clone();
    let main = id.clone();
    Effect::new(move |_| {
        emit(
            &DETAILS,
            NodeEvent {
                node: main(),
                value: details.get(),
            },
        )
    });
    let metrics = mock.metrics.clone();
    Effect::new(move |_| {
        emit(
            &METRICS,
            NodeEvent {
                node: id(),
                value: metrics.get(),
            },
        )
    });
}

pub(super) fn emit_status(node: &NodeId, status: Status) {
    emit(
        &STATUS,
        NodeEvent {
            node: node.clone(),
            value: status,
        },
    );
}

pub(super) fn emit_stats(node: &NodeId, stats: Option<NodeStats>) {
    emit(
        &STATS,
        NodeEvent {
            node: node.clone(),
            value: stats,
        },
    );
}

/// Sends the node list to the `nodes` handlers, as the backend does after every change.
pub(super) fn emit_nodes() {
    emit(&NODES, list());
}

/// The nodes, the main one first.
fn list() -> Vec<NodeInfo> {
    std::iter::once(mock().info.get_untracked())
        .chain(herd::infos())
        .collect()
}

fn is_main(node: &NodeId) -> bool {
    mock().info.with_untracked(|info| &info.id == node)
}

fn not_found(node: &NodeId) -> String {
    format!("there is no node {node}")
}

pub async fn list_nodes() -> Vec<NodeInfo> {
    sleep(50).await;
    list()
}

pub fn on_nodes(handler: impl Fn(Vec<NodeInfo>) + 'static) {
    register(&NODES, handler);
}

pub fn on_status(handler: impl Fn(NodeEvent<Status>) + 'static) {
    register(&STATUS, handler);
}

pub fn on_node_stats(handler: impl Fn(NodeEvent<Option<NodeStats>>) + 'static) {
    register(&STATS, handler);
}

pub fn on_node_details(handler: impl Fn(NodeEvent<Option<NodeDetails>>) + 'static) {
    register(&DETAILS, handler);
}

pub fn on_metrics(handler: impl Fn(NodeEvent<Option<MetricsSnapshot>>) + 'static) {
    register(&METRICS, handler);
}

pub async fn get_status(node: &NodeId) -> Result<Status, String> {
    if is_main(node) {
        return Ok(super::get_status().await);
    }
    sleep(50).await;
    herd::status(node).ok_or_else(|| not_found(node))
}

pub async fn get_node_stats(node: &NodeId) -> Result<Option<NodeStats>, String> {
    if is_main(node) {
        return Ok(super::get_node_stats().await);
    }
    sleep(50).await;
    herd::stats(node).ok_or_else(|| not_found(node))
}

/// The herd's nodes are light simulations: no details and no metrics page.
pub async fn get_node_details(node: &NodeId) -> Result<Option<NodeDetails>, String> {
    if is_main(node) {
        return Ok(super::get_node_details().await);
    }
    sleep(50).await;
    herd::status(node)
        .map(|_| None)
        .ok_or_else(|| not_found(node))
}

pub async fn get_metrics(node: &NodeId) -> Result<Option<MetricsSnapshot>, String> {
    if is_main(node) {
        return Ok(super::metrics::get_metrics().await);
    }
    sleep(50).await;
    herd::status(node)
        .map(|_| None)
        .ok_or_else(|| not_found(node))
}

pub fn subscribe_logs(node: &NodeId, on_batch: impl Fn(Vec<LogLine>) + 'static) {
    if is_main(node) {
        super::subscribe_logs(on_batch);
    } else {
        herd::subscribe_node_logs(node, on_batch);
    }
}

pub async fn start_node(node: &NodeId) -> Result<(), String> {
    if is_main(node) {
        return super::start_zebrad().await;
    }
    herd::start_node(node).await.ok_or_else(|| not_found(node))
}

pub async fn start_node_killing_stale(node: &NodeId) -> Result<(), String> {
    if is_main(node) {
        super::start_zebrad_killing_stale().await;
        return Ok(());
    }
    herd::start_node(node).await.ok_or_else(|| not_found(node))
}

pub async fn stop_node(node: &NodeId) -> Result<(), String> {
    if is_main(node) {
        super::stop_zebrad().await;
        return Ok(());
    }
    herd::stop_node(node).await.ok_or_else(|| not_found(node))
}

pub async fn restart_node(node: &NodeId) -> Result<(), String> {
    if is_main(node) {
        super::restart_zebrad().await;
        return Ok(());
    }
    herd::restart_node(node)
        .await
        .ok_or_else(|| not_found(node))
}

pub async fn read_config(node: &NodeId) -> Result<ConfigDocument, String> {
    if is_main(node) {
        return super::config::read_config().await;
    }
    sleep(150).await;
    herd::read_config(node)
}

pub async fn apply_config(
    node: &NodeId,
    text: String,
    token: String,
) -> Result<ApplyOutcome, ApplyError> {
    if is_main(node) {
        return super::config::apply_config(text, token).await;
    }
    sleep(300).await;
    herd::apply_config(node, text, token)
        .await
        .map_err(|message| ApplyError {
            kind: ApplyErrorKind::Conflict,
            message,
        })
}

pub async fn reset_config(node: &NodeId) -> Result<ConfigDocument, String> {
    if is_main(node) {
        return super::config::reset_config().await;
    }
    sleep(300).await;
    herd::reset_config(node)
}

pub async fn choose_data_dir(node: &NodeId, path: String) -> Result<NodeDataDirChoice, String> {
    if is_main(node) {
        let choice = super::choose_data_dir(path).await?;
        emit_nodes();
        return Ok(choice);
    }
    Err("(mock) only the main node's data folder can be changed".into())
}

pub async fn delete_node_data(node: &NodeId) -> Result<u64, String> {
    if is_main(node) {
        return super::delete_node_data().await;
    }
    sleep(700).await;
    herd::delete_data(node)
}

/// The rules for a name, as the backend keeps them: not empty, at most
/// [`NodeInfo::MAX_NAME_LEN`] characters, no other node's (ignoring case).
fn check_name(name: &str, others: &[NodeInfo]) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("give the node a name".into());
    }
    if name.chars().count() > NodeInfo::MAX_NAME_LEN {
        return Err(format!(
            "the name is longer than {} characters",
            NodeInfo::MAX_NAME_LEN
        ));
    }
    if others
        .iter()
        .any(|other| other.name.trim().to_lowercase() == name.to_lowercase())
    {
        return Err(format!("another node is called {name:?}"));
    }
    Ok(name.to_owned())
}

fn taken_ports(nodes: &[NodeInfo]) -> Vec<u16> {
    nodes
        .iter()
        .flat_map(|node| node.ports.iter().map(|port| port.port))
        .collect()
}

pub async fn add_node(draft: NodeDraft) -> Result<NodeInfo, String> {
    sleep(400).await;
    let nodes = list();
    if nodes.len() >= MAX_NODES {
        return Err(format!("Zeal runs at most {MAX_NODES} nodes"));
    }
    if !draft.kind.addable() {
        return Err(format!("{} nodes come in a later release", draft.kind));
    }
    let name = check_name(&draft.name, &nodes)?;
    let mut taken: Vec<NodeId> = nodes.iter().map(|node| node.id.clone()).collect();
    taken.extend(KEPT.with_borrow(Clone::clone));
    let id = match draft.id {
        Some(id) if taken.contains(&id) => return Err(format!("another node has the id {id}")),
        Some(id) => id,
        None => NodeId::suggest(draft.kind, draft.network, &taken),
    };
    let ports = draft.ports.unwrap_or_else(|| {
        assign_ports(
            &draft.kind.default_ports(draft.network),
            &taken_ports(&nodes),
        )
    });
    let info = NodeInfo {
        data_dir: draft
            .data_dir
            .unwrap_or_else(|| format!("{DEFAULT_DATA_DIR}/nodes/{id}")),
        id,
        name,
        kind: draft.kind,
        network: draft.network,
        depends_on: None,
        ports,
        program: NodeProgram::Bundled,
        autostart: draft.autostart,
        stop_grace_secs: DEFAULT_STOP_GRACE_SECS,
    };
    herd::add_node(info.clone());
    emit_nodes();
    Ok(info)
}

pub async fn update_node(node: &NodeId, update: NodeUpdate) -> Result<NodeInfo, String> {
    sleep(200).await;
    let nodes = list();
    let current = nodes
        .iter()
        .find(|info| &info.id == node)
        .cloned()
        .ok_or_else(|| not_found(node))?;
    let others: Vec<NodeInfo> = nodes.into_iter().filter(|info| &info.id != node).collect();
    let mut next = current.clone();
    if let Some(name) = &update.name {
        next.name = check_name(name, &others)?;
    }
    if let Some(autostart) = update.autostart {
        next.autostart = autostart;
    }
    if let Some(secs) = update.stop_grace_secs {
        if !STOP_GRACE_SECS.contains(&secs) {
            return Err(format!(
                "the stop grace period of {secs} s must be between {} and {} s",
                STOP_GRACE_SECS.start(),
                STOP_GRACE_SECS.end()
            ));
        }
        next.stop_grace_secs = secs;
    }
    if let Some(network) = update.network
        && network != current.network
    {
        next.network = network;
        next.ports = assign_ports(&current.kind.default_ports(network), &taken_ports(&others));
        if update.name.is_none()
            && NodeInfo::is_default_name(&current.name, current.kind, current.network)
        {
            let names: Vec<&str> = others.iter().map(|other| other.name.as_str()).collect();
            next.name = NodeInfo::unique_default_name(current.kind, network, names);
        }
        // The first run gives the first node its network's id.
        let first_run = !mock()
            .settings
            .with_untracked(|settings| settings.first_run_done);
        if first_run && is_main(node) {
            let ids: Vec<NodeId> = others.iter().map(|other| other.id.clone()).collect();
            next.id = NodeId::suggest(current.kind, network, &ids);
        }
    }
    if is_main(node) {
        mock().info.set(next.clone());
    } else {
        herd::set_info(node, next.clone());
    }
    emit_nodes();
    Ok(next)
}

/// The main node at first: Zebra on Mainnet in the launcher's own folders, with zebrad's
/// default ports.
pub(super) fn first_node(data_dir: &str) -> NodeInfo {
    let (kind, network) = (NodeKind::Zebrad, launcher_types::Network::Mainnet);
    NodeInfo {
        id: NodeId::suggest(kind, network, &Vec::new()),
        name: NodeInfo::default_name(kind, network),
        kind,
        network,
        depends_on: None,
        data_dir: data_dir.to_owned(),
        ports: kind.default_ports(network),
        program: NodeProgram::Bundled,
        autostart: true,
        stop_grace_secs: DEFAULT_STOP_GRACE_SECS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_keep_the_backends_rules() {
        let first = first_node(DEFAULT_DATA_DIR);
        assert!(check_name("  ", &[]).is_err());
        assert!(check_name("zebra · mainnet", std::slice::from_ref(&first)).is_err());
        assert_eq!(
            check_name("  Archive ", std::slice::from_ref(&first)),
            Ok("Archive".to_owned())
        );
    }
}
