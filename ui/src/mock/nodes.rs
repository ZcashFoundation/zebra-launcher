//! The mock's node-scoped API, the same as the backend's: every node command and event takes a
//! node's id, routed to the mock's main zebrad (the full simulation, the first node) or to a
//! node of `?herd` or one the add-node dialog added. Adding, changing and removing nodes keep
//! the backend's rules (`launcher_core::nodes`), and every node's changes go to the handlers the
//! UI registered, also those of nodes added later. The main node can be removed too, as long as
//! another node stays; its commands then find no node.

use std::{cell::RefCell, rc::Rc, sync::atomic::Ordering};

use launcher_types::{
    ApplyError, ApplyErrorKind, ApplyOutcome, ConfigDocument, DEFAULT_STOP_GRACE_SECS, LogLine,
    MAX_NODES, MetricsSnapshot, NodeDataDirChoice, NodeDetails, NodeDraft, NodeEvent, NodeId,
    NodeInfo, NodeKind, NodeProgram, NodeResources, NodeStats, NodeUpdate, STOP_GRACE_SECS, Status,
    node_ports,
};
use leptos::prelude::*;

use super::{DEFAULT_DATA_DIR, herd, mock, sleep};
use crate::{components::zebrad_channel::channel_available, format};

type Handlers<T> = RefCell<Vec<Rc<dyn Fn(T)>>>;

thread_local! {
    static NODES: Handlers<Vec<NodeInfo>> = RefCell::new(Vec::new());
    static STATUS: Handlers<NodeEvent<Status>> = RefCell::new(Vec::new());
    static STATS: Handlers<NodeEvent<Option<NodeStats>>> = RefCell::new(Vec::new());
    static DETAILS: Handlers<NodeEvent<Option<NodeDetails>>> = RefCell::new(Vec::new());
    static METRICS: Handlers<NodeEvent<Option<MetricsSnapshot>>> = RefCell::new(Vec::new());
    static RESOURCES: Handlers<NodeEvent<Option<NodeResources>>> = RefCell::new(Vec::new());
    /// Whether the main node's signals already feed the handlers.
    static WATCHING_MAIN: RefCell<bool> = const { RefCell::new(false) };
    /// The ids of removed nodes whose data was kept: like the backend, a new node does not get
    /// one while its folder is there.
    static KEPT: RefCell<Vec<NodeId>> = const { RefCell::new(Vec::new()) };
    /// The main node was removed.
    static MAIN_REMOVED: RefCell<bool> = const { RefCell::new(false) };
}

fn main_removed() -> bool {
    MAIN_REMOVED.with_borrow(|removed| *removed)
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

/// Feeds the main node's status, statistics, details, metrics and resources to the handlers,
/// once.
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
    let main = id.clone();
    Effect::new(move |_| {
        emit(
            &METRICS,
            NodeEvent {
                node: main(),
                value: metrics.get(),
            },
        )
    });
    let resources = mock.resources.clone();
    Effect::new(move |_| emit_resources(&id(), resources.get()));
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

pub(super) fn emit_resources(node: &NodeId, sample: Option<NodeResources>) {
    emit(
        &RESOURCES,
        NodeEvent {
            node: node.clone(),
            value: sample,
        },
    );
}

/// Sends the node list to the `nodes` handlers, as the backend does after every change.
pub(super) fn emit_nodes() {
    emit(&NODES, list());
}

/// The nodes, the main one first while it is there.
fn list() -> Vec<NodeInfo> {
    let main = (!main_removed()).then(|| mock().info.get_untracked());
    main.into_iter().chain(herd::infos()).collect()
}

/// Whether `node` is the main node, while it is there.
fn is_main(node: &NodeId) -> bool {
    !main_removed() && mock().info.with_untracked(|info| &info.id == node)
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

pub fn on_node_resources(handler: impl Fn(NodeEvent<Option<NodeResources>>) + 'static) {
    register(&RESOURCES, handler);
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

pub async fn get_node_resources(node: &NodeId) -> Result<Option<NodeResources>, String> {
    if is_main(node) {
        return Ok(super::resources::get_node_resources().await);
    }
    sleep(50).await;
    herd::resources(node).ok_or_else(|| not_found(node))
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

/// The backend's bundled-program guard, plus the browser mock's explicit unavailable-Beta state.
fn check_program(program: NodeProgram, beta_unavailable: bool) -> Result<(), String> {
    let channel = program.channel();
    if channel_available(channel, beta_unavailable) {
        Ok(())
    } else {
        Err(format!(
            "the bundled {} channel is not available",
            channel.as_str()
        ))
    }
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
    check_program(draft.program, mock().flags.beta_unavailable)?;
    let name = check_name(&draft.name, &nodes)?;
    let mut taken: Vec<NodeId> = nodes.iter().map(|node| node.id.clone()).collect();
    taken.extend(KEPT.with_borrow(Clone::clone));
    let id = match draft.id {
        Some(id) if taken.contains(&id) => return Err(format!("another node has the id {id}")),
        Some(id) => id,
        None => NodeId::suggest(draft.kind, draft.network, &taken),
    };
    let ports = draft
        .ports
        .unwrap_or_else(|| node_ports(draft.kind, draft.network, &taken_ports(&nodes)));
    if let Some(dir) = &draft.data_dir {
        if !dir.starts_with('/') {
            return Err(format!("the data folder {dir:?} is not an absolute path"));
        }
        if let Some(holder) = nodes.iter().find(|node| &node.data_dir == dir) {
            return Err(format!(
                "{dir} is {}'s data folder, or overlaps it",
                holder.name
            ));
        }
    }
    let own_folder = format!("{DEFAULT_DATA_DIR}/nodes/{id}");
    let info = NodeInfo {
        data_dir: draft.data_dir.unwrap_or_else(|| own_folder.clone()),
        own_folder: Some(own_folder),
        id,
        name,
        kind: draft.kind,
        network: draft.network,
        depends_on: None,
        ports,
        program: draft.program,
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
        next.ports = node_ports(current.kind, network, &taken_ports(&others));
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

pub async fn remove_node(node: &NodeId, delete_data: bool) -> Result<u64, String> {
    sleep(300).await;
    if let [only] = list().as_slice() {
        return Err(format!(
            "Zeal needs a node: add another before removing {}",
            only.name
        ));
    }
    if is_main(node) {
        return remove_main(delete_data).await;
    }
    let name = herd::infos()
        .into_iter()
        .find(|info| &info.id == node)
        .map(|info| info.name)
        .ok_or_else(|| not_found(node))?;
    herd::stop_node(node).await.ok_or_else(|| not_found(node))?;
    sleep(800).await;
    let freed = herd::remove(node, delete_data).ok_or_else(|| not_found(node))?;
    // `?leftover`: the node leaves the list, but its folder stays, which keeps its id.
    let leftover = delete_data && mock().flags.leftover;
    if !delete_data || leftover {
        KEPT.with_borrow_mut(|kept| kept.push(node.clone()));
    }
    emit_nodes();
    if leftover {
        return Err(format!(
            "{name} was removed, but {DEFAULT_DATA_DIR}/nodes/{node} could not be deleted: (mock) permission denied"
        ));
    }
    Ok(freed)
}

/// Removes the main node, as the backend removes the first node: stopped first, refused while a
/// zebrad from an earlier launcher run is still running (`?stale`), and with its data deleted
/// its chain state goes while the launcher's own files stay. Its id is free again, as its files
/// are not a folder of its own (`?leftover` leaves its chain state behind).
async fn remove_main(delete_data: bool) -> Result<u64, String> {
    let mock = mock();
    if let Some(stale) = mock.status.with_untracked(|status| status.stale.clone()) {
        return Err(format!(
            "a zebrad from an earlier launcher run is still running (pid {}): stop it, or start \
             the node and let Zeal stop it, then remove the node",
            stale.pid
        ));
    }
    if !format::data_change_allowed(&mock.phase()) {
        mock.stop(false);
        sleep(1600).await;
    }
    let name = mock.info.with_untracked(|info| info.name.clone());
    MAIN_REMOVED.with_borrow_mut(|removed| *removed = true);
    emit_nodes();
    if !delete_data {
        return Ok(0);
    }
    if mock.flags.leftover {
        return Err(format!(
            "{name} was removed, but {} could not be deleted: (mock) permission denied",
            mock.state_dir()
        ));
    }
    Ok(mock.state_bytes.swap(0, Ordering::Relaxed))
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
        own_folder: None,
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

    #[test]
    fn add_node_checks_pins_and_the_mock_beta_override() {
        for channel in launcher_types::ZebradChannel::ALL {
            let program = NodeProgram::from_channel(channel);
            assert_eq!(
                check_program(program, false).is_ok(),
                channel.is_available(),
                "the normal mock must follow the {channel:?} pin"
            );
        }
        assert_eq!(
            check_program(NodeProgram::BundledBeta, true),
            Err("the bundled beta channel is not available".to_owned())
        );
        assert_eq!(check_program(NodeProgram::Bundled, true), Ok(()));
        assert_eq!(check_program(NodeProgram::BundledNightly, true), Ok(()));
    }
}
