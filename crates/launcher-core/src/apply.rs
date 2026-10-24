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

use std::{fmt::Display, time::Duration};

use launcher_types::{ApplyError, ApplyErrorKind, ApplyOutcome, ExitClass};
use tokio::task::spawn_blocking;

use crate::{
    config::{self, ConfigEditError, ManagedKeys},
    paths::Paths,
    supervisor::{SupervisorError, SupervisorHandle},
};

fn apply_error(kind: ApplyErrorKind, message: impl Display) -> ApplyError {
    ApplyError {
        kind,
        message: message.to_string(),
    }
}

fn io_error(err: impl Display) -> ApplyError {
    apply_error(ApplyErrorKind::Io, err)
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
    spawn_blocking(f).await.map_err(io_error)
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
pub async fn apply_and_restart(
    paths: &Paths,
    supervisor: &SupervisorHandle,
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
    if !supervisor.status().phase.is_active() {
        blocking(move || pending.commit())
            .await?
            .map_err(io_error)?;
        tracing::info!("saved the new configuration; zebrad uses it on its next start");
        return Ok(ApplyOutcome::Applied);
    }

    if let Err(err) = supervisor.stop().await {
        // The launcher is exiting. Restore the file now; if that fails, the
        // pending marker makes the next launch do it.
        let _ = blocking(move || pending.rollback()).await;
        return Err(io_error(err.to_string()));
    }

    match tokio::time::timeout(start_timeout, supervisor.start()).await {
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
            match tokio::time::timeout(start_timeout, supervisor.start()).await {
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
