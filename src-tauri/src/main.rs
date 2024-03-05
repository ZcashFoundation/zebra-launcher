//! Tauri app for Zebra

// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::fs;

use child_process::{run_zebrad, spawn_logs_emitter, zebrad_config_path};
use tauri::{ipc::InvokeError, AppHandle, Manager, RunEvent};

mod child_process;
mod state;

use state::AppState;

#[tauri::command]
fn save_config(app_handle: AppHandle, new_config: String) -> Result<String, InvokeError> {
    app_handle.state::<AppState>().kill_zebrad_child();
    let zebrad_config_path = zebrad_config_path();

    let old_config_contents = fs::read_to_string(&zebrad_config_path)
        .map_err(|err| format!("could not read existing config file, error: {err}"))?;

    fs::write(zebrad_config_path, new_config)
        .map_err(|err| format!("could not write to config file, error: {err}"))?;

    let (zebrad_child, zebrad_output_receiver) = run_zebrad();
    app_handle
        .state::<AppState>()
        .insert_zebrad_child(zebrad_child);
    spawn_logs_emitter(zebrad_output_receiver, app_handle);

    Ok(old_config_contents)
}

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
