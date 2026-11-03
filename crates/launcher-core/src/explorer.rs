//! The block explorer's backend: an actor that answers the `explorer` command from the node's
//! own RPC. See docs/EXPLORER.md.
//!
//! ```text
//!   ExplorerHandle ──Command──▶ actor ──▶ watch<ExplorerLiveEvent> ──▶ event explorer_live
//!   watch<Status>, watch<NodeStats>,       │  ▲
//!   watch<NodeDetails>, watch<Settings> ─▶ │  │ reads (merged, cached, scheduled)
//!                                          ▼  │
//!                    request tasks ──▶ fetch tasks ──▶ ChainSource (zebrad's RPC)
//! ```
//!
//! The actor owns everything mutable: the cache, the chain index, the scheduler and the
//! permits, so there are no locks. Each request runs as a task that asks the actor for every
//! read it needs; the actor answers from the cache, joins a read already in flight, or queues
//! one. At most six reads run at once (four while the node syncs), interactive ones first.
//!
//! The explorer reads nothing until it is asked, and follows the tip only while the Explorer
//! is on screen and the node is at the network's tip. Logs name a read's method and how long
//! it took, never what was asked for.
//!
//! Until the backend runs several nodes (docs/MULTI-NODE.md), one explorer serves the
//! launcher's zebrad under the id of the network zebrad reports, `zebrad-mainnet` or
//! `zebrad-testnet`.

mod actor;
mod assemble;
mod cache;
mod chain_index;
mod sched;
mod source;
#[cfg(test)]
mod tests;

use std::{net::SocketAddr, path::PathBuf, time::Duration};

use launcher_types::{
    NodeDetails, NodeId, NodeStats, Settings, Status,
    explorer::{ExplorerCall, ExplorerError, ExplorerErrorKind, ExplorerLiveEvent, ExplorerReply},
};
use tokio::sync::{mpsc, oneshot, watch};

use crate::rpc::RpcError;
use source::{ChainSource, RpcSource};

/// What the explorer follows.
#[derive(Clone, Debug)]
pub struct ExplorerInputs {
    /// The supervisor's status: whether zebrad runs, and its RPC address.
    pub status: watch::Receiver<Status>,
    /// The monitor's statistics: the chain zebrad runs and whether it is at the tip.
    pub stats: watch::Receiver<Option<NodeStats>>,
    /// The monitor's details: the network upgrades' heights.
    pub details: watch::Receiver<Option<NodeDetails>>,
    /// The settings: the network, before zebrad reports one.
    pub settings: watch::Receiver<Settings>,
}

/// How the explorer runs.
#[derive(Clone, Debug)]
pub struct ExplorerConfig {
    /// zebrad's RPC cookie file.
    pub cookie_file: PathBuf,
    /// The cache's size, in bytes.
    pub cache_bytes: usize,
    /// The cache's size once the Explorer has been hidden for [`ExplorerConfig::idle_after`].
    pub idle_cache_bytes: usize,
    pub idle_after: Duration,
    /// How often the tip is checked while the Explorer is on screen and the node is at the tip.
    pub tip_poll: Duration,
    /// How long a check of the tip vouches for the hashes above the final heights: within it, a
    /// block asked for by height there comes from the cache.
    pub fresh: Duration,
}

impl ExplorerConfig {
    /// A 64 MiB cache, 8 MiB after 10 minutes hidden, a tip check every 2 s, and 10 s that a
    /// check vouches for.
    pub fn new(cookie_file: PathBuf) -> Self {
        Self {
            cookie_file,
            cache_bytes: 64 << 20,
            idle_cache_bytes: 8 << 20,
            idle_after: Duration::from_secs(10 * 60),
            tip_poll: Duration::from_secs(2),
            fresh: Duration::from_secs(10),
        }
    }
}

/// A cloneable handle to the explorer actor. The actor stops when the last handle is dropped.
#[derive(Clone, Debug)]
pub struct ExplorerHandle {
    commands: mpsc::Sender<actor::Command>,
    live: watch::Receiver<ExplorerLiveEvent>,
}

impl ExplorerHandle {
    /// Answers `call` for the explorer of `node`.
    pub async fn call(
        &self,
        node: &NodeId,
        call: ExplorerCall,
    ) -> Result<ExplorerReply, ExplorerError> {
        let (reply, response) = oneshot::channel();
        let command = actor::Command::Call {
            node: node.clone(),
            call,
            reply,
        };
        self.commands.send(command).await.map_err(|_| closed())?;
        response.await.map_err(|_| closed())?
    }

    /// What the explorer publishes on its own, with the id it serves: the `explorer_live`
    /// event.
    pub fn live(&self) -> watch::Receiver<ExplorerLiveEvent> {
        self.live.clone()
    }
}

fn closed() -> ExplorerError {
    ExplorerError::new(ExplorerErrorKind::Internal, "the explorer has stopped")
}

/// Starts the explorer over zebrad's RPC. Must be called inside a Tokio runtime.
pub fn spawn(inputs: ExplorerInputs, config: ExplorerConfig) -> ExplorerHandle {
    spawn_with(inputs, config, RpcSource::new)
}

/// Like [`spawn`], with the source built by `new_source` for each zebrad run.
pub(crate) fn spawn_with<S: ChainSource>(
    inputs: ExplorerInputs,
    config: ExplorerConfig,
    new_source: impl Fn(SocketAddr, PathBuf) -> Result<S, RpcError> + Send + 'static,
) -> ExplorerHandle {
    let (commands, live) = actor::start(inputs, config, new_source);
    ExplorerHandle { commands, live }
}
