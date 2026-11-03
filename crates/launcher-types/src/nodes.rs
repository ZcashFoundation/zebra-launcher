//! Nodes: the processes Zeal runs. Each is one implementation ([`NodeKind`]) of a Zcash full
//! node or of a service that works with one, on one [`Network`]. See docs/MULTI-NODE.md.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::Network;

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
    /// The original C++ node, deprecated.
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

    /// The kinds a node of this kind can use, for a service: Zaino reads a full node. Empty
    /// for kinds that stand alone.
    pub fn uses(self) -> &'static [NodeKind] {
        match self {
            NodeKind::Zaino => &[NodeKind::Zebrad, NodeKind::Zakura, NodeKind::Zcashd],
            NodeKind::Zallet => &[NodeKind::Zebrad, NodeKind::Zakura, NodeKind::Zaino],
            _ => &[],
        }
    }

    /// Whether Zeal ships the kind's binary. Every other kind runs a program the user has.
    pub fn bundled(self) -> bool {
        self == NodeKind::Zebrad
    }

    /// One sentence for the add-node dialog: what the kind is.
    pub fn summary(self) -> &'static str {
        match self {
            NodeKind::Zebrad => "The Zcash Foundation's node. Bundled with Zeal.",
            NodeKind::Zakura => {
                "A Zebra fork built for scale, with block pruning and snapshots for a fast start."
            }
            NodeKind::Zcashd => "The original Zcash node, deprecated in favour of Zebra.",
            NodeKind::Zebend => {
                "An independent node written in Bend. Young and unaudited: not for funds."
            }
            NodeKind::Zaino => "An indexer that serves light wallets from a full node.",
            NodeKind::Zallet => "The wallet of the Z3 stack, replacing zcashd's wallet.",
            NodeKind::Generic => "Any other program: you give its command line.",
        }
    }
}

impl fmt::Display for NodeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// What everyone lists about a node: who it is, not how it is doing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeInfo {
    pub id: NodeId,
    /// The name people see, editable: "Zebra · Mainnet" by default.
    pub name: String,
    pub kind: NodeKind,
    pub network: Network,
    /// For a service, the node it uses.
    pub depends_on: Option<NodeId>,
}

impl NodeInfo {
    /// The default name of a node: its kind and network, "Zebra · Mainnet".
    pub fn default_name(kind: NodeKind, network: Network) -> String {
        format!("{} · {network}", kind.label())
    }
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
    fn kinds_know_their_role_and_what_they_use() {
        for kind in NodeKind::ALL {
            assert!(NodeId::new(kind.slug()).is_some(), "{kind:?}");
            for used in kind.uses() {
                assert_ne!(*used, kind, "{kind:?} uses itself");
                assert!(used.uses().is_empty() || *used == NodeKind::Zaino);
            }
        }
        assert!(NodeKind::Zebrad.bundled());
        assert_eq!(
            NodeKind::ALL.iter().filter(|kind| kind.bundled()).count(),
            1
        );
        assert_eq!(NodeKind::Zaino.role(), NodeRole::Indexer);
        assert_eq!(
            NodeInfo::default_name(NodeKind::Zebrad, Network::Testnet),
            "Zebra · Testnet"
        );
    }
}
