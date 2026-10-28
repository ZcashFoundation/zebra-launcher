//! Polls zebrad's `/metrics` page while it runs and publishes
//! [`MetricsSnapshot`]s.
//!
//! The poller follows the supervisor's `watch<Status>` like the RPC monitor
//! ([`crate::monitor`]): while the phase is [`Phase::Running`] and a metrics
//! address is known, it reads the page every interval and publishes the parsed
//! snapshot; otherwise it publishes `None`. [`FAILURE_LIMIT`] consecutive
//! failed reads also publish `None`, with one warning in the launcher log,
//! until a read succeeds again. The task stops when the status sender or every
//! snapshot receiver is dropped.

use std::{
    net::SocketAddr,
    time::{Duration, SystemTime},
};

use launcher_types::{MetricsSnapshot, Phase, Status};
use tokio::sync::watch;

use super::{ParseError, parse};

/// The default polling interval.
pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(10);
/// How long one read may take. zebrad answers from memory, on loopback.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// Consecutive failed reads after which the snapshot is withdrawn.
pub const FAILURE_LIMIT: u32 = 3;

/// A read of the metrics page failed.
#[derive(Debug, thiserror::Error)]
pub enum MetricsError {
    /// The HTTP request failed: connection refused, timeout, and so on.
    #[error("metrics request failed: {0}")]
    Http(#[from] reqwest::Error),
    /// zebrad answered with an HTTP error status.
    #[error("metrics request failed with HTTP status {0}")]
    Status(u16),
    /// The page is not in the Prometheus text format.
    #[error(transparent)]
    Parse(#[from] ParseError),
}

/// Starts the polling task.
pub struct MetricsPoller;

impl MetricsPoller {
    /// Polls every [`DEFAULT_INTERVAL`] while zebrad runs. Must be called
    /// inside a Tokio runtime.
    pub fn spawn(status: watch::Receiver<Status>) -> watch::Receiver<Option<MetricsSnapshot>> {
        Self::spawn_with(status, DEFAULT_INTERVAL)
    }

    /// Like [`MetricsPoller::spawn`], polling every `interval`.
    pub fn spawn_with(
        status: watch::Receiver<Status>,
        interval: Duration,
    ) -> watch::Receiver<Option<MetricsSnapshot>> {
        let (tx, rx) = watch::channel(None);
        tokio::spawn(run(status, interval, tx));
        rx
    }
}

/// A client for zebrad's loopback endpoint: short timeout, no proxy.
pub fn client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        // zebrad is always on loopback; never route through a system proxy.
        .no_proxy()
        .build()
}

/// Reads and parses `http://<addr>/metrics` once.
pub async fn fetch(
    client: &reqwest::Client,
    addr: SocketAddr,
) -> Result<MetricsSnapshot, MetricsError> {
    let response = client.get(format!("http://{addr}/metrics")).send().await?;
    let status = response.status();
    if !status.is_success() {
        return Err(MetricsError::Status(status.as_u16()));
    }
    let text = response.text().await?;
    Ok(parse(&text, SystemTime::now())?)
}

/// The polling task.
async fn run(
    mut status: watch::Receiver<Status>,
    interval: Duration,
    tx: watch::Sender<Option<MetricsSnapshot>>,
) {
    let client = match client() {
        Ok(client) => client,
        Err(err) => {
            tracing::error!(%err, "could not create the metrics HTTP client; metrics stay off");
            return;
        }
    };
    let mut failures = 0u32;
    loop {
        let metrics_addr = {
            let status = status.borrow_and_update();
            match status.phase {
                Phase::Running => status.metrics_addr,
                _ => None,
            }
        };
        let Some(addr) = metrics_addr else {
            failures = 0;
            tx.send_if_modified(|snapshot| snapshot.take().is_some());
            tokio::select! {
                changed = status.changed() => if changed.is_err() { return },
                () = tx.closed() => return,
            }
            continue;
        };

        let result = fetch(&client, addr).await;
        // A status change during the read makes the result stale: re-evaluate.
        if !status.has_changed().unwrap_or(true) {
            match result {
                Ok(snapshot) => {
                    failures = 0;
                    tx.send_replace(Some(snapshot));
                }
                Err(err) => {
                    failures = failures.saturating_add(1);
                    if failures == FAILURE_LIMIT {
                        tracing::warn!(
                            %err,
                            %addr,
                            "reading zebrad's metrics failed {FAILURE_LIMIT} times in a row; the metrics are withdrawn until a read succeeds"
                        );
                        tx.send_if_modified(|snapshot| snapshot.take().is_some());
                    } else {
                        tracing::debug!(%err, failures, "reading zebrad's metrics failed");
                    }
                }
            }
        }

        tokio::select! {
            () = tokio::time::sleep(interval) => {}
            changed = status.changed() => if changed.is_err() { return },
            () = tx.closed() => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, TcpListener};

    use super::*;

    /// A loopback address nothing listens on.
    fn closed_port() -> SocketAddr {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
        listener.local_addr().expect("addr")
    }

    #[tokio::test]
    async fn nothing_is_polled_until_running_with_an_address() {
        let (status_tx, status_rx) = watch::channel(Status::default());
        let mut metrics = MetricsPoller::spawn_with(status_rx, Duration::from_millis(10));
        assert_eq!(*metrics.borrow(), None);

        // Starting with the address known, then Running without it: still nothing.
        status_tx.send_modify(|s| {
            s.phase = Phase::Starting;
            s.metrics_addr = Some(closed_port());
        });
        status_tx.send_modify(|s| {
            s.phase = Phase::Running;
            s.metrics_addr = None;
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!metrics.has_changed().expect("the poller is alive"));
        assert_eq!(*metrics.borrow(), None);

        // Dropping the status sender ends the task.
        drop(status_tx);
        assert!(
            tokio::time::timeout(Duration::from_secs(10), metrics.changed())
                .await
                .expect("in time")
                .is_err(),
            "the poller ended"
        );
    }

    #[tokio::test]
    async fn failed_reads_leave_the_snapshot_empty_and_the_task_alive() {
        let (status_tx, status_rx) = watch::channel(Status {
            phase: Phase::Running,
            metrics_addr: Some(closed_port()),
            ..Status::default()
        });
        let metrics = MetricsPoller::spawn_with(status_rx, Duration::from_millis(10));
        // Well past FAILURE_LIMIT reads.
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(!metrics.has_changed().expect("the poller is alive"));
        assert_eq!(*metrics.borrow(), None);
        drop(status_tx);
    }

    #[tokio::test]
    async fn fetch_rejects_a_non_metrics_answer() {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            for body in ["", "<html>not metrics</html>"] {
                let (mut stream, _) = listener.accept().await.expect("accept");
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf).await;
                let status = if body.is_empty() {
                    "404 Not Found"
                } else {
                    "200 OK"
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.expect("write");
                stream.shutdown().await.expect("shutdown");
            }
        });
        let client = client().expect("client");
        let err = fetch(&client, addr).await.expect_err("404");
        assert!(matches!(err, MetricsError::Status(404)), "{err}");
        let err = fetch(&client, addr).await.expect_err("html");
        assert!(matches!(err, MetricsError::Parse(_)), "{err}");
        assert!(err.to_string().starts_with("metrics line 1: "), "{err}");
    }
}
