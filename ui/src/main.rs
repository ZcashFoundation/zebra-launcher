//! The Zebra Launcher user interface: a client-side rendered Leptos app that trunk builds to
//! WebAssembly and Tauri serves from `ui/dist`.
//!
//! The crate also compiles natively so the workspace-wide `cargo clippy` and `cargo test` cover
//! it, including the unit tests of the pure modules ([`log_view`], [`format`]); the native
//! binary does nothing.

#![forbid(unsafe_code)]
// Natively nothing mounts the app, so most of it is unused there.
#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

mod api;
mod app;
mod charts;
mod clipboard;
mod clock;
mod config_form;
mod config_view;
mod format;
mod grafana_url;
mod insights;
mod log_view;
mod mock;
mod stats_history;
mod tauri;
mod theme;

mod components {
    pub mod config_form;
    pub mod log_insights;
    pub mod nav;
    pub mod outcome_note;
    pub mod reset_config;
    pub mod stale_banner;
    pub mod status_strip;
    pub mod tooltip;
    pub mod transient;
    pub mod update_mode_choice;
    pub mod updates;
}

mod pages {
    pub mod configure;
    pub mod dashboard;
    pub mod first_run;
    pub mod grafana;
    pub mod logs;
    pub mod settings;
}

#[cfg(target_arch = "wasm32")]
fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(app::App);
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    println!("The Zebra Launcher UI runs in a webview: run `trunk serve` in `ui/` instead.");
}
