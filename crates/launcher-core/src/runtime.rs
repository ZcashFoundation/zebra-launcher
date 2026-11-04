//! One node's runtime: its files brought up to date, then the zebrad
//! supervisor, the RPC monitor, the metrics poller and the resource sampler
//! that run it.

use std::{io, time::Duration};

use launcher_types::{MetricsSnapshot, NodeDetails, NodeResources, NodeStats};
use tokio::sync::watch;

use crate::{
    config,
    logfile::RotatingLogConfig,
    metrics::poller::MetricsPoller,
    monitor::{Monitor, MonitorOutput},
    nodes::NodeSpec,
    paths::Paths,
    process::ZebradCommand,
    registry::RegistryConfig,
    resources::{self, ResourceSampler},
    supervisor::{self, Listener, SupervisorConfig, SupervisorHandle},
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
    /// The newest sample of the node's process tree, `None` while it has no
    /// run or on a platform that is not measured yet.
    pub resources: watch::Receiver<Option<NodeResources>>,
}

/// Rolls back an interrupted configuration change of `spec`'s node, then
/// creates its folders and brings the managed keys of its `zebrad.toml` up
/// to date. What fails is logged.
pub fn prepare_files(paths: &Paths, spec: &NodeSpec) {
    let files = spec.paths(paths);
    match config::recover(&files) {
        Ok(Some(_)) => tracing::warn!(
            node = %spec.id,
            "a configuration change was interrupted by a launcher exit; restored the previous zebrad.toml"
        ),
        Ok(None) => {}
        Err(err) => tracing::error!(
            node = %spec.id,
            %err,
            "could not check for an interrupted configuration change"
        ),
    }
    // A broken zebrad.toml is not fatal: zebrad reports it, and the user can
    // fix it in the editor.
    if let Err(err) = files
        .ensure_dirs()
        .map_err(config::ConfigError::from)
        .and_then(|()| config::ensure(&files, &spec.managed_keys(paths)))
    {
        tracing::error!(node = %spec.id, %err, "could not update the managed keys in zebrad.toml");
    }
}

/// Starts the actors of `spec`'s node as `config` says: its supervisor, which
/// runs the zebrad in `config`, checks `listeners` before every start and
/// kills zebrad when a stop takes longer than the node's stop grace, its RPC
/// monitor, its metrics poller and its resource sampler, which reads the
/// process tree of each of the supervisor's runs. Fails only when the
/// supervisor cannot start.
pub fn spawn(
    config: &RegistryConfig,
    spec: &NodeSpec,
    listeners: watch::Receiver<Vec<Listener>>,
) -> io::Result<NodeActors> {
    let node = spec.paths(&config.paths);
    let zebrad = config.zebrad_for(&spec.program)?;
    let command = ZebradCommand::new(zebrad, node.zebrad_config());
    let mut supervisor_config = SupervisorConfig::new(command);
    supervisor_config.stop_grace = Duration::from_secs(spec.stop_grace_secs);
    supervisor_config.pidfile = Some(node.pidfile());
    supervisor_config.log_file = Some(RotatingLogConfig::new(node.zebrad_log()));
    supervisor_config.listeners = listeners;
    if let Some(customize) = &config.customize {
        customize(spec, &mut supervisor_config);
    }
    let supervisor = supervisor::spawn(supervisor_config)?;

    let MonitorOutput { stats, details } = Monitor::spawn(
        supervisor.watch_status(),
        node.rpc_cookie_file(),
        config.monitor_interval,
    );
    let metrics = MetricsPoller::spawn_with(supervisor.watch_status(), config.metrics_interval);
    // In the node's span, so what the sampler logs goes to this node's log.
    let resources = tracing::info_span!("node", node = %spec.id).in_scope(|| {
        ResourceSampler::spawn_with(
            supervisor.watch_probe(),
            config.resource_interval,
            resources::platform_reader(),
        )
    });
    Ok(NodeActors {
        supervisor,
        stats,
        details,
        metrics,
        resources,
    })
}
