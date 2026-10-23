//! The IPC commands the UI calls through `window.__TAURI__.core.invoke`.
//!
//! Argument names are camelCase on the JavaScript side, as Tauri converts
//! them; payloads are the `launcher-types` structs. Errors are strings,
//! except for `apply_config`, which rejects with an [`ApplyError`].

use std::{fmt::Display, path::Path, sync::atomic::Ordering, time::Duration};

use launcher_core::{
    config::{self, ConfigEditError},
    supervisor::{StartOptions, SupervisorError},
};
use launcher_types::{
    ApplyError, ApplyErrorKind, ApplyOutcome, ConfigDocument, ExitClass, LogLine, NodeStats,
    Settings, Status,
};
use tauri::{AppHandle, Runtime, State, ipc::Channel};
use tauri_plugin_opener::OpenerExt;

use crate::{
    app_state::{AppState, managed_keys},
    bridge,
};

/// How long `apply_config` waits for zebrad to accept a configuration.
/// zebrad reports a bad file within a second; a start still pending after
/// this has accepted it.
const APPLY_START_TIMEOUT: Duration = Duration::from_secs(20);

/// The page `open_path("releases_page")` opens.
pub const RELEASES_URL: &str = "https://github.com/ZcashFoundation/zebra-launcher/releases";

fn message(err: impl Display) -> String {
    err.to_string()
}

/// Runs blocking filesystem work off the async runtime.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(message)
}

/// The supervisor's current status.
#[tauri::command]
pub fn get_status(state: State<'_, AppState>) -> Status {
    state.supervisor.status()
}

/// Streams zebrad's output to `channel`: first the retained history, then
/// live lines in batches every 100 ms. A newer subscription ends this one.
#[tauri::command]
pub async fn subscribe_logs(
    state: State<'_, AppState>,
    channel: Channel<Vec<LogLine>>,
) -> Result<(), String> {
    let mine = state.log_generation.fetch_add(1, Ordering::SeqCst) + 1;
    let subscription = state.supervisor.subscribe().await.map_err(message)?;
    bridge::forward_logs(subscription, channel, state.log_generation.clone(), mine);
    Ok(())
}

/// The retained log lines with `seq >= since`, oldest first.
#[tauri::command]
pub async fn log_history(state: State<'_, AppState>, since: u64) -> Result<Vec<LogLine>, String> {
    let lines = state
        .supervisor
        .history_since(since)
        .await
        .map_err(message)?;
    Ok(lines.iter().map(|line| LogLine::clone(line)).collect())
}

/// Starts zebrad and waits until it accepted its configuration. Fails when a
/// zebrad from an earlier launcher run is still alive (see `status.stale`);
/// `start_zebrad_killing_stale` handles that case.
#[tauri::command]
pub async fn start_zebrad(state: State<'_, AppState>) -> Result<(), String> {
    state.supervisor.start().await.map_err(message)
}

/// Terminates a zebrad left behind by an earlier launcher run, then starts.
#[tauri::command]
pub async fn start_zebrad_killing_stale(state: State<'_, AppState>) -> Result<(), String> {
    state
        .supervisor
        .start_with(StartOptions { kill_stale: true })
        .await
        .map_err(message)
}

/// Stops zebrad and waits until it exited.
#[tauri::command]
pub async fn stop_zebrad(state: State<'_, AppState>) -> Result<(), String> {
    state.supervisor.stop().await.map_err(message)
}

/// Stops zebrad, then starts it again.
#[tauri::command]
pub async fn restart_zebrad(state: State<'_, AppState>) -> Result<(), String> {
    state.supervisor.restart().await.map_err(message)
}

/// The managed `zebrad.toml` with its compare-and-swap token.
#[tauri::command]
pub async fn read_config(state: State<'_, AppState>) -> Result<ConfigDocument, String> {
    let paths = state.paths.clone();
    blocking(move || config::read(&paths))
        .await?
        .map_err(message)
}

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

/// Saves `text` as `zebrad.toml` and restarts zebrad with it.
///
/// `token` is the one `read_config` returned. If zebrad refuses the file as
/// a configuration error, the previous file is restored, zebrad is started
/// with it, and the outcome is `rolled_back` with zebrad's message. Any
/// other start failure keeps the new file (zebrad got past loading it) and
/// shows up in the status. Rejections leave the file unchanged.
#[tauri::command]
pub async fn apply_config(
    state: State<'_, AppState>,
    text: String,
    token: String,
) -> Result<ApplyOutcome, ApplyError> {
    let _permit = state
        .config_writer
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            apply_error(
                ApplyErrorKind::ApplyInProgress,
                ConfigEditError::ApplyInProgress,
            )
        })?;

    let paths = state.paths.clone();
    let pending = blocking(move || config::begin_apply(&paths, &text, &token))
        .await
        .map_err(io_error)?
        .map_err(edit_error)?;

    if let Err(err) = state.supervisor.stop().await {
        // The launcher is exiting. Restore the file now; if that fails, the
        // pending marker makes the next launch do it.
        let _ = blocking(move || pending.rollback()).await;
        return Err(io_error(err.to_string()));
    }

    match tokio::time::timeout(APPLY_START_TIMEOUT, state.supervisor.start()).await {
        Ok(Err(SupervisorError::ExitedDuringStart(exit)))
            if exit.class == ExitClass::ConfigError =>
        {
            let error = exit
                .detail
                .unwrap_or_else(|| "zebrad could not load the configuration".to_string());
            tracing::warn!(%error, "zebrad refused the new configuration; rolling back");
            blocking(move || pending.rollback())
                .await
                .map_err(io_error)?
                .map_err(|err| {
                    io_error(format!(
                        "zebrad refused the new configuration ({error}), and the previous one could not be restored: {err}"
                    ))
                })?;
            match tokio::time::timeout(APPLY_START_TIMEOUT, state.supervisor.start()).await {
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
                .await
                .map_err(io_error)?
                .map_err(io_error)?;
            Ok(ApplyOutcome::Applied)
        }
    }
}

/// The launcher settings.
#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Settings {
    state.settings.current()
}

/// Validates and saves `settings`, returning what was saved.
///
/// Managed `zebrad.toml` keys that depend on the settings (network, data
/// directory) are updated right away and take effect on the next zebrad
/// start; the stop grace period takes effect on the next launch.
#[tauri::command]
pub async fn set_settings(
    state: State<'_, AppState>,
    settings: Settings,
) -> Result<Settings, String> {
    let saved = state
        .settings
        .update(move |current| *current = settings)
        .await
        .map_err(message)?;

    // `ensure` only writes when a managed value changed.
    let _permit = state
        .config_writer
        .clone()
        .acquire_owned()
        .await
        .map_err(message)?;
    let paths = state.paths.clone();
    let keys = managed_keys(&saved, &paths);
    blocking(move || config::ensure(&paths, &keys))
        .await?
        .map_err(|err| format!("settings saved, but zebrad.toml could not be updated: {err}"))?;
    Ok(saved)
}

/// The latest node statistics, `None` while zebrad is not running or has not
/// answered yet.
#[tauri::command]
pub fn get_node_stats(state: State<'_, AppState>) -> Option<NodeStats> {
    state.node_stats.borrow().clone()
}

fn path_arg(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Opens one of a fixed set of locations with the system's default handler:
/// `config_dir`, `data_dir` (the chosen chain data directory, or the
/// launcher's data directory), `log_dir`, or `releases_page`.
#[tauri::command]
pub fn open_path<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    target: String,
) -> Result<(), String> {
    let opener = app.opener();
    let paths = &state.paths;
    let result = match target.as_str() {
        "config_dir" => opener.open_path(path_arg(paths.config_dir()), None::<&str>),
        "data_dir" => {
            let dir = match state.settings.current().data_dir {
                Some(dir) => dir,
                None => path_arg(paths.data_dir()),
            };
            opener.open_path(dir, None::<&str>)
        }
        "log_dir" => opener.open_path(path_arg(paths.log_dir()), None::<&str>),
        "releases_page" => opener.open_url(RELEASES_URL, None::<&str>),
        other => return Err(format!("cannot open {other:?}")),
    };
    result.map_err(message)
}

/// What the launcher's updater is doing.
#[tauri::command]
pub fn get_update_state(
    updater: State<'_, launcher_core::updates::UpdaterHandle>,
) -> launcher_types::UpdateState {
    updater.state()
}

/// Everything the UI shows about launcher updates: the state, the mode, the
/// stale-check warning, whether this install can update itself, and a
/// post-update health failure.
#[tauri::command]
pub fn get_update_info(
    updater: State<'_, launcher_core::updates::UpdaterHandle>,
) -> launcher_types::UpdateInfo {
    updater.info()
}

/// Checks for a launcher update now and returns the resulting state. Allowed
/// in every update mode, Off included, because the user asked.
#[tauri::command]
pub async fn check_for_update(
    updater: State<'_, launcher_core::updates::UpdaterHandle>,
) -> Result<launcher_types::UpdateState, String> {
    updater.check().await.map_err(message)
}

/// Starts installing the available update: download (if needed), signature
/// check, graceful zebrad stop, install, relaunch. Returns once it started;
/// progress arrives as `update_state` events.
#[tauri::command]
pub async fn install_update(
    updater: State<'_, launcher_core::updates::UpdaterHandle>,
) -> Result<(), String> {
    updater.install().await.map_err(message)
}

/// Cancels an update check, a download, or an automatic-install countdown.
#[tauri::command]
pub async fn cancel_update(
    updater: State<'_, launcher_core::updates::UpdaterHandle>,
) -> Result<(), String> {
    updater.cancel().await.map_err(message)
}
