//! One node's state in the UI: what the backend reports about the node and what the UI keeps
//! about it (the histories, the log analysis). The shell holds one per node in
//! [`AppState::nodes`](crate::app::AppState::nodes); the node pages read the selected one
//! through [`AppState::node`](crate::app::AppState::node).

use std::time::SystemTime;

use launcher_types::{
    DEFAULT_STOP_GRACE_SECS, LogLine, MetricsSnapshot, Network, NodeDetails, NodeId, NodeInfo,
    NodeKind, NodeProgram, NodeResources, NodeStats, Phase, StaleProcessInfo, Status,
};
use leptos::prelude::*;

use crate::{
    api, clock,
    insights::{Insights, RunEvent, events::status_events},
    log_view::LogBuffer,
    metrics_history::MetricsHistory,
    resource_history::ResourceHistory,
    stats_history::StatsHistory,
};

/// Which of a node's events have arrived: a snapshot fetched while one was on its way is older
/// than the event, so it is not applied over it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Seen {
    pub status: bool,
    pub stats: bool,
    pub details: bool,
    pub metrics: bool,
}

/// One node's signals. `Copy`, like [`AppState`](crate::app::AppState): every field is a signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NodeState {
    /// Who the node is: id, name, kind, network, folder, ports.
    pub info: RwSignal<NodeInfo>,
    /// The events that have arrived for the node, for its snapshots.
    pub seen: StoredValue<Seen>,
    pub status: RwSignal<Status>,
    /// Whether a status has arrived from the backend: until then `status` is the default
    /// (Stopped) and the dashboard is still connecting.
    pub status_loaded: RwSignal<bool>,
    pub stats: RwSignal<Option<NodeStats>>,
    /// The details behind `stats` (the peer list, the best block, the chain's supply and
    /// pools), from the same poll; `None` whenever `stats` is. The peer list's counts by
    /// direction also go onto the sample of the same poll in `history`.
    pub details: RwSignal<Option<NodeDetails>>,
    pub logs: RwSignal<LogBuffer>,
    /// What the log stream says about the node, folded in as batches arrive.
    pub insights: RwSignal<Insights>,
    /// The node statistics published since the launcher opened, for charts and rates.
    pub history: RwSignal<StatsHistory>,
    /// The newest reading of the node's metrics page, `None` while it does not run.
    pub metrics: RwSignal<Option<MetricsSnapshot>>,
    /// The metrics snapshots since the launcher opened.
    pub metrics_history: RwSignal<MetricsHistory>,
    /// The newest sample of the node's process tree, `None` while it has no run.
    pub resources: RwSignal<Option<NodeResources>>,
    /// The resource samples since the launcher opened.
    pub resource_history: RwSignal<ResourceHistory>,
    /// Since when the dashboard's verdict has read as it does, for its "for N min" and "stop
    /// requested N s ago". One rule, the dashboard's: the page resets it to now whenever the
    /// verdict's rule or level changes; the first status seeds it here, from `started_at`
    /// while the node is starting or running (the launcher may have been opened long after
    /// it started) and from now otherwise. A phase change on its own does not touch it: the
    /// verdict that follows it does.
    pub phase_since: RwSignal<SystemTime>,
    /// Bumped whenever the node's config file was replaced outside the Configure page's own
    /// save (a `reset_config` from any page), so the Configure page (always mounted) re-reads
    /// it.
    pub config_changed: RwSignal<u64>,
}

impl NodeState {
    /// A node nothing has been heard about yet.
    pub fn new(info: NodeInfo) -> Self {
        NodeState {
            info: RwSignal::new(info),
            seen: StoredValue::new(Seen::default()),
            status: RwSignal::new(Status::default()),
            status_loaded: RwSignal::new(false),
            stats: RwSignal::new(None),
            details: RwSignal::new(None),
            logs: RwSignal::new(LogBuffer::default()),
            insights: RwSignal::new(Insights::default()),
            history: RwSignal::new(StatsHistory::default()),
            metrics: RwSignal::new(None),
            metrics_history: RwSignal::new(MetricsHistory::default()),
            resources: RwSignal::new(None),
            resource_history: RwSignal::new(ResourceHistory::default()),
            phase_since: RwSignal::new(clock::now()),
            config_changed: RwSignal::new(0),
        }
    }

    /// Publishes a status, turning what changed since the last one into session events
    /// ([`status_events`]). The first status is the baseline: it says where the node is, not
    /// that anything happened, and it seeds `phase_since`.
    pub fn record_status(&self, status: Status) {
        let now = clock::now();
        let previous = self.status.get_untracked();
        if self.status_loaded.get_untracked() {
            for event in status_events(&previous, &status, now) {
                self.record_event(event);
            }
        } else {
            self.status_loaded.set(true);
            let since = match status.phase {
                Phase::Starting | Phase::Running => status.started_at.unwrap_or(now),
                _ => now,
            };
            self.phase_since.set(since);
        }
        self.status.set(status);
    }

    /// The backend found a zebrad from an earlier launcher run: a status change too.
    pub fn record_stale(&self, stale: StaleProcessInfo) {
        let status = Status {
            stale: Some(stale),
            ..self.status.get_untracked()
        };
        self.record_status(status);
    }

    /// Keeps an event the UI observed, for the dashboard's timeline ([`Insights::events`]
    /// merges it with what the log says).
    pub fn record_event(&self, event: RunEvent) {
        self.insights
            .update(|insights| insights.record_session_event(event));
    }

    /// Publishes new node statistics and keeps them in the history.
    pub fn record_stats(&self, stats: Option<NodeStats>) {
        if let Some(stats) = &stats {
            self.history.update(|history| history.push(stats));
        }
        self.stats.set(stats);
    }

    /// Publishes the details behind the statistics; the peer list's counts by direction go
    /// onto the sample of the same poll.
    pub fn record_details(&self, details: Option<NodeDetails>) {
        if let Some(details) = &details {
            self.history.update(|history| history.record_peers(details));
        }
        self.details.set(details);
    }

    /// Publishes a metrics reading and keeps it in the history.
    pub fn record_metrics(&self, snapshot: Option<MetricsSnapshot>) {
        if let Some(snapshot) = &snapshot {
            self.metrics_history
                .update(|history| history.push(snapshot));
        }
        self.metrics.set(snapshot);
    }

    /// Publishes a sample of the node's process tree and keeps it in the history.
    pub fn record_resources(&self, sample: Option<NodeResources>) {
        if let Some(sample) = &sample {
            self.resource_history.update(|history| history.push(sample));
        }
        self.resources.set(sample);
    }

    /// Folds a batch of log lines into the insights and the log buffer. Problems that arrive
    /// while the user is looking at this node's Logs page (`on_logs_page`) are seen as they
    /// come.
    pub fn record_logs(&self, batch: Vec<LogLine>, on_logs_page: bool) {
        let now = clock::now();
        self.insights.update(|insights| {
            insights.ingest(&batch, now);
            if on_logs_page {
                insights.mark_seen();
            }
        });
        self.logs.update(|logs| logs.merge(batch));
    }

    /// Clears the node's count of unseen problems: the user is looking at its Logs page.
    pub fn mark_problems_seen(&self) {
        if self
            .insights
            .with_untracked(|insights| insights.unseen_problems() > 0)
        {
            self.insights.update(Insights::mark_seen);
        }
    }
}

/// The first node as the UI expects it before the backend's list arrives: the bundled zebrad
/// on Mainnet with zebrad's default ports.
pub fn placeholder() -> NodeInfo {
    let (kind, network) = (NodeKind::Zebrad, Network::Mainnet);
    NodeInfo {
        id: NodeId::suggest(kind, network, &Vec::new()),
        name: NodeInfo::default_name(kind, network),
        kind,
        network,
        depends_on: None,
        data_dir: String::new(),
        own_folder: None,
        ports: kind.default_ports(network),
        program: NodeProgram::Bundled,
        autostart: true,
        stop_grace_secs: DEFAULT_STOP_GRACE_SECS,
    }
}

impl NodeState {
    /// The node's id, untracked.
    pub fn id(&self) -> NodeId {
        self.info.with_untracked(|info| info.id.clone())
    }

    pub async fn start(self) -> api::ApiResult<()> {
        api::start_node(&self.id()).await
    }

    pub async fn stop(self) -> api::ApiResult<()> {
        api::stop_node(&self.id()).await
    }

    pub async fn restart(self) -> api::ApiResult<()> {
        api::restart_node(&self.id()).await
    }

    /// Notes that an event of the kind `pick` names arrived.
    pub fn mark_seen(&self, pick: fn(&mut Seen) -> &mut bool) {
        self.seen.update_value(|seen| *pick(seen) = true);
    }

    /// Whether an event of the kind `pick` names has arrived.
    pub fn has_seen(&self, pick: fn(&Seen) -> bool) -> bool {
        self.seen.with_value(pick)
    }
}
