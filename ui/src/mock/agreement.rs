//! The herd's agreement in the mock (docs/HERD.md): the real engine
//! ([`launcher_herd::herd::Engine`]) runs a round every 5 s per network over the mock's nodes,
//! as the backend's actors do, and answers its probes from a scripted chain. Every node's
//! best block comes from [`best`]: on the main chain, or on a branch of its own where a
//! scenario puts it.
//!
//! `?herd&agree=…` picks a scenario. It adds Zebra · Mainnet 2 (and for `channels` Zebra ·
//! Mainnet 3 on Beta), keeps zebrad at the tip, and puts Zakura · Mainnet at the tip too
//! (except for `split` and `channels`, where it is still catching up). Before the first view,
//! ten minutes of rounds run at once for the nodes at the tip, so a scenario's state shows from
//! the start.
//!
//! | `agree=` | Mainnet |
//! | --- | --- |
//! | (no value) | the three nodes agree |
//! | `behind` | Zebra · Mainnet 2 fell 6 blocks behind and is still advancing: lagging |
//! | `stall` | Zebra · Mainnet 2 is stuck where it was 16 blocks ago |
//! | `fork` | Zebra · Mainnet 2 has been on a chain of its own for about 4 blocks |
//! | `race` | Zebra · Mainnet 2 holds a competing block at every new height: normal |
//! | `split` | Zebra · Mainnet and Zebra · Mainnet 2 split, and no side has a majority |
//! | `impl` | Zakura · Mainnet is on a chain of its own: Zebra and Zakura disagree |
//! | `channels` | Zebra · Mainnet 3, on Beta, is on a chain of its own: Zebra Stable and Zebra Beta disagree |
//! | `restart` | Zebra · Mainnet 2 is restarting |
//! | `deepfork` | Zebra · Mainnet 2 is on a chain that split 1,500 blocks ago |
//! | `silent` | Zebra · Mainnet 2, a block ahead, does not answer probes |
//!
//! Testnet's two nodes agree. Without a scenario the Mainnet nodes are still catching up.

use std::{
    cell::RefCell,
    collections::BTreeMap,
    rc::Rc,
    time::{Duration, SystemTime},
};

use launcher_herd::herd::{Engine, MemberFacts, Now, Probe, ProbeResult};
use launcher_types::{
    Network, NodeId, NodeStats, Phase, SYNCED_PROGRESS,
    herd::{BlockRef, HerdView},
};
use leptos::prelude::*;

use super::{mock, nodes};
use crate::clock;

/// The mock's scenarios ([`super::Flags`]'s `agree`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Agree {
    Agreed,
    Behind,
    Stall,
    Fork,
    Race,
    Split,
    Impl,
    Channels,
    Restart,
    DeepFork,
    Silent,
}

impl Agree {
    /// The value of `agree=`; no value or an unknown one is plain agreement.
    pub(super) fn parse(value: &str) -> Self {
        match value {
            "behind" | "lag" => Agree::Behind,
            "stall" => Agree::Stall,
            "fork" => Agree::Fork,
            "race" => Agree::Race,
            "split" => Agree::Split,
            "impl" => Agree::Impl,
            "channels" => Agree::Channels,
            "restart" => Agree::Restart,
            "deepfork" => Agree::DeepFork,
            "silent" => Agree::Silent,
            _ => Agree::Agreed,
        }
    }

    /// Whether Zakura · Mainnet is at the tip in this scenario, or still catching up.
    pub(super) fn zakura_at_tip(self) -> bool {
        !matches!(self, Agree::Split | Agree::Channels)
    }
}

/// How much history the rounds before the first view cover.
const WARMUP: Duration = Duration::from_secs(600);
/// The rounds' interval, as the backend's.
const ROUND: Duration = Duration::from_secs(5);
/// The block spacing at the mock's tip, past NU7.
const SPACING: i64 = 25;
/// When, in seconds from the first view, a scenario's fork began.
const FORK_SINCE: i64 = -100;
/// When `stall`'s node stopped.
const STALL_SINCE: i64 = -400;
/// How far below the tip `behind`'s node is, and since when: it was at the tip before, so it
/// passed its probation.
const BEHIND: u32 = 6;
const BEHIND_SINCE: i64 = -300;

#[derive(Default)]
struct State {
    started: bool,
    engines: Vec<Engine>,
    views: Vec<HerdView>,
    /// Each node's height and branch as its newest statistics give them, for the probes.
    positions: BTreeMap<NodeId, (u32, Option<u32>)>,
    /// The time of the first view: a scenario's times count from it.
    zero: Option<SystemTime>,
}

type Handlers = RefCell<Vec<Rc<dyn Fn(HerdView)>>>;

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State {
        engines: vec![Engine::new(Network::Mainnet), Engine::new(Network::Testnet)],
        views: vec![
            HerdView::off(Network::Mainnet, SystemTime::UNIX_EPOCH),
            HerdView::off(Network::Testnet, SystemTime::UNIX_EPOCH),
        ],
        ..State::default()
    });
    static HANDLERS: Handlers = const { RefCell::new(Vec::new()) };
}

fn scenario() -> Option<Agree> {
    mock().flags.agree
}

/// The height the main zebrad is at when the scenario's clock is at 0.
fn top_zero() -> u32 {
    mock()
        .flags
        .at_tip()
        .and_then(|height| u32::try_from(height).ok())
        .unwrap_or(0)
}

/// Seconds since the first view; 0 before it.
fn seconds() -> i64 {
    let zero = STATE.with_borrow(|state| state.zero);
    zero.and_then(|zero| clock::now().duration_since(zero).ok())
        .map_or(0, |since| since.as_secs() as i64)
}

/// The main chain's top at `t` seconds: a block every 25 s before the first view, the main
/// zebrad's own height after it.
fn top_at(t: i64) -> u32 {
    if t < 0 {
        return top_zero().saturating_sub(((-t + SPACING - 1) / SPACING) as u32);
    }
    mock()
        .stats
        .with_untracked(|stats| stats.as_ref().map(|stats| stats.height))
        .and_then(|height| u32::try_from(height).ok())
        .unwrap_or_else(|| top_zero() + (t / SPACING) as u32)
}

/// Where the scenario puts `node` when the top is `top` at `t`: its height, and the first
/// height of its own branch. `None` for a node the scenario leaves alone.
fn script(node: &str, top: u32, t: i64) -> Option<(u32, Option<u32>)> {
    let agree = scenario()?;
    let fork = || top_at(FORK_SINCE) + 1;
    let forked = t >= FORK_SINCE;
    Some(match (agree, node) {
        (Agree::Behind, "zebrad-mainnet-2") if t >= BEHIND_SINCE => {
            (top.saturating_sub(BEHIND), None)
        }
        (Agree::Stall, "zebrad-mainnet-2") => (top.min(top_at(STALL_SINCE)), None),
        (Agree::Fork | Agree::Split, "zebrad-mainnet-2") if forked => (top, Some(fork())),
        (Agree::Race, "zebrad-mainnet-2") => (top, Some(top)),
        (Agree::Impl, "zakura-mainnet") if forked => (top, Some(fork())),
        (Agree::Channels, "zebrad-mainnet-3") if forked => (top, Some(fork())),
        (Agree::DeepFork, "zebrad-mainnet-2") => (top, Some(top_zero().saturating_sub(1_500))),
        (Agree::Silent, "zebrad-mainnet-2") => (top + 1, None),
        (agree, "zakura-mainnet") if !agree.zakura_at_tip() => return None,
        (_, "zebrad-mainnet-2" | "zebrad-mainnet-3" | "zakura-mainnet") => (top, None),
        _ => return None,
    })
}

/// The height the scenario gives `node` now, if it scripts it.
pub(super) fn scripted_height(node: &NodeId) -> Option<u64> {
    let t = seconds();
    script(node.as_str(), top_at(t), t).map(|(height, _)| u64::from(height))
}

/// A block hash of the scripted chain: the main chain's, or a branch's from its first height.
fn hash(network: Network, branch: Option<u32>, height: u32) -> String {
    let branch = branch.filter(|first| height >= *first).map_or(0, u64::from);
    let seed = match network {
        Network::Mainnet => 0x6d61_696e,
        Network::Testnet => 0x7465_7374,
    };
    (0..4u64)
        .map(|part| {
            let mut z = seed ^ (branch << 34) ^ (u64::from(height) << 2) ^ part;
            z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            format!("{:016x}", z ^ (z >> 31))
        })
        .collect()
}

/// `node`'s best block at `height` on `network`, on the branch the scenario puts it on. It is
/// remembered, so the probes answer from the same chain as the statistics.
pub(super) fn best(node: &NodeId, network: Network, height: u64) -> Option<BlockRef> {
    let height = u32::try_from(height).ok()?;
    let t = seconds();
    let branch = script(node.as_str(), top_at(t), t).and_then(|(_, branch)| branch);
    STATE.with_borrow_mut(|state| {
        state.positions.insert(node.clone(), (height, branch));
    });
    Some(BlockRef {
        height,
        hash: hash(network, branch, height),
    })
}

/// A node's answer to a probe, from where it is.
fn answer(
    network: Network,
    positions: &BTreeMap<NodeId, (u32, Option<u32>)>,
    probe: &Probe,
) -> ProbeResult {
    if scenario() == Some(Agree::Silent) && probe.node.as_str() == "zebrad-mainnet-2" {
        return ProbeResult::Failed;
    }
    match positions.get(&probe.node) {
        Some((height, _)) if probe.height > *height => ProbeResult::AboveTip,
        Some((_, branch)) => ProbeResult::Hash(hash(network, *branch, probe.height)),
        None => ProbeResult::Failed,
    }
}

fn index(network: Network) -> usize {
    match network {
        Network::Mainnet => 0,
        Network::Testnet => 1,
    }
}

/// The engine's facts of the nodes on `network`, as the mock's API gives them.
fn facts(network: Network) -> Vec<MemberFacts> {
    nodes::snapshot()
        .into_iter()
        .filter(|(info, _, _)| info.network == network)
        .map(|(info, status, stats)| MemberFacts {
            id: info.id,
            name: info.name,
            kind: info.kind,
            program: info.program,
            phase: status.phase,
            stats,
        })
        .collect()
}

fn publish(view: HerdView) {
    let changed = STATE.with_borrow_mut(|state| {
        let slot = &mut state.views[index(view.network)];
        let changed = HerdView {
            updated_at: view.updated_at,
            ..slot.clone()
        } != view;
        *slot = view.clone();
        changed
    });
    if changed {
        let handlers: Vec<Rc<dyn Fn(HerdView)>> = HANDLERS.with_borrow(Clone::clone);
        for handler in handlers {
            handler(view.clone());
        }
    }
}

fn mono(now: SystemTime) -> Duration {
    let zero = STATE.with_borrow(|state| state.zero).unwrap_or(now);
    WARMUP + now.duration_since(zero).unwrap_or_default()
}

/// One live round on `network`, published.
fn round(network: Network) {
    let facts = facts(network);
    let wall = clock::now();
    let now = Now {
        mono: mono(wall),
        wall,
    };
    let view = STATE.with_borrow_mut(|state| {
        let State {
            engines, positions, ..
        } = state;
        engines[index(network)].round(facts, now, |probe| answer(network, positions, probe))
    });
    publish(view);
}

/// The rounds of the ten minutes before the first view on `network`, for its nodes at the
/// tip: those the scenario places, and any other a block every 25 s below where it is now.
/// Nodes still syncing start with the live rounds.
fn warm_up(network: Network, zero: SystemTime) {
    let members: Vec<MemberFacts> = facts(network)
        .into_iter()
        .filter(|fact| {
            script(fact.id.as_str(), 0, 0).is_some()
                || fact
                    .stats
                    .as_ref()
                    .is_some_and(|stats| stats.verification_progress >= SYNCED_PROGRESS)
        })
        .collect();
    let steps = WARMUP.as_secs() as i64 / ROUND.as_secs() as i64;
    for step in 0..steps {
        let t = -(WARMUP.as_secs() as i64) + step * ROUND.as_secs() as i64;
        let blocks_ago = ((-t + SPACING - 1) / SPACING) as u32;
        let top = top_at(t);
        let wall = zero - Duration::from_secs((-t) as u64);
        let mut positions = BTreeMap::new();
        let facts: Vec<MemberFacts> = members
            .iter()
            .map(|member| {
                let now = member
                    .stats
                    .as_ref()
                    .and_then(|stats| u32::try_from(stats.height).ok())
                    .unwrap_or(0);
                let (height, branch) = script(member.id.as_str(), top, t)
                    .unwrap_or((now.saturating_sub(blocks_ago), None));
                positions.insert(member.id.clone(), (height, branch));
                // Running, also a node that is restarting now.
                MemberFacts {
                    phase: Phase::Running,
                    stats: Some(NodeStats {
                        height: u64::from(height),
                        estimated_height: u64::from(height),
                        best: Some(BlockRef {
                            height,
                            hash: hash(network, branch, height),
                        }),
                        verification_progress: 1.0,
                        peers: 8,
                        mempool_transactions: 0,
                        chain: String::new(),
                        zebrad_version: String::new(),
                        last_error: None,
                        eos: None,
                        updated_at: wall,
                    }),
                    ..member.clone()
                }
            })
            .collect();
        let now = Now {
            mono: Duration::from_secs((WARMUP.as_secs() as i64 + t) as u64),
            wall,
        };
        STATE.with_borrow_mut(|state| {
            state.engines[index(network)]
                .round(facts, now, |probe| answer(network, &positions, probe));
        });
    }
}

/// Starts the rounds once the main zebrad answers (or after 10 s without it): the warm-up
/// for a scenario, then a round per network every 5 s.
fn start() {
    if STATE.with_borrow_mut(|state| std::mem::replace(&mut state.started, true)) {
        return;
    }
    wait(0);
}

fn wait(attempt: u32) {
    let answering = mock()
        .stats
        .with_untracked(|stats| stats.as_ref().is_some_and(|stats| stats.best.is_some()));
    if !answering && attempt < 10 {
        set_timeout(move || wait(attempt + 1), Duration::from_secs(1));
        return;
    }
    let zero = clock::now();
    STATE.with_borrow_mut(|state| state.zero = Some(zero));
    for network in [Network::Mainnet, Network::Testnet] {
        warm_up(network, zero);
    }
    let rounds = || {
        round(Network::Mainnet);
        round(Network::Testnet);
    };
    rounds();
    set_interval(rounds, ROUND);
}

/// Calls `handler` with every change of a network's herd, as the `herd` event does.
pub fn on_herd(handler: impl Fn(HerdView) + 'static) {
    HANDLERS.with_borrow_mut(|handlers| handlers.push(Rc::new(handler)));
    start();
}

/// Each network's herd now, Mainnet first: the `get_herd` snapshot.
pub async fn get_herd() -> Vec<HerdView> {
    start();
    super::sleep(50).await;
    STATE.with_borrow(|state| state.views.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenarios_parse_with_plain_agreement_as_the_default() {
        assert_eq!(Agree::parse("fork"), Agree::Fork);
        assert_eq!(Agree::parse("lag"), Agree::Behind);
        assert_eq!(Agree::parse(""), Agree::Agreed);
        assert_eq!(Agree::parse("bogus"), Agree::Agreed);
        assert!(!Agree::Split.zakura_at_tip());
        assert!(Agree::Fork.zakura_at_tip());
    }

    #[test]
    fn branches_share_the_main_chain_below_their_first_height() {
        let main = hash(Network::Mainnet, None, 100);
        assert_eq!(hash(Network::Mainnet, Some(101), 100), main);
        assert_ne!(hash(Network::Mainnet, Some(100), 100), main);
        assert_ne!(hash(Network::Testnet, None, 100), main);
        assert_eq!(main.len(), 64);
    }
}
