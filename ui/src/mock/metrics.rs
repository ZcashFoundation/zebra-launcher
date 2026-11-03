//! The mock's metrics page: a snapshot every 10 s with plausible, moving values for every
//! series the dashboard reads, derived from the node statistics so the two agree. Names are
//! the wire names (underscores); zebrad labels the `zcash_*` gauges with the network.

use std::time::SystemTime;

use launcher_types::{MetricFamily, MetricKind, MetricSample, MetricsSnapshot, NodeStats};
use leptos::prelude::*;

use super::{Scenario, mock};

/// The final checkpoint of zebrad 7.0.0 on Mainnet: full validation runs above it.
const FINAL_CHECKPOINT: f64 = 2_900_000.0;

/// What the snapshot is built from.
pub(super) struct MetricsInput<'a> {
    pub stats: &'a NodeStats,
    /// Metrics ticks since the launcher opened (one per 10 s), for the moving values.
    pub tick: u64,
    /// The height the current run started at: zebrad's counters begin at zero with each run.
    pub run_height_base: u64,
    /// The size of the chain state on disk.
    pub state_bytes: u64,
    pub scenario: Scenario,
    pub at: SystemTime,
}

fn family(name: &str, kind: MetricKind, labels: &[(&str, &str)], value: f64) -> MetricFamily {
    MetricFamily {
        name: name.to_owned(),
        kind,
        help: None,
        samples: vec![MetricSample {
            name: name.to_owned(),
            labels: labels
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect(),
            value,
        }],
    }
}

fn gauge(name: &str, value: f64) -> MetricFamily {
    family(name, MetricKind::Gauge, &[], value)
}

fn counter(name: &str, value: f64) -> MetricFamily {
    family(name, MetricKind::Counter, &[], value)
}

/// A `zcash_*` gauge, labelled with the network as zebrad's are.
fn net_gauge(name: &str, value: f64) -> MetricFamily {
    family(name, MetricKind::Gauge, &[("network", "main")], value)
}

/// A snapshot derived from the mock's node statistics.
pub(super) fn snapshot(input: &MetricsInput<'_>) -> MetricsSnapshot {
    let stats = input.stats;
    let tick = input.tick as f64;
    let height = stats.height as f64;
    let behind = stats.estimated_height.saturating_sub(stats.height) as f64;
    let verified_this_run = stats.height.saturating_sub(input.run_height_base) as f64;
    let peers = f64::from(stats.peers);
    let syncing = behind > 1.0 && input.scenario == Scenario::Normal;
    // Block downloads only while catching up; a few in flight at the tip.
    let in_flight = match input.scenario {
        Scenario::Normal if syncing => 40.0 + (tick % 7.0) * 3.0,
        Scenario::Normal => (tick % 3.0).floor(),
        Scenario::Stalled => 2.0,
        Scenario::NoPeers => 0.0,
    };
    let obsolete = if input.scenario == Scenario::NoPeers {
        0.0
    } else {
        1.0
    };
    MetricsSnapshot {
        at: input.at,
        families: vec![
            net_gauge("zcash_net_peers", peers),
            net_gauge("zcash_net_peers_version_connected", peers),
            net_gauge("zcash_net_peers_version_obsolete", obsolete),
            gauge("pool_num_ready", (peers - 1.0).max(0.0)),
            gauge("pool_num_unready", peers.min(1.0)),
            gauge("sync_downloads_in_flight", in_flight),
            gauge("sync_estimated_distance_to_tip", behind),
            gauge(
                "sync_estimated_network_tip_height",
                stats.estimated_height as f64,
            ),
            gauge("sync_prospective_tips_len", if syncing { 1.0 } else { 0.0 }),
            gauge("checkpoint_verified_height", FINAL_CHECKPOINT),
            gauge("checkpoint_verified_count", FINAL_CHECKPOINT / 400.0),
            net_gauge("zcash_chain_verified_block_height", height),
            gauge("state_finalized_block_height", height - 100.0),
            gauge("state_memory_best_chain_length", 100.0),
            gauge("state_memory_chain_count", 1.0),
            gauge(
                "zebra_state_rocksdb_total_disk_size_bytes",
                input.state_bytes as f64 + tick * 25_000_000.0,
            ),
            gauge(
                "zebra_state_rocksdb_live_data_size_bytes",
                input.state_bytes as f64 + tick * 24_500_000.0,
            ),
            gauge(
                "zebra_state_rocksdb_total_memory_size_bytes",
                1_200_000_000.0 + (tick % 12.0) * 8_000_000.0,
            ),
            gauge(
                "zebra_state_rocksdb_block_cache_usage_bytes",
                420_000_000.0 + (tick % 9.0) * 3_000_000.0,
            ),
            net_gauge(
                "zcash_mempool_size_transactions",
                stats.mempool_transactions as f64,
            ),
            net_gauge(
                "zcash_mempool_size_bytes",
                stats.mempool_transactions as f64 * 1_850.0,
            ),
            net_gauge(
                "zcash_mempool_cost_bytes",
                stats.mempool_transactions as f64 * 2_100.0,
            ),
            counter("zcash_chain_verified_block_total", verified_this_run),
            counter("sync_downloaded_block_count", verified_this_run + 60.0),
            counter("sync_verified_block_count", verified_this_run),
            counter(
                "state_finalized_block_count",
                (verified_this_run - 100.0).max(0.0),
            ),
            // About 1.2 MB/s in and 0.3 MB/s out while syncing, with some unevenness.
            counter(
                "zcash_net_in_bytes_total",
                tick * 12_000_000.0 + (tick % 4.0) * 900_000.0,
            ),
            counter(
                "zcash_net_out_bytes_total",
                tick * 3_000_000.0 + (tick % 3.0) * 200_000.0,
            ),
            counter("zcash_net_in_messages", tick * 2_400.0),
            counter("zcash_net_out_messages", tick * 1_900.0),
            counter(
                "zcash_net_peer_handshake_failures_total",
                3.0 + (tick / 50.0).floor(),
            ),
            counter("zcash_net_peers_obsolete", obsolete),
            counter("rpc_requests_total", tick * 4.0 + 4.0),
            counter("rpc_errors_total", 0.0),
        ],
    }
}

/// Calls `handler` with every metrics snapshot, starting with the current one.
pub fn on_metrics(handler: impl Fn(Option<MetricsSnapshot>) + 'static) {
    let metrics = mock().metrics;
    Effect::new(move |_| handler(metrics.get()));
}

pub async fn get_metrics() -> Option<MetricsSnapshot> {
    super::sleep(50).await;
    mock().metrics.get_untracked()
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

    fn stats(height: u64, peers: u32) -> NodeStats {
        NodeStats {
            height,
            estimated_height: 3_120_000,
            verification_progress: height as f64 / 3_120_000.0,
            peers,
            mempool_transactions: 24,
            chain: "main".into(),
            zebrad_version: "7.0.0".into(),
            last_error: None,
            eos: None,
            updated_at: UNIX_EPOCH,
        }
    }

    fn input(stats: &NodeStats, tick: u64, scenario: Scenario) -> MetricsInput<'_> {
        MetricsInput {
            stats,
            tick,
            run_height_base: 2_950_000,
            state_bytes: 248_300_000_000,
            scenario,
            at: UNIX_EPOCH + Duration::from_secs(tick * 10),
        }
    }

    #[test]
    fn series_the_dashboard_reads_are_present_and_move() {
        let early = stats(2_950_400, 10);
        let first = snapshot(&input(&early, 0, Scenario::Normal));
        let later = snapshot(&input(&stats(2_951_000, 10), 6, Scenario::Normal));
        for name in [
            "zcash_net_peers",
            "pool_num_ready",
            "pool_num_unready",
            "sync_downloads_in_flight",
            "sync_estimated_distance_to_tip",
            "checkpoint_verified_height",
            "state_memory_chain_count",
            "zebra_state_rocksdb_total_disk_size_bytes",
            "zebra_state_rocksdb_total_memory_size_bytes",
            "zebra_state_rocksdb_block_cache_usage_bytes",
            "zcash_mempool_size_bytes",
            "zcash_net_peers_version_obsolete",
        ] {
            assert!(first.gauge(name).is_some(), "{name}");
        }
        for name in [
            "zcash_chain_verified_block_total",
            "zcash_net_in_bytes_total",
            "zcash_net_out_bytes_total",
        ] {
            assert!(first.counter(name).is_some(), "{name}");
            assert!(
                later.counter(name).unwrap() > first.counter(name).unwrap(),
                "{name} grows"
            );
        }
        // A handshake fails now and then, not every ten seconds.
        assert_eq!(
            first.counter("zcash_net_peer_handshake_failures_total"),
            Some(3.0)
        );
        assert_eq!(
            snapshot(&input(&early, 100, Scenario::Normal))
                .counter("zcash_net_peer_handshake_failures_total"),
            Some(5.0)
        );
        assert_eq!(first.gauge("zcash_net_peers"), Some(10.0));
        assert_eq!(first.gauge("pool_num_ready"), Some(9.0));
        assert_eq!(first.gauge("pool_num_unready"), Some(1.0));
        assert_eq!(
            first.counter("zcash_chain_verified_block_total"),
            Some(400.0)
        );
        assert_eq!(
            later.counter("zcash_chain_verified_block_total"),
            Some(1_000.0)
        );
        assert_eq!(
            first.gauge("sync_estimated_distance_to_tip"),
            Some(169_600.0)
        );
        assert!(
            later
                .gauge("zebra_state_rocksdb_total_disk_size_bytes")
                .unwrap()
                > first
                    .gauge("zebra_state_rocksdb_total_disk_size_bytes")
                    .unwrap()
        );
        // The network-labelled gauges still read as single values.
        assert_eq!(
            first.family("zcash_net_peers").unwrap().samples[0].labels,
            vec![("network".to_string(), "main".to_string())]
        );
        assert_eq!(first.at, UNIX_EPOCH);
        assert_eq!(later.at, UNIX_EPOCH + Duration::from_secs(60));
    }

    #[test]
    fn scenarios_shape_the_peer_and_sync_gauges() {
        let none = snapshot(&input(&stats(2_950_400, 0), 2, Scenario::NoPeers));
        assert_eq!(none.gauge("zcash_net_peers"), Some(0.0));
        assert_eq!(none.gauge("pool_num_ready"), Some(0.0));
        assert_eq!(none.gauge("pool_num_unready"), Some(0.0));
        assert_eq!(none.gauge("sync_downloads_in_flight"), Some(0.0));
        assert_eq!(none.gauge("zcash_net_peers_version_obsolete"), Some(0.0));
        let stalled = snapshot(&input(&stats(2_950_400, 10), 2, Scenario::Stalled));
        assert_eq!(stalled.gauge("sync_downloads_in_flight"), Some(2.0));
        let tip = snapshot(&input(&stats(3_120_000, 10), 2, Scenario::Normal));
        assert_eq!(tip.gauge("sync_estimated_distance_to_tip"), Some(0.0));
        assert!(tip.gauge("sync_downloads_in_flight").unwrap() < 3.0);
    }
}
