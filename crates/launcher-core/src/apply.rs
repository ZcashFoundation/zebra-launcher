//! Saving an edited `zebrad.toml` and restarting zebrad with it: the
//! orchestration behind the `apply_config` command.
//!
//! [`config::begin_apply_with_managed`] writes the file with a rollback
//! marker; this module decides what happens next. When zebrad is not active
//! the file is committed and waits for the next start. Otherwise zebrad is
//! stopped and started with the new file: a start that fails as a
//! configuration error restores the previous file and starts zebrad again
//! with it; any other outcome keeps the new file, since zebrad got past
//! loading it (or is still starting when the wait runs out).
//!
//! The supervisor is reached through [`ApplyNode`], so the flow is tested
//! here against a scripted node (every branch, the rollback ones included)
//! and in `tests/apply.rs` against `fake-zebrad`.

use std::{fmt::Display, future::Future, time::Duration};

use launcher_types::{ApplyError, ApplyErrorKind, ApplyOutcome, ExitClass, Phase};
use tokio::task::{JoinError, spawn_blocking};

use crate::{
    config::{self, ConfigEditError, ManagedKeys},
    paths::Paths,
    supervisor::{SupervisorError, SupervisorHandle},
};

/// What [`apply_and_restart`] needs from the zebrad supervisor. Implemented
/// by [`SupervisorHandle`]; the tests script one.
pub trait ApplyNode: Sync {
    /// The current phase.
    fn phase(&self) -> Phase;
    /// Stops zebrad and waits until it exited. Does nothing when it is not
    /// running.
    fn stop(&self) -> impl Future<Output = Result<(), SupervisorError>> + Send;
    /// Starts zebrad and waits until it accepted its configuration.
    fn start(&self) -> impl Future<Output = Result<(), SupervisorError>> + Send;
}

impl ApplyNode for SupervisorHandle {
    fn phase(&self) -> Phase {
        self.status().phase
    }

    async fn stop(&self) -> Result<(), SupervisorError> {
        SupervisorHandle::stop(self).await
    }

    async fn start(&self) -> Result<(), SupervisorError> {
        SupervisorHandle::start(self).await
    }
}

fn apply_error(kind: ApplyErrorKind, message: impl Display) -> ApplyError {
    ApplyError {
        kind,
        message: message.to_string(),
    }
}

/// A file operation failed: [`ApplyErrorKind::Io`].
fn io_error(err: impl Display) -> ApplyError {
    apply_error(ApplyErrorKind::Io, err)
}

/// The blocking task did not finish (it panicked, or the runtime is shutting
/// down): a launcher problem, not the file's.
fn join_error(err: JoinError) -> ApplyError {
    apply_error(
        ApplyErrorKind::Internal,
        format!("the launcher could not finish the configuration change: {err}"),
    )
}

/// The supervisor refused to stop zebrad: it is gone because the launcher
/// is exiting.
fn stop_error(err: SupervisorError) -> ApplyError {
    match err {
        SupervisorError::Closed => apply_error(
            ApplyErrorKind::Closed,
            "the launcher is shutting down; the configuration change was undone",
        ),
        other => apply_error(ApplyErrorKind::Internal, other),
    }
}

fn edit_error(err: ConfigEditError) -> ApplyError {
    let kind = match &err {
        ConfigEditError::InvalidToml(_) => ApplyErrorKind::InvalidToml,
        ConfigEditError::Forbidden { .. } => ApplyErrorKind::Forbidden,
        ConfigEditError::Conflict => ApplyErrorKind::Conflict,
        ConfigEditError::ApplyInProgress => ApplyErrorKind::ApplyInProgress,
        ConfigEditError::Io(_) => ApplyErrorKind::Io,
    };
    apply_error(kind, err)
}

/// Runs blocking filesystem work off the async runtime.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, ApplyError> {
    spawn_blocking(f).await.map_err(join_error)
}

/// Saves `text` as `zebrad.toml` and, if zebrad was running, restarts it
/// with the new file.
///
/// `token` is the one `config::read` returned. The launcher-managed `keys`
/// are set to the launcher's values before the file is checked and written.
/// When zebrad is not running (stopped, or failed and waiting for the user)
/// the file is saved and the outcome is [`ApplyOutcome::Applied`]; zebrad
/// reads it on its next start. Otherwise zebrad is restarted, waiting up to
/// `start_timeout` for it to accept the file: if it refuses the file as a
/// configuration error, the previous file is restored, zebrad is started
/// with it, and the outcome is [`ApplyOutcome::RolledBack`] with zebrad's
/// message. Any other start failure keeps the new file (zebrad got past
/// loading it) and shows up in the status. Rejections leave the file
/// unchanged.
///
/// The caller keeps zebrad's data from changing underneath the restart
/// (the Tauri app holds its start permit across the call).
pub async fn apply_and_restart<N: ApplyNode>(
    paths: &Paths,
    node: &N,
    text: String,
    token: String,
    keys: &ManagedKeys,
    start_timeout: Duration,
) -> Result<ApplyOutcome, ApplyError> {
    let pending = {
        let (paths, keys) = (paths.clone(), keys.clone());
        blocking(move || config::begin_apply_with_managed(&paths, &text, &token, &keys))
            .await?
            .map_err(edit_error)?
    };

    // A stopping zebrad is left stopped: the file waits for the next start.
    if !node.phase().is_active() {
        blocking(move || pending.commit())
            .await?
            .map_err(io_error)?;
        tracing::info!("saved the new configuration; zebrad uses it on its next start");
        return Ok(ApplyOutcome::Applied);
    }

    if let Err(err) = node.stop().await {
        // The launcher is exiting. Restore the file now; if that fails, the
        // pending marker makes the next launch do it.
        let _ = blocking(move || pending.rollback()).await;
        return Err(stop_error(err));
    }

    match tokio::time::timeout(start_timeout, node.start()).await {
        Ok(Err(SupervisorError::ExitedDuringStart(exit)))
            if exit.class == ExitClass::ConfigError =>
        {
            let error = exit
                .detail
                .unwrap_or_else(|| "zebrad could not load the configuration".to_string());
            tracing::warn!(%error, "zebrad refused the new configuration; rolling back");
            blocking(move || pending.rollback())
                .await?
                .map_err(|err| {
                    io_error(format!(
                        "zebrad refused the new configuration ({error}), and the previous one could not be restored: {err}"
                    ))
                })?;
            match tokio::time::timeout(start_timeout, node.start()).await {
                Ok(Ok(())) => {}
                Ok(Err(err)) => {
                    tracing::warn!(%err, "zebrad did not start with the restored configuration")
                }
                Err(_) => {
                    tracing::warn!("zebrad is still starting with the restored configuration")
                }
            }
            Ok(ApplyOutcome::RolledBack { error })
        }
        started => {
            match started {
                Ok(Ok(())) => tracing::info!("zebrad started with the new configuration"),
                Ok(Err(err)) => {
                    tracing::warn!(%err, "zebrad accepted the new configuration but did not start")
                }
                Err(_) => tracing::info!("zebrad is still starting with the new configuration"),
            }
            blocking(move || pending.commit())
                .await?
                .map_err(io_error)?;
            Ok(ApplyOutcome::Applied)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::atomic::{AtomicUsize, Ordering},
        time::SystemTime,
    };

    use launcher_types::{ConfigDocument, ExitInfo, Network};
    use tokio::sync::watch;

    use super::*;

    const TIMEOUT: Duration = Duration::from_secs(5);

    /// What the scripted node does on each `start`, in order; `Running`
    /// once the script is used up.
    #[derive(Clone, Copy, Debug)]
    enum StartOutcome {
        Running,
        ConfigError,
        Crash,
        Hang,
    }

    /// A supervisor stand-in: the phase lives in a `watch` channel, the
    /// scripted start outcomes are read by an atomic index. No locks.
    struct ScriptedNode {
        phase: watch::Sender<Phase>,
        starts: Vec<StartOutcome>,
        started: AtomicUsize,
        stopped: AtomicUsize,
        /// `stop` fails as if the supervisor had shut down.
        stop_closed: bool,
        /// Run before every start, so a test can break the files in between.
        before_start: Option<Box<dyn Fn() + Send + Sync>>,
    }

    impl ScriptedNode {
        fn new(phase: Phase, starts: Vec<StartOutcome>) -> Self {
            Self {
                phase: watch::Sender::new(phase),
                starts,
                started: AtomicUsize::new(0),
                stopped: AtomicUsize::new(0),
                stop_closed: false,
                before_start: None,
            }
        }

        fn starts(&self) -> usize {
            self.started.load(Ordering::SeqCst)
        }

        fn stops(&self) -> usize {
            self.stopped.load(Ordering::SeqCst)
        }
    }

    fn exit(class: ExitClass, detail: Option<&str>) -> SupervisorError {
        SupervisorError::ExitedDuringStart(ExitInfo {
            code: Some(1),
            class,
            detail: detail.map(str::to_string),
            at: SystemTime::now(),
            requested: false,
        })
    }

    impl ApplyNode for ScriptedNode {
        fn phase(&self) -> Phase {
            self.phase.borrow().clone()
        }

        async fn stop(&self) -> Result<(), SupervisorError> {
            self.stopped.fetch_add(1, Ordering::SeqCst);
            if self.stop_closed {
                return Err(SupervisorError::Closed);
            }
            self.phase.send_replace(Phase::Stopped);
            Ok(())
        }

        async fn start(&self) -> Result<(), SupervisorError> {
            if let Some(hook) = &self.before_start {
                hook();
            }
            let index = self.started.fetch_add(1, Ordering::SeqCst);
            match self
                .starts
                .get(index)
                .copied()
                .unwrap_or(StartOutcome::Running)
            {
                StartOutcome::Running => {
                    self.phase.send_replace(Phase::Running);
                    Ok(())
                }
                StartOutcome::ConfigError => {
                    self.phase.send_replace(Phase::Failed {
                        class: ExitClass::ConfigError,
                    });
                    Err(exit(
                        ExitClass::ConfigError,
                        Some("Configuration error: unknown field `bogus`"),
                    ))
                }
                StartOutcome::Crash => {
                    self.phase.send_replace(Phase::Backoff {
                        until: SystemTime::now(),
                        crashes: 1,
                    });
                    Err(exit(ExitClass::Crash, None))
                }
                StartOutcome::Hang => {
                    self.phase.send_replace(Phase::Starting);
                    std::future::pending().await
                }
            }
        }
    }

    /// A managed `zebrad.toml` in a temporary layout, with what `read` returns.
    fn setup() -> (tempfile::TempDir, Paths, ManagedKeys, ConfigDocument) {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::under(dir.path());
        paths.ensure_dirs().expect("dirs");
        let keys = ManagedKeys::defaults(Network::Mainnet, &paths, paths.default_state_dir());
        config::ensure(&paths, &keys).expect("ensure");
        let original = config::read(&paths).expect("read");
        (dir, paths, keys, original)
    }

    fn new_text(original: &ConfigDocument) -> String {
        format!("{}\n[mempool]\ntx_cost_limit = 1000\n", original.text)
    }

    async fn apply(
        paths: &Paths,
        node: &ScriptedNode,
        keys: &ManagedKeys,
        original: &ConfigDocument,
    ) -> Result<ApplyOutcome, ApplyError> {
        apply_and_restart(
            paths,
            node,
            new_text(original),
            original.token.clone(),
            keys,
            TIMEOUT,
        )
        .await
    }

    fn file(paths: &Paths) -> String {
        fs::read_to_string(paths.zebrad_config()).expect("read zebrad.toml")
    }

    fn assert_new_file_kept(paths: &Paths, original: &ConfigDocument) {
        assert_eq!(file(paths), new_text(original));
        assert!(!paths.pending_apply_marker().exists(), "marker removed");
        assert_eq!(
            fs::read_to_string(paths.zebrad_config_backup()).expect("backup"),
            original.text
        );
    }

    fn assert_restored(paths: &Paths, original: &ConfigDocument) {
        assert_eq!(file(paths), original.text);
        assert!(!paths.pending_apply_marker().exists(), "marker removed");
    }

    #[tokio::test]
    async fn saves_without_a_restart_while_zebrad_is_settled() {
        let (_dir, paths, keys, original) = setup();
        for phase in [
            Phase::Stopped,
            Phase::Failed {
                class: ExitClass::Crash,
            },
            Phase::Stopping,
        ] {
            let (_dir, paths, keys, original) = setup();
            let node = ScriptedNode::new(phase.clone(), vec![]);
            let outcome = apply(&paths, &node, &keys, &original).await;
            assert_eq!(outcome, Ok(ApplyOutcome::Applied), "{phase:?}");
            assert_eq!((node.stops(), node.starts()), (0, 0), "{phase:?}");
            assert_eq!(node.phase(), phase);
            assert_new_file_kept(&paths, &original);
        }
        // The token is checked first: the saved file has a new one.
        let node = ScriptedNode::new(Phase::Stopped, vec![]);
        apply(&paths, &node, &keys, &original)
            .await
            .expect("applied");
        let err = apply(&paths, &node, &keys, &original)
            .await
            .expect_err("stale token");
        assert_eq!(err.kind, ApplyErrorKind::Conflict);
    }

    #[tokio::test]
    async fn restarts_an_active_zebrad_and_keeps_the_new_file() {
        for phase in [
            Phase::Running,
            Phase::Starting,
            Phase::Backoff {
                until: SystemTime::now(),
                crashes: 2,
            },
        ] {
            let (_dir, paths, keys, original) = setup();
            let node = ScriptedNode::new(phase.clone(), vec![StartOutcome::Running]);
            let outcome = apply(&paths, &node, &keys, &original).await;
            assert_eq!(outcome, Ok(ApplyOutcome::Applied), "{phase:?}");
            assert_eq!((node.stops(), node.starts()), (1, 1), "{phase:?}");
            assert_eq!(node.phase(), Phase::Running);
            assert_new_file_kept(&paths, &original);
        }
    }

    #[tokio::test]
    async fn rolls_back_when_zebrad_refuses_the_file_and_restarts_the_old_one() {
        let (_dir, paths, keys, original) = setup();
        let node = ScriptedNode::new(
            Phase::Running,
            vec![StartOutcome::ConfigError, StartOutcome::Running],
        );
        let outcome = apply(&paths, &node, &keys, &original).await;
        assert_eq!(
            outcome,
            Ok(ApplyOutcome::RolledBack {
                error: "Configuration error: unknown field `bogus`".into()
            })
        );
        assert_eq!((node.stops(), node.starts()), (1, 2));
        assert_eq!(node.phase(), Phase::Running);
        assert_restored(&paths, &original);
    }

    #[tokio::test]
    async fn rollback_is_reported_even_when_the_restored_start_fails() {
        let (_dir, paths, keys, original) = setup();
        let node = ScriptedNode::new(
            Phase::Running,
            vec![StartOutcome::ConfigError, StartOutcome::ConfigError],
        );
        let outcome = apply(&paths, &node, &keys, &original).await;
        assert!(matches!(outcome, Ok(ApplyOutcome::RolledBack { .. })));
        assert_eq!(node.starts(), 2);
        assert_restored(&paths, &original);

        // A rollback whose restart hangs is reported too, once the wait runs out.
        let (_dir, paths, keys, original) = setup();
        let node = ScriptedNode::new(
            Phase::Running,
            vec![StartOutcome::ConfigError, StartOutcome::Hang],
        );
        let outcome = apply_and_restart(
            &paths,
            &node,
            new_text(&original),
            original.token.clone(),
            &keys,
            Duration::from_millis(50),
        )
        .await;
        assert!(matches!(outcome, Ok(ApplyOutcome::RolledBack { .. })));
        assert_restored(&paths, &original);
    }

    #[tokio::test]
    async fn a_rollback_that_cannot_restore_fails_and_keeps_the_marker() {
        let (_dir, paths, keys, original) = setup();
        let mut node = ScriptedNode::new(Phase::Running, vec![StartOutcome::ConfigError]);
        // The backup disappears before zebrad refuses the file.
        let backup = paths.zebrad_config_backup();
        node.before_start = Some(Box::new(move || {
            let _ = fs::remove_file(&backup);
        }));
        let err = apply(&paths, &node, &keys, &original)
            .await
            .expect_err("the rollback fails");
        assert_eq!(err.kind, ApplyErrorKind::Io);
        assert!(err.message.contains("unknown field `bogus`"), "{err}");
        assert!(err.message.contains("could not be restored"), "{err}");
        // Nothing was committed: the new file is still there, and the marker
        // makes the next launch retry the rollback. No second start either.
        assert_eq!(file(&paths), new_text(&original));
        assert!(paths.pending_apply_marker().exists());
        assert_eq!(node.starts(), 1);
    }

    #[tokio::test]
    async fn other_start_failures_keep_the_new_file() {
        let (_dir, paths, keys, original) = setup();
        let node = ScriptedNode::new(Phase::Running, vec![StartOutcome::Crash]);
        let outcome = apply(&paths, &node, &keys, &original).await;
        assert_eq!(outcome, Ok(ApplyOutcome::Applied));
        assert_eq!(node.starts(), 1);
        assert_new_file_kept(&paths, &original);
    }

    #[tokio::test]
    async fn a_start_still_pending_at_the_timeout_keeps_the_new_file() {
        let (_dir, paths, keys, original) = setup();
        let node = ScriptedNode::new(Phase::Running, vec![StartOutcome::Hang]);
        let outcome = apply_and_restart(
            &paths,
            &node,
            new_text(&original),
            original.token.clone(),
            &keys,
            Duration::from_millis(50),
        )
        .await;
        assert_eq!(outcome, Ok(ApplyOutcome::Applied));
        assert_eq!(node.phase(), Phase::Starting);
        assert_new_file_kept(&paths, &original);
    }

    #[tokio::test]
    async fn a_closed_supervisor_undoes_the_change_and_says_so() {
        let (_dir, paths, keys, original) = setup();
        let mut node = ScriptedNode::new(Phase::Running, vec![]);
        node.stop_closed = true;
        let err = apply(&paths, &node, &keys, &original)
            .await
            .expect_err("closed");
        assert_eq!(err.kind, ApplyErrorKind::Closed);
        assert_eq!(node.starts(), 0);
        assert_restored(&paths, &original);
    }

    #[tokio::test]
    async fn rejections_leave_the_file_and_zebrad_alone() {
        let (_dir, paths, keys, original) = setup();
        let node = ScriptedNode::new(Phase::Running, vec![]);
        let cases = [
            (
                "[mempool".to_string(),
                original.token.clone(),
                ApplyErrorKind::InvalidToml,
            ),
            (
                new_text(&original),
                "stale".to_string(),
                ApplyErrorKind::Conflict,
            ),
            // A listener the launcher does not manage (a managed one, such as
            // `rpc.listen_addr`, is overwritten with the launcher's value).
            (
                format!(
                    "{}\n[health]\nlisten_addr = \"0.0.0.0:8080\"\n",
                    original.text
                ),
                original.token.clone(),
                ApplyErrorKind::Forbidden,
            ),
        ];
        for (text, token, kind) in cases {
            let err = apply_and_restart(&paths, &node, text, token, &keys, TIMEOUT)
                .await
                .expect_err("rejected");
            assert_eq!(err.kind, kind, "{err}");
        }
        assert_eq!((node.stops(), node.starts()), (0, 0));
        assert_eq!(file(&paths), original.text);
        assert!(!paths.pending_apply_marker().exists());
        assert!(!paths.zebrad_config_backup().exists());
    }
}
