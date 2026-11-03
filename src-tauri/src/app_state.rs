//! The state shared by commands, bridges and the exit flow.
//!
//! Everything here is a cheap, cloneable handle: the actors own the mutable
//! state, so there are no locks. Each node's own handles (supervisor,
//! monitors, gates) are in the registry, found by id.

use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
};

use launcher_core::{
    firstrun::{LegacyLocations, SizeCache},
    paths::Paths,
    registry::{NodeHandle, RegistryHandle},
    settings::SettingsHandle,
};
use launcher_types::{MachineResources, NodeId};
use tokio::sync::watch;

use crate::exit::ExitGuard;

/// Managed with `app.manage` once startup succeeds.
#[derive(Clone, Debug)]
pub struct AppState {
    /// The launcher's file layout.
    pub paths: Paths,
    /// The nodes and their runtimes.
    pub registry: RegistryHandle,
    /// The launcher settings actor.
    pub settings: SettingsHandle,
    /// The newest sample of the computer's CPU and memory, `None` until the
    /// first reading and on a platform that is not measured yet. Each node's
    /// process tree is in the registry, with its other handles.
    pub machine: watch::Receiver<Option<MachineResources>>,
    /// Makes the exit flow run once.
    pub exit: Arc<ExitGuard>,
    /// Set once the tray icon exists, so closing the window may hide it
    /// instead of quitting.
    pub tray_ready: Arc<AtomicBool>,
    /// The alpha.4 state size, measured once per launcher session by
    /// `get_first_run_info` and reused by `delete_node_data` and
    /// `delete_legacy_state`; the registry shares it for removals.
    pub sizes: Arc<SizeCache>,
}

impl AppState {
    /// Bundles the handles.
    pub fn new(
        paths: Paths,
        registry: RegistryHandle,
        settings: SettingsHandle,
        sizes: Arc<SizeCache>,
        machine: watch::Receiver<Option<MachineResources>>,
    ) -> Self {
        Self {
            paths,
            registry,
            settings,
            machine,
            exit: Arc::new(ExitGuard::default()),
            tray_ready: Arc::new(AtomicBool::new(false)),
            sizes,
        }
    }

    /// The node with `id`, or the message for the UI.
    pub fn node(&self, id: &NodeId) -> Result<NodeHandle, String> {
        self.registry.node(id).map_err(|err| err.to_string())
    }

    /// The first node: the one the first run sets up.
    pub fn first_node(&self) -> Result<NodeHandle, String> {
        self.registry
            .first()
            .ok_or_else(|| "Zeal has no node".to_string())
    }
}

/// zebrad's default cache directory, where an alpha.4 launcher (or a zebrad
/// run by hand) kept its chain state.
pub fn legacy_cache_dir() -> Result<PathBuf, String> {
    LegacyLocations::current()
        .zebra_cache_dir
        .ok_or_else(|| "this system has no default cache directory".to_string())
}
