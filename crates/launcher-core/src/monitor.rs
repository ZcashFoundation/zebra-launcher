//! Polls zebrad's RPC interface while it runs and publishes [`NodeStats`].
//!
//! The monitor follows the supervisor's `watch<Status>`: while the phase is
//! [`Phase::Running`] and an RPC address is known, it polls every interval;
//! otherwise it publishes `None` and waits for the next status change. It
//! stops when the status sender or every stats receiver is dropped.

use std::{
    net::SocketAddr,
    path::PathBuf,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use launcher_types::{EosInfo, NodeStats, Phase, Status};
use tokio::sync::watch;

use crate::rpc::{RpcClient, RpcError};

/// The default polling interval.
pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(5);
/// The JSON-RPC error code for an unknown method.
const METHOD_NOT_FOUND: i64 = -32601;

/// The default interval between `getdeprecationinfo` calls.
pub const DEFAULT_EOS_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// How the monitor polls.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonitorConfig {
    /// zebrad's RPC cookie file.
    pub cookie_file: PathBuf,
    /// Time between polls.
    pub interval: Duration,
    /// Time between end-of-support checks, which only run on Mainnet.
    pub eos_interval: Duration,
}

impl MonitorConfig {
    /// Polls every `interval` and checks end of support hourly.
    pub fn new(cookie_file: PathBuf, interval: Duration) -> Self {
        Self {
            cookie_file,
            interval,
            eos_interval: DEFAULT_EOS_INTERVAL,
        }
    }
}

/// Starts the polling task.
pub struct Monitor;

impl Monitor {
    /// Polls every `interval` while zebrad runs, with hourly end-of-support
    /// checks. Must be called inside a Tokio runtime.
    pub fn spawn(
        status: watch::Receiver<Status>,
        cookie_file: PathBuf,
        interval: Duration,
    ) -> watch::Receiver<Option<NodeStats>> {
        Self::spawn_with(status, MonitorConfig::new(cookie_file, interval))
    }

    /// Like [`Monitor::spawn`], with every parameter configurable.
    pub fn spawn_with(
        status: watch::Receiver<Status>,
        config: MonitorConfig,
    ) -> watch::Receiver<Option<NodeStats>> {
        let (tx, rx) = watch::channel(None);
        tokio::spawn(run(status, config, tx, RpcClient::new));
        rx
    }
}

/// The state of one zebrad run.
struct Run {
    addr: SocketAddr,
    client: RpcClient,
    eos: Option<EosInfo>,
    eos_checked: Option<Instant>,
}

/// The monitor task. `new_client` builds the client for each run; it is a
/// parameter so tests can make it fail.
async fn run(
    mut status: watch::Receiver<Status>,
    config: MonitorConfig,
    tx: watch::Sender<Option<NodeStats>>,
    new_client: impl Fn(SocketAddr, PathBuf) -> Result<RpcClient, RpcError>,
) {
    let mut current: Option<Run> = None;
    loop {
        let rpc_addr = {
            let status = status.borrow_and_update();
            match status.phase {
                Phase::Running => status.rpc_addr,
                _ => None,
            }
        };
        let Some(addr) = rpc_addr else {
            current = None;
            tx.send_if_modified(|stats| stats.take().is_some());
            tokio::select! {
                changed = status.changed() => if changed.is_err() { return },
                () = tx.closed() => return,
            }
            continue;
        };

        // Keep the run for the same address; a new address is a new run.
        let run = match current.take() {
            Some(run) if run.addr == addr => current.insert(run),
            _ => {
                let client = match new_client(addr, config.cookie_file.clone()) {
                    Ok(client) => client,
                    Err(err) => {
                        // Keep the task (and the stats watch) alive; try again
                        // after the interval.
                        tracing::warn!(%err, "could not create the RPC client");
                        if !wait_for_next_poll(&mut status, &tx, config.interval).await {
                            return;
                        }
                        continue;
                    }
                };
                current.insert(Run {
                    addr,
                    client,
                    eos: None,
                    eos_checked: None,
                })
            }
        };

        let result = poll(run, config.eos_interval).await;
        // A status change during the poll makes the result stale: re-evaluate.
        if !status.has_changed().unwrap_or(true) {
            match result {
                Ok(stats) => {
                    tx.send_replace(Some(stats));
                }
                Err(err) => {
                    tracing::debug!(%err, "polling zebrad failed");
                    tx.send_if_modified(|stats| match stats {
                        Some(stats) => {
                            stats.last_error = Some(err.to_string());
                            true
                        }
                        None => false,
                    });
                }
            }
        }

        if !wait_for_next_poll(&mut status, &tx, config.interval).await {
            return;
        }
    }
}

/// Waits for the interval or a status change. False when the monitor should
/// stop: the status sender or every stats receiver is gone.
async fn wait_for_next_poll(
    status: &mut watch::Receiver<Status>,
    tx: &watch::Sender<Option<NodeStats>>,
    interval: Duration,
) -> bool {
    tokio::select! {
        () = tokio::time::sleep(interval) => true,
        changed = status.changed() => changed.is_ok(),
        () = tx.closed() => false,
    }
}

/// Polls zebrad once.
async fn poll(run: &mut Run, eos_interval: Duration) -> Result<NodeStats, RpcError> {
    // One cookie read for the whole poll.
    let client = run.client.session().await?;
    let (chain, info, mempool, peers) = tokio::join!(
        client.get_blockchain_info(),
        client.get_info(),
        client.get_mempool_info(),
        client.get_peer_info(),
    );
    let (chain, info, mempool, peers) = (chain?, info?, mempool?, peers?);

    // End of support is only enforced (and reported) on Mainnet.
    let eos_due = run
        .eos_checked
        .is_none_or(|checked| checked.elapsed() >= eos_interval);
    if chain.chain == "main" && eos_due {
        match client.get_deprecation_info().await {
            Ok(info) => {
                run.eos_checked = Some(Instant::now());
                run.eos = info.end_of_service.map(|eos| EosInfo {
                    block_height: eos.block_height,
                    estimated_time: unix_time(eos.estimated_time),
                });
            }
            // An older zebrad without the method: no countdown, and no point
            // asking again before the next interval.
            Err(
                err @ RpcError::Rpc {
                    code: METHOD_NOT_FOUND,
                    ..
                },
            ) => {
                run.eos_checked = Some(Instant::now());
                tracing::debug!(%err, "zebrad has no getdeprecationinfo");
            }
            // Anything else may be transient: retry on the next poll.
            Err(err) => tracing::debug!(%err, "getdeprecationinfo failed"),
        }
    }

    Ok(NodeStats {
        height: chain.blocks,
        estimated_height: chain.estimated_height,
        verification_progress: chain.verification_progress,
        peers: u32::try_from(peers).unwrap_or(u32::MAX),
        mempool_transactions: mempool.size,
        chain: chain.chain,
        zebrad_version: info.release(),
        last_error: info.last_error().map(str::to_string),
        eos: run.eos.clone(),
        updated_at: SystemTime::now(),
    })
}

fn unix_time(secs: i64) -> SystemTime {
    match u64::try_from(secs) {
        Ok(secs) => UNIX_EPOCH + Duration::from_secs(secs),
        Err(_) => UNIX_EPOCH,
    }
}

#[cfg(test)]
mod tests {
    use std::{
        net::Ipv4Addr,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use super::*;

    #[tokio::test]
    async fn a_failing_client_is_retried_and_the_stats_watch_stays_open() {
        let (status_tx, status_rx) = watch::channel(Status::default());
        let (tx, mut stats) = watch::channel(None);
        let attempts = Arc::new(AtomicUsize::new(0));
        let config = MonitorConfig {
            cookie_file: PathBuf::from("/nonexistent/.cookie"),
            interval: Duration::from_millis(10),
            eos_interval: DEFAULT_EOS_INTERVAL,
        };
        let counter = attempts.clone();
        let task = tokio::spawn(run(status_rx, config, tx, move |_, _| {
            counter.fetch_add(1, Ordering::Relaxed);
            Err(RpcError::InvalidResponse("no client".to_string()))
        }));
        status_tx.send_modify(|s| {
            s.phase = Phase::Running;
            s.rpc_addr = Some(SocketAddr::from((Ipv4Addr::LOCALHOST, 1)));
        });

        tokio::time::timeout(Duration::from_secs(10), async {
            while attempts.load(Ordering::Relaxed) < 3 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the client is retried");
        assert!(!task.is_finished());
        assert!(stats.has_changed().is_ok(), "the stats sender is alive");

        drop(status_tx);
        tokio::time::timeout(Duration::from_secs(10), task)
            .await
            .expect("the monitor stops in time")
            .expect("the monitor task did not panic");
        assert!(
            tokio::time::timeout(Duration::from_secs(10), stats.changed())
                .await
                .expect("in time")
                .is_err()
        );
    }
}
