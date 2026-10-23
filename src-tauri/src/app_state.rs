//! The state shared by commands, bridges and the exit flow.
//!
//! Everything here is a cheap, cloneable handle: the actors own the mutable
//! state, so there are no locks.

use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicU64},
};

use launcher_core::{
    config::ManagedKeys, paths::Paths, settings::SettingsHandle, supervisor::SupervisorHandle,
};
use launcher_types::{NodeStats, Settings};
use tokio::sync::{Semaphore, watch};

use crate::exit::ExitGuard;

/// Managed with `app.manage` once startup succeeds.
#[derive(Clone, Debug)]
pub struct AppState {
    /// The launcher's file layout.
    pub paths: Paths,
    /// The zebrad supervisor.
    pub supervisor: SupervisorHandle,
    /// The launcher settings actor.
    pub settings: SettingsHandle,
    /// The latest RPC poll, `None` while zebrad is not running.
    pub node_stats: watch::Receiver<Option<NodeStats>>,
    /// One permit: held while `zebrad.toml` is being written, so a config
    /// apply and a settings change never interleave.
    pub config_writer: Arc<Semaphore>,
    /// Bumped by every `subscribe_logs`; older log forwarders stop when it
    /// changes, so a reloaded UI does not leave forwarders behind.
    pub log_generation: Arc<AtomicU64>,
    /// Makes the exit flow run once.
    pub exit: Arc<ExitGuard>,
}

impl AppState {
    /// Bundles the handles.
    pub fn new(
        paths: Paths,
        supervisor: SupervisorHandle,
        settings: SettingsHandle,
        node_stats: watch::Receiver<Option<NodeStats>>,
    ) -> Self {
        Self {
            paths,
            supervisor,
            settings,
            node_stats,
            config_writer: Arc::new(Semaphore::new(1)),
            log_generation: Arc::new(AtomicU64::new(0)),
            exit: Arc::new(ExitGuard::default()),
        }
    }
}

/// The chain state directory: `<settings.data_dir>/state` when the user
/// chose a data directory, otherwise [`Paths::default_state_dir`].
pub fn state_dir(settings: &Settings, paths: &Paths) -> PathBuf {
    match &settings.data_dir {
        Some(dir) => PathBuf::from(dir).join("state"),
        None => paths.default_state_dir(),
    }
}

/// The `zebrad.toml` keys the launcher manages for `settings`.
pub fn managed_keys(settings: &Settings, paths: &Paths) -> ManagedKeys {
    ManagedKeys::defaults(settings.network, paths, state_dir(settings, paths))
}
