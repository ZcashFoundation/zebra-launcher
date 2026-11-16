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
