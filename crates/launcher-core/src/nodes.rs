//! The nodes Zeal runs, as `launcher.json` keeps them: what each one is, where
//! its files are and which ports it has. The registry (`crate::registry`)
//! runs them; this module is the plain data and the rules about it.

use std::{
    net::{Ipv4Addr, SocketAddr},
    path::PathBuf,
};

use launcher_types::{
    DEFAULT_STOP_GRACE_SECS, Network, NodeId, NodeInfo, NodeKind, NodePort, NodeProgram, PortRole,
    STOP_GRACE_SECS,
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
