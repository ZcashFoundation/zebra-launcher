//! Helpers shared by the integration tests, included with
//! `#[path = "support/helpers.rs"] mod support;` (a file directly under
//! `tests/` would be a test target of its own). Each test file compiles the
//! whole module and uses the part it needs.

#![allow(dead_code)]

use std::{
    future::Future,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use launcher_core::{
    policy::RestartPolicy,
    process::ZebradCommand,
    supervisor::{SupervisorConfig, SupervisorHandle},
};
use launcher_types::{LogLine, Status};
use tokio::time::timeout;

/// The limit on every await of the fake-zebrad tests.
pub const LIMIT: Duration = Duration::from_secs(20);

/// `future`, or a panic naming `what` once `limit` has passed, so a hang
/// fails fast instead of stalling the test run.
pub async fn within_limit<F: Future>(limit: Duration, what: &str, future: F) -> F::Output {
    timeout(limit, future)
        .await
        .unwrap_or_else(|_| panic!("{what} did not finish within {limit:?}"))
}

/// [`within_limit`] with [`LIMIT`].
pub async fn within<F: Future>(future: F) -> F::Output {
    within_limit(LIMIT, "the operation", future).await
}

/// The `fake-zebrad` binary Cargo built for these tests.
pub fn fake_zebrad() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_fake-zebrad"))
}

/// `fake-zebrad -c config start` in `mode` (`FAKE_ZEBRAD_MODE`), with a
/// 50 ms heartbeat.
pub fn fake_zebrad_command(mode: &str, config: &Path) -> ZebradCommand {
    let mut command = ZebradCommand::new(fake_zebrad(), config);
    command.env = vec![
        ("FAKE_ZEBRAD_MODE".into(), mode.into()),
        ("FAKE_ZEBRAD_HEARTBEAT_MS".into(), "50".into()),
    ];
    command
}

/// A supervisor configuration for fast tests: a 3 s stop grace, a 500 ms
/// exit drain, and restarts that back off 50 to 200 ms and give up after
/// three crashes within a minute. No pid file and no log file.
pub fn fast_supervisor_config(command: ZebradCommand) -> SupervisorConfig {
    let mut config = SupervisorConfig::new(command);
    config.stop_grace = Duration::from_secs(3);
    config.exit_drain = Duration::from_millis(500);
    config.restart = RestartPolicy {
        initial_backoff: Duration::from_millis(50),
        max_backoff: Duration::from_millis(200),
        window: Duration::from_secs(60),
        max_crashes: 3,
    };
    config
}

/// The messages of `lines`.
pub fn messages(lines: &[Arc<LogLine>]) -> Vec<String> {
    lines.iter().map(|l| l.message.clone()).collect()
}

/// Whether a message in `lines` contains `needle`.
pub fn contains(lines: &[Arc<LogLine>], needle: &str) -> bool {
    lines.iter().any(|l| l.message.contains(needle))
}

/// Prints `lines`, so a failure on CI shows what zebrad said.
pub fn dump(lines: &[Arc<LogLine>]) {
    for line in lines {
        eprintln!("[{:?}] {}", line.stream, line.message);
    }
}

/// The status once `predicate` holds for it, within `limit`.
pub async fn wait_status(
    handle: &SupervisorHandle,
    limit: Duration,
    what: &str,
    predicate: impl FnMut(&Status) -> bool,
) -> Status {
    within_limit(limit, what, handle.wait_until(predicate))
        .await
        .expect("supervisor is alive")
}
