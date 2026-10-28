//! Forwards launcher-core's channels to the webview.
//!
//! Status, node statistics and metrics change rarely, so every change is
//! emitted as a Tauri event ([`events::STATUS`], [`events::NODE_STATS`],
//! [`events::NODE_DETAILS`], [`events::METRICS`]). Logs can arrive by the thousand per second, so they
//! go through a `tauri::ipc::Channel` in batches.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use launcher_core::supervisor::LogSubscription;
use launcher_types::{LogLine, MetricsSnapshot, NodeDetails, NodeStats, Status, events};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Runtime, ipc::Channel};
use tokio::{
    sync::{broadcast::error::RecvError, watch},
    time::MissedTickBehavior,
};

/// How long log lines are collected before a batch is sent.
pub const LOG_BATCH_INTERVAL: Duration = Duration::from_millis(100);
/// A batch is sent early once it holds this many lines.
pub const MAX_LOG_BATCH: usize = 1000;

/// Emits [`events::STATUS`] now and on every change, until the supervisor ends.
pub fn forward_status<R: Runtime>(app: AppHandle<R>, rx: watch::Receiver<Status>) {
    tauri::async_runtime::spawn(forward_watch(app, events::STATUS, rx));
}

/// Emits [`events::NODE_STATS`] now and on every change, until the monitor ends.
pub fn forward_node_stats<R: Runtime>(app: AppHandle<R>, rx: watch::Receiver<Option<NodeStats>>) {
    tauri::async_runtime::spawn(forward_watch(app, events::NODE_STATS, rx));
}

/// Emits [`events::NODE_DETAILS`] now and on every change, until the monitor ends.
pub fn forward_node_details<R: Runtime>(
    app: AppHandle<R>,
    rx: watch::Receiver<Option<NodeDetails>>,
) {
    tauri::async_runtime::spawn(forward_watch(app, events::NODE_DETAILS, rx));
}

/// Emits [`events::METRICS`] now and on every change, until the poller ends.
pub fn forward_metrics<R: Runtime>(
    app: AppHandle<R>,
    rx: watch::Receiver<Option<MetricsSnapshot>>,
) {
    tauri::async_runtime::spawn(forward_watch(app, events::METRICS, rx));
}

async fn forward_watch<R: Runtime, T: Clone + Serialize>(
    app: AppHandle<R>,
    event: &'static str,
    mut rx: watch::Receiver<T>,
) {
    loop {
        let value = rx.borrow_and_update().clone();
        if let Err(err) = app.emit(event, value) {
            tracing::warn!(%err, event, "could not emit an event to the UI");
        }
        if rx.changed().await.is_err() {
            return;
        }
    }
}

/// Sends the subscription's history, then live lines in batches, to
/// `channel` until the supervisor ends or `generation` moves past `mine`
/// (a newer subscription replaced this one).
pub fn forward_logs(
    subscription: LogSubscription,
    channel: Channel<Vec<LogLine>>,
    generation: Arc<AtomicU64>,
    mine: u64,
) {
    let current = move || generation.load(Ordering::SeqCst) == mine;
    tauri::async_runtime::spawn(async move {
        let send = |batch: Vec<LogLine>| match channel.send(batch) {
            Ok(()) => true,
            Err(err) => {
                tracing::debug!(%err, "log channel closed");
                false
            }
        };
        pump_logs(subscription, send, current).await;
    });
}

/// The batching loop behind [`forward_logs`], independent of Tauri.
///
/// `send` returns false when the receiver is gone; `active` returns false
/// when this forwarder has been replaced. When the live feed lags, a
/// [`gap_marker`] takes the place of the dropped lines.
pub async fn pump_logs(
    subscription: LogSubscription,
    mut send: impl FnMut(Vec<LogLine>) -> bool,
    active: impl Fn() -> bool,
) {
    let LogSubscription { history, mut live } = subscription;
    for chunk in history.chunks(MAX_LOG_BATCH) {
        if !active() || !send(chunk.iter().map(|line| LogLine::clone(line)).collect()) {
            return;
        }
    }

    let mut batch = Vec::new();
    let mut dropped = 0u64;
    let mut tick = tokio::time::interval(LOG_BATCH_INTERVAL);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            received = live.recv() => match received {
                Ok(line) => {
                    if dropped > 0 {
                        batch.push(gap_marker(dropped, line.seq));
                        dropped = 0;
                    }
                    batch.push(Arc::unwrap_or_clone(line));
                    if batch.len() >= MAX_LOG_BATCH && !send(std::mem::take(&mut batch)) {
                        return;
                    }
                }
                Err(RecvError::Lagged(n)) => dropped += n,
                Err(RecvError::Closed) => {
                    if !batch.is_empty() {
                        send(batch);
                    }
                    return;
                }
            },
            _ = tick.tick() => {
                if !active() {
                    return;
                }
                if !batch.is_empty() && !send(std::mem::take(&mut batch)) {
                    return;
                }
            }
        }
    }
}

/// A launcher line standing in for `dropped` lines the UI fell behind on
/// ([`LogLine::gap_marker`]), placed before the live line with `next_seq`: it
/// takes the sequence number of the last dropped line, `next_seq - 1`.
pub fn gap_marker(dropped: u64, next_seq: u64) -> LogLine {
    LogLine::gap_marker(dropped, next_seq.saturating_sub(1))
}

#[cfg(test)]
mod tests {
    use launcher_types::LogStream;
    use tokio::sync::broadcast;

    use super::*;

    fn line(seq: u64) -> Arc<LogLine> {
        Arc::new(LogLine {
            seq,
            stream: LogStream::Stdout,
            timestamp: None,
            level: None,
            spans: None,
            target: None,
            message: format!("line {seq}"),
            dropped: None,
        })
    }

    #[tokio::test]
    async fn history_then_live_with_a_gap_marker() {
        let (tx, live) = broadcast::channel(2);
        // Five lines into a two-slot channel: the receiver lags by three
        // (2, 3 and 4) and still gets 5 and 6.
        for seq in 2..7 {
            tx.send(line(seq)).expect("send");
        }
        drop(tx);
        let subscription = LogSubscription {
            history: vec![line(0), line(1)],
            live,
        };
        let mut batches = Vec::new();
        tokio::time::timeout(
            Duration::from_secs(5),
            pump_logs(
                subscription,
                |batch| {
                    batches.push(batch);
                    true
                },
                || true,
            ),
        )
        .await
        .expect("the pump ends when the feed closes");

        let lines: Vec<LogLine> = batches.into_iter().flatten().collect();
        let seqs: Vec<u64> = lines.iter().map(|l| l.seq).collect();
        assert_eq!(seqs, vec![0, 1, 4, 5, 6]);
        assert_eq!(lines[2].stream, LogStream::Launcher);
        assert_eq!(lines[2].dropped, Some(3));
        assert_eq!(lines[2].message, "3 lines dropped");
        assert!(lines.iter().filter(|line| line.is_gap_marker()).count() == 1);
    }

    #[test]
    fn gap_marker_shape() {
        let marker = gap_marker(7, 10);
        assert_eq!(marker.seq, 9);
        assert_eq!(marker.dropped, Some(7));
        let json = serde_json::to_value(&marker).expect("serialize");
        assert_eq!(json["stream"], "launcher");
        assert_eq!(json["dropped"], 7);
        // Ordinary lines carry no `dropped` key, so the field is invisible to them.
        let json = serde_json::to_value(&*line(3)).expect("serialize");
        assert!(json.get("dropped").is_none(), "{json}");
    }
}
