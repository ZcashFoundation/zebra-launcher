//! The Zebra Launcher executable. Everything lives in the library crate.

// Release builds on Windows are GUI programs: no console window opens with the app.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    zebra_launcher::run();
}
