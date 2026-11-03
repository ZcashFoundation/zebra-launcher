//! The IPC commands the UI calls through `window.__TAURI__.core.invoke`.
//!
//! Argument names are camelCase on the JavaScript side, as Tauri converts
//! them; payloads are the `launcher-types` structs. Errors are strings,
//! except for `apply_config`, which rejects with an [`ApplyError`].
//!
//! A command about one node takes its id as `node`. Its work runs in a
//! tracing span with the node's id, so the launcher lines it logs join that
//! node's log stream only ([`crate::logging`]).

use std::{
    fmt::Display,
    future::Future,
    path::{Path, PathBuf},
    sync::atomic::Ordering,
    time::Duration,
};

use launcher_core::{
    apply,
    config::{self, ConfigEditError},
    diagnostics,
    firstrun::{self, LegacyLocations},
    supervisor::StartOptions,
};
use launcher_types::{
    ApplyError, ApplyErrorKind, ApplyOutcome, ConfigDocument, FirstRunInfo, ImportOutcome, LogLine,
    MachineResources, MetricsSnapshot, NodeDataDirChoice, NodeDetails, NodeDraft, NodeId, NodeInfo,
    NodeResources, NodeStats, NodeUpdate, OpenTarget, Settings, Status,
};
use tauri::{AppHandle, Runtime, State, ipc::Channel};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;
use tokio::sync::oneshot;
use tracing::Instrument;

use crate::{
    app_state::{AppState, legacy_cache_dir},
    bridge, sidecar,
};

/// How long `apply_config` waits for zebrad to accept a configuration.
/// zebrad reports a bad file within a second; a start still pending after
/// this has accepted it.
const APPLY_START_TIMEOUT: Duration = Duration::from_secs(20);

/// The page `open_path` opens for [`OpenTarget::ReleasesPage`].
pub const RELEASES_URL: &str = "https://github.com/ZcashFoundation/zebra-launcher/releases";

fn message(err: impl Display) -> String {
    err.to_string()
}

/// Runs blocking filesystem work off the async runtime, in the caller's
/// tracing span.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    let span = tracing::Span::current();
    tauri::async_runtime::spawn_blocking(move || span.in_scope(f))
        .await
        .map_err(message)
}

/// `work` in the span of `node`: what it logs goes to that node's log.
fn on_node<F: Future>(node: &NodeId, work: F) -> tracing::instrument::Instrumented<F> {
    work.instrument(tracing::info_span!("node", node = %node))
}

/// The nodes, in the list's order. Every change is also sent as the `nodes`
/// event.
#[tauri::command]
pub fn list_nodes(state: State<'_, AppState>) -> Vec<NodeInfo> {
    state.registry.infos()
}

/// Adds the node `draft` describes, stopped, and returns it: at most eight
/// nodes, zebrad only for now, a name and id no other node has, ports no
/// other node has and no other program holds, and a data folder that is not
/// another node's ([`launcher_core::nodes::plan_new_node`]).
#[tauri::command]
pub async fn add_node(state: State<'_, AppState>, draft: NodeDraft) -> Result<NodeInfo, String> {
    state.registry.add(draft).await.map_err(message)
}

/// Changes a node's name, start at launch, stop grace period or network
/// ([`launcher_core::registry::RegistryHandle::update`]) and returns it.
#[tauri::command]
pub async fn update_node(
    state: State<'_, AppState>,
    node: NodeId,
    update: NodeUpdate,
) -> Result<NodeInfo, String> {
    on_node(&node, state.registry.update(&node, update))
        .await
        .map_err(message)
}

/// Removes a node: stops it, takes it off the list, and then deletes its
/// chain state when `delete_data` is set (with the rules of
/// `delete_node_data`). Refused for the last node. Returns the bytes freed (0
/// for unknown or kept); an error once the node has left the list names what
/// could not be deleted.
#[tauri::command]
pub async fn remove_node(
    state: State<'_, AppState>,
    node: NodeId,
    delete_data: bool,
) -> Result<u64, String> {
    on_node(&node, state.registry.remove(&node, delete_data))
        .await
        .map_err(message)
}

/// The node's supervisor status.
#[tauri::command]
pub fn get_status(state: State<'_, AppState>, node: NodeId) -> Result<Status, String> {
    Ok(state.node(&node)?.supervisor.status())
}

/// Streams the node's output to `channel`: first the retained history, then
/// live lines in batches every 100 ms. A newer subscription to the same node
/// ends this one.
#[tauri::command]
pub async fn subscribe_logs(
    state: State<'_, AppState>,
    node: NodeId,
    channel: Channel<Vec<LogLine>>,
) -> Result<(), String> {
    let node = state.node(&node)?;
    let mine = node.log_generation.fetch_add(1, Ordering::SeqCst) + 1;
    let subscription = node.supervisor.subscribe().await.map_err(message)?;
    bridge::forward_logs(subscription, channel, node.log_generation.clone(), mine);
    Ok(())
}

/// The node's retained log lines with `seq >= since`, oldest first.
#[tauri::command]
pub async fn log_history(
    state: State<'_, AppState>,
    node: NodeId,
    since: u64,
) -> Result<Vec<LogLine>, String> {
    let lines = state
        .node(&node)?
        .supervisor
        .history_since(since)
        .await
        .map_err(message)?;
    Ok(lines.iter().map(|line| LogLine::clone(line)).collect())
}

/// Starts the node and waits until zebrad accepted its configuration. Fails
/// when a zebrad from an earlier launcher run is still alive (see
/// `status.stale`); `start_node_killing_stale` handles that case. Fails
/// before the first run is done ([`firstrun::start_allowed`]).
#[tauri::command]
pub async fn start_node(state: State<'_, AppState>, node: NodeId) -> Result<(), String> {
    let handle = state.node(&node)?;
    let settings = state.settings.current();
    on_node(&node, handle.start(&settings, StartOptions::default()))
        .await
        .map_err(message)
}

/// Terminates a zebrad left behind by an earlier launcher run of the node,
/// then starts it. Fails before the first run is done, without terminating
/// anything.
#[tauri::command]
pub async fn start_node_killing_stale(
    state: State<'_, AppState>,
    node: NodeId,
) -> Result<(), String> {
    let handle = state.node(&node)?;
    let settings = state.settings.current();
    on_node(
        &node,
        handle.start(&settings, StartOptions { kill_stale: true }),
    )
    .await
    .map_err(message)
}

/// Stops the node and waits until zebrad exited.
#[tauri::command]
pub async fn stop_node(state: State<'_, AppState>, node: NodeId) -> Result<(), String> {
    let handle = state.node(&node)?;
    on_node(&node, handle.supervisor.stop())
        .await
        .map_err(message)
}

/// Stops the node, then starts it again. Like `start_node`, fails before the
/// first run is done.
#[tauri::command]
pub async fn restart_node(state: State<'_, AppState>, node: NodeId) -> Result<(), String> {
    let handle = state.node(&node)?;
    let settings = state.settings.current();
    on_node(&node, handle.restart(&settings))
        .await
        .map_err(message)
}

/// The node's managed `zebrad.toml` with its compare-and-swap token.
#[tauri::command]
pub async fn read_config(
    state: State<'_, AppState>,
    node: NodeId,
) -> Result<ConfigDocument, String> {
    let paths = state.node(&node)?.paths;
    blocking(move || config::read(&paths))
        .await?
        .map_err(message)
}

/// Replaces the node's `zebrad.toml` with a fresh minimal file holding only
/// the managed keys, keeping the old file as `zebrad.toml.bak`, and returns
/// the new document. Only while the node is stopped or failed: it holds the
/// same gate as the data changes, so zebrad cannot start in the middle, and
/// refuses while a configuration change is being applied.
#[tauri::command]
pub async fn reset_config(
    state: State<'_, AppState>,
    node: NodeId,
) -> Result<ConfigDocument, String> {
    let handle = state.node(&node)?;
    let _data = handle.gates.data_change_permit()?;
    let _config = handle.gates.try_config_permit()?;
    let keys = handle.managed_keys();
    let paths = handle.paths.clone();
    on_node(&node, blocking(move || config::reset(&paths, &keys)))
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

/// Saves `text` as the node's `zebrad.toml` and, if the node was running,
/// restarts it with the new file: [`apply::apply_and_restart`], behind the
/// node's permits.
///
/// `token` is the one `read_config` returned. When the node is not running
/// (stopped, or failed and waiting for the user) the file is saved and the
/// outcome is `applied`; zebrad reads it on its next start. Otherwise zebrad
/// is restarted: if it refuses the file as a configuration error, the
/// previous file is restored, zebrad is started with it, and the outcome is
/// `rolled_back` with zebrad's message. Any other start failure keeps the
/// new file (zebrad got past loading it) and shows up in the status.
/// Rejections leave the file unchanged.
#[tauri::command]
pub async fn apply_config(
    state: State<'_, AppState>,
    node: NodeId,
    text: String,
    token: String,
) -> Result<ApplyOutcome, ApplyError> {
    let handle = state
        .node(&node)
        .map_err(|err| apply_error(ApplyErrorKind::Internal, err))?;
    let _config = handle
        .gates
        .config_writer
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            apply_error(
                ApplyErrorKind::ApplyInProgress,
                ConfigEditError::ApplyInProgress,
            )
        })?;
    // Held across the stop and the start, so the node data cannot change
    // while zebrad is briefly stopped. Taken before anything is written, so
    // a refusal leaves the file unchanged.
    let _start = handle.gates.start_permit().map_err(|_| {
        apply_error(
            ApplyErrorKind::Busy,
            "the node data is being changed; apply the configuration when that has finished",
        )
    })?;

    let keys = handle.managed_keys();
    on_node(
        &node,
        apply::apply_and_restart(
            &handle.paths,
            &handle.supervisor,
            text,
            token,
            &keys,
            APPLY_START_TIMEOUT,
        ),
    )
    .await
}

/// The launcher settings.
#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Settings {
    state.settings.current()
}

/// Validates and saves `settings`, returning what was saved. They are the
/// launcher's own (updates, appearance, the tray, Grafana); a node's settings
/// change with `update_node` and `choose_data_dir`. `first_run_done` is kept
/// as it is: only `complete_first_run` sets it.
#[tauri::command]
pub async fn set_settings(
    state: State<'_, AppState>,
    settings: Settings,
) -> Result<Settings, String> {
    state
        .settings
        .update(move |current| launcher_core::settings::apply_from_ui(current, settings))
        .await
        .map_err(message)
}

/// The node's latest statistics, `None` while it is not running or has not
/// answered yet.
#[tauri::command]
pub fn get_node_stats(
    state: State<'_, AppState>,
    node: NodeId,
) -> Result<Option<NodeStats>, String> {
    Ok(state.node(&node)?.stats.borrow().clone())
}

/// The details behind the node's statistics (peers, chain supply, the active
/// network upgrade, ...), from the same poll; `None` whenever
/// `get_node_stats` is. Every change is also sent as the `node_details`
/// event.
#[tauri::command]
pub fn get_node_details(
    state: State<'_, AppState>,
    node: NodeId,
) -> Result<Option<NodeDetails>, String> {
    Ok(state.node(&node)?.details.borrow().clone())
}

/// The latest read of the node's Prometheus metrics page, `None` while it is
/// not running, has not answered yet, or stopped answering. Every change is
/// also sent as the `metrics` event.
#[tauri::command]
pub fn get_metrics(
    state: State<'_, AppState>,
    node: NodeId,
) -> Result<Option<MetricsSnapshot>, String> {
    Ok(state.node(&node)?.metrics.borrow().clone())
}

/// The newest sample of the first node's process tree (CPU, memory, disk I/O
/// and processes), `None` while it has no run or on a platform that is not
/// measured yet. Every change is also sent as the `node_resources` event.
#[tauri::command]
pub fn get_node_resources(state: State<'_, AppState>) -> Option<NodeResources> {
    state.node_resources.borrow().clone()
}

/// The newest sample of the computer's CPU and memory, `None` before the
/// first reading and on a platform that is not measured yet. Every change is
/// also sent as the `machine_resources` event.
#[tauri::command]
pub fn get_machine_resources(state: State<'_, AppState>) -> Option<MachineResources> {
    state.machine.borrow().clone()
}

fn path_arg(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// The launcher's source repository, where the observability stack the
/// Grafana tab's setup card describes lives (`docs/observability/`).
const REPO_URL: &str = "https://github.com/ZcashFoundation/zebra-launcher";

/// Opens one of a fixed set of locations ([`OpenTarget`]) with the system's
/// default handler. A `target` outside the set is refused by Tauri before
/// this runs. The folder of a node's `zebrad.toml`, its data folder and its
/// metrics page are those of `node`, or of the first node without one.
#[tauri::command]
pub fn open_path<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    target: OpenTarget,
    node: Option<NodeId>,
) -> Result<(), String> {
    let opener = app.opener();
    let node = || match &node {
        Some(id) => state.node(id),
        None => state.first_node(),
    };
    let result = match target {
        OpenTarget::ConfigDir => {
            opener.open_path(path_arg(node()?.paths.config_dir()), None::<&str>)
        }
        OpenTarget::DataDir => opener.open_path(path_arg(&node()?.data_dir()), None::<&str>),
        OpenTarget::LogDir => opener.open_path(path_arg(state.paths.log_dir()), None::<&str>),
        OpenTarget::ReleasesPage => opener.open_url(RELEASES_URL, None::<&str>),
        OpenTarget::Repo => opener.open_url(REPO_URL, None::<&str>),
        // Validated as a loopback http URL when it was saved (`is_loopback_http_url`).
        OpenTarget::Grafana => match state.settings.current().grafana_url {
            Some(url) => opener.open_url(url, None::<&str>),
            None => return Err("no Grafana URL is set".into()),
        },
        // The address zebrad logged as opened in this run; cleared when it exits.
        OpenTarget::MetricsEndpoint => match node()?.supervisor.status().metrics_addr {
            Some(addr) => opener.open_url(format!("http://{addr}/metrics"), None::<&str>),
            None => return Err("zebrad is not running, so its metrics page is closed".into()),
        },
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
    let state_dir = state.first_node()?.state_dir();
    blocking(move || {
        let legacy = firstrun::detect_legacy(&LegacyLocations::current(), &state_dir, &sizes);
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
        .set_title("Choose where Zeal keeps the chain state")
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

/// Makes `path` the node's data folder: its chain state then lives in
/// `<path>/state`. The folder is created if needed and must be writable,
/// and may not be another node's; choosing the node's default folder
/// restores the default.
///
/// Only while the node is stopped. The managed `state.cache_dir` in its
/// `zebrad.toml` is updated right away. State in the previous location is
/// left where it is. Returns the node and the free space at `path`; warning
/// about too little space is up to the UI.
#[tauri::command]
pub async fn choose_data_dir(
    state: State<'_, AppState>,
    node: NodeId,
    path: String,
) -> Result<NodeDataDirChoice, String> {
    let (node, free_bytes) = on_node(&node, state.registry.choose_data_dir(&node, &path))
        .await
        .map_err(message)?;
    Ok(NodeDataDirChoice { node, free_bytes })
}

/// Moves the alpha.4 chain state from zebrad's default cache directory into
/// the first node's state directory. Only while that node is stopped; fails
/// without copying anything when the two are on different volumes.
#[tauri::command]
pub async fn import_legacy_state(state: State<'_, AppState>) -> Result<ImportOutcome, String> {
    let first = state.first_node()?;
    let _data = first.gates.data_change_permit()?;
    let state_dir = first.state_dir();
    on_node(
        &first.id,
        blocking(move || {
            let legacy = legacy_cache_dir()?;
            firstrun::import_legacy_state(&legacy, &state_dir).map_err(message)
        }),
    )
    .await?
}

/// Deletes the alpha.4 chain state in zebrad's default cache directory
/// (`dirs::cache_dir()/zebra/state`; the peer cache next to it stays) and
/// returns the bytes freed, from the size `get_first_run_info` measured, or 0
/// (unknown). Only while the first node is stopped; refuses when a node's
/// own state directory is inside the zebra cache directory. The UI asks for
/// confirmation first.
#[tauri::command]
pub async fn delete_legacy_state(state: State<'_, AppState>) -> Result<u64, String> {
    let first = state.first_node()?;
    let _data = first.gates.data_change_permit()?;
    let state_dirs: Vec<PathBuf> = state
        .registry
        .nodes()
        .iter()
        .map(|node| node.state_dir())
        .collect();
    let sizes = state.sizes.clone();
    blocking(move || {
        let legacy = legacy_cache_dir()?;
        let state_dirs: Vec<&Path> = state_dirs.iter().map(PathBuf::as_path).collect();
        firstrun::delete_legacy_state(&legacy, &state_dirs, &sizes).map_err(message)
    })
    .await?
}

/// Deletes the node's chain state (config, logs and the RPC cookie stay) and
/// returns the bytes freed. Only while the node is stopped; the UI asks for
/// confirmation first.
///
/// The state is not walked just to count it: the bytes come from a size
/// measured earlier this session ([`AppState::sizes`]), and are 0 when none
/// was, which the UI should show as "unknown".
#[tauri::command]
pub async fn delete_node_data(state: State<'_, AppState>, node: NodeId) -> Result<u64, String> {
    let handle = state.node(&node)?;
    let _data = handle.gates.data_change_permit()?;
    let roots = handle.data_roots();
    let dir = handle.state_dir();
    let sizes = state.sizes.clone();
    on_node(
        &node,
        blocking(move || {
            let roots: Vec<&Path> = roots.iter().map(PathBuf::as_path).collect();
            firstrun::delete_node_data(&dir, &roots, &sizes)
        }),
    )
    .await?
    .map_err(message)
}

/// The diagnostics text for a bug report ([`diagnostics::bundle`]): versions,
/// OS, the zebrad binary, the settings, the updater's records and the
/// launcher log, then for each node its spec, stop grace, status,
/// statistics, `zebrad.toml` and the last lines of its log, and last the
/// newest resource samples of the first node and the computer
/// ([`diagnostics::resources_section`]). The UI copies it to the clipboard.
#[tauri::command]
pub async fn copy_diagnostics(state: State<'_, AppState>) -> Result<String, String> {
    let paths = state.paths.clone();
    let settings = state.settings.current();
    let nodes: Vec<_> = state
        .registry
        .nodes()
        .iter()
        .map(|node| {
            (
                node.info(),
                node.paths.clone(),
                node.supervisor.status(),
                node.stats.borrow().clone(),
                node.supervisor.stop_grace(),
            )
        })
        .collect();
    let resources = state.node_resources.borrow().clone();
    let machine = state.machine.borrow().clone();
    blocking(move || {
        let sidecar = sidecar::resolve(&paths);
        let reports: Vec<diagnostics::NodeReport<'_>> = nodes
            .iter()
            .map(
                |(info, files, status, stats, stop_grace)| diagnostics::NodeReport {
                    info,
                    paths: files,
                    status,
                    stats: stats.as_ref(),
                    stop_grace: *stop_grace,
                },
            )
            .collect();
        let mut text = diagnostics::bundle(&paths, &settings, &reports, Some(&sidecar));
        text.push_str(&diagnostics::resources_section(
            resources.as_ref(),
            machine.as_ref(),
        ));
        text
    })
    .await
}

/// Records that the first-run flow is done, so it is not shown again and the
/// nodes that start at launch do from now on.
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
/// check, a graceful stop of every node, install, relaunch. Returns once it
/// started; progress arrives as `update_state` events.
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
