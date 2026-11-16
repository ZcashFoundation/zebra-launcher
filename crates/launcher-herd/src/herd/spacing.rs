//! Block spacing and the time rules that follow from it (docs/HERD.md, Block spacing and
//! thresholds). S is the target spacing at the herd's top: 75 s before NU7, 25 s from it
//! (explorer-model's activation heights, zebrad 7.0.0's). Every time rule uses S_eff, the
//! measured spacing held between S and 3 S, so a slow stretch after NU7 or a drop in hash
//! power does not read as a stall.

use std::{collections::VecDeque, time::Duration};

use explorer_model::upgrades::UpgradeHeights;
use launcher_types::Network;

/// How many top blocks S_eff is measured over.
const WINDOW_BLOCKS: u32 = 100;
/// How many top blocks Zeal must have seen arrive before it trusts its measurement.
const MIN_BLOCKS: u32 = 30;

/// The target spacing of the block at `height` on `network`.
pub fn target_spacing(network: Network, height: u32) -> Duration {
    Duration::from_secs(u64::from(
        UpgradeHeights::defaults(network).target_spacing(height),
    ))
}

/// The gaps between the top blocks Zeal saw arrive, with its own clock: S_eff.
#[derive(Clone, Debug, Default)]
pub struct SpacingMeter {
    /// `(time, blocks)`: each time the top moved, how long since the last move and by how
    /// many blocks. The newest last; at most [`WINDOW_BLOCKS`] blocks in all.
    gaps: VecDeque<(Duration, u32)>,
}

impl SpacingMeter {
    /// Records that the top gained `blocks` blocks `elapsed` after it last moved. Only
    /// finished gaps are recorded, so a stall in progress never raises its own threshold.
    pub fn record(&mut self, elapsed: Duration, blocks: u32) {
        if blocks == 0 {
            return;
        }
        self.gaps.push_back((elapsed, blocks));
        while self.blocks() - self.gaps.front().map_or(0, |gap| gap.1) >= WINDOW_BLOCKS {
            self.gaps.pop_front();
        }
    }

    fn blocks(&self) -> u32 {
        self.gaps.iter().map(|gap| gap.1).sum()
    }

    /// S_eff for a target spacing `target`: the measured mean gap once [`MIN_BLOCKS`] blocks
    /// arrived, held between `target` and three times it; `target` until then.
    pub fn effective(&self, target: Duration) -> Duration {
        let blocks = self.blocks();
        if blocks < MIN_BLOCKS {
            return target;
        }
        let total: Duration = self.gaps.iter().map(|gap| gap.0).sum();
        (total / blocks).clamp(target, target * 3)
    }
}

/// The time rules at one spacing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Thresholds {
    /// S_eff.
    pub spacing: Duration,
    /// Blocks a node may be behind and still agree: zebrad's `ready_max_blocks_behind`.
    pub tolerance: u32,
    /// A competing branch this long (the longer side, past the fork point) is a race at any
    /// age; one block more is a race for less than [`Thresholds::race_for`].
    pub race_blocks: u32,
    pub race_for: Duration,
    /// More than `tolerance` behind this long: lagging, attention.
    pub lag: Duration,
    /// More than [`Thresholds::lag_problem_blocks`] behind this long: lagging, a problem.
    pub lag_problem: Duration,
    pub lag_problem_blocks: u32,
    /// A tip unchanged this long while the main branch gained 3 blocks: stalled.
    pub stall: Duration,
    /// Stalled this long: a problem, except on Testnet.
    pub stall_problem: Option<Duration>,
    /// No new top block this long: the herd is stalled (attention), then a problem.
    pub herd_stall: Duration,
    pub herd_stall_problem: Option<Duration>,
    /// No herd stall before this long after the first round or a wake.
    pub grace: Duration,
}

impl Thresholds {
    /// The rules for `network` at S_eff `spacing`. Testnet's minimum-difficulty blocks make
    /// sibling forks common: races up to 6 blocks, and stalls at attention only.
    pub fn new(network: Network, spacing: Duration) -> Self {
        let testnet = network == Network::Testnet;
        Thresholds {
            spacing,
            tolerance: 2,
            race_blocks: if testnet { 6 } else { 1 },
            race_for: spacing * 3,
            lag: spacing * 4,
            lag_problem: spacing * 12,
            lag_problem_blocks: 12,
            stall: spacing * 5,
            stall_problem: (!testnet).then_some(spacing * 24),
            herd_stall: spacing * 12,
            herd_stall_problem: (!testnet).then_some(spacing * 24),
            grace: spacing * 12,
        }
    }
}

/// How long a node in probation must stay within tolerance of the top before its state
/// counts: 2 min, or 10 s for one already there when Zeal first saw it.
pub const PROBATION: Duration = Duration::from_secs(120);
pub const SHORT_PROBATION: Duration = Duration::from_secs(10);

/// A new state must hold this long, across two rounds, before a node takes it.
pub const DEBOUNCE: Duration = Duration::from_secs(10);

/// How far below the other node a fork search looks: zebrad's rollback window
/// (`MAX_BLOCK_REORG_HEIGHT`). Branches that differ even there are a deep fork.
pub const MAX_REORG: u32 = 1_000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_target_follows_nu7() {
        assert_eq!(
            target_spacing(Network::Mainnet, 3_543_404),
            Duration::from_secs(75)
        );
        assert_eq!(
            target_spacing(Network::Mainnet, 3_543_405),
            Duration::from_secs(25)
        );
        assert_eq!(
            target_spacing(Network::Testnet, 4_465_025),
            Duration::from_secs(75)
        );
        assert_eq!(
            target_spacing(Network::Testnet, 4_465_026),
            Duration::from_secs(25)
        );
    }

    #[test]
    fn the_effective_spacing_is_measured_and_held_between_s_and_3s() {
        let target = Duration::from_secs(25);
        let mut meter = SpacingMeter::default();
        // Too few blocks: the target.
        for _ in 0..29 {
            meter.record(Duration::from_secs(60), 1);
        }
        assert_eq!(meter.effective(target), target);
        meter.record(Duration::from_secs(60), 1);
        assert_eq!(meter.effective(target), Duration::from_secs(60));
        // Very slow blocks are capped at 3 S, fast ones at S.
        for _ in 0..100 {
            meter.record(Duration::from_secs(600), 1);
        }
        assert_eq!(meter.effective(target), target * 3);
        for _ in 0..50 {
            meter.record(Duration::from_secs(5), 2);
        }
        assert_eq!(
            meter.effective(target),
            target,
            "the newest 100 blocks only"
        );
        // A move without new blocks is no gap.
        meter.record(Duration::from_secs(9_000), 0);
        assert_eq!(meter.effective(target), target);
    }

    #[test]
    fn thresholds_scale_with_the_spacing_and_testnet_is_lenient() {
        let main = Thresholds::new(Network::Mainnet, Duration::from_secs(25));
        assert_eq!(main.lag, Duration::from_secs(100));
        assert_eq!(main.stall, Duration::from_secs(125));
        assert_eq!(main.stall_problem, Some(Duration::from_secs(600)));
        assert_eq!(main.herd_stall, Duration::from_secs(300));
        assert_eq!(main.race_for, Duration::from_secs(75));
        assert_eq!(main.race_blocks, 1);
        let slow = Thresholds::new(Network::Mainnet, Duration::from_secs(75));
        assert_eq!(slow.herd_stall, Duration::from_secs(900));
        let test = Thresholds::new(Network::Testnet, Duration::from_secs(25));
        assert_eq!(test.race_blocks, 6);
        assert_eq!(test.stall_problem, None);
        assert_eq!(test.herd_stall_problem, None);
    }
}
