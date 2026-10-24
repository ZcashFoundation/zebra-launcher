//! Light and dark appearance: resolves the setting against the system preference and applies it
//! to the document.

use leptos::prelude::*;

use crate::app::AppState;

/// Applies the initial appearance. Until a setting exists the app is dark.
pub fn init(state: &AppState) {
    state.resolved_dark.set(true);
}
