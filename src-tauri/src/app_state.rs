//! The state shared by commands, bridges and the exit flow.
//!
//! Everything here is a cheap, cloneable handle: the actors own the mutable
//! state, so there are no locks.

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64},
    },
};

use launcher_core::{
    config::ManagedKeys, firstrun::SizeCache, paths::Paths, settings::SettingsHandle,
    supervisor::SupervisorHandle,
};
use launcher_types::{NodeStats, Phase, Settings};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};

use crate::exit::ExitGuard;

/// The permits in [`AppState::data_gate`]: how many starts may run at once.
const DATA_GATE_PERMITS: u32 = 64;

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
    /// Set once the tray icon exists, so closing the window may hide it
    /// instead of quitting.
    pub tray_ready: Arc<AtomicBool>,
    /// The alpha.4 state size, measured once per launcher session by
    /// `get_first_run_info` and reused by `delete_node_data` and
    /// `delete_legacy_state`.
    pub sizes: Arc<SizeCache>,
    /// Keeps zebrad starts and node-data changes apart: every start holds one
    /// permit until it has finished, and a data change (choosing the data
    /// directory, importing or deleting the state or the alpha.4 state,
    /// resetting `zebrad.toml`) holds all of them.
    data_gate: Arc<Semaphore>,
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
            tray_ready: Arc::new(AtomicBool::new(false)),
            sizes: Arc::new(SizeCache::default()),
            data_gate: Arc::new(Semaphore::new(DATA_GATE_PERMITS as usize)),
        }
    }

    /// Held while zebrad is being started (or stopped and started again), so
    /// the node data cannot change underneath it. Fails while a data change
    /// is running.
    pub fn start_permit(&self) -> Result<OwnedSemaphorePermit, String> {
        self.data_gate.clone().try_acquire_owned().map_err(|_| {
            "the node data is being changed; start zebrad when that has finished".to_string()
        })
    }

    /// Held while the node data changes. Fails unless zebrad is stopped (see
    /// [`is_stopped`]) and no start or other data change is running.
    pub fn data_change_permit(&self) -> Result<OwnedSemaphorePermit, String> {
        let permit = self
            .data_gate
            .clone()
            .try_acquire_many_owned(DATA_GATE_PERMITS)
            .map_err(|_| "zebrad is starting, or another data change is running".to_string())?;
        if !is_stopped(&self.supervisor.status().phase) {
            return Err("stop zebrad first".to_string());
        }
        Ok(permit)
    }

    /// The directories `delete_node_data` may delete a state directory in:
    /// the launcher's data directory and the one chosen in the settings.
    pub fn data_roots(&self, settings: &Settings) -> Vec<PathBuf> {
        let mut roots = vec![self.paths.data_dir().to_path_buf()];
        roots.extend(settings.data_dir.as_ref().map(PathBuf::from));
        roots
    }
}

/// True when no zebrad runs and none is scheduled: [`Phase::Stopped`], or
/// [`Phase::Failed`], which waits for the user.
pub fn is_stopped(phase: &Phase) -> bool {
    matches!(phase, Phase::Stopped | Phase::Failed { .. })
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
