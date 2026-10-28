//! The metrics poller against fake-zebrad's `/metrics` endpoint, which serves
//! the `zebrad-7.0.0-metrics.txt` fixture.

use std::{net::SocketAddr, process::Stdio, time::Duration};

use launcher_core::{
    metrics::poller::{FAILURE_LIMIT, MetricsPoller},
    supervisor,
};
use launcher_types::{MetricsSnapshot, Phase, Status};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
    sync::watch,
};

#[path = "support/helpers.rs"]
mod support;
use support::{
    LIMIT, fake_zebrad, fake_zebrad_command, fast_supervisor_config, within, within_limit,
};

/// A fake-zebrad config that binds its metrics endpoint to a free loopback port.
const METRICS_CONFIG: &str =
    "[network]\nnetwork = \"Regtest\"\n\n[metrics]\nendpoint_addr = \"127.0.0.1:0\"\n";

/// The brief's bound for the first snapshot after `Running`.
const FIRST_SNAPSHOT_LIMIT: Duration = Duration::from_secs(15);

async fn wait_for(
    metrics: &mut watch::Receiver<Option<MetricsSnapshot>>,
    limit: Duration,
    what: &str,
    predicate: impl FnMut(&Option<MetricsSnapshot>) -> bool,
) -> Option<MetricsSnapshot> {
    within_limit(limit, what, metrics.wait_for(predicate))
        .await
        .expect("the poller is running")
        .clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_snapshot_appears_while_running_and_clears_on_stop() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("zebrad.toml");
    std::fs::write(&config_path, METRICS_CONFIG).expect("write config");
    let config = fast_supervisor_config(fake_zebrad_command("ok", &config_path));
    let handle = supervisor::spawn(config).expect("spawn supervisor");
    let mut metrics = MetricsPoller::spawn_with(handle.watch_status(), Duration::from_millis(200));
    assert_eq!(*metrics.borrow(), None);

    within(handle.start()).await.expect("start");
    let snapshot = wait_for(
        &mut metrics,
        FIRST_SNAPSHOT_LIMIT,
        "the first metrics snapshot",
        Option::is_some,
    )
    .await
    .expect("some");
    let addr = handle.status().metrics_addr.expect("metrics address");
    assert_ne!(addr.port(), 0, "the fake printed its bound port");
    assert_eq!(snapshot.families.len(), 57);
    assert_eq!(snapshot.gauge("zcash_net_peers"), Some(0.0));
    assert_eq!(
        snapshot.counter("zcash_chain_verified_block_total"),
        Some(1.0)
    );
    assert!(snapshot.at <= std::time::SystemTime::now());

    // The next poll is a fresh read.
    let seen = snapshot.at;
    wait_for(&mut metrics, LIMIT, "a newer snapshot", |s| {
        s.as_ref().is_some_and(|s| s.at > seen)
    })
    .await;

    // Not running: the snapshot is withdrawn.
    within(handle.stop()).await.expect("stop");
    assert_eq!(handle.status().phase, Phase::Stopped);
    wait_for(&mut metrics, LIMIT, "the cleared snapshot", Option::is_none).await;

    within(handle.shutdown()).await.expect("shutdown");
    // The supervisor is gone, so the poller ends and drops its sender.
    let closed = within(metrics.changed()).await;
    assert!(closed.is_err(), "the poller ended");
}

/// fake-zebrad started by hand, with the metrics address it printed.
struct Fake {
    child: Child,
    metrics_addr: SocketAddr,
    _dir: tempfile::TempDir,
}

async fn spawn_fake() -> Fake {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("zebrad.toml");
    std::fs::write(&config, METRICS_CONFIG).expect("write config");
    let mut child = Command::new(fake_zebrad())
        .arg("-c")
        .arg(&config)
        .arg("start")
        .env("FAKE_ZEBRAD_HEARTBEAT_MS", "60000")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("fake-zebrad spawns");
    let mut lines = BufReader::new(child.stdout.take().expect("piped stdout")).lines();
    let metrics_addr = within(async {
        loop {
            let line = lines
                .next_line()
                .await
                .expect("stdout is readable")
                .expect("fake-zebrad printed the metrics line before exiting");
            if let Some((_, addr)) = line.split_once("Opened metrics endpoint at ") {
                return addr.trim().parse::<SocketAddr>().expect("a socket address");
            }
        }
    })
    .await;
    // Keep draining stdout so the fake never blocks on a full pipe.
    tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
    assert_ne!(metrics_addr.port(), 0, "the bound port is printed");
    Fake {
        child,
        metrics_addr,
        _dir: dir,
    }
}

#[tokio::test]
async fn consecutive_failed_reads_withdraw_the_snapshot() {
    let mut fake = spawn_fake().await;
    let (status_tx, status_rx) = watch::channel(Status {
        phase: Phase::Running,
        metrics_addr: Some(fake.metrics_addr),
        ..Status::default()
    });
    let interval = Duration::from_millis(100);
    let mut metrics = MetricsPoller::spawn_with(status_rx, interval);
    let snapshot = wait_for(&mut metrics, LIMIT, "the first snapshot", Option::is_some)
        .await
        .expect("some");
    assert_eq!(snapshot.gauge("zcash_net_peers"), Some(0.0));

    // The endpoint goes away while the status still says Running: the
    // snapshot is kept through the first failures, then withdrawn.
    fake.child.kill().await.expect("kill fake-zebrad");
    let started = std::time::Instant::now();
    wait_for(
        &mut metrics,
        LIMIT,
        "the withdrawn snapshot",
        Option::is_none,
    )
    .await;
    assert!(
        started.elapsed() >= interval * (FAILURE_LIMIT - 1),
        "withdrawn after {:?}, before {FAILURE_LIMIT} reads could fail",
        started.elapsed()
    );
    drop(status_tx);
}
