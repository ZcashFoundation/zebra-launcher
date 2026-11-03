//! What the explorer knows of the node's best chain: its tip and the hashes at the heights it
//! has read, so a block asked for by height can come from the cache. Hashes at final heights
//! (1,000 below the tip, zebrad's reorg limit) are trusted as they are; hashes above are
//! trusted only while the tip was checked a moment ago. A reorganisation drops every height
//! above the highest one its check proved shared, and bumps the epoch once.
//!
//! The hashes come two ways. The tip follower vouches for the ones it read from the node: the
//! tips it checked, the blocks they build on and the answers to its probes, and so for their
//! parents, which the blocks pages read show. Pages teach the others. Only vouched hashes lie on
//! one chain for sure, so only they go into a search for where two chains part; a page may
//! have read its block on either side of a reorganisation the follower has not seen yet. A
//! block a page read that disagrees with the index at its height, or at its parent's, is such a
//! reorganisation: the page's hashes from there up are dropped and the tip counts as unchecked
//! until the follower checks it. The vouched hashes stay for that check: either the old tip is
//! still in the node's chain, and so is every vouched hash, which all lie below it on one
//! chain; or the check's search over them bounds the reorganisation, and everything above the
//! height it proved shared goes.

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
    /// The tip went back, and the highest vouched height at or below it, `shared`, still holds
    /// (with none to compare, the window's floor), so nothing shows that blocks were replaced:
    /// zebrad restarted a block or two lower, say. The heights above `shared` are dropped.
    WentBack { shared: u32 },
    /// The node replaced blocks: the chains share `shared` and differ at `first_replaced`, so
    /// they part at a height between the two. The heights above `shared` are dropped.
    Replaced { shared: u32, first_replaced: u32 },
}

/// What [`ChainIndex::apply`] undid: the heights above `above` are no longer known and the
/// epoch moved on. `replaced` when the node replaced blocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Undone {
    pub(crate) above: u32,
    pub(crate) replaced: Option<Replaced>,
}

/// A reorganisation: the chains share `shared`, differ at `first_replaced`, and at least
/// `blocks` blocks of the old chain were replaced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Replaced {
    pub(crate) shared: u32,
    pub(crate) first_replaced: u32,
    pub(crate) blocks: u32,
}

/// One known height: its hash, whether the tip follower vouches for it, and its block's
/// parent, once a read of the block showed it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Known {
    hash: BlockHash,
    vouched: bool,
    parent: Option<BlockHash>,
}

#[derive(Debug)]
pub(crate) struct ChainIndex {
    tip: Option<(u32, BlockHash)>,
    known: BTreeMap<u32, Known>,
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

    /// Takes the tip as unchecked: nothing above the final heights is trusted until the next
    /// check.
    pub(crate) fn uncheck(&mut self) {
        self.checked = None;
    }

    /// The hash at `height`, when it can be trusted now.
    pub(crate) fn hash_at(&self, height: u32, now: Instant) -> Option<BlockHash> {
        let (tip, _) = self.tip?;
        if height > tip {
            return None;
        }
        let hash = self.known.get(&height)?.hash;
        let is_final = tip - height >= REORG_WINDOW;
        (is_final || self.is_fresh(now)).then_some(hash)
    }

    /// Records a block a page read in the node's best chain: `hash` at `height`, on top of
    /// `parent`. A block whose hash the follower vouches for vouches for its parent. A block
    /// that disagrees with what the index holds at its height or its parent's means the node
    /// replaced blocks the follower has not seen yet: the pages' hashes from the lower
    /// disagreement up are dropped, the tip counts as unchecked, and the answer is `true`.
    /// Blocks above the tip teach nothing.
    pub(crate) fn learn_block(
        &mut self,
        height: u32,
        hash: BlockHash,
        parent: Option<BlockHash>,
    ) -> bool {
        if self.tip.is_none_or(|(tip, _)| height > tip) {
            return false;
        }
        let below = parent.zip(height.checked_sub(1));
        if let Some((parent, below)) = below
            && self.disagrees(below, parent)
        {
            self.forget_from(below);
            return true;
        }
        if self.disagrees(height, hash) {
            self.forget_from(height);
            return true;
        }
        let vouched = match self.known.get_mut(&height) {
            Some(known) => {
                known.parent = known.parent.or(parent);
                known.vouched
            }
            None => {
                self.insert(
                    height,
                    Known {
                        hash,
                        vouched: false,
                        parent,
                    },
                );
                false
            }
        };
        if let Some((parent, below)) = below {
            if vouched {
                self.vouch(below, parent);
            } else if !self.known.contains_key(&below) {
                self.insert(
                    below,
                    Known {
                        hash: parent,
                        vouched: false,
                        parent: None,
                    },
                );
            }
        }
        false
    }

    /// The vouched hashes within the reorg window of the tip, lowest first: what a fork search
    /// compares against the node.
    pub(crate) fn window(&self) -> Vec<(u32, BlockHash)> {
        let Some((tip, _)) = self.tip else {
            return Vec::new();
        };
        self.known
            .range(tip.saturating_sub(REORG_WINDOW)..=tip)
            .filter(|(_, known)| known.vouched)
            .map(|(height, known)| (*height, known.hash))
            .collect()
    }

    /// Applies a checked tip. Returns what it undid, if anything: then the epoch moved on.
    pub(crate) fn apply(&mut self, update: TipUpdate, now: Instant) -> Option<Undone> {
        let old_tip = self.tip.map(|(height, _)| height);
        let undone = match update.change {
            Change::Follows => None,
            Change::Jumped => {
                if let Some(old_tip) = old_tip {
                    self.drop_above(old_tip.saturating_sub(REORG_WINDOW));
                }
                None
            }
            Change::WentBack { shared } => Some(Undone {
                above: shared,
                replaced: None,
            }),
            Change::Replaced {
                shared,
                first_replaced,
            } => Some(Undone {
                above: shared,
                replaced: Some(Replaced {
                    shared,
                    first_replaced,
                    blocks: old_tip.map_or(0, |tip| (tip + 1).saturating_sub(first_replaced)),
                }),
            }),
        };
        if let Some(undone) = undone {
            self.drop_above(undone.above);
            self.epoch += 1;
        }
        self.tip = Some(update.tip);
        for (height, hash) in update.learned {
            self.vouch(height, hash);
        }
        self.vouch(update.tip.0, update.tip.1);
        self.checked = Some(now);
        undone
    }

    /// Forgets everything: the node now runs another chain.
    pub(crate) fn clear(&mut self) {
        *self = Self::new(self.fresh);
    }

    /// Forgets the hashes above `height`.
    fn drop_above(&mut self, height: u32) {
        self.known.split_off(&height.saturating_add(1));
    }

    /// Forgets the hashes from `height` up that the follower does not vouch for, and that the
    /// tip was checked.
    fn forget_from(&mut self, height: u32) {
        let mut above = self.known.split_off(&height);
        above.retain(|_, known| known.vouched);
        self.known.append(&mut above);
        self.checked = None;
    }

    fn disagrees(&self, height: u32, hash: BlockHash) -> bool {
        self.known
            .get(&height)
            .is_some_and(|known| known.hash != hash)
    }

    fn insert(&mut self, height: u32, known: Known) {
        self.known.insert(height, known);
        while self.known.len() > MAX_KNOWN {
            self.known.pop_first();
        }
    }

    /// Records a hash the follower vouches for, at or below the tip, in place of whatever the
    /// index held there; and the parents it vouches for in turn, as far down as the index saw
    /// them.
    fn vouch(&mut self, height: u32, hash: BlockHash) {
        let mut next = Some((height, hash));
        while let Some((height, hash)) = next.take() {
            if self.tip.is_none_or(|(tip, _)| height > tip) {
                return;
            }
            let parent = match self.known.get_mut(&height) {
                Some(known) if known.hash == hash => {
                    if known.vouched {
                        return;
                    }
                    known.vouched = true;
                    known.parent
                }
                _ => {
                    self.insert(
                        height,
                        Known {
                            hash,
                            vouched: true,
                            parent: None,
                        },
                    );
                    None
                }
            };
            next = parent
                .zip(height.checked_sub(1))
                .map(|(parent, below)| (below, parent));
        }
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
        for height in [3_000, 4_990, 6_000] {
            assert!(!index.learn_block(height, hash(height, 0), None));
        }
        assert_eq!(index.hash_at(6_000, start), None, "above the tip");
        assert_eq!(index.hash_at(4_990, start), Some(hash(4_990, 0)));
        let later = start + FRESH + Duration::from_secs(1);
        assert_eq!(index.hash_at(4_990, later), None, "the check is stale");
        assert_eq!(index.hash_at(3_000, later), Some(hash(3_000, 0)), "final");
    }

    #[test]
    fn only_the_followers_hashes_go_into_a_fork_search() {
        let now = Instant::now();
        let mut index = ChainIndex::new(FRESH);
        index.apply(
            TipUpdate {
                learned: vec![(4_990, hash(4_990, 0))],
                ..update(5_000, 0)
            },
            now,
        );
        // A block a page read: known, but not vouched for.
        assert!(!index.learn_block(4_995, hash(4_995, 1), None));
        assert_eq!(index.hash_at(4_995, now), Some(hash(4_995, 1)));
        assert_eq!(
            index.window(),
            vec![(4_990, hash(4_990, 0)), (5_000, hash(5_000, 0))]
        );
    }

    #[test]
    fn a_block_on_a_vouched_one_vouches_for_its_parents() {
        let now = Instant::now();
        let mut index = ChainIndex::new(FRESH);
        index.apply(update(100, 0), now);
        // Pages read the blocks below the tip first, in any order, then the tip's.
        for height in [98, 97, 99] {
            index.learn_block(height, hash(height, 0), Some(hash(height - 1, 0)));
        }
        assert_eq!(index.window(), vec![(100, hash(100, 0))]);
        index.learn_block(100, hash(100, 0), Some(hash(99, 0)));
        let vouched: Vec<u32> = index.window().iter().map(|(height, _)| *height).collect();
        assert_eq!(vouched, [96, 97, 98, 99, 100]);
    }

    #[test]
    fn a_block_that_disagrees_leaves_only_the_vouched_heights_from_there_up() {
        let now = Instant::now();
        let mut index = ChainIndex::new(FRESH);
        index.apply(update(100, 0), now);
        // The tip's block vouches for 99; pages taught 89 to 91.
        index.learn_block(100, hash(100, 0), Some(hash(99, 0)));
        index.learn_block(90, hash(90, 0), Some(hash(89, 0)));
        index.learn_block(91, hash(91, 0), Some(hash(90, 0)));
        // A block of another branch: its parent at 91 is another block.
        assert!(index.learn_block(92, hash(92, 1), Some(hash(91, 1))));
        assert!(!index.is_fresh(now), "the tip counts as unchecked");
        assert_eq!(index.hash_at(90, now), None);
        // The check finds the same tip.
        index.apply(update(100, 0), now);
        assert_eq!(index.hash_at(90, now), Some(hash(90, 0)), "below it");
        assert_eq!(index.hash_at(91, now), None, "a page's, dropped");
        assert_eq!(index.hash_at(92, now), None, "not learned");
        assert_eq!(
            index.hash_at(99, now),
            Some(hash(99, 0)),
            "vouched for, kept"
        );
        // A block that disagrees with a vouched hash at its own height.
        assert!(index.learn_block(99, hash(99, 2), None));
        index.apply(update(100, 0), now);
        assert_eq!(index.hash_at(99, now), Some(hash(99, 0)));
    }

    #[test]
    fn a_reorganisation_drops_what_it_replaced_and_bumps_the_epoch_once() {
        let now = Instant::now();
        let mut index = ChainIndex::new(FRESH);
        index.apply(update(100, 0), now);
        for height in 90..100 {
            index.learn_block(height, hash(height, 0), None);
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
            replaced.and_then(|undone| undone.replaced),
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
    fn a_tip_that_went_back_drops_what_was_above_it_but_replaced_nothing() {
        let now = Instant::now();
        let mut index = ChainIndex::new(FRESH);
        index.apply(update(100, 0), now);
        index.learn_block(100, hash(100, 0), Some(hash(99, 0)));
        let undone = index.apply(
            TipUpdate {
                change: Change::WentBack { shared: 99 },
                ..update(99, 0)
            },
            now,
        );
        assert_eq!(
            undone,
            Some(Undone {
                above: 99,
                replaced: None
            })
        );
        assert_eq!(index.epoch(), 1, "pages read again");
        assert_eq!(index.tip(), Some((99, hash(99, 0))));
        assert_eq!(index.hash_at(99, now), Some(hash(99, 0)));
        assert_eq!(index.hash_at(100, now), None);
        // A new run of zebrad: nothing near the tip is trusted until the next check.
        index.uncheck();
        assert_eq!(index.hash_at(99, now), None);
    }

    #[test]
    fn a_long_jump_drops_the_volatile_heights() {
        let now = Instant::now();
        let mut index = ChainIndex::new(FRESH);
        index.apply(update(10_000, 0), now);
        index.learn_block(8_000, hash(8_000, 0), None);
        index.learn_block(9_999, hash(9_999, 0), None);
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
        index.learn_block(3_000, hash(3_000, 0), None);
        index.learn_block(4_990, hash(4_990, 0), None);
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
            Some(Undone {
                above: 4_000,
                replaced: Some(Replaced {
                    shared: 4_000,
                    first_replaced: 4_990,
                    blocks: 11
                })
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
