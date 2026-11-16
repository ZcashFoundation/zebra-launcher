//! The engine's cases as scripted rounds over a simulated chain, with a fake clock: rounds
//! every 5 s, as the actor runs them.

use std::time::{Duration, SystemTime};

use launcher_types::{
    Level, Network, NodeId, NodeKind, NodeProgram, NodeStats, Phase,
    herd::{ExcludedWhy, HerdState, HerdView, Relation, Side},
};

use super::{Engine, MemberFacts, Now, sim::SimChain};

/// Past NU7 on Mainnet: S is 25 s.
const H: u32 = 3_551_204;

struct Node {
    id: NodeId,
    name: String,
    kind: NodeKind,
    program: NodeProgram,
    phase: Phase,
    progress: f64,
}

struct Harness {
    engine: Engine,
    chain: SimChain,
    nodes: Vec<Node>,
    mono: Duration,
    wall: SystemTime,
}

fn id(index: usize) -> NodeId {
    NodeId::new(match index {
        0 => "zebrad-mainnet".to_owned(),
        n => format!("zebrad-mainnet-{}", n + 1),
    })
    .unwrap()
}

fn name(index: usize) -> String {
    match index {
        0 => "Zebra · Mainnet".to_owned(),
        n => format!("Zebra · Mainnet {}", n + 1),
    }
}

impl Harness {
    /// `count` stable zebrad nodes on the main chain at `height`.
    fn new(network: Network, count: usize, height: u32) -> Self {
        let mut chain = SimChain::new(42);
        let nodes = (0..count)
            .map(|index| {
                chain.put(&id(index), 0, height);
                Node {
                    id: id(index),
                    name: name(index),
                    kind: NodeKind::Zebrad,
                    program: NodeProgram::Bundled,
                    phase: Phase::Running,
                    progress: 1.0,
                }
            })
            .collect();
        Harness {
            engine: Engine::new(network),
            chain,
            nodes,
            mono: Duration::from_secs(1_000),
            wall: SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000),
        }
    }

    fn facts(&self) -> Vec<MemberFacts> {
        self.nodes
            .iter()
            .map(|node| {
                let stats = (node.phase == Phase::Running)
                    .then(|| self.chain.position(&node.id))
                    .flatten()
                    .map(|(_, height)| NodeStats {
                        height: u64::from(height),
                        estimated_height: u64::from(height),
                        best: Some(self.chain.tip(&node.id)),
                        verification_progress: node.progress,
                        peers: 8,
                        mempool_transactions: 0,
                        chain: "main".into(),
                        zebrad_version: "7.0.0".into(),
                        last_error: None,
                        eos: None,
                        updated_at: self.wall,
                    });
                MemberFacts {
                    id: node.id.clone(),
                    name: node.name.clone(),
                    kind: node.kind,
                    program: node.program,
                    phase: node.phase.clone(),
                    stats,
                }
            })
            .collect()
    }

    fn round(&mut self) -> HerdView {
        let facts = self.facts();
        let now = Now {
            mono: self.mono,
            wall: self.wall,
        };
        let chain = &self.chain;
        self.engine.round(facts, now, |probe| chain.answer(probe))
    }

    /// Rounds every 5 s for `secs`, calling `between` before each; the last view.
    fn run_with(&mut self, secs: u64, mut between: impl FnMut(&mut Self, u64)) -> HerdView {
        let mut view = None;
        let mut elapsed = 0;
        while elapsed <= secs {
            between(self, elapsed);
            view = Some(self.round());
            self.mono += Duration::from_secs(5);
            self.wall += Duration::from_secs(5);
            elapsed += 5;
        }
        view.expect("at least one round")
    }

    fn run(&mut self, secs: u64) -> HerdView {
        self.run_with(secs, |_, _| {})
    }

    fn height(&self, index: usize) -> u32 {
        self.chain.position(&id(index)).expect("placed").1
    }

    /// Moves the nodes in `indices` up one block on their branches.
    fn grow(&mut self, indices: &[usize]) {
        for &index in indices {
            let (branch, height) = self.chain.position(&id(index)).expect("placed");
            self.chain.put(&id(index), branch, height + 1);
        }
    }
}

fn relation(view: &HerdView, index: usize) -> Relation {
    view.node(&id(index)).expect("in the view").relation.clone()
}

#[test]
fn nodes_on_one_chain_agree_after_a_short_probation() {
    let mut herd = Harness::new(Network::Mainnet, 3, H);
    herd.chain.put(&id(2), 0, H - 1);
    let first = herd.round();
    assert_eq!(first.state, HerdState::Checking, "probation first");
    assert_eq!(first.headline, "Checking your Mainnet nodes…");
    let view = herd.run(10);
    assert_eq!(view.state, HerdState::Agreed);
    assert_eq!(view.level, Level::Healthy);
    assert_eq!(
        view.headline,
        "Your 3 Mainnet nodes agree on block 3,551,204"
    );
    assert_eq!(view.top.as_ref().map(|top| top.height), Some(H));
    assert_eq!(relation(&view, 2), Relation::InAgreement { behind: 1 });
    assert_eq!(view.node(&id(2)).unwrap().word, "1 block behind");
    assert_eq!(view.node(&id(0)).unwrap().word, "agrees");
    assert_eq!(view.spacing_secs, 25);
    assert_eq!(view.branches.len(), 1);
}

#[test]
fn a_node_behind_and_still_advancing_lags() {
    let mut herd = Harness::new(Network::Mainnet, 3, H);
    // Through probation at the tip first, then falling behind.
    herd.run(10);
    // Blocks arrive every 25 s; the third node gets one in three.
    let view = herd.run_with(250, |herd, elapsed| {
        if elapsed % 25 == 0 {
            herd.grow(&[0, 1]);
        }
        if elapsed % 75 == 0 {
            herd.grow(&[2]);
        }
    });
    let behind = herd.height(0) - herd.height(2);
    assert!(behind > 2);
    assert_eq!(relation(&view, 2), Relation::Lagging { blocks: behind });
    assert_eq!(view.state, HerdState::Degraded);
    assert_eq!(view.level, Level::Attention);
    assert_eq!(
        view.headline,
        format!(
            "2 of your 3 Mainnet nodes agree on block {}; Zebra · Mainnet 3 is {behind} blocks behind",
            super::verdict::thousands(herd.height(0))
        )
    );
}

#[test]
fn a_node_whose_tip_stops_while_the_others_move_stalls() {
    let mut herd = Harness::new(Network::Mainnet, 3, H);
    herd.run(10);
    let stuck_at = herd.height(2);
    let view = herd.run_with(150, |herd, elapsed| {
        if elapsed % 25 == 0 {
            herd.grow(&[0, 1]);
        }
    });
    assert_eq!(
        relation(&view, 2),
        Relation::Stalled {
            at: stuck_at,
            replaced_block: false
        }
    );
    assert_eq!(view.state, HerdState::Degraded);
    assert!(
        view.headline
            .ends_with("Zebra · Mainnet 3 is stuck at block 3,551,204"),
        "{}",
        view.headline
    );
    // Stuck for 24 S: a problem.
    let view = herd.run_with(600, |herd, elapsed| {
        if elapsed % 25 == 0 {
            herd.grow(&[0, 1]);
        }
    });
    assert_eq!(view.node(&id(2)).unwrap().level, Level::Problem);
}

#[test]
fn past_probation_a_falling_progress_changes_no_state() {
    // zebrad's progress is its height over one estimated from the clock: it drops for a node
    // that is stuck or behind. 0.9998 is about 700 blocks at this height.
    let low = 0.9998;
    let grow_two = |herd: &mut Harness, elapsed: u64| {
        if elapsed.is_multiple_of(25) {
            herd.grow(&[0, 1]);
        }
    };

    // A stalled node stays stalled, and becomes a problem.
    let mut herd = Harness::new(Network::Mainnet, 3, H);
    herd.run(10);
    let view = herd.run_with(150, grow_two);
    assert!(matches!(relation(&view, 2), Relation::Stalled { .. }));
    herd.nodes[2].progress = low;
    let view = herd.run_with(600, grow_two);
    assert_eq!(
        relation(&view, 2),
        Relation::Stalled {
            at: H,
            replaced_block: false
        }
    );
    assert_eq!(view.node(&id(2)).unwrap().level, Level::Problem);

    // A node behind and still advancing stays lagging.
    let mut herd = Harness::new(Network::Mainnet, 3, H);
    herd.run(10);
    herd.nodes[2].progress = low;
    let view = herd.run_with(250, |herd, elapsed| {
        grow_two(herd, elapsed);
        if elapsed % 75 == 0 {
            herd.grow(&[2]);
        }
    });
    let behind = herd.height(0) - herd.height(2);
    assert_eq!(relation(&view, 2), Relation::Lagging { blocks: behind });
    assert_eq!(view.level, Level::Attention);

    // A herd whose every node stopped stays stalled.
    let mut herd = Harness::new(Network::Mainnet, 2, H);
    herd.run(290);
    for node in &mut herd.nodes {
        node.progress = low;
    }
    let view = herd.run(20);
    assert_eq!(view.state, HerdState::Stalled);
}

#[test]
fn a_fork_is_found_and_named_with_its_height() {
    let mut herd = Harness::new(Network::Mainnet, 3, H);
    herd.run(10);
    // The third node follows a branch of its own from H - 2 and keeps growing it.
    let fork_at = H - 2;
    let branch = herd.chain.branch(fork_at);
    herd.chain.put(&id(2), branch, H);
    let view = herd.run_with(30, |herd, elapsed| {
        if elapsed % 10 == 0 {
            herd.grow(&[0, 1, 2]);
        }
    });
    assert_eq!(view.branches.len(), 2);
    assert_eq!(view.branches[1].fork_at, Some(fork_at));
    assert!(!view.branches[1].deep);
    let Relation::Forked { at, side, .. } = relation(&view, 2) else {
        panic!("{:?}", relation(&view, 2));
    };
    assert_eq!((at, side), (fork_at, Side::Minority));
    assert_eq!(view.state, HerdState::Split);
    assert_eq!(view.level, Level::Problem);
    assert_eq!(
        view.headline,
        format!(
            "Zebra · Mainnet 3 is on a different chain since block 3,551,202; your other 2 Mainnet nodes agree on block {}",
            super::verdict::thousands(herd.height(0))
        )
    );
    assert_eq!(relation(&view, 0), Relation::InAgreement { behind: 0 });
}

#[test]
fn a_one_round_blip_changes_nothing() {
    let mut herd = Harness::new(Network::Mainnet, 3, H);
    herd.run(10);
    let branch = herd.chain.branch(H - 2);
    herd.chain.put(&id(2), branch, H + 1);
    let blip = herd.round();
    assert_eq!(relation(&blip, 2), Relation::InAgreement { behind: 0 });
    assert_eq!(blip.state, HerdState::Agreed);
    herd.chain.put(&id(2), 0, H);
    herd.mono += Duration::from_secs(5);
    herd.wall += Duration::from_secs(5);
    let back = herd.round();
    assert_eq!(back.state, HerdState::Agreed);
}

#[test]
fn a_competing_block_is_a_race_and_settles() {
    let mut herd = Harness::new(Network::Mainnet, 3, H);
    herd.run(10);
    // Two nodes take X at H + 1, the third Y.
    herd.grow(&[0, 1]);
    let branch = herd.chain.branch(H + 1);
    herd.chain.put(&id(2), branch, H + 1);
    let view = herd.run(15);
    assert_eq!(relation(&view, 2), Relation::Racing { at: H + 1 });
    assert_eq!(view.state, HerdState::Agreed);
    assert_eq!(view.node(&id(2)).unwrap().word, "agrees");
    // The next block settles it: all on a child of X.
    herd.grow(&[0, 1]);
    herd.chain.put(&id(2), 0, H + 2);
    let view = herd.run(10);
    assert_eq!(relation(&view, 2), Relation::InAgreement { behind: 0 });
    assert_eq!(view.branches.len(), 1);
}

#[test]
fn a_node_still_holding_a_replaced_block_is_stuck_on_it() {
    let mut herd = Harness::new(Network::Mainnet, 3, H);
    herd.run(10);
    let branch = herd.chain.branch(H + 1);
    herd.chain.put(&id(2), branch, H + 1);
    let view = herd.run_with(150, |herd, elapsed| {
        if elapsed % 25 == 0 {
            herd.grow(&[0, 1]);
        }
    });
    assert_eq!(
        relation(&view, 2),
        Relation::Stalled {
            at: H + 1,
            replaced_block: true
        }
    );
    assert!(
        view.node(&id(2))
            .unwrap()
            .word
            .contains("which your other nodes replaced")
    );
}

#[test]
fn a_replaced_block_is_a_stall_also_where_no_side_has_a_majority() {
    // Two zebrads, a zebrad and a Zakura, and two zebrads beside a Zakura: the odd node keeps
    // the block the others replaced. It is stuck on it, and the others are not on a fork.
    for (count, zakura) in [(2, None), (2, Some(1)), (3, Some(2))] {
        let mut herd = Harness::new(Network::Mainnet, count, H);
        if let Some(index) = zakura {
            herd.nodes[index].kind = NodeKind::Zakura;
            herd.nodes[index].name = "Zakura · Mainnet".into();
        }
        herd.run(10);
        let odd = count - 1;
        let others: Vec<usize> = (0..odd).collect();
        let branch = herd.chain.branch(H + 1);
        herd.chain.put(&id(odd), branch, H + 1);
        let view = herd.run_with(150, |herd, elapsed| {
            if elapsed % 25 == 0 {
                herd.grow(&others);
            }
        });
        let case = format!("{count} nodes, Zakura {zakura:?}");
        assert_eq!(
            relation(&view, odd),
            Relation::Stalled {
                at: H + 1,
                replaced_block: true
            },
            "{case}"
        );
        for &index in &others {
            assert_eq!(
                relation(&view, index),
                Relation::InAgreement { behind: 0 },
                "{case}"
            );
        }
        assert_eq!(view.state, HerdState::Degraded, "{case}");
        assert_eq!(view.level, Level::Attention, "{case}");
        assert!(
            view.headline
                .ends_with("is stuck on block 3,551,205, which your other nodes replaced"),
            "{case}: {}",
            view.headline
        );
    }
}

#[test]
fn a_node_in_probation_with_a_competing_block_is_being_checked() {
    // First seen 5 blocks behind, the third node is in its 2-minute probation when it takes
    // the competing block at H + 1.
    let mut herd = Harness::new(Network::Mainnet, 3, H);
    herd.chain.put(&id(2), 0, H - 5);
    herd.run(10);
    herd.chain.put(&id(2), 0, H);
    herd.run(10);
    herd.grow(&[0, 1]);
    let branch = herd.chain.branch(H + 1);
    herd.chain.put(&id(2), branch, H + 1);
    let view = herd.run(30);
    let node = view.node(&id(2)).unwrap();
    assert_eq!(
        node.relation,
        Relation::CatchingUp {
            progress: 1.0,
            same_chain: None
        }
    );
    assert_eq!(node.word, "being checked");
    assert_eq!(node.level, Level::Neutral);
    assert_eq!(view.level, Level::Neutral);
    // A branch of its own, 3 blocks past the fork: another chain while it catches up.
    let branch = herd.chain.branch(H);
    herd.chain.put(&id(2), branch, H + 2);
    let view = herd.run(10);
    let node = view.node(&id(2)).unwrap();
    assert_eq!(
        node.relation,
        Relation::CatchingUp {
            progress: 1.0,
            same_chain: Some(false)
        }
    );
    assert_eq!(node.level, Level::Problem);

    // Testnet races up to 6 blocks: a 4-block one in probation is no other chain either.
    let mut herd = Harness::new(Network::Testnet, 3, 4_471_230);
    herd.chain.put(&id(2), 0, 4_471_225);
    herd.run(10);
    let branch = herd.chain.branch(4_471_229);
    herd.chain.put(&id(2), branch, 4_471_232);
    herd.grow(&[0, 1]);
    let view = herd.run(30);
    let node = view.node(&id(2)).unwrap();
    assert_eq!(
        node.relation,
        Relation::CatchingUp {
            progress: 1.0,
            same_chain: None
        }
    );
    assert_ne!(node.level, Level::Problem);
    assert_ne!(view.level, Level::Problem);
}

#[test]
fn a_restarting_node_counts_and_comes_back() {
    let mut herd = Harness::new(Network::Mainnet, 3, H);
    herd.run(10);
    herd.nodes[1].phase = Phase::Starting;
    let view = herd.round();
    assert_eq!(
        relation(&view, 1),
        Relation::Excluded {
            why: ExcludedWhy::Restarting
        }
    );
    assert_eq!(view.state, HerdState::Degraded);
    assert_eq!(
        view.headline,
        "2 of your 3 Mainnet nodes agree on block 3,551,204; Zebra · Mainnet 2 is restarting"
    );
    herd.nodes[1].phase = Phase::Running;
    let view = herd.run(15);
    assert_eq!(view.state, HerdState::Agreed);
    // A node stopped by the user is not counted.
    herd.nodes[2].phase = Phase::Stopped;
    let view = herd.round();
    assert_eq!(view.state, HerdState::Agreed);
    assert_eq!(
        view.headline,
        "Your 2 Mainnet nodes agree on block 3,551,204; Zebra · Mainnet 3 is stopped"
    );
    // Every node stopped: nothing is checked.
    for node in &mut herd.nodes {
        node.phase = Phase::Stopped;
    }
    let view = herd.round();
    assert_eq!(view.state, HerdState::Checking);
    assert_eq!(
        view.headline,
        "Not checking: your Mainnet nodes are stopped"
    );
}

#[test]
fn one_node_is_no_herd_and_one_at_the_tip_checks_alone() {
    let mut herd = Harness::new(Network::Mainnet, 1, H);
    let view = herd.run(15);
    assert_eq!(view.state, HerdState::Off);
    assert!(view.headline.is_empty());
    // A service beside it changes nothing: it has no vote.
    let mut with_zaino = Harness::new(Network::Mainnet, 2, H);
    with_zaino.nodes[1].kind = NodeKind::Zaino;
    let view = with_zaino.run(15);
    assert_eq!(view.state, HerdState::Off);
    assert_eq!(view.nodes.len(), 1, "a service is not in the herd");
    // A second witness catching up: one node checks the chain.
    let mut syncing = Harness::new(Network::Mainnet, 2, H);
    syncing.chain.put(&id(1), 0, 2_000_000);
    syncing.nodes[1].progress = 0.56;
    let view = syncing.run(15);
    assert_eq!(view.state, HerdState::Single);
    assert_eq!(
        relation(&view, 1),
        Relation::CatchingUp {
            progress: 0.56,
            same_chain: Some(true)
        }
    );
    assert_eq!(
        view.headline,
        "One Mainnet node checks the chain, at block 3,551,204; Zebra · Mainnet 2 is catching up (56.0 %)"
    );
    // Both catching up: nothing is through probation.
    syncing.nodes[0].progress = 0.5;
    let mut fresh = Harness::new(Network::Mainnet, 2, 2_000_000);
    fresh.nodes[0].progress = 0.5;
    fresh.nodes[1].progress = 0.5;
    let view = fresh.run(15);
    assert_eq!(view.state, HerdState::Checking);
    assert_eq!(view.headline, "Your Mainnet nodes are catching up");
}

#[test]
fn a_split_between_channels_is_two_implementations_disagreeing() {
    let mut herd = Harness::new(Network::Mainnet, 3, H);
    herd.nodes[2].program = NodeProgram::BundledBeta;
    herd.run(10);
    let branch = herd.chain.branch(H - 1);
    herd.chain.put(&id(2), branch, H + 1);
    let view = herd.run_with(30, |herd, elapsed| {
        if elapsed % 10 == 0 {
            herd.grow(&[0, 1, 2]);
        }
    });
    assert_eq!(view.state, HerdState::Split);
    let Relation::Forked { side, .. } = relation(&view, 2) else {
        panic!("{:?}", relation(&view, 2));
    };
    assert_eq!(side, Side::Implementations);
    assert!(matches!(
        relation(&view, 0),
        Relation::Forked {
            side: Side::Implementations,
            ..
        }
    ));
    assert_eq!(
        view.headline,
        "Zebra Stable and Zebra Beta disagree since block 3,551,203"
    );
}

#[test]
fn a_split_whose_other_side_stops_still_reads_as_a_sentence() {
    let mut herd = Harness::new(Network::Mainnet, 3, H);
    herd.nodes[2].program = NodeProgram::BundledBeta;
    herd.run(10);
    let branch = herd.chain.branch(H - 1);
    herd.chain.put(&id(2), branch, H + 1);
    let view = herd.run_with(30, |herd, elapsed| {
        if elapsed % 10 == 0 {
            herd.grow(&[0, 1, 2]);
        }
    });
    assert_eq!(view.state, HerdState::Split);
    // The Beta node stops: the Stable nodes show the split for the debounce, with no other
    // side to name.
    herd.nodes[2].phase = Phase::Stopped;
    let view = herd.run(5);
    assert_eq!(view.state, HerdState::Split);
    assert_eq!(
        view.headline,
        "Your Mainnet nodes disagree since block 3,551,203, and Zeal can't tell which is right"
    );
    let view = herd.run(15);
    assert_eq!(view.state, HerdState::Agreed);
    assert_eq!(
        view.headline,
        format!(
            "Your 2 Mainnet nodes agree on block {}; Zebra · Mainnet 3 is stopped",
            super::verdict::thousands(herd.height(0))
        )
    );
}

#[test]
fn a_split_against_several_implementations_names_both_sides() {
    let mut herd = Harness::new(Network::Mainnet, 3, H);
    herd.nodes[1].kind = NodeKind::Zakura;
    herd.nodes[1].name = "Zakura · Mainnet".into();
    herd.nodes[2].program = NodeProgram::BundledBeta;
    herd.run(10);
    let branch = herd.chain.branch(H - 1);
    herd.chain.put(&id(2), branch, H + 1);
    let view = herd.run_with(30, |herd, elapsed| {
        if elapsed % 10 == 0 {
            herd.grow(&[0, 1, 2]);
        }
    });
    assert_eq!(
        view.headline,
        "Zebra Stable and Zakura disagree with Zebra Beta since block 3,551,203"
    );
}

#[test]
fn two_nodes_that_split_cannot_name_the_odd_one() {
    // The same channel: no majority.
    let mut same = Harness::new(Network::Mainnet, 2, H);
    same.run(10);
    let branch = same.chain.branch(H - 2);
    same.chain.put(&id(1), branch, H + 1);
    let view = same.run_with(30, |herd, elapsed| {
        if elapsed % 10 == 0 {
            herd.grow(&[0, 1]);
        }
    });
    assert_eq!(view.state, HerdState::Split);
    assert_eq!(
        view.headline,
        "Your Mainnet nodes disagree since block 3,551,202, and Zeal can't tell which is right"
    );
    // Two channels, one node each: either may be the one cut off.
    let mut mixed = Harness::new(Network::Mainnet, 2, H);
    mixed.nodes[1].program = NodeProgram::BundledNightly;
    mixed.run(10);
    let branch = mixed.chain.branch(H - 2);
    mixed.chain.put(&id(1), branch, H + 1);
    let view = mixed.run_with(30, |herd, elapsed| {
        if elapsed % 10 == 0 {
            herd.grow(&[0, 1]);
        }
    });
    assert!(
        view.headline.starts_with(
            "Zebra · Mainnet and Zebra · Mainnet 2 disagree since block 3,551,202: one of them"
        ),
        "{}",
        view.headline
    );
}

#[test]
fn a_fork_past_the_rollback_window_is_deep() {
    let mut herd = Harness::new(Network::Mainnet, 3, H);
    herd.run(10);
    let branch = herd.chain.branch(H - 1_500);
    herd.chain.put(&id(2), branch, H);
    let view = herd.run(20);
    assert!(view.branches[1].deep);
    let Relation::Forked { deep, at, .. } = relation(&view, 2) else {
        panic!("{:?}", relation(&view, 2));
    };
    assert!(deep);
    assert!(at <= H - 1_000);
    assert!(
        view.node(&id(2))
            .unwrap()
            .sentence
            .contains("can't rejoin it by itself")
    );
}

#[test]
fn a_deep_fork_keeps_its_height_as_the_chains_grow() {
    let mut herd = Harness::new(Network::Mainnet, 3, H);
    herd.run(10);
    let branch = herd.chain.branch(H - 1_500);
    herd.chain.put(&id(2), branch, H);
    let view = herd.run(20);
    assert_eq!(view.branches[1].fork_at, Some(H - 1_000));
    let view = herd.run_with(250, |herd, elapsed| {
        if elapsed % 25 == 0 {
            herd.grow(&[0, 1, 2]);
        }
    });
    assert!(herd.height(2) >= H + 10);
    assert_eq!(view.branches[1].fork_at, Some(H - 1_000));
    assert_eq!(
        view.node(&id(2)).unwrap().word,
        "on a different chain since block 3,550,204 or earlier"
    );
}

#[test]
fn a_deep_fork_names_its_height_as_a_bound() {
    // The last node's chain splits 1,500 blocks back. The search stops 1,000 blocks below the
    // lower top, at block 3,550,204, so the fork is there or earlier.
    let deep = |count: usize, setup: &dyn Fn(&mut Harness)| -> HerdView {
        let mut herd = Harness::new(Network::Mainnet, count, H);
        setup(&mut herd);
        herd.run(10);
        let branch = herd.chain.branch(H - 1_500);
        herd.chain.put(&id(count - 1), branch, H);
        let view = herd.run(20);
        assert_eq!(view.state, HerdState::Split);
        assert!(view.branches[1].deep);
        view
    };
    let word = |view: &HerdView, index: usize| view.node(&id(index)).unwrap().word.clone();

    // A minority.
    let view = deep(3, &|_| {});
    assert_eq!(
        word(&view, 2),
        "on a different chain since block 3,550,204 or earlier"
    );
    assert_eq!(
        view.headline,
        "Zebra · Mainnet 3 is on a different chain since block 3,550,204 or earlier, and can't rejoin it by itself; your other 2 Mainnet nodes agree on block 3,551,204"
    );

    // No majority: two nodes on one channel.
    let view = deep(2, &|_| {});
    for index in 0..2 {
        assert_eq!(
            word(&view, index),
            "disagrees since block 3,550,204 or earlier"
        );
    }
    assert_eq!(
        view.headline,
        "Your Mainnet nodes disagree since block 3,550,204 or earlier, and Zeal can't tell which is right"
    );

    // Two nodes on two channels.
    let view = deep(2, &|herd| {
        herd.nodes[1].program = NodeProgram::BundledNightly
    });
    assert!(
        view.headline.starts_with(
            "Zebra · Mainnet and Zebra · Mainnet 2 disagree since block 3,550,204 or earlier: one of them"
        ),
        "{}",
        view.headline
    );

    // Two channels, and two implementations against one channel.
    let view = deep(3, &|herd| herd.nodes[2].program = NodeProgram::BundledBeta);
    assert_eq!(word(&view, 2), "disagrees since block 3,550,204 or earlier");
    assert_eq!(
        view.headline,
        "Zebra Stable and Zebra Beta disagree since block 3,550,204 or earlier"
    );
    let view = deep(3, &|herd| {
        herd.nodes[1].kind = NodeKind::Zakura;
        herd.nodes[1].name = "Zakura · Mainnet".into();
        herd.nodes[2].program = NodeProgram::BundledBeta;
    });
    assert_eq!(
        view.headline,
        "Zebra Stable and Zakura disagree with Zebra Beta since block 3,550,204 or earlier"
    );
}

#[test]
fn a_deep_fork_above_a_lower_exact_one_still_reads_as_a_bound() {
    // The third node's chain splits 1,500 blocks back: found only as block 3,550,204 or
    // earlier. The fourth, 600 blocks below the top, splits 1,400 back: found exactly, at
    // block 3,549,804. The lowest height named is the exact one, yet the deep fork may start
    // below it.
    let mut herd = Harness::new(Network::Mainnet, 4, H);
    herd.run(10);
    let deep = herd.chain.branch(H - 1_500);
    herd.chain.put(&id(2), deep, H);
    let shallow = herd.chain.branch(H - 1_400);
    herd.chain.put(&id(3), shallow, H - 600);
    let view = herd.run(20);
    assert_eq!(view.state, HerdState::Split);
    assert_eq!(view.branches.len(), 3);
    assert_eq!(view.branches[1].fork_at, Some(H - 1_000));
    assert!(view.branches[1].deep);
    assert_eq!(view.branches[2].fork_at, Some(H - 1_400));
    assert!(!view.branches[2].deep);
    assert_eq!(
        view.node(&id(3)).unwrap().word,
        "disagrees since block 3,549,804"
    );
    assert_eq!(
        view.headline,
        "Your Mainnet nodes disagree since block 3,549,804 or earlier, and Zeal can't tell which is right"
    );
}

#[test]
fn a_top_that_reorganizes_and_climbs_between_rounds_is_asked_again() {
    // The second node sits 250 blocks below the first, which is asked for its hash there.
    let mut herd = Harness::new(Network::Mainnet, 2, H);
    herd.chain.put(&id(1), 0, H - 250);
    let view = herd.run(200);
    assert_eq!(view.branches.len(), 1);
    // Before the next round the first node reorganizes from H - 500 and climbs 1,500 blocks:
    // its old answer at H - 250 is on the chain it left.
    herd.chain.fork(&id(0), H - 500, H + 1_500);
    let view = herd.round();
    assert_eq!(view.branches.len(), 2);
    assert_eq!(view.branches[1].fork_at, Some(H - 500));
    assert_eq!(
        relation(&view, 1),
        Relation::CatchingUp {
            progress: 1.0,
            same_chain: Some(false)
        }
    );
}

#[test]
fn a_node_whose_chain_changed_while_it_was_stopped_is_asked_again() {
    // The third node sits 2,000 blocks below the others, and the first is asked for its hash
    // there: an answer deep enough to keep.
    let mut herd = Harness::new(Network::Mainnet, 3, H);
    herd.chain.put(&id(2), 0, H - 2_000);
    let view = herd.run(30);
    assert_eq!(view.branches.len(), 1);
    // The user stops the first node and gives it a data folder with a chain that split from
    // the others' 3,000 blocks ago.
    herd.nodes[0].phase = Phase::Stopped;
    herd.run(10);
    herd.chain.fork(&id(0), H - 3_000, H + 5);
    herd.nodes[0].phase = Phase::Running;
    let view = herd.run(10);
    assert_eq!(view.branches.len(), 2);
    assert_eq!(view.branches[0].nodes, [id(1), id(2)]);
    assert_eq!(view.branches[1].nodes, [id(0)]);
}

#[test]
fn probes_that_fail_twice_make_a_node_unknown() {
    let mut herd = Harness::new(Network::Mainnet, 3, H);
    herd.chain.put(&id(2), 0, H - 1);
    herd.run(10);
    // The top nodes take a block, and the first, which the others are placed against, stops
    // answering: the node below cannot be placed, and keeps its state.
    herd.chain.fail(&id(0), true);
    herd.grow(&[0, 1]);
    let view = herd.round();
    assert_eq!(relation(&view, 2), Relation::InAgreement { behind: 1 });
    herd.mono += Duration::from_secs(5);
    herd.wall += Duration::from_secs(5);
    let view = herd.round();
    assert_eq!(relation(&view, 0), Relation::Unknown);
    assert_eq!(view.node(&id(0)).unwrap().word, "not answering");
    // Set aside, it holds up no one: the third node is placed against the second.
    herd.mono += Duration::from_secs(5);
    herd.wall += Duration::from_secs(5);
    let view = herd.round();
    assert_eq!(relation(&view, 2), Relation::InAgreement { behind: 2 });
    assert_eq!(view.state, HerdState::Degraded);
    assert_eq!(
        view.headline,
        "2 of your 3 Mainnet nodes agree on block 3,551,205; Zebra · Mainnet is not answering"
    );
    // Answering again, it is asked again after a while, and is back.
    herd.chain.fail(&id(0), false);
    let view = herd.run(45);
    assert_eq!(view.state, HerdState::Agreed);
}

#[test]
fn a_sudden_genesis_tip_is_unknown_for_the_round() {
    let mut herd = Harness::new(Network::Mainnet, 3, H);
    herd.run(10);
    herd.chain.put(&id(2), 0, 0);
    let view = herd.round();
    assert_eq!(relation(&view, 2), Relation::Unknown);
    assert_eq!(view.branches.len(), 1, "never placed as a fork");
}

#[test]
fn testnet_races_up_to_six_blocks() {
    let mut herd = Harness::new(Network::Testnet, 3, 4_471_230);
    herd.run(10);
    let branch = herd.chain.branch(4_471_229);
    herd.chain.put(&id(2), branch, 4_471_233);
    herd.grow(&[0, 1]);
    let view = herd.run(30);
    assert_eq!(relation(&view, 2), Relation::Racing { at: 4_471_229 });
    assert_eq!(view.state, HerdState::Agreed);
}

#[test]
fn the_spacing_follows_nu7() {
    let mut before = Harness::new(Network::Mainnet, 2, 3_500_000);
    let view = before.run(10);
    assert_eq!((view.spacing_secs, view.effective_spacing_secs), (75, 75));
    let mut after = Harness::new(Network::Mainnet, 2, H);
    let view = after.run(10);
    assert_eq!((view.spacing_secs, view.effective_spacing_secs), (25, 25));
}

#[test]
fn the_herd_stalls_after_twelve_spacings_without_a_block_but_not_after_a_wake() {
    let mut herd = Harness::new(Network::Mainnet, 2, H);
    let view = herd.run(290);
    assert_eq!(view.state, HerdState::Agreed);
    let view = herd.run(20);
    assert_eq!(view.state, HerdState::Stalled);
    assert_eq!(view.level, Level::Attention);
    assert!(
        view.headline
            .starts_with("No new block on your Mainnet nodes for"),
        "{}",
        view.headline
    );
    let view = herd.run(300);
    assert_eq!(view.level, Level::Problem);
    // A wake: an hour by the wall clock in one round. The herd gets its grace again.
    herd.wall += Duration::from_secs(3_600);
    let view = herd.round();
    assert_eq!(view.state, HerdState::Agreed);
}

#[test]
fn the_time_every_node_was_stopped_is_not_a_herd_stall() {
    let mut herd = Harness::new(Network::Mainnet, 2, H);
    let view = herd.run_with(400, |herd, elapsed| {
        if elapsed % 25 == 0 {
            herd.grow(&[0, 1]);
        }
    });
    assert_eq!(view.state, HerdState::Agreed);
    // Stop all for 20 min, then Start all: the nodes come back on the tips they had.
    for node in &mut herd.nodes {
        node.phase = Phase::Stopped;
    }
    herd.run(1_200);
    for node in &mut herd.nodes {
        node.phase = Phase::Running;
    }
    let view = herd.run(120);
    assert_eq!(view.state, HerdState::Agreed, "{}", view.headline);
    // The quiet time counts from their return.
    let view = herd.run(200);
    assert_eq!(view.state, HerdState::Stalled);
    assert_eq!(view.level, Level::Attention);
    assert_eq!(
        view.headline,
        "No new block on your Mainnet nodes for 5 min"
    );
}

#[test]
fn the_nodes_order_does_not_matter() {
    let build = |reverse: bool| {
        let mut herd = Harness::new(Network::Mainnet, 4, H);
        herd.chain.put(&id(3), 0, H - 1);
        let branch = herd.chain.branch(H - 3);
        herd.chain.put(&id(2), branch, H);
        if reverse {
            herd.nodes.reverse();
        }
        herd.run(40)
    };
    let (forward, reversed) = (build(false), build(true));
    assert_eq!(forward.state, reversed.state);
    assert_eq!(forward.headline, reversed.headline);
    for index in 0..4 {
        assert_eq!(relation(&forward, index), relation(&reversed, index));
    }
}

#[test]
fn observers_and_validators_are_shown_but_not_counted() {
    let mut herd = Harness::new(Network::Mainnet, 3, H);
    herd.nodes[2].kind = NodeKind::Zcashd;
    let view = herd.run(15);
    assert_eq!(
        relation(&view, 2),
        Relation::Excluded {
            why: ExcludedWhy::NotCounted
        }
    );
    assert_eq!(view.state, HerdState::Agreed);
    assert_eq!(
        view.headline,
        "Your 2 Mainnet nodes agree on block 3,551,204"
    );
}

#[test]
fn a_round_asks_at_most_its_budget_and_the_next_round_goes_on() {
    let mut herd = Harness::new(Network::Mainnet, 8, H);
    herd.run(10);
    for index in 1..8 {
        let branch = herd.chain.branch(H - 600 - index as u32 * 50);
        herd.chain.put(&id(index), branch, H);
    }
    let mut asked = 0;
    let facts = herd.facts();
    let now = Now {
        mono: herd.mono,
        wall: herd.wall,
    };
    let chain = &herd.chain;
    let view = herd.engine.round(facts, now, |probe| {
        asked += 1;
        chain.answer(probe)
    });
    assert!(asked <= super::MAX_PROBES, "{asked} probes");
    assert!(view.branches.len() > 1);
    // Later rounds finish the searches from what the first one learned.
    let view = herd.run(60);
    assert_eq!(view.branches.len(), 8);
    assert!(
        view.branches[1..]
            .iter()
            .all(|branch| branch.fork_at.is_some())
    );
}
