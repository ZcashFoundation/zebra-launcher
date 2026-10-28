//! The snapshots of zebrad's metrics page kept since the launcher opened, for the dashboard's
//! derived series (disk growth, network rates, ready peers, downloads in flight). Pure Rust,
//! tested natively.
//!
//! A page has well over a hundred families, histograms among them, so a kept snapshot is
//! reduced to the families in [`KEPT_FAMILIES`]; the newest snapshot stays whole for the
//! one-off gauges the page reads ([`MetricsHistory::latest`]).

// Read by the dashboard's view workstream; the accessors it does not use yet stay for it.
#![allow(dead_code, reason = "read by the dashboard once its view lands")]

use std::time::{Duration, SystemTime};

use launcher_types::MetricsSnapshot;

use crate::stats_history::MIN_RATE_SPAN;

/// Four hours of 10-second snapshots.
pub const MAX_SNAPSHOTS: usize = 1440;

/// The wire names (underscores) of the series the dashboard derives from the history.
pub mod names {
    pub const PEERS: &str = "zcash_net_peers";
    pub const READY_PEERS: &str = "pool_num_ready";
    pub const UNREADY_PEERS: &str = "pool_num_unready";
    pub const DOWNLOADS_IN_FLIGHT: &str = "sync_downloads_in_flight";
    pub const DISTANCE_TO_TIP: &str = "sync_estimated_distance_to_tip";
    pub const CHECKPOINT_HEIGHT: &str = "checkpoint_verified_height";
    pub const FINALIZED_HEIGHT: &str = "state_finalized_block_height";
    pub const CHAIN_COUNT: &str = "state_memory_chain_count";
    pub const DISK_BYTES: &str = "zebra_state_rocksdb_total_disk_size_bytes";
    pub const MEMORY_BYTES: &str = "zebra_state_rocksdb_total_memory_size_bytes";
    pub const BLOCK_CACHE_BYTES: &str = "zebra_state_rocksdb_block_cache_usage_bytes";
    pub const MEMPOOL_TRANSACTIONS: &str = "zcash_mempool_size_transactions";
    pub const MEMPOOL_BYTES: &str = "zcash_mempool_size_bytes";
    pub const VERIFIED_BLOCKS: &str = "zcash_chain_verified_block_total";
    pub const DOWNLOADED_BLOCKS: &str = "sync_downloaded_block_count";
    pub const NET_IN_BYTES: &str = "zcash_net_in_bytes_total";
    pub const NET_OUT_BYTES: &str = "zcash_net_out_bytes_total";
    pub const HANDSHAKE_FAILURES: &str = "zcash_net_peer_handshake_failures_total";
    pub const OBSOLETE_PEERS: &str = "zcash_net_peers_version_obsolete";
}

/// The families a kept snapshot holds: everything the dashboard plots or derives over time.
pub const KEPT_FAMILIES: &[&str] = &[
    names::PEERS,
    names::READY_PEERS,
    names::UNREADY_PEERS,
    names::DOWNLOADS_IN_FLIGHT,
    names::DISTANCE_TO_TIP,
    names::CHECKPOINT_HEIGHT,
    names::FINALIZED_HEIGHT,
    names::CHAIN_COUNT,
    names::DISK_BYTES,
    names::MEMORY_BYTES,
    names::BLOCK_CACHE_BYTES,
    names::MEMPOOL_TRANSACTIONS,
    names::MEMPOOL_BYTES,
    names::VERIFIED_BLOCKS,
    names::DOWNLOADED_BLOCKS,
    names::NET_IN_BYTES,
    names::NET_OUT_BYTES,
    names::HANDSHAKE_FAILURES,
    names::OBSOLETE_PEERS,
];

/// The snapshots, oldest first.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MetricsHistory {
    /// Reduced to [`KEPT_FAMILIES`].
    snapshots: Vec<MetricsSnapshot>,
    /// The newest snapshot, whole.
    newest: Option<MetricsSnapshot>,
}

/// The value of the family `name` in `snapshot`, whatever its kind: its one sample
/// ([`launcher_types::MetricFamily::single`]), when finite.
fn value_of(snapshot: &MetricsSnapshot, name: &str) -> Option<f64> {
    snapshot
        .family(name)?
        .single()
        .filter(|value| value.is_finite())
}

/// A non-negative value as a count.
fn count_of(value: f64) -> u64 {
    value.round().max(0.0) as u64
}

/// `snapshot` with only the kept families.
fn reduce(snapshot: &MetricsSnapshot) -> MetricsSnapshot {
    MetricsSnapshot {
        at: snapshot.at,
        families: snapshot
            .families
            .iter()
            .filter(|family| KEPT_FAMILIES.contains(&family.name.as_str()))
            .cloned()
            .collect(),
    }
}

impl MetricsHistory {
    /// Records a published snapshot. One not newer than the newest kept (a repeat, or a
    /// late one) is ignored; past [`MAX_SNAPSHOTS`] the oldest go.
    pub fn push(&mut self, snapshot: &MetricsSnapshot) {
        if self
            .snapshots
            .last()
            .is_some_and(|last| snapshot.at <= last.at)
        {
            return;
        }
        self.snapshots.push(reduce(snapshot));
        self.newest = Some(snapshot.clone());
        let excess = self.snapshots.len().saturating_sub(MAX_SNAPSHOTS);
        self.snapshots.drain(..excess);
    }

    pub fn is_empty(&self) -> bool {
        self.snapshots.is_empty()
    }

    /// The newest snapshot, whole.
    pub fn latest(&self) -> Option<&MetricsSnapshot> {
        self.newest.as_ref()
    }

    /// The snapshots within `span` of `now` (all of them for `None`), oldest first, each
    /// reduced to [`KEPT_FAMILIES`].
    pub fn window(&self, span: Option<Duration>, now: SystemTime) -> &[MetricsSnapshot] {
        let Some(span) = span else {
            return &self.snapshots;
        };
        let Some(cutoff) = now.checked_sub(span) else {
            return &self.snapshots;
        };
        let start = self.snapshots.partition_point(|s| s.at < cutoff);
        &self.snapshots[start..]
    }

    /// The series `name` over the window: `(time, value)` for every snapshot that has it.
    pub fn series(
        &self,
        name: &str,
        span: Option<Duration>,
        now: SystemTime,
    ) -> Vec<(SystemTime, f64)> {
        self.window(span, now)
            .iter()
            .filter_map(|s| Some((s.at, value_of(s, name)?)))
            .collect()
    }

    /// Newest minus oldest value of the gauge `name` over the window; `None` with fewer than
    /// two readings.
    fn gauge_delta(&self, name: &str, span: Option<Duration>, now: SystemTime) -> Option<f64> {
        let series = self.series(name, span, now);
        let (first, last) = (series.first()?, series.last()?);
        (series.len() >= 2).then_some(last.1 - first.1)
    }

    /// The counter `name` over the window: the sum of its rises between consecutive readings
    /// and the time those rises took. A fall (the counter was reset by a restart) is left
    /// out, with its interval, so a rate is never negative. `None` without a pair.
    fn counter_growth(
        &self,
        name: &str,
        span: Option<Duration>,
        now: SystemTime,
    ) -> Option<(f64, Duration)> {
        let series = self.series(name, span, now);
        let mut growth = 0.0;
        let mut elapsed = Duration::ZERO;
        let mut pairs = 0;
        for pair in series.windows(2) {
            let (delta, interval) = (pair[1].1 - pair[0].1, pair[1].0.duration_since(pair[0].0));
            let Ok(interval) = interval else {
                continue;
            };
            if delta < 0.0 || interval.is_zero() {
                continue;
            }
            growth += delta;
            elapsed += interval;
            pairs += 1;
        }
        (pairs > 0).then_some((growth, elapsed))
    }

    /// Bytes the state database grew by over the window.
    pub fn disk_delta(&self, span: Option<Duration>, now: SystemTime) -> Option<i64> {
        self.gauge_delta(names::DISK_BYTES, span, now)
            .map(|delta| delta.round() as i64)
    }

    /// Network bytes per second in and out over the window, skipping counter resets.
    pub fn net_rate(&self, span: Option<Duration>, now: SystemTime) -> Option<(f64, f64)> {
        let rate = |name| {
            let (growth, elapsed) = self.counter_growth(name, span, now)?;
            Some(growth / elapsed.as_secs_f64())
        };
        Some((rate(names::NET_IN_BYTES)?, rate(names::NET_OUT_BYTES)?))
    }

    /// Verified blocks per minute over the window, from zebrad's own counter (gossiped blocks
    /// count too). `None` until the readings span [`MIN_RATE_SPAN`].
    pub fn verified_block_rate(&self, span: Option<Duration>, now: SystemTime) -> Option<f64> {
        let (growth, elapsed) = self.counter_growth(names::VERIFIED_BLOCKS, span, now)?;
        if elapsed < MIN_RATE_SPAN {
            return None;
        }
        Some(growth / (elapsed.as_secs_f64() / 60.0))
    }

    /// The newest reading of `name`, whatever its kind.
    pub fn newest_value(&self, name: &str) -> Option<f64> {
        value_of(self.newest.as_ref()?, name)
    }

    /// The newest reading of `name` as a count.
    fn newest_count(&self, name: &str) -> Option<u64> {
        self.newest_value(name).map(count_of)
    }

    /// Peers the peer set reports as ready, from the newest snapshot.
    pub fn ready_peers(&self) -> Option<u64> {
        self.newest_count(names::READY_PEERS)
    }

    /// Peers the peer set reports as not ready, from the newest snapshot.
    pub fn unready_peers(&self) -> Option<u64> {
        self.newest_count(names::UNREADY_PEERS)
    }

    /// Block downloads in flight, from the newest snapshot.
    pub fn downloads_in_flight(&self) -> Option<u64> {
        self.newest_count(names::DOWNLOADS_IN_FLIGHT)
    }

    /// The checkpoint verifier's height, from the newest snapshot.
    pub fn checkpoint_height(&self) -> Option<u64> {
        self.newest_count(names::CHECKPOINT_HEIGHT)
    }

    /// Chains in memory (more than one means a fork is being resolved).
    pub fn chain_count(&self) -> Option<u64> {
        self.newest_count(names::CHAIN_COUNT)
    }

    /// The state database's size on disk, in bytes.
    pub fn disk_bytes(&self) -> Option<u64> {
        self.newest_count(names::DISK_BYTES)
    }

    /// The state database's memory, in bytes: its tables plus the block cache.
    pub fn memory_bytes(&self) -> Option<u64> {
        let tables = self.newest_count(names::MEMORY_BYTES)?;
        let cache = self.newest_count(names::BLOCK_CACHE_BYTES).unwrap_or(0);
        Some(tables.saturating_add(cache))
    }

    /// The mempool's transactions' size, in bytes.
    pub fn mempool_bytes(&self) -> Option<u64> {
        self.newest_count(names::MEMPOOL_BYTES)
    }

    /// Peer handshakes that failed since zebrad started.
    pub fn handshake_failures(&self) -> Option<u64> {
        self.newest_count(names::HANDSHAKE_FAILURES)
    }

    /// Connected peers on an obsolete protocol version.
    pub fn obsolete_peers(&self) -> Option<u64> {
        self.newest_count(names::OBSOLETE_PEERS)
    }
}

#[cfg(test)]
mod tests {
    use std::time::UNIX_EPOCH;

    use launcher_types::{MetricFamily, MetricKind, MetricSample};

    use super::*;

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn family(name: &str, kind: MetricKind, value: f64) -> MetricFamily {
        MetricFamily {
            name: name.into(),
            kind,
            help: None,
            samples: vec![MetricSample {
                name: name.into(),
                labels: Vec::new(),
                value,
            }],
        }
    }

    /// A snapshot with the series the tests move: `(disk, net in, net out, verified blocks)`.
    fn snapshot(secs: u64, disk: f64, net_in: f64, net_out: f64, verified: f64) -> MetricsSnapshot {
        MetricsSnapshot {
            at: at(secs),
            families: vec![
                family(names::DISK_BYTES, MetricKind::Gauge, disk),
                family(names::NET_IN_BYTES, MetricKind::Counter, net_in),
                family(names::NET_OUT_BYTES, MetricKind::Counter, net_out),
                family(names::VERIFIED_BLOCKS, MetricKind::Counter, verified),
                family(names::READY_PEERS, MetricKind::Gauge, 11.0),
                family(names::UNREADY_PEERS, MetricKind::Gauge, 1.0),
                family(names::DOWNLOADS_IN_FLIGHT, MetricKind::Gauge, 42.0),
                family(names::CHECKPOINT_HEIGHT, MetricKind::Gauge, 2_500_000.0),
                family(names::CHAIN_COUNT, MetricKind::Gauge, 2.0),
                family(names::MEMORY_BYTES, MetricKind::Gauge, 1_000_000_000.0),
                family(names::BLOCK_CACHE_BYTES, MetricKind::Gauge, 200_000_000.0),
                family(names::MEMPOOL_BYTES, MetricKind::Gauge, 44_400.0),
                family(names::HANDSHAKE_FAILURES, MetricKind::Counter, 3.0),
                family(names::OBSOLETE_PEERS, MetricKind::Gauge, 1.0),
                // Not kept in the history, but on the newest snapshot.
                family("rpc_active_requests", MetricKind::Gauge, 2.0),
                MetricFamily {
                    name: "sync_block_verify_duration_seconds".into(),
                    kind: MetricKind::Histogram,
                    help: None,
                    samples: vec![MetricSample {
                        name: "sync_block_verify_duration_seconds_count".into(),
                        labels: Vec::new(),
                        value: 7.0,
                    }],
                },
            ],
        }
    }

    fn history(points: &[(u64, f64, f64, f64, f64)]) -> MetricsHistory {
        let mut history = MetricsHistory::default();
        for &(secs, disk, net_in, net_out, verified) in points {
            history.push(&snapshot(secs, disk, net_in, net_out, verified));
        }
        history
    }

    #[test]
    fn push_keeps_order_reduces_and_caps() {
        let mut history = history(&[(0, 1.0, 0.0, 0.0, 0.0), (10, 2.0, 0.0, 0.0, 0.0)]);
        assert!(!history.is_empty());
        assert!(MetricsHistory::default().is_empty());
        // A repeat and a late snapshot are ignored.
        history.push(&snapshot(10, 99.0, 0.0, 0.0, 0.0));
        history.push(&snapshot(5, 99.0, 0.0, 0.0, 0.0));
        let all = history.window(None, at(10));
        assert_eq!(all.len(), 2);
        assert_eq!(value_of(&all[1], names::DISK_BYTES), Some(2.0));
        // Kept snapshots hold the dashboard's families only; the newest is whole.
        assert!(
            all.iter()
                .all(|s| s.families.len() == KEPT_FAMILIES.len() - 5)
        );
        assert!(all[0].family("rpc_active_requests").is_none());
        let newest = history.latest().unwrap();
        assert_eq!(newest.gauge("rpc_active_requests"), Some(2.0));
        assert_eq!(newest.at, at(10));
        assert_eq!(history.newest_value("rpc_active_requests"), Some(2.0));
        // The cap drops the oldest.
        let mut long = MetricsHistory::default();
        for i in 0..(MAX_SNAPSHOTS as u64 + 5) {
            long.push(&snapshot(i * 10, i as f64, 0.0, 0.0, 0.0));
        }
        let all = long.window(None, at(0));
        assert_eq!(all.len(), MAX_SNAPSHOTS);
        assert_eq!(value_of(&all[0], names::DISK_BYTES), Some(5.0));
    }

    #[test]
    fn window_is_by_time() {
        let history = history(&[
            (0, 1.0, 0.0, 0.0, 0.0),
            (60, 2.0, 0.0, 0.0, 0.0),
            (120, 3.0, 0.0, 0.0, 0.0),
            (180, 4.0, 0.0, 0.0, 0.0),
        ]);
        let now = at(200);
        assert_eq!(history.window(None, now).len(), 4);
        let recent = history.window(Some(Duration::from_secs(90)), now);
        assert_eq!(recent.len(), 2, "cutoff 110 keeps 120 and 180");
        assert_eq!(recent[0].at, at(120));
        assert_eq!(history.window(Some(Duration::from_secs(80)), now).len(), 2);
        assert!(history.window(Some(Duration::from_secs(5)), now).is_empty());
        assert_eq!(
            history
                .window(Some(Duration::from_secs(u64::MAX)), now)
                .len(),
            4
        );
        let series = history.series(names::DISK_BYTES, Some(Duration::from_secs(90)), now);
        assert_eq!(series, vec![(at(120), 3.0), (at(180), 4.0)]);
        assert!(history.series("missing", None, now).is_empty());
    }

    #[test]
    fn disk_delta_is_last_minus_first() {
        let now = at(30);
        assert_eq!(MetricsHistory::default().disk_delta(None, now), None);
        assert_eq!(
            history(&[(0, 100.0, 0.0, 0.0, 0.0)]).disk_delta(None, now),
            None,
            "one reading is no delta"
        );
        let history = history(&[
            (0, 100.0, 0.0, 0.0, 0.0),
            (10, 150.0, 0.0, 0.0, 0.0),
            (20, 130.0, 0.0, 0.0, 0.0),
        ]);
        assert_eq!(history.disk_delta(None, now), Some(30));
        assert_eq!(
            history.disk_delta(Some(Duration::from_secs(25)), now),
            Some(-20)
        );
        assert_eq!(history.disk_delta(Some(Duration::from_secs(5)), now), None);
    }

    #[test]
    fn net_rate_skips_counter_resets() {
        let now = at(100);
        assert_eq!(MetricsHistory::default().net_rate(None, now), None);
        assert_eq!(
            history(&[(0, 0.0, 1000.0, 100.0, 0.0)]).net_rate(None, now),
            None
        );
        // 12 MB in and 3 MB out per 10 s.
        let steady = history(&[
            (0, 0.0, 0.0, 0.0, 0.0),
            (10, 0.0, 120e6, 30e6, 0.0),
            (20, 0.0, 240e6, 60e6, 0.0),
        ]);
        let (rate_in, rate_out) = steady.net_rate(None, now).unwrap();
        assert!((rate_in - 12e6).abs() < 1e-6, "{rate_in}");
        assert!((rate_out - 3e6).abs() < 1e-6, "{rate_out}");
        // A restart resets the counters: that pair is left out, the rate stays positive.
        let reset = history(&[
            (0, 0.0, 100e6, 10e6, 0.0),
            (10, 0.0, 220e6, 40e6, 0.0),
            (20, 0.0, 5e6, 1e6, 0.0),
            (30, 0.0, 125e6, 31e6, 0.0),
        ]);
        let (rate_in, rate_out) = reset.net_rate(None, now).unwrap();
        assert!((rate_in - 12e6).abs() < 1e-6, "{rate_in}");
        assert!((rate_out - 3e6).abs() < 1e-6, "{rate_out}");
        // Only the reset pair: no rate at all rather than a negative one.
        let only_reset = history(&[(0, 0.0, 220e6, 40e6, 0.0), (10, 0.0, 5e6, 1e6, 0.0)]);
        assert_eq!(only_reset.net_rate(None, now), None);
        // A gap in the readings counts its whole time.
        let gap = history(&[(0, 0.0, 0.0, 0.0, 0.0), (60, 0.0, 60e6, 6e6, 0.0)]);
        let (rate_in, rate_out) = gap.net_rate(None, now).unwrap();
        assert!((rate_in - 1e6).abs() < 1e-6 && (rate_out - 0.1e6).abs() < 1e-6);
        // The window scopes it.
        assert_eq!(reset.net_rate(Some(Duration::from_secs(5)), at(30)), None);
    }

    #[test]
    fn verified_block_rate_needs_thirty_seconds() {
        let now = at(100);
        assert_eq!(
            MetricsHistory::default().verified_block_rate(None, now),
            None
        );
        assert_eq!(
            history(&[(0, 0.0, 0.0, 0.0, 100.0), (20, 0.0, 0.0, 0.0, 400.0)])
                .verified_block_rate(None, now),
            None,
            "20 s is too short"
        );
        let rate = history(&[
            (0, 0.0, 0.0, 0.0, 100.0),
            (30, 0.0, 0.0, 0.0, 130.0),
            (60, 0.0, 0.0, 0.0, 190.0),
        ])
        .verified_block_rate(None, now)
        .unwrap();
        assert!((rate - 90.0).abs() < 1e-9, "90 blocks in a minute: {rate}");
        // A reset in the middle is skipped and the rest still needs the span.
        let reset = history(&[
            (0, 0.0, 0.0, 0.0, 1000.0),
            (10, 0.0, 0.0, 0.0, 1100.0),
            (20, 0.0, 0.0, 0.0, 5.0),
            (30, 0.0, 0.0, 0.0, 105.0),
        ]);
        assert_eq!(reset.verified_block_rate(None, now), None, "20 s of rises");
        let mut reset = reset;
        reset.push(&snapshot(40, 0.0, 0.0, 0.0, 205.0));
        let rate = reset.verified_block_rate(None, now).unwrap();
        assert!((rate - 600.0).abs() < 1e-9, "100 blocks per 10 s: {rate}");
    }

    #[test]
    fn newest_counts() {
        let empty = MetricsHistory::default();
        assert_eq!(empty.ready_peers(), None);
        assert_eq!(empty.chain_count(), None);
        assert_eq!(empty.memory_bytes(), None);
        let history = history(&[(0, 312_400_000_000.0, 0.0, 0.0, 0.0)]);
        assert_eq!(history.ready_peers(), Some(11));
        assert_eq!(history.unready_peers(), Some(1));
        assert_eq!(history.downloads_in_flight(), Some(42));
        assert_eq!(history.checkpoint_height(), Some(2_500_000));
        assert_eq!(history.chain_count(), Some(2));
        assert_eq!(history.disk_bytes(), Some(312_400_000_000));
        assert_eq!(history.memory_bytes(), Some(1_200_000_000));
        assert_eq!(history.mempool_bytes(), Some(44_400));
        assert_eq!(history.handshake_failures(), Some(3));
        assert_eq!(history.obsolete_peers(), Some(1));
        // A gauge with a non-finite value, or missing, is unknown; a negative one is zero.
        let mut odd = MetricsHistory::default();
        odd.push(&MetricsSnapshot {
            at: at(0),
            families: vec![
                family(names::READY_PEERS, MetricKind::Gauge, f64::NAN),
                family(names::CHAIN_COUNT, MetricKind::Gauge, -1.0),
                family(names::MEMORY_BYTES, MetricKind::Gauge, 5.0),
            ],
        });
        assert_eq!(odd.ready_peers(), None);
        assert_eq!(odd.downloads_in_flight(), None);
        assert_eq!(odd.chain_count(), Some(0));
        assert_eq!(
            odd.memory_bytes(),
            Some(5),
            "no block cache gauge: tables alone"
        );
    }
}
