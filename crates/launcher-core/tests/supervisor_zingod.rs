//! The supervisor running zingod through `Launch::Zingod`, against fake-zingod: `Running` at
//! its gRPC listener, its exit classes, and redaction before the ring and the file. zingod
//! runs on Linux only, and a stop sends it `SIGTERM`.

#![cfg(unix)]

use std::{
    net::{Ipv4Addr, SocketAddr, TcpListener},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use launcher_core::{
    logfile::RotatingLogConfig,
    supervisor::{self, Listener, SupervisorConfig, SupervisorError},
    zingod::{ZingodCommand, ZingodConfig, ZingodNetwork},
};
use launcher_types::{ExitClass, LogLine, Phase, PortRole};
use tokio::sync::watch;

#[path = "support/helpers.rs"]
mod support;
use support::{
    LIMIT, contains, dump, fake_zebrad_command, fake_zingod, fast_supervisor_config, wait_status,
    within,
};

#[path = "../src/bin/fake/chain.rs"]
mod chain;

fn free_port() -> u16 {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

struct Fixture {
    _dir: tempfile::TempDir,
    config: SupervisorConfig,
    listen: SocketAddr,
    log: PathBuf,
}

/// fake-zingod in `mode` on Zeal's Testnet configuration, with nothing at the validator's
/// address, a pid file and a log file.
fn fixture(mode: &str) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    fixture_in(dir, mode, free_port())
}

fn fixture_in(dir: tempfile::TempDir, mode: &str, port: u16) -> Fixture {
    let listen = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let zingod = ZingodConfig {
        network: ZingodNetwork::Testnet,
        validator: SocketAddr::from((Ipv4Addr::LOCALHOST, free_port())),
        cookie: dir.path().join("rpc/.cookie"),
        listen,
        index_dir: dir.path().join("zingod/testnet"),
    };
    let path = dir.path().join(format!("zingod-{port}.toml"));
    zingod.write(&path).unwrap();
    zingod.ensure_dirs().unwrap();
    std::fs::write(
        dir.path().join("fake-zingod.env"),
        format!("FAKE_ZINGOD_MODE={mode}\nFAKE_ZINGOD_FAIL_MS=300\nFAKE_ZINGOD_HEARTBEAT_MS=50\n"),
    )
    .unwrap();
    let mut config =
        fast_supervisor_config(ZingodCommand::new(fake_zingod(), &path, zingod.home_dir()));
    config.pidfile = Some(dir.path().join(format!("zingod-{port}.pid")));
    let log = dir.path().join(format!("zingod-{port}.log"));
    config.log_file = Some(RotatingLogConfig::new(&log));
    config.listeners = watch::channel(vec![Listener {
        role: PortRole::Grpc,
        addr: listen,
    }])
    .1;
    Fixture {
        _dir: dir,
        config,
        listen,
        log,
    }
}

async fn history(handle: &supervisor::SupervisorHandle) -> Vec<Arc<LogLine>> {
    within(handle.subscribe()).await.unwrap().history
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn zingod_runs_from_its_grpc_listener_and_stops_on_sigterm() {
    let fx = fixture("serving");
    let handle = supervisor::spawn(fx.config.clone()).unwrap();
    assert!(handle.status().stop_is_graceful);
    within(handle.start()).await.expect("zingod starts");
    let status = handle.status();
    assert_eq!(status.phase, Phase::Running);
    assert_eq!(
        status.lightwalletd_addr,
        Some(fx.listen),
        "the gRPC listener's endpoint"
    );
    let pidfile = std::fs::read_to_string(fx.config.pidfile.as_ref().unwrap()).unwrap();
    assert!(
        pidfile.contains(&fake_zingod().display().to_string()),
        "{pidfile}"
    );
    let lines = history(&handle).await;
    assert!(
        contains(&lines, "zingod started (pid "),
        "the note names zingod"
    );
    let listening = lines
        .iter()
        .find(|line| line.message.starts_with("Listening "))
        .expect("the Listening line");
    assert_eq!(listening.target.as_deref(), Some("Grpc"));

    within(handle.stop()).await.unwrap();
    let status = handle.status();
    assert_eq!(status.phase, Phase::Stopped);
    let exit = status.last_exit.expect("an exit");
    assert_eq!((exit.code, exit.class), (Some(0), ExitClass::Clean));
    let lines = history(&handle).await;
    assert!(contains(&lines, "stop requested (SIGTERM)"));
    assert!(contains(&lines, "Shutdown complete"));
    assert!(contains(
        &lines,
        "zingod exited (exit code 0, Clean, stop requested)"
    ));
    assert!(!fx.config.pidfile.as_ref().unwrap().exists());
    within(handle.shutdown()).await.unwrap();
}

/// What the supervisor makes of zingod's last words, each from fake-zingod.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn zingods_failures_have_their_classes() {
    for (mode, class, detail) in [
        (
            "index-broken",
            ExitClass::IndexBroken,
            "MANIFEST checksum mismatch",
        ),
        (
            "disk-quota",
            ExitClass::DiskFull,
            "segment io: Disk quota exceeded (os error 122)",
        ),
        (
            "disk-full",
            ExitClass::DiskFull,
            "compact_block index commit failed: disk ",
        ),
    ] {
        let fx = fixture(mode);
        let handle = supervisor::spawn(fx.config.clone()).unwrap();
        let _ = within(handle.start()).await;
        let status = wait_status(&handle, LIMIT, mode, |s| {
            matches!(s.phase, Phase::Failed { .. })
        })
        .await;
        let exit = status.last_exit.clone().expect("an exit");
        if exit.class != class {
            dump(&history(&handle).await);
        }
        assert_eq!(status.phase, Phase::Failed { class }, "{mode}: not retried");
        assert!(
            exit.detail
                .as_deref()
                .is_some_and(|d| d.starts_with(detail)),
            "{mode}: {exit:?}"
        );
        assert_eq!(status.restarts, 0, "{mode}");
        within(handle.shutdown()).await.unwrap();
    }

    // A refused permission at open is a crash: retried, then a crash loop.
    let fx = fixture("permission-denied");
    let handle = supervisor::spawn(fx.config.clone()).unwrap();
    let _ = within(handle.start()).await;
    let status = wait_status(&handle, LIMIT, "the crash loop", |s| {
        matches!(s.phase, Phase::Failed { .. })
    })
    .await;
    assert_eq!(
        status.phase,
        Phase::Failed {
            class: ExitClass::Crash
        }
    );
    assert!(status.restarts >= 2, "{status:?}");
    within(handle.shutdown()).await.unwrap();
}

/// A held lock, a taken port and an unknown key, each from its real cause.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_lock_the_port_and_the_configuration() {
    let first = fixture("serving");
    let running = supervisor::spawn(first.config.clone()).unwrap();
    within(running.start()).await.unwrap();

    // The same index on another port.
    let dir = tempfile::tempdir().unwrap();
    let mut second = fixture_in(dir, "serving", free_port());
    let index = |config: &SupervisorConfig| match &config.command {
        supervisor::Launch::Zingod(command) => command.config.clone(),
        supervisor::Launch::Zebrad(_) => unreachable!(),
    };
    let text = std::fs::read_to_string(index(&first.config)).unwrap();
    let port = second.listen.port();
    let moved = text.replace(
        &format!("127.0.0.1:{}", first.listen.port()),
        &format!("127.0.0.1:{port}"),
    );
    std::fs::write(index(&second.config), moved).unwrap();
    second.config.pidfile = None;
    let locked = supervisor::spawn(second.config.clone()).unwrap();
    let _ = within(locked.start()).await;
    let status = wait_status(&locked, LIMIT, "the lock", |s| {
        matches!(s.phase, Phase::Failed { .. })
    })
    .await;
    assert_eq!(
        status.phase,
        Phase::Failed {
            class: ExitClass::DbLocked
        }
    );
    assert!(
        status
            .last_exit
            .and_then(|exit| exit.detail)
            .is_some_and(|d| d.ends_with("/utxo_set/LOCK is held by another process")),
    );
    within(locked.shutdown()).await.unwrap();

    // The first one's port, checked before the spawn.
    let mut taken = fixture("serving");
    taken.config.listeners = watch::channel(vec![Listener {
        role: PortRole::Grpc,
        addr: first.listen,
    }])
    .1;
    let blocked = supervisor::spawn(taken.config.clone()).unwrap();
    let started = within(blocked.start()).await;
    assert!(
        matches!(&started, Err(SupervisorError::PortInUse(detail)) if detail.contains("gRPC")),
        "{started:?}"
    );
    assert_eq!(
        blocked.status().phase,
        Phase::Failed {
            class: ExitClass::PortInUse
        }
    );
    within(blocked.shutdown()).await.unwrap();
    within(running.shutdown()).await.unwrap();

    // A key zingod doesn't know.
    let unknown = fixture("serving");
    let path = index(&unknown.config);
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, format!("bogus = 1\n{text}")).unwrap();
    let refused = supervisor::spawn(unknown.config.clone()).unwrap();
    let _ = within(refused.start()).await;
    let status = wait_status(&refused, LIMIT, "the refusal", |s| {
        matches!(s.phase, Phase::Failed { .. })
    })
    .await;
    assert_eq!(
        status.phase,
        Phase::Failed {
            class: ExitClass::ConfigError
        }
    );
    assert!(
        status
            .last_exit
            .and_then(|exit| exit.detail)
            .is_some_and(|d| d.contains("unknown field `bogus`")),
    );
    within(refused.shutdown()).await.unwrap();
}

/// zingod's txids are gone from the ring and from the file; zebrad's lines reach the file as
/// zebrad wrote them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn redaction_comes_before_the_ring_and_the_file() {
    let txid = chain::block_hash(7);
    let fx = fixture("txid");
    let handle = supervisor::spawn(fx.config.clone()).unwrap();
    within(handle.start()).await.unwrap();
    within(async {
        while !contains(&history(&handle).await, "Transaction not accepted") {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    within(handle.stop()).await.unwrap();
    let lines = history(&handle).await;
    within(handle.shutdown()).await.unwrap();

    let rejected: Vec<_> = lines
        .iter()
        .filter(|line| line.message.starts_with("Transaction not accepted"))
        .collect();
    assert!(!rejected.is_empty());
    for line in &rejected {
        assert!(line.message.contains("txid=[txid]"), "{}", line.message);
    }
    assert!(!lines.iter().any(|line| line.message.contains(&txid)));
    let file = read_log(&fx.log).await;
    assert!(file.contains("txid=[txid]"), "{file}");
    assert!(!file.contains(&txid), "the file holds a txid");

    // zebrad: its reader redacts nothing.
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("zebrad.toml");
    std::fs::write(&config_path, "[network]\nnetwork = \"Regtest\"\n").unwrap();
    let mut config = fast_supervisor_config(fake_zebrad_command("ok", &config_path));
    let log = dir.path().join("zebrad.log");
    config.log_file = Some(RotatingLogConfig::new(&log));
    let zebrad = supervisor::spawn(config).unwrap();
    within(zebrad.start()).await.unwrap();
    within(async {
        while !contains(&history(&zebrad).await, "estimated progress to chain tip") {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    within(zebrad.stop()).await.unwrap();
    let lines = history(&zebrad).await;
    within(zebrad.shutdown()).await.unwrap();
    let file = read_log(&log).await;
    let raw: Vec<&str> = file.lines().filter(|line| !line.starts_with('[')).collect();
    assert!(
        raw.iter()
            .any(|line| line.ends_with("INFO zebrad::commands::start: Starting zebrad"))
    );
    for line in lines
        .iter()
        .filter(|line| line.stream == launcher_types::LogStream::Stdout)
    {
        assert!(
            raw.iter().any(|raw| raw.ends_with(&line.message)),
            "{} is not in the file as zebrad wrote it",
            line.message
        );
    }
}

/// The log file once the writer thread has flushed it.
async fn read_log(path: &Path) -> String {
    let mut text = String::new();
    for _ in 0..100 {
        text = std::fs::read_to_string(path).unwrap_or_default();
        if text.contains("exited (") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    text
}
