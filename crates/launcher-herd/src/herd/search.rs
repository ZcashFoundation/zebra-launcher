//! Placement and the fork search (docs/HERD.md, A round and The fork search), sans-IO: both
//! read block hashes from [`Answers`] and say which probes they still need. The engine runs
//! them again after every batch of answers; each run is cheap, and answers already known are
//! not asked again.

use std::collections::{BTreeMap, BTreeSet};

use launcher_types::{NodeId, herd::BlockRef};

use super::spacing::MAX_REORG;

/// A question for one node: the hash of the block at `height` on its best chain
/// (`getblockhash`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Probe {
    pub node: NodeId,
    pub height: u32,
}

/// A node's answer to a [`Probe`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProbeResult {
    Hash(String),
    /// The height is above the node's tip (zebrad's −32602).
    AboveTip,
    /// zebrad's −8.
    NotFound,
    /// An error or a timeout: it decides nothing.
    Failed,
}

/// What a step needs or found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome<T> {
    Done(T),
    /// These probes first.
    Need(Vec<Probe>),
    /// A probe it needs failed this round.
    Failed,
}

/// The block hashes known of each node's best chain. Answers well below a node's tip outlive
/// the round; the rest are dropped when the node's tip changes.
#[derive(Clone, Debug, Default)]
pub struct Answers {
    hashes: BTreeMap<(NodeId, u32), String>,
    /// Probes that failed this round.
    failed: BTreeSet<(NodeId, u32)>,
    /// This round's tips: a node's hash at its own tip needs no probe.
    tips: BTreeMap<NodeId, BlockRef>,
    /// The tip each node's kept answers were read under.
    read_under: BTreeMap<NodeId, BlockRef>,
}

/// The most answers kept; past it, the oldest nodes' are dropped.
const MAX_ANSWERS: usize = 4_096;

enum Look<'a> {
    Hash(&'a str),
    Failed,
    Need(Probe),
}

impl Answers {
    /// Starts a round with these tips: a node whose tip changed loses its answers within
    /// [`MAX_REORG`] of the new tip, since its best chain may have changed there.
    pub fn begin(&mut self, tips: impl IntoIterator<Item = (NodeId, BlockRef)>) {
        self.failed.clear();
        self.tips = tips.into_iter().collect();
        for (node, tip) in &self.tips {
            if self.read_under.get(node) != Some(tip) {
                let keep_below = tip.height.saturating_sub(MAX_REORG);
                self.hashes
                    .retain(|(owner, height), _| owner != node || *height < keep_below);
                self.read_under.insert(node.clone(), tip.clone());
            }
        }
        if self.hashes.len() > MAX_ANSWERS {
            self.hashes.clear();
        }
    }

    /// Records an answer. A hash is kept; anything else fails the probe for this round.
    pub fn record(&mut self, probe: Probe, result: ProbeResult) {
        let key = (probe.node, probe.height);
        match result {
            ProbeResult::Hash(hash) => {
                self.hashes.insert(key, hash);
            }
            ProbeResult::AboveTip | ProbeResult::NotFound | ProbeResult::Failed => {
                self.failed.insert(key);
            }
        }
    }

    /// Whether `node`'s probes failed this round.
    pub fn failed_for(&self, node: &NodeId) -> bool {
        self.failed.iter().any(|(owner, _)| owner == node)
    }

    fn look(&self, node: &NodeId, height: u32) -> Look<'_> {
        if let Some(tip) = self.tips.get(node)
            && tip.height == height
        {
            return Look::Hash(&tip.hash);
        }
        let key = (node.clone(), height);
        if let Some(hash) = self.hashes.get(&key) {
            return Look::Hash(hash);
        }
        if self.failed.contains(&key) {
            return Look::Failed;
        }
        Look::Need(Probe {
            node: node.clone(),
            height,
        })
    }

    /// Whether `a` and `b` have the same block at `height`.
    fn agree(&self, a: &NodeId, b: &NodeId, height: u32) -> Outcome<bool> {
        match (self.look(a, height), self.look(b, height)) {
            (Look::Hash(x), Look::Hash(y)) => Outcome::Done(x == y),
            (Look::Failed, _) | (_, Look::Failed) => Outcome::Failed,
            (left, right) => Outcome::Need(
                [left, right]
                    .into_iter()
                    .filter_map(|look| match look {
                        Look::Need(probe) => Some(probe),
                        _ => None,
                    })
                    .collect(),
            ),
        }
    }

    /// Whether `tip` is on the best chain of `node`, whose tip is higher.
    pub fn holds(&self, node: &NodeId, tip: &BlockRef) -> Outcome<bool> {
        match self.look(node, tip.height) {
            Look::Hash(hash) => Outcome::Done(hash == tip.hash),
            Look::Failed => Outcome::Failed,
            Look::Need(probe) => Outcome::Need(vec![probe]),
        }
    }
}

/// A chain some nodes are on, as placement found it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placed {
    /// The node at its top.
    pub top_node: NodeId,
    pub top: BlockRef,
    /// Every node on it with its height, the top node first.
    pub members: Vec<(NodeId, u32)>,
}

/// What placement found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Placement {
    /// Largest first: the most nodes, then the highest top, then the smallest top node.
    pub branches: Vec<Placed>,
    /// Nodes whose placement needed a probe that failed this round.
    pub unplaced: Vec<NodeId>,
    /// Probes still needed; while there are any, the placement is incomplete.
    pub needs: Vec<Probe>,
}

/// Places each distinct tip, highest first (then by node count, then by id): on a branch
/// whose top node has the tip's hash at its height, or on a branch of its own. Equal hashes
/// need no probe, and height never decides by itself.
pub fn place(answers: &Answers, tips: &[(NodeId, BlockRef)]) -> Placement {
    // Nodes with the same tip, as groups.
    let mut groups: Vec<(BlockRef, Vec<NodeId>)> = Vec::new();
    for (node, tip) in tips {
        match groups.iter_mut().find(|(block, _)| block == tip) {
            Some((_, nodes)) => nodes.push(node.clone()),
            None => groups.push((tip.clone(), vec![node.clone()])),
        }
    }
    for (_, nodes) in &mut groups {
        nodes.sort();
    }
    groups.sort_by(|(a, an), (b, bn)| {
        b.height
            .cmp(&a.height)
            .then(bn.len().cmp(&an.len()))
            .then(an[0].cmp(&bn[0]))
    });

    let mut placement = Placement::default();
    'groups: for (tip, nodes) in groups {
        let mut failed = false;
        sort_largest_first(&mut placement.branches);
        for index in 0..placement.branches.len() {
            let branch = &placement.branches[index];
            if branch.top.height <= tip.height {
                // As high or lower, with another hash: not this branch.
                continue;
            }
            match answers.holds(&branch.top_node, &tip) {
                Outcome::Done(true) => {
                    let branch = &mut placement.branches[index];
                    branch
                        .members
                        .extend(nodes.iter().map(|node| (node.clone(), tip.height)));
                    continue 'groups;
                }
                Outcome::Done(false) => {}
                Outcome::Need(probes) => {
                    // Largest branch first; every later tip depends on where this one goes.
                    placement.needs = probes;
                    return placement;
                }
                Outcome::Failed => failed = true,
            }
        }
        if failed {
            placement.unplaced.extend(nodes);
            continue;
        }
        placement.branches.push(Placed {
            top_node: nodes[0].clone(),
            top: tip.clone(),
            members: nodes
                .iter()
                .map(|node| (node.clone(), tip.height))
                .collect(),
        });
    }
    sort_largest_first(&mut placement.branches);
    placement
}

fn sort_largest_first(branches: &mut [Placed]) {
    branches.sort_by(|a, b| {
        b.members
            .len()
            .cmp(&a.members.len())
            .then(b.top.height.cmp(&a.top.height))
            .then(a.top_node.cmp(&b.top_node))
    });
}

/// Where two branches split.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fork {
    /// The first height where they differ: one above their last common block. For a deep
    /// fork, a lower bound.
    pub at: u32,
    /// They differ even [`MAX_REORG`] blocks below the lower top.
    pub deep: bool,
}

/// The heights the search tries below `hi`, before bisecting: a walk of three, then gallops of
/// 4, 8, 16 and so on, down to `floor`.
fn search_heights(hi: u32, floor: u32) -> Vec<u32> {
    let mut heights = Vec::new();
    let mut offset = 0u32;
    let mut step = 1u32;
    loop {
        offset = offset.saturating_add(step);
        let height = hi.saturating_sub(offset).max(floor);
        heights.push(height);
        if height <= floor {
            return heights;
        }
        if offset >= 3 {
            step = if step == 1 { 4 } else { step.saturating_mul(2) };
        }
    }
}

/// Finds where the chains of `a` and `b` split, given that they differ at `hi` (the lower of
/// their tops, where placement found them apart). `hint` is the pair's last fork, tried
/// first. Two chains that agree at a height agree at every height below it, since each block
/// commits to its parent, so the bisection is exact. At most about 20 steps of two probes.
pub fn fork_search(
    answers: &Answers,
    a: &NodeId,
    b: &NodeId,
    hi: u32,
    hint: Option<u32>,
) -> Outcome<Fork> {
    if hi == 0 {
        return Outcome::Done(Fork { at: 0, deep: true });
    }
    let floor = hi.saturating_sub(MAX_REORG);
    if let Some(at) = hint.filter(|at| (1..=hi).contains(at)) {
        let differs = match answers.agree(a, b, at) {
            Outcome::Done(agree) => !agree,
            other => return map_pending(other),
        };
        if differs {
            match answers.agree(a, b, at - 1) {
                Outcome::Done(true) => {
                    return Outcome::Done(Fork {
                        at,
                        deep: at <= floor,
                    });
                }
                Outcome::Done(false) => {}
                other => return map_pending(other),
            }
        }
    }
    let mut differs_at = hi;
    let mut agrees_at = None;
    for height in search_heights(hi, floor) {
        match answers.agree(a, b, height) {
            Outcome::Done(true) => {
                agrees_at = Some(height);
                break;
            }
            Outcome::Done(false) => differs_at = height,
            other => return map_pending(other),
        }
    }
    let Some(mut low) = agrees_at else {
        return Outcome::Done(Fork {
            at: differs_at,
            deep: true,
        });
    };
    while differs_at - low > 1 {
        let mid = low + (differs_at - low) / 2;
        match answers.agree(a, b, mid) {
            Outcome::Done(true) => low = mid,
            Outcome::Done(false) => differs_at = mid,
            other => return map_pending(other),
        }
    }
    Outcome::Done(Fork {
        at: differs_at,
        deep: false,
    })
}

fn map_pending<T, U>(outcome: Outcome<T>) -> Outcome<U> {
    match outcome {
        Outcome::Need(probes) => Outcome::Need(probes),
        Outcome::Failed | Outcome::Done(_) => Outcome::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::herd::sim::SimChain;

    fn id(text: &str) -> NodeId {
        NodeId::new(text).unwrap()
    }

    /// Runs `step` against `chain`, answering every probe, and returns the result and the
    /// probes asked.
    fn drive<T>(
        chain: &SimChain,
        answers: &mut Answers,
        step: impl Fn(&Answers) -> Outcome<T>,
    ) -> (Outcome<T>, Vec<Probe>) {
        let mut asked = Vec::new();
        loop {
            match step(answers) {
                Outcome::Need(probes) => {
                    for probe in probes {
                        assert!(!asked.contains(&probe), "asked twice: {probe:?}");
                        let result = chain.answer(&probe);
                        answers.record(probe.clone(), result);
                        asked.push(probe);
                    }
                }
                other => return (other, asked),
            }
        }
    }

    #[test]
    fn the_search_walks_gallops_and_bisects() {
        assert_eq!(search_heights(100, 0)[..5], [99, 98, 97, 93, 85]);
        assert_eq!(search_heights(10, 5), [9, 8, 7, 5]);
        assert_eq!(*search_heights(5_000, 4_000).last().unwrap(), 4_000);
    }

    #[test]
    fn the_fork_search_finds_the_fork_on_random_chains() {
        // SplitMix64, as `updates::Jitter` uses: the same trees on every run.
        let mut seed = 0x5eed_u64;
        let mut next = move || {
            seed = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = seed;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        };
        let (a, b) = (id("node-a"), id("node-b"));
        for _ in 0..2_000 {
            let depth = 1 + (next() % 1_200) as u32;
            let main_len = (next() % 30) as u32;
            let base = 3_000_000 + (next() % 100_000) as u32;
            let fork_at = base - depth + 1;
            let mut chain = SimChain::new(7);
            chain.put(&a, 0, base + main_len);
            chain.fork(&b, fork_at, base);
            let mut answers = Answers::default();
            answers.begin([(a.clone(), chain.tip(&a)), (b.clone(), chain.tip(&b))]);
            let hi = base;
            let (outcome, asked) = drive(&chain, &mut answers, |answers| {
                fork_search(answers, &a, &b, hi, None)
            });
            let Outcome::Done(fork) = outcome else {
                panic!("no result for a fork at {fork_at}");
            };
            assert!(
                asked.iter().all(|probe| probe.height < hi),
                "asked above the lower tip"
            );
            assert!(
                asked.len() <= 44,
                "{} probes for depth {depth}",
                asked.len()
            );
            if depth > MAX_REORG {
                // The fork is at or below the height reported.
                assert!(fork.deep, "depth {depth}");
                assert!(fork_at <= fork.at);
            } else {
                assert_eq!(
                    fork,
                    Fork {
                        at: fork_at,
                        deep: false
                    },
                    "depth {depth}"
                );
            }
            // The next round starts from the remembered fork: four probes at most.
            let mut again = Answers::default();
            again.begin([(a.clone(), chain.tip(&a)), (b.clone(), chain.tip(&b))]);
            let (outcome, asked) = drive(&chain, &mut again, |answers| {
                fork_search(answers, &a, &b, hi, Some(fork.at))
            });
            assert_eq!(outcome, Outcome::Done(fork));
            if !fork.deep {
                assert!(asked.len() <= 4, "{asked:?}");
            }
        }
    }

    #[test]
    fn placement_groups_equal_tips_and_asks_the_highest_branch() {
        let (a, b, c, d) = (id("a"), id("b"), id("c"), id("d"));
        let mut chain = SimChain::new(1);
        chain.put(&a, 0, 1_000);
        chain.put(&b, 0, 1_000);
        chain.put(&c, 0, 998);
        chain.fork(&d, 996, 999);
        let tips: Vec<(NodeId, BlockRef)> = [&d, &c, &b, &a]
            .iter()
            .map(|node| ((*node).clone(), chain.tip(node)))
            .collect();
        let mut answers = Answers::default();
        answers.begin(tips.clone());
        let mut asked = Vec::new();
        let placement = loop {
            let placement = place(&answers, &tips);
            if placement.needs.is_empty() {
                break placement;
            }
            for probe in placement.needs {
                answers.record(probe.clone(), chain.answer(&probe));
                asked.push(probe);
            }
        };
        assert_eq!(placement.branches.len(), 2);
        let main = &placement.branches[0];
        assert_eq!(main.top_node, a);
        assert_eq!(
            main.members,
            [(a.clone(), 1_000), (b.clone(), 1_000), (c.clone(), 998)]
        );
        assert_eq!(placement.branches[1].members, [(d.clone(), 999)]);
        // One probe each for D and C; A and B share a hash and need none.
        assert_eq!(asked.len(), 2, "{asked:?}");
        // The order of the nodes does not matter.
        let mut reversed = tips.clone();
        reversed.reverse();
        assert_eq!(place(&answers, &reversed), placement);
    }

    #[test]
    fn a_failed_probe_decides_nothing() {
        let (a, b) = (id("a"), id("b"));
        let mut chain = SimChain::new(1);
        chain.put(&a, 0, 1_000);
        chain.put(&b, 0, 990);
        let tips = vec![(a.clone(), chain.tip(&a)), (b.clone(), chain.tip(&b))];
        let mut answers = Answers::default();
        answers.begin(tips.clone());
        let placement = place(&answers, &tips);
        assert_eq!(
            placement.needs,
            [Probe {
                node: a.clone(),
                height: 990
            }]
        );
        answers.record(placement.needs[0].clone(), ProbeResult::Failed);
        let placement = place(&answers, &tips);
        assert_eq!(placement.unplaced, std::slice::from_ref(&b));
        assert_eq!(placement.branches.len(), 1);
        assert!(answers.failed_for(&a));
        // The next round asks again.
        answers.begin(tips.clone());
        assert_eq!(place(&answers, &tips).needs.len(), 1);
    }

    #[test]
    fn answers_near_a_tip_that_moved_are_asked_again() {
        let a = id("a");
        let mut chain = SimChain::new(1);
        chain.put(&a, 0, 5_000);
        let mut answers = Answers::default();
        answers.begin([(a.clone(), chain.tip(&a))]);
        for height in [3_000, 4_500] {
            let probe = Probe {
                node: a.clone(),
                height,
            };
            answers.record(probe.clone(), chain.answer(&probe));
        }
        chain.put(&a, 0, 5_001);
        answers.begin([(a.clone(), chain.tip(&a))]);
        assert!(
            matches!(answers.look(&a, 3_000), Look::Hash(_)),
            "deep answers stay"
        );
        assert!(matches!(answers.look(&a, 4_500), Look::Need(_)));
    }
}
