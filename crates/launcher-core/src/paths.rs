//! Where the launcher keeps its files.
//!
//! The base directories come from the caller (the Tauri app resolves its
//! per-user config, data and log directories; tests use a temporary directory),
//! so this module never touches platform directory APIs itself.

use std::{
    io,
    path::{Path, PathBuf},
};

/// The launcher's file layout under its per-user directories.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    config_dir: PathBuf,
    data_dir: PathBuf,
    log_dir: PathBuf,
}

impl Paths {
    /// Builds the layout from the three base directories.
    pub fn new(
        config_dir: impl Into<PathBuf>,
        data_dir: impl Into<PathBuf>,
        log_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            config_dir: config_dir.into(),
            data_dir: data_dir.into(),
            log_dir: log_dir.into(),
        }
    }

    /// Puts config, data and logs under one root. Used by tests and `cargo run` setups.
    pub fn under(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref();
        Self::new(root.join("config"), root.join("data"), root.join("logs"))
    }

    /// The per-user configuration directory.
    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    /// The per-user data directory. The default chain state lives under it.
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// The per-user log directory.
    pub fn log_dir(&self) -> &Path {
        &self.log_dir
    }

    /// The `zebrad.toml` the launcher manages and passes with `-c`.
    pub fn zebrad_config(&self) -> PathBuf {
        self.config_dir.join("zebrad.toml")
    }

    /// The previous `zebrad.toml`, kept so a failed apply can be rolled back.
    pub fn zebrad_config_backup(&self) -> PathBuf {
        self.config_dir.join("zebrad.toml.bak")
    }

    /// A marker written while a config change is being applied, so a crash
    /// mid-apply is rolled back on the next launch.
    pub fn pending_apply_marker(&self) -> PathBuf {
        self.config_dir.join("zebrad.toml.pending")
    }

    /// The launcher's own settings.
    pub fn settings_file(&self) -> PathBuf {
        self.config_dir.join("launcher.json")
    }

    /// The pid of the zebrad the launcher started, for stale-process detection.
    pub fn pidfile(&self) -> PathBuf {
        self.data_dir.join("zebrad.pid")
    }

    /// Where zebrad writes its RPC auth cookie (`rpc.cookie_dir`).
    pub fn rpc_cookie_dir(&self) -> PathBuf {
        self.data_dir.join("rpc")
    }

    /// The RPC auth cookie file zebrad writes on every start.
    pub fn rpc_cookie_file(&self) -> PathBuf {
        self.rpc_cookie_dir().join(".cookie")
    }

    /// The default `state.cache_dir`; the user may choose another location at first run.
    pub fn default_state_dir(&self) -> PathBuf {
        self.data_dir.join("state")
    }

    /// Where the zebrad sidecar is copied when the launcher runs from a read-only
    /// or transient location such as an AppImage mount.
    pub fn sidecar_dir(&self) -> PathBuf {
        self.data_dir.join("sidecar")
    }

    /// zebrad's output, rotated by the supervisor.
    pub fn zebrad_log(&self) -> PathBuf {
        self.log_dir.join("zebrad.log")
    }

    /// The launcher's own log.
    pub fn launcher_log(&self) -> PathBuf {
        self.log_dir.join("launcher.log")
    }

    /// Creates every directory the layout needs.
    pub fn ensure_dirs(&self) -> io::Result<()> {
        for dir in [
            self.config_dir.as_path(),
            self.data_dir.as_path(),
            self.log_dir.as_path(),
            &self.rpc_cookie_dir(),
            &self.sidecar_dir(),
        ] {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn every_path_is_distinct_and_under_a_base_dir() {
        let paths = Paths::under("/root");
        let all = [
            paths.zebrad_config(),
            paths.zebrad_config_backup(),
            paths.pending_apply_marker(),
            paths.settings_file(),
            paths.pidfile(),
            paths.rpc_cookie_dir(),
            paths.rpc_cookie_file(),
            paths.default_state_dir(),
            paths.sidecar_dir(),
            paths.zebrad_log(),
            paths.launcher_log(),
        ];
        let distinct: BTreeSet<_> = all.iter().collect();
        assert_eq!(distinct.len(), all.len(), "paths collide: {all:?}");
        for path in &all {
            assert!(
                path.starts_with(paths.config_dir())
                    || path.starts_with(paths.data_dir())
                    || path.starts_with(paths.log_dir()),
                "{path:?} is outside the base directories"
            );
        }
    }

    #[test]
    fn ensure_dirs_creates_the_layout() {
        let root = tempfile::tempdir().expect("tempdir is writable");
        let paths = Paths::under(root.path());
        paths.ensure_dirs().expect("directories are created");
        for dir in [
            paths.config_dir().to_path_buf(),
            paths.data_dir().to_path_buf(),
            paths.log_dir().to_path_buf(),
            paths.rpc_cookie_dir(),
            paths.sidecar_dir(),
        ] {
            assert!(dir.is_dir(), "{dir:?} was not created");
        }
        // Creating the layout twice is fine.
        paths
            .ensure_dirs()
            .expect("existing directories are accepted");
    }
}
