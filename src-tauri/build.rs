//! Build script for the Zebra Launcher app.
//!
//! Besides the usual Tauri build steps, it generates `allow-<command>` and
//! `deny-<command>` permissions for each app command, so the capability file
//! has to grant every command the UI may call.
//!
//! The command list exists three times: [`COMMANDS`] here, the
//! `generate_handler!` invocation in `src/lib.rs` and the `allow-*`
//! permissions in `capabilities/main.json`. A command missing from one of
//! them compiles and fails only in the webview, at runtime ("not allowed"),
//! so [`check_command_lists`] makes the build fail instead.

use std::collections::BTreeSet;

/// Every command the UI may call. Checked against `src/lib.rs` and
/// `capabilities/main.json` by [`check_command_lists`].
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
    "reset_config",
    "get_settings",
    "set_settings",
    "get_node_stats",
    "get_metrics",
    "open_path",
    "get_first_run_info",
    "pick_data_dir",
    "choose_data_dir",
    "free_space",
    "import_legacy_state",
    "delete_node_data",
    "delete_legacy_state",
    "copy_diagnostics",
    "complete_first_run",
    "get_update_state",
    "get_update_info",
    "check_for_update",
    "install_update",
    "cancel_update",
];

const LIB_RS: &str = "src/lib.rs";
const CAPABILITY: &str = "capabilities/main.json";

fn main() {
    check_command_lists();
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)),
    )
    .expect("failed to run the Tauri build script");
}

/// Fails the build unless `generate_handler!` in `src/lib.rs` and the
/// `allow-*` permissions in `capabilities/main.json` name exactly the
/// commands in [`COMMANDS`].
fn check_command_lists() {
    println!("cargo:rerun-if-changed={LIB_RS}");
    println!("cargo:rerun-if-changed={CAPABILITY}");
    let expected: BTreeSet<String> = COMMANDS.iter().map(|name| name.to_string()).collect();
    assert_eq!(
        expected.len(),
        COMMANDS.len(),
        "COMMANDS in build.rs lists a command twice"
    );

    let lib = std::fs::read_to_string(LIB_RS).unwrap_or_else(|err| panic!("read {LIB_RS}: {err}"));
    expect_same(
        &format!("generate_handler! in {LIB_RS}"),
        &handler_commands(&lib),
        &expected,
        "a command in one list only is unreachable from the UI",
    );

    let capability = std::fs::read_to_string(CAPABILITY)
        .unwrap_or_else(|err| panic!("read {CAPABILITY}: {err}"));
    expect_same(
        &format!("the allow-* permissions in {CAPABILITY}"),
        &allowed_commands(&capability),
        &expected,
        "a command without a permission is refused by the webview",
    );
}

/// Panics, naming the differences, unless `found` is exactly `expected`.
fn expect_same(what: &str, found: &BTreeSet<String>, expected: &BTreeSet<String>, hint: &str) {
    let missing: Vec<&String> = expected.difference(found).collect();
    let extra: Vec<&String> = found.difference(expected).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "{what} does not match COMMANDS in build.rs: missing {missing:?}, unexpected {extra:?}; {hint}"
    );
}

/// The commands named in the `generate_handler![commands::a, commands::b, ...]`
/// invocation of `lib`.
fn handler_commands(lib: &str) -> BTreeSet<String> {
    const OPEN: &str = "generate_handler![";
    let start = lib
        .find(OPEN)
        .unwrap_or_else(|| panic!("{LIB_RS} has no {OPEN} invocation"))
        + OPEN.len();
    let body = &lib[start..];
    let end = body
        .find(']')
        .unwrap_or_else(|| panic!("the generate_handler! invocation in {LIB_RS} does not close"));
    body[..end]
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            entry
                .strip_prefix("commands::")
                .unwrap_or_else(|| {
                    panic!("{entry:?} in generate_handler! is not a commands:: path")
                })
                .to_string()
        })
        .collect()
}

/// The commands the `"allow-<kebab-name>"` strings in the capability `json`
/// grant. Core permissions (`core:...:allow-...`) are not app commands and
/// are skipped; the file is small and ours, so a scan of its string
/// literals is enough.
fn allowed_commands(json: &str) -> BTreeSet<String> {
    json.split('"')
        .filter_map(|literal| literal.strip_prefix("allow-"))
        .map(|kebab| kebab.replace('-', "_"))
        .collect()
}
