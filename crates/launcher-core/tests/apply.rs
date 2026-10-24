//! `apply::apply_and_restart` against the `fake-zebrad` binary: the file
//! handling and the restart with a real supervisor. The rollback branches,
//! which need a zebrad that refuses one file and accepts the next, are
//! covered by the scripted node in the module's unit tests (fake-zebrad's
//! mode is fixed per process).
//!
//! Every await is bounded by [`within`], so a hang fails fast.

use std::{fs, future::Future, path::PathBuf, time::Duration};

use launcher_core::{
    apply,
    config::{self, ManagedKeys},
    paths::Paths,
    policy::RestartPolicy,
    process::ZebradCommand,
    supervisor::{self, SupervisorConfig, SupervisorError, SupervisorHandle},
};
use launcher_types::{ApplyOutcome, ConfigDocument, ExitClass, Network, Phase};
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
    paths: Paths,
    keys: ManagedKeys,
    original: ConfigDocument,
    handle: SupervisorHandle,
}

/// A managed `zebrad.toml` in a temporary layout and a supervisor running
/// fake-zebrad in `mode` on it. The RPC listener takes any free port, so
/// tests do not collide.
fn fixture(mode: &str) -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = Paths::under(dir.path());
    paths.ensure_dirs().expect("dirs");
    let mut keys = ManagedKeys::defaults(Network::Mainnet, &paths, paths.default_state_dir());
    keys.rpc_addr = "127.0.0.1:0".parse().expect("socket address");
    config::ensure(&paths, &keys).expect("ensure");
    let original = config::read(&paths).expect("read");

    let mut command = ZebradCommand::new(fake_zebrad(), paths.zebrad_config());
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
    config.pidfile = Some(paths.pidfile());
    let handle = supervisor::spawn(config).expect("spawn supervisor");
    Fixture {
        _dir: dir,
        paths,
        keys,
        original,
        handle,
    }
}

fn new_text(original: &ConfigDocument) -> String {
    format!("{}\n[mempool]\ntx_cost_limit = 1000\n", original.text)
}

async fn apply(fx: &Fixture) -> Result<ApplyOutcome, launcher_types::ApplyError> {
    within(apply::apply_and_restart(
        &fx.paths,
        &fx.handle,
        new_text(&fx.original),
        fx.original.token.clone(),
        &fx.keys,
        LIMIT,
    ))
    .await
}

fn assert_new_file_kept(fx: &Fixture) {
    let text = fs::read_to_string(fx.paths.zebrad_config()).expect("read zebrad.toml");
    assert!(text.contains("tx_cost_limit = 1000"), "{text}");
    assert!(
        !fx.paths.pending_apply_marker().exists(),
        "the marker is removed once the change is committed"
    );
    assert_eq!(
        fs::read_to_string(fx.paths.zebrad_config_backup()).expect("backup"),
        fx.original.text
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restarts_a_running_zebrad_with_the_new_file() {
    let fx = fixture("ok");
    within(fx.handle.start()).await.expect("start");
    let before = within(fx.handle.wait_until(|s| s.pid.is_some()))
        .await
        .expect("running");

    assert_eq!(apply(&fx).await, Ok(ApplyOutcome::Applied));
    assert_new_file_kept(&fx);

    // A new zebrad process runs on the new file.
    let after = fx.handle.status();
    assert_eq!(after.phase, Phase::Running);
    assert!(after.pid.is_some());
    assert_ne!(after.pid, before.pid);
    let exit = after.last_exit.expect("the previous run was stopped");
    assert!(exit.requested, "{exit:?}");
    assert_eq!(exit.class, ExitClass::Clean);

    within(fx.handle.shutdown()).await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn saves_only_while_zebrad_is_stopped() {
    let fx = fixture("ok");
    assert_eq!(apply(&fx).await, Ok(ApplyOutcome::Applied));
    assert_new_file_kept(&fx);
    let status = fx.handle.status();
    assert_eq!(status.phase, Phase::Stopped);
    assert_eq!(status.pid, None, "nothing was started");

    // The saved file is what the next start uses.
    within(fx.handle.start()).await.expect("start");
    assert_eq!(fx.handle.status().phase, Phase::Running);
    within(fx.handle.shutdown()).await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_zebrad_gets_the_file_for_its_next_start() {
    let fx = fixture("config-error");
    let err = within(fx.handle.start())
        .await
        .expect_err("fake-zebrad refuses every file in this mode");
    assert!(
        matches!(err, SupervisorError::ExitedDuringStart(_)),
        "{err}"
    );
    let failed = Phase::Failed {
        class: ExitClass::ConfigError,
    };
    assert_eq!(fx.handle.status().phase, failed);

    // Failed is settled: the file is saved and zebrad is left alone.
    assert_eq!(apply(&fx).await, Ok(ApplyOutcome::Applied));
    assert_new_file_kept(&fx);
    assert_eq!(fx.handle.status().phase, failed);
    within(fx.handle.shutdown()).await.expect("shutdown");
}
