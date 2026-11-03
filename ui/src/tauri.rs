//! Hand-written bindings to the `window.__TAURI__` global that Tauri injects when
//! `app.withGlobalTauri` is on. Deliberately small, instead of depending on `tauri-sys`.
//!
//! Arguments are serialized with serde-wasm-bindgen's JSON-compatible serializer (plain objects,
//! not `Map`s), so Tauri's `JSON.stringify` of the IPC payload sees ordinary JSON.

use std::cell::Cell;

use serde::{Serialize, de::DeserializeOwned};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    /// `window.__TAURI__.core.invoke(cmd, args) -> Promise<result>`.
    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "core"], js_name = invoke, catch)]
    async fn invoke_raw(cmd: &str, args: &JsValue) -> Result<JsValue, JsValue>;

    /// `window.__TAURI__.event.listen(event, handler) -> Promise<unlisten>`.
    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "event"], js_name = listen, catch)]
    async fn listen_raw(event: &str, handler: &js_sys::Function) -> Result<JsValue, JsValue>;

    /// `window.__TAURI__.core.Channel`: passed as a command argument, the backend streams
    /// messages to its `onmessage` handler.
    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "core"])]
    pub type Channel;

    /// `new window.__TAURI__.core.Channel()`.
    #[wasm_bindgen(constructor, js_namespace = ["window", "__TAURI__", "core"])]
    fn new_channel() -> Channel;

    /// `channel.onmessage = handler`.
    #[wasm_bindgen(method, setter = onmessage)]
    fn set_onmessage(this: &Channel, handler: &js_sys::Function);
}

/// Returns true when running inside the Tauri webview, false in a plain browser (`trunk serve`).
pub fn is_tauri() -> bool {
    js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("__TAURI__"))
        .map(|value| !value.is_undefined())
        .unwrap_or(false)
}

/// Serializes `value` the way Tauri expects command arguments.
pub fn to_js<T: Serialize + ?Sized>(value: &T) -> Result<JsValue, JsValue> {
    value
        .serialize(&serde_wasm_bindgen::Serializer::json_compatible())
        .map_err(JsValue::from)
}

/// Deserializes a command result or event payload.
pub fn from_js<T: DeserializeOwned>(value: JsValue) -> Result<T, JsValue> {
    serde_wasm_bindgen::from_value(value).map_err(JsValue::from)
}

/// Invokes a command with an already-built argument object and returns the raw result.
/// The error is whatever the command rejected with.
pub async fn invoke_js(cmd: &str, args: &JsValue) -> Result<JsValue, JsValue> {
    invoke_raw(cmd, args).await
}

/// Invokes a command with serializable arguments (a struct or map with camelCase keys).
pub async fn invoke<A, R>(cmd: &str, args: &A) -> Result<R, JsValue>
where
    A: Serialize + ?Sized,
    R: DeserializeOwned,
{
    let result = invoke_raw(cmd, &to_js(args)?).await?;
    from_js(result)
}

/// Invokes a command that takes no arguments.
pub async fn invoke0<R: DeserializeOwned>(cmd: &str) -> Result<R, JsValue> {
    let result = invoke_raw(cmd, &js_sys::Object::new().into()).await?;
    from_js(result)
}

/// Handles a payload that did not deserialize: logs every failure to the console, and calls
/// `on_error` with a message naming `source` the first time, so a contract mismatch between the
/// UI and the backend is visible instead of silently freezing a view.
pub fn bad_payload(
    source: &str,
    error: &JsValue,
    reported: &Cell<bool>,
    on_error: &dyn Fn(String),
) {
    let detail = error
        .dyn_ref::<js_sys::Error>()
        .map(|error| String::from(error.message()))
        .or_else(|| error.as_string())
        .unwrap_or_else(|| format!("{error:?}"));
    leptos::logging::warn!("bad {source} payload: {detail}");
    if !reported.replace(true) {
        on_error(format!(
            "The UI could not read a {source} payload from the backend: {detail}"
        ));
    }
}

/// Listens to a backend event for the rest of the page's life, calling `handler` with each
/// deserialized payload. Payloads that do not deserialize are skipped and reported through
/// [`bad_payload`].
pub async fn listen<T, F, E>(event: &str, handler: F, on_error: E) -> Result<(), JsValue>
where
    T: DeserializeOwned + 'static,
    F: Fn(T) + 'static,
    E: Fn(String) + 'static,
{
    let source = format!("`{event}` event");
    let reported = Cell::new(false);
    let callback = Closure::<dyn Fn(JsValue)>::new(move |event: JsValue| {
        let payload = js_sys::Reflect::get(&event, &JsValue::from_str("payload"))
            .unwrap_or(JsValue::UNDEFINED);
        match from_js::<T>(payload) {
            Ok(payload) => handler(payload),
            Err(error) => bad_payload(&source, &error, &reported, &on_error),
        }
    });
    // The listeners live as long as the page, so the closure is handed to JS and never freed
    // and the returned unlisten function is dropped.
    let function: js_sys::Function = callback.into_js_value().unchecked_into();
    listen_raw(event, &function).await.map(drop)
}

/// Creates a channel whose messages are deserialized and passed to `handler`. Like
/// [`listen`], the handler lives for the rest of the page's life, and messages that do not
/// deserialize are reported through [`bad_payload`] as coming from `source`.
pub fn channel<T, F, E>(source: &str, handler: F, on_error: E) -> Channel
where
    T: DeserializeOwned + 'static,
    F: Fn(T) + 'static,
    E: Fn(String) + 'static,
{
    let channel = Channel::new_channel();
    let source = source.to_owned();
    let reported = Cell::new(false);
    let callback =
        Closure::<dyn Fn(JsValue)>::new(move |message: JsValue| match from_js::<T>(message) {
            Ok(message) => handler(message),
            Err(error) => bad_payload(&source, &error, &reported, &on_error),
        });
    let function: js_sys::Function = callback.into_js_value().unchecked_into();
    channel.set_onmessage(&function);
    channel
}
