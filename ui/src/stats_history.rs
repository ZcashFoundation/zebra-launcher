//! A short history of the node statistics the RPC monitor publishes, for the dashboard's
//! charts and rates. Pure Rust, tested natively.

use std::time::{Duration, SystemTime};

use launcher_types::NodeStats;

/// How many samples are kept: four hours of the monitor's 5-second polls.
pub const MAX_SAMPLES: usize = 4 * 60 * 60 / 5;

/// A block rate needs at least this much time between the first and last sample of a window.
pub const MIN_RATE_SPAN: Duration = Duration::from_secs(30);

/// One published statistics value, reduced to the fields the dashboard plots.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    /// When zebrad answered: `NodeStats::updated_at`.
    pub at: SystemTime,
    pub height: u64,
    pub estimated_height: u64,
    pub peers: u32,
    pub mempool_transactions: u64,
    pub verification_progress: f64,
}

impl Sample {
    pub fn from_stats(stats: &NodeStats) -> Self {
        Sample {
            at: stats.updated_at,
            height: stats.height,
            estimated_height: stats.estimated_height,
            peers: stats.peers,
            mempool_transactions: stats.mempool_transactions,
            verification_progress: stats.verification_progress,
        }
    }

    /// Blocks between the verified height and zebrad's estimate of the tip.
    pub fn remaining(&self) -> u64 {
        self.estimated_height.saturating_sub(self.height)
    }

    /// Synced: within a block of the estimated tip, or a progress of 99.99 % or more.
    pub fn at_tip(&self) -> bool {
        self.remaining() <= 1 || self.verification_progress >= 0.9999
    }

    /// A counted field's value, for deltas and charts.
    pub fn get(&self, field: Field) -> u64 {
        match field {
            Field::Height => self.height,
            Field::EstimatedHeight => self.estimated_height,
            Field::Peers => u64::from(self.peers),
            Field::Mempool => self.mempool_transactions,
        }
    }
}

/// The counted fields of a [`Sample`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Height,
    EstimatedHeight,
    Peers,
    Mempool,
}

/// The samples kept since the launcher opened, oldest first.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatsHistory {
    samples: Vec<Sample>,
}

impl StatsHistory {
    /// Records a published statistics value. A value with the newest sample's time is a repeat
    /// and is ignored; an older time is filed in order; past [`MAX_SAMPLES`] the oldest go.
    pub fn push(&mut self, stats: &NodeStats) {
        let sample = Sample::from_stats(stats);
        if self
            .samples
            .last()
            .is_some_and(|newest| newest.at == sample.at)
        {
            return;
        }
        let index = self.samples.partition_point(|s| s.at < sample.at);
        match self.samples.get_mut(index) {
            Some(existing) if existing.at == sample.at => *existing = sample,
            _ => self.samples.insert(index, sample),
        }
        if self.samples.len() > MAX_SAMPLES {
            let excess = self.samples.len() - MAX_SAMPLES;
            self.samples.drain(..excess);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// The newest sample.
    pub fn latest(&self) -> Option<&Sample> {
        self.samples.last()
    }

    /// The samples from `now - span` on, oldest first; all of them for `None`.
    pub fn window(&self, span: Option<Duration>, now: SystemTime) -> &[Sample] {
        let Some(span) = span else {
            return &self.samples;
        };
        let Some(cutoff) = now.checked_sub(span) else {
            return &self.samples;
        };
        let start = self.samples.partition_point(|s| s.at < cutoff);
        &self.samples[start..]
    }

    /// Blocks verified per minute between the first and last sample of the window. `None`
    /// until the window spans [`MIN_RATE_SPAN`].
    pub fn block_rate(&self, span: Option<Duration>, now: SystemTime) -> Option<f64> {
        let window = self.window(span, now);
        let (first, last) = (window.first()?, window.last()?);
        let elapsed = last.at.duration_since(first.at).ok()?;
        if elapsed < MIN_RATE_SPAN {
            return None;
        }
        let blocks = last.height as f64 - first.height as f64;
        Some(blocks / (elapsed.as_secs_f64() / 60.0))
    }

    /// How long the newest sample's remaining blocks take at the window's rate. `Some(ZERO)`
    /// at the tip; `None` while the rate is unknown or not positive.
    pub fn eta_to_tip(&self, span: Option<Duration>, now: SystemTime) -> Option<Duration> {
        let newest = self.latest()?;
        if newest.at_tip() {
            return Some(Duration::ZERO);
        }
        let rate = self.block_rate(span, now)?;
        if rate <= 0.0 {
            return None;
        }
        let minutes = newest.remaining() as f64 / rate;
        Duration::try_from_secs_f64(minutes * 60.0).ok()
    }

    /// Newest minus oldest value of `field` in the window; `None` with fewer than two samples.
    pub fn delta(&self, field: Field, span: Option<Duration>, now: SystemTime) -> Option<i64> {
        let window = self.window(span, now);
        let (first, last) = (window.first()?, window.last()?);
        if window.len() < 2 {
            return None;
        }
        let first = i64::try_from(first.get(field)).unwrap_or(i64::MAX);
        let last = i64::try_from(last.get(field)).unwrap_or(i64::MAX);
        Some(last.saturating_sub(first))
    }
}

#[cfg(test)]
mod tests {
    use std::time::UNIX_EPOCH;

    use super::*;

    const T0: SystemTime = UNIX_EPOCH;

    fn at(secs: u64) -> SystemTime {
        T0 + Duration::from_secs(secs)
    }

    fn stats(secs: u64, height: u64) -> NodeStats {
        NodeStats {
            height,
            estimated_height: 3_120_000,
            verification_progress: height as f64 / 3_120_000.0,
            peers: 8 + (secs % 3) as u32,
            mempool_transactions: secs,
            chain: "main".into(),
            zebrad_version: "7.0.0".into(),
            last_error: None,
            eos: None,
            updated_at: at(secs),
        }
    }

    fn history(points: &[(u64, u64)]) -> StatsHistory {
        let mut history = StatsHistory::default();
        for &(secs, height) in points {
            history.push(&stats(secs, height));
        }
        history
    }

    #[test]
    fn push_dedups_repeats_and_keeps_order() {
        let mut history = history(&[(0, 100), (5, 110), (10, 120)]);
        // The same event delivered twice.
        history.push(&stats(10, 120));
        history.push(&stats(10, 999));
        assert_eq!(history.window(None, at(10)).len(), 3);
        assert_eq!(history.latest().unwrap().height, 120);
        // A late sample is filed in order; one for a known older time replaces it.
        history.push(&stats(7, 115));
        history.push(&stats(5, 111));
        let heights: Vec<_> = history
            .window(None, at(10))
            .iter()
            .map(|s| (s.at, s.height))
            .collect();
        assert_eq!(
            heights,
            vec![(at(0), 100), (at(5), 111), (at(7), 115), (at(10), 120)]
        );
        assert!(!history.is_empty());
        assert!(StatsHistory::default().is_empty());
    }

    #[test]
    fn cap_drops_the_oldest() {
        let mut history = StatsHistory::default();
        for i in 0..(MAX_SAMPLES as u64 + 10) {
            history.push(&stats(i * 5, i));
        }
        let all = history.window(None, at(0));
        assert_eq!(all.len(), MAX_SAMPLES);
        assert_eq!(all[0].height, 10);
        assert_eq!(all[MAX_SAMPLES - 1].height, MAX_SAMPLES as u64 + 9);
    }

    #[test]
    fn window_is_by_time() {
        let history = history(&[(0, 1), (60, 2), (120, 3), (180, 4)]);
        let now = at(200);
        assert_eq!(history.window(None, now).len(), 4);
        let recent = history.window(Some(Duration::from_secs(90)), now);
        assert_eq!(recent.len(), 2, "cutoff 110 keeps 120 and 180");
        assert_eq!(recent[0].height, 3);
        // A cutoff on a sample keeps that sample.
        assert_eq!(history.window(Some(Duration::from_secs(80)), now).len(), 2);
        assert!(history.window(Some(Duration::from_secs(5)), now).is_empty());
        // A span before the epoch means everything.
        assert_eq!(
            history
                .window(Some(Duration::from_secs(u64::MAX)), now)
                .len(),
            4
        );
    }

    #[test]
    fn rate_needs_thirty_seconds() {
        let now = at(100);
        assert_eq!(history(&[]).block_rate(None, now), None);
        assert_eq!(history(&[(0, 100)]).block_rate(None, now), None);
        assert_eq!(
            history(&[(0, 100), (25, 200)]).block_rate(None, now),
            None,
            "25 s is too short"
        );
        let rate = history(&[(0, 100), (30, 130), (60, 190)])
            .block_rate(None, now)
            .unwrap();
        assert!((rate - 90.0).abs() < 1e-9, "90 blocks in a minute: {rate}");
        // Only the window counts.
        let rate = history(&[(0, 0), (30, 1000), (60, 1030), (90, 1090)])
            .block_rate(Some(Duration::from_secs(60)), at(90))
            .unwrap();
        assert!((rate - 90.0).abs() < 1e-9, "{rate}");
    }

    #[test]
    fn eta_from_rate_and_remaining() {
        let now = at(60);
        // 90 blocks in a minute, 3_120_000 - 190 remaining.
        let history = history(&[(0, 100), (30, 130), (60, 190)]);
        let eta = history.eta_to_tip(None, now).unwrap();
        let expected = (3_120_000.0 - 190.0) / 90.0 * 60.0;
        assert!((eta.as_secs_f64() - expected).abs() < 1.0, "{eta:?}");
        // Unknown rate, falling height.
        assert_eq!(history.eta_to_tip(Some(Duration::from_secs(10)), now), None);
        assert_eq!(StatsHistory::default().eta_to_tip(None, now), None);
        let falling = self::history(&[(0, 500), (60, 400)]);
        assert_eq!(falling.eta_to_tip(None, now), None);
        // At the tip the answer is zero whatever the rate.
        let mut synced = StatsHistory::default();
        synced.push(&stats(0, 3_119_999));
        assert_eq!(synced.eta_to_tip(None, now), Some(Duration::ZERO));
    }

    #[test]
    fn at_tip_rule() {
        let sample = Sample::from_stats(&stats(0, 3_120_000));
        assert!(sample.at_tip());
        assert!(Sample::from_stats(&stats(0, 3_119_999)).at_tip());
        // Two blocks behind with a progress under 99.99 %: not there yet.
        let mut behind = stats(0, 3_119_998);
        behind.verification_progress = 0.99;
        assert!(!Sample::from_stats(&behind).at_tip());
        let mut nearly = stats(0, 3_119_000);
        nearly.verification_progress = 0.99995;
        assert!(Sample::from_stats(&nearly).at_tip());
        assert_eq!(Sample::from_stats(&stats(0, 3_119_000)).remaining(), 1000);
        assert_eq!(Sample::from_stats(&stats(0, 3_130_000)).remaining(), 0);
    }

    #[test]
    fn delta_is_newest_minus_oldest() {
        let history = history(&[(0, 100), (60, 130), (120, 190)]);
        let now = at(120);
        assert_eq!(history.delta(Field::Height, None, now), Some(90));
        assert_eq!(
            history.delta(Field::Height, Some(Duration::from_secs(60)), now),
            Some(60)
        );
        assert_eq!(
            history.delta(Field::Height, Some(Duration::from_secs(10)), now),
            None,
            "one sample is no delta"
        );
        assert_eq!(history.delta(Field::Mempool, None, now), Some(120));
        // Peers: 8 + secs % 3, so 8 then 8 then 8 here; make one differ.
        let mixed = self::history(&[(0, 1), (1, 1), (2, 1)]);
        assert_eq!(mixed.delta(Field::Peers, None, now), Some(2));
        assert_eq!(mixed.delta(Field::EstimatedHeight, None, now), Some(0));
        assert_eq!(
            StatsHistory::default().delta(Field::Height, None, now),
            None
        );
    }
}
