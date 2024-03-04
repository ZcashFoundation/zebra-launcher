//! Tauri app for Zebra

// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    io::{BufRead, BufReader},
    process::{Command, Stdio},
};

use tauri::{AppHandle, Manager, RunEvent};

// TODO: Add a command for updating the config and restarting `zebrad` child process
#[tauri::command]
fn save_config() {}

fn main() {
    // Spawn initial zebrad process
    let mut zebrad = Command::new("zebrad")
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("zebrad should be installed as a bundled binary and should start successfully");

    // Spawn a task for reading output and sending it to a channel
    let (zebrad_log_sender, mut zebrad_log_receiver) = tokio::sync::mpsc::channel(100);
    let zebrad_stdout = zebrad.stdout.take().expect("should have anonymous pipe");

    // TODO: Use a blocking tokio/async_runtime thread? The io is blocking (reading the child process output from stdio), so
    //       it shouldn't use a green thread
    let _log_emitter_handle = std::thread::spawn(move || {
        for line in BufReader::new(zebrad_stdout).lines() {
            // Ignore send errors for now
            let _ =
                zebrad_log_sender.blocking_send(line.expect("zebrad logs should be valid UTF-8"));
        }
    });

    tauri::Builder::default()
        .setup(|app| {
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    if let Some(output) = zebrad_log_receiver.recv().await {
                        app_handle.emit("log", output.clone()).unwrap();
                    }
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![save_config])
        .build(tauri::generate_context!())
        .unwrap()
        .run(move |app_handle: &AppHandle, _event| {
            if let RunEvent::Exit = &_event {
                zebrad.kill().expect("could not kill zebrad process");
                app_handle.exit(0);
            }
        });
}
