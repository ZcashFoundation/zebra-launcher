//! Child process management

use std::{
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
};

use tauri::{async_runtime::spawn_blocking, utils};

/// Zebrad Configuration Filename
pub const CONFIG_FILE: &str = "zebrad.toml";

#[cfg(windows)]
pub const ZEBRAD_COMMAND_NAME: &str = "zebrad.exe";

#[cfg(not(windows))]
pub const ZEBRAD_COMMAND_NAME: &str = "zebrad";

pub fn run_zebrad_and_read_output() -> Child {
    let exe_path =
        utils::platform::current_exe().expect("could not get path to current executable");

    let exe_dir_path = exe_path
        .parent()
        .expect("could not get path to parent directory of executable");

    let zebrad_config_path = exe_dir_path.join(CONFIG_FILE);
    let zebrad_path = exe_dir_path.join(ZEBRAD_COMMAND_NAME);

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

    Command::new(zebrad_path_str)
        .args(["-c", &zebrad_config_path_str])
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("zebrad should be installed as a bundled binary and should start successfully")
}
