//! Child process management

use std::{
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::Duration,
};

use tauri::{utils, AppHandle, Manager};

use tokio::sync::mpsc::Receiver;

/// Zebrad Configuration Filename
pub const CONFIG_FILE: &str = "zebrad.toml";

#[cfg(windows)]
pub const ZEBRAD_COMMAND_NAME: &str = "zebrad.exe";

#[cfg(not(windows))]
pub const ZEBRAD_COMMAND_NAME: &str = "zebrad";

pub fn zebrad_config_path() -> PathBuf {
    let exe_path =
        utils::platform::current_exe().expect("could not get path to current executable");

    let exe_dir_path = exe_path
        .parent()
        .expect("could not get path to parent directory of executable");

    exe_dir_path.join(CONFIG_FILE)
}

pub fn zebrad_bin_path() -> PathBuf {
    let exe_path =
        utils::platform::current_exe().expect("could not get path to current executable");

    let exe_dir_path = exe_path
        .parent()
        .expect("could not get path to parent directory of executable");

    exe_dir_path.join(ZEBRAD_COMMAND_NAME)
}

pub fn run_zebrad() -> (Child, Receiver<String>) {
    let zebrad_config_path = zebrad_config_path();
    let zebrad_path = zebrad_bin_path();

    let zebrad_config_path_str = zebrad_config_path.display().to_string();
    let zebrad_path_str = zebrad_path.display().to_string();

    // Generate a default config if there's no existing config file
    if !zebrad_config_path.exists() {
        Command::new(&zebrad_path_str)
            .args(["generate", "-o", &zebrad_config_path_str])
            .spawn()
            .expect("could not start zebrad to generate default config")
            .wait()
            .expect("error waiting for `zebrad generate` to exit");

        assert!(
            zebrad_config_path.exists(),
            "config file should exist after `zebrad generate` has exited"
        );
    }

    let mut zebrad_child = Command::new(zebrad_path_str)
        .args(["-c", &zebrad_config_path_str])
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("zebrad should be installed as a bundled binary and should start successfully");

    let zebrad_stdout = zebrad_child
        .stdout
        .take()
        .expect("should have anonymous pipe");

    // Spawn a task for reading output and sending it to a channel
    let (output_sender, output_receiver) = tokio::sync::mpsc::channel(100);
    let _output_reader_task_handle = tauri::async_runtime::spawn_blocking(move || {
        for line in BufReader::new(zebrad_stdout).lines() {
            // Ignore send errors for now
            if let Err(error) =
                output_sender.blocking_send(line.expect("zebrad logs should be valid UTF-8"))
            {
                tracing::warn!(
                    ?error,
                    "zebrad output channel is closed before output terminated"
                );
            }
        }
    });

    (zebrad_child, output_receiver)
}

pub fn spawn_logs_emitter(
    mut output_receiver: Receiver<String>,
    app_handle: AppHandle,
    should_wait_for_webview: bool,
) {
    tauri::async_runtime::spawn(async move {
        // Wait for webview to start
        if should_wait_for_webview {
            tokio::time::sleep(Duration::from_secs(3)).await;
        }

        // Exit the task once the channel is closed and empty.
        while let Some(output) = output_receiver.recv().await {
            if let Err(error) = app_handle.emit("log", output) {
                tracing::warn!(?error, "log could not be serialized to JSON");
            }
        }
    });
}
