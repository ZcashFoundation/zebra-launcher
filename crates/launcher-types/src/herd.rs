//! The herd's agreement on the wire (docs/HERD.md): for each network, whether the nodes Zeal
//! runs are on one chain, which of them are behind, stuck or on another branch, and the
//! sentence the overview shows. `launcher-herd` computes a [`HerdView`] from the nodes' tips;
//! the backend publishes it as the `herd` event and the `get_herd` snapshot.
//!
//! Heights are `u32`, as zebrad's are; a fork's height is always the first block where the
//! two branches differ, the block after their last common one.

use std::{fmt, time::SystemTime};

use serde::{Deserialize, Serialize};

use crate::{Level, Network, NodeId, ZebradChannel};

/// A block: its height and its hash, hex as zebrad writes it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BlockRef {
    pub height: u32,
    pub hash: String,
}

/// Whether a node counts when Zeal says "k of n agree".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Vote {
    /// Its own peers and its own validation: zebrad on any channel, Zakura. Counts.
    Witness,
    /// Its own consensus code, but it hears only the node it follows (the zcashd
    /// compatibility build): shown, never counted.
    Validator,
    /// Shown, never counted: a node Zeal cannot rely on alone.
    Observer,
    /// Not a full node: Zaino, Zallet.
    None,
}

/// The consensus code a node runs. Two families that split are two implementations that
/// disagree, not one node that is wrong; zebrad's channels count as families of their own,
/// since their code differs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "family", rename_all = "snake_case")]
pub enum Family {
    Zebra { channel: ZebradChannel },
    Zakura,
    Zcashd,
    Zebend,
    Other,
}

impl Family {
    /// The family's name: "Zebra", "Zebra Beta", "Zakura". `channels` says whether to name
    /// zebrad's channel, which matters only when the nodes run more than one.
    pub fn label(self, channels: bool) -> String {
        match self {
            Family::Zebra { channel } if channels => format!("Zebra {}", channel.label()),
            Family::Zebra { .. } => "Zebra".to_owned(),
            Family::Zakura => "Zakura".to_owned(),
            Family::Zcashd => "zcashd".to_owned(),
            Family::Zebend => "Zebend".to_owned(),
            Family::Other => "another implementation".to_owned(),
        }
    }
}

/// The state of one network's herd. When several hold, the first of Off, Checking, Split,
/// Stalled, Single, Degraded and Agreed applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HerdState {
    /// Fewer than two witnesses: nothing to compare, and no herd words anywhere.
    Off,
    /// No witness is through probation yet.
    Checking,
    /// One witness at the tip; the others are catching up or not running.
    Single,
    /// Every running witness agrees.
    Agreed,
    /// A majority agrees; the others are behind, stuck, catching up, restarting or silent.
    Degraded,
    /// A witness is on another chain.
    Split,
    /// No witness at the tip saw a new block for 12 block spacings.
    Stalled,
}

/// Why a node takes no part in this round.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExcludedWhy {
    /// Stopped, or failed and waiting for the user.
    Stopped,
    /// Starting for the first time this session.
    Starting,
    /// Starting again, after a crash or a restart: it still counts in "of n".
    Restarting,
    Stopping,
    /// Halted at its release's end of support.
    EndOfSupport,
    /// Running, but its RPC has not answered recently.
    NoStats,
    /// Not a witness ([`Vote`]).
    NotCounted,
}

/// Which side of a split a forked node is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    /// A majority of the witnesses is on the other branch.
    Minority,
    /// No branch has a majority: Zeal can't tell which is right.
    NoMajority,
    /// The split lines up with implementations (or zebrad channels): they disagree about
    /// the rules, and Zeal counts no nodes.
    Implementations,
}

/// Where a node stands against the herd's main branch. Heights of forks and races are the
/// first block that differs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "relation", rename_all = "snake_case")]
pub enum Relation {
    /// Not part of this round.
    Excluded { why: ExcludedWhy },
    /// Not through probation yet: syncing, or not yet at the tip for long. `same_chain` is
    /// what its one placement read said, `None` before it.
    CatchingUp {
        progress: f64,
        same_chain: Option<bool>,
    },
    /// On the main branch, at most 2 blocks below its top.
    InAgreement { behind: u32 },
    /// On a competing block at `at`, which the next block settles: normal.
    Racing { at: u32 },
    /// On the main branch, more than 2 blocks behind for a while, and still advancing.
    Lagging { blocks: u32 },
    /// Its tip has not moved while the main branch gained blocks. `replaced_block`: it holds
    /// a block at `at` the other nodes replaced.
    Stalled { at: u32, replaced_block: bool },
    /// On a branch of its own since `at`: `own` blocks long, against `main` on the main
    /// branch. `deep`: the branches differ for more than 1,000 blocks, past zebrad's rollback
    /// window, so the fork is at `at` or below it and the node can't rejoin by itself.
    Forked {
        at: u32,
        own: u32,
        main: u32,
        side: Side,
        deep: bool,
    },
    /// Its probes failed for two rounds, or it suddenly reports the genesis block.
    Unknown,
}

/// One node in a network's herd.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NodeInHerd {
    pub id: NodeId,
    pub name: String,
    pub vote: Vote,
    pub family: Family,
    /// Its best block, from its latest statistics.
    pub tip: Option<BlockRef>,
    pub relation: Relation,
    pub level: Level,
    /// A few words for its card: "agrees", "15 blocks behind", "stuck at block 3,551,190".
    pub word: String,
    /// The same with its name, for the herd's sentence: "Zebra · Mainnet 2 is 15 blocks
    /// behind".
    pub sentence: String,
}

/// A chain some of the nodes follow.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Branch {
    /// The first height where it differs from the main branch; `None` for the main branch.
    pub fork_at: Option<u32>,
    /// Whether the fork search found no common block within 1,000 blocks.
    pub deep: bool,
    /// Its highest block.
    pub top: BlockRef,
    /// The nodes on it, the one at the top first.
    pub nodes: Vec<NodeId>,
}

/// One network's herd, as the overview shows it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HerdView {
    pub network: Network,
    pub state: HerdState,
    pub level: Level,
    /// The main branch's top.
    pub top: Option<BlockRef>,
    /// The target block spacing at the top, in seconds: 75 before NU7, 25 from it.
    pub spacing_secs: u32,
    /// The spacing the time rules use: the measured one, between the target and three times
    /// it.
    pub effective_spacing_secs: u32,
    /// One sentence: "Your 3 Mainnet nodes agree on block 3,551,204". Empty while Off.
    pub headline: String,
    /// The network's full nodes, in the node list's order.
    pub nodes: Vec<NodeInHerd>,
    /// The main branch first.
    pub branches: Vec<Branch>,
    pub updated_at: SystemTime,
}

impl HerdView {
    /// The view of a network without a herd.
    pub fn off(network: Network, now: SystemTime) -> Self {
        HerdView {
            network,
            state: HerdState::Off,
            level: Level::Neutral,
            top: None,
            spacing_secs: 0,
            effective_spacing_secs: 0,
            headline: String::new(),
            nodes: Vec::new(),
            branches: Vec::new(),
            updated_at: now,
        }
    }

    /// The node with `id`, if it is in this herd.
    pub fn node(&self, id: &NodeId) -> Option<&NodeInHerd> {
        self.nodes.iter().find(|node| &node.id == id)
    }
}

impl fmt::Display for HerdState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            HerdState::Off => "off",
            HerdState::Checking => "checking",
            HerdState::Single => "single",
            HerdState::Agreed => "agreed",
            HerdState::Degraded => "degraded",
            HerdState::Split => "split",
            HerdState::Stalled => "stalled",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn views_travel_with_tagged_relations() {
        let id = NodeId::new("zebrad-mainnet").unwrap();
        let view = HerdView {
            nodes: vec![NodeInHerd {
                id: id.clone(),
                name: "Zebra · Mainnet".into(),
                vote: Vote::Witness,
                family: Family::Zebra {
                    channel: ZebradChannel::Beta,
                },
                tip: Some(BlockRef {
                    height: 3_551_204,
                    hash: "00ab".into(),
                }),
                relation: Relation::Forked {
                    at: 3_551_201,
                    own: 4,
                    main: 3,
                    side: Side::Minority,
                    deep: false,
                },
                level: Level::Problem,
                word: "on a different chain since block 3,551,201".into(),
                sentence: String::new(),
            }],
            ..HerdView::off(Network::Mainnet, SystemTime::UNIX_EPOCH)
        };
        let json = serde_json::to_value(&view).unwrap();
        assert_eq!(json["state"], "off");
        assert_eq!(json["nodes"][0]["relation"]["relation"], "forked");
        assert_eq!(json["nodes"][0]["relation"]["side"], "minority");
        assert_eq!(json["nodes"][0]["family"]["family"], "zebra");
        let back: HerdView = serde_json::from_value(json).unwrap();
        assert_eq!(back, view);
        assert!(back.node(&id).is_some());
    }

    #[test]
    fn families_name_zebrads_channel_only_when_it_matters() {
        let beta = Family::Zebra {
            channel: ZebradChannel::Beta,
        };
        assert_eq!(beta.label(false), "Zebra");
        assert_eq!(beta.label(true), "Zebra Beta");
        assert_eq!(Family::Zakura.label(true), "Zakura");
    }
}
