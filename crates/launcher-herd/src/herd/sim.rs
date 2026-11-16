//! A simulated chain with branches, for the engine's tests and the browser mock (feature
//! `sim`): each node sits at a height on a branch, and answers probes as zebrad's
//! `getblockhash` would.

use std::collections::{BTreeMap, BTreeSet};

use launcher_types::{NodeId, herd::BlockRef};

use super::search::{Probe, ProbeResult};

/// A main chain and branches off it, with deterministic hashes.
#[derive(Clone, Debug, Default)]
pub struct SimChain {
    seed: u64,
    /// The first height of each branch of its own; branch 0, the main chain, has none.
    branches: Vec<u32>,
    nodes: BTreeMap<NodeId, (usize, u32)>,
    failing: BTreeSet<NodeId>,
}

impl SimChain {
    /// A main chain whose hashes follow from `seed`.
    pub fn new(seed: u64) -> Self {
        SimChain {
            seed,
            branches: vec![0],
            ..SimChain::default()
        }
    }

    /// A new branch off the main chain, with blocks of its own from `fork_at`.
    pub fn branch(&mut self, fork_at: u32) -> usize {
        self.branches.push(fork_at);
        self.branches.len() - 1
    }

    /// Puts `node` at `height` on `branch`.
    pub fn put(&mut self, node: &NodeId, branch: usize, height: u32) {
        self.nodes.insert(node.clone(), (branch, height));
    }

    /// Puts `node` at `height` on a new branch with blocks of its own from `fork_at`.
    pub fn fork(&mut self, node: &NodeId, fork_at: u32, height: u32) -> usize {
        let branch = self.branch(fork_at);
        self.put(node, branch, height);
        branch
    }

    /// Makes `node`'s probes fail, or answer again.
    pub fn fail(&mut self, node: &NodeId, failing: bool) {
        if failing {
            self.failing.insert(node.clone());
        } else {
            self.failing.remove(node);
        }
    }

    /// Where `node` is: its branch and height.
    pub fn position(&self, node: &NodeId) -> Option<(usize, u32)> {
        self.nodes.get(node).copied()
    }

    /// The hash of the block at `height` on `branch`, below its fork the main chain's.
    pub fn hash_on(&self, branch: usize, height: u32) -> String {
        let own = branch != 0 && self.branches.get(branch).is_some_and(|at| height >= *at);
        let branch = if own { branch as u64 } else { 0 };
        let mut parts = [0u64; 4];
        for (index, part) in parts.iter_mut().enumerate() {
            *part = mix(self.seed ^ (branch << 40) ^ (u64::from(height) << 2) ^ index as u64);
        }
        parts.iter().map(|part| format!("{part:016x}")).collect()
    }

    /// `node`'s tip.
    pub fn tip(&self, node: &NodeId) -> BlockRef {
        let (branch, height) = self.nodes[node];
        BlockRef {
            height,
            hash: self.hash_on(branch, height),
        }
    }

    /// `node`'s answer to `probe`, as zebrad gives it.
    pub fn answer(&self, probe: &Probe) -> ProbeResult {
        if self.failing.contains(&probe.node) {
            return ProbeResult::Failed;
        }
        match self.nodes.get(&probe.node) {
            None => ProbeResult::Failed,
            Some((_, height)) if probe.height > *height => ProbeResult::AboveTip,
            Some((branch, _)) => ProbeResult::Hash(self.hash_on(*branch, probe.height)),
        }
    }
}

/// SplitMix64's finalizer.
fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branches_share_the_main_chain_below_their_fork() {
        let mut chain = SimChain::new(3);
        let side = chain.branch(100);
        assert_eq!(chain.hash_on(side, 99), chain.hash_on(0, 99));
        assert_ne!(chain.hash_on(side, 100), chain.hash_on(0, 100));
        assert_eq!(chain.hash_on(0, 7).len(), 64);
        let node = NodeId::new("a").unwrap();
        chain.put(&node, side, 120);
        let probe = |height| Probe {
            node: node.clone(),
            height,
        };
        assert_eq!(chain.answer(&probe(121)), ProbeResult::AboveTip);
        assert_eq!(
            chain.answer(&probe(50)),
            ProbeResult::Hash(chain.hash_on(0, 50))
        );
        chain.fail(&node, true);
        assert_eq!(chain.answer(&probe(50)), ProbeResult::Failed);
    }
}
