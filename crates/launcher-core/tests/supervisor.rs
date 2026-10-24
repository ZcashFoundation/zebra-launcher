//! Supervisor behaviour against the `fake-zebrad` binary.
//!
//! Every await is bounded by [`within`], so a hang fails fast instead of
//! stalling the test run.

use std::{
    future::Future,
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{Command, Stdio},
    sync::Arc,
    time::Duration,
};

use launcher_core::{
    logfile::RotatingLogConfig,
    pidfile,
    policy::RestartPolicy,
    process::ZebradCommand,
    supervisor::{self, StartOptions, SupervisorConfig, SupervisorError, SupervisorHandle},
};
use launcher_types::{ExitClass, LogLine, LogStream, Phase};
use tokio::time::timeout;

const LIMIT: Duration = Duration::from_secs(20);

async fn within<F: Future>(future: F) -> F::Output {
    timeout(LIMIT, future)
        .await
        .expect("operation finished within the test limit")
}

fn fake_zebrad() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_fake-zebrad"))
}

struct Fixture {
    _dir: tempfile::TempDir,
    config: SupervisorConfig,
}

fn fixture(mode: &str) -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("zebrad.toml");
    std::fs::write(&config_path, "[network]\nnetwork = \"Regtest\"\n").expect("write config");
    let mut command = ZebradCommand::new(fake_zebrad(), &config_path);
    command.env = vec![
        ("FAKE_ZEBRAD_MODE".into(), mode.into()),
        ("FAKE_ZEBRAD_HEARTBEAT_MS".into(), "50".into()),
    ];
    let mut config = SupervisorConfig::new(command);
    config.stop_grace = Duration::from_secs(3);
    config.exit_drain = Duration::from_millis(500);
    config.restart = RestartPolicy {
        initial_backoff: Duration::from_millis(50),
        max_backoff: Duration::from_millis(200),
        window: Duration::from_secs(60),
        max_crashes: 3,
    };
    config.pidfile = Some(dir.path().join("zebrad.pid"));
    config.log_file = Some(RotatingLogConfig::new(dir.path().join("zebrad.log")));
    Fixture { _dir: dir, config }
}

fn messages(lines: &[Arc<LogLine>]) -> Vec<String> {
    lines.iter().map(|l| l.message.clone()).collect()
}

fn contains(lines: &[Arc<LogLine>], needle: &str) -> bool {
    lines.iter().any(|l| l.message.contains(needle))
}

async fn wait_phase(
    handle: &SupervisorHandle,
    f: impl FnMut(&Phase) -> bool,
) -> launcher_types::Status {
    let mut f = f;
    within(handle.wait_until(|s| f(&s.phase)))
        .await
        .expect("supervisor is alive")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn starts_streams_logs_and_stops_cleanly() {
    let fx = fixture("ok");
    let handle = supervisor::spawn(fx.config.clone()).expect("spawn supervisor");
    let mut sub = within(handle.subscribe()).await.expect("subscribe");
    assert!(sub.history.is_empty());

    within(handle.start()).await.expect("start");
    assert_eq!(handle.status().phase, Phase::Running);
    // `start` resolves when zebrad accepts its config; the listeners open a little later.
    let status = within(handle.wait_until(|s| s.rpc_addr.is_some() && s.metrics_addr.is_some()))
        .await
        .expect("listeners open");
    assert!(status.pid.is_some());
    assert_eq!(status.rpc_addr, Some("127.0.0.1:8232".parse().unwrap()));
    assert_eq!(status.metrics_addr, Some("127.0.0.1:9999".parse().unwrap()));
    assert!(fx.config.pidfile.as_ref().unwrap().exists());

    // Live lines arrive with contiguous sequence numbers.
    let mut seen = Vec::new();
    while !contains(&seen, "estimated progress to chain tip") {
        let line = within(sub.live.recv()).await.expect("live line");
        seen.push(line);
    }
    for pair in seen.windows(2) {
        assert_eq!(pair[1].seq, pair[0].seq + 1, "gap in {:?}", messages(&seen));
    }

    within(handle.stop()).await.expect("stop");
    let status = handle.status();
    assert_eq!(status.phase, Phase::Stopped);
    assert_eq!(status.pid, None);
    let exit = status.last_exit.expect("exit recorded");
    assert_eq!(exit.code, Some(0), "{exit:?}");
    assert_eq!(exit.class, ExitClass::Clean);
    assert!(exit.requested);
    assert!(!fx.config.pidfile.as_ref().unwrap().exists());

    let history = within(handle.history_since(0)).await.expect("history");
    if cfg!(unix) {
        assert!(
            contains(&history, "received SIGTERM, starting shutdown"),
            "{:#?}",
            messages(&history)
        );
        assert!(contains(&history, "stopping zebrad"));
    }
    assert!(
        history
            .iter()
            .any(|l| l.stream == LogStream::Launcher && l.message.contains("zebrad started"))
    );
    assert!(
        history
            .iter()
            .any(|l| l.stream == LogStream::Launcher && l.message.contains("zebrad exited"))
    );

    within(handle.shutdown()).await.expect("shutdown");
    let log = std::fs::read_to_string(fx.config.log_file.as_ref().unwrap().path.clone())
        .unwrap_or_default();
    assert!(log.contains("Starting zebrad"), "log file: {log:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stdin_close_mode_stops_gracefully_on_every_platform() {
    let mut fx = fixture("ok");
    fx.config.command.shutdown_on_stdin_close = true;
    let handle = supervisor::spawn(fx.config.clone()).expect("spawn supervisor");
    within(handle.start()).await.expect("start");
    within(handle.stop()).await.expect("stop");
    let exit = handle.status().last_exit.expect("exit recorded");
    assert_eq!(exit.code, Some(0), "{exit:?}");
    let history = within(handle.history_since(0)).await.expect("history");
    assert!(
        contains(&history, "stdin closed, starting shutdown"),
        "{:#?}",
        messages(&history)
    );
    assert!(contains(&history, "stop requested (stdin closed)"));
    within(handle.shutdown()).await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hanging_child_is_killed_after_the_grace_period() {
    let mut fx = fixture("hang-on-signal");
    fx.config.stop_grace = Duration::from_millis(500);
    // Stop while the fake is still starting up: like zebrad, it handles
    // shutdown requests from the start.
    fx.config
        .command
        .env
        .push(("FAKE_ZEBRAD_STARTUP_DELAY_MS".into(), "5000".into()));
    let handle = supervisor::spawn(fx.config.clone()).expect("spawn supervisor");
    within(handle.start()).await.expect("start");
    let started = std::time::Instant::now();
    within(handle.stop()).await.expect("stop");
    assert!(started.elapsed() < Duration::from_secs(10));
    let exit = handle.status().last_exit.expect("exit recorded");
    assert!(exit.requested);
    assert_ne!(exit.code, Some(0), "{exit:?}");
    // The kill finished the stop the user asked for: not a crash.
    assert_eq!(exit.class, ExitClass::Clean, "{exit:?}");
    assert_eq!(exit.detail.as_deref(), Some("killed by the launcher"));
    let history = within(handle.history_since(0)).await.expect("history");
    assert!(
        contains(&history, "did not stop within"),
        "{:#?}",
        messages(&history)
    );
    // The SIGTERM reached the fake's handler instead of killing it outright.
    #[cfg(unix)]
    assert!(
        contains(&history, "ignoring in hang-on-signal mode"),
        "{:#?}",
        messages(&history)
    );
    within(handle.shutdown()).await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_crash_during_a_graceful_stop_is_not_clean() {
    let mut fx = fixture("crash-on-stop");
    // The graceful path on every platform, so the stop is never forced.
    fx.config.command.shutdown_on_stdin_close = true;
    let handle = supervisor::spawn(fx.config.clone()).expect("spawn supervisor");
    within(handle.start()).await.expect("start");
    within(handle.stop()).await.expect("stop");

    let status = handle.status();
    assert_eq!(
        status.phase,
        Phase::Stopped,
        "a requested stop never restarts"
    );
    let exit = status.last_exit.expect("exit recorded");
    assert_eq!(exit.code, Some(3), "{exit:?}");
    assert_eq!(exit.class, ExitClass::Crash, "{exit:?}");
    assert!(exit.requested, "{exit:?}");
    assert_eq!(exit.detail, None, "not killed by the launcher: {exit:?}");
    let history = within(handle.history_since(0)).await.expect("history");
    assert!(
        contains(
            &history,
            "zebrad exited (exit code 3, Crash, stop requested)"
        ),
        "{:#?}",
        messages(&history)
    );
    within(handle.shutdown()).await.expect("shutdown");
}

/// A crashing fake whose output pipes stay open for `linger` after it exits
/// (a grandchild holds them, as a zebrad subprocess could), so the supervisor
/// sits in its exit drain with the phase still `Running`.
fn draining_crash_fixture(linger: Duration) -> Fixture {
    let mut fx = fixture("crash");
    fx.config.exit_drain = Duration::from_millis(1500);
    fx.config.command.env.push((
        "FAKE_ZEBRAD_LINGER_MS".into(),
        linger.as_millis().to_string().into(),
    ));
    fx
}

/// Starts `handle` and returns once the fake has crashed and the supervisor
/// has had time to record the exit, while its pipes are still open.
async fn start_and_wait_for_the_crash(handle: &SupervisorHandle) {
    let mut sub = within(handle.subscribe()).await.expect("subscribe");
    within(handle.start()).await.expect("start");
    loop {
        let line = within(sub.live.recv()).await.expect("live line");
        if line.message.contains("simulated crash") {
            break;
        }
    }
    // The exit event follows the last line within microseconds; the actor
    // processes child events before commands.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        handle.status().phase,
        Phase::Running,
        "the exit is not classified while the pipes are open"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_after_the_exit_does_not_signal_or_relabel_the_crash() {
    let fx = draining_crash_fixture(Duration::from_secs(4));
    let handle = supervisor::spawn(fx.config.clone()).expect("spawn supervisor");
    start_and_wait_for_the_crash(&handle).await;

    // Answered when the drain ends, not after the stop grace.
    within(handle.stop()).await.expect("stop");
    let status = handle.status();
    assert_eq!(status.phase, Phase::Stopped);
    let exit = status.last_exit.expect("exit recorded");
    assert_eq!(exit.code, Some(2), "{exit:?}");
    assert_eq!(exit.class, ExitClass::Crash, "{exit:?}");
    assert!(!exit.requested, "the stop did not end the run: {exit:?}");

    let history = within(handle.history_since(0)).await.expect("history");
    assert!(
        !contains(&history, "stop requested ("),
        "no signal to a reaped pid: {:#?}",
        messages(&history)
    );
    assert!(
        contains(&history, "stop requested after zebrad exited"),
        "{:#?}",
        messages(&history)
    );
    // The user asked for a stop: the crash policy does not restart zebrad.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(handle.status().phase, Phase::Stopped);
    let history = within(handle.history_since(0)).await.expect("history");
    assert!(!contains(&history, "restarting zebrad in"));
    within(handle.shutdown()).await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_during_the_exit_drain_learns_that_zebrad_exited() {
    let fx = draining_crash_fixture(Duration::from_secs(4));
    let handle = supervisor::spawn(fx.config.clone()).expect("spawn supervisor");
    start_and_wait_for_the_crash(&handle).await;

    let err = within(handle.start()).await.expect_err("the child is gone");
    let SupervisorError::ExitedDuringStart(info) = err else {
        panic!("unexpected error: {err}");
    };
    assert_eq!(info.class, ExitClass::Crash, "{info:?}");
    assert_eq!(info.code, Some(2), "{info:?}");
    // Nobody asked for a stop, so the crash policy applies.
    assert!(
        !matches!(handle.status().phase, Phase::Stopped),
        "{:?}",
        handle.status().phase
    );
    within(handle.shutdown()).await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_error_fails_without_restarting() {
    let fx = fixture("config-error");
    let handle = supervisor::spawn(fx.config.clone()).expect("spawn supervisor");
    let err = within(handle.start()).await.expect_err("start fails");
    let SupervisorError::ExitedDuringStart(info) = err else {
        panic!("unexpected error: {err}");
    };
    assert_eq!(info.class, ExitClass::ConfigError);
    assert_eq!(info.code, Some(1));
    assert!(
        info.detail
            .as_deref()
            .unwrap_or_default()
            .contains("Configuration error"),
        "{info:?}"
    );
    assert_eq!(
        handle.status().phase,
        Phase::Failed {
            class: ExitClass::ConfigError
        }
    );
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        handle.status().phase,
        Phase::Failed {
            class: ExitClass::ConfigError
        },
        "must not restart"
    );
    within(handle.shutdown()).await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn crashes_restart_with_backoff_then_give_up() {
    let fx = fixture("crash");
    let handle = supervisor::spawn(fx.config.clone()).expect("spawn supervisor");
    // The fake crashes right after it reports `Starting zebrad`, so start succeeds.
    within(handle.start()).await.expect("start");
    let status = wait_phase(&handle, |p| matches!(p, Phase::Failed { .. })).await;
    assert_eq!(
        status.phase,
        Phase::Failed {
            class: ExitClass::Crash
        }
    );
    let history = within(handle.history_since(0)).await.expect("history");
    let restarts = history
        .iter()
        .filter(|l| l.message.starts_with("restarting zebrad in"))
        .count();
    assert_eq!(restarts, 2, "{:#?}", messages(&history));
    let exits = history
        .iter()
        .filter(|l| l.message.starts_with("zebrad exited"))
        .count();
    assert_eq!(exits, 3);
    within(handle.shutdown()).await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn database_lock_and_port_conflicts_are_deterministic_failures() {
    let fx = fixture("db-locked");
    let handle = supervisor::spawn(fx.config.clone()).expect("spawn supervisor");
    // The fake prints `Starting zebrad` before it panics, so the start resolves first.
    let _ = within(handle.start()).await;
    let status = wait_phase(&handle, |p| matches!(p, Phase::Failed { .. })).await;
    assert_eq!(
        status.phase,
        Phase::Failed {
            class: ExitClass::DbLocked
        }
    );
    let exit = status.last_exit.expect("exit");
    assert!(
        exit.detail
            .as_deref()
            .unwrap_or_default()
            .contains("Database likely already open"),
        "{exit:?}"
    );
    within(handle.shutdown()).await.expect("shutdown");

    let fx = fixture("port-in-use");
    let handle = supervisor::spawn(fx.config.clone()).expect("spawn supervisor");
    let err = within(handle.start())
        .await
        .expect_err("start fails before Starting zebrad");
    let SupervisorError::ExitedDuringStart(info) = err else {
        panic!("unexpected error: {err}");
    };
    assert_eq!(info.class, ExitClass::PortInUse);
    assert_eq!(
        handle.status().phase,
        Phase::Failed {
            class: ExitClass::PortInUse
        }
    );
    within(handle.shutdown()).await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn late_subscriber_gets_history_then_contiguous_live_lines() {
    let fx = fixture("ok");
    let handle = supervisor::spawn(fx.config.clone()).expect("spawn supervisor");
    within(handle.start()).await.expect("start");
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut sub = within(handle.subscribe()).await.expect("subscribe");
    assert!(sub.history.len() > 5, "{:#?}", messages(&sub.history));
    let last = sub.history.last().unwrap().seq;
    let next = within(sub.live.recv()).await.expect("live line");
    assert_eq!(next.seq, last + 1);
    within(handle.shutdown()).await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stderr_flood_does_not_block_and_history_is_bounded() {
    let mut fx = fixture("stderr-flood");
    fx.config.history_capacity = 500;
    let handle = supervisor::spawn(fx.config.clone()).expect("spawn supervisor");
    let started = std::time::Instant::now();
    within(handle.start()).await.expect("start");
    // Wait until the flood has been drained: the heartbeat after it shows up.
    let mut sub = within(handle.subscribe()).await.expect("subscribe");
    loop {
        let line = within(sub.live.recv()).await;
        match line {
            Ok(line) if line.message.contains("estimated progress") => break,
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
            Err(err) => panic!("{err}"),
        }
    }
    assert!(started.elapsed() < Duration::from_secs(10));
    let history = within(handle.history_since(0)).await.expect("history");
    assert!(history.len() <= 500, "{}", history.len());
    assert!(
        history
            .iter()
            .any(|l| l.stream == LogStream::Stderr && l.message.starts_with("flood line"))
    );
    within(handle.shutdown()).await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_utf8_is_replaced_not_fatal() {
    let fx = fixture("non-utf8");
    let handle = supervisor::spawn(fx.config.clone()).expect("spawn supervisor");
    within(handle.start()).await.expect("start");
    let mut sub = within(handle.subscribe()).await.expect("subscribe");
    loop {
        if sub
            .history
            .iter()
            .chain(std::iter::empty())
            .any(|l| l.message.contains('\u{FFFD}'))
        {
            break;
        }
        let line = within(sub.live.recv()).await.expect("live line");
        if line.message.contains('\u{FFFD}') {
            break;
        }
    }
    assert_eq!(handle.status().phase, Phase::Running);
    within(handle.shutdown()).await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stays_running_while_idle() {
    // Guards the Linux parent-death-signal thread semantics: the spawning
    // thread must outlive the idle period, or zebrad would receive SIGTERM.
    let fx = fixture("ok");
    let handle = supervisor::spawn(fx.config.clone()).expect("spawn supervisor");
    within(handle.start()).await.expect("start");
    tokio::time::sleep(Duration::from_secs(12)).await;
    assert_eq!(handle.status().phase, Phase::Running);
    within(handle.shutdown()).await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_pidfile_blocks_start_unless_killed() {
    let fx = fixture("ok");
    // A zebrad "left behind": the fake started outside the supervisor.
    let mut orphan = Command::new(fake_zebrad())
        .args([
            "-c",
            &fx.config.command.config.display().to_string(),
            "start",
        ])
        .env("FAKE_ZEBRAD_MODE", "ok")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn orphan");
    let pidfile_path = fx.config.pidfile.clone().unwrap();
    let orphan_pid = orphan.id();
    pidfile::write(&pidfile_path, orphan_pid, &fake_zebrad()).expect("write pidfile");

    let handle = supervisor::spawn(fx.config.clone()).expect("spawn supervisor");
    let err = within(handle.start())
        .await
        .expect_err("stale process blocks start");
    assert!(
        matches!(err, SupervisorError::StaleProcess(ref s) if s.pid == orphan_pid),
        "{err}"
    );
    assert_eq!(
        handle.status().stale.as_ref().map(|s| s.pid),
        Some(orphan_pid)
    );

    // A real orphan is reaped by init once it dies; here the test process is its
    // parent, so reap it in the background or it would linger as a zombie that
    // `is_alive` still reports.
    let reaper = tokio::task::spawn_blocking(move || orphan.wait());
    within(handle.start_with(StartOptions { kill_stale: true }))
        .await
        .expect("start after kill");
    assert_eq!(handle.status().phase, Phase::Running);
    assert_eq!(handle.status().stale, None);
    // The orphan either shut down gracefully on SIGTERM (exit 0) or was killed
    // before its handler was up; either way it is gone.
    let _status = within(reaper).await.unwrap().expect("orphan reaped");
    assert!(!pidfile::is_alive(orphan_pid));
    within(handle.shutdown()).await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_actor_answers_commands_while_a_stale_zebrad_is_terminated() {
    let fx = fixture("ok");
    // A left-behind zebrad that ignores the termination request, so the
    // supervisor waits the whole stale-kill period for it (on Unix; Windows
    // terminates it outright).
    let mut orphan = Command::new(fake_zebrad())
        .args([
            "-c",
            &fx.config.command.config.display().to_string(),
            "start",
        ])
        .env("FAKE_ZEBRAD_MODE", "hang-on-signal")
        .env("FAKE_ZEBRAD_HEARTBEAT_MS", "60000")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn orphan");
    // Its signal handlers are installed before its first line.
    let mut stdout = BufReader::new(orphan.stdout.take().expect("piped stdout"));
    let mut first_line = String::new();
    tokio::task::spawn_blocking(move || stdout.read_line(&mut first_line))
        .await
        .expect("task")
        .expect("the orphan started");
    let orphan_pid = orphan.id();
    pidfile::write(
        fx.config.pidfile.as_ref().unwrap(),
        orphan_pid,
        &fake_zebrad(),
    )
    .expect("write pidfile");

    let handle = supervisor::spawn(fx.config.clone()).expect("spawn supervisor");
    let starter = {
        let handle = handle.clone();
        tokio::spawn(async move { handle.start_with(StartOptions { kill_stale: true }).await })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    // The actor is not blocked on the stale zebrad.
    let history = timeout(Duration::from_secs(1), handle.history_since(0))
        .await
        .expect("the actor answers while the stale zebrad is waited for")
        .expect("supervisor is alive");
    assert!(
        !contains(&history, "zebrad started"),
        "{:#?}",
        messages(&history)
    );
    #[cfg(unix)]
    assert!(!starter.is_finished(), "the stale zebrad is still alive");

    let result = within(starter).await.expect("task");
    if cfg!(unix) {
        assert!(
            matches!(result, Err(SupervisorError::StaleProcess(ref s)) if s.pid == orphan_pid),
            "{result:?}"
        );
        assert_eq!(
            handle.status().stale.as_ref().map(|s| s.pid),
            Some(orphan_pid)
        );
    } else {
        result.expect("the stale zebrad was terminated");
    }
    orphan.kill().expect("kill orphan");
    orphan.wait().expect("reap orphan");
    within(handle.shutdown()).await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_stops_zebrad_and_closes_the_actor() {
    let fx = fixture("ok");
    let handle = supervisor::spawn(fx.config.clone()).expect("spawn supervisor");
    within(handle.start()).await.expect("start");
    let pid = handle.status().pid.unwrap();
    within(handle.shutdown()).await.expect("shutdown");
    assert_eq!(handle.status().phase, Phase::Stopped);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !pidfile::is_alive(pid) || cfg!(windows),
        "zebrad should be gone"
    );
    assert!(matches!(
        within(handle.start()).await,
        Err(SupervisorError::Closed)
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_during_startup_and_restart() {
    let mut fx = fixture("ok");
    fx.config
        .command
        .env
        .push(("FAKE_ZEBRAD_STARTUP_DELAY_MS".into(), "1500".into()));
    let handle = supervisor::spawn(fx.config.clone()).expect("spawn supervisor");
    let starter = {
        let handle = handle.clone();
        tokio::spawn(async move { handle.start().await })
    };
    wait_phase(&handle, |p| matches!(p, Phase::Starting | Phase::Running)).await;
    within(handle.stop()).await.expect("stop");
    assert_eq!(handle.status().phase, Phase::Stopped);
    let start_result = within(starter).await.expect("task");
    assert!(
        matches!(
            start_result,
            Err(SupervisorError::ExitedDuringStart(_)) | Ok(())
        ),
        "{start_result:?}"
    );

    within(handle.restart()).await.expect("restart");
    assert_eq!(handle.status().phase, Phase::Running);
    within(handle.shutdown()).await.expect("shutdown");
}
