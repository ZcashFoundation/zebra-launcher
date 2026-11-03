//! What the explorer knows of the node's best chain: its tip and the hashes at the heights it
//! has read, so a block asked for by height can come from the cache. Hashes at final heights
//! (1,000 below the tip, zebrad's reorg limit) are trusted as they are; hashes above are
//! trusted only while the tip was checked a moment ago. A reorganisation drops every height
//! above the highest one its check proved shared, and bumps the epoch once.

use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use launcher_types::explorer::BlockHash;

/// Blocks this deep are final in Zebra (a policy, not a consensus rule).
pub(crate) const REORG_WINDOW: u32 = 1_000;
/// The most heights kept; the oldest go first.
const MAX_KNOWN: usize = 50_000;

/// The outcome of checking the node's tip against what the index knew.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TipUpdate {
    pub(crate) tip: (u32, BlockHash),
    pub(crate) change: Change,
    /// The node's hashes at the heights the check read on the way.
    pub(crate) learned: Vec<(u32, BlockHash)>,
}

/// What a check of the tip found about the chain the index knew.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Change {
    /// The same tip, or one that builds on it.
    Follows,
    /// The tip moved further ahead than the reorg window: nothing compared the old tip's
    /// heights within the window below it with the node, so they are dropped.
    Jumped,
    /// The node replaced blocks: the chains share `shared` and differ at `first_replaced`, so
    /// they part at a height between the two. The heights above `shared` are dropped.
    Replaced { shared: u32, first_replaced: u32 },
}

/// A reorganisation [`ChainIndex::apply`] found: the chains share `shared`, differ at
/// `first_replaced`, and at least `blocks` blocks of the old chain were replaced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Replaced {
    pub(crate) shared: u32,
    pub(crate) first_replaced: u32,
    pub(crate) blocks: u32,
}

#[derive(Debug)]
pub(crate) struct ChainIndex {
    tip: Option<(u32, BlockHash)>,
    known: BTreeMap<u32, BlockHash>,
    epoch: u64,
    checked: Option<Instant>,
    /// How long a check of the tip vouches for the hashes above the final ones.
    fresh: Duration,
}

impl ChainIndex {
    /// An empty index whose tip checks vouch for `fresh`.
    pub(crate) fn new(fresh: Duration) -> Self {
        Self {
            tip: None,
            known: BTreeMap::new(),
            epoch: 0,
            checked: None,
            fresh,
        }
    }

    pub(crate) fn tip(&self) -> Option<(u32, BlockHash)> {
        self.tip
    }

    pub(crate) fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Whether the tip was checked within `max_age`.
    pub(crate) fn checked_within(&self, max_age: Duration, now: Instant) -> bool {
        self.checked
            .is_some_and(|checked| now.saturating_duration_since(checked) <= max_age)
    }

    /// Whether the last check of the tip still vouches for the heights above the final ones.
    pub(crate) fn is_fresh(&self, now: Instant) -> bool {
        self.checked_within(self.fresh, now)
    }

    /// The hash at `height`, when it can be trusted now.
    pub(crate) fn hash_at(&self, height: u32, now: Instant) -> Option<BlockHash> {
        let (tip, _) = self.tip?;
        if height > tip {
            return None;
        }
        let hash = *self.known.get(&height)?;
        let is_final = tip - height >= REORG_WINDOW;
        (is_final || self.is_fresh(now)).then_some(hash)
    }

    /// Records the hash a read found at `height`, at or below the tip.
    pub(crate) fn learn(&mut self, height: u32, hash: BlockHash) {
        if self.tip.is_some_and(|(tip, _)| height <= tip) {
            self.known.insert(height, hash);
            while self.known.len() > MAX_KNOWN {
                self.known.pop_first();
            }
        }
    }

    /// The known hashes within the reorg window of the tip, lowest first: what a fork search
    /// compares against the node.
    pub(crate) fn window(&self) -> Vec<(u32, BlockHash)> {
        let Some((tip, _)) = self.tip else {
            return Vec::new();
        };
        self.known
            .range(tip.saturating_sub(REORG_WINDOW)..=tip)
            .map(|(height, hash)| (*height, *hash))
            .collect()
    }

    /// Applies a checked tip. Returns the reorganisation it reveals, if any.
    pub(crate) fn apply(&mut self, update: TipUpdate, now: Instant) -> Option<Replaced> {
        let old_tip = self.tip.map(|(height, _)| height);
        let mut replaced = None;
        match update.change {
            Change::Follows => {}
            Change::Jumped => {
                if let Some(old_tip) = old_tip {
                    self.drop_above(old_tip.saturating_sub(REORG_WINDOW));
                }
            }
            Change::Replaced {
                shared,
                first_replaced,
            } => {
                self.drop_above(shared);
                self.epoch += 1;
                replaced = Some(Replaced {
                    shared,
                    first_replaced,
                    blocks: old_tip.map_or(0, |tip| (tip + 1).saturating_sub(first_replaced)),
                });
            }
        }
        self.tip = Some(update.tip);
        for (height, hash) in update.learned {
            self.learn(height, hash);
        }
        self.learn(update.tip.0, update.tip.1);
        self.checked = Some(now);
        replaced
    }

    /// Forgets everything: the node now runs another chain.
    pub(crate) fn clear(&mut self) {
        *self = Self::new(self.fresh);
    }

    /// Forgets the hashes above `height`.
    fn drop_above(&mut self, height: u32) {
        self.known.split_off(&height.saturating_add(1));
    }
}

/// A binary search for where two chains part, over the heights the index knows: the chains
/// agree up to the fork and nowhere above it, so each probe ("does the node still have this
/// hash here?") halves what is left. A thousand heights take at most ten probes. It proves only
/// what it probed: the chains part at or above the highest height that matched and below the
/// lowest that did not, which are next to each other only when the index knew every height
/// between.
#[derive(Debug)]
pub(crate) struct ForkSearch {
    candidates: Vec<(u32, BlockHash)>,
    /// Candidates below `low` match; candidates at or above `high` do not.
    low: usize,
    high: usize,
}

impl ForkSearch {
    /// A search over `candidates`, lowest height first.
    pub(crate) fn new(candidates: Vec<(u32, BlockHash)>) -> Self {
        let high = candidates.len();
        Self {
            candidates,
            low: 0,
            high,
        }
    }

    /// The next height to ask the node about, with the hash the index has there; `None` once
    /// the search is over.
    pub(crate) fn probe(&self) -> Option<(u32, BlockHash)> {
        (self.low < self.high).then(|| self.candidates[(self.low + self.high) / 2])
    }

    /// Whether the node has the probed hash at the probed height.
    pub(crate) fn answer(&mut self, same: bool) {
        let middle = (self.low + self.high) / 2;
        if same {
            self.low = middle + 1;
        } else {
            self.high = middle;
        }
    }

    /// What the search proved, once it is over.
    pub(crate) fn bounds(&self) -> ForkBounds {
        ForkBounds {
            shared: self
                .low
                .checked_sub(1)
                .map(|index| self.candidates[index].0),
            first_replaced: self.candidates.get(self.high).map(|(height, _)| *height),
        }
    }
}

/// What a finished [`ForkSearch`] proved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ForkBounds {
    /// The highest candidate the node still has.
    pub(crate) shared: Option<u32>,
    /// The lowest candidate the node no longer has.
    pub(crate) first_replaced: Option<u32>,
}

#[cfg(test)]
mod tests {
    use launcher_types::explorer::Hash32;

    use super::*;

    const FRESH: Duration = Duration::from_secs(10);

    fn hash(height: u32, branch: u8) -> BlockHash {
        let mut bytes = [branch; 32];
        bytes[..4].copy_from_slice(&height.to_be_bytes());
        Hash32(bytes)
    }

    fn update(tip: u32, branch: u8) -> TipUpdate {
        TipUpdate {
            tip: (tip, hash(tip, branch)),
            change: Change::Follows,
            learned: Vec::new(),
        }
    }

    #[test]
    fn heights_near_the_tip_need_a_fresh_check() {
        let start = Instant::now();
        let mut index = ChainIndex::new(FRESH);
        assert_eq!(index.hash_at(10, start), None);
        index.apply(update(5_000, 0), start);
        index.learn(3_000, hash(3_000, 0));
        index.learn(4_990, hash(4_990, 0));
        index.learn(6_000, hash(6_000, 0));
        assert_eq!(index.hash_at(6_000, start), None, "above the tip");
        assert_eq!(index.hash_at(4_990, start), Some(hash(4_990, 0)));
        let later = start + FRESH + Duration::from_secs(1);
        assert_eq!(index.hash_at(4_990, later), None, "the check is stale");
        assert_eq!(index.hash_at(3_000, later), Some(hash(3_000, 0)), "final");
        assert_eq!(
            index.window(),
            vec![(4_990, hash(4_990, 0)), (5_000, hash(5_000, 0))]
        );
    }

    #[test]
    fn a_reorganisation_drops_what_it_replaced_and_bumps_the_epoch_once() {
        let now = Instant::now();
        let mut index = ChainIndex::new(FRESH);
        index.apply(update(100, 0), now);
        for height in 90..100 {
            index.learn(height, hash(height, 0));
        }
        let replaced = index.apply(
            TipUpdate {
                change: Change::Replaced {
                    shared: 98,
                    first_replaced: 99,
                },
                ..update(101, 1)
            },
            now,
        );
        assert_eq!(
            replaced,
            Some(Replaced {
                shared: 98,
                first_replaced: 99,
                blocks: 2
            })
        );
        assert_eq!(index.epoch(), 1);
        assert_eq!(index.hash_at(98, now), Some(hash(98, 0)));
        assert_eq!(index.hash_at(99, now), None);
        assert_eq!(index.hash_at(101, now), Some(hash(101, 1)));
        // Following on is no reorganisation.
        assert_eq!(index.apply(update(102, 1), now), None);
        assert_eq!(index.epoch(), 1);
    }

    #[test]
    fn a_long_jump_drops_the_volatile_heights() {
        let now = Instant::now();
        let mut index = ChainIndex::new(FRESH);
        index.apply(update(10_000, 0), now);
        index.learn(8_000, hash(8_000, 0));
        index.learn(9_999, hash(9_999, 0));
        index.apply(
            TipUpdate {
                change: Change::Jumped,
                ..update(20_000, 0)
            },
            now,
        );
        assert_eq!(index.hash_at(8_000, now), Some(hash(8_000, 0)));
        assert_eq!(index.hash_at(9_999, now), None, "never checked");
        assert_eq!(index.epoch(), 0);
        index.clear();
        assert_eq!(index.tip(), None);
    }

    #[test]
    fn the_fork_search_halves_what_is_left() {
        let candidates: Vec<(u32, BlockHash)> = (0..1_000).map(|h| (h, hash(h, 0))).collect();
        for fork in [0u32, 1, 499, 998, 999] {
            let mut search = ForkSearch::new(candidates.clone());
            let mut probes = 0;
            while let Some((height, _)) = search.probe() {
                probes += 1;
                search.answer(height <= fork);
            }
            assert!(probes <= 10, "{probes} probes");
            let bounds = search.bounds();
            assert_eq!(bounds.shared, Some(fork));
            assert_eq!(bounds.first_replaced, (fork < 999).then_some(fork + 1));
        }
    }

    #[test]
    fn the_fork_search_proves_only_what_it_probed() {
        let finish = |mut search: ForkSearch, fork: u32| {
            while let Some((height, _)) = search.probe() {
                search.answer(height <= fork);
            }
            search.bounds()
        };
        // Nothing matches: the chains part somewhere below the lowest candidate.
        let candidates = vec![(50, hash(50, 0)), (60, hash(60, 0))];
        assert_eq!(
            finish(ForkSearch::new(candidates.clone()), 10),
            ForkBounds {
                shared: None,
                first_replaced: Some(50)
            }
        );
        // Far apart: the chains part between the two.
        assert_eq!(
            finish(ForkSearch::new(candidates), 55),
            ForkBounds {
                shared: Some(50),
                first_replaced: Some(60)
            }
        );
        assert_eq!(
            finish(ForkSearch::new(Vec::new()), 55),
            ForkBounds {
                shared: None,
                first_replaced: None
            }
        );
    }

    #[test]
    fn a_reorganisation_counts_only_the_blocks_seen_replaced() {
        let now = Instant::now();
        let mut index = ChainIndex::new(FRESH);
        index.apply(update(5_000, 0), now);
        index.learn(3_000, hash(3_000, 0));
        index.learn(4_990, hash(4_990, 0));
        // The chains share 4,000 at least and differ at 4,990: at least 11 blocks were replaced.
        let replaced = index.apply(
            TipUpdate {
                change: Change::Replaced {
                    shared: 4_000,
                    first_replaced: 4_990,
                },
                ..update(5_001, 1)
            },
            now,
        );
        assert_eq!(
            replaced,
            Some(Replaced {
                shared: 4_000,
                first_replaced: 4_990,
                blocks: 11
            })
        );
        assert_eq!(index.hash_at(3_000, now), Some(hash(3_000, 0)));
        assert_eq!(
            index.hash_at(4_990, now),
            None,
            "above what was proved shared"
        );
    }
}
