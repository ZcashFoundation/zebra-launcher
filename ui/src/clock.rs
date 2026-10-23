//! Wall-clock helpers. `SystemTime::now()` panics on `wasm32-unknown-unknown`, so the current
//! time comes from JavaScript's `Date`.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use wasm_bindgen::JsValue;

/// The current time, from `Date.now()`.
pub fn now() -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(js_sys::Date::now() as u64)
}

/// The current time as an ISO 8601 UTC string, such as `2026-10-23T18:35:01.937Z`.
pub fn now_iso() -> String {
    js_sys::Date::new_0().to_iso_string().into()
}

/// `time` in the user's locale, such as `23/10/2026, 18:35:01`.
pub fn local_string(time: SystemTime) -> String {
    let millis = time
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis() as f64)
        .unwrap_or(0.0);
    js_sys::Date::new(&JsValue::from_f64(millis))
        .to_locale_string("default", &JsValue::UNDEFINED)
        .into()
}
