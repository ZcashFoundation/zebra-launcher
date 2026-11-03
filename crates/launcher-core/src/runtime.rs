//! One node's runtime: its `zebrad.toml` brought up to date, then the zebrad
//! supervisor, the RPC monitor and the metrics poller that run it.

use std::{io, path::PathBuf, time::Duration};

use launcher_types::{MetricsSnapshot, NodeDetails, NodeStats};
use tokio::sync::watch;

use crate::{
    config::{self, ManagedKeys},
    logfile::RotatingLogConfig,
    metrics::poller::MetricsPoller,
    monitor::{self, Monitor, MonitorOutput},
    paths::NodePaths,
    process::ZebradCommand,
    supervisor::{self, SupervisorConfig, SupervisorHandle},
};

/// What [`spawn`] starts for a node.
#[derive(Debug)]
pub struct NodeActors {
    /// The zebrad supervisor.
    pub supervisor: SupervisorHandle,
    /// The latest RPC poll, `None` while zebrad is not running.
    pub stats: watch::Receiver<Option<NodeStats>>,
    /// The details behind `stats`, from the same poll.
    pub details: watch::Receiver<Option<NodeDetails>>,
    /// The latest read of zebrad's Prometheus metrics page, `None` while
    /// zebrad is not running or the page stopped answering.
    pub metrics: watch::Receiver<Option<MetricsSnapshot>>,
}

/// Brings the managed keys of `node`'s `zebrad.toml` up to date (`keys`),
/// then starts its supervisor, which runs `zebrad` and kills it when a stop
/// takes longer than `stop_grace_secs`, its RPC monitor and its metrics
/// poller. Fails only when the supervisor cannot start.
pub fn spawn(
    node: &NodePaths,
    keys: &ManagedKeys,
    zebrad: PathBuf,
    shutdown_on_stdin_close: bool,
    stop_grace_secs: u64,
) -> io::Result<NodeActors> {
    // A broken zebrad.toml is not fatal: zebrad reports it, and the user can
    // fix it in the editor.
    if let Err(err) = config::ensure(node, keys) {
        tracing::error!(%err, "could not update the managed keys in zebrad.toml");
    }

    let mut command = ZebradCommand::new(zebrad, node.zebrad_config());
    command.shutdown_on_stdin_close = shutdown_on_stdin_close;
    let mut supervisor_config = SupervisorConfig::new(command);
    supervisor_config.stop_grace = Duration::from_secs(stop_grace_secs);
    supervisor_config.pidfile = Some(node.pidfile());
    supervisor_config.log_file = Some(RotatingLogConfig::new(node.zebrad_log()));
    let supervisor = supervisor::spawn(supervisor_config)?;

    let MonitorOutput { stats, details } = Monitor::spawn(
        supervisor.watch_status(),
        node.rpc_cookie_file(),
        monitor::DEFAULT_INTERVAL,
    );
    let metrics = MetricsPoller::spawn(supervisor.watch_status());
    Ok(NodeActors {
        supervisor,
        stats,
        details,
        metrics,
    })
}
