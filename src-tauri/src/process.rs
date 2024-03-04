// Copyright 2019-2023 Tauri Programme within The Commons Conservancy
// Copyright 2024 Zcash Foundation
// This code is modified from the Tauri code.

use tauri::utils;

// See <https://docs.rs/tauri/latest/src/tauri/api/process/command.rs.html#137-145>
pub fn relative_command_path(command: impl std::fmt::Display) -> Option<String> {
    let current_exe = utils::platform::current_exe();

    let exe_path = match current_exe {
        Ok(exe_dir) => exe_dir,
        Err(err) => {
            println!("error getting path to current executable: {err}");
            return None;
        }
    };

    let exe_dir = exe_path.parent()?.display();

    #[cfg(windows)]
    let path = format!("{}\\{command}.exe", exe_dir);

    #[cfg(not(windows))]
    let path = format!("{}/{command}", exe_dir);

    Some(path)
}
