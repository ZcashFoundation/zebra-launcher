//! The RPC client and the monitor against fake-zebrad's JSON-RPC server.

use std::{
    net::SocketAddr,
    path::PathBuf,
    process::Stdio,
    time::{Duration, UNIX_EPOCH},
};

use launcher_core::{
    monitor::{Monitor, MonitorConfig, MonitorOutput},
    rpc::{RpcClient, RpcError},
};
use launcher_types::{NodeStats, Phase, Status};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
    sync::watch,
    time::timeout,
};

const T: Duration = Duration::from_secs(20);

/// A running fake-zebrad with its RPC server.
struct Fake {
    child: Child,
    addr: SocketAddr,
    cookie_file: PathBuf,
    _dir: tempfile::TempDir,
}

async fn spawn_fake() -> Fake {
    spawn_fake_with(&[]).await
}

/// Like [`spawn_fake`], with extra environment variables for the fake.
async fn spawn_fake_with(env: &[(&str, &str)]) -> Fake {
    let dir = tempfile::tempdir().expect("tempdir");
    let cookie_dir = dir.path().join("rpc");
    let config = dir.path().join("zebrad.toml");
    let cookie_dir_toml = toml_edit::Value::from(cookie_dir.to_str().expect("utf-8 tempdir"));
    std::fs::write(
        &config,
        format!("[rpc]\nlisten_addr = \"127.0.0.1:0\"\ncookie_dir = {cookie_dir_toml}\n"),
    )
    .expect("write config");

    let mut child = Command::new(env!("CARGO_BIN_EXE_fake-zebrad"))
        .arg("-c")
        .arg(&config)
        .arg("start")
        .env("FAKE_ZEBRAD_HEARTBEAT_MS", "60000")
        .envs(env.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("fake-zebrad spawns");
    let mut lines = BufReader::new(child.stdout.take().expect("piped stdout")).lines();
    let addr = timeout(T, async {
        loop {
            let line = lines
                .next_line()
                .await
                .expect("stdout is readable")
                .expect("fake-zebrad printed the RPC line before exiting");
            if let Some((_, addr)) = line.split_once("Opened RPC endpoint at ") {
                return addr.trim().parse::<SocketAddr>().expect("a socket address");
            }
        }
    })
    .await
    .expect("RPC endpoint opened in time");
    // Keep draining stdout so the fake never blocks on a full pipe.
    tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });

    assert_ne!(addr.port(), 0, "the bound port is printed");
    Fake {
        child,
        addr,
        cookie_file: cookie_dir.join(".cookie"),
        _dir: dir,
    }
}

#[tokio::test]
async fn rpc_client_calls_every_method() {
    let fake = spawn_fake().await;
    let cookie = std::fs::read_to_string(&fake.cookie_file).expect("cookie written");
    assert!(cookie.starts_with("__cookie__:"), "{cookie}");

    let client = RpcClient::new(fake.addr, fake.cookie_file.clone()).expect("client");
    let chain = timeout(T, client.get_blockchain_info())
        .await
        .expect("in time")
        .expect("getblockchaininfo");
    assert_eq!(chain.blocks, 2_500_000);
    assert_eq!(chain.estimated_height, 3_000_000);
    assert_eq!(chain.verification_progress, 0.8333);
    assert_eq!(chain.chain, "main");
    assert_eq!(chain.headers, Some(2_500_001));
    assert_eq!(chain.difficulty, Some(123_456_789.5));
    assert_eq!(chain.size_on_disk, Some(321_000_000_000));
    assert_eq!(chain.best_block_hash.as_deref().map(str::len), Some(64));
    assert_eq!(
        chain.chain_supply.and_then(|pool| pool.chain_value_zat),
        Some(1_593_750_000_000_000)
    );
    assert_eq!(chain.value_pools.len(), 6);
    assert_eq!(chain.upgrades.len(), 5);

    let info = timeout(T, client.get_info())
        .await
        .expect("in time")
        .expect("getinfo");
    assert_eq!(info.version, 7_000_050);
    assert_eq!(info.subversion, "/Zebra:7.0.0/");
    assert_eq!(info.protocol_version, Some(170_140));
    assert_eq!(info.connections, 8);
    assert_eq!(info.release(), "7.0.0");
    assert_eq!(info.last_error(), None);

    let mempool = timeout(T, client.get_mempool_info())
        .await
        .expect("in time")
        .expect("getmempoolinfo");
    assert_eq!(mempool.size, 42);
    assert_eq!(mempool.bytes, Some(32_500));
    assert_eq!(mempool.usage, Some(41_000));

    let peers = timeout(T, client.get_peer_info())
        .await
        .expect("in time")
        .expect("getpeerinfo");
    assert_eq!(peers.len(), 8);
    assert_eq!(peers[0].addr, "192.0.2.0:8233");
    assert!(peers[0].inbound);
    assert!(!peers[1].inbound);
    assert_eq!(peers[0].subver, "/Zebra:7.0.0/");
    assert_eq!(peers[1].subver, "/MagicBean:6.2.0/");
    assert_eq!(peers[0].version, 170_140);
    assert_eq!(peers[0].connection_state, "connected");
    assert_eq!(peers[0].lastrecv, 1_800_000_000);
    assert_eq!(peers[0].pingtime, Some(0.012));
    assert_eq!(peers[7].pingtime, None);

    let eos = timeout(T, client.get_deprecation_info())
        .await
        .expect("in time")
        .expect("getdeprecationinfo")
        .end_of_service
        .expect("Mainnet reports end of service");
    assert_eq!(eos.block_height, 3_706_000);
    assert_eq!(eos.estimated_time, 1_800_000_000);

    let unknown = timeout(T, client.call::<Value>("nosuchmethod", json!([])))
        .await
        .expect("in time")
        .expect_err("unknown method");
    assert!(
        matches!(unknown, RpcError::Rpc { code: -32601, .. }),
        "{unknown:?}"
    );
}

#[tokio::test]
async fn rpc_client_requires_the_cookie() {
    let fake = spawn_fake().await;
    let dir = tempfile::tempdir().expect("tempdir");

    let wrong_cookie = dir.path().join(".cookie");
    std::fs::write(&wrong_cookie, "__cookie__:not-the-cookie").expect("write");
    let client = RpcClient::new(fake.addr, wrong_cookie).expect("client");
    let err = timeout(T, client.get_info())
        .await
        .expect("in time")
        .expect_err("wrong cookie");
    assert!(matches!(err, RpcError::Status(401)), "{err:?}");

    let client = RpcClient::new(fake.addr, dir.path().join("missing")).expect("client");
    let err = timeout(T, client.get_info())
        .await
        .expect("in time")
        .expect_err("missing cookie");
    assert!(matches!(err, RpcError::Cookie { .. }), "{err:?}");
}

#[tokio::test]
async fn a_session_reads_the_cookie_once() {
    let fake = spawn_fake().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let cookie_file = dir.path().join(".cookie");
    std::fs::copy(&fake.cookie_file, &cookie_file).expect("copy cookie");
    let client = RpcClient::new(fake.addr, cookie_file.clone()).expect("client");

    let session = timeout(T, client.session())
        .await
        .expect("in time")
        .expect("session");
    // Later calls in the session do not read the file again.
    std::fs::remove_file(&cookie_file).expect("remove cookie");
    let (chain, info, mempool, peers) = timeout(T, async {
        tokio::join!(
            session.get_blockchain_info(),
            session.get_info(),
            session.get_mempool_info(),
            session.get_peer_info(),
        )
    })
    .await
    .expect("in time");
    assert_eq!(chain.expect("getblockchaininfo").blocks, 2_500_000);
    assert_eq!(info.expect("getinfo").release(), "7.0.0");
    assert_eq!(mempool.expect("getmempoolinfo").size, 42);
    assert_eq!(peers.expect("getpeerinfo").len(), 8);

    // The client itself reads the file per call.
    let err = timeout(T, client.get_info())
        .await
        .expect("in time")
        .expect_err("cookie gone");
    assert!(matches!(err, RpcError::Cookie { .. }), "{err:?}");
}

async fn wait_for<T: Clone>(
    output: &mut watch::Receiver<Option<T>>,
    what: &str,
    predicate: impl FnMut(&Option<T>) -> bool,
) -> Option<T> {
    timeout(T, output.wait_for(predicate))
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
        .expect("the monitor is running")
        .clone()
}

#[tokio::test]
async fn monitor_follows_the_supervisor_status() {
    let mut fake = spawn_fake().await;
    let (status_tx, status_rx) = watch::channel(Status::default());
    let MonitorOutput {
        mut stats,
        mut details,
    } = Monitor::spawn_with(
        status_rx,
        MonitorConfig {
            cookie_file: fake.cookie_file.clone(),
            interval: Duration::from_millis(100),
            eos_interval: Duration::from_secs(3600),
        },
    );
    assert_eq!(*stats.borrow(), None);
    assert_eq!(*details.borrow(), None);

    // Starting is not Running: nothing is polled yet.
    status_tx.send_modify(|s| {
        s.phase = Phase::Starting;
        s.rpc_addr = Some(fake.addr);
    });
    status_tx.send_modify(|s| s.phase = Phase::Running);
    let first = wait_for(&mut stats, "stats", Option::is_some)
        .await
        .expect("some");
    assert_eq!(first.height, 2_500_000);
    assert_eq!(first.estimated_height, 3_000_000);
    assert_eq!(first.verification_progress, 0.8333);
    assert_eq!(first.peers, 8);
    assert_eq!(first.mempool_transactions, 42);
    assert_eq!(first.chain, "main");
    assert_eq!(first.zebrad_version, "7.0.0");
    assert_eq!(first.last_error, None);
    let eos = first.eos.clone().expect("end of service on Mainnet");
    assert_eq!(eos.block_height, 3_706_000);
    assert_eq!(
        eos.estimated_time,
        UNIX_EPOCH + Duration::from_secs(1_800_000_000)
    );

    // The details come from the same poll, in the wire's shapes.
    let detail = wait_for(&mut details, "details", Option::is_some)
        .await
        .expect("some");
    assert!(detail.updated_at >= first.updated_at);
    assert_eq!(detail.peers.len(), 8);
    let peer = &detail.peers[0];
    assert_eq!(peer.addr, "192.0.2.0:8233");
    assert!(peer.inbound);
    assert_eq!(peer.user_agent.as_deref(), Some("/Zebra:7.0.0/"));
    assert_eq!(peer.protocol_version, Some(170_140));
    assert_eq!(peer.state.as_deref(), Some("connected"));
    assert_eq!(
        peer.last_seen,
        Some(UNIX_EPOCH + Duration::from_secs(1_800_000_000))
    );
    assert_eq!(peer.ping_secs, Some(0.012));
    assert!(!detail.peers[1].inbound);
    assert_eq!(
        detail.peers[1].user_agent.as_deref(),
        Some("/MagicBean:6.2.0/")
    );
    assert_eq!(detail.peers[7].ping_secs, None);
    assert_eq!(
        detail.peers[7].last_seen,
        Some(UNIX_EPOCH + Duration::from_secs(1_799_999_930))
    );
    assert_eq!(detail.best_block_hash.as_deref().map(str::len), Some(64));
    assert_eq!(detail.headers, Some(2_500_001));
    assert_eq!(detail.difficulty, Some(123_456_789.5));
    assert_eq!(detail.size_on_disk, Some(321_000_000_000));
    assert_eq!(detail.chain_supply_zec, Some(15_937_500.0));
    assert_eq!(detail.value_pools.len(), 6);
    assert_eq!(detail.value_pools[0].id, "transparent");
    assert_eq!(detail.value_pools[0].zec, 10_000_000.0);
    assert_eq!(detail.value_pools[4].id, "lockbox");
    assert_eq!(detail.value_pools[4].zec, 437_500.0);
    let pools: f64 = detail.value_pools.iter().map(|pool| pool.zec).sum();
    assert_eq!(
        Some(pools),
        detail.chain_supply_zec,
        "the pools add up to the supply"
    );
    assert_eq!(detail.active_upgrade.as_deref(), Some("NU6"));
    assert_eq!(detail.subversion.as_deref(), Some("/Zebra:7.0.0/"));
    assert_eq!(detail.protocol_version, Some(170_140));
    assert_eq!(detail.mempool_bytes, Some(32_500));
    assert_eq!(detail.mempool_usage, Some(41_000));

    // Several more polls, but only one end-of-support check per hour.
    let mut seen = first.updated_at;
    for _ in 0..3 {
        let next = wait_for(&mut stats, "a newer poll", |s| {
            s.as_ref().is_some_and(|s| s.updated_at > seen)
        })
        .await
        .expect("some");
        seen = next.updated_at;
    }
    let client = RpcClient::new(fake.addr, fake.cookie_file.clone()).expect("client");
    let counts: Value = timeout(T, client.call("fakegetcallcounts", json!([])))
        .await
        .expect("in time")
        .expect("call counts");
    assert_eq!(counts["getdeprecationinfo"], 1, "{counts}");
    assert!(counts["getinfo"].as_u64().expect("count") >= 4, "{counts}");

    // RPC errors keep the last stats and record the error.
    fake.child.kill().await.expect("kill fake-zebrad");
    let failed = wait_for(&mut stats, "an RPC error", |s| {
        s.as_ref().is_some_and(|s| s.last_error.is_some())
    })
    .await
    .expect("stats are kept");
    assert_eq!(failed.height, 2_500_000);

    // Not running: both outputs are cleared.
    status_tx.send_modify(|s| {
        s.phase = Phase::Stopped;
        s.rpc_addr = None;
    });
    wait_for(&mut stats, "cleared stats", Option::is_none).await;
    wait_for(&mut details, "cleared details", Option::is_none).await;

    // Dropping the status sender stops the monitor, which drops its sender.
    drop(status_tx);
    let closed = timeout(T, stats.changed()).await.expect("in time");
    assert!(closed.is_err(), "the monitor task ended");
}

/// How often the fake answered `getdeprecationinfo`.
async fn deprecation_calls(fake: &Fake) -> u64 {
    let client = RpcClient::new(fake.addr, fake.cookie_file.clone()).expect("client");
    let counts: Value = timeout(T, client.call("fakegetcallcounts", json!([])))
        .await
        .expect("in time")
        .expect("call counts");
    counts["getdeprecationinfo"].as_u64().expect("count")
}

/// Starts a monitor that polls `fake` every 100 ms with hourly EOS checks.
fn monitor_for(fake: &Fake) -> (watch::Sender<Status>, watch::Receiver<Option<NodeStats>>) {
    let (status_tx, status_rx) = watch::channel(Status::default());
    let MonitorOutput { stats, .. } = Monitor::spawn_with(
        status_rx,
        MonitorConfig {
            cookie_file: fake.cookie_file.clone(),
            interval: Duration::from_millis(100),
            eos_interval: Duration::from_secs(3600),
        },
    );
    status_tx.send_modify(|s| {
        s.phase = Phase::Running;
        s.rpc_addr = Some(fake.addr);
    });
    (status_tx, stats)
}

#[tokio::test]
async fn a_transient_end_of_support_failure_is_retried_on_the_next_poll() {
    let fake = spawn_fake_with(&[("FAKE_ZEBRAD_DEPRECATION_FAILURES", "2")]).await;
    let (_status_tx, mut stats) = monitor_for(&fake);
    let stats = wait_for(&mut stats, "the end-of-support countdown", |s| {
        s.as_ref().is_some_and(|s| s.eos.is_some())
    })
    .await
    .expect("some");
    assert_eq!(stats.eos.expect("eos").block_height, 3_706_000);
    assert_eq!(deprecation_calls(&fake).await, 3);
}

#[tokio::test]
async fn a_zebrad_without_getdeprecationinfo_is_asked_once_per_interval() {
    let fake = spawn_fake_with(&[
        ("FAKE_ZEBRAD_DEPRECATION_FAILURES", "1000"),
        ("FAKE_ZEBRAD_DEPRECATION_ERROR_CODE", "-32601"),
    ])
    .await;
    let (_status_tx, mut stats) = monitor_for(&fake);
    let first = wait_for(&mut stats, "stats", Option::is_some)
        .await
        .expect("some");
    let mut seen = first.updated_at;
    for _ in 0..3 {
        let next = wait_for(&mut stats, "a newer poll", |s| {
            s.as_ref().is_some_and(|s| s.updated_at > seen)
        })
        .await
        .expect("some");
        assert_eq!(next.eos, None);
        seen = next.updated_at;
    }
    assert_eq!(deprecation_calls(&fake).await, 1);
}
