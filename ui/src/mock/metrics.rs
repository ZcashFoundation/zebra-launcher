//! The mock's metrics page: a snapshot every 10 s with plausible, moving values for the series
//! the dashboard reads. Names are the wire names (underscores).

use std::time::SystemTime;

use launcher_types::{MetricFamily, MetricKind, MetricSample, MetricsSnapshot, NodeStats};
use leptos::prelude::*;

use super::mock;

fn gauge(name: &str, value: f64) -> MetricFamily {
    family(name, MetricKind::Gauge, value)
}

fn counter(name: &str, value: f64) -> MetricFamily {
    family(name, MetricKind::Counter, value)
}

fn family(name: &str, kind: MetricKind, value: f64) -> MetricFamily {
    MetricFamily {
        name: name.to_owned(),
        kind,
        help: None,
        samples: vec![MetricSample {
            name: name.to_owned(),
            labels: Vec::new(),
            value,
        }],
    }
}

/// A snapshot derived from the mock's node statistics at `tick` (one per 10 s).
pub(super) fn snapshot(stats: &NodeStats, tick: u64, at: SystemTime) -> MetricsSnapshot {
    let tick = tick as f64;
    MetricsSnapshot {
        at,
        families: vec![
            gauge("zcash_net_peers", f64::from(stats.peers)),
            gauge("pool_num_ready", f64::from(stats.peers.saturating_sub(1))),
            gauge("pool_num_unready", 1.0),
            gauge("sync_downloads_in_flight", 40.0 + (tick % 7.0) * 3.0),
            gauge("checkpoint_verified_height", stats.height as f64 - 12_000.0),
            gauge("state_finalized_block_height", stats.height as f64 - 100.0),
            gauge("state_memory_chain_count", 1.0),
            gauge(
                "zebra_state_rocksdb_total_disk_size_bytes",
                248_300_000_000.0 + tick * 2_500_000.0,
            ),
            gauge(
                "zebra_state_rocksdb_total_memory_size_bytes",
                1_200_000_000.0,
            ),
            gauge(
                "zcash_mempool_size_transactions",
                stats.mempool_transactions as f64,
            ),
            gauge(
                "zcash_mempool_size_bytes",
                stats.mempool_transactions as f64 * 1_850.0,
            ),
            counter("zcash_chain_verified_block_total", stats.height as f64),
            counter("sync_downloaded_block_count", stats.height as f64 + 60.0),
            counter("zcash_net_in_bytes_total", tick * 12_000_000.0),
            counter("zcash_net_out_bytes_total", tick * 3_000_000.0),
            counter("zcash_net_peer_handshake_failures_total", 3.0),
            gauge("zcash_net_peers_version_obsolete", 1.0),
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
