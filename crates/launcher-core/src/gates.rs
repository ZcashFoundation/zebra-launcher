//! The semaphores that keep zebrad starts, node-data changes and writes of
//! `zebrad.toml` apart. No locks: a permit is a slot in a semaphore.

use std::sync::Arc;

use launcher_types::Status;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};

/// The permits in [`NodeGates::data_gate`]: how many starts may run at once.
const DATA_GATE_PERMITS: u32 = 64;

/// The gates of one zebrad. Cheap to clone.
#[derive(Clone, Debug)]
pub struct NodeGates {
    /// One permit: held while `zebrad.toml` is being written, so a config
    /// apply and a settings change never interleave.
    ///
    /// The rule: only `apply_config` holds it for long (across a zebrad
    /// restart, up to about a minute). Every other writer takes it with
    /// [`NodeGates::try_config_permit`] and fails fast with "try again"
    /// instead of waiting, so no IPC call hangs behind an apply, and takes it
    /// before changing anything, so a refusal leaves everything as it was.
    pub config_writer: Arc<Semaphore>,
    /// Keeps zebrad starts and node-data changes apart: every start holds one
    /// permit until it has finished, and a data change (choosing the data
    /// directory, importing or deleting the state or the alpha.4 state,
    /// resetting `zebrad.toml`) holds all of them.
    data_gate: Arc<Semaphore>,
    /// The supervisor's status: a data change needs zebrad stopped or failed.
    status: watch::Receiver<Status>,
}

impl NodeGates {
    /// Open gates for the zebrad whose supervisor publishes `status`.
    pub fn new(status: watch::Receiver<Status>) -> Self {
        Self {
            config_writer: Arc::new(Semaphore::new(1)),
            data_gate: Arc::new(Semaphore::new(DATA_GATE_PERMITS as usize)),
            status,
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

    /// Held while the node data changes. Fails unless zebrad is stopped or
    /// failed ([`launcher_types::Phase::is_settled`]) and no start or other
    /// data change is running.
    pub fn data_change_permit(&self) -> Result<OwnedSemaphorePermit, String> {
        let permit = self.block_starts()?;
        if !self.status.borrow().phase.is_settled() {
            return Err("stop zebrad first".to_string());
        }
        Ok(permit)
    }

    /// Holds every start permit, whatever zebrad is doing: no start can begin
    /// until it is dropped. A removal takes it before it stops the node. Fails
    /// while a start or a data change is running.
    pub fn block_starts(&self) -> Result<OwnedSemaphorePermit, String> {
        self.data_gate
            .clone()
            .try_acquire_many_owned(DATA_GATE_PERMITS)
            .map_err(|_| "zebrad is starting, or another data change is running".to_string())
    }

    /// Held while `zebrad.toml` is written by a command that does not wait
    /// for a running config change (a reset, a data directory change, a
    /// settings save that changes a managed key); see
    /// [`NodeGates::config_writer`].
    pub fn try_config_permit(&self) -> Result<OwnedSemaphorePermit, String> {
        self.config_writer
            .clone()
            .try_acquire_owned()
            .map_err(|_| "a configuration change is being applied; try again".to_string())
    }
}
