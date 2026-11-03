//! Zeal: a thin Tauri 2 layer over `launcher-core`.
//!
//! Startup (in [`run`]'s setup hook): resolve the per-user directories,
//! start the launcher log, roll back an interrupted config change of the
//! first node, load the settings and the node list, start the node registry
//! (each node's supervisor, RPC monitor and metrics poller, with its managed
//! `zebrad.toml` keys brought up to date) and the resource samplers, forward
//! their state to the UI, and start the nodes that start at launch. Quitting
//! stops every node first; see [`exit`].

pub mod app_state;
pub mod bridge;
pub mod commands;
pub mod exit;
pub mod logging;
pub mod paths;
pub mod sidecar;
pub mod tray;
pub mod updater;

use std::{sync::Arc, time::Duration};

use launcher_core::{
    config,
    firstrun::{self, SizeCache},
    pidfile::StaleProcess,
    registry::{self, NodeHandle, RegistryConfig, StartError},
    resources::{MachineSampler, ResourceSampler},
    settings::SettingsStore,
    supervisor::{StartOptions, SupervisorError},
};
use launcher_types::{NodeEvent, NodeId, NodeInfo, StaleProcessInfo, events};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tokio::sync::watch;
use tracing::Instrument;

use crate::app_state::{AppState, ResourceWatches};

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
    #[error("could not start the nodes' supervisors: {0}")]
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
        // build.rs fails the build unless this list, its COMMANDS and the
        // allow-* permissions in capabilities/main.json agree.
        .invoke_handler(tauri::generate_handler![
            commands::list_nodes,
            commands::add_node,
            commands::update_node,
            commands::remove_node,
            commands::get_status,
            commands::subscribe_logs,
            commands::log_history,
            commands::start_node,
            commands::start_node_killing_stale,
            commands::stop_node,
            commands::restart_node,
            commands::read_config,
            commands::apply_config,
            commands::reset_config,
            commands::get_settings,
            commands::set_settings,
            commands::get_node_stats,
            commands::get_node_details,
            commands::get_metrics,
            commands::get_node_resources,
            commands::get_machine_resources,
            commands::open_path,
            commands::get_first_run_info,
            commands::pick_data_dir,
            commands::choose_data_dir,
            commands::free_space,
            commands::import_legacy_state,
            commands::delete_node_data,
            commands::delete_legacy_state,
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
            eprintln!("Zeal could not start: {err}");
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
            // One snapshot of the settings and the nodes decides who starts
            // which node at launch.
            let plan = launch_plan(&state.settings.current(), &state.registry.infos());
            app.manage(updater::start(app, &state, &plan));
            bridge::forward_nodes(app.clone(), state.registry.clone());
            bridge::forward_node_resources(app.clone(), state.node_resources.clone());
            bridge::forward_machine_resources(app.clone(), state.machine.clone());
            tray::install(app, &state);
            for id in &plan.start_now {
                if let Ok(node) = state.node(id) {
                    tauri::async_runtime::spawn(start_in_background(
                        app.clone(),
                        state.clone(),
                        node,
                    ));
                }
            }
        }
        Err(err) => {
            tracing::error!(%err, "Zeal could not start");
            eprintln!("Zeal could not start: {err}");
            show_startup_error(app, &err);
        }
    }
}

/// How long the startup error dialog may stay unanswered before the launcher
/// exits anyway, so a dialog that never appeared cannot leave a hidden
/// process behind.
const STARTUP_ERROR_DIALOG_TIMEOUT: Duration = Duration::from_secs(60);

/// Tells the user why startup failed and where the log is, then exits with
/// code 1 once the dialog is dismissed. The dialog is not modal to the event
/// loop: `setup` runs on the main thread, where a blocking dialog must not.
///
/// The process also exits with code 1 if the dialog plugin drops the
/// callback without calling it (the dialog could not be shown), or if no
/// answer arrives within [`STARTUP_ERROR_DIALOG_TIMEOUT`].
fn show_startup_error(app: &AppHandle, err: &StartupError) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.hide();
    }
    let log_hint = match app.path().app_log_dir() {
        Ok(dir) => format!("\n\nThe launcher log is in {}.", dir.display()),
        Err(_) => String::new(),
    };
    let (answered, answer) = std::sync::mpsc::channel::<()>();
    let handle = app.clone();
    let watchdog = std::thread::Builder::new()
        .name("startup-error-dialog".into())
        .spawn(move || {
            match answer.recv_timeout(STARTUP_ERROR_DIALOG_TIMEOUT) {
                Ok(()) => {
                    tracing::info!("startup error dialog dismissed; exiting");
                    handle.exit(1);
                    return;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    tracing::error!("the startup error dialog could not be shown; exiting");
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => tracing::error!(
                    secs = STARTUP_ERROR_DIALOG_TIMEOUT.as_secs(),
                    "the startup error dialog got no answer; exiting"
                ),
            }
            // The event loop may be what keeps the dialog from showing.
            std::process::exit(1);
        });
    if let Err(err) = watchdog {
        tracing::error!(%err, "could not watch the startup error dialog; exiting");
        std::process::exit(1);
    }
    app.dialog()
        .message(format!("Zeal could not start:\n\n{err}{log_hint}"))
        .title("Zeal")
        .kind(MessageDialogKind::Error)
        .buttons(MessageDialogButtons::Ok)
        .show(move |_| {
            let _ = answered.send(());
        });
}

/// The startup sequence up to a running registry. Runs inside the Tokio
/// runtime, which the registry, its nodes' actors and the settings actor
/// need.
async fn start(app: &AppHandle) -> Result<AppState, StartupError> {
    let paths = paths::resolve(app)?;
    let notes = match logging::init(&paths.launcher_log()) {
        Ok(notes) => Some(notes),
        Err(err) => {
            eprintln!("Zeal is running without a log file: {err}");
            None
        }
    };
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        config_dir = %paths.config_dir().display(),
        data_dir = %paths.data_dir().display(),
        log_dir = %paths.log_dir().display(),
        "Zeal starting"
    );

    // Before the settings are read: migrating a single-node launcher's
    // settings reads the P2P port from the first node's zebrad.toml. The
    // registry checks every node again.
    match config::recover(&paths.launcher_node()) {
        Ok(Some(_)) => tracing::warn!(
            "a configuration change was interrupted by a launcher exit; restored the previous zebrad.toml"
        ),
        Ok(None) => {}
        Err(err) => {
            tracing::error!(%err, "could not check for an interrupted configuration change")
        }
    }

    let settings = SettingsStore::open(&paths)?;
    let sizes = Arc::new(SizeCache::default());
    let stdin_close = std::env::var_os(STDIN_CLOSE_ENV).is_some_and(|v| v == "1");
    let mut config = RegistryConfig::new(paths.clone(), sidecar::resolve(&paths));
    config.customize = Some(Arc::new(move |_node, supervisor| {
        supervisor.command.shutdown_on_stdin_close = stdin_close;
    }));
    config.sizes = sizes.clone();
    let registry = registry::spawn(config, settings.clone())?;
    // From here on the launcher's own events show in the Logs tab too.
    if let Some(notes) = notes {
        logging::forward_notes(notes, registry.clone());
    }
    // One process tree is sampled, the first node's, as the one node's was.
    let resources = ResourceWatches {
        node: match registry.first() {
            Some(node) => ResourceSampler::spawn(node.supervisor.watch_probe()),
            None => watch::channel(None).1,
        },
        machine: MachineSampler::spawn(),
    };
    Ok(AppState::new(paths, registry, settings, sizes, resources))
}

/// Who starts which node at launch. Decided once, from one snapshot of the
/// settings and the nodes, so the two paths that can start a node cannot
/// read different ones.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LaunchPlan {
    /// The nodes `setup` starts right away: those that start at launch,
    /// once the first run is done. Until then the user has not chosen where
    /// the chain state goes, so every node waits for them.
    pub start_now: Vec<NodeId>,
}

impl LaunchPlan {
    /// Whether the post-update health gate may start `node` when the update
    /// stopped it: only when the launcher does not start it anyway, so a
    /// node is never started twice.
    pub fn gate_may_start(&self, node: &NodeId) -> bool {
        !self.start_now.contains(node)
    }
}

/// The [`LaunchPlan`] for `settings` and `nodes`.
pub(crate) fn launch_plan(settings: &launcher_types::Settings, nodes: &[NodeInfo]) -> LaunchPlan {
    let allowed = firstrun::start_allowed(settings).is_ok();
    LaunchPlan {
        start_now: nodes
            .iter()
            .filter(|node| allowed && node.autostart)
            .map(|node| node.id.clone())
            .collect(),
    }
}

/// Starts `node` the way every start without a UI request waiting for the
/// result does: not before the first run is done
/// ([`firstrun::start_allowed`]), not while the node's data is being changed
/// ([`NodeGates::start_permit`](launcher_core::gates::NodeGates::start_permit)),
/// and with a zebrad left by an earlier run reported to the UI with
/// [`events::STALE_PROCESS`]. The error is the message for the log or the
/// caller.
pub(crate) async fn start_node<R: tauri::Runtime>(
    app: &AppHandle<R>,
    state: &AppState,
    node: &NodeHandle,
) -> Result<(), String> {
    let settings = state.settings.current();
    let started = node
        .start(&settings, StartOptions::default())
        .instrument(tracing::info_span!("node", node = %node.id))
        .await;
    match started {
        Ok(()) => Ok(()),
        Err(StartError::Supervisor(SupervisorError::StaleProcess(stale))) => {
            report_stale_process(app, &node.id, &stale);
            Err(SupervisorError::StaleProcess(stale).to_string())
        }
        Err(err) => Err(err.to_string()),
    }
}

/// [`start_node`] for the places that only log a failure: at launch for
/// the nodes in [`LaunchPlan::start_now`], and from the tray.
pub(crate) async fn start_in_background(app: AppHandle, state: AppState, node: NodeHandle) {
    if let Err(err) = start_node(&app, &state, &node).await {
        tracing::warn!(node = %node.id, %err, "zebrad was not started");
    }
}

/// Tells the UI with [`events::STALE_PROCESS`] that a zebrad from an earlier
/// launcher run of `node` is still alive.
pub(crate) fn report_stale_process<R: tauri::Runtime>(
    app: &AppHandle<R>,
    node: &NodeId,
    stale: &StaleProcess,
) {
    tracing::warn!(
        node = %node,
        pid = stale.pid,
        "a zebrad from an earlier launcher run is still running"
    );
    let info = NodeEvent {
        node: node.clone(),
        value: StaleProcessInfo::from(stale),
    };
    if let Err(err) = app.emit(events::STALE_PROCESS, info) {
        tracing::warn!(%err, "could not emit stale_process");
    }
}

#[cfg(test)]
mod tests {
    use launcher_core::{nodes::NodeSpec, paths::Paths};
    use launcher_types::{Network, Settings};

    use super::launch_plan;

    #[test]
    fn launch_plan_starts_each_node_once_at_most() {
        let paths = Paths::under("/root");
        let main = NodeSpec::first(Network::Mainnet).info(&paths);
        let manual = launcher_types::NodeInfo {
            id: launcher_types::NodeId::new("zebrad-testnet").unwrap(),
            autostart: false,
            ..NodeSpec::first(Network::Testnet).info(&paths)
        };
        let nodes = [main.clone(), manual.clone()];
        // A fresh install: every node waits for the first run, and the gate
        // could start any (none ran before an update anyway).
        let fresh = launch_plan(&Settings::default(), &nodes);
        assert!(fresh.start_now.is_empty());
        assert!(fresh.gate_may_start(&main.id));
        // The first run is done: setup starts the node that starts at
        // launch, the gate only the one that does not.
        let done = Settings {
            first_run_done: true,
            ..Settings::default()
        };
        let plan = launch_plan(&done, &nodes);
        assert_eq!(plan.start_now, std::slice::from_ref(&main.id));
        assert!(!plan.gate_may_start(&main.id));
        assert!(plan.gate_may_start(&manual.id));
    }
}
