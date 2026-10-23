//! The IPC commands the UI calls through `window.__TAURI__.core.invoke`.
//!
//! Argument names are camelCase on the JavaScript side, as Tauri converts
//! them; payloads are the `launcher-types` structs. Errors are strings,
//! except for `apply_config`, which rejects with an [`ApplyError`].

use std::{
    fmt::Display,
    path::{Path, PathBuf},
    sync::atomic::Ordering,
    time::Duration,
};

use launcher_core::{
    config::{self, ConfigEditError},
    diagnostics,
    firstrun::{self, LegacyLocations},
    supervisor::{StartOptions, SupervisorError},
};
use launcher_types::{
    ApplyError, ApplyErrorKind, ApplyOutcome, ConfigDocument, DataDirChoice, ExitClass,
    FirstRunInfo, ImportOutcome, LogLine, NodeStats, Phase, Settings, Status,
};
use tauri::{AppHandle, Runtime, State, ipc::Channel};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;
use tokio::sync::oneshot;

use crate::{
    app_state::{AppState, managed_keys, state_dir},
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
/// `start_zebrad_killing_stale` handles that case. Fails before the first
/// run is done ([`firstrun::start_allowed`]).
#[tauri::command]
pub async fn start_zebrad(state: State<'_, AppState>) -> Result<(), String> {
    firstrun::start_allowed(&state.settings.current())?;
    let _permit = state.start_permit()?;
    state.supervisor.start().await.map_err(message)
}

/// Terminates a zebrad left behind by an earlier launcher run, then starts.
/// Fails before the first run is done, without terminating anything.
#[tauri::command]
pub async fn start_zebrad_killing_stale(state: State<'_, AppState>) -> Result<(), String> {
    firstrun::start_allowed(&state.settings.current())?;
    let _permit = state.start_permit()?;
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

/// Stops zebrad, then starts it again. Like `start_zebrad`, fails before
/// the first run is done.
#[tauri::command]
pub async fn restart_zebrad(state: State<'_, AppState>) -> Result<(), String> {
    firstrun::start_allowed(&state.settings.current())?;
    let _permit = state.start_permit()?;
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

/// Replaces `zebrad.toml` with a fresh minimal file holding only the managed
/// keys, keeping the old file as `zebrad.toml.bak`, and returns the new
/// document. Only while zebrad is stopped or failed: it holds the same gate as
/// the data changes, so zebrad cannot start in the middle, and refuses while
/// a configuration change is being applied.
#[tauri::command]
pub async fn reset_config(state: State<'_, AppState>) -> Result<ConfigDocument, String> {
    let _data = state.data_change_permit()?;
    let _config = state
        .config_writer
        .clone()
        .try_acquire_owned()
        .map_err(|_| "a configuration change is being applied; try again".to_string())?;
    let paths = state.paths.clone();
    let keys = managed_keys(&state.settings.current(), &paths);
    blocking(move || config::reset(&paths, &keys))
        .await?
        // The UI says what failed ("Could not reset the config"), so only the cause here.
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

/// Whether `apply_config` restarts zebrad: it is running, starting, or about
/// to be restarted after a crash. A stopping zebrad is left stopped.
fn was_running(phase: &Phase) -> bool {
    matches!(
        phase,
        Phase::Starting | Phase::Running | Phase::Backoff { .. }
    )
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

/// Saves `text` as `zebrad.toml` and, if zebrad was running, restarts it
/// with the new file.
///
/// `token` is the one `read_config` returned. The launcher-managed keys are
/// set to the launcher's values before the file is checked and written.
/// When zebrad is not running (stopped, or failed and waiting for the user)
/// the file is saved and the outcome is `applied`; zebrad reads it on its
/// next start. Otherwise zebrad is restarted: if it refuses the file as a
/// configuration error, the previous file is restored, zebrad is started
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
    let keys = managed_keys(&state.settings.current(), &paths);
    let pending = blocking(move || config::begin_apply_with_managed(&paths, &text, &token, &keys))
        .await
        .map_err(io_error)?
        .map_err(edit_error)?;

    if !was_running(&state.supervisor.status().phase) {
        blocking(move || pending.commit())
            .await
            .map_err(io_error)?
            .map_err(io_error)?;
        tracing::info!("saved the new configuration; zebrad uses it on its next start");
        return Ok(ApplyOutcome::Applied);
    }

    // Held across the stop and the start, so the node data cannot change
    // while zebrad is briefly stopped.
    let _start = match state.start_permit() {
        Ok(permit) => permit,
        Err(err) => {
            let _ = blocking(move || pending.rollback()).await;
            return Err(io_error(err));
        }
    };
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
/// `first_run_done` is kept as it is: only `complete_first_run` sets it.
/// `data_dir` is kept as it is too: only `choose_data_dir` changes it, after
/// checking that zebrad is stopped and the directory is writable, so a stale
/// copy of the settings in the UI cannot undo a choice.
#[tauri::command]
pub async fn set_settings(
    state: State<'_, AppState>,
    settings: Settings,
) -> Result<Settings, String> {
    let saved = state
        .settings
        .update(move |current| launcher_core::settings::apply_from_ui(current, settings))
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

/// What the first-run flow shows: whether it is due, the default data
/// directory with its free space, and any alpha.4 leftovers. The alpha.4
/// state is measured on the first call only (see [`AppState::sizes`]).
#[tauri::command]
pub async fn get_first_run_info(state: State<'_, AppState>) -> Result<FirstRunInfo, String> {
    let paths = state.paths.clone();
    let settings = state.settings.current();
    let sizes = state.sizes.clone();
    blocking(move || {
        let legacy = firstrun::detect_legacy(
            &LegacyLocations::current(),
            &state_dir(&settings, &paths),
            &sizes,
        );
        firstrun::first_run_info(&paths, &settings, legacy)
    })
    .await
}

/// The free space, in bytes, on the volume that holds `path`, or of its
/// closest existing ancestor when `path` does not exist yet. Read-only: it
/// creates nothing and saves nothing. `None` when `path` is empty or not
/// absolute, when none of it exists, or when the space cannot be read.
#[tauri::command]
pub async fn free_space(path: String) -> Result<Option<u64>, String> {
    let path = PathBuf::from(path.trim());
    if !path.is_absolute() {
        return Ok(None);
    }
    blocking(move || firstrun::free_space(&path).ok()).await
}

/// Opens the system's folder picker for the data directory. `None` when
/// the user cancelled.
#[tauri::command]
pub async fn pick_data_dir<R: Runtime>(app: AppHandle<R>) -> Result<Option<String>, String> {
    let (tx, rx) = oneshot::channel();
    app.dialog()
        .file()
        .set_title("Choose where Zebra Launcher keeps the chain state")
        .set_can_create_directories(true)
        .pick_folder(move |folder| {
            let _ = tx.send(folder);
        });
    let folder = rx.await.map_err(message)?;
    folder
        .map(|folder| {
            folder
                .into_path()
                .map(|path| path.display().to_string())
                .map_err(message)
        })
        .transpose()
}

/// Makes `path` the data directory: the chain state then lives in
/// `<path>/state`. The directory is created if needed and must be writable;
/// choosing the launcher's own data directory restores the default.
///
/// Only while zebrad is stopped. The managed `state.cache_dir` in
/// `zebrad.toml` is updated right away. State in the previous location is
/// left where it is. Returns the saved settings and the free space at `path`;
/// warning about too little space is up to the UI.
#[tauri::command]
pub async fn choose_data_dir(
    state: State<'_, AppState>,
    path: String,
) -> Result<DataDirChoice, String> {
    let _data = state.data_change_permit()?;
    let _config = state
        .config_writer
        .clone()
        .try_acquire_owned()
        .map_err(|_| "a configuration change is being applied; try again".to_string())?;

    let chosen = path.trim().to_string();
    let dir = PathBuf::from(&chosen);
    let default = state.paths.data_dir().to_path_buf();
    let (free_bytes, is_default) = blocking(move || -> Result<_, String> {
        firstrun::check_writable_dir(&dir)
            .map_err(|err| format!("cannot use {} as the data directory: {err}", dir.display()))?;
        let free = firstrun::free_space(&dir).ok();
        Ok((free, same_dir(&dir, &default)))
    })
    .await??;
    let data_dir = (!is_default).then_some(chosen);

    let saved = state
        .settings
        .update(move |settings| settings.data_dir = data_dir)
        .await
        .map_err(message)?;
    let paths = state.paths.clone();
    let keys = managed_keys(&saved, &paths);
    blocking(move || config::ensure(&paths, &keys))
        .await?
        .map_err(|err| {
            format!("the data directory was saved, but zebrad.toml could not be updated: {err}")
        })?;
    tracing::info!(data_dir = ?saved.data_dir, "data directory chosen");
    Ok(DataDirChoice {
        settings: saved,
        free_bytes,
    })
}

/// Whether two directories are the same after resolving links.
fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Moves the alpha.4 chain state from zebrad's default cache directory into
/// the current state directory. Only while zebrad is stopped; fails without
/// copying anything when the two are on different volumes.
#[tauri::command]
pub async fn import_legacy_state(state: State<'_, AppState>) -> Result<ImportOutcome, String> {
    let _data = state.data_change_permit()?;
    let paths = state.paths.clone();
    let settings = state.settings.current();
    blocking(move || {
        let legacy = LegacyLocations::current()
            .zebra_cache_dir
            .ok_or_else(|| "this system has no default cache directory".to_string())?;
        firstrun::import_legacy_state(&legacy, &state_dir(&settings, &paths)).map_err(message)
    })
    .await?
}

/// Deletes the alpha.4 chain state in zebrad's default cache directory
/// (`dirs::cache_dir()/zebra/state`; the peer cache next to it stays) and
/// returns the bytes freed, from the size `get_first_run_info` measured, or 0
/// (unknown). Only while zebrad is stopped; refuses when the launcher's own
/// state directory is inside the zebra cache directory. The UI asks for
/// confirmation first.
#[tauri::command]
pub async fn delete_legacy_state(state: State<'_, AppState>) -> Result<u64, String> {
    let _data = state.data_change_permit()?;
    let paths = state.paths.clone();
    let settings = state.settings.current();
    let sizes = state.sizes.clone();
    blocking(move || {
        let legacy = LegacyLocations::current()
            .zebra_cache_dir
            .ok_or_else(|| "this system has no default cache directory".to_string())?;
        firstrun::delete_legacy_state(&legacy, &state_dir(&settings, &paths), &sizes)
            .map_err(message)
    })
    .await?
}

/// Deletes the chain state (config, logs and the RPC cookie stay) and
/// returns the bytes freed. Only while zebrad is stopped; the UI asks for
/// confirmation first.
///
/// The state is not walked just to count it: the bytes come from a size
/// measured earlier this session ([`AppState::sizes`]), and are 0 when none
/// was, which the UI should show as "unknown".
#[tauri::command]
pub async fn delete_node_data(state: State<'_, AppState>) -> Result<u64, String> {
    let _data = state.data_change_permit()?;
    let settings = state.settings.current();
    let roots = state.data_roots(&settings);
    let dir = state_dir(&settings, &state.paths);
    let sizes = state.sizes.clone();
    blocking(move || {
        let roots: Vec<&Path> = roots.iter().map(PathBuf::as_path).collect();
        firstrun::delete_node_data(&dir, &roots, &sizes)
    })
    .await?
    .map_err(message)
}

/// The diagnostics text for a bug report: versions, OS, status, settings,
/// `zebrad.toml` and the last lines of both logs. The UI copies it to the
/// clipboard.
#[tauri::command]
pub async fn copy_diagnostics(state: State<'_, AppState>) -> Result<String, String> {
    let paths = state.paths.clone();
    let status = state.supervisor.status();
    let settings = state.settings.current();
    blocking(move || {
        let launcher = log_tail(diagnostics::latest_launcher_log(&paths.launcher_log()));
        let zebrad = log_tail(Some(paths.zebrad_log()));
        diagnostics::diagnostics_bundle(&paths, &status, &settings, &launcher, &zebrad)
    })
    .await
}

fn log_tail(path: Option<PathBuf>) -> String {
    let Some(path) = path else {
        return "(no log file yet)\n".to_string();
    };
    match diagnostics::tail_lines(&path, diagnostics::TAIL_LINES) {
        Ok(tail) => tail,
        Err(err) => format!("(could not read {}: {err})\n", path.display()),
    }
}

/// Records that the first-run flow is done, so it is not shown again and
/// zebrad starts at launch from now on (if `start_node_on_launch` is set).
#[tauri::command]
pub async fn complete_first_run(state: State<'_, AppState>) -> Result<(), String> {
    state
        .settings
        .update(|settings| settings.first_run_done = true)
        .await
        .map(|_| ())
        .map_err(message)
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
