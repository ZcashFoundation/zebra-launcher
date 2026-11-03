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
    config::ManagedKeys,
    firstrun::{LegacyLocations, SizeCache},
    gates::NodeGates,
    paths::Paths,
    settings::SettingsHandle,
    supervisor::SupervisorHandle,
};
use launcher_types::{
    MachineResources, MetricsSnapshot, NodeDetails, NodeResources, NodeStats, Settings,
};
use tokio::sync::watch;

use crate::exit::ExitGuard;

/// The resource samplers' outputs, for [`AppState::new`].
#[derive(Clone, Debug)]
pub struct ResourceWatches {
    /// The node's process tree, `None` while it has no run.
    pub node: watch::Receiver<Option<NodeResources>>,
    /// The computer, `None` until the first reading and where it cannot be read.
    pub machine: watch::Receiver<Option<MachineResources>>,
}

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
    /// The details behind [`AppState::node_stats`], from the same poll.
    pub node_details: watch::Receiver<Option<NodeDetails>>,
    /// The latest read of zebrad's Prometheus metrics page, `None` while
    /// zebrad is not running or the page stopped answering.
    pub metrics: watch::Receiver<Option<MetricsSnapshot>>,
    /// The newest sample of zebrad's process tree, `None` while it has no
    /// run or on a platform that is not measured yet.
    pub node_resources: watch::Receiver<Option<NodeResources>>,
    /// The newest sample of the computer's CPU and memory.
    pub machine: watch::Receiver<Option<MachineResources>>,
    /// Keeps zebrad starts, node-data changes and writes of `zebrad.toml`
    /// apart.
    pub gates: NodeGates,
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
}

impl AppState {
    /// Bundles the handles.
    pub fn new(
        paths: Paths,
        supervisor: SupervisorHandle,
        settings: SettingsHandle,
        node_stats: watch::Receiver<Option<NodeStats>>,
        node_details: watch::Receiver<Option<NodeDetails>>,
        metrics: watch::Receiver<Option<MetricsSnapshot>>,
        resources: ResourceWatches,
    ) -> Self {
        Self {
            gates: NodeGates::new(supervisor.watch_status()),
            paths,
            supervisor,
            settings,
            node_stats,
            node_details,
            metrics,
            node_resources: resources.node,
            machine: resources.machine,
            log_generation: Arc::new(AtomicU64::new(0)),
            exit: Arc::new(ExitGuard::default()),
            tray_ready: Arc::new(AtomicBool::new(false)),
            sizes: Arc::new(SizeCache::default()),
        }
    }

    /// The directories `delete_node_data` may delete a state directory in:
    /// the launcher's data directory and the one chosen in the settings.
    pub fn data_roots(&self, settings: &Settings) -> Vec<PathBuf> {
        let mut roots = vec![self.paths.data_dir().to_path_buf()];
        roots.extend(settings.data_dir.as_ref().map(PathBuf::from));
        roots
    }
}

/// zebrad's default cache directory, where an alpha.4 launcher (or a zebrad
/// run by hand) kept its chain state.
pub fn legacy_cache_dir() -> Result<PathBuf, String> {
    LegacyLocations::current()
        .zebra_cache_dir
        .ok_or_else(|| "this system has no default cache directory".to_string())
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
