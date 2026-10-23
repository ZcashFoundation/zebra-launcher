//! Build script for the Zebra Launcher app.
//!
//! Besides the usual Tauri build steps, it generates `allow-<command>` and
//! `deny-<command>` permissions for each app command, so the capability file
//! has to grant every command the UI may call.

/// Every command in `generate_handler!` (src/lib.rs). Keep the two lists and
/// `capabilities/main.json` in sync.
const COMMANDS: &[&str] = &[
    "get_status",
    "subscribe_logs",
    "log_history",
    "start_zebrad",
    "start_zebrad_killing_stale",
    "stop_zebrad",
    "restart_zebrad",
    "read_config",
    "apply_config",
    "get_settings",
    "set_settings",
    "get_node_stats",
    "open_path",
];

fn main() {
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)),
    )
    .expect("failed to run the Tauri build script");
}
