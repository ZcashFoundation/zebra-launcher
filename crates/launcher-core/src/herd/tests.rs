//! The agreement actors with scripted sources over a simulated chain, under a paused clock.

use std::{
    collections::BTreeMap,
    net::{Ipv4Addr, SocketAddr},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime},
};

use launcher_herd::herd::{Probe, ProbeResult, sim::SimChain};
use launcher_types::{
    Network, NodeId, NodeStats, Phase, Status,
    herd::{HerdState, HerdView, Relation},
};
use tokio::sync::watch;

use super::{
    agreement::{HerdConfig, MemberInput, spawn_with},
    tips::TipSource,
};
use crate::nodes::{NodeLayout, NodeSpec};

const H: u32 = 3_551_204;

/// Answers from the shared simulated chain, after `delay`, counting the probes in flight.
#[derive(Clone)]
struct FakeTips {
    node: NodeId,
    chain: watch::Receiver<SimChain>,
    delay: Duration,
    in_flight: Arc<AtomicUsize>,
    most: Arc<AtomicUsize>,
}

impl TipSource for FakeTips {
    async fn hash_at(&self, height: u32, _timeout: Duration) -> ProbeResult {
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.most.fetch_max(now, Ordering::SeqCst);
        tokio::time::sleep(self.delay).await;
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        self.chain.borrow().answer(&Probe {
            node: self.node.clone(),
            height,
        })
    }
}

struct Node {
    id: NodeId,
    spec: watch::Sender<NodeSpec>,
    status: watch::Sender<Status>,
    stats: watch::Sender<Option<NodeStats>>,
    most: Arc<AtomicUsize>,
}

struct Fixture {
    chain: watch::Sender<SimChain>,
    nodes: Vec<Node>,
    members: watch::Sender<Vec<MemberInput>>,
}

fn id(index: usize) -> NodeId {
    NodeId::new(format!("zebrad-node-{index}")).unwrap()
}

fn addr(index: usize) -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, 20_000 + index as u16))
}

impl Fixture {
    /// `networks.len()` running zebrad nodes at `H` on the main chain; the node at `slow`
    /// answers after 10 s, past the probe timeout.
    fn start(networks: &[Network], slow: Option<usize>) -> (Self, super::HerdHandle) {
        Self::start_with(networks, slow, false)
    }

    /// [`Fixture::start`], where building the first source panics when `panic_once` is set.
    fn start_with(
        networks: &[Network],
        slow: Option<usize>,
        panic_once: bool,
    ) -> (Self, super::HerdHandle) {
        let mut chain = SimChain::new(9);
        for index in 0..networks.len() {
            chain.put(&id(index), 0, H);
        }
        let (chain_tx, chain_rx) = watch::channel(chain);
        let mut nodes = Vec::new();
        let mut members = Vec::new();
        let mut sources = BTreeMap::new();
        for (index, network) in networks.iter().enumerate() {
            let spec = NodeSpec {
                id: id(index),
                name: format!("Zebra {index}"),
                layout: NodeLayout::Folder,
                ..NodeSpec::first(*network)
            };
            let (spec_tx, spec_rx) = watch::channel(spec);
            let (status_tx, status_rx) = watch::channel(Status {
                phase: Phase::Running,
                rpc_addr: Some(addr(index)),
                ..Status::default()
            });
            let (stats_tx, stats_rx) = watch::channel(None);
            let most = Arc::new(AtomicUsize::new(0));
            sources.insert(
                addr(index),
                FakeTips {
                    node: id(index),
                    chain: chain_rx.clone(),
                    delay: if Some(index) == slow {
                        Duration::from_secs(10)
                    } else {
                        Duration::from_millis(20)
                    },
                    in_flight: Arc::new(AtomicUsize::new(0)),
                    most: most.clone(),
                },
            );
            members.push(MemberInput {
                id: id(index),
                serial: index as u64,
                spec: spec_rx,
                status: status_rx,
                stats: stats_rx,
                cookie_file: PathBuf::from("/nonexistent/.cookie"),
            });
            nodes.push(Node {
                id: id(index),
                spec: spec_tx,
                status: status_tx,
                stats: stats_tx,
                most,
            });
        }
        let (members_tx, members_rx) = watch::channel(members);
        let fixture = Fixture {
            chain: chain_tx,
            nodes,
            members: members_tx,
        };
        fixture.publish();
        let panicked = AtomicBool::new(false);
        let handle = spawn_with(members_rx, HerdConfig::default(), move |addr, _| {
            if panic_once && !panicked.swap(true, Ordering::SeqCst) {
                panic!("a source that fails once");
            }
            sources.get(&addr).cloned()
        });
        (fixture, handle)
    }

    /// Every node's statistics from the chain, as the monitor publishes them.
    fn publish(&self) {
        let chain = self.chain.borrow();
        for node in &self.nodes {
            let Some((_, height)) = chain.position(&node.id) else {
                continue;
            };
            node.stats.send_replace(Some(NodeStats {
                height: u64::from(height),
                estimated_height: u64::from(height),
                best: Some(chain.tip(&node.id)),
                verification_progress: 1.0,
                peers: 8,
                mempool_transactions: 0,
                chain: "main".into(),
                zebrad_version: "7.0.0".into(),
                last_error: None,
                eos: None,
                updated_at: SystemTime::now(),
            }));
        }
    }
}

async fn until(
    view: &mut watch::Receiver<HerdView>,
    what: &str,
    done: impl Fn(&HerdView) -> bool,
) -> HerdView {
    tokio::time::timeout(Duration::from_secs(600), async {
        loop {
            if done(&view.borrow_and_update()) {
                return view.borrow().clone();
            }
            view.changed().await.expect("the actor runs");
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{what} in time: {:?}", view.borrow()))
}

#[tokio::test(start_paused = true)]
async fn nodes_agree_and_a_fork_is_found_through_their_probes() {
    let (fixture, herd) = Fixture::start(&[Network::Mainnet; 3], None);
    let mut view = herd.watch(Network::Mainnet);
    let agreed = until(&mut view, "agreement", |view| {
        view.state == HerdState::Agreed
    })
    .await;
    assert_eq!(
        agreed.headline,
        "Your 3 Mainnet nodes agree on block 3,551,204"
    );
    // The third node goes its own way from H - 2 and builds on it.
    fixture.chain.send_modify(|chain| {
        let branch = chain.branch(H - 2);
        chain.put(&id(2), branch, H + 1);
        chain.put(&id(0), 0, H + 1);
        chain.put(&id(1), 0, H + 1);
    });
    fixture.publish();
    let split = until(&mut view, "the split", |view| {
        view.state == HerdState::Split
    })
    .await;
    assert_eq!(split.branches[1].fork_at, Some(H - 2));
    assert!(matches!(
        split.node(&id(2)).unwrap().relation,
        Relation::Forked { at, .. } if at == H - 2
    ));
    assert!(
        split
            .headline
            .starts_with("Zebra 2 is on a different chain since block 3,551,202"),
        "{}",
        split.headline
    );
    // The snapshot has both networks, the Testnet one without a herd.
    let views = herd.views();
    assert_eq!(views.len(), 2);
    assert_eq!(views[1].network, Network::Testnet);
    assert_eq!(views[1].state, HerdState::Off);
}

#[tokio::test(start_paused = true)]
async fn probes_are_two_at_a_time_per_node() {
    let (fixture, herd) = Fixture::start(&[Network::Mainnet; 4], None);
    let mut view = herd.watch(Network::Mainnet);
    until(&mut view, "agreement", |view| {
        view.state == HerdState::Agreed
    })
    .await;
    // Three nodes on deep branches of their own: many probes to the main node at once.
    fixture.chain.send_modify(|chain| {
        for index in 1..4 {
            let branch = chain.branch(H - 700 - index as u32 * 40);
            chain.put(&id(index), branch, H);
        }
    });
    fixture.publish();
    let split = until(&mut view, "the forks", |view| {
        view.state == HerdState::Split
            && view.branches.len() == 4
            && view.branches[1..].iter().all(|b| b.fork_at.is_some())
    })
    .await;
    assert!(split.branches[1..].iter().all(|branch| !branch.deep));
    for node in &fixture.nodes {
        assert!(
            node.most.load(Ordering::SeqCst) <= 2,
            "{} had {} probes in flight",
            node.id,
            node.most.load(Ordering::SeqCst)
        );
    }
    assert_eq!(fixture.nodes[0].most.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn a_node_that_does_not_answer_holds_up_no_one() {
    // The highest node answers after 10 s, past the 3 s timeout: every other tip is asked of
    // it, fails, and after two rounds it is set aside.
    let (fixture, herd) = Fixture::start(&[Network::Mainnet; 4], Some(3));
    fixture
        .chain
        .send_modify(|chain| chain.put(&id(3), 0, H + 1));
    fixture.publish();
    let mut view = herd.watch(Network::Mainnet);
    let view = until(&mut view, "the others agree", |view| {
        view.state == HerdState::Degraded
            && view.node(&id(3)).map(|node| &node.relation) == Some(&Relation::Unknown)
    })
    .await;
    assert_eq!(
        view.headline,
        "3 of your 4 Mainnet nodes agree on block 3,551,204; Zebra 3 is not answering"
    );
}

#[tokio::test(start_paused = true)]
async fn each_network_has_its_own_herd_and_follows_the_node_list() {
    let (fixture, herd) = Fixture::start(
        &[Network::Mainnet, Network::Mainnet, Network::Testnet],
        None,
    );
    let mut mainnet = herd.watch(Network::Mainnet);
    let view = until(&mut mainnet, "agreement", |view| {
        view.state == HerdState::Agreed
    })
    .await;
    assert_eq!(
        view.nodes.len(),
        2,
        "the Testnet node is not in the Mainnet herd"
    );
    assert_eq!(herd.watch(Network::Testnet).borrow().state, HerdState::Off);
    // A stopped node leaves the count at once.
    fixture.nodes[1].status.send_modify(|status| {
        status.phase = Phase::Stopped;
        status.rpc_addr = None;
    });
    let view = until(&mut mainnet, "the stop", |view| {
        matches!(
            view.node(&id(1)).map(|node| &node.relation),
            Some(Relation::Excluded { .. })
        )
    })
    .await;
    assert_eq!(view.state, HerdState::Single, "one node checks the chain");
    // A node moved to Testnet joins that herd; the Mainnet one has a single node left.
    fixture.nodes[1]
        .spec
        .send_modify(|spec| spec.network = Network::Testnet);
    fixture.nodes[1].status.send_modify(|status| {
        status.phase = Phase::Running;
        status.rpc_addr = Some(addr(1));
    });
    fixture.publish();
    let mut testnet = herd.watch(Network::Testnet);
    until(&mut testnet, "a Testnet herd", |view| {
        view.state == HerdState::Agreed && view.nodes.len() == 2
    })
    .await;
    until(&mut mainnet, "no Mainnet herd", |view| {
        view.state == HerdState::Off
    })
    .await;
    // A node that leaves the list leaves its herd.
    fixture
        .members
        .send_modify(|members| members.retain(|member| member.id != id(2)));
    until(&mut testnet, "the removal", |view| view.nodes.len() == 1).await;
}

#[tokio::test(start_paused = true)]
async fn an_actor_that_panics_is_started_again() {
    // The Mainnet actor panics in its first round, when it builds its first source.
    let (_fixture, herd) = Fixture::start_with(&[Network::Mainnet; 3], None, true);
    let mut view = herd.watch(Network::Mainnet);
    let agreed = until(&mut view, "agreement after the restart", |view| {
        view.state == HerdState::Agreed
    })
    .await;
    assert_eq!(
        agreed.headline,
        "Your 3 Mainnet nodes agree on block 3,551,204"
    );
}
