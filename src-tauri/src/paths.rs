//! Resolves the launcher's per-user directories from Tauri's path resolver.

use std::{io, path::PathBuf};

use launcher_core::paths::Paths;
use tauri::{AppHandle, Manager, Runtime};

/// The launcher's directories could not be found or created.
#[derive(Debug, thiserror::Error)]
pub enum PathsError {
    /// The platform has no such directory for this user.
    #[error("could not find the {what} directory: {source}")]
    Resolve {
        /// Which directory.
        what: &'static str,
        /// The resolver's error.
        #[source]
        source: tauri::Error,
    },
    /// A directory could not be created.
    #[error("could not create the launcher directories under {dir}: {source}")]
    Create {
        /// One of the base directories, for the message.
        dir: PathBuf,
        /// The filesystem error.
        #[source]
        source: io::Error,
    },
}

/// Resolves the config, local data and log directories for this app and
/// creates the layout under them.
///
/// - config: `app_config_dir` (`zebrad.toml`, `launcher.json`)
/// - data: `app_local_data_dir` (pidfile, RPC cookie, default chain state,
///   AppImage sidecar copy); local, so it never roams on Windows
/// - logs: `app_log_dir` (`launcher.log`, `zebrad.log`)
pub fn resolve<R: Runtime>(app: &AppHandle<R>) -> Result<Paths, PathsError> {
    let resolver = app.path();
    let resolve = |what, result: tauri::Result<PathBuf>| {
        result.map_err(|source| PathsError::Resolve { what, source })
    };
    let paths = Paths::new(
        resolve("config", resolver.app_config_dir())?,
        resolve("local data", resolver.app_local_data_dir())?,
        resolve("log", resolver.app_log_dir())?,
    );
    paths.ensure_dirs().map_err(|source| PathsError::Create {
        dir: paths.data_dir().to_path_buf(),
        source,
    })?;
    Ok(paths)
}
