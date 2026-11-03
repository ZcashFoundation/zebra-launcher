//! The nodes Zeal runs, as `launcher.json` keeps them: what each one is, where
//! its files are and which ports it has. The registry (`crate::registry`)
//! runs them; this module is the plain data and the rules about it.

use std::{
    net::{Ipv4Addr, SocketAddr},
    path::PathBuf,
};

use launcher_types::{
    DEFAULT_STOP_GRACE_SECS, MAX_NODES, Network, NodeDraft, NodeId, NodeInfo, NodeKind, NodePort,
    NodeProgram, NodeUpdate, PortRole, STOP_GRACE_SECS, node_ports,
};
use serde::{Deserialize, Serialize};

use crate::{
    config::ManagedKeys,
    paths::{NodePaths, Paths},
};

/// Where a node keeps its files.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeLayout {
    /// In the launcher's own directories, where the single zebrad has always
    /// kept them ([`Paths::launcher_node`]): the first node, migrated from a
    /// single-node launcher or created at the first run.
    Launcher,
    /// In a folder of its own, `<data>/nodes/<id>` ([`Paths::node_folder`]).
    #[default]
    Folder,
}

/// A node as `launcher.json` stores it. [`NodeSpec::info`] is what the UI
/// sees of it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeSpec {
    pub id: NodeId,
    /// The name people see.
    pub name: String,
    pub kind: NodeKind,
    pub network: Network,
    #[serde(default)]
    pub program: NodeProgram,
    /// The data folder the user chose, whose `state` folder holds the chain;
    /// `None`: the layout's own ([`NodePaths::data_dir`]), so the folder
    /// follows the launcher's data directory if that ever moves.
    #[serde(default)]
    pub data_dir: Option<String>,
    /// The ports it listens on, one per role.
    pub ports: Vec<NodePort>,
    /// Start it when Zeal opens, once the first run is done.
    #[serde(default = "autostart_default")]
    pub autostart: bool,
    /// How long a requested stop may take before it is killed, in seconds.
    #[serde(default = "stop_grace_default")]
    pub stop_grace_secs: u64,
    /// For a service, the node it uses. Always `None` so far.
    #[serde(default)]
    pub depends_on: Option<NodeId>,
    pub layout: NodeLayout,
}

fn autostart_default() -> bool {
    true
}

fn stop_grace_default() -> u64 {
    DEFAULT_STOP_GRACE_SECS
}

impl NodeSpec {
    /// The first node: the bundled zebrad on `network`, in the launcher's own
    /// directories, with the network's default ports. A fresh install starts
    /// with it, and a single-node launcher's settings become it.
    pub fn first(network: Network) -> Self {
        let kind = NodeKind::Zebrad;
        NodeSpec {
            id: NodeId::suggest(kind, network, &Vec::new()),
            name: NodeInfo::default_name(kind, network),
            kind,
            network,
            program: NodeProgram::Bundled,
            data_dir: None,
            ports: kind.default_ports(network),
            autostart: true,
            stop_grace_secs: DEFAULT_STOP_GRACE_SECS,
            depends_on: None,
            layout: NodeLayout::Launcher,
        }
    }

    /// Where the node's own files are: `zebrad.toml`, pid file, cookie, log.
    pub fn paths(&self, launcher: &Paths) -> NodePaths {
        match self.layout {
            NodeLayout::Launcher => launcher.launcher_node(),
            NodeLayout::Folder => launcher.node_folder(&self.id),
        }
    }

    /// The node's data folder: the one the user chose, or its layout's.
    pub fn data_dir(&self, launcher: &Paths) -> PathBuf {
        match &self.data_dir {
            Some(dir) => PathBuf::from(dir),
            None => self.paths(launcher).data_dir().to_path_buf(),
        }
    }

    /// The node's chain state, `state.cache_dir`: the `state` folder in its
    /// data folder.
    pub fn state_dir(&self, launcher: &Paths) -> PathBuf {
        self.data_dir(launcher).join("state")
    }

    /// The node's port for `role`, if it has one.
    pub fn port(&self, role: PortRole) -> Option<u16> {
        self.ports
            .iter()
            .find(|port| port.role == role)
            .map(|port| port.port)
    }

    /// The `zebrad.toml` keys the launcher manages for the node: its network,
    /// its ports, its cookie and its state folder. A role the node has no
    /// port for gets the network's default.
    pub fn managed_keys(&self, launcher: &Paths) -> ManagedKeys {
        let defaults = ManagedKeys::defaults(
            self.network,
            &self.paths(launcher),
            self.state_dir(launcher),
        );
        let loopback = |port: u16| SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        ManagedKeys {
            p2p_port: self.port(PortRole::P2p).unwrap_or(defaults.p2p_port),
            rpc_addr: self.port(PortRole::Rpc).map_or(defaults.rpc_addr, loopback),
            metrics_addr: self
                .port(PortRole::Metrics)
                .map_or(defaults.metrics_addr, loopback),
            ..defaults
        }
    }

    /// The node as the UI sees it.
    pub fn info(&self, launcher: &Paths) -> NodeInfo {
        NodeInfo {
            id: self.id.clone(),
            name: self.name.clone(),
            kind: self.kind,
            network: self.network,
            depends_on: self.depends_on.clone(),
            data_dir: self.data_dir(launcher).display().to_string(),
            own_folder: (self.layout == NodeLayout::Folder)
                .then(|| self.paths(launcher).data_dir().display().to_string()),
            ports: self.ports.clone(),
            program: self.program.clone(),
            autostart: self.autostart,
            stop_grace_secs: self.stop_grace_secs,
        }
    }
}

/// Why a node list cannot be kept.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum NodeListError {
    /// A launcher needs at least one node.
    #[error("the node list is empty")]
    Empty,
    /// Two nodes have the same id.
    #[error("two nodes have the id {0}")]
    DuplicateId(NodeId),
    /// More than one node keeps its files in the launcher's own directories.
    #[error("more than one node keeps its files in the launcher's directories")]
    SharedFiles,
    /// A node has no name.
    #[error("the node {0} has no name")]
    NoName(NodeId),
    /// A node's stop grace period is outside the bounds.
    #[error("the stop grace period of {0} must be between {min} and {max} s", min = STOP_GRACE_SECS.start(), max = STOP_GRACE_SECS.end())]
    StopGrace(NodeId),
}

/// Checks the rules every saved node list keeps: at least one node, unique
/// ids, at most one node in the launcher's directories, a name each, and stop
/// grace periods within bounds.
pub fn check_list(nodes: &[NodeSpec]) -> Result<(), NodeListError> {
    if nodes.is_empty() {
        return Err(NodeListError::Empty);
    }
    for (index, node) in nodes.iter().enumerate() {
        if nodes[..index].iter().any(|other| other.id == node.id) {
            return Err(NodeListError::DuplicateId(node.id.clone()));
        }
        if node.name.trim().is_empty() {
            return Err(NodeListError::NoName(node.id.clone()));
        }
        if !STOP_GRACE_SECS.contains(&node.stop_grace_secs) {
            return Err(NodeListError::StopGrace(node.id.clone()));
        }
    }
    let shared = nodes
        .iter()
        .filter(|node| node.layout == NodeLayout::Launcher)
        .count();
    if shared > 1 {
        return Err(NodeListError::SharedFiles);
    }
    Ok(())
}

/// Why a node could not be added, changed or removed. The message is for the
/// user, after "Could not add the node: " or the like.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum NodeError {
    /// No node has the id.
    #[error("there is no node {0}")]
    NotFound(NodeId),
    /// The list already holds [`MAX_NODES`] nodes.
    #[error("Zeal runs at most {MAX_NODES} nodes")]
    TooMany,
    /// The kind cannot be added in this release ([`NodeKind::addable`]).
    #[error("{0} nodes come in a later release")]
    KindNotAddable(NodeKind),
    /// The node's name is empty.
    #[error("give the node a name")]
    NoName,
    /// The node's name is longer than [`NodeInfo::MAX_NAME_LEN`].
    #[error("the name is longer than {} characters", NodeInfo::MAX_NAME_LEN)]
    LongName,
    /// Another node has the name.
    #[error("another node is called {0:?}")]
    NameTaken(String),
    /// Another node has the id.
    #[error("another node has the id {0}")]
    IdTaken(NodeId),
    /// A removed node whose data was kept still has its folder under the id.
    #[error("the id {0} belongs to a removed node whose files are still in {1}")]
    IdHasFiles(NodeId, String),
    /// The node lacks a port its kind listens on.
    #[error("the node needs a {} port", .0.label())]
    MissingPort(PortRole),
    /// A port is 0 or listed twice.
    #[error("the {} port {port} cannot be used: {why}", role.label())]
    BadPort {
        role: PortRole,
        port: u16,
        why: &'static str,
    },
    /// Another node has the port.
    #[error("the {} port {port} is {holder}'s", role.label())]
    PortTaken {
        role: PortRole,
        port: u16,
        holder: String,
    },
    /// Another program on this computer has the port.
    #[error("the {} port {port} is in use by another program", role.label())]
    PortInUse { role: PortRole, port: u16 },
    /// The data folder is not an absolute path.
    #[error("the data folder {0:?} is not an absolute path")]
    RelativeDataDir(String),
    /// The data folder, or its chain state, is another node's or overlaps it.
    #[error("{dir} is {holder}'s data folder, or overlaps it")]
    DataDirTaken { dir: String, holder: String },
    /// The stop grace period is out of bounds.
    #[error("the stop grace period of {0} s must be between {min} and {max} s", min = STOP_GRACE_SECS.start(), max = STOP_GRACE_SECS.end())]
    StopGrace(u64),
    /// The node is the only one.
    #[error("Zeal needs a node: add another before removing {0}")]
    LastNode(String),
    /// The node left the list, but some of its files could not be deleted:
    /// not a refusal, so the message says what happened.
    #[error("{name} was removed, but {path} could not be deleted: {why}")]
    LeftBehind {
        name: String,
        path: String,
        why: String,
    },
    /// Something else is under way on the node, or a file operation failed.
    #[error("{0}")]
    Failed(String),
    /// The registry has shut down.
    #[error("Zeal is shutting down")]
    Closed,
}

/// The rules for a node's name: not empty once trimmed, at most
/// [`NodeInfo::MAX_NAME_LEN`] characters, and no other node's (ignoring case).
/// Returns the trimmed name.
fn check_name<'a>(
    name: &str,
    others: impl IntoIterator<Item = &'a NodeSpec>,
) -> Result<String, NodeError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(NodeError::NoName);
    }
    if name.chars().count() > NodeInfo::MAX_NAME_LEN {
        return Err(NodeError::LongName);
    }
    if others
        .into_iter()
        .any(|other| other.name.trim().to_lowercase() == name.to_lowercase())
    {
        return Err(NodeError::NameTaken(name.to_string()));
    }
    Ok(name.to_string())
}

/// The node that has `port`, among `nodes`.
fn holder_of(nodes: &[NodeSpec], port: u16) -> Option<&NodeSpec> {
    nodes
        .iter()
        .find(|node| node.ports.iter().any(|held| held.port == port))
}

/// The ports a new node of `kind` on `network` gets: the kind's defaults, moved
/// out of the way of every node's ports and of the other network's defaults
/// ([`node_ports`]), and then of ports another program on this computer holds
/// (`in_use`).
pub fn assign_free_ports(
    kind: NodeKind,
    network: Network,
    nodes: &[NodeSpec],
    in_use: &dyn Fn(PortRole, u16) -> bool,
) -> Vec<NodePort> {
    let mut taken: Vec<u16> = nodes
        .iter()
        .flat_map(|node| node.ports.iter().map(|port| port.port))
        .collect();
    // Each round skips at least one port more; give up after a few rounds and
    // let the start report what is still taken.
    for _ in 0..16 {
        let ports = node_ports(kind, network, &taken);
        let busy: Vec<u16> = ports
            .iter()
            .filter(|port| in_use(port.role, port.port))
            .map(|port| port.port)
            .collect();
        if busy.is_empty() {
            return ports;
        }
        taken.extend(busy);
    }
    node_ports(kind, network, &taken)
}

/// Whether two chain state folders are the same or one holds the other,
/// compared as written.
fn overlaps(a: &std::path::Path, b: &std::path::Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

/// The node `draft` describes, checked against the `nodes` there are: at most
/// [`MAX_NODES`], a kind this release adds, a free name and id (none of
/// another node, and none whose folder a removed node left behind), ports no
/// other node has and no other program holds (`in_use`), and a data folder
/// that is not another node's and whose chain state does not overlap
/// another's. What the draft leaves out is filled in: the first free id, the
/// kind's default ports moved out of the way, and a folder of its own under
/// [`Paths::nodes_dir`].
pub fn plan_new_node(
    draft: &NodeDraft,
    nodes: &[NodeSpec],
    paths: &Paths,
    in_use: &dyn Fn(PortRole, u16) -> bool,
) -> Result<NodeSpec, NodeError> {
    if nodes.len() >= MAX_NODES {
        return Err(NodeError::TooMany);
    }
    if !draft.kind.addable() || draft.program != NodeProgram::Bundled {
        return Err(NodeError::KindNotAddable(draft.kind));
    }
    let name = check_name(&draft.name, nodes)?;
    let has_files = |id: &NodeId| paths.node_folder(id).data_dir().exists();
    let id = match &draft.id {
        Some(id) => {
            if nodes.iter().any(|node| &node.id == id) {
                return Err(NodeError::IdTaken(id.clone()));
            }
            if has_files(id) {
                let folder = paths.node_folder(id).data_dir().display().to_string();
                return Err(NodeError::IdHasFiles(id.clone(), folder));
            }
            id.clone()
        }
        None => {
            let mut taken: Vec<NodeId> = nodes.iter().map(|node| node.id.clone()).collect();
            loop {
                let id = NodeId::suggest(draft.kind, draft.network, &taken);
                if !has_files(&id) {
                    break id;
                }
                taken.push(id);
            }
        }
    };
    let ports = match &draft.ports {
        None => assign_free_ports(draft.kind, draft.network, nodes, in_use),
        Some(ports) => {
            for role in draft
                .kind
                .default_ports(draft.network)
                .into_iter()
                .map(|port| port.role)
            {
                if !ports.iter().any(|port| port.role == role) {
                    return Err(NodeError::MissingPort(role));
                }
            }
            for (index, port) in ports.iter().enumerate() {
                let bad = |why| NodeError::BadPort {
                    role: port.role,
                    port: port.port,
                    why,
                };
                if port.port == 0 {
                    return Err(bad("pick a port above 0"));
                }
                if ports[..index].iter().any(|other| other.port == port.port) {
                    return Err(bad("it is given twice"));
                }
                if let Some(holder) = holder_of(nodes, port.port) {
                    return Err(NodeError::PortTaken {
                        role: port.role,
                        port: port.port,
                        holder: holder.name.clone(),
                    });
                }
                if in_use(port.role, port.port) {
                    return Err(NodeError::PortInUse {
                        role: port.role,
                        port: port.port,
                    });
                }
            }
            ports.clone()
        }
    };
    if let Some(dir) = &draft.data_dir
        && !std::path::Path::new(dir).is_absolute()
    {
        return Err(NodeError::RelativeDataDir(dir.clone()));
    }
    let node = NodeSpec {
        id,
        name,
        kind: draft.kind,
        network: draft.network,
        program: draft.program.clone(),
        data_dir: draft.data_dir.clone(),
        ports,
        autostart: draft.autostart,
        stop_grace_secs: DEFAULT_STOP_GRACE_SECS,
        depends_on: None,
        layout: NodeLayout::Folder,
    };
    check_data_dir(&node, nodes, paths)?;
    Ok(node)
}

/// Refuses a data folder that is another node's, or whose chain state is
/// another node's or holds it or lies inside it.
pub fn check_data_dir(
    node: &NodeSpec,
    others: &[NodeSpec],
    paths: &Paths,
) -> Result<(), NodeError> {
    let data = node.data_dir(paths);
    let state = node.state_dir(paths);
    for other in others.iter().filter(|other| other.id != node.id) {
        if other.data_dir(paths) == data || overlaps(&other.state_dir(paths), &state) {
            return Err(NodeError::DataDirTaken {
                dir: data.display().to_string(),
                holder: other.name.clone(),
            });
        }
    }
    Ok(())
}

/// `node` with `update` applied, checked against the `others`: a free name,
/// a stop grace within bounds, and for a new network the ports moved to that
/// network's defaults, out of the way of the others' ports, with a default
/// name following the network. With `rename`, a node in the launcher's
/// directories also takes the network's id (the first run, before anything
/// ran or could depend on the node).
pub fn plan_update(
    node: &NodeSpec,
    update: &NodeUpdate,
    others: &[NodeSpec],
    rename: bool,
) -> Result<NodeSpec, NodeError> {
    let mut next = node.clone();
    if let Some(name) = &update.name {
        next.name = check_name(name, others)?;
    }
    if let Some(autostart) = update.autostart {
        next.autostart = autostart;
    }
    if let Some(secs) = update.stop_grace_secs {
        if !STOP_GRACE_SECS.contains(&secs) {
            return Err(NodeError::StopGrace(secs));
        }
        next.stop_grace_secs = secs;
    }
    if let Some(network) = update.network
        && network != node.network
    {
        next.network = network;
        let taken: Vec<u16> = others
            .iter()
            .flat_map(|other| other.ports.iter().map(|port| port.port))
            .collect();
        next.ports = node_ports(node.kind, network, &taken);
        if update.name.is_none() && NodeInfo::is_default_name(&node.name, node.kind, node.network) {
            let names: Vec<&str> = others.iter().map(|other| other.name.as_str()).collect();
            next.name = NodeInfo::unique_default_name(node.kind, network, names);
        }
        if rename && node.layout == NodeLayout::Launcher {
            let ids: Vec<NodeId> = others.iter().map(|other| other.id.clone()).collect();
            next.id = NodeId::suggest(node.kind, network, &ids);
        }
    }
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_node_keeps_the_single_nodes_files_and_ports() {
        let paths = Paths::under("/root");
        let node = NodeSpec::first(Network::Testnet);
        assert_eq!(node.id.as_str(), "zebrad-testnet");
        assert_eq!(node.name, "Zebra · Testnet");
        assert_eq!(node.paths(&paths), paths.launcher_node());
        assert_eq!(node.state_dir(&paths), paths.data_dir().join("state"));
        let keys = node.managed_keys(&paths);
        assert_eq!(
            keys,
            ManagedKeys::defaults(
                Network::Testnet,
                &paths.launcher_node(),
                paths.launcher_node().default_state_dir()
            ),
            "the managed keys of a single-node launcher"
        );
        let info = node.info(&paths);
        assert_eq!(info.data_dir, paths.data_dir().display().to_string());
        assert_eq!(
            info.own_folder, None,
            "the launcher's files are not the node's"
        );
        assert_eq!(info.port(PortRole::P2p), Some(18233));
    }

    #[test]
    fn a_node_in_its_own_folder_gets_its_own_ports_cookie_and_state() {
        let paths = Paths::under("/root");
        let node = NodeSpec {
            id: NodeId::new("zebrad-mainnet-2").unwrap(),
            layout: NodeLayout::Folder,
            ports: vec![
                NodePort {
                    role: PortRole::P2p,
                    port: 18233,
                },
                NodePort {
                    role: PortRole::Rpc,
                    port: 18232,
                },
                NodePort {
                    role: PortRole::Metrics,
                    port: 19999,
                },
            ],
            ..NodeSpec::first(Network::Mainnet)
        };
        let folder = paths.nodes_dir().join("zebrad-mainnet-2");
        let keys = node.managed_keys(&paths);
        assert_eq!(keys.p2p_port, 18233);
        assert_eq!(keys.rpc_addr.to_string(), "127.0.0.1:18232");
        assert_eq!(keys.metrics_addr.to_string(), "127.0.0.1:19999");
        assert_eq!(keys.cookie_dir, folder.join("rpc"));
        assert_eq!(keys.state_dir, folder.join("state"));
        // A chosen data folder holds the state, not the node's own files.
        let moved = NodeSpec {
            data_dir: Some("/big/disk".into()),
            ..node
        };
        assert_eq!(moved.state_dir(&paths), PathBuf::from("/big/disk/state"));
        assert_eq!(
            moved.paths(&paths).zebrad_config(),
            folder.join("zebrad.toml")
        );
        let info = moved.info(&paths);
        assert_eq!(info.data_dir, "/big/disk");
        assert_eq!(info.own_folder, Some(folder.display().to_string()));
    }

    fn draft(network: Network, name: &str) -> NodeDraft {
        NodeDraft {
            kind: NodeKind::Zebrad,
            network,
            name: name.into(),
            id: None,
            ports: None,
            data_dir: None,
            program: NodeProgram::Bundled,
            autostart: true,
        }
    }

    fn port(role: PortRole, port: u16) -> NodePort {
        NodePort { role, port }
    }

    fn nothing_in_use(_: PortRole, _: u16) -> bool {
        false
    }

    #[test]
    fn a_new_node_gets_its_own_id_ports_and_folder() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::under(dir.path());
        let first = NodeSpec::first(Network::Mainnet);
        let nodes = [first.clone()];

        // A Testnet node takes Testnet's defaults: nothing else has them.
        let testnet = plan_new_node(
            &draft(Network::Testnet, "Zebra · Testnet"),
            &nodes,
            &paths,
            &nothing_in_use,
        )
        .expect("a Testnet node");
        assert_eq!(testnet.id.as_str(), "zebrad-testnet");
        assert_eq!(testnet.layout, NodeLayout::Folder);
        // Testnet's own P2P and RPC defaults; the metrics port is 9999 on both
        // networks, so it moves.
        let numbers: Vec<u16> = testnet.ports.iter().map(|port| port.port).collect();
        assert_eq!(numbers, [18233, 18232, 19999]);
        assert_eq!(
            testnet.paths(&paths),
            paths.node_folder(&testnet.id),
            "a folder of its own"
        );

        // A second Mainnet node moves out of the first's way, and off Testnet's
        // defaults.
        let second = plan_new_node(
            &draft(Network::Mainnet, "Zebra · Mainnet 2"),
            &nodes,
            &paths,
            &nothing_in_use,
        )
        .expect("a second Mainnet node");
        assert_eq!(second.id.as_str(), "zebrad-mainnet-2");
        let numbers: Vec<u16> = second.ports.iter().map(|port| port.port).collect();
        assert_eq!(numbers, [28233, 28232, 19999]);

        // A port another program holds is skipped too.
        let held = |_: PortRole, port: u16| port == 28232;
        let around = plan_new_node(
            &draft(Network::Mainnet, "Zebra · Mainnet 2"),
            &nodes,
            &paths,
            &held,
        )
        .expect("ports around the held one");
        assert_eq!(around.port(PortRole::Rpc), Some(38232));
        assert!(!around.ports.iter().any(|port| port.port == 28232));

        // The folder a removed node left behind keeps its id from being reused.
        std::fs::create_dir_all(paths.node_folder(&testnet.id).data_dir()).expect("mkdir");
        let next = plan_new_node(
            &draft(Network::Testnet, "Zebra · Testnet"),
            &nodes,
            &paths,
            &nothing_in_use,
        )
        .expect("the next id");
        assert_eq!(next.id.as_str(), "zebrad-testnet-2");
        let asked = NodeDraft {
            id: Some(testnet.id.clone()),
            ..draft(Network::Testnet, "Zebra · Testnet")
        };
        assert!(matches!(
            plan_new_node(&asked, &nodes, &paths, &nothing_in_use),
            Err(NodeError::IdHasFiles(id, _)) if id == testnet.id
        ));
    }

    #[test]
    fn a_draft_that_breaks_a_rule_is_refused() {
        // An absolute root on every OS: a draft's folder must be absolute, and "/root" is
        // not on Windows.
        let paths = Paths::under(if cfg!(windows) { r"C:\root" } else { "/root" });
        let first = NodeSpec::first(Network::Mainnet);
        let nodes = vec![first.clone()];
        let refused = |draft: NodeDraft| {
            plan_new_node(&draft, &nodes, &paths, &nothing_in_use).expect_err("refused")
        };
        assert_eq!(
            refused(NodeDraft {
                kind: NodeKind::Zakura,
                ..draft(Network::Mainnet, "Zakura")
            }),
            NodeError::KindNotAddable(NodeKind::Zakura)
        );
        assert_eq!(refused(draft(Network::Testnet, "  ")), NodeError::NoName);
        assert_eq!(
            refused(draft(
                Network::Testnet,
                &"x".repeat(NodeInfo::MAX_NAME_LEN + 1)
            )),
            NodeError::LongName
        );
        assert_eq!(
            refused(draft(Network::Testnet, "zebra · MAINNET")),
            NodeError::NameTaken("zebra · MAINNET".into())
        );
        assert_eq!(
            refused(NodeDraft {
                id: Some(first.id.clone()),
                ..draft(Network::Mainnet, "Another")
            }),
            NodeError::IdTaken(first.id.clone())
        );
        // Ports: another node's, given twice, 0, missing a role, another program's.
        let ports = |list: &[(PortRole, u16)]| {
            Some(
                list.iter()
                    .map(|&(role, n)| port(role, n))
                    .collect::<Vec<_>>(),
            )
        };
        let with_ports = |list: &[(PortRole, u16)]| NodeDraft {
            ports: ports(list),
            ..draft(Network::Testnet, "Zebra · Testnet")
        };
        assert_eq!(
            refused(with_ports(&[
                (PortRole::P2p, 18233),
                (PortRole::Rpc, 8232),
                (PortRole::Metrics, 19999)
            ])),
            NodeError::PortTaken {
                role: PortRole::Rpc,
                port: 8232,
                holder: first.name.clone()
            }
        );
        assert!(matches!(
            refused(with_ports(&[
                (PortRole::P2p, 18233),
                (PortRole::Rpc, 18233),
                (PortRole::Metrics, 19999)
            ])),
            NodeError::BadPort { port: 18233, .. }
        ));
        assert!(matches!(
            refused(with_ports(&[
                (PortRole::P2p, 0),
                (PortRole::Rpc, 18232),
                (PortRole::Metrics, 19999)
            ])),
            NodeError::BadPort { port: 0, .. }
        ));
        assert_eq!(
            refused(with_ports(&[
                (PortRole::P2p, 18233),
                (PortRole::Rpc, 18232)
            ])),
            NodeError::MissingPort(PortRole::Metrics)
        );
        let held = |_: PortRole, port: u16| port == 18232;
        assert_eq!(
            plan_new_node(
                &with_ports(&[
                    (PortRole::P2p, 18233),
                    (PortRole::Rpc, 18232),
                    (PortRole::Metrics, 19999)
                ]),
                &nodes,
                &paths,
                &held
            ),
            Err(NodeError::PortInUse {
                role: PortRole::Rpc,
                port: 18232
            })
        );
        // Data folders: another node's, one inside another's chain state, a relative one.
        let with_dir = |dir: &str| NodeDraft {
            data_dir: Some(dir.into()),
            ..draft(Network::Testnet, "Zebra · Testnet")
        };
        let launcher_data = paths.data_dir().display().to_string();
        assert!(matches!(
            refused(with_dir(&launcher_data)),
            NodeError::DataDirTaken { holder, .. } if holder == first.name
        ));
        let inside = paths.data_dir().join("state").join("mine");
        assert!(matches!(
            refused(with_dir(inside.to_str().unwrap())),
            NodeError::DataDirTaken { .. }
        ));
        assert!(matches!(
            refused(with_dir("relative/folder")),
            NodeError::RelativeDataDir(_)
        ));
        // A folder of its own elsewhere is fine.
        let elsewhere = plan_new_node(
            &with_dir("/mnt/big/testnet"),
            &nodes,
            &paths,
            &nothing_in_use,
        )
        .expect("a folder elsewhere");
        assert_eq!(
            elsewhere.state_dir(&paths),
            PathBuf::from("/mnt/big/testnet/state")
        );
        // At most MAX_NODES.
        let full: Vec<NodeSpec> = (0..MAX_NODES)
            .map(|n| NodeSpec {
                id: NodeId::new(format!("node-{n}")).unwrap(),
                name: format!("Node {n}"),
                ..first.clone()
            })
            .collect();
        assert_eq!(
            plan_new_node(
                &draft(Network::Testnet, "One more"),
                &full,
                &paths,
                &nothing_in_use
            ),
            Err(NodeError::TooMany)
        );
    }

    #[test]
    fn a_new_network_moves_the_ports_the_default_name_and_during_the_first_run_the_id() {
        let first = NodeSpec::first(Network::Mainnet);
        let to_testnet = NodeUpdate {
            network: Some(Network::Testnet),
            ..NodeUpdate::default()
        };
        let moved = plan_update(&first, &to_testnet, &[], false).expect("update");
        assert_eq!(moved.network, Network::Testnet);
        assert_eq!(
            moved.ports,
            NodeKind::Zebrad.default_ports(Network::Testnet)
        );
        assert_eq!(moved.name, "Zebra · Testnet");
        assert_eq!(
            moved.id, first.id,
            "ids are stable once the first run is done"
        );
        let renamed = plan_update(&first, &to_testnet, &[], true).expect("update");
        assert_eq!(renamed.id.as_str(), "zebrad-testnet");

        // Out of the way of another node; a name the user gave stays.
        let other = NodeSpec {
            id: NodeId::new("zebrad-testnet").unwrap(),
            layout: NodeLayout::Folder,
            ..NodeSpec::first(Network::Testnet)
        };
        let named = NodeSpec {
            name: "Archive".into(),
            ..first.clone()
        };
        let moved =
            plan_update(&named, &to_testnet, std::slice::from_ref(&other), true).expect("update");
        let numbers: Vec<u16> = moved.ports.iter().map(|port| port.port).collect();
        assert_eq!(numbers, [28233, 28232, 19999]);
        assert_eq!(moved.name, "Archive");
        assert_eq!(moved.id.as_str(), "zebrad-testnet-2");

        // Names and stop grace periods keep their rules; autostart is a switch.
        assert_eq!(
            plan_update(
                &first,
                &NodeUpdate {
                    name: Some("zebra · testnet".into()),
                    ..NodeUpdate::default()
                },
                std::slice::from_ref(&other),
                false
            ),
            Err(NodeError::NameTaken("zebra · testnet".into()))
        );
        assert_eq!(
            plan_update(
                &first,
                &NodeUpdate {
                    stop_grace_secs: Some(1),
                    ..NodeUpdate::default()
                },
                &[],
                false
            ),
            Err(NodeError::StopGrace(1))
        );
        let quiet = plan_update(
            &first,
            &NodeUpdate {
                autostart: Some(false),
                stop_grace_secs: Some(120),
                name: Some("  Main  ".into()),
                ..NodeUpdate::default()
            },
            &[],
            false,
        )
        .expect("update");
        assert!(!quiet.autostart);
        assert_eq!(quiet.stop_grace_secs, 120);
        assert_eq!(quiet.name, "Main");
        assert_eq!(quiet.ports, first.ports, "the same network keeps its ports");
    }

    #[test]
    fn node_lists_keep_their_rules() {
        let first = NodeSpec::first(Network::Mainnet);
        let second = NodeSpec {
            id: NodeId::new("zebrad-testnet").unwrap(),
            layout: NodeLayout::Folder,
            ..NodeSpec::first(Network::Testnet)
        };
        assert_eq!(check_list(&[first.clone(), second.clone()]), Ok(()));
        assert_eq!(check_list(&[]), Err(NodeListError::Empty));
        assert_eq!(
            check_list(&[first.clone(), first.clone()]),
            Err(NodeListError::DuplicateId(first.id.clone()))
        );
        let shared = NodeSpec {
            layout: NodeLayout::Launcher,
            ..second.clone()
        };
        assert_eq!(
            check_list(&[first.clone(), shared]),
            Err(NodeListError::SharedFiles)
        );
        let unnamed = NodeSpec {
            name: " ".into(),
            ..second.clone()
        };
        assert_eq!(
            check_list(&[first.clone(), unnamed]),
            Err(NodeListError::NoName(second.id.clone()))
        );
        let hasty = NodeSpec {
            stop_grace_secs: 0,
            ..second.clone()
        };
        assert_eq!(
            check_list(&[first, hasty]),
            Err(NodeListError::StopGrace(second.id))
        );
    }
}
