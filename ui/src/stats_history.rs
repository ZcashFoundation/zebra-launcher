//! A short history of the node statistics the RPC monitor publishes, for the dashboard's
//! charts and rates. Pure Rust, tested natively.

use std::time::{Duration, SystemTime};

use launcher_types::{NodeDetails, NodeStats, PeerInfo};

/// How many samples are kept: four hours of the monitor's 5-second polls.
pub const MAX_SAMPLES: usize = 4 * 60 * 60 / 5;

/// A block rate needs at least this much time between the first and last sample of a window.
pub const MIN_RATE_SPAN: Duration = Duration::from_secs(30);

/// The window of the sync hero's rate and ETA, fixed so the figures do not move with the
/// range control (the whole history while it is shorter).
pub const HERO_RATE_WINDOW: Duration = Duration::from_secs(15 * 60);

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
    /// Peers that connected to us, from the peer list of the same poll
    /// ([`StatsHistory::record_peers`]); `None` until a list landed.
    pub peers_inbound: Option<u32>,
    /// Peers we connected to, likewise.
    pub peers_outbound: Option<u32>,
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
            peers_inbound: None,
            peers_outbound: None,
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

    /// The peers by direction, `(inbound, outbound)`, when a peer list landed with the poll.
    pub fn peer_split(&self) -> Option<(u32, u32)> {
        Some((self.peers_inbound?, self.peers_outbound?))
    }

    /// A counted field's value, for deltas and charts. The directions read 0 while no peer
    /// list landed; [`Sample::peer_split`] tells the two apart.
    pub fn get(&self, field: Field) -> u64 {
        match field {
            Field::Height => self.height,
            Field::EstimatedHeight => self.estimated_height,
            Field::Peers => u64::from(self.peers),
            Field::Mempool => self.mempool_transactions,
            Field::Behind => self.remaining(),
            Field::PeersInbound => u64::from(self.peers_inbound.unwrap_or(0)),
            Field::PeersOutbound => u64::from(self.peers_outbound.unwrap_or(0)),
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
    /// The estimated tip minus the verified height ([`Sample::remaining`]).
    Behind,
    PeersInbound,
    PeersOutbound,
}

/// The newest peer list's counts, stamped with the list's time. A list belongs to the sample
/// with its time: the monitor stamps a poll's statistics and details with one `now`, whichever
/// of the two lands first.
#[derive(Clone, Copy, Debug, PartialEq)]
struct PeerSplit {
    inbound: u32,
    outbound: u32,
    at: SystemTime,
}

/// Whether zebrad is connected to a listed peer: one without a state, or in a connected one.
/// A peer the node merely knows (`failed`, pending) is not a connection.
pub fn is_connected(peer: &PeerInfo) -> bool {
    peer.state.as_deref().is_none_or(|state| {
        state.eq_ignore_ascii_case("connected") || state.eq_ignore_ascii_case("responded")
    })
}

impl PeerSplit {
    /// Counts the connected peers ([`is_connected`]) by direction.
    fn of(details: &NodeDetails) -> Self {
        let (inbound, outbound) = details.peers.iter().filter(|peer| is_connected(peer)).fold(
            (0, 0),
            |(inbound, outbound), peer| {
                if peer.inbound {
                    (inbound + 1, outbound)
                } else {
                    (inbound, outbound + 1)
                }
            },
        );
        PeerSplit {
            inbound,
            outbound,
            at: details.updated_at,
        }
    }

    /// Whether the list belongs to the poll of a sample taken at `at`: the two share one stamp.
    fn fits(&self, at: SystemTime) -> bool {
        self.at == at
    }

    fn apply(&self, sample: &mut Sample) {
        sample.peers_inbound = Some(self.inbound);
        sample.peers_outbound = Some(self.outbound);
    }
}

/// The samples kept since the launcher opened, oldest first.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatsHistory {
    samples: Vec<Sample>,
    peer_split: Option<PeerSplit>,
}

impl StatsHistory {
    /// Records a published statistics value. A value with the newest sample's time is a repeat
    /// and is ignored; an older time is filed in order; past [`MAX_SAMPLES`] the oldest go.
    /// The newest peer list ([`StatsHistory::record_peers`]) is attached when it is from the
    /// same poll.
    pub fn push(&mut self, stats: &NodeStats) {
        let mut sample = Sample::from_stats(stats);
        if self
            .samples
            .last()
            .is_some_and(|newest| newest.at == sample.at)
        {
            return;
        }
        if let Some(split) = self.peer_split.filter(|split| split.fits(sample.at)) {
            split.apply(&mut sample);
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

    /// Records the peer list of a poll: its inbound and outbound counts go onto the sample with
    /// its time, already filed or still to come. A neighbouring poll's sample is left alone.
    pub fn record_peers(&mut self, details: &NodeDetails) {
        let split = PeerSplit::of(details);
        self.peer_split = Some(split);
        let index = self.samples.partition_point(|s| s.at < split.at);
        if let Some(sample) = self.samples.get_mut(index).filter(|s| split.fits(s.at)) {
            split.apply(sample);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// The newest sample.
    pub fn latest(&self) -> Option<&Sample> {
        self.samples.last()
    }

    /// The newest numbers: the sample of the published `stats`, with the peer list's split
    /// when one landed for the same poll, or the newest sample once zebrad stopped
    /// publishing (`None` stats). The published value wins when the history has not filed
    /// it yet.
    pub fn sample_for(&self, stats: Option<&NodeStats>) -> Option<Sample> {
        match stats {
            Some(stats) => Some(
                self.latest()
                    .filter(|sample| sample.at == stats.updated_at)
                    .copied()
                    .unwrap_or_else(|| Sample::from_stats(stats)),
            ),
            None => self.latest().copied(),
        }
    }

    /// The oldest sample at or after `from` (the oldest of all for `None`).
    pub fn oldest_from(&self, from: Option<SystemTime>) -> Option<&Sample> {
        let start = from.map_or(0, |from| self.samples.partition_point(|s| s.at < from));
        self.samples.get(start)
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

    /// When the height last went up: the `at` of the newest sample whose height exceeds its
    /// predecessor's (5 s resolution). `None` while no rise was seen.
    pub fn last_height_change_at(&self) -> Option<SystemTime> {
        self.samples
            .windows(2)
            .rev()
            .find(|pair| pair[1].height > pair[0].height)
            .map(|pair| pair[1].at)
    }

    /// When `field` last changed: the `at` of the newest sample whose value differs from its
    /// predecessor's. `None` while it never did.
    pub fn last_change_at(&self, field: Field) -> Option<SystemTime> {
        self.samples
            .windows(2)
            .rev()
            .find(|pair| pair[1].get(field) != pair[0].get(field))
            .map(|pair| pair[1].at)
    }

    /// Since when the peer count has been under `limit`: the `at` of the first sample of the
    /// trailing run with `peers < limit`, looking back no further than `from`. `None` when
    /// the newest sample has `limit` peers or more, or no sample is at or after `from`.
    pub fn low_peers_since(&self, limit: u32, from: Option<SystemTime>) -> Option<SystemTime> {
        let start = from.map_or(0, |from| self.samples.partition_point(|s| s.at < from));
        let run = &self.samples[start..];
        if run.last()?.peers >= limit {
            return None;
        }
        let first_low = run
            .iter()
            .rposition(|s| s.peers >= limit)
            .map_or(0, |index| index + 1);
        Some(run[first_low].at)
    }

    /// Whether any sample of the window carries a peer list's split, so the direction series
    /// mean something.
    pub fn has_peer_split(&self, span: Option<Duration>, now: SystemTime) -> bool {
        self.window(span, now)
            .iter()
            .any(|s| s.peer_split().is_some())
    }

    /// Whether every peer list of the window counted no inbound peer (and there was one).
    pub fn no_inbound_over(&self, span: Option<Duration>, now: SystemTime) -> bool {
        let mut known = false;
        for sample in self.window(span, now) {
            match sample.peers_inbound {
                Some(0) => known = true,
                Some(_) => return false,
                None => {}
            }
        }
        known
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

    fn with_peers(secs: u64, height: u64, peers: u32) -> NodeStats {
        NodeStats {
            peers,
            ..stats(secs, height)
        }
    }

    fn history(points: &[(u64, u64)]) -> StatsHistory {
        let mut history = StatsHistory::default();
        for &(secs, height) in points {
            history.push(&stats(secs, height));
        }
        history
    }

    fn peer(inbound: bool, state: Option<&str>) -> PeerInfo {
        PeerInfo {
            addr: "192.0.2.1:8233".into(),
            inbound,
            user_agent: None,
            protocol_version: None,
            state: state.map(str::to_owned),
            last_seen: None,
            ping_secs: None,
        }
    }

    fn details(secs: u64, peers: Vec<PeerInfo>) -> NodeDetails {
        NodeDetails {
            peers,
            best_block_hash: None,
            headers: None,
            difficulty: None,
            size_on_disk: None,
            chain_supply_zec: None,
            value_pools: Vec::new(),
            active_upgrade: None,
            subversion: None,
            protocol_version: None,
            mempool_bytes: None,
            mempool_usage: None,
            updated_at: at(secs),
        }
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
        assert_eq!(history.oldest_from(None).map(|s| s.height), Some(100));
        assert_eq!(
            history.oldest_from(Some(at(6))).map(|s| s.height),
            Some(115)
        );
        assert_eq!(history.oldest_from(Some(at(11))), None);
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
        // The hero's window is shorter than the range control's longest and covers a short
        // history whole.
        assert_eq!(HERO_RATE_WINDOW, Duration::from_secs(900));
        let rate = history(&[(0, 100), (30, 130), (60, 190)])
            .block_rate(Some(HERO_RATE_WINDOW), now)
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

    #[test]
    fn behind_is_the_estimate_minus_the_height() {
        let sample = Sample::from_stats(&stats(0, 2_952_680));
        assert_eq!(sample.get(Field::Behind), 167_320);
        assert_eq!(sample.get(Field::Behind), sample.remaining());
        assert_eq!(
            Sample::from_stats(&stats(0, 3_130_000)).get(Field::Behind),
            0
        );
        // Converging while syncing: the delta of the field is negative.
        let history = history(&[(0, 100), (60, 1100)]);
        assert_eq!(history.delta(Field::Behind, None, at(60)), Some(-1000));
    }

    #[test]
    fn last_height_change_is_the_newest_rise() {
        let now = at(100);
        assert_eq!(StatsHistory::default().last_height_change_at(), None);
        assert_eq!(history(&[(0, 100)]).last_height_change_at(), None);
        // Flat for the whole history: no rise.
        assert_eq!(
            history(&[(0, 100), (5, 100), (10, 100)]).last_height_change_at(),
            None
        );
        let rose = history(&[(0, 100), (5, 110), (10, 110), (15, 110)]);
        assert_eq!(rose.last_height_change_at(), Some(at(5)));
        // A fall is not a rise, a rise after it is.
        let fell = history(&[(0, 100), (5, 90), (10, 95), (15, 95)]);
        assert_eq!(fell.last_height_change_at(), Some(at(10)));
        // Any change counts for the general accessor, the estimate included.
        assert_eq!(fell.last_change_at(Field::Height), Some(at(10)));
        assert_eq!(fell.last_change_at(Field::EstimatedHeight), None);
        let mut estimate_moved = fell.clone();
        estimate_moved.push(&NodeStats {
            estimated_height: 3_120_010,
            ..stats(20, 95)
        });
        assert_eq!(
            estimate_moved.last_change_at(Field::EstimatedHeight),
            Some(at(20))
        );
        assert_eq!(estimate_moved.last_height_change_at(), Some(at(10)));
        let _ = now;
    }

    #[test]
    fn low_peers_since_the_trailing_run() {
        let mut history = StatsHistory::default();
        for (secs, peers) in [(0, 5), (5, 0), (10, 0), (15, 2), (20, 0), (25, 0), (30, 0)] {
            history.push(&with_peers(secs, 100, peers));
        }
        // No peers since the sample after the last one with peers.
        assert_eq!(history.low_peers_since(1, None), Some(at(20)));
        // Few peers (under 3) since the first sample after the one with 5.
        assert_eq!(history.low_peers_since(3, None), Some(at(5)));
        // Looking back no further than a run start.
        assert_eq!(history.low_peers_since(3, Some(at(12))), Some(at(15)));
        assert_eq!(history.low_peers_since(3, Some(at(22))), Some(at(25)));
        assert_eq!(history.low_peers_since(3, Some(at(31))), None, "no sample");
        // Peers are back: nothing low.
        history.push(&with_peers(35, 100, 4));
        assert_eq!(history.low_peers_since(1, None), None);
        assert_eq!(history.low_peers_since(3, None), None);
        assert_eq!(StatsHistory::default().low_peers_since(1, None), None);
        // Zero peers from the first sample on: since that sample.
        let never = {
            let mut h = StatsHistory::default();
            h.push(&with_peers(0, 1, 0));
            h.push(&with_peers(5, 1, 0));
            h
        };
        assert_eq!(never.low_peers_since(1, None), Some(at(0)));
    }

    #[test]
    fn peer_lists_split_the_sample_of_their_poll() {
        let mut history = history(&[(0, 100), (5, 110)]);
        assert_eq!(history.latest().unwrap().peer_split(), None);
        assert!(!history.has_peer_split(None, at(5)));
        assert!(!history.no_inbound_over(None, at(5)));
        // The list of the same poll, landing after the stats: the sample of its time takes it.
        let list = vec![
            peer(true, None),
            peer(false, None),
            peer(false, Some("connected")),
            peer(false, Some("failed")),
            peer(true, Some("Responded")),
        ];
        history.record_peers(&details(5, list.clone()));
        let newest = *history.latest().unwrap();
        assert_eq!(
            newest.peer_split(),
            Some((2, 2)),
            "failed peers not counted"
        );
        // The published stats of that poll read with the split; newer stats, not filed yet,
        // without it; no stats at all gives the newest sample.
        assert_eq!(history.sample_for(Some(&stats(5, 110))), Some(newest));
        let unfiled = history.sample_for(Some(&stats(7, 111))).unwrap();
        assert_eq!((unfiled.height, unfiled.peer_split()), (111, None));
        assert_eq!(history.sample_for(None), Some(newest));
        assert_eq!(StatsHistory::default().sample_for(None), None);
        assert_eq!(newest.get(Field::PeersInbound), 2);
        assert_eq!(newest.get(Field::PeersOutbound), 2);
        assert_eq!(history.window(None, at(6))[0].peer_split(), None);
        assert!(history.has_peer_split(None, at(6)));
        // A list landing before its stats is attached when they come.
        history.record_peers(&details(10, vec![peer(false, None)]));
        history.push(&stats(10, 120));
        assert_eq!(history.latest().unwrap().peer_split(), Some((0, 1)));
        // The previous poll's list is not attached to the next poll's sample.
        history.push(&stats(40, 130));
        assert_eq!(history.latest().unwrap().peer_split(), None);
        // The list goes to the sample of its time, not the newest one.
        history.push(&stats(45, 131));
        history.record_peers(&details(40, vec![peer(true, None)]));
        let samples = history.window(None, at(45));
        assert_eq!(samples[samples.len() - 2].peer_split(), Some((1, 0)));
        assert_eq!(samples[samples.len() - 1].peer_split(), None);
        // A list of a poll whose sample landed first, a neighbouring poll's sample already
        // filed: neither neighbour takes it.
        history.record_peers(&details(43, vec![peer(true, None), peer(true, None)]));
        let samples = history.window(None, at(45));
        assert_eq!(samples[samples.len() - 2].peer_split(), Some((1, 0)));
        assert_eq!(samples[samples.len() - 1].peer_split(), None);
        assert!(
            !history.no_inbound_over(None, at(45)),
            "one split has inbound"
        );
        // "No inbound peers" over a range needs every known split at zero inbound.
        let mut quiet = StatsHistory::default();
        quiet.push(&stats(0, 1));
        quiet.push(&stats(5, 2));
        assert!(!quiet.no_inbound_over(None, at(5)), "nothing known");
        quiet.record_peers(&details(5, vec![peer(false, None)]));
        assert!(quiet.no_inbound_over(None, at(5)));
        quiet.push(&stats(30, 3));
        assert!(
            quiet.no_inbound_over(None, at(30)),
            "unknown samples do not count"
        );
        quiet.record_peers(&details(30, vec![peer(true, None)]));
        assert!(!quiet.no_inbound_over(None, at(30)));
        assert!(!quiet.no_inbound_over(Some(Duration::from_secs(1)), at(30)));
        // A list far from every sample is kept for the next push only.
        let mut late = StatsHistory::default();
        late.record_peers(&details(0, vec![peer(true, None)]));
        late.push(&stats(100, 1));
        assert_eq!(late.latest().unwrap().peer_split(), None);
        assert_eq!(
            PeerSplit::of(&details(0, Vec::new())),
            PeerSplit {
                inbound: 0,
                outbound: 0,
                at: at(0)
            }
        );
    }
}
