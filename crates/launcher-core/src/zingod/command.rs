//! How zingod is started: `zeal-zingod -c <config> start`, with stdin null, at nice 10, in an
//! environment that holds only what Zeal sets.
//!
//! The environment is cleared, an exception to zebrad's rule (docs/ARCHITECTURE.md): zingod
//! runs on Linux only and, with `[snapshot]` off, starts no program, so it needs no `PATH`.
//! Clearing keeps out `ZINGOD_CONFIG_*`, which zingod matches in any case and lets win over the
//! file, an unknown one failing the load; a malformed `RUST_LOG`, which makes it exit 2;
//! `NOTIFY_SOCKET`, so it never tells a service manager Zeal runs under that it is ready; and
//! allocator settings.

use std::{ffi::OsString, path::PathBuf};

/// The filter zingod logs with: its own crates at INFO, its default.
pub const RUST_LOG: &str = "zingod=info";

/// zingod's nice value: zebrad gets the CPU first while zingod builds its index on every core.
pub const NICE: i32 = 10;

/// How to run zingod.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZingodCommand {
    /// `zeal-zingod`.
    pub binary: PathBuf,
    /// The file Zeal writes ([`super::ZingodConfig::write`]), passed with `-c`.
    pub config: PathBuf,
    /// `HOME` and the XDG folders: [`super::ZingodConfig::home_dir`].
    pub home: PathBuf,
}

impl ZingodCommand {
    pub fn new(
        binary: impl Into<PathBuf>,
        config: impl Into<PathBuf>,
        home: impl Into<PathBuf>,
    ) -> Self {
        Self {
            binary: binary.into(),
            config: config.into(),
            home: home.into(),
        }
    }

    /// The arguments after the binary.
    pub fn args(&self) -> [OsString; 3] {
        ["-c".into(), self.config.clone().into(), "start".into()]
    }

    /// zingod's whole environment.
    pub fn env(&self) -> [(&'static str, OsString); 5] {
        let under = |dir: &str| self.home.join(dir).into_os_string();
        [
            ("HOME", self.home.clone().into_os_string()),
            ("XDG_CONFIG_HOME", under(".config")),
            ("XDG_DATA_HOME", under(".local/share")),
            ("XDG_CACHE_HOME", under(".cache")),
            ("RUST_LOG", RUST_LOG.into()),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zingod_gets_its_config_and_five_variables() {
        let command = ZingodCommand::new("/app/zeal-zingod", "/data/zingod.toml", "/idx/home");
        assert_eq!(
            command.args(),
            ["-c", "/data/zingod.toml", "start"].map(OsString::from)
        );
        let env = command.env();
        let names: Vec<&str> = env.iter().map(|(name, _)| *name).collect();
        assert_eq!(
            names,
            [
                "HOME",
                "XDG_CONFIG_HOME",
                "XDG_DATA_HOME",
                "XDG_CACHE_HOME",
                "RUST_LOG"
            ]
        );
        for (name, value) in &env[..4] {
            assert!(
                PathBuf::from(value).starts_with("/idx/home"),
                "{name}={value:?}"
            );
        }
        assert_eq!(env[4].1, "zingod=info");
    }
}
