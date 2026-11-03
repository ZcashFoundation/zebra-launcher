//! The block explorer for the window: [`launcher_core::explorer`], started once with the
//! supervisor's, the monitor's and the settings' watches, and managed as Tauri state for the
//! `explorer` command. Its live state reaches the UI through
//! [`bridge::forward_explorer_live`](crate::bridge::forward_explorer_live).

use launcher_core::explorer::{self, ExplorerConfig, ExplorerHandle, ExplorerInputs};

use crate::app_state::AppState;

/// Starts the explorer of the launcher's zebrad. It reads nothing until the UI asks.
pub fn start(state: &AppState) -> ExplorerHandle {
    let inputs = ExplorerInputs {
        status: state.supervisor.watch_status(),
        stats: state.node_stats.clone(),
        details: state.node_details.clone(),
        settings: state.settings.watch(),
    };
    let config = ExplorerConfig::new(state.paths.rpc_cookie_file());
    // The actor is a Tokio task: spawn it inside the app's runtime.
    tauri::async_runtime::block_on(async move { explorer::spawn(inputs, config) })
}
