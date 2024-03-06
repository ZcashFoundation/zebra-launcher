//! Tauri app for Zebra

// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{fs, time::Duration};

use child_process::{run_zebrad, spawn_logs_emitter, zebrad_config_path};
use tauri::{ipc::InvokeError, AppHandle, Manager, RunEvent};

mod child_process;
mod state;

use state::AppState;

#[tauri::command]
async fn save_config(app_handle: AppHandle, new_config: String) -> Result<String, InvokeError> {
    tracing::info!("dropping and killing zebrad child process");
    app_handle.state::<AppState>().kill_zebrad_child();
    let zebrad_config_path = zebrad_config_path();

    tracing::info!("reading old config");
    let old_config_contents = fs::read_to_string(&zebrad_config_path)
        .map_err(|err| format!("could not read existing config file, error: {err}"))?;

    tracing::info!("writing new config");
    fs::write(zebrad_config_path, new_config)
        .map_err(|err| format!("could not write to config file, error: {err}"))?;

    tracing::info!("waiting for old zebrad child process to shutdown");
    tokio::time::sleep(Duration::from_secs(5)).await;

    tracing::info!("starting new zebrad child process");
    let (zebrad_child, zebrad_output_receiver) = run_zebrad();

    tracing::info!("started new zebrad child process, starting output reader task");
    app_handle
        .state::<AppState>()
        .insert_zebrad_child(zebrad_child);
    spawn_logs_emitter(zebrad_output_receiver, app_handle, false);

    Ok(old_config_contents)
}

#[tauri::command]
fn read_config() -> Result<String, InvokeError> {
    Ok(fs::read_to_string(zebrad_config_path())
        .map_err(|err| format!("could not read existing config file, error: {err}"))?)
}

fn main() {
    let (zebrad_child, zebrad_output_receiver) = run_zebrad();

    tauri::Builder::default()
        .manage(AppState::new(zebrad_child))
        .setup(|app| {
            spawn_logs_emitter(zebrad_output_receiver, app.handle().clone(), true);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![save_config, read_config])
        .build(tauri::generate_context!())
        .unwrap()
        .run(move |app_handle: &AppHandle, _event| {
            if let RunEvent::Exit = &_event {
                app_handle.state::<AppState>().kill_zebrad_child();
                app_handle.exit(0);
            }
        });
}
