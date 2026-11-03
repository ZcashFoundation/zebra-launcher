//! Typed wrappers around the backend commands and events. Each one goes to Tauri when the page
//! runs in the Tauri webview, and to the [mock](crate::mock) backend otherwise.

use std::{cell::Cell, fmt};

use launcher_types::{
    ApplyError, ApplyErrorKind, ApplyOutcome, ConfigDocument, FirstRunInfo, ImportOutcome, LogLine,
    MachineResources, MetricsSnapshot, NodeDataDirChoice, NodeDetails, NodeDraft, NodeEvent,
    NodeId, NodeInfo, NodeResources, NodeStats, NodeUpdate, OpenTarget, Settings, StaleProcessInfo,
    Status, UpdateHealthFailure, UpdateInfo, UpdateState, events,
};
use serde::{Serialize, de::DeserializeOwned};
use wasm_bindgen::{JsCast, JsValue};

use crate::{
    mock,
    tauri::{self, is_tauri},
};

/// A failed command, with a message for display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiError(pub String);

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<JsValue> for ApiError {
    /// Commands reject with a string, an object with a `message`, or a deserialization error.
    fn from(value: JsValue) -> Self {
        if let Some(text) = value.as_string() {
            return ApiError(text);
        }
        if let Some(error) = value.dyn_ref::<js_sys::Error>() {
            return ApiError(error.message().into());
        }
        if let Some(message) = js_sys::Reflect::get(&value, &JsValue::from_str("message"))
            .ok()
            .and_then(|message| message.as_string())
        {
            return ApiError(message);
        }
        ApiError(
            js_sys::JSON::stringify(&value)
                .ok()
                .and_then(|text| text.as_string())
                .unwrap_or_else(|| format!("{value:?}")),
        )
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

/// Sends a mock value through the same JSON (de)serialization as a Tauri payload, so the mock
/// exercises the real `launcher-types` wire shapes (`SystemTime` as
/// `{secs_since_epoch, nanos_since_epoch}`, `SocketAddr` as a string, tagged enums).
fn wire<T: Serialize + DeserializeOwned>(value: T) -> Result<T, JsValue> {
    tauri::from_js(tauri::to_js(&value)?)
}

/// Wraps a mock event handler so its payloads take the [`wire`] round trip, with failures
/// reported like those of a Tauri event.
fn mock_handler<T: Serialize + DeserializeOwned>(
    source: &'static str,
    handler: impl Fn(T) + 'static,
    on_error: impl Fn(String) + 'static,
) -> impl Fn(T) + 'static {
    let reported = Cell::new(false);
    move |value| match wire(value) {
        Ok(value) => handler(value),
        Err(error) => tauri::bad_payload(source, &error, &reported, &on_error),
    }
}

/// The arguments of a command about one node.
#[derive(Serialize)]
struct NodeArgs<'a> {
    node: &'a NodeId,
}

/// The nodes, in the backend's order.
pub async fn list_nodes() -> ApiResult<Vec<NodeInfo>> {
    if !is_tauri() {
        return Ok(wire(mock::nodes::list_nodes().await)?);
    }
    Ok(tauri::invoke0("list_nodes").await?)
}

/// Calls `on_nodes` with the node list whenever it changes (`nodes`).
pub async fn on_nodes(
    on_nodes: impl Fn(Vec<NodeInfo>) + 'static,
    on_error: impl Fn(String) + 'static,
) -> ApiResult<()> {
    if !is_tauri() {
        mock::nodes::on_nodes(mock_handler("`nodes` event", on_nodes, on_error));
        return Ok(());
    }
    Ok(tauri::listen(events::NODES, on_nodes, on_error).await?)
}

#[derive(Serialize)]
struct AddNodeArgs<'a> {
    draft: &'a NodeDraft,
}

/// Adds a node, stopped, and returns it as the backend set it up: the backend checks the
/// draft and fills in what it leaves out (the id, the ports, the data folder).
pub async fn add_node(draft: NodeDraft) -> ApiResult<NodeInfo> {
    if !is_tauri() {
        return Ok(wire(
            mock::nodes::add_node(wire(draft)?)
                .await
                .map_err(ApiError)?,
        )?);
    }
    Ok(tauri::invoke("add_node", &AddNodeArgs { draft: &draft }).await?)
}

#[derive(Serialize)]
struct UpdateNodeArgs<'a> {
    node: &'a NodeId,
    update: &'a NodeUpdate,
}

/// Changes a node's name, start at launch, stop grace period or network, and returns it.
pub async fn update_node(node: &NodeId, update: NodeUpdate) -> ApiResult<NodeInfo> {
    if !is_tauri() {
        return Ok(wire(
            mock::nodes::update_node(node, wire(update)?)
                .await
                .map_err(ApiError)?,
        )?);
    }
    Ok(tauri::invoke(
        "update_node",
        &UpdateNodeArgs {
            node,
            update: &update,
        },
    )
    .await?)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RemoveNodeArgs<'a> {
    node: &'a NodeId,
    delete_data: bool,
}

/// Stops and removes a node, deleting its chain state with `delete_data`, and returns the
/// bytes freed (0 when kept or unknown). Refused for the last node.
pub async fn remove_node(node: &NodeId, delete_data: bool) -> ApiResult<u64> {
    if !is_tauri() {
        return Ok(wire(
            mock::nodes::remove_node(node, delete_data)
                .await
                .map_err(ApiError)?,
        )?);
    }
    Ok(tauri::invoke("remove_node", &RemoveNodeArgs { node, delete_data }).await?)
}

pub async fn get_status(node: &NodeId) -> ApiResult<Status> {
    if !is_tauri() {
        return Ok(wire(
            mock::nodes::get_status(node).await.map_err(ApiError)?,
        )?);
    }
    Ok(tauri::invoke("get_status", &NodeArgs { node }).await?)
}

/// Starts a node. Fails when a zebrad from an earlier launcher run is still alive
/// (`status.stale`).
pub async fn start_node(node: &NodeId) -> ApiResult<()> {
    if !is_tauri() {
        return mock::nodes::start_node(node).await.map_err(ApiError);
    }
    Ok(tauri::invoke("start_node", &NodeArgs { node }).await?)
}

/// Terminates the zebrad left behind by an earlier launcher run of a node, then starts it.
pub async fn start_node_killing_stale(node: &NodeId) -> ApiResult<()> {
    if !is_tauri() {
        return mock::nodes::start_node_killing_stale(node)
            .await
            .map_err(ApiError);
    }
    Ok(tauri::invoke("start_node_killing_stale", &NodeArgs { node }).await?)
}

pub async fn stop_node(node: &NodeId) -> ApiResult<()> {
    if !is_tauri() {
        return mock::nodes::stop_node(node).await.map_err(ApiError);
    }
    Ok(tauri::invoke("stop_node", &NodeArgs { node }).await?)
}

pub async fn restart_node(node: &NodeId) -> ApiResult<()> {
    if !is_tauri() {
        return mock::nodes::restart_node(node).await.map_err(ApiError);
    }
    Ok(tauri::invoke("restart_node", &NodeArgs { node }).await?)
}

/// Calls `on_status` with every `status` event, of every node, for the rest of the page's
/// life. `on_error` gets a message the first time a payload does not deserialize.
pub async fn on_status(
    on_status: impl Fn(NodeEvent<Status>) + 'static,
    on_error: impl Fn(String) + 'static,
) -> ApiResult<()> {
    if !is_tauri() {
        mock::nodes::on_status(mock_handler("`status` event", on_status, on_error));
        return Ok(());
    }
    Ok(tauri::listen(events::STATUS, on_status, on_error).await?)
}

/// Calls `on_stale` when the backend reports, at launch, that a zebrad from an earlier launcher
/// run of a node is still alive. It may fire before the UI listens, so `get_status().stale` is
/// read too.
pub async fn on_stale_process(
    on_stale: impl Fn(NodeEvent<StaleProcessInfo>) + 'static,
    on_error: impl Fn(String) + 'static,
) -> ApiResult<()> {
    if !is_tauri() {
        // The mock reports a stale process through `status.stale` only.
        return Ok(());
    }
    Ok(tauri::listen(events::STALE_PROCESS, on_stale, on_error).await?)
}

/// Calls `on_stats` with every `node_stats` event for the rest of the page's life.
pub async fn on_node_stats(
    on_stats: impl Fn(NodeEvent<Option<NodeStats>>) + 'static,
    on_error: impl Fn(String) + 'static,
) -> ApiResult<()> {
    if !is_tauri() {
        mock::nodes::on_node_stats(mock_handler("`node_stats` event", on_stats, on_error));
        return Ok(());
    }
    Ok(tauri::listen(events::NODE_STATS, on_stats, on_error).await?)
}

/// Calls `on_details` with every `node_details` event: the details behind a node's statistics
/// (the peer list, the chain's supply, the best block), from the same RPC poll, `None` while it
/// does not run.
pub async fn on_node_details(
    on_details: impl Fn(NodeEvent<Option<NodeDetails>>) + 'static,
    on_error: impl Fn(String) + 'static,
) -> ApiResult<()> {
    if !is_tauri() {
        mock::nodes::on_node_details(mock_handler("`node_details` event", on_details, on_error));
        return Ok(());
    }
    Ok(tauri::listen(events::NODE_DETAILS, on_details, on_error).await?)
}

/// Calls `on_metrics` with every `metrics` event: a node's metrics page, read every 10 s while
/// it runs, `None` while it does not.
pub async fn on_metrics(
    on_metrics: impl Fn(NodeEvent<Option<MetricsSnapshot>>) + 'static,
    on_error: impl Fn(String) + 'static,
) -> ApiResult<()> {
    if !is_tauri() {
        mock::nodes::on_metrics(mock_handler("`metrics` event", on_metrics, on_error));
        return Ok(());
    }
    Ok(tauri::listen(events::METRICS, on_metrics, on_error).await?)
}

/// Calls `on_resources` with every `node_resources` event: what the first node's processes use,
/// read every 5 s from the spawn until the run ends, `None` while the node has no run and on a
/// platform the backend does not measure yet.
pub async fn on_node_resources(
    on_resources: impl Fn(Option<NodeResources>) + 'static,
    on_error: impl Fn(String) + 'static,
) -> ApiResult<()> {
    if !is_tauri() {
        mock::on_node_resources(mock_handler(
            "`node_resources` event",
            on_resources,
            on_error,
        ));
        return Ok(());
    }
    Ok(tauri::listen(events::NODE_RESOURCES, on_resources, on_error).await?)
}

/// Calls `on_machine` with every `machine_resources` event: the computer's CPU and memory, read
/// every 5 s.
pub async fn on_machine_resources(
    on_machine: impl Fn(Option<MachineResources>) + 'static,
    on_error: impl Fn(String) + 'static,
) -> ApiResult<()> {
    if !is_tauri() {
        mock::on_machine_resources(mock_handler(
            "`machine_resources` event",
            on_machine,
            on_error,
        ));
        return Ok(());
    }
    Ok(tauri::listen(events::MACHINE_RESOURCES, on_machine, on_error).await?)
}

/// Streams a node's log batches to `on_batch` for the rest of the page's life. When the
/// backend fell behind, a gap marker (`LogLine::gap_marker`, with `dropped` set) takes the
/// place of the lost lines.
pub async fn subscribe_logs(
    node: &NodeId,
    on_batch: impl Fn(Vec<LogLine>) + 'static,
    on_error: impl Fn(String) + 'static,
) -> ApiResult<()> {
    if !is_tauri() {
        mock::nodes::subscribe_logs(node, mock_handler("log batch", on_batch, on_error));
        return Ok(());
    }
    let channel = tauri::channel("log batch", on_batch, on_error);
    let args = js_sys::Object::new();
    js_sys::Reflect::set(&args, &JsValue::from_str("node"), &tauri::to_js(node)?)?;
    js_sys::Reflect::set(&args, &JsValue::from_str("channel"), &channel)?;
    tauri::invoke_js("subscribe_logs", &args).await?;
    Ok(())
}

pub async fn read_config(node: &NodeId) -> ApiResult<ConfigDocument> {
    if !is_tauri() {
        return Ok(wire(
            mock::nodes::read_config(node).await.map_err(ApiError)?,
        )?);
    }
    Ok(tauri::invoke("read_config", &NodeArgs { node }).await?)
}

#[derive(Serialize)]
struct ApplyConfigArgs<'a> {
    node: &'a NodeId,
    text: &'a str,
    token: &'a str,
}

/// Writes a node's config and restarts it. `Err` means nothing was written.
pub async fn apply_config(
    node: &NodeId,
    text: String,
    token: String,
) -> Result<ApplyOutcome, ApplyError> {
    if !is_tauri() {
        return match mock::nodes::apply_config(node, text, token).await {
            Ok(outcome) => wire(outcome).map_err(apply_error),
            Err(error) => Err(wire(error).unwrap_or_else(apply_error)),
        };
    }
    let args = ApplyConfigArgs {
        node,
        text: &text,
        token: &token,
    };
    match tauri::invoke("apply_config", &args).await {
        Ok(outcome) => Ok(outcome),
        Err(error) => Err(apply_error(error)),
    }
}

/// Reads an `ApplyError` rejection. Anything else (the IPC call itself failed, or the payload
/// did not deserialize) is reported as kind `internal`, which the page shows as a plain failure.
fn apply_error(error: JsValue) -> ApplyError {
    tauri::from_js::<ApplyError>(error.clone()).unwrap_or_else(|_| ApplyError {
        kind: ApplyErrorKind::Internal,
        message: ApiError::from(error).0,
    })
}

/// Replaces a node's `zebrad.toml` with the minimal managed file, keeping the old one as
/// `zebrad.toml.bak`, and returns the new file. Only while the node is stopped or failed.
pub async fn reset_config(node: &NodeId) -> ApiResult<ConfigDocument> {
    if !is_tauri() {
        return Ok(wire(
            mock::nodes::reset_config(node).await.map_err(ApiError)?,
        )?);
    }
    Ok(tauri::invoke("reset_config", &NodeArgs { node }).await?)
}

pub async fn get_settings() -> ApiResult<Settings> {
    if !is_tauri() {
        return Ok(wire(mock::get_settings().await)?);
    }
    Ok(tauri::invoke0("get_settings").await?)
}

#[derive(Serialize)]
struct SetSettingsArgs<'a> {
    settings: &'a Settings,
}

/// Saves the settings and returns them as stored.
pub async fn set_settings(settings: Settings) -> ApiResult<Settings> {
    if !is_tauri() {
        return Ok(wire(mock::set_settings(wire(settings)?).await)?);
    }
    Ok(tauri::invoke(
        "set_settings",
        &SetSettingsArgs {
            settings: &settings,
        },
    )
    .await?)
}

pub async fn get_node_stats(node: &NodeId) -> ApiResult<Option<NodeStats>> {
    if !is_tauri() {
        return Ok(wire(
            mock::nodes::get_node_stats(node).await.map_err(ApiError)?,
        )?);
    }
    Ok(tauri::invoke("get_node_stats", &NodeArgs { node }).await?)
}

/// The details behind a node's statistics, `None` whenever `get_node_stats` is.
pub async fn get_node_details(node: &NodeId) -> ApiResult<Option<NodeDetails>> {
    if !is_tauri() {
        return Ok(wire(
            mock::nodes::get_node_details(node)
                .await
                .map_err(ApiError)?,
        )?);
    }
    Ok(tauri::invoke("get_node_details", &NodeArgs { node }).await?)
}

/// The newest reading of a node's metrics page, if it runs and answered.
pub async fn get_metrics(node: &NodeId) -> ApiResult<Option<MetricsSnapshot>> {
    if !is_tauri() {
        return Ok(wire(
            mock::nodes::get_metrics(node).await.map_err(ApiError)?,
        )?);
    }
    Ok(tauri::invoke("get_metrics", &NodeArgs { node }).await?)
}

/// The newest sample of the first node's processes, `None` while it has no run.
pub async fn get_node_resources() -> ApiResult<Option<NodeResources>> {
    if !is_tauri() {
        return Ok(wire(mock::get_node_resources().await)?);
    }
    Ok(tauri::invoke0("get_node_resources").await?)
}

/// The newest sample of the computer's CPU and memory, `None` before the first one.
pub async fn get_machine_resources() -> ApiResult<Option<MachineResources>> {
    if !is_tauri() {
        return Ok(wire(mock::get_machine_resources().await)?);
    }
    Ok(tauri::invoke0("get_machine_resources").await?)
}

/// What the first-run flow shows: the default data directory, its free space and any alpha.4
/// leftovers. Sizing the leftovers can take a moment.
pub async fn get_first_run_info() -> ApiResult<FirstRunInfo> {
    if !is_tauri() {
        return Ok(wire(mock::get_first_run_info().await)?);
    }
    Ok(tauri::invoke0("get_first_run_info").await?)
}

/// Opens the system's folder picker. `None` when the user cancelled.
pub async fn pick_data_dir() -> ApiResult<Option<String>> {
    if !is_tauri() {
        return Ok(wire(mock::pick_data_dir().await)?);
    }
    Ok(tauri::invoke0("pick_data_dir").await?)
}

#[derive(Serialize)]
struct ChooseDataDirArgs<'a> {
    node: &'a NodeId,
    path: &'a str,
}

/// Makes `path` a node's data folder (only while it is stopped or failed) and returns the node
/// with the free space there. State in the old location stays where it is.
pub async fn choose_data_dir(node: &NodeId, path: String) -> ApiResult<NodeDataDirChoice> {
    if !is_tauri() {
        return Ok(wire(
            mock::nodes::choose_data_dir(node, path)
                .await
                .map_err(ApiError)?,
        )?);
    }
    Ok(tauri::invoke("choose_data_dir", &ChooseDataDirArgs { node, path: &path }).await?)
}

#[derive(Serialize)]
struct FreeSpaceArgs<'a> {
    path: &'a str,
}

/// The free space at `path` (or its nearest existing ancestor). Read-only: nothing is created
/// or saved. `None` when it cannot be read.
pub async fn free_space(path: String) -> ApiResult<Option<u64>> {
    if !is_tauri() {
        return Ok(wire(mock::free_space(path).await)?);
    }
    Ok(tauri::invoke("free_space", &FreeSpaceArgs { path: &path }).await?)
}

/// Moves the alpha.4 state into the current state directory: a rename on the same volume,
/// only while zebrad is stopped or failed.
pub async fn import_legacy_state() -> ApiResult<ImportOutcome> {
    if !is_tauri() {
        return Ok(wire(mock::import_legacy_state().await.map_err(ApiError)?)?);
    }
    Ok(tauri::invoke0("import_legacy_state").await?)
}

/// Deletes the alpha.4 chain state in zebrad's default cache directory and returns the bytes
/// freed (0 when unknown). Only while zebrad is stopped or failed.
pub async fn delete_legacy_state() -> ApiResult<u64> {
    if !is_tauri() {
        return Ok(wire(mock::delete_legacy_state().await.map_err(ApiError)?)?);
    }
    Ok(tauri::invoke0("delete_legacy_state").await?)
}

/// Deletes a node's chain state and returns the bytes freed. Only while it is stopped or
/// failed.
pub async fn delete_node_data(node: &NodeId) -> ApiResult<u64> {
    if !is_tauri() {
        return Ok(wire(
            mock::nodes::delete_node_data(node)
                .await
                .map_err(ApiError)?,
        )?);
    }
    Ok(tauri::invoke("delete_node_data", &NodeArgs { node }).await?)
}

/// The diagnostics text for a bug report.
pub async fn copy_diagnostics() -> ApiResult<String> {
    if !is_tauri() {
        return Ok(wire(mock::copy_diagnostics().await)?);
    }
    Ok(tauri::invoke0("copy_diagnostics").await?)
}

/// Records that the first-run flow is done; zebrad starts at launch from now on.
pub async fn complete_first_run() -> ApiResult<()> {
    if !is_tauri() {
        return Ok(wire(mock::complete_first_run().await)?);
    }
    Ok(tauri::invoke0("complete_first_run").await?)
}

/// Everything the UI shows about launcher updates.
pub async fn get_update_info() -> ApiResult<UpdateInfo> {
    if !is_tauri() {
        return Ok(wire(mock::get_update_info().await)?);
    }
    Ok(tauri::invoke0("get_update_info").await?)
}

/// Checks for a launcher update now, in every update mode (Off included), and returns the
/// resulting state.
pub async fn check_for_update() -> ApiResult<UpdateState> {
    if !is_tauri() {
        return Ok(wire(mock::check_for_update().await)?);
    }
    Ok(tauri::invoke0("check_for_update").await?)
}

/// Starts installing the available update; progress arrives as `update_state` events.
pub async fn install_update() -> ApiResult<()> {
    if !is_tauri() {
        return Ok(wire(mock::install_update().await.map_err(ApiError)?)?);
    }
    Ok(tauri::invoke0("install_update").await?)
}

/// Cancels an update check, a download, or an automatic-install countdown.
pub async fn cancel_update() -> ApiResult<()> {
    if !is_tauri() {
        return Ok(wire(mock::cancel_update().await)?);
    }
    Ok(tauri::invoke0("cancel_update").await?)
}

/// Calls `on_state` with every `update_state` event (each change of the updater's state).
pub async fn on_update_state(
    on_state: impl Fn(UpdateState) + 'static,
    on_error: impl Fn(String) + 'static,
) -> ApiResult<()> {
    if !is_tauri() {
        mock::on_update_state(mock_handler("`update_state` event", on_state, on_error));
        return Ok(());
    }
    Ok(tauri::listen(events::UPDATE_STATE, on_state, on_error).await?)
}

/// Calls `on_info` with every `update_info` event (any change of what the UI shows about
/// updates, the state included).
pub async fn on_update_info(
    on_info: impl Fn(UpdateInfo) + 'static,
    on_error: impl Fn(String) + 'static,
) -> ApiResult<()> {
    if !is_tauri() {
        mock::on_update_info(mock_handler("`update_info` event", on_info, on_error));
        return Ok(());
    }
    Ok(tauri::listen(events::UPDATE_INFO, on_info, on_error).await?)
}

/// Calls `on_failure` when the launcher did not come back healthy after an update. It may fire
/// before the UI listens, so `get_update_info().health_failure` is read too.
pub async fn on_update_health_failed(
    on_failure: impl Fn(UpdateHealthFailure) + 'static,
    on_error: impl Fn(String) + 'static,
) -> ApiResult<()> {
    if !is_tauri() {
        mock::on_update_health_failed(mock_handler(
            "`update_health_failed` event",
            on_failure,
            on_error,
        ));
        return Ok(());
    }
    Ok(tauri::listen(events::UPDATE_HEALTH_FAILED, on_failure, on_error).await?)
}

#[derive(Serialize)]
struct OpenPathArgs<'a> {
    target: OpenTarget,
    node: Option<&'a NodeId>,
}

/// Opens one of the backend's fixed locations with the system's default handler. The folder
/// of a node's `zebrad.toml`, its data folder and its metrics page are those of `node`, or of
/// the first node without one.
pub async fn open_path(target: OpenTarget, node: Option<&NodeId>) -> ApiResult<()> {
    if !is_tauri() {
        mock::open_path(target, node).await;
        return Ok(());
    }
    Ok(tauri::invoke("open_path", &OpenPathArgs { target, node }).await?)
}
