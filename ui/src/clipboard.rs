//! The system clipboard.

pub async fn copy_to_clipboard(text: &str) -> Result<(), String> {
    let window = web_sys::window().ok_or("no window")?;
    let promise = window.navigator().clipboard().write_text(text);
    wasm_bindgen_futures::JsFuture::from(promise)
        .await
        .map(drop)
        .map_err(|error| crate::api::ApiError::from(error).0)
}
