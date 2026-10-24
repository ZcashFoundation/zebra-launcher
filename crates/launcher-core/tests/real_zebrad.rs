//! Contract tests against a real zebrad.
//!
//! Ignored by default, so `cargo test` stays hermetic; CI runs them on bump pull
//! requests with a fetched release binary. Run them by hand with:
//!
//! ```sh
//! ZEBRAD_BIN=/path/to/zebrad cargo test -p launcher-core --test real_zebrad -- --include-ignored --nocapture
//! ```
//!
//! They start zebrad on Regtest through the supervisor, with every listener on
//! loopback and all state in a temporary directory, and check the log lines and
//! exit behaviour the supervisor relies on. Every await is bounded by a timeout
//! generous enough for a slow CI machine.

use std::{
    future::Future,
    net::{Ipv4Addr, SocketAddr, TcpListener},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use launcher_core::{
    logfile::RotatingLogConfig,
    logs::Detector,
    process::ZebradCommand,
    supervisor::{self, SupervisorConfig, SupervisorError, SupervisorHandle},
};
use launcher_types::{ExitClass, LogEvent, LogLine, Phase, Status};
use tokio::time::timeout;

/// Starting includes opening the state database, which can be slow on CI.
const START_LIMIT: Duration = Duration::from_secs(120);
/// Above the 60 s stop grace, so an escalation to kill is observed, not timed out.
const STOP_LIMIT: Duration = Duration::from_secs(90);
/// Commands that only talk to the actor.
const CMD_LIMIT: Duration = Duration::from_secs(10);

async fn within<F: Future>(limit: Duration, what: &str, future: F) -> F::Output {
    timeout(limit, future)
        .await
        .unwrap_or_else(|_| panic!("{what} did not finish within {limit:?}"))
}

/// The zebrad under test. The tests are `#[ignore]`, so reaching this without `ZEBRAD_BIN`
/// means `--include-ignored` was passed by mistake: fail loudly instead of passing silently.
fn zebrad_bin() -> Option<PathBuf> {
    match std::env::var_os("ZEBRAD_BIN").filter(|v| !v.is_empty()) {
        Some(path) => {
            let path = PathBuf::from(path);
            assert!(
                path.is_file(),
                "ZEBRAD_BIN is set but {} is not a file",
                path.display()
            );
            Some(path)
        }
        None => panic!(
            "ZEBRAD_BIN is not set: these contract tests are ignored by default and run only with \
             `--include-ignored` and ZEBRAD_BIN pointing at a zebrad binary"
        ),
    }
}

/// A loopback port that was free a moment ago.
fn free_port() -> u16 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind a free port");
    listener.local_addr().expect("local addr").port()
}

/// A TOML literal string for a path (no escapes, so Windows paths work).
fn toml_path(path: &Path) -> String {
    let text = path.display().to_string();
    assert!(!text.contains('\''), "unsupported path {text}");
    format!("'{text}'")
}

/// One zebrad instance: its config, cookie dir and supervisor config.
struct Instance {
    metrics_port: u16,
    cookie_dir: PathBuf,
    config: SupervisorConfig,
}

/// Writes a Regtest config under `dir` that keeps state in `state_dir`, and a
/// supervisor config with a pid file, a log file and a 60 s stop grace.
fn instance(zebrad: &Path, dir: &Path, state_dir: &Path) -> Instance {
    std::fs::create_dir_all(dir).expect("instance dir");
    let metrics_port = free_port();
    let cookie_dir = dir.join("rpc");
    let config_path = dir.join("zebrad.toml");
    let toml = format!(
        "[network]\n\
         network = \"Regtest\"\n\
         listen_addr = \"127.0.0.1:0\"\n\
         cache_dir = {state}\n\
         \n\
         [state]\n\
         cache_dir = {state}\n\
         \n\
         [rpc]\n\
         listen_addr = \"127.0.0.1:0\"\n\
         cookie_dir = {cookie}\n\
         \n\
         [metrics]\n\
         endpoint_addr = \"127.0.0.1:{metrics_port}\"\n\
         \n\
         [tracing]\n\
         use_color = false\n",
        state = toml_path(state_dir),
        cookie = toml_path(&cookie_dir),
    );
    std::fs::write(&config_path, toml).expect("write zebrad.toml");

    let mut config = SupervisorConfig::new(ZebradCommand::new(zebrad, &config_path));
    config.stop_grace = Duration::from_secs(60);
    config.pidfile = Some(dir.join("zebrad.pid"));
    config.log_file = Some(RotatingLogConfig::new(dir.join("zebrad.log")));
    Instance {
        metrics_port,
        cookie_dir,
        config,
    }
}

fn contains(lines: &[Arc<LogLine>], needle: &str) -> bool {
    lines.iter().any(|line| line.message.contains(needle))
}

/// Prints the history, so a failure on CI shows what zebrad said.
fn dump(lines: &[Arc<LogLine>]) {
    for line in lines {
        eprintln!("[{:?}] {}", line.stream, line.message);
    }
}

/// The name of each startup event the supervisor depends on.
fn startup_event_name(event: &LogEvent) -> Option<&'static str> {
    Some(match event {
        LogEvent::TracingStarted => "TracingStarted",
        LogEvent::MetricsOpened { .. } => "MetricsOpened",
        LogEvent::StartingZebrad => "StartingZebrad",
        LogEvent::P2pOpened { .. } => "P2pOpened",
        LogEvent::RpcCookieWritten => "RpcCookieWritten",
        LogEvent::RpcOpened { .. } => "RpcOpened",
        _ => return None,
    })
}

async fn wait_status(
    handle: &SupervisorHandle,
    limit: Duration,
    what: &str,
    predicate: impl FnMut(&Status) -> bool,
) -> Status {
    within(limit, what, handle.wait_until(predicate))
        .await
        .expect("supervisor is alive")
}

/// Starts `config` and waits until zebrad's RPC and metrics endpoints are open.
async fn start_running(config: SupervisorConfig) -> SupervisorHandle {
    let handle = supervisor::spawn(config).expect("spawn supervisor");
    if let Err(err) = within(START_LIMIT, "start", handle.start()).await {
        let history = within(CMD_LIMIT, "history", handle.history_since(0))
            .await
            .unwrap_or_default();
        dump(&history);
        panic!("zebrad did not start: {err}");
    }
    wait_status(&handle, START_LIMIT, "RPC and metrics open", |s| {
        s.rpc_addr.is_some() && s.metrics_addr.is_some()
    })
    .await;
    handle
}

#[ignore = "needs ZEBRAD_BIN; run with -- --include-ignored"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn regtest_starts_reports_and_stops_cleanly() {
    let Some(zebrad) = zebrad_bin() else {
        return;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let node = instance(&zebrad, &dir.path().join("node"), &dir.path().join("state"));
    let pidfile = node.config.pidfile.clone().expect("pidfile");
    let log_file = node.config.log_file.clone().expect("log file").path;

    let handle = start_running(node.config.clone()).await;
    let status = handle.status();
    assert_eq!(status.phase, Phase::Running);
    assert!(status.pid.is_some());
    assert!(pidfile.exists(), "pid file written while running");
    assert_eq!(
        status.metrics_addr,
        Some(SocketAddr::from((Ipv4Addr::LOCALHOST, node.metrics_port)))
    );
    let rpc_addr = status.rpc_addr.expect("rpc addr");
    assert!(rpc_addr.ip().is_loopback(), "{rpc_addr}");
    assert_ne!(rpc_addr.port(), 0, "the real port is reported, not 0");

    // The startup events, recomputed from the history, in the order zebrad logs them.
    let sub = within(CMD_LIMIT, "subscribe", handle.subscribe())
        .await
        .expect("subscribe");
    let mut detector = Detector::new();
    let events: Vec<LogEvent> = sub
        .history
        .iter()
        .filter_map(|line| detector.detect(line))
        .collect();
    let names: Vec<_> = events.iter().filter_map(startup_event_name).collect();
    if names
        != [
            "TracingStarted",
            "MetricsOpened",
            "StartingZebrad",
            "P2pOpened",
            "RpcCookieWritten",
            "RpcOpened",
        ]
    {
        dump(&sub.history);
        panic!("unexpected startup events: {events:#?}");
    }
    assert!(events.contains(&LogEvent::RpcOpened { addr: rpc_addr }));
    for event in &events {
        if let LogEvent::P2pOpened { addr } = event {
            assert!(addr.ip().is_loopback(), "{addr}");
        }
    }

    // zebrad wrote its RPC cookie where the config said, in `__cookie__:<base64>` form.
    let cookie = std::fs::read_to_string(node.cookie_dir.join(".cookie")).expect("read .cookie");
    let password = cookie
        .strip_prefix("__cookie__:")
        .unwrap_or_else(|| panic!("unexpected cookie format: {cookie:?}"));
    assert!(!password.is_empty() && !password.contains(char::is_whitespace));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(node.cookie_dir.join(".cookie"))
            .expect("cookie metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "cookie is owner-only");
    }

    let stop_started = Instant::now();
    within(STOP_LIMIT, "stop", handle.stop())
        .await
        .expect("stop");
    eprintln!(
        "zebrad stopped {:?} after the stop request",
        stop_started.elapsed()
    );

    let status = handle.status();
    let history = within(CMD_LIMIT, "history", handle.history_since(0))
        .await
        .expect("history");
    let exit = status.last_exit.clone().expect("exit recorded");
    if exit.code != Some(0) || exit.class != ExitClass::Clean {
        dump(&history);
    }
    assert_eq!(status.phase, Phase::Stopped);
    assert_eq!(status.pid, None);
    assert_eq!(exit.code, Some(0), "{exit:?}");
    assert_eq!(exit.class, ExitClass::Clean);
    assert!(exit.requested);
    assert_eq!(exit.detail, None, "a graceful stop is not a kill: {exit:?}");
    assert!(!pidfile.exists(), "pid file removed after exit");

    for needle in [
        "received SIGTERM, starting shutdown",
        "shutting down Zebra",
        "stopping zebrad",
    ] {
        assert!(contains(&history, needle), "missing {needle:?}");
    }
    assert!(
        !contains(&history, "did not stop within"),
        "the stop did not escalate to a kill"
    );

    within(CMD_LIMIT, "shutdown", handle.shutdown())
        .await
        .expect("shutdown");
    // The writer thread flushes once the actor is gone; poll briefly for the file.
    let deadline = Instant::now() + CMD_LIMIT;
    loop {
        let text = std::fs::read_to_string(&log_file).unwrap_or_default();
        if text.contains("Starting zebrad") && text.contains("stopping zebrad") {
            break;
        }
        assert!(Instant::now() < deadline, "log file incomplete:\n{text}");
        within(
            CMD_LIMIT,
            "poll",
            tokio::time::sleep(Duration::from_millis(100)),
        )
        .await;
    }
}

#[ignore = "needs ZEBRAD_BIN; run with -- --include-ignored"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn second_instance_on_the_same_state_fails_with_db_locked() {
    let Some(zebrad) = zebrad_bin() else {
        return;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let state = dir.path().join("state");
    let first = instance(&zebrad, &dir.path().join("first"), &state);
    let second = instance(&zebrad, &dir.path().join("second"), &state);

    let first_handle = start_running(first.config).await;

    let second_handle = supervisor::spawn(second.config).expect("spawn supervisor");
    // zebrad logs "Starting zebrad" before it opens the state, so the start can
    // succeed and the lock panic follow it, or zebrad can exit first.
    match within(START_LIMIT, "second start", second_handle.start()).await {
        Ok(()) => eprintln!("second zebrad reached Running before the lock panic"),
        Err(SupervisorError::ExitedDuringStart(info)) => {
            eprintln!("second zebrad exited before reaching Running");
            assert_eq!(info.class, ExitClass::DbLocked, "{info:?}");
        }
        Err(err) => panic!("unexpected start error: {err}"),
    }
    let status = wait_status(&second_handle, START_LIMIT, "second zebrad fails", |s| {
        !matches!(s.phase, Phase::Starting | Phase::Running)
    })
    .await;
    let history = within(CMD_LIMIT, "history", second_handle.history_since(0))
        .await
        .expect("history");
    if status.phase
        != (Phase::Failed {
            class: ExitClass::DbLocked,
        })
    {
        dump(&history);
    }
    assert_eq!(
        status.phase,
        Phase::Failed {
            class: ExitClass::DbLocked
        }
    );
    let exit = status.last_exit.expect("exit recorded");
    eprintln!("second zebrad: {exit:?}");
    assert_eq!(exit.class, ExitClass::DbLocked);
    assert!(!exit.requested);
    assert!(
        exit.detail
            .as_deref()
            .is_some_and(|d| d.contains("Database likely already open")),
        "{exit:?}"
    );
    assert!(contains(&history, "Database likely already open"));

    // A deterministic failure is not retried.
    within(
        CMD_LIMIT,
        "no restart",
        tokio::time::sleep(Duration::from_secs(2)),
    )
    .await;
    assert_eq!(second_handle.status().phase, status.phase);

    // The first zebrad was not disturbed.
    assert_eq!(first_handle.status().phase, Phase::Running);
    within(STOP_LIMIT, "stop first", first_handle.stop())
        .await
        .expect("stop first");
    let exit = first_handle.status().last_exit.expect("exit recorded");
    assert_eq!(exit.code, Some(0), "{exit:?}");
    assert_eq!(exit.class, ExitClass::Clean);

    within(CMD_LIMIT, "shutdown second", second_handle.shutdown())
        .await
        .expect("shutdown second");
    within(CMD_LIMIT, "shutdown first", first_handle.shutdown())
        .await
        .expect("shutdown first");
}
