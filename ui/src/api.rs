//! Typed wrappers around the backend commands and events. Each one goes to Tauri when the page
//! runs in the Tauri webview, and to the [mock](crate::mock) backend otherwise.

use std::{cell::Cell, fmt};

use launcher_types::{
    ApplyError, ApplyErrorKind, ApplyOutcome, ConfigDocument, DataDirChoice, FirstRunInfo,
    ImportOutcome, LogLine, NodeStats, Settings, StaleProcessInfo, Status, UpdateHealthFailure,
    UpdateInfo, UpdateState, events,
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

pub async fn get_status() -> ApiResult<Status> {
    if !is_tauri() {
        return Ok(wire(mock::get_status().await)?);
    }
    Ok(tauri::invoke0("get_status").await?)
}

/// Fails when a zebrad from an earlier launcher run is still alive (`status.stale`).
pub async fn start_zebrad() -> ApiResult<()> {
    if !is_tauri() {
        return mock::start_zebrad().await.map_err(ApiError);
    }
    Ok(tauri::invoke0("start_zebrad").await?)
}

/// Terminates the zebrad left behind by an earlier launcher run, then starts.
pub async fn start_zebrad_killing_stale() -> ApiResult<()> {
    if !is_tauri() {
        mock::start_zebrad_killing_stale().await;
        return Ok(());
    }
    Ok(tauri::invoke0("start_zebrad_killing_stale").await?)
}

pub async fn stop_zebrad() -> ApiResult<()> {
    if !is_tauri() {
        mock::stop_zebrad().await;
        return Ok(());
    }
    Ok(tauri::invoke0("stop_zebrad").await?)
}

pub async fn restart_zebrad() -> ApiResult<()> {
    if !is_tauri() {
        mock::restart_zebrad().await;
        return Ok(());
    }
    Ok(tauri::invoke0("restart_zebrad").await?)
}

/// Calls `on_status` with every `status` event for the rest of the page's life. `on_error`
/// gets a message the first time a payload does not deserialize.
pub async fn on_status(
    on_status: impl Fn(Status) + 'static,
    on_error: impl Fn(String) + 'static,
) -> ApiResult<()> {
    if !is_tauri() {
        mock::on_status(mock_handler("`status` event", on_status, on_error));
        return Ok(());
    }
    Ok(tauri::listen(events::STATUS, on_status, on_error).await?)
}

/// Calls `on_stale` when the backend reports, at launch, that a zebrad from an earlier launcher
/// run is still alive. It may fire before the UI listens, so `get_status().stale` is read too.
pub async fn on_stale_process(
    on_stale: impl Fn(StaleProcessInfo) + 'static,
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
    on_stats: impl Fn(Option<NodeStats>) + 'static,
    on_error: impl Fn(String) + 'static,
) -> ApiResult<()> {
    if !is_tauri() {
        mock::on_node_stats(mock_handler("`node_stats` event", on_stats, on_error));
        return Ok(());
    }
    Ok(tauri::listen(events::NODE_STATS, on_stats, on_error).await?)
}

/// Streams log batches to `on_batch` for the rest of the page's life. When the backend fell
/// behind, a gap marker (`LogLine::gap_marker`, with `dropped` set) takes the place of the lost
/// lines.
pub async fn subscribe_logs(
    on_batch: impl Fn(Vec<LogLine>) + 'static,
    on_error: impl Fn(String) + 'static,
) -> ApiResult<()> {
    if !is_tauri() {
        mock::subscribe_logs(mock_handler("log batch", on_batch, on_error));
        return Ok(());
    }
    let channel = tauri::channel("log batch", on_batch, on_error);
    let args = js_sys::Object::new();
    js_sys::Reflect::set(&args, &JsValue::from_str("channel"), &channel)?;
    tauri::invoke_js("subscribe_logs", &args).await?;
    Ok(())
}

pub async fn read_config() -> ApiResult<ConfigDocument> {
    if !is_tauri() {
        return Ok(wire(mock::read_config().await.map_err(ApiError)?)?);
    }
    Ok(tauri::invoke0("read_config").await?)
}

#[derive(Serialize)]
struct ApplyConfigArgs<'a> {
    text: &'a str,
    token: &'a str,
}

/// Writes the config and restarts zebrad. `Err` means nothing was written.
pub async fn apply_config(text: String, token: String) -> Result<ApplyOutcome, ApplyError> {
    if !is_tauri() {
        return match mock::apply_config(text, token).await {
            Ok(outcome) => wire(outcome).map_err(apply_error),
            Err(error) => Err(wire(error).unwrap_or_else(apply_error)),
        };
    }
    let args = ApplyConfigArgs {
        text: &text,
        token: &token,
    };
    match tauri::invoke("apply_config", &args).await {
        Ok(outcome) => Ok(outcome),
        Err(error) => Err(apply_error(error)),
    }
}

/// Reads an `ApplyError` rejection. Anything else (the IPC call itself failed, or the payload
/// did not deserialize) is reported as kind `io`, which the page shows as a plain failure.
fn apply_error(error: JsValue) -> ApplyError {
    tauri::from_js::<ApplyError>(error.clone()).unwrap_or_else(|_| ApplyError {
        kind: ApplyErrorKind::Io,
        message: ApiError::from(error).0,
    })
}

/// Replaces `zebrad.toml` with the minimal managed file, keeping the old one as
/// `zebrad.toml.bak`, and returns the new file. Only while zebrad is stopped or failed.
pub async fn reset_config() -> ApiResult<ConfigDocument> {
    if !is_tauri() {
        return Ok(wire(mock::reset_config().await.map_err(ApiError)?)?);
    }
    Ok(tauri::invoke0("reset_config").await?)
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

pub async fn get_node_stats() -> ApiResult<Option<NodeStats>> {
    if !is_tauri() {
        return Ok(wire(mock::get_node_stats().await)?);
    }
    Ok(tauri::invoke0("get_node_stats").await?)
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
    path: &'a str,
}

/// Makes `path` the data directory (only while zebrad is stopped or failed) and returns the
/// saved settings with the free space there. State in the old location stays where it is.
pub async fn choose_data_dir(path: String) -> ApiResult<DataDirChoice> {
    if !is_tauri() {
        return Ok(wire(mock::choose_data_dir(path).await.map_err(ApiError)?)?);
    }
    Ok(tauri::invoke("choose_data_dir", &ChooseDataDirArgs { path: &path }).await?)
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

/// Deletes the chain state and returns the bytes freed. Only while zebrad is stopped or
/// failed.
pub async fn delete_node_data() -> ApiResult<u64> {
    if !is_tauri() {
        return Ok(wire(mock::delete_node_data().await.map_err(ApiError)?)?);
    }
    Ok(tauri::invoke0("delete_node_data").await?)
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
struct OpenPathArgs {
    target: &'static str,
}

/// Opens one of the backend's fixed locations with the system's default handler. `target` is
/// the wire name of a `launcher_types::OpenTarget` (`config_dir`, `data_dir`, `log_dir`,
/// `releases_page`, `repo`, `grafana`); the backend refuses anything else. The string form stays
/// until the call sites take the enum.
pub async fn open_path(target: &'static str) -> ApiResult<()> {
    if !is_tauri() {
        mock::open_path(target).await;
        return Ok(());
    }
    Ok(tauri::invoke("open_path", &OpenPathArgs { target }).await?)
}
