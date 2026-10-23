//! Zebra Launcher: a thin Tauri 2 layer over `launcher-core`.
//!
//! Startup (in [`run`]'s setup hook): resolve the per-user directories,
//! start the launcher log, roll back an interrupted config change, load the
//! settings, bring the managed `zebrad.toml` keys up to date, start the
//! supervisor and the RPC monitor, forward their state to the UI, and start
//! zebrad if the settings say so. Quitting stops zebrad first; see [`exit`].

pub mod app_state;
pub mod bridge;
pub mod commands;
pub mod exit;
pub mod logging;
pub mod paths;
pub mod sidecar;
pub mod tray;
pub mod updater;

use std::time::Duration;

use launcher_core::{
    config,
    logfile::RotatingLogConfig,
    monitor::{self, Monitor},
    process::ZebradCommand,
    settings::SettingsStore,
    supervisor::{self, SupervisorConfig, SupervisorError},
};
use launcher_types::StaleProcessInfo;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

use crate::app_state::{AppState, managed_keys};

/// The event emitted when zebrad was to start at launch but a zebrad from
/// an earlier launcher run is still alive. The UI offers
/// `start_zebrad_killing_stale`.
pub const STALE_PROCESS_EVENT: &str = "stale_process";

/// Set to `1` to run zebrad with `--shutdown-on-stdin-close`, for testing
/// with a zebrad that has the upstream change.
const STDIN_CLOSE_ENV: &str = "LAUNCHER_STDIN_CLOSE";

/// Startup failed before the supervisor was running.
#[derive(Debug, thiserror::Error)]
enum StartupError {
    #[error(transparent)]
    Paths(#[from] paths::PathsError),
    #[error("could not load the launcher settings: {0}")]
    Settings(#[from] launcher_core::settings::SettingsError),
    #[error("could not start the zebrad supervisor: {0}")]
    Supervisor(#[from] std::io::Error),
}

/// Builds and runs the app. Returns when the app exits.
pub fn run() {
    let app = tauri::Builder::default()
        // Must be the first plugin: a second launch focuses this window and exits.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main_window(app);
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_process::init())
        // Rust-side only: the UI gets no updater permissions; see updater.rs.
        .plugin(tauri_plugin_updater::Builder::new().build())
        // Keep in sync with build.rs and capabilities/main.json.
        .invoke_handler(tauri::generate_handler![
            commands::get_status,
            commands::subscribe_logs,
            commands::log_history,
            commands::start_zebrad,
            commands::start_zebrad_killing_stale,
            commands::stop_zebrad,
            commands::restart_zebrad,
            commands::read_config,
            commands::apply_config,
            commands::get_settings,
            commands::set_settings,
            commands::get_node_stats,
            commands::open_path,
            commands::get_first_run_info,
            commands::pick_data_dir,
            commands::choose_data_dir,
            commands::import_legacy_state,
            commands::delete_node_data,
            commands::copy_diagnostics,
            commands::complete_first_run,
            commands::get_update_state,
            commands::get_update_info,
            commands::check_for_update,
            commands::install_update,
            commands::cancel_update,
        ])
        .setup(|app| {
            setup(app.handle());
            Ok(())
        })
        .build(tauri::generate_context!());
    match app {
        Ok(app) => app.run(exit::on_run_event),
        Err(err) => {
            tracing::error!(%err, "could not build the app");
            eprintln!("Zebra Launcher could not start: {err}");
            std::process::exit(1);
        }
    }
}

/// Shows, unminimises and focuses the main window: from the tray, a second
/// launch, or the macOS dock.
pub(crate) fn show_main_window<R: tauri::Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// The app state, once startup succeeded.
pub(crate) fn try_state<R: tauri::Runtime>(app: &AppHandle<R>) -> Option<AppState> {
    app.try_state::<AppState>()
        .map(|state| state.inner().clone())
}

/// Starts everything. A failure is logged, shown in an error dialog, and ends
/// the app with exit code 1 rather than panicking inside the event loop.
fn setup(app: &AppHandle) {
    match tauri::async_runtime::block_on(start(app)) {
        Ok(state) => {
            app.manage(state.clone());
            app.manage(updater::start(app, &state));
            bridge::forward_status(app.clone(), state.supervisor.watch_status());
            bridge::forward_node_stats(app.clone(), state.node_stats.clone());
            tray::install(app, &state);
            // Until the first-run flow is done the user has not chosen where
            // the chain state goes, so zebrad waits for them.
            let settings = state.settings.current();
            if settings.start_node_on_launch && settings.first_run_done {
                tauri::async_runtime::spawn(start_in_background(app.clone(), state));
            }
        }
        Err(err) => {
            tracing::error!(%err, "Zebra Launcher could not start");
            eprintln!("Zebra Launcher could not start: {err}");
            show_startup_error(app, &err);
        }
    }
}

/// Tells the user why startup failed and where the log is, then exits with
/// code 1 once the dialog is dismissed. The dialog is not modal to the event
/// loop: `setup` runs on the main thread, where a blocking dialog must not.
fn show_startup_error(app: &AppHandle, err: &StartupError) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.hide();
    }
    let log_hint = match app.path().app_log_dir() {
        Ok(dir) => format!("\n\nThe launcher log is in {}.", dir.display()),
        Err(_) => String::new(),
    };
    let handle = app.clone();
    app.dialog()
        .message(format!(
            "Zebra Launcher could not start:\n\n{err}{log_hint}"
        ))
        .title("Zebra Launcher")
        .kind(MessageDialogKind::Error)
        .buttons(MessageDialogButtons::Ok)
        .show(move |_| handle.exit(1));
}

/// The startup sequence up to a running supervisor. Runs inside the Tokio
/// runtime, which the supervisor, monitor and settings actors need.
async fn start(app: &AppHandle) -> Result<AppState, StartupError> {
    let paths = paths::resolve(app)?;
    if let Err(err) = logging::init(&paths.launcher_log()) {
        eprintln!("Zebra Launcher is running without a log file: {err}");
    }
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        config_dir = %paths.config_dir().display(),
        data_dir = %paths.data_dir().display(),
        log_dir = %paths.log_dir().display(),
        "Zebra Launcher starting"
    );

    match config::recover(&paths) {
        Ok(Some(_)) => tracing::warn!(
            "a configuration change was interrupted by a launcher exit; restored the previous zebrad.toml"
        ),
        Ok(None) => {}
        Err(err) => {
            tracing::error!(%err, "could not check for an interrupted configuration change")
        }
    }

    let settings = SettingsStore::open(&paths)?;
    let current = settings.current();
    // A broken zebrad.toml is not fatal: zebrad reports it, and the user can
    // fix it in the editor.
    if let Err(err) = config::ensure(&paths, &managed_keys(&current, &paths)) {
        tracing::error!(%err, "could not update the managed keys in zebrad.toml");
    }

    let mut command = ZebradCommand::new(sidecar::resolve(&paths), paths.zebrad_config());
    command.shutdown_on_stdin_close = std::env::var_os(STDIN_CLOSE_ENV).is_some_and(|v| v == "1");
    command.kill_on_launcher_exit = true;
    let mut supervisor_config = SupervisorConfig::new(command);
    supervisor_config.stop_grace = Duration::from_secs(current.stop_grace_secs);
    supervisor_config.pidfile = Some(paths.pidfile());
    supervisor_config.log_file = Some(RotatingLogConfig::new(paths.zebrad_log()));
    let supervisor = supervisor::spawn(supervisor_config)?;

    let node_stats = Monitor::spawn(
        supervisor.watch_status(),
        paths.rpc_cookie_file(),
        monitor::DEFAULT_INTERVAL,
    );
    Ok(AppState::new(paths, supervisor, settings, node_stats))
}

/// Starts zebrad without a UI request waiting for the result: at launch
/// when `start_node_on_launch` is set, and from the tray. A zebrad left by an
/// earlier run is reported to the UI with [`STALE_PROCESS_EVENT`].
pub(crate) async fn start_in_background(app: AppHandle, state: AppState) {
    let _permit = match state.start_permit() {
        Ok(permit) => permit,
        Err(err) => {
            tracing::warn!(%err, "zebrad was not started");
            return;
        }
    };
    match state.supervisor.start().await {
        Ok(()) => {}
        Err(SupervisorError::StaleProcess(stale)) => {
            tracing::warn!(
                pid = stale.pid,
                "a zebrad from an earlier launcher run is still running"
            );
            let info = StaleProcessInfo {
                pid: stale.pid,
                binary: stale.binary.display().to_string(),
            };
            if let Err(err) = app.emit(STALE_PROCESS_EVENT, info) {
                tracing::warn!(%err, "could not emit stale_process");
            }
        }
        Err(err) => tracing::warn!(%err, "zebrad did not start at launch"),
    }
}
