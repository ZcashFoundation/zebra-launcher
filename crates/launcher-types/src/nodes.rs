//! Nodes: the processes Zeal runs. Each is one implementation ([`NodeKind`]) of a Zcash full
//! node or of a service that works with one, on one [`Network`]. See docs/MULTI-NODE.md.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{DEFAULT_STOP_GRACE_SECS, Network};

/// The most nodes Zeal runs: adding another is refused at this many.
pub const MAX_NODES: usize = 8;

/// A node's id: a short slug of lowercase ASCII letters, digits and dashes (`zebrad-mainnet`),
/// unique and stable for the node's life. It names the node's folders and travels with every
/// node-scoped command and event.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct NodeId(String);

impl NodeId {
    /// The longest id, so that folder names stay short.
    pub const MAX_LEN: usize = 40;

    /// `id` if it is a valid slug: 1 to [`NodeId::MAX_LEN`] characters from `a-z`, `0-9` and
    /// `-`, starting with a letter and not ending with a dash.
    pub fn new(id: impl Into<String>) -> Option<Self> {
        let id = id.into();
        let valid = !id.is_empty()
            && id.len() <= Self::MAX_LEN
            && id.starts_with(|c: char| c.is_ascii_lowercase())
            && !id.ends_with('-')
            && id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        valid.then_some(NodeId(id))
    }

    /// The id for a new node of `kind` on `network` that none of `taken` has:
    /// `zebrad-mainnet`, then `zebrad-mainnet-2`, `zebrad-mainnet-3` and so on.
    pub fn suggest<'a>(
        kind: NodeKind,
        network: Network,
        taken: impl IntoIterator<Item = &'a NodeId> + Clone,
    ) -> NodeId {
        let base = format!("{}-{}", kind.slug(), network.as_str().to_ascii_lowercase());
        let free = |candidate: &str| taken.clone().into_iter().all(|id| id.0 != candidate);
        if free(&base) {
            return NodeId(base);
        }
        (2u32..)
            .map(|n| format!("{base}-{n}"))
            .find(|candidate| free(candidate))
            .map(NodeId)
            .expect("an unbounded range has a free number")
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for NodeId {
    type Error = String;

    fn try_from(id: String) -> Result<Self, Self::Error> {
        NodeId::new(id.clone()).ok_or_else(|| format!("not a valid node id: {id:?}"))
    }
}

impl From<NodeId> for String {
    fn from(id: NodeId) -> Self {
        id.0
    }
}

/// What a node is for: a full node keeps the chain, a service works with one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeRole {
    /// Downloads and verifies the chain and serves it over RPC.
    FullNode,
    /// Indexes a full node's chain and serves light wallets.
    Indexer,
    /// Keeps keys and builds transactions on top of a node or an indexer.
    Wallet,
    /// Anything else: whatever the user's command is.
    Custom,
}

/// The implementation a node runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    /// The Zcash Foundation's node, bundled with Zeal.
    Zebrad,
    /// Valar Group and Project Tachyon's Zebra fork, with pruning and snapshots.
    Zakura,
    /// The Zcash Foundation's compatibility build of zcashd, following a zebrad or Zakura
    /// node: stock zcashd stopped at its end of support.
    Zcashd,
    /// An independent node written in Bend.
    Zebend,
    /// An indexer serving light wallets from a full node.
    Zaino,
    /// The Z3 wallet.
    Zallet,
    /// Any program, described by its command line.
    Generic,
}

impl NodeKind {
    /// Every kind, in the order the add-node dialog offers them.
    pub const ALL: [NodeKind; 7] = [
        NodeKind::Zebrad,
        NodeKind::Zakura,
        NodeKind::Zcashd,
        NodeKind::Zebend,
        NodeKind::Zaino,
        NodeKind::Zallet,
        NodeKind::Generic,
    ];

    /// The kind's name in ids and folder names.
    pub fn slug(self) -> &'static str {
        match self {
            NodeKind::Zebrad => "zebrad",
            NodeKind::Zakura => "zakura",
            NodeKind::Zcashd => "zcashd",
            NodeKind::Zebend => "zebend",
            NodeKind::Zaino => "zaino",
            NodeKind::Zallet => "zallet",
            NodeKind::Generic => "custom",
        }
    }

    /// The kind's name for people: the project's own spelling.
    pub fn label(self) -> &'static str {
        match self {
            NodeKind::Zebrad => "Zebra",
            NodeKind::Zakura => "Zakura",
            NodeKind::Zcashd => "zcashd",
            NodeKind::Zebend => "Zebend",
            NodeKind::Zaino => "Zaino",
            NodeKind::Zallet => "Zallet",
            NodeKind::Generic => "Custom",
        }
    }

    pub fn role(self) -> NodeRole {
        match self {
            NodeKind::Zebrad | NodeKind::Zakura | NodeKind::Zcashd | NodeKind::Zebend => {
                NodeRole::FullNode
            }
            NodeKind::Zaino => NodeRole::Indexer,
            NodeKind::Zallet => NodeRole::Wallet,
            NodeKind::Generic => NodeRole::Custom,
        }
    }

    /// The kinds a node of this kind needs one of: Zaino and Zallet read a zebrad, and the
    /// zcashd compatibility build follows a zebrad or Zakura node. Empty for kinds that stand
    /// alone.
    pub fn uses(self) -> &'static [NodeKind] {
        match self {
            NodeKind::Zaino | NodeKind::Zallet => &[NodeKind::Zebrad],
            NodeKind::Zcashd => &[NodeKind::Zebrad, NodeKind::Zakura],
            _ => &[],
        }
    }

    /// The kind's program, as its version is reported ("zakurad 1.8.0"); `None` for a
    /// generic node, whose program is whatever the user named.
    pub fn program(self) -> Option<&'static str> {
        match self {
            NodeKind::Zebrad => Some("zebrad"),
            NodeKind::Zakura => Some("zakurad"),
            NodeKind::Zcashd => Some("zcashd"),
            NodeKind::Zebend => Some("zebend"),
            NodeKind::Zaino => Some("zainod"),
            NodeKind::Zallet => Some("zallet"),
            NodeKind::Generic => None,
        }
    }

    /// Whether Zeal ships the kind's binary. Every other kind runs a program the user has.
    pub fn bundled(self) -> bool {
        self == NodeKind::Zebrad
    }

    /// Whether the node gets an Explorer: it keeps the chain and answers zebrad's RPC
    /// methods. The zcashd compatibility build waits until its RPC has been checked.
    pub fn has_explorer(self) -> bool {
        matches!(self, NodeKind::Zebrad | NodeKind::Zakura)
    }

    /// Whether this Zeal can add a node of the kind: zebrad only, for now. The other kinds
    /// come in later releases (docs/MULTI-NODE.md).
    pub fn addable(self) -> bool {
        self == NodeKind::Zebrad
    }

    /// One sentence for the add-node dialog: what the kind is.
    pub fn summary(self) -> &'static str {
        match self {
            NodeKind::Zebrad => "The Zcash Foundation's node. Bundled with Zeal.",
            NodeKind::Zakura => {
                "A Zebra fork built for scale, with block pruning and snapshots for a fast start."
            }
            NodeKind::Zcashd => {
                "zcashd's compatibility build, following one of your Zebra or Zakura nodes."
            }
            NodeKind::Zebend => {
                "An independent node written in Bend. Young and unaudited: not for funds."
            }
            NodeKind::Zaino => "An indexer that serves light wallets from a Zebra node.",
            NodeKind::Zallet => "The Z3 wallet, replacing zcashd's. Beta.",
            NodeKind::Generic => "Any other program: you give its command line.",
        }
    }
}

impl NodeKind {
    /// The ports a node of this kind listens on by default, before any clash is resolved:
    /// what its own documentation uses. Services Zeal knows less about get the ports their
    /// projects document; a generic node declares its own.
    pub fn default_ports(self, network: Network) -> Vec<NodePort> {
        let testnet = network == Network::Testnet;
        let pick = |mainnet: u16, testnet_port: u16| if testnet { testnet_port } else { mainnet };
        let port = |role, port| NodePort { role, port };
        match self {
            NodeKind::Zebrad | NodeKind::Zakura => vec![
                port(PortRole::P2p, pick(8233, 18233)),
                port(PortRole::Rpc, pick(8232, 18232)),
                port(PortRole::Metrics, 9999),
            ],
            // The compatibility build opens no P2P listener: it connects to its node.
            NodeKind::Zcashd => vec![port(PortRole::Rpc, pick(8232, 18232))],
            NodeKind::Zebend => vec![port(PortRole::Rpc, 18233)],
            NodeKind::Zaino => vec![port(PortRole::Grpc, 8137)],
            NodeKind::Zallet => vec![port(PortRole::Rpc, 28232)],
            NodeKind::Generic => Vec::new(),
        }
    }
}

/// The ports a new node of `kind` on `network` gets beside nodes that hold `taken`: the kind's
/// defaults for the network, moved with [`assign_ports`] out of the way of `taken` and of the
/// kind's defaults on the other network. A second Mainnet zebrad so gets 28233 and 28232, not
/// Testnet's 18233 and 18232, and a tool pointed at Testnet's default RPC port never reaches a
/// Mainnet node.
pub fn node_ports(kind: NodeKind, network: Network, taken: &[u16]) -> Vec<NodePort> {
    let wanted = kind.default_ports(network);
    let other = match network {
        Network::Mainnet => Network::Testnet,
        Network::Testnet => Network::Mainnet,
    };
    let mut avoid = taken.to_vec();
    avoid.extend(
        kind.default_ports(other)
            .into_iter()
            .map(|port| port.port)
            .filter(|port| !wanted.iter().any(|own| own.port == *port)),
    );
    assign_ports(&wanted, &avoid)
}

/// `wanted` with every port that `taken` holds moved out of the way: up by 10,000 at a time
/// (8233 becomes 18233, then 28233), which keeps a port recognisable, and up by one when that
/// runs out of room. Ports are also kept apart from each other. The backend checks again,
/// against the OS, when the node starts.
pub fn assign_ports(wanted: &[NodePort], taken: &[u16]) -> Vec<NodePort> {
    let mut used: Vec<u16> = taken.to_vec();
    wanted
        .iter()
        .map(|want| {
            let mut port = want.port;
            while used.contains(&port) {
                port = port
                    .checked_add(10_000)
                    .unwrap_or(port.wrapping_add(1).max(1024));
            }
            used.push(port);
            NodePort {
                role: want.role,
                port,
            }
        })
        .collect()
}

impl fmt::Display for NodeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// What a port of a node is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortRole {
    /// Connections from other nodes.
    P2p,
    /// The JSON-RPC interface.
    Rpc,
    /// The Prometheus metrics page.
    Metrics,
    /// A gRPC interface, such as an indexer's lightwalletd-compatible one.
    Grpc,
}

impl PortRole {
    pub fn label(self) -> &'static str {
        match self {
            PortRole::P2p => "P2P",
            PortRole::Rpc => "RPC",
            PortRole::Metrics => "metrics",
            PortRole::Grpc => "gRPC",
        }
    }
}

/// A port a node listens on, as assigned when the node was added.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodePort {
    pub role: PortRole,
    pub port: u16,
}

/// Where a node's program comes from. On the wire it is tagged by `from`:
/// `{"from": "bundled"}`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "from", rename_all = "snake_case")]
pub enum NodeProgram {
    /// The zebrad bundled with Zeal.
    #[default]
    Bundled,
}

/// What everyone lists about a node: who it is and where it lives, not how it is doing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeInfo {
    pub id: NodeId,
    /// The name people see, editable: "Zebra · Mainnet" by default.
    pub name: String,
    pub kind: NodeKind,
    pub network: Network,
    /// For a service, the node it uses.
    pub depends_on: Option<NodeId>,
    /// The node's data folder: its chain state is in the `state` folder inside it.
    pub data_dir: String,
    /// The node's own folder, `<app data>/nodes/<id>`, with its configuration, log and
    /// cookie: removing the node with its data deletes it too. `None` for the first node,
    /// whose files are the launcher's own and stay.
    #[serde(default)]
    pub own_folder: Option<String>,
    /// The ports it listens on.
    pub ports: Vec<NodePort>,
    /// Where its program comes from.
    #[serde(default)]
    pub program: NodeProgram,
    /// Zeal starts the node when it opens, once the first run is done.
    #[serde(default = "autostart_default")]
    pub autostart: bool,
    /// How long a requested stop may take before the node is killed, in seconds.
    #[serde(default = "stop_grace_default")]
    pub stop_grace_secs: u64,
}

fn autostart_default() -> bool {
    true
}

fn stop_grace_default() -> u64 {
    DEFAULT_STOP_GRACE_SECS
}

impl NodeInfo {
    /// The longest name a node can have, in characters.
    pub const MAX_NAME_LEN: usize = 64;

    /// The default name of a node: its kind and network, "Zebra · Mainnet".
    pub fn default_name(kind: NodeKind, network: Network) -> String {
        format!("{} · {network}", kind.label())
    }

    /// The default name for a new node that none of `taken` has: "Zebra · Testnet", then
    /// "Zebra · Testnet 2", "Zebra · Testnet 3" and so on.
    pub fn unique_default_name<'a>(
        kind: NodeKind,
        network: Network,
        taken: impl IntoIterator<Item = &'a str> + Clone,
    ) -> String {
        let base = Self::default_name(kind, network);
        let free = |candidate: &str| taken.clone().into_iter().all(|name| name != candidate);
        if free(&base) {
            return base;
        }
        (2u32..)
            .map(|n| format!("{base} {n}"))
            .find(|candidate| free(candidate))
            .expect("an unbounded range has a free number")
    }

    /// Whether `name` is a default name of a node of `kind` on `network`: "Zebra · Mainnet",
    /// or a numbered one such as "Zebra · Mainnet 2".
    pub fn is_default_name(name: &str, kind: NodeKind, network: Network) -> bool {
        match name.strip_prefix(&Self::default_name(kind, network)) {
            Some("") => true,
            Some(rest) => rest
                .strip_prefix(' ')
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())),
            None => false,
        }
    }

    /// The port the node listens on for `role`, if it has one.
    pub fn port(&self, role: PortRole) -> Option<u16> {
        self.ports
            .iter()
            .find(|port| port.role == role)
            .map(|port| port.port)
    }
}

/// A node to add (`add_node`): what the user chose, and what the backend fills in when it is
/// left out.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeDraft {
    pub kind: NodeKind,
    pub network: Network,
    /// The name people see. Not empty, and not the name of another node.
    pub name: String,
    /// The node's id; `None`: the backend picks the first free one for the kind and network
    /// ([`NodeId::suggest`]), skipping the folders of removed nodes whose data was kept.
    #[serde(default)]
    pub id: Option<NodeId>,
    /// The ports; `None`: the kind's defaults, moved out of the way of every other node's
    /// ports, of the other network's defaults and of ports already in use on this computer
    /// ([`node_ports`]).
    #[serde(default)]
    pub ports: Option<Vec<NodePort>>,
    /// The data folder; `None`: a folder of its own in the launcher's data folder.
    #[serde(default)]
    pub data_dir: Option<String>,
    #[serde(default)]
    pub program: NodeProgram,
    #[serde(default = "autostart_default")]
    pub autostart: bool,
}

/// A change to a node (`update_node`): each field that is set replaces the node's.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeUpdate {
    /// A new name: not empty, and not the name of another node.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub autostart: Option<bool>,
    /// A new stop grace period, within [`STOP_GRACE_SECS`](crate::STOP_GRACE_SECS); it takes
    /// effect when Zeal next opens.
    #[serde(default)]
    pub stop_grace_secs: Option<u64>,
    /// A new network: the ports move to the network's defaults, out of the way of the other
    /// nodes, and a default name follows. The change takes effect on the node's next start.
    #[serde(default)]
    pub network: Option<Network>,
}

/// An event about one node: which node, and what changed. Every node-scoped event (`status`,
/// `node_stats`, `node_details`, `metrics`, `stale_process`) carries one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeEvent<T> {
    pub node: NodeId,
    pub value: T,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_short_lowercase_slugs() {
        assert!(NodeId::new("zebrad-mainnet").is_some());
        assert!(NodeId::new("zakura-mainnet-2").is_some());
        for bad in [
            "",
            "Zebrad",
            "2zebrad",
            "zebrad-",
            "zebrad mainnet",
            "zébrad",
            "-a",
        ] {
            assert_eq!(NodeId::new(bad), None, "{bad:?}");
        }
        assert!(NodeId::new("a".repeat(NodeId::MAX_LEN)).is_some());
        assert_eq!(NodeId::new("a".repeat(NodeId::MAX_LEN + 1)), None);
    }

    #[test]
    fn ids_deserialize_only_when_valid() {
        let id: NodeId = serde_json::from_str("\"zebrad-testnet\"").unwrap();
        assert_eq!(id.as_str(), "zebrad-testnet");
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"zebrad-testnet\"");
        assert!(serde_json::from_str::<NodeId>("\"Not An Id\"").is_err());
    }

    #[test]
    fn suggested_ids_skip_the_taken_ones() {
        let none: Vec<NodeId> = Vec::new();
        assert_eq!(
            NodeId::suggest(NodeKind::Zebrad, Network::Mainnet, &none).as_str(),
            "zebrad-mainnet"
        );
        let taken = [
            NodeId::new("zebrad-mainnet").unwrap(),
            NodeId::new("zebrad-mainnet-2").unwrap(),
        ];
        assert_eq!(
            NodeId::suggest(NodeKind::Zebrad, Network::Mainnet, &taken).as_str(),
            "zebrad-mainnet-3"
        );
        assert_eq!(
            NodeId::suggest(NodeKind::Generic, Network::Testnet, &taken).as_str(),
            "custom-testnet"
        );
    }

    #[test]
    fn ports_move_out_of_each_others_way() {
        let zebrad = NodeKind::Zebrad.default_ports(Network::Mainnet);
        assert_eq!(assign_ports(&zebrad, &[]), zebrad);
        let taken: Vec<u16> = zebrad.iter().map(|port| port.port).collect();
        let second = assign_ports(&NodeKind::Zakura.default_ports(Network::Mainnet), &taken);
        let numbers: Vec<u16> = second.iter().map(|port| port.port).collect();
        assert_eq!(numbers, [18233, 18232, 19999]);
        // A Testnet zebrad already holds 18233 and 18232: the next step is 28233.
        let third = assign_ports(
            &NodeKind::Zakura.default_ports(Network::Mainnet),
            &[8233, 8232, 18233, 18232],
        );
        let numbers: Vec<u16> = third.iter().map(|port| port.port).collect();
        assert_eq!(numbers, [28233, 28232, 9999]);
        // Near the top of the range the step is one.
        let high = assign_ports(
            &[NodePort {
                role: PortRole::Rpc,
                port: 60_000,
            }],
            &[60_000],
        );
        assert_eq!(high[0].port, 60_001);
    }

    #[test]
    fn a_node_stays_off_the_other_networks_default_ports() {
        let numbers =
            |ports: Vec<NodePort>| -> Vec<u16> { ports.iter().map(|port| port.port).collect() };
        let zebrad = NodeKind::Zebrad;
        // Beside the first Mainnet node, a Testnet node keeps Testnet's defaults but the
        // metrics port both share...
        let first = [8233, 8232, 9999];
        assert_eq!(
            numbers(node_ports(zebrad, Network::Testnet, &first)),
            [18233, 18232, 19999]
        );
        // ...and a second Mainnet node skips Testnet's.
        assert_eq!(
            numbers(node_ports(zebrad, Network::Mainnet, &first)),
            [28233, 28232, 19999]
        );
        // A Testnet node after that still gets Testnet's own.
        assert_eq!(
            numbers(node_ports(
                zebrad,
                Network::Testnet,
                &[8233, 8232, 9999, 28233, 28232, 19999]
            )),
            [18233, 18232, 29999]
        );
        // With nothing else, the defaults.
        assert_eq!(
            node_ports(zebrad, Network::Mainnet, &[]),
            zebrad.default_ports(Network::Mainnet)
        );
    }

    #[test]
    fn node_payloads_fill_in_what_older_ones_lack() {
        // A node as the phase 1 mock sent it: no program, autostart or stop grace.
        let info: NodeInfo = serde_json::from_value(serde_json::json!({
            "id": "zebrad-testnet",
            "name": "Zebra · Testnet",
            "kind": "zebrad",
            "network": "testnet",
            "depends_on": null,
            "data_dir": "/data/nodes/zebrad-testnet",
            "ports": [{"role": "p2p", "port": 18233}, {"role": "rpc", "port": 18232}],
        }))
        .expect("an older node payload");
        assert_eq!(info.program, NodeProgram::Bundled);
        assert!(info.autostart);
        assert_eq!(info.stop_grace_secs, DEFAULT_STOP_GRACE_SECS);
        assert_eq!(info.port(PortRole::Rpc), Some(18232));
        assert_eq!(info.port(PortRole::Metrics), None);
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["program"], serde_json::json!({"from": "bundled"}));

        let draft: NodeDraft = serde_json::from_value(serde_json::json!({
            "kind": "zebrad",
            "network": "mainnet",
            "name": "Zebra · Mainnet 2",
        }))
        .expect("a draft with the choices only");
        assert_eq!(
            (draft.id, draft.ports, draft.data_dir),
            (None, None, None),
            "the backend fills these in"
        );
        assert!(draft.autostart);
        assert_eq!(
            serde_json::from_str::<NodeUpdate>("{}").unwrap(),
            NodeUpdate::default()
        );
        let event = NodeEvent {
            node: NodeId::new("zebrad-mainnet").unwrap(),
            value: 7u32,
        };
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            serde_json::json!({"node": "zebrad-mainnet", "value": 7})
        );
    }

    #[test]
    fn kinds_know_their_role_and_what_they_use() {
        for kind in NodeKind::ALL {
            assert!(NodeId::new(kind.slug()).is_some(), "{kind:?}");
            for used in kind.uses() {
                assert_ne!(*used, kind, "{kind:?} uses itself");
                assert!(used.uses().is_empty() || *used == NodeKind::Zaino);
            }
        }
        assert!(NodeKind::Zebrad.bundled());
        let explorers: Vec<NodeKind> = NodeKind::ALL
            .into_iter()
            .filter(|kind| kind.has_explorer())
            .collect();
        assert_eq!(explorers, [NodeKind::Zebrad, NodeKind::Zakura]);
        assert_eq!(
            NodeKind::ALL.iter().filter(|kind| kind.bundled()).count(),
            1
        );
        // Only the bundled kind can be added so far.
        for kind in NodeKind::ALL {
            assert_eq!(kind.addable(), kind.bundled(), "{kind:?}");
        }
        assert_eq!(NodeKind::Zaino.role(), NodeRole::Indexer);
        assert_eq!(
            NodeInfo::default_name(NodeKind::Zebrad, Network::Testnet),
            "Zebra · Testnet"
        );
        assert_eq!(
            NodeInfo::unique_default_name(NodeKind::Zebrad, Network::Testnet, ["Zebra · Mainnet"]),
            "Zebra · Testnet"
        );
        assert_eq!(
            NodeInfo::unique_default_name(
                NodeKind::Zebrad,
                Network::Testnet,
                ["Zebra · Testnet", "Zebra · Testnet 2"]
            ),
            "Zebra · Testnet 3"
        );
        for (name, default) in [
            ("Zebra · Testnet", true),
            ("Zebra · Testnet 3", true),
            ("Zebra · Testnet node", false),
            ("Zebra · Testnet ", false),
            ("Zebra · Mainnet", false),
            ("My node", false),
        ] {
            assert_eq!(
                NodeInfo::is_default_name(name, NodeKind::Zebrad, Network::Testnet),
                default,
                "{name:?}"
            );
        }
    }
}
