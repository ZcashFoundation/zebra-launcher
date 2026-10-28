//! The dashboard's health verdict: one ordered rule set that turns the node's status, the
//! statistics, the log analysis and the updater state into a level, a headline, a reason and
//! the actions that fix it (the page specification's section 4). Pure Rust, tested natively.

// Filled in by the dashboard's model workstream; the page reads it once both land.
#![allow(dead_code)]

use std::time::SystemTime;

use launcher_types::{MetricsSnapshot, NodeStats, Status, UpdateInfo};

use crate::{insights::Insights, metrics_history::MetricsHistory, stats_history::StatsHistory};

/// How the verdict reads. The icon and the word carry it; a status colour only accompanies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    /// At the tip with peers, nothing to do.
    Healthy,
    /// A normal state that is not "healthy": syncing, starting, stopping, stopped.
    Neutral,
    /// Something to look at.
    Attention,
    /// Attention with a deadline (end of support in days, an install counting down).
    Urgent,
    /// The node is not doing its job.
    Problem,
}

/// What the verdict offers to fix the situation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Start,
    StartKillingStale,
    StartNow,
    Restart,
    Stop,
    /// Switch to the Logs page with this text filter.
    ShowInLogs(String),
    OpenConfigure,
    ResetConfig,
    CheckForUpdates,
    Install,
    CancelUpdate,
    OpenLogsFolder,
    OpenReleases,
}

impl Action {
    /// The button text.
    pub fn label(&self) -> &'static str {
        match self {
            Action::Start => "Start",
            Action::StartKillingStale => "Stop it and start",
            Action::StartNow => "Start now",
            Action::Restart => "Restart",
            Action::Stop => "Stop",
            Action::ShowInLogs(_) => "Show in logs",
            Action::OpenConfigure => "Open Configure",
            Action::ResetConfig => "Reset the configuration",
            Action::CheckForUpdates => "Check for updates",
            Action::Install => "Install",
            Action::CancelUpdate => "Cancel",
            Action::OpenLogsFolder => "Open logs folder",
            Action::OpenReleases => "Open the releases page",
        }
    }
}

/// The verdict the dashboard shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    pub level: Level,
    /// The rule that fired, 1 to 18 in the specification's order (0 while nothing is loaded).
    pub rule: u8,
    /// The condition, such as `At the chain tip` or `Sync stalled`.
    pub headline: String,
    /// One sentence with the since or for time and the evidence.
    pub reason: String,
    pub actions: Vec<Action>,
}

/// Everything the rules read, borrowed, so tests build it without signals.
pub struct HealthInput<'a> {
    pub status: &'a Status,
    pub stats: Option<&'a NodeStats>,
    pub history: &'a StatsHistory,
    pub metrics: Option<&'a MetricsSnapshot>,
    pub metrics_history: &'a MetricsHistory,
    pub insights: &'a Insights,
    pub update: Option<&'a UpdateInfo>,
    pub now: SystemTime,
    /// When the current phase (or the current rule) began, for "for N min".
    pub phase_since: SystemTime,
}

/// The verdict for `input`: the first rule that matches, in the specification's order.
pub fn health_verdict(input: &HealthInput<'_>) -> Verdict {
    let _ = input;
    Verdict {
        level: Level::Neutral,
        rule: 0,
        headline: "Connecting to the launcher…".to_owned(),
        reason: String::new(),
        actions: Vec::new(),
    }
}

/// The plain-text report "Copy summary" puts on the clipboard: versions, phase, sync, peers,
/// mempool, data folder, ports, end of support, the newest problems and the run's events.
pub fn summary_text(input: &HealthInput<'_>, verdict: &Verdict) -> String {
    let _ = input;
    format!("{}\n{}\n", verdict.headline, verdict.reason)
}
