//! Tauri app for Zebra

// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use child_process::{run_zebrad, spawn_logs_emitter};
use tauri::{AppHandle, Manager, RunEvent};

mod child_process;
mod state;

use state::AppState;

// TODO: Add a command for updating the config and restarting `zebrad` child process
#[tauri::command]
fn save_config() {}

fn main() {
    let (zebrad_child, zebrad_output_receiver) = run_zebrad();

    tauri::Builder::default()
        .manage(AppState::new(zebrad_child))
        .setup(|app| {
            spawn_logs_emitter(zebrad_output_receiver, app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![save_config])
        .build(tauri::generate_context!())
        .unwrap()
        .run(move |app_handle: &AppHandle, _event| {
            if let RunEvent::Exit = &_event {
                app_handle.state::<AppState>().kill_zebrad_child();
                app_handle.exit(0);
            }
        });
}
