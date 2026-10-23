//! The Zebra Launcher user interface: a client-side rendered Leptos app that trunk builds to
//! WebAssembly and Tauri serves from `ui/dist`.
//!
//! The crate also compiles natively so the workspace-wide `cargo clippy` and `cargo test` cover
//! it; the native binary does nothing.

#![forbid(unsafe_code)]
// Natively nothing mounts the app, so most of it is unused there.
#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

mod api;
mod clock;
mod mock;
mod tauri;
mod types;

#[cfg(target_arch = "wasm32")]
fn main() {
    use leptos::prelude::*;

    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(|| view! { <p>"Zebra Launcher"</p> });
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    println!("The Zebra Launcher UI runs in a webview: run `trunk serve` in `ui/` instead.");
}
