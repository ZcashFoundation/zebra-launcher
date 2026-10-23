//! Typed wrappers around the backend commands and events. Each one goes to Tauri when the page
//! runs in the Tauri webview, and to the [mock](crate::mock) backend otherwise.

use std::fmt;

use launcher_types::{
    ApplyError, ApplyErrorKind, ApplyOutcome, ConfigDocument, LogLine, NodeStats, Settings, Status,
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

pub async fn get_status() -> ApiResult<Status> {
    if !is_tauri() {
        return Ok(wire(mock::get_status().await)?);
    }
    Ok(tauri::invoke0("get_status").await?)
}

pub async fn start_zebrad() -> ApiResult<()> {
    if !is_tauri() {
        mock::start_zebrad().await;
        return Ok(());
    }
    Ok(tauri::invoke0("start_zebrad").await?)
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

/// Calls `on_status` with every `status` event for the rest of the page's life.
pub async fn on_status(on_status: impl Fn(Status) + 'static) -> ApiResult<()> {
    if !is_tauri() {
        mock::on_status(on_status);
        return Ok(());
    }
    Ok(tauri::listen("status", on_status).await?)
}

/// Calls `on_stats` with every `node_stats` event for the rest of the page's life.
pub async fn on_node_stats(on_stats: impl Fn(Option<NodeStats>) + 'static) -> ApiResult<()> {
    if !is_tauri() {
        mock::on_node_stats(on_stats);
        return Ok(());
    }
    Ok(tauri::listen("node_stats", on_stats).await?)
}

/// Streams log batches to `on_batch` for the rest of the page's life. A batch may follow a gap
/// in `seq` when the backend dropped lines.
pub async fn subscribe_logs(on_batch: impl Fn(Vec<LogLine>) + 'static) -> ApiResult<()> {
    if !is_tauri() {
        mock::subscribe_logs(on_batch);
        return Ok(());
    }
    let channel = tauri::channel(on_batch);
    let args = js_sys::Object::new();
    js_sys::Reflect::set(&args, &JsValue::from_str("channel"), &channel)?;
    tauri::invoke_js("subscribe_logs", &args).await?;
    Ok(())
}

#[derive(Serialize)]
struct LogHistoryArgs {
    since: u64,
}

/// The buffered lines with `seq >= since`.
pub async fn log_history(since: u64) -> ApiResult<Vec<LogLine>> {
    if !is_tauri() {
        return Ok(wire(mock::log_history(since).await)?);
    }
    Ok(tauri::invoke("log_history", &LogHistoryArgs { since }).await?)
}

pub async fn read_config() -> ApiResult<ConfigDocument> {
    if !is_tauri() {
        return Ok(wire(mock::read_config().await)?);
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
