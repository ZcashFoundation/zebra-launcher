//! The block explorer for the window: [`launcher_core::explorer`], started once with the
//! first node's supervisor, monitor and spec watches, and managed as Tauri state for the
//! `explorer` command. Other nodes get one in a later phase (docs/EXPLORER.md, P6). Its live state reaches the UI through
//! [`bridge::forward_explorer_live`](crate::bridge::forward_explorer_live).

use launcher_core::explorer::{self, ExplorerConfig, ExplorerHandle, ExplorerInputs};

use crate::app_state::AppState;

/// Starts the explorer of the first node. It reads nothing until the UI asks.
pub fn start(state: &AppState) -> ExplorerHandle {
    // The registry always holds a node: the migration and a fresh install create the first,
    // and the last one cannot be removed.
    let node = state
        .registry
        .first()
        .expect("the registry always holds a node");
    let inputs = ExplorerInputs {
        status: node.supervisor.watch_status(),
        stats: node.stats.clone(),
        details: node.details.clone(),
        spec: node.watch_spec(),
    };
    let config = ExplorerConfig::new(node.paths.rpc_cookie_file());
    // The actor is a Tokio task: spawn it inside the app's runtime.
    tauri::async_runtime::block_on(async move { explorer::spawn(inputs, config) })
}
