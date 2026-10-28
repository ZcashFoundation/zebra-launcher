//! The snapshots of zebrad's metrics page kept since the launcher opened, for the dashboard's
//! derived series (disk growth, network rates, ready peers, downloads in flight). Pure Rust,
//! tested natively.

// Filled in by the dashboard's model workstream; the page reads it once both land.
#![allow(dead_code)]

use std::time::{Duration, SystemTime};

use launcher_types::MetricsSnapshot;

/// Four hours of 10-second snapshots.
pub const MAX_SNAPSHOTS: usize = 1440;

/// The snapshots, oldest first.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MetricsHistory {
    snapshots: Vec<MetricsSnapshot>,
}

impl MetricsHistory {
    /// Records a published snapshot; a repeat of the newest `at` is ignored.
    pub fn push(&mut self, snapshot: &MetricsSnapshot) {
        if self
            .snapshots
            .last()
            .is_some_and(|last| last.at == snapshot.at)
        {
            return;
        }
        self.snapshots.push(snapshot.clone());
        let excess = self.snapshots.len().saturating_sub(MAX_SNAPSHOTS);
        self.snapshots.drain(..excess);
    }

    /// The newest snapshot.
    pub fn latest(&self) -> Option<&MetricsSnapshot> {
        self.snapshots.last()
    }

    /// The snapshots within `span` of `now` (all of them for `None`).
    pub fn window(&self, span: Option<Duration>, now: SystemTime) -> &[MetricsSnapshot] {
        let Some(span) = span else {
            return &self.snapshots;
        };
        let start = self
            .snapshots
            .iter()
            .position(|s| now.duration_since(s.at).is_ok_and(|age| age <= span))
            .unwrap_or(self.snapshots.len());
        &self.snapshots[start..]
    }

    /// Bytes the state database grew by over the window.
    pub fn disk_delta(&self, span: Option<Duration>, now: SystemTime) -> Option<i64> {
        let _ = (span, now);
        None
    }

    /// Network bytes per second in and out over the window, skipping counter resets.
    pub fn net_rate(&self, span: Option<Duration>, now: SystemTime) -> Option<(f64, f64)> {
        let _ = (span, now);
        None
    }

    /// Verified blocks per minute over the window, from zebrad's own counter.
    pub fn verified_block_rate(&self, span: Option<Duration>, now: SystemTime) -> Option<f64> {
        let _ = (span, now);
        None
    }

    /// Peers the peer set reports as ready, from the newest snapshot.
    pub fn ready_peers(&self) -> Option<u64> {
        None
    }

    /// Block downloads in flight, from the newest snapshot.
    pub fn downloads_in_flight(&self) -> Option<u64> {
        None
    }

    /// The checkpoint verifier's height, from the newest snapshot.
    pub fn checkpoint_height(&self) -> Option<u64> {
        None
    }

    /// Chains in memory (more than one means a fork is being resolved).
    pub fn chain_count(&self) -> Option<u64> {
        None
    }
}
