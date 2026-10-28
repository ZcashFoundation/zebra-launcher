//! The dashboard's health verdict: one ordered rule set that turns the node's status, the
//! statistics, the log analysis and the updater state into a level, a headline, a reason and
//! the actions that fix it (the page specification's section 4). Pure Rust, tested natively.
//!
//! Cause comes before symptom: a failed process before a silent RPC, no peers before a stalled
//! sync, so the operator reads the reason, not the consequence. The same facts feed the hero
//! line and the tiles, so they never disagree with the verdict.

use std::time::{Duration, SystemTime};

use launcher_types::{
    EOS_WARNING_WINDOW, EosInfo, ExitClass, MetricsSnapshot, NodeStats, Phase, RecoveryAction,
    StaleProcessInfo, Status, UpdateInfo, UpdateState, eos_within,
};

use crate::{
    charts::{eta_text, percent_1},
    format::{
        bytes, exit_class_text, exit_code_text, local_date_short, local_datetime_short, relative,
        rough_duration, thousands,
    },
    insights::{Insights, ProblemLevel, Run, SyncSnapshot, SyncStage, pick_filter},
    metrics_history::{MetricsHistory, names},
    stats_history::{Field, HERO_RATE_WINDOW, Sample, StatsHistory},
};

/// A statistics sample older than this, while zebrad runs, means the RPC is not answering.
pub const STALE_AFTER: Duration = Duration::from_secs(30);
/// A running zebrad gets this long to answer its first RPC poll.
pub const RPC_FIRST_ANSWER: Duration = Duration::from_secs(60);
/// A silent RPC this long is a problem, not a warning.
pub const RPC_SILENT_PROBLEM: Duration = Duration::from_secs(5 * 60);
/// A start without an RPC endpoint this long is slow.
pub const SLOW_START: Duration = Duration::from_secs(5 * 60);
/// No peers this long is a problem.
pub const NO_PEERS_FOR: Duration = Duration::from_secs(2 * 60);
/// Fewer peers than this for [`FEW_PEERS_FOR`] wants a look.
pub const FEW_PEERS: u32 = 3;
pub const FEW_PEERS_FOR: Duration = Duration::from_secs(5 * 60);
/// No new block while behind the tip: a warning after this long, a problem after
/// [`STALL_PROBLEM`].
pub const STALL_ATTENTION: Duration = Duration::from_secs(2 * 60);
pub const STALL_PROBLEM: Duration = Duration::from_secs(10 * 60);
/// No new block at the tip this long wants a look. zebrad's tip estimate keeps creeping a
/// block per 75 s meanwhile: a frozen height under a moving estimate is what a tip stall
/// looks like.
pub const STALL_AT_TIP: Duration = Duration::from_secs(10 * 60);
/// Errors in the log newer than this want a look.
pub const RECENT_ERRORS: Duration = Duration::from_secs(10 * 60);
/// A crash the node restarted from within this long is still worth a mention.
pub const RECENT_CRASH: Duration = Duration::from_secs(60 * 60);
/// This many crashes in a row is a crash loop.
pub const CRASH_LOOP: u32 = 3;
/// The end of support this close is urgent.
pub const EOS_URGENT: Duration = Duration::from_secs(3 * 24 * 60 * 60);
/// The checkpoint verifier is still at work while its height is within this many blocks of
/// the verified height; further behind, it has finished and full validation runs.
pub const CHECKPOINT_NEAR_BLOCKS: u64 = 5_000;

/// A log timestamp may run a little ahead of the supervisor's spawn time; a line this close
/// to the start still belongs to the run.
const RUN_SLACK: Duration = Duration::from_secs(2);
/// How many characters a log filter cut from an exit detail keeps.
const FILTER_CHARS: usize = 60;
/// How many problems and run events the summary lists.
const SUMMARY_PROBLEMS: usize = 5;
const SUMMARY_EVENTS: usize = 8;

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

impl Level {
    /// The chip's word. A neutral verdict has none of its own: its headline (Syncing,
    /// Starting, Stopping, Stopped, Running) is the word.
    pub fn word(self) -> Option<&'static str> {
        match self {
            Level::Healthy => Some("Healthy"),
            Level::Neutral => None,
            Level::Attention | Level::Urgent => Some("Attention"),
            Level::Problem => Some("Problem"),
        }
    }

    /// The status tone's name, for a class or token suffix: `good`, `neutral`, `warning`,
    /// `serious` or `critical`.
    pub fn tone(self) -> &'static str {
        match self {
            Level::Healthy => "good",
            Level::Neutral => "neutral",
            Level::Attention => "warning",
            Level::Urgent => "serious",
            Level::Problem => "critical",
        }
    }
}

/// What the verdict offers to fix the situation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Start,
    StartKillingStale,
    StartNow,
    Restart,
    /// Switch to the Logs page with this text filter (empty: no filter).
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

    fn from_recovery(recovery: RecoveryAction) -> Self {
        match recovery {
            RecoveryAction::OpenLogs => Action::OpenLogsFolder,
            RecoveryAction::ResetConfig => Action::ResetConfig,
            RecoveryAction::OpenReleasesPage => Action::OpenReleases,
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

impl Verdict {
    fn new(
        level: Level,
        rule: u8,
        headline: impl Into<String>,
        reason: impl Into<String>,
        actions: Vec<Action>,
    ) -> Self {
        Verdict {
            level,
            rule,
            headline: headline.into(),
            reason: reason.into(),
            actions,
        }
    }

    /// The verdict before the launcher's first status arrives: rule 0, no actions. The page
    /// shows it until then; [`health_verdict`] needs a status.
    pub fn connecting() -> Self {
        Verdict::new(
            Level::Neutral,
            0,
            "Connecting to the launcher…",
            "",
            Vec::new(),
        )
    }

    /// The chip and the headline as one line: `Problem · Sync stalled`, or `Syncing`.
    pub fn title(&self) -> String {
        match self.level.word() {
            Some(word) => format!("{word} · {}", self.headline),
            None => self.headline.clone(),
        }
    }
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
    let facts = Facts::new(input);
    // 1: the launcher itself did not come back healthy from an update.
    if let Some(failure) = input
        .update
        .and_then(|update| update.health_failure.as_ref())
    {
        let actions = failure
            .recovery
            .iter()
            .map(|recovery| Action::from_recovery(*recovery))
            .collect();
        return Verdict::new(
            Level::Problem,
            1,
            "Launcher update failed",
            failure.message.clone(),
            actions,
        );
    }
    // 2 to 6: every phase but running has its own row.
    match &input.status.phase {
        Phase::Failed { class } => return facts.failed(*class),
        Phase::Backoff { until, crashes } => return facts.backoff(*until, *crashes),
        Phase::Stopping => return facts.stopping(),
        Phase::Stopped => return facts.stopped(),
        Phase::Starting => return facts.starting(),
        Phase::Running => {}
    }
    // 7 to 16: what can be wrong while running, cause before symptom; then 17 and 18,
    // running as it should.
    facts
        .rpc_silent()
        .or_else(|| facts.no_peers())
        .or_else(|| facts.stalled())
        .or_else(|| facts.quiet_at_tip())
        .or_else(|| facts.urgent())
        .or_else(|| facts.few_peers())
        .or_else(|| facts.slow())
        .or_else(|| facts.recent_errors())
        .or_else(|| facts.restarted_after_crash())
        .or_else(|| facts.eos_warning())
        .unwrap_or_else(|| facts.running())
}

/// The facts the rules share, read once.
struct Facts<'a> {
    input: &'a HealthInput<'a>,
    /// The newest statistics of the current run: the published value, or the last sample
    /// once zebrad stopped publishing. `None` before the run's first answer.
    latest: Option<Sample>,
}

impl<'a> Facts<'a> {
    fn new(input: &'a HealthInput<'a>) -> Self {
        let mut facts = Facts {
            input,
            latest: None,
        };
        let latest = input
            .history
            .sample_for(input.stats)
            .filter(|sample| facts.this_run(sample.at));
        facts.latest = latest;
        facts
    }

    fn now(&self) -> SystemTime {
        self.input.now
    }

    fn status(&self) -> &'a Status {
        self.input.status
    }

    fn started_at(&self) -> Option<SystemTime> {
        self.status().started_at
    }

    /// How long ago `since` was; zero for a time still to come.
    fn ago(&self, since: SystemTime) -> Duration {
        self.now().duration_since(since).unwrap_or(Duration::ZERO)
    }

    /// How long the current phase has been going.
    fn phase_for(&self) -> Duration {
        self.ago(self.input.phase_since)
    }

    /// How long the run has been going (the phase, without a start time).
    fn run_for(&self) -> Duration {
        self.ago(self.started_at().unwrap_or(self.input.phase_since))
    }

    /// [`this_run`] for the current run.
    fn this_run(&self, at: SystemTime) -> bool {
        this_run(self.started_at(), at)
    }

    /// The log's current run, when it is this run.
    fn run(&self) -> Option<&'a Run> {
        self.input
            .insights
            .current_run()
            .filter(|run| self.this_run(run.started_at))
    }

    /// The newest progress line, when it is this run's.
    fn sync_snapshot(&self) -> Option<&'a SyncSnapshot> {
        self.input
            .insights
            .sync()
            .filter(|sync| self.this_run(sync.at))
    }

    fn stage(&self) -> Option<SyncStage> {
        self.sync_snapshot().map(|sync| sync.stage)
    }

    /// When the height last went up in this run.
    fn last_block_at(&self) -> Option<SystemTime> {
        self.input
            .history
            .last_height_change_at()
            .filter(|at| self.this_run(*at))
    }

    /// Since when `field` has not changed in this run, as far as the samples show: its last
    /// change, or else the run's oldest sample (a lower bound).
    fn unchanged_since(&self, field: Field) -> Option<SystemTime> {
        let changed = match field {
            Field::Height => self.last_block_at(),
            field => self
                .input
                .history
                .last_change_at(field)
                .filter(|at| self.this_run(*at)),
        };
        changed.or_else(|| {
            self.input
                .history
                .oldest_from(self.started_at())
                .map(|sample| sample.at)
        })
    }

    /// How long no new block has arrived: from the samples, or longer when zebrad's own
    /// progress line, newer than the last rise we saw, says so.
    fn quiet_for(&self) -> Option<Duration> {
        let own = self
            .unchanged_since(Field::Height)
            .map(|since| self.ago(since));
        let rise = self.last_block_at();
        let zebrad = self
            .sync_snapshot()
            .filter(|sync| rise.is_none_or(|rise| sync.at >= rise))
            .and_then(|sync| {
                let said = humantime_secs(sync.since_last_block.as_deref()?)?;
                Some(Duration::from_secs(said) + self.ago(sync.at))
            });
        match (own, zebrad) {
            (Some(own), Some(zebrad)) => Some(own.max(zebrad)),
            (own, zebrad) => own.or(zebrad),
        }
    }

    /// A metric from the newest snapshot: the published one, or the history's when it is this
    /// run's. A snapshot withdrawn from an earlier process says nothing about this one.
    fn metric(&self, name: &str) -> Option<f64> {
        self.input
            .metrics
            .and_then(|snapshot| snapshot.family(name)?.single())
            .filter(|value| value.is_finite())
            .or_else(|| {
                let history = self.input.metrics_history;
                history
                    .latest()
                    .filter(|newest| self.this_run(newest.at))
                    .and_then(|_| history.newest_value(name))
            })
    }

    fn metric_count(&self, name: &str) -> Option<u64> {
        self.metric(name).map(|value| value.round().max(0.0) as u64)
    }

    fn downloads_in_flight(&self) -> Option<u64> {
        self.metric_count(names::DOWNLOADS_IN_FLIGHT)
    }

    fn ready_peers(&self) -> Option<u64> {
        self.metric_count(names::READY_PEERS)
    }

    /// The sync hero's figures: `94.6 % · 3,747 blocks/min · about 44 min left`, on the fixed
    /// [`HERO_RATE_WINDOW`] over this run's samples, so the downtime before a restart does not
    /// dilute the pace.
    fn sync_line(&self) -> String {
        let Some(latest) = self.latest else {
            return "Waiting for zebrad's RPC".to_owned();
        };
        let history = self.input.history;
        let (started, now) = (self.started_at(), self.now());
        let mut parts = vec![percent_1(latest.verification_progress)];
        match history.block_rate_from(Some(HERO_RATE_WINDOW), started, now) {
            // Only a running node is estimating anything.
            None if self.status().phase == Phase::Running => parts.push("estimating…".to_owned()),
            None => {}
            Some(rate) => {
                parts.push(format!(
                    "{} blocks/min",
                    thousands(rate.round().max(0.0) as u64)
                ));
                parts.push(
                    history
                        .eta_to_tip_from(Some(HERO_RATE_WINDOW), started, now)
                        .map_or_else(|| "not catching up".to_owned(), eta_text),
                );
            }
        }
        parts.join(" · ")
    }

    /// [`last_block_text`] for this run.
    fn last_block_text(&self) -> Option<String> {
        last_block_text(
            self.input.history,
            self.input.insights.sync(),
            self.started_at(),
            self.now(),
        )
    }

    /// The running release's end of support and the sentence about it.
    fn eos(&self) -> Option<(&'a EosInfo, String)> {
        let stats = self.input.stats?;
        let eos = stats.eos.as_ref()?;
        let sentence = format!(
            "zebrad {} stops working at block {} (about {}). Update the launcher before then.",
            stats.zebrad_version,
            thousands(eos.block_height),
            local_date_short(eos.estimated_time)
        );
        Some((eos, sentence))
    }

    fn eos_headline(&self, eos: &EosInfo) -> String {
        if eos.estimated_time <= self.now() {
            "End of support reached".to_owned()
        } else {
            format!(
                "End of support {}",
                relative(eos.estimated_time, self.now())
            )
        }
    }

    // Rule 2.
    fn failed(&self, class: ExitClass) -> Verdict {
        let status = self.status();
        let exit = status.last_exit.as_ref();
        let detail = exit.and_then(|exit| exit.detail.as_deref());
        let mut parts = Vec::new();
        match (class, &status.stale) {
            (ExitClass::DbLocked, Some(stale)) => parts.push(stale_text(stale)),
            _ => {
                if let Some(detail) = detail {
                    parts.push(first_line(detail).to_owned());
                }
            }
        }
        parts.push(match exit {
            Some(exit) => format!(
                "exited {} ({})",
                relative(exit.at, self.now()),
                exit_code_text(exit)
            ),
            None => for_text(self.phase_for()),
        });
        let actions = match class {
            ExitClass::ConfigError => vec![Action::OpenConfigure, Action::ResetConfig],
            ExitClass::EosHalt => Vec::new(),
            ExitClass::DbLocked if status.stale.is_some() => vec![Action::StartKillingStale],
            ExitClass::DbLocked | ExitClass::Clean => vec![Action::Start],
            ExitClass::PortInUse => vec![
                Action::OpenConfigure,
                Action::ShowInLogs(log_filter(detail, "already in use")),
            ],
            ExitClass::SpawnFailed => {
                vec![
                    Action::Start,
                    Action::ShowInLogs(log_filter(detail, "zebrad")),
                ]
            }
            ExitClass::Crash => vec![
                Action::Start,
                Action::ShowInLogs(log_filter(detail, "panicked")),
            ],
        };
        Verdict::new(
            Level::Problem,
            2,
            sentence(exit_class_text(class)),
            parts.join(" · "),
            actions,
        )
    }

    // Rule 3.
    fn backoff(&self, until: SystemTime, crashes: u32) -> Verdict {
        let (level, headline) = if crashes >= CRASH_LOOP {
            (Level::Problem, "Crash loop")
        } else {
            (Level::Attention, "zebrad crashed")
        };
        let restart = match until.duration_since(self.now()) {
            Ok(left) if !left.is_zero() => format!("restarting in {}", rough_duration(left)),
            _ => "restarting now".to_owned(),
        };
        let detail = self
            .status()
            .last_exit
            .as_ref()
            .and_then(|exit| exit.detail.as_deref());
        // The headline already says it crashed; a streak is worth a mention from the second.
        let mut parts = Vec::new();
        if crashes > 1 {
            parts.push(format!("{} in a row", crashes_text(crashes)));
        }
        parts.push(restart);
        if let Some(detail) = detail {
            parts.push(first_line(detail).to_owned());
        }
        Verdict::new(
            level,
            3,
            headline,
            parts.join(" · "),
            vec![
                Action::StartNow,
                Action::ShowInLogs(log_filter(detail, "panicked")),
            ],
        )
    }

    // Rule 4.
    fn stopping(&self) -> Verdict {
        let mut reason = format!(
            "Stop requested {}",
            relative(self.input.phase_since, self.now())
        );
        if !self.status().stop_is_graceful {
            reason.push_str(" · a hard stop: zebrad gets no shutdown");
        }
        Verdict::new(Level::Neutral, 4, "Stopping", reason, Vec::new())
    }

    // Rule 5.
    fn stopped(&self) -> Verdict {
        let status = self.status();
        if let Some(stale) = &status.stale {
            return Verdict::new(
                Level::Attention,
                5,
                "An earlier zebrad is still running",
                stale_text(stale),
                vec![Action::StartKillingStale],
            );
        }
        let reason = match &status.last_exit {
            Some(exit) => format!(
                "{} ({}), {}",
                sentence(exit_class_text(exit.class)),
                exit_code_text(exit),
                relative(exit.at, self.now())
            ),
            None => "Not started yet".to_owned(),
        };
        Verdict::new(Level::Neutral, 5, "Stopped", reason, vec![Action::Start])
    }

    // Rule 6.
    fn starting(&self) -> Verdict {
        let elapsed = self.run_for();
        let slow = self.status().rpc_addr.is_none() && elapsed >= SLOW_START;
        let step = self
            .run()
            .and_then(|run| run.milestones.last())
            .map_or("Waiting for zebrad's first line", |(milestone, _)| {
                milestone.label()
            });
        let reason = format!("{step} · {}", for_text(elapsed));
        if slow {
            Verdict::new(
                Level::Attention,
                6,
                "Slow start",
                reason,
                vec![Action::ShowInLogs(String::new())],
            )
        } else {
            Verdict::new(Level::Neutral, 6, "Starting", reason, Vec::new())
        }
    }

    // Rule 7.
    fn rpc_silent(&self) -> Option<Verdict> {
        let age = match self.latest {
            Some(sample) => {
                let age = self.ago(sample.at);
                if age <= STALE_AFTER {
                    return None;
                }
                age
            }
            None => {
                let age = self.run_for();
                if age < RPC_FIRST_ANSWER {
                    return None;
                }
                age
            }
        };
        let level = if age >= RPC_SILENT_PROBLEM {
            Level::Problem
        } else {
            Level::Attention
        };
        let reason = self
            .input
            .stats
            .and_then(|stats| stats.last_error.clone())
            .unwrap_or_else(|| {
                let pid = self
                    .status()
                    .pid
                    .map_or_else(String::new, |pid| format!(" (pid {pid})"));
                format!(
                    "No answer for {} · zebrad is still running{pid}",
                    rough_duration(age)
                )
            });
        let actions = if level == Level::Problem {
            vec![Action::Restart]
        } else {
            Vec::new()
        };
        Some(Verdict::new(level, 7, "RPC not answering", reason, actions))
    }

    // Rule 8.
    fn no_peers(&self) -> Option<Verdict> {
        self.latest?;
        let since = self.input.history.low_peers_since(1, self.started_at())?;
        let quiet = self.ago(since);
        if quiet < NO_PEERS_FOR {
            return None;
        }
        let peers = self.input.insights.peers();
        let mut parts = vec![
            format!("No peers {}", for_text(quiet)),
            format!("{} outbound connections failed", peers.failed_connections),
            format!("{} initial peers failed", peers.initial_errors),
        ];
        if let Some(ready) = self.ready_peers() {
            parts.push(format!("ready peers {ready}"));
        }
        Some(Verdict::new(
            Level::Problem,
            8,
            "No peers",
            parts.join(" · "),
            vec![Action::ShowInLogs("peer".to_owned()), Action::OpenConfigure],
        ))
    }

    // Rule 9.
    fn stalled(&self) -> Option<Verdict> {
        let latest = self.latest?;
        if latest.at_tip() {
            return None;
        }
        let stage_stalled = matches!(
            self.stage(),
            Some(SyncStage::Stalled | SyncStage::GenesisStalled)
        );
        let quiet = self.quiet_for();
        let level = if stage_stalled || quiet.is_some_and(|quiet| quiet >= STALL_PROBLEM) {
            Level::Problem
        } else if quiet.is_some_and(|quiet| quiet >= STALL_ATTENTION) {
            Level::Attention
        } else {
            return None;
        };
        let no_block = match quiet {
            Some(quiet) => format!("No new block {}", for_text(quiet)),
            None => "No new block".to_owned(),
        };
        let mut facts = vec![
            format!("height {}", thousands(latest.height)),
            peers_text(latest.peers),
        ];
        if let Some(downloads) = self.downloads_in_flight() {
            facts.push(format!("{downloads} downloads in flight"));
        }
        let facts = facts.join(" · ");
        let mut actions = vec![Action::ShowInLogs("sync".to_owned())];
        if level == Level::Problem {
            actions.push(Action::Restart);
            Some(Verdict::new(
                level,
                9,
                "Sync stalled",
                format!("{no_block} at {facts}"),
                actions,
            ))
        } else {
            Some(Verdict::new(level, 9, no_block, sentence(&facts), actions))
        }
    }

    // Rule 10. zebrad's own stall line counts too, since rule 9 leaves the tip to this rule.
    fn quiet_at_tip(&self) -> Option<Verdict> {
        let latest = self.latest?;
        if !latest.at_tip() || latest.peers == 0 {
            return None;
        }
        let quiet = self.quiet_for()?;
        let stage_stalled = self.stage() == Some(SyncStage::Stalled);
        if quiet < STALL_AT_TIP && !stage_stalled {
            return None;
        }
        let reason = match latest.remaining() {
            0 | 1 => "Blocks usually arrive every 75 s · the tip estimate has not moved".to_owned(),
            behind => format!(
                "Blocks usually arrive every 75 s · the tip estimate is {} blocks ahead",
                thousands(behind)
            ),
        };
        Some(Verdict::new(
            Level::Attention,
            10,
            format!("No new block {}", for_text(quiet)),
            reason,
            vec![Action::ShowInLogs("sync".to_owned())],
        ))
    }

    // Rule 11.
    fn urgent(&self) -> Option<Verdict> {
        if let Some((eos, sentence)) = self.eos()
            && eos_within(eos.estimated_time, self.now(), EOS_URGENT)
        {
            return Some(Verdict::new(
                Level::Urgent,
                11,
                self.eos_headline(eos),
                sentence,
                vec![Action::CheckForUpdates],
            ));
        }
        if let Some(UpdateState::ReadyToInstall {
            version,
            countdown_secs: Some(secs),
        }) = self.input.update.map(|update| &update.state)
        {
            return Some(Verdict::new(
                Level::Urgent,
                11,
                format!("Update installs in {secs} s"),
                format!(
                    "Launcher {version} is ready · the launcher restarts and brings zebrad back"
                ),
                vec![Action::Install, Action::CancelUpdate],
            ));
        }
        None
    }

    // Rule 12.
    fn few_peers(&self) -> Option<Verdict> {
        let latest = self.latest?;
        let since = self
            .input
            .history
            .low_peers_since(FEW_PEERS, self.started_at())?;
        let quiet = self.ago(since);
        if quiet < FEW_PEERS_FOR {
            return None;
        }
        Some(Verdict::new(
            Level::Attention,
            12,
            "Few peers",
            format!("{} {}", peers_text(latest.peers), for_text(quiet)),
            vec![Action::ShowInLogs("peer".to_owned())],
        ))
    }

    // Rule 13.
    fn slow(&self) -> Option<Verdict> {
        (self.stage() == Some(SyncStage::Slow)).then(|| {
            Verdict::new(
                Level::Attention,
                13,
                "Syncing slowly",
                self.sync_line(),
                Vec::new(),
            )
        })
    }

    // Rule 14.
    fn recent_errors(&self) -> Option<Verdict> {
        let problems = self.input.insights.problems(usize::MAX);
        let recent: Vec<_> = problems
            .iter()
            .filter(|problem| {
                problem.level >= ProblemLevel::Error
                    && self.this_run(problem.last)
                    && problem
                        .last
                        .checked_add(RECENT_ERRORS)
                        .is_none_or(|until| until >= self.now())
            })
            .collect();
        let newest = recent.first()?;
        // Lines, not distinct patterns: the count the Recent problems rows show as ×N.
        let total: u64 = recent.iter().map(|problem| u64::from(problem.count)).sum();
        let headline = match total {
            1 => "1 error in the log".to_owned(),
            n => format!("{n} errors in the log"),
        };
        Some(Verdict::new(
            Level::Attention,
            14,
            headline,
            newest.sample.clone(),
            vec![Action::ShowInLogs(pick_filter(newest))],
        ))
    }

    // Rule 15.
    fn restarted_after_crash(&self) -> Option<Verdict> {
        let exit = self.status().last_exit.as_ref()?;
        if exit.requested || exit.class == ExitClass::Clean || self.started_at()? <= exit.at {
            return None;
        }
        if self.now().duration_since(exit.at).ok()? > RECENT_CRASH {
            return None;
        }
        let mut reason = sentence(&format!(
            "{} {}",
            exit_class_text(exit.class),
            relative(exit.at, self.now())
        ));
        let detail = exit.detail.as_deref();
        if let Some(detail) = detail {
            reason.push_str(" · ");
            reason.push_str(first_line(detail));
        }
        Some(Verdict::new(
            Level::Attention,
            15,
            "Restarted after a crash",
            reason,
            vec![Action::ShowInLogs(log_filter(detail, "panicked"))],
        ))
    }

    // Rule 16.
    fn eos_warning(&self) -> Option<Verdict> {
        let (eos, sentence) = self.eos()?;
        eos_within(eos.estimated_time, self.now(), EOS_WARNING_WINDOW).then(|| {
            Verdict::new(
                Level::Attention,
                16,
                self.eos_headline(eos),
                sentence,
                vec![Action::CheckForUpdates],
            )
        })
    }

    // Rules 17 and 18.
    fn running(&self) -> Verdict {
        match self.latest {
            None => Verdict::new(
                Level::Neutral,
                17,
                "Running",
                format!("Waiting for zebrad's RPC · {}", for_text(self.run_for())),
                Vec::new(),
            ),
            Some(latest) if !latest.at_tip() => {
                Verdict::new(Level::Neutral, 17, "Syncing", self.sync_line(), Vec::new())
            }
            Some(latest) => {
                let mut parts = vec![peers_text(latest.peers)];
                parts.extend(self.last_block_text());
                parts.push(format!("RPC answered {}", relative(latest.at, self.now())));
                Verdict::new(
                    Level::Healthy,
                    18,
                    "At the chain tip",
                    parts.join(" · "),
                    Vec::new(),
                )
            }
        }
    }
}

/// Whether a time belongs to the run started at `started_at`: not before it, give or take
/// [`RUN_SLACK`] for zebrad's own clock stamps. Everything counts without a start time. The
/// verdict's facts and the page's latest sample are filtered with it, so they agree on which
/// run a figure belongs to.
pub fn this_run(started_at: Option<SystemTime>, at: SystemTime) -> bool {
    started_at.is_none_or(|started| at.checked_add(RUN_SLACK).is_none_or(|at| at >= started))
}

/// `last block 48 s ago`: the newest height rise among this run's samples or, while they show
/// none, zebrad's own words in its progress line (`5s`, `1m 2s`, stamped when it said so) aged
/// to `now` and written in the page's units. The hero line and the verdict both read it, so
/// they never disagree.
pub fn last_block_text(
    history: &StatsHistory,
    sync: Option<&SyncSnapshot>,
    started_at: Option<SystemTime>,
    now: SystemTime,
) -> Option<String> {
    if let Some(at) = history
        .last_height_change_at()
        .filter(|at| this_run(started_at, *at))
    {
        return Some(format!("last block {}", relative(at, now)));
    }
    let sync = sync.filter(|sync| this_run(started_at, sync.at))?;
    let said = Duration::from_secs(humantime_secs(sync.since_last_block.as_deref()?)?);
    let since = said + now.duration_since(sync.at).unwrap_or(Duration::ZERO);
    Some(format!("last block {} ago", rough_duration(since)))
}

/// `for 12 min`.
fn for_text(duration: Duration) -> String {
    format!("for {}", rough_duration(duration))
}

/// `12 peers`, `1 peer`.
fn peers_text(peers: u32) -> String {
    match peers {
        1 => "1 peer".to_owned(),
        n => format!("{n} peers"),
    }
}

/// `3 crashes`, `1 crash`.
fn crashes_text(crashes: u32) -> String {
    match crashes {
        1 => "1 crash".to_owned(),
        n => format!("{n} crashes"),
    }
}

/// `pid 4242, /opt/zebrad, from an earlier launcher run`.
fn stale_text(stale: &StaleProcessInfo) -> String {
    format!(
        "pid {}, {}, from an earlier launcher run",
        stale.pid, stale.binary
    )
}

/// The first line of an exit detail, trimmed.
fn first_line(detail: &str) -> &str {
    detail.lines().next().unwrap_or("").trim()
}

/// A Logs page filter from an exit detail: its first line cut to [`FILTER_CHARS`], or
/// `fallback` without one.
fn log_filter(detail: Option<&str>, fallback: &str) -> String {
    let line = detail.map(first_line).unwrap_or("");
    if line.is_empty() {
        return fallback.to_owned();
    }
    line.chars()
        .take(FILTER_CHARS)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// `text` as a sentence: its first letter upper case, unless it starts with a name written
/// in lower case (`zebrad`).
fn sentence(text: &str) -> String {
    if text.starts_with("zebrad") {
        return text.to_owned();
    }
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The seconds in a `humantime` duration as zebrad logs it: `5s`, `7m 2s`, `1h 2m`,
/// `2days 3h`. Parts under a second count as nothing; an unknown unit is no duration.
pub fn humantime_secs(text: &str) -> Option<u64> {
    let mut total = 0u64;
    let mut any = false;
    for part in text.split_whitespace() {
        let digits = part.find(|c: char| !c.is_ascii_digit())?;
        let (number, unit) = part.split_at(digits);
        let number: u64 = number.parse().ok()?;
        let seconds = match unit {
            "s" | "sec" | "secs" => 1,
            "m" | "min" | "mins" => 60,
            "h" | "hr" | "hours" | "hour" => 3600,
            "d" | "day" | "days" => 86_400,
            "month" | "months" => 30 * 86_400,
            "y" | "year" | "years" => 365 * 86_400,
            "ms" | "us" | "µs" | "ns" => 0,
            _ => return None,
        };
        total = total.saturating_add(number.saturating_mul(seconds));
        any = true;
    }
    any.then_some(total)
}

/// The plain-text report "Copy summary" puts on the clipboard: the verdict, versions, phase,
/// sync, peers, mempool, disk, ports, end of support, the newest problems and the run's events,
/// with absolute local times.
pub fn summary_text(input: &HealthInput<'_>, verdict: &Verdict) -> String {
    let facts = Facts::new(input);
    let now = input.now;
    let status = input.status;
    let mut lines = vec![format!("Zebra Launcher · {}", local_datetime_short(now))];

    lines.push(format!("Verdict: {}", verdict.title()));
    if !verdict.reason.is_empty() {
        lines.push(format!("  {}", verdict.reason));
    }

    let run = input.insights.current_run();
    let mut versions = Vec::new();
    if let Some(update) = input.update {
        versions.push(format!("launcher {}", update.current_version));
    }
    let zebrad = input
        .stats
        .map(|stats| stats.zebrad_version.clone())
        .or_else(|| run.and_then(|run| run.version.clone()));
    if let Some(zebrad) = zebrad {
        versions.push(format!("zebrad {zebrad}"));
    }
    let network = input
        .stats
        .map(|stats| stats.chain.clone())
        .or_else(|| run.and_then(|run| run.network.clone()));
    if let Some(network) = network {
        versions.push(format!("chain {network}"));
    }
    if !versions.is_empty() {
        lines.push(format!("Versions: {}", versions.join(" · ")));
    }

    let mut phase = match &status.phase {
        Phase::Stopped => "Stopped".to_owned(),
        Phase::Starting => "Starting".to_owned(),
        Phase::Running => "Running".to_owned(),
        Phase::Stopping => "Stopping".to_owned(),
        Phase::Backoff { until, crashes } => format!(
            "Restarting {} ({})",
            match until.duration_since(now) {
                Ok(left) if !left.is_zero() => format!("in {}", rough_duration(left)),
                _ => "now".to_owned(),
            },
            crashes_text(*crashes)
        ),
        Phase::Failed { class } => format!("Failed: {}", exit_class_text(*class)),
    };
    if let Some(started) = status.started_at {
        phase.push_str(&format!(" since {}", local_datetime_short(started)));
    }
    if let Some(pid) = status.pid {
        phase.push_str(&format!(" (pid {pid})"));
    }
    if let Some(stale) = &status.stale {
        phase.push_str(&format!(
            " · an earlier zebrad is still running: {}",
            stale_text(stale)
        ));
    }
    lines.push(format!("Phase: {phase}"));
    if let Some(exit) = &status.last_exit {
        let mut text = format!(
            "{} ({}), {}",
            exit_class_text(exit.class),
            exit_code_text(exit),
            local_datetime_short(exit.at)
        );
        if let Some(detail) = &exit.detail {
            text.push_str(" · ");
            text.push_str(first_line(detail));
        }
        lines.push(format!("Last exit: {text}"));
    }

    match facts.latest.or_else(|| input.history.sample_for(None)) {
        None => lines.push("Sync: no answer from zebrad's RPC yet".to_owned()),
        Some(sample) => {
            let mut parts = if sample.at_tip() {
                vec![format!(
                    "at the chain tip · height {}",
                    thousands(sample.height)
                )]
            } else {
                vec![
                    format!(
                        "{} of {}",
                        thousands(sample.height),
                        thousands(sample.estimated_height)
                    ),
                    facts.sync_line(),
                ]
            };
            if let Some(at) = facts.last_block_at() {
                parts.push(format!("last block {}", local_datetime_short(at)));
            }
            if let Some(height) = facts.metric_count(names::CHECKPOINT_HEIGHT) {
                parts.push(format!("checkpoints verified to {}", thousands(height)));
            }
            parts.push(format!("RPC answered {}", local_datetime_short(sample.at)));
            lines.push(format!("Sync: {}", parts.join(" · ")));

            let mut peers = vec![peers_text(sample.peers)];
            if let Some((inbound, outbound)) = sample.peer_split() {
                peers.push(format!("{outbound} outbound · {inbound} inbound"));
            }
            if let Some(ready) = facts.ready_peers() {
                peers.push(format!("{ready} ready"));
            }
            lines.push(format!("Peers: {}", peers.join(" · ")));

            let mut mempool = vec![format!(
                "{} transactions",
                thousands(sample.mempool_transactions)
            )];
            if let Some(size) = facts.metric_count(names::MEMPOOL_BYTES) {
                mempool.push(bytes(size));
            }
            lines.push(format!("Mempool: {}", mempool.join(" · ")));
        }
    }
    if let Some(disk) = facts.metric_count(names::DISK_BYTES) {
        lines.push(format!("State on disk: {}", bytes(disk)));
    }

    let mut ports = Vec::new();
    if let Some(addr) = status.rpc_addr {
        ports.push(format!("RPC {addr}"));
    }
    if let Some(addr) = status.metrics_addr {
        ports.push(format!("metrics {addr}"));
    }
    if !ports.is_empty() {
        lines.push(format!("Ports: {}", ports.join(" · ")));
    }
    if let Some((eos, _)) = facts.eos() {
        lines.push(format!(
            "End of support: block {}, about {} ({})",
            thousands(eos.block_height),
            local_date_short(eos.estimated_time),
            relative(eos.estimated_time, now)
        ));
    }
    if let Some(error) = input.stats.and_then(|stats| stats.last_error.as_deref()) {
        lines.push(format!("Last RPC error: {error}"));
    }

    let problems = input.insights.problems(SUMMARY_PROBLEMS);
    if problems.is_empty() {
        lines.push("Recent problems: none in this session".to_owned());
    } else {
        lines.push("Recent problems:".to_owned());
        for problem in &problems {
            let mut text = format!(
                "  {} {}",
                local_datetime_short(problem.last),
                problem.level.label()
            );
            if let Some(target) = &problem.target {
                text.push_str(&format!(" {target}"));
            }
            text.push_str(&format!(" · {}", problem.sample));
            if problem.count > 1 {
                text.push_str(&format!(" ×{}", problem.count));
            }
            lines.push(text);
        }
    }

    let events = input.insights.events(SUMMARY_EVENTS);
    if !events.is_empty() {
        lines.push("This run:".to_owned());
        for event in &events {
            lines.push(format!(
                "  {} {}",
                local_datetime_short(event.at),
                event.text
            ));
        }
    } else if let Some(run) = run {
        lines.push("This run:".to_owned());
        for (milestone, at) in run.milestones.iter().rev().take(SUMMARY_EVENTS) {
            lines.push(format!(
                "  {} {}",
                local_datetime_short(*at),
                milestone.label()
            ));
        }
    }

    let mut text = lines.join("\n");
    text.push('\n');
    text
}

#[cfg(test)]
mod tests {
    use std::time::UNIX_EPOCH;

    use launcher_types::{
        EosInfo, ExitInfo, LogLevel, LogLine, LogStream, MetricFamily, MetricKind, MetricSample,
        UpdateHealthFailure, UpdateMode,
    };

    use super::*;

    /// 2026-10-23 19:00:00 UTC: the run started.
    const START: u64 = 1_792_782_000;

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    /// `secs` after the start.
    fn t(secs: u64) -> SystemTime {
        at(START + secs)
    }

    fn running() -> Status {
        Status {
            phase: Phase::Running,
            pid: Some(4242),
            started_at: Some(t(0)),
            rpc_addr: "127.0.0.1:8232".parse().ok(),
            metrics_addr: "127.0.0.1:9999".parse().ok(),
            last_exit: None,
            stale: None,
            stop_is_graceful: true,
            restarts: 0,
            last_stop: None,
        }
    }

    fn stats(secs: u64, height: u64, peers: u32) -> NodeStats {
        NodeStats {
            height,
            estimated_height: 3_120_000,
            verification_progress: height as f64 / 3_120_000.0,
            peers,
            mempool_transactions: 24,
            chain: "main".into(),
            zebrad_version: "7.0.0".into(),
            last_error: None,
            eos: None,
            updated_at: t(secs),
        }
    }

    /// Samples every 5 s from `from` to `to` seconds after the start, heights rising by
    /// `step` per sample from `height`, with `peers`.
    fn history(from: u64, to: u64, height: u64, step: u64, peers: u32) -> StatsHistory {
        let mut history = StatsHistory::default();
        let mut secs = from;
        let mut index = 0;
        while secs <= to {
            history.push(&stats(secs, height + index * step, peers));
            secs += 5;
            index += 1;
        }
        history
    }

    fn stamp(secs: u64) -> String {
        let (y, mo, d, h, mi) = {
            // 2026-10-23 19:00:00 UTC plus `secs`, written by hand for the log lines.
            let total = 19 * 3600 + secs;
            (
                2026,
                10,
                23 + total / 86_400,
                total % 86_400 / 3600,
                total % 3600 / 60,
            )
        };
        format!("{y}-{mo:02}-{d:02}T{h:02}:{mi:02}:{:02}Z", secs % 60)
    }

    fn line(seq: u64, secs: u64, level: LogLevel, target: &str, message: &str) -> LogLine {
        LogLine {
            seq,
            stream: LogStream::Stdout,
            timestamp: Some(stamp(secs)),
            level: Some(level),
            spans: None,
            target: Some(target.into()),
            message: message.into(),
            dropped: None,
        }
    }

    fn tracing_started(seq: u64, secs: u64) -> LogLine {
        line(
            seq,
            secs,
            LogLevel::Info,
            "zebrad::components::tracing::component",
            "started tracing component filter=\"info\"",
        )
    }

    fn progress(seq: u64, secs: u64, message: &str) -> LogLine {
        line(
            seq,
            secs,
            LogLevel::Info,
            "zebrad::components::sync::progress",
            message,
        )
    }

    fn insights(lines: &[LogLine]) -> Insights {
        let mut insights = Insights::default();
        insights.ingest(lines, t(0));
        insights
    }

    fn metrics(secs: u64, pairs: &[(&str, f64)]) -> MetricsSnapshot {
        MetricsSnapshot {
            at: t(secs),
            families: pairs
                .iter()
                .map(|(name, value)| MetricFamily {
                    name: (*name).into(),
                    kind: MetricKind::Gauge,
                    help: None,
                    samples: vec![MetricSample {
                        name: (*name).into(),
                        labels: Vec::new(),
                        value: *value,
                    }],
                })
                .collect(),
        }
    }

    fn update(state: UpdateState) -> UpdateInfo {
        UpdateInfo {
            state,
            mode: UpdateMode::Notify,
            current_version: "0.2.0".into(),
            last_successful_check: None,
            stale_check_warning: false,
            can_install: true,
            releases_url: "https://example.test/releases".into(),
            health_failure: None,
        }
    }

    /// The inputs of one scenario, owned, so a test can tweak them and borrow them as a
    /// [`HealthInput`].
    struct Scene {
        status: Status,
        stats: Option<NodeStats>,
        history: StatsHistory,
        metrics: Option<MetricsSnapshot>,
        metrics_history: MetricsHistory,
        insights: Insights,
        update: Option<UpdateInfo>,
        now: SystemTime,
        phase_since: SystemTime,
    }

    impl Scene {
        /// A node that started at `t(0)`, polled since, syncing at 90 blocks a minute with
        /// 12 peers, looked at `now` seconds after the start.
        fn syncing(now: u64) -> Self {
            let history = history(5, now, 2_950_000, 8, 12);
            let stats = history.latest().map(|s| stats(now, s.height, s.peers));
            Scene {
                status: running(),
                stats,
                history,
                metrics: Some(metrics(now, &[(names::DOWNLOADS_IN_FLIGHT, 40.0)])),
                metrics_history: MetricsHistory::default(),
                insights: insights(&[tracing_started(0, 1)]),
                update: Some(update(UpdateState::Idle)),
                now: t(now),
                phase_since: t(0),
            }
        }

        /// A node at the tip for `now` seconds, a block every 75 s.
        fn at_tip(now: u64) -> Self {
            let mut scene = Scene::syncing(now);
            let mut history = StatsHistory::default();
            let mut secs = 5;
            while secs <= now {
                let blocks = secs / 75;
                history.push(&NodeStats {
                    estimated_height: 3_120_000 + blocks,
                    verification_progress: 1.0,
                    ..stats(secs, 3_120_000 + blocks, 12)
                });
                secs += 5;
            }
            scene.stats = history.latest().map(|s| NodeStats {
                estimated_height: s.estimated_height,
                verification_progress: 1.0,
                ..stats(now, s.height, s.peers)
            });
            scene.history = history;
            scene
        }

        fn input(&self) -> HealthInput<'_> {
            HealthInput {
                status: &self.status,
                stats: self.stats.as_ref(),
                history: &self.history,
                metrics: self.metrics.as_ref(),
                metrics_history: &self.metrics_history,
                insights: &self.insights,
                update: self.update.as_ref(),
                now: self.now,
                phase_since: self.phase_since,
            }
        }

        fn verdict(&self) -> Verdict {
            health_verdict(&self.input())
        }

        fn summary(&self) -> String {
            let input = self.input();
            summary_text(&input, &health_verdict(&input))
        }

        /// Every sample's peers set to `peers` from `from` seconds on.
        fn peers_from(&mut self, from: u64, peers: u32) {
            let samples: Vec<Sample> = self.history.window(None, self.now).to_vec();
            let mut history = StatsHistory::default();
            for sample in samples {
                let secs = sample.at.duration_since(t(0)).unwrap().as_secs();
                let peers = if secs >= from { peers } else { sample.peers };
                history.push(&stats(secs, sample.height, peers));
            }
            self.history = history;
            if let Some(stats) = &mut self.stats {
                stats.peers = peers;
            }
        }

        /// The height frozen from `from` seconds on.
        fn frozen_from(&mut self, from: u64) {
            let samples: Vec<Sample> = self.history.window(None, self.now).to_vec();
            let mut history = StatsHistory::default();
            let mut frozen = None;
            for sample in samples {
                let secs = sample.at.duration_since(t(0)).unwrap().as_secs();
                let height = if secs >= from {
                    *frozen.get_or_insert(sample.height)
                } else {
                    sample.height
                };
                history.push(&stats(secs, height, sample.peers));
            }
            self.history = history;
            if let (Some(stats), Some(frozen)) = (&mut self.stats, frozen) {
                stats.height = frozen;
                stats.verification_progress = frozen as f64 / 3_120_000.0;
            }
        }
    }

    fn exit(class: ExitClass, code: Option<i32>, secs: u64, requested: bool) -> ExitInfo {
        ExitInfo {
            code,
            class,
            detail: Some(
                "thread 'tokio-rt-worker' panicked at zebrad/src/x.rs:38:27:\nboom".into(),
            ),
            at: t(secs),
            requested,
        }
    }

    #[test]
    fn rule_0_connecting() {
        let verdict = Verdict::connecting();
        assert_eq!(verdict.rule, 0);
        assert_eq!(verdict.level, Level::Neutral);
        assert_eq!(verdict.title(), "Connecting to the launcher…");
        assert!(verdict.actions.is_empty());
    }

    #[test]
    fn rule_1_update_health_failure_beats_everything() {
        let mut scene = Scene::syncing(600);
        scene.status.phase = Phase::Failed {
            class: ExitClass::Crash,
        };
        scene.update = Some(UpdateInfo {
            health_failure: Some(UpdateHealthFailure {
                expected_version: "0.3.0".into(),
                running_version: "0.2.0".into(),
                message: "The launcher 0.3.0 did not start".into(),
                recovery: vec![
                    RecoveryAction::OpenLogs,
                    RecoveryAction::ResetConfig,
                    RecoveryAction::OpenReleasesPage,
                ],
                releases_url: "https://example.test/releases".into(),
            }),
            ..update(UpdateState::Idle)
        });
        let verdict = scene.verdict();
        assert_eq!((verdict.rule, verdict.level), (1, Level::Problem));
        assert_eq!(verdict.headline, "Launcher update failed");
        assert_eq!(verdict.reason, "The launcher 0.3.0 did not start");
        assert_eq!(
            verdict.actions,
            vec![
                Action::OpenLogsFolder,
                Action::ResetConfig,
                Action::OpenReleases
            ]
        );
        assert_eq!(verdict.title(), "Problem · Launcher update failed");
    }

    #[test]
    fn rule_2_failed_by_class() {
        let mut scene = Scene::syncing(600);
        let cases = [
            (
                ExitClass::ConfigError,
                "zebrad rejected its configuration",
                vec![Action::OpenConfigure, Action::ResetConfig],
            ),
            (
                ExitClass::EosHalt,
                "This zebrad release is past its end of support",
                vec![],
            ),
            (
                ExitClass::DbLocked,
                "Another zebrad is using the state database",
                vec![Action::Start],
            ),
            (
                ExitClass::PortInUse,
                "A port zebrad needs is already in use",
                vec![
                    Action::OpenConfigure,
                    Action::ShowInLogs(
                        "thread 'tokio-rt-worker' panicked at zebrad/src/x.rs:38:27:".into(),
                    ),
                ],
            ),
            (
                ExitClass::Crash,
                "zebrad crashed",
                vec![
                    Action::Start,
                    Action::ShowInLogs(
                        "thread 'tokio-rt-worker' panicked at zebrad/src/x.rs:38:27:".into(),
                    ),
                ],
            ),
        ];
        for (class, headline, actions) in cases {
            scene.status.phase = Phase::Failed { class };
            scene.status.last_exit = Some(exit(class, Some(101), 300, false));
            let verdict = scene.verdict();
            assert_eq!(
                (verdict.rule, verdict.level),
                (2, Level::Problem),
                "{class:?}"
            );
            assert_eq!(verdict.headline, headline);
            assert_eq!(
                verdict.reason,
                "thread 'tokio-rt-worker' panicked at zebrad/src/x.rs:38:27: · exited 5 min ago (code 101)"
            );
            assert_eq!(verdict.actions, actions, "{class:?}");
        }
        // A spawn failure has no detail: the fallback filter.
        scene.status.phase = Phase::Failed {
            class: ExitClass::SpawnFailed,
        };
        scene.status.last_exit = Some(ExitInfo {
            detail: None,
            code: None,
            ..exit(ExitClass::SpawnFailed, None, 590, false)
        });
        let verdict = scene.verdict();
        assert_eq!(verdict.headline, "zebrad could not be started");
        assert_eq!(verdict.reason, "exited 10 s ago (no exit code)");
        assert_eq!(
            verdict.actions,
            vec![Action::Start, Action::ShowInLogs("zebrad".into())]
        );
        // A locked database with the stale process known: name it and offer to stop it.
        scene.status.phase = Phase::Failed {
            class: ExitClass::DbLocked,
        };
        scene.status.last_exit = Some(exit(ExitClass::DbLocked, Some(1), 300, false));
        scene.status.stale = Some(StaleProcessInfo {
            pid: 999,
            binary: "/opt/zebrad".into(),
        });
        let verdict = scene.verdict();
        assert_eq!(
            verdict.reason,
            "pid 999, /opt/zebrad, from an earlier launcher run · exited 5 min ago (code 1)"
        );
        assert_eq!(verdict.actions, vec![Action::StartKillingStale]);
        // No exit info at all: how long the phase has lasted.
        scene.status.last_exit = None;
        scene.status.stale = None;
        scene.phase_since = t(540);
        assert_eq!(scene.verdict().reason, "for 1 min");
        assert_eq!(scene.verdict().actions, vec![Action::Start]);
    }

    #[test]
    fn rule_3_backoff_and_crash_loop() {
        let mut scene = Scene::syncing(600);
        scene.status.last_exit = Some(exit(ExitClass::Crash, Some(101), 590, false));
        scene.status.phase = Phase::Backoff {
            until: t(625),
            crashes: 1,
        };
        let verdict = scene.verdict();
        assert_eq!((verdict.rule, verdict.level), (3, Level::Attention));
        assert_eq!(verdict.headline, "zebrad crashed");
        assert_eq!(
            verdict.reason,
            "restarting in 25 s · thread 'tokio-rt-worker' panicked at zebrad/src/x.rs:38:27:",
            "the first crash is no streak"
        );
        assert_eq!(
            verdict.actions,
            vec![
                Action::StartNow,
                Action::ShowInLogs(
                    "thread 'tokio-rt-worker' panicked at zebrad/src/x.rs:38:27:".into()
                )
            ]
        );
        scene.status.phase = Phase::Backoff {
            until: t(590),
            crashes: CRASH_LOOP,
        };
        let verdict = scene.verdict();
        assert_eq!((verdict.rule, verdict.level), (3, Level::Problem));
        assert_eq!(verdict.headline, "Crash loop");
        assert!(
            verdict
                .reason
                .starts_with("3 crashes in a row · restarting now · ")
        );
    }

    #[test]
    fn rule_4_stopping() {
        let mut scene = Scene::syncing(600);
        scene.status.phase = Phase::Stopping;
        scene.phase_since = t(595);
        let verdict = scene.verdict();
        assert_eq!((verdict.rule, verdict.level), (4, Level::Neutral));
        assert_eq!(verdict.title(), "Stopping");
        assert_eq!(verdict.reason, "Stop requested 5 s ago");
        assert!(verdict.actions.is_empty());
        scene.status.stop_is_graceful = false;
        assert_eq!(
            scene.verdict().reason,
            "Stop requested 5 s ago · a hard stop: zebrad gets no shutdown"
        );
    }

    #[test]
    fn rule_5_stopped_and_stale() {
        let mut scene = Scene::syncing(600);
        scene.status = Status::default();
        let verdict = scene.verdict();
        assert_eq!((verdict.rule, verdict.level), (5, Level::Neutral));
        assert_eq!(verdict.headline, "Stopped");
        assert_eq!(verdict.reason, "Not started yet");
        assert_eq!(verdict.actions, vec![Action::Start]);
        scene.status.last_exit = Some(ExitInfo {
            detail: None,
            ..exit(ExitClass::Clean, Some(0), 0, true)
        });
        assert_eq!(
            scene.verdict().reason,
            "Stopped cleanly (code 0), 10 min ago"
        );
        scene.status.stale = Some(StaleProcessInfo {
            pid: 999,
            binary: "/opt/zebrad".into(),
        });
        let verdict = scene.verdict();
        assert_eq!((verdict.rule, verdict.level), (5, Level::Attention));
        assert_eq!(verdict.headline, "An earlier zebrad is still running");
        assert_eq!(
            verdict.reason,
            "pid 999, /opt/zebrad, from an earlier launcher run"
        );
        assert_eq!(verdict.actions, vec![Action::StartKillingStale]);
    }

    #[test]
    fn rule_6_starting_and_slow_start() {
        let mut scene = Scene::syncing(20);
        scene.status.phase = Phase::Starting;
        scene.status.rpc_addr = None;
        scene.stats = None;
        scene.history = StatsHistory::default();
        scene.insights = insights(&[
            tracing_started(0, 1),
            line(
                1,
                3,
                LogLevel::Info,
                "zebra_state::service::finalized_state::disk_db",
                "Opened Zebra state cache at /data",
            ),
        ]);
        let verdict = scene.verdict();
        assert_eq!((verdict.rule, verdict.level), (6, Level::Neutral));
        assert_eq!(verdict.title(), "Starting");
        assert_eq!(verdict.reason, "State database opened · for 20 s");
        assert!(verdict.actions.is_empty());
        // Five minutes without an RPC endpoint is slow.
        scene.now = t(5 * 60);
        let verdict = scene.verdict();
        assert_eq!((verdict.rule, verdict.level), (6, Level::Attention));
        assert_eq!(verdict.headline, "Slow start");
        assert_eq!(verdict.reason, "State database opened · for 5 min");
        assert_eq!(verdict.actions, vec![Action::ShowInLogs(String::new())]);
        // With the RPC open it is a long start, not a slow one; a run from before this start
        // lends no milestone.
        scene.status.rpc_addr = "127.0.0.1:8232".parse().ok();
        scene.insights = insights(&[tracing_started(0, 0)]);
        scene.status.started_at = Some(t(10));
        let verdict = scene.verdict();
        assert_eq!(verdict.level, Level::Neutral);
        assert_eq!(
            verdict.reason,
            "Waiting for zebrad's first line · for 4 min"
        );
    }

    #[test]
    fn rule_7_rpc_not_answering() {
        // Within the first minute nothing is wrong yet.
        let mut scene = Scene::syncing(30);
        scene.stats = None;
        scene.history = StatsHistory::default();
        let verdict = scene.verdict();
        assert_eq!((verdict.rule, verdict.level), (17, Level::Neutral));
        assert_eq!(verdict.title(), "Running");
        assert_eq!(verdict.reason, "Waiting for zebrad's RPC · for 30 s");
        // After it, attention; after five minutes, a problem with a restart.
        scene.now = t(90);
        let verdict = scene.verdict();
        assert_eq!((verdict.rule, verdict.level), (7, Level::Attention));
        assert_eq!(verdict.headline, "RPC not answering");
        assert_eq!(
            verdict.reason,
            "No answer for 1 min · zebrad is still running (pid 4242)"
        );
        assert!(verdict.actions.is_empty());
        scene.now = t(5 * 60);
        let verdict = scene.verdict();
        assert_eq!((verdict.rule, verdict.level), (7, Level::Problem));
        assert_eq!(verdict.actions, vec![Action::Restart]);
        // A sample from the previous run does not count for this one.
        let mut scene = Scene::syncing(600);
        scene.status.started_at = Some(t(610));
        scene.now = t(620);
        let verdict = scene.verdict();
        assert_eq!(verdict.rule, 17, "{verdict:?}");
        assert_eq!(verdict.reason, "Waiting for zebrad's RPC · for 10 s");
        scene.now = t(680);
        assert_eq!(scene.verdict().rule, 7);
        // A stale sample while running, with zebrad's own error as the reason.
        let mut scene = Scene::syncing(600);
        scene.now = t(645);
        let verdict = scene.verdict();
        assert_eq!((verdict.rule, verdict.level), (7, Level::Attention));
        assert_eq!(
            verdict.reason,
            "No answer for 45 s · zebrad is still running (pid 4242)"
        );
        scene.stats.as_mut().unwrap().last_error = Some("connection refused".into());
        assert_eq!(scene.verdict().reason, "connection refused");
        // Thirty seconds exactly is still fresh.
        scene.now = t(630);
        assert_ne!(scene.verdict().rule, 7);
    }

    #[test]
    fn rule_8_no_peers() {
        let mut scene = Scene::syncing(600);
        scene.peers_from(500, 0);
        // 100 s without peers: not yet.
        assert_ne!(scene.verdict().rule, 8);
        scene.peers_from(470, 0);
        scene.insights = insights(&[
            tracing_started(0, 1),
            line(
                1,
                2,
                LogLevel::Info,
                "zebra_network::peer_set::initialize",
                "finished connecting to initial seed and disk cache peers handshake_success_total=0 handshake_error_total=5 outbound_connections=0",
            ),
            line(
                2,
                3,
                LogLevel::Info,
                "zebra_network::peer_set::initialize",
                "failed to make outbound connection to peer error=timeout candidate.addr=203.0.113.9:8233",
            ),
            line(
                3,
                4,
                LogLevel::Info,
                "zebra_network::peer_set::initialize",
                "failed to make outbound connection to peer error=timeout candidate.addr=203.0.113.10:8233",
            ),
        ]);
        scene.metrics = Some(metrics(600, &[(names::READY_PEERS, 0.0)]));
        let verdict = scene.verdict();
        assert_eq!((verdict.rule, verdict.level), (8, Level::Problem));
        assert_eq!(verdict.headline, "No peers");
        assert_eq!(
            verdict.reason,
            "No peers for 2 min · 2 outbound connections failed · 5 initial peers failed · ready peers 0"
        );
        assert_eq!(
            verdict.actions,
            vec![Action::ShowInLogs("peer".into()), Action::OpenConfigure]
        );
        // Without the gauge the ready count is left out, even while an earlier run's snapshot
        // is still in the history.
        scene.metrics = None;
        scene.metrics_history.push(&MetricsSnapshot {
            at: t(0) - Duration::from_secs(30),
            ..metrics(0, &[(names::READY_PEERS, 11.0)])
        });
        assert!(scene.verdict().reason.ends_with("5 initial peers failed"));
        // This run's newest snapshot in the history stands in for a missing publication.
        scene
            .metrics_history
            .push(&metrics(590, &[(names::READY_PEERS, 3.0)]));
        assert!(scene.verdict().reason.ends_with("ready peers 3"));
        // No peers before a stalled sync: the cause first.
        scene.frozen_from(0);
        assert_eq!(scene.verdict().rule, 8);
    }

    #[test]
    fn rule_9_stall_from_samples_and_from_the_log() {
        // Two minutes without a block: attention.
        let mut scene = Scene::syncing(600);
        scene.frozen_from(470);
        let verdict = scene.verdict();
        assert_eq!((verdict.rule, verdict.level), (9, Level::Attention));
        assert_eq!(verdict.headline, "No new block for 2 min");
        assert_eq!(
            verdict.reason,
            "Height 2,950,744 · 12 peers · 40 downloads in flight"
        );
        assert_eq!(verdict.actions, vec![Action::ShowInLogs("sync".into())]);
        // Ten minutes: a problem, with a restart.
        let mut scene = Scene::syncing(900);
        scene.frozen_from(290);
        let verdict = scene.verdict();
        assert_eq!((verdict.rule, verdict.level), (9, Level::Problem));
        assert_eq!(verdict.headline, "Sync stalled");
        assert_eq!(
            verdict.reason,
            "No new block for 10 min at height 2,950,456 · 12 peers · 40 downloads in flight"
        );
        assert_eq!(
            verdict.actions,
            vec![Action::ShowInLogs("sync".into()), Action::Restart]
        );
        // Under two minutes: nothing.
        let mut scene = Scene::syncing(600);
        scene.frozen_from(500);
        assert_eq!(scene.verdict().rule, 17);
        // zebrad's own stall line makes it a problem at once, and its time wins when longer.
        scene.insights = insights(&[
            tracing_started(0, 1),
            progress(
                1,
                590,
                "chain updates have stalled, state height has not increased for 7 minutes. Hint: check your network connection sync_percent=94.567% current_height=Height(2950000) time_since_last_state_block=7m 2s",
            ),
        ]);
        let verdict = scene.verdict();
        assert_eq!((verdict.rule, verdict.level), (9, Level::Problem));
        assert!(
            verdict
                .reason
                .starts_with("No new block for 7 min at height"),
            "{}",
            verdict.reason
        );
        // A stall line from the previous run says nothing about this one.
        scene.insights = insights(&[progress(
            0,
            0,
            "chain updates have stalled, state height has not increased for 7 minutes. sync_percent=94.567% current_height=Height(2950000) time_since_last_state_block=7m 2s",
        )]);
        scene.status.started_at = Some(t(5));
        assert_eq!(scene.verdict().rule, 17);
        // A block after zebrad's line makes our own, shorter, time the truth.
        let mut scene = Scene::syncing(600);
        scene.insights = insights(&[
            tracing_started(0, 1),
            progress(
                1,
                500,
                "estimated progress to chain tip sync_percent=94.5% current_height=Height(2950000) time_since_last_state_block=1m 50s",
            ),
        ]);
        assert_eq!(scene.verdict().rule, 17);
        // Behind the tip, the launcher just opened: the oldest sample bounds the stall.
        let mut scene = Scene::syncing(600);
        scene.frozen_from(0);
        scene.history = history(300, 600, 2_950_000, 0, 12);
        assert_eq!(scene.verdict().headline, "No new block for 5 min");
    }

    #[test]
    fn rule_10_quiet_at_tip() {
        let mut scene = Scene::at_tip(1200);
        assert_eq!(scene.verdict().rule, 18);
        // The shape of a tip stall: the height freezes at 3,120,008 after ten minutes while
        // zebrad's time-based estimate keeps rising a block every 75 s, so the node stays
        // "at the tip" (99.99 % or more) for hours. Attention after ten quiet minutes.
        let frozen = Scene::at_tip(600);
        scene.history = frozen.history.clone();
        let creeping = |secs: u64| {
            let estimate = 3_120_008 + (secs - 600) / 75;
            NodeStats {
                estimated_height: estimate,
                verification_progress: 3_120_008.0 / estimate as f64,
                ..stats(secs, 3_120_008, 12)
            }
        };
        for secs in (605..=1200).step_by(5) {
            scene.history.push(&creeping(secs));
        }
        scene.stats = Some(creeping(1200));
        assert!(scene.stats.as_ref().unwrap().verification_progress < 1.0);
        let verdict = scene.verdict();
        assert_eq!((verdict.rule, verdict.level), (10, Level::Attention));
        assert_eq!(verdict.headline, "No new block for 10 min");
        assert_eq!(
            verdict.reason,
            "Blocks usually arrive every 75 s · the tip estimate is 8 blocks ahead"
        );
        assert_eq!(verdict.actions, vec![Action::ShowInLogs("sync".into())]);
        // A minute short of the threshold: still healthy.
        scene.now = t(1140);
        scene.stats = Some(creeping(1140));
        assert_eq!(scene.verdict().rule, 18);
        // An estimate that stands still with the height reads as "has not moved".
        let mut still = Scene::at_tip(600);
        for secs in (605..=1200).step_by(5) {
            still.history.push(&NodeStats {
                estimated_height: 3_120_008,
                verification_progress: 1.0,
                ..stats(secs, 3_120_008, 12)
            });
        }
        still.stats = Some(NodeStats {
            estimated_height: 3_120_008,
            verification_progress: 1.0,
            ..stats(1200, 3_120_008, 12)
        });
        still.now = t(1200);
        let verdict = still.verdict();
        assert_eq!(verdict.rule, 10);
        assert_eq!(
            verdict.reason,
            "Blocks usually arrive every 75 s · the tip estimate has not moved"
        );
        // zebrad's own stall line fires the rule before the ten minutes are up, since rule 9
        // leaves the tip alone; its time wins when longer than ours.
        let mut stalled = Scene::at_tip(600);
        stalled.frozen_from(300);
        stalled.insights = insights(&[
            tracing_started(0, 1),
            progress(
                1,
                590,
                "chain updates have stalled, state height has not increased for 5 minutes. Hint: check your network connection sync_percent=99.999% current_height=Height(3120003) time_since_last_state_block=5m 0s",
            ),
        ]);
        let verdict = stalled.verdict();
        assert_eq!((verdict.rule, verdict.level), (10, Level::Attention));
        assert_eq!(verdict.headline, "No new block for 5 min");
        // Without peers the stall is rule 8's, not this one's.
        stalled.peers_from(0, 0);
        assert_eq!(stalled.verdict().rule, 8);
    }

    #[test]
    fn rule_11_urgent_end_of_support_and_install_countdown() {
        let mut scene = Scene::syncing(600);
        let eos = |days: u64| EosInfo {
            block_height: 3_160_000,
            estimated_time: t(600 + days * 86_400),
        };
        scene.stats.as_mut().unwrap().eos = Some(eos(2));
        let verdict = scene.verdict();
        assert_eq!((verdict.rule, verdict.level), (11, Level::Urgent));
        assert_eq!(verdict.headline, "End of support in 2 days");
        assert_eq!(
            verdict.reason,
            "zebrad 7.0.0 stops working at block 3,160,000 (about 25 Oct 2026). Update the launcher before then."
        );
        assert_eq!(verdict.actions, vec![Action::CheckForUpdates]);
        assert_eq!(verdict.title(), "Attention · End of support in 2 days");
        // Past the estimate.
        scene.stats.as_mut().unwrap().eos = Some(EosInfo {
            block_height: 3_160_000,
            estimated_time: t(0),
        });
        assert_eq!(scene.verdict().headline, "End of support reached");
        // The countdown of an automatic install.
        scene.stats.as_mut().unwrap().eos = None;
        scene.update = Some(update(UpdateState::ReadyToInstall {
            version: "0.3.0".into(),
            countdown_secs: Some(42),
        }));
        let verdict = scene.verdict();
        assert_eq!((verdict.rule, verdict.level), (11, Level::Urgent));
        assert_eq!(verdict.headline, "Update installs in 42 s");
        assert_eq!(
            verdict.reason,
            "Launcher 0.3.0 is ready · the launcher restarts and brings zebrad back"
        );
        assert_eq!(verdict.actions, vec![Action::Install, Action::CancelUpdate]);
        // Ready without a countdown waits for the user: the shell's banner, not the verdict.
        scene.update = Some(update(UpdateState::ReadyToInstall {
            version: "0.3.0".into(),
            countdown_secs: None,
        }));
        assert_eq!(scene.verdict().rule, 17);
    }

    #[test]
    fn rule_12_few_peers() {
        let mut scene = Scene::syncing(600);
        scene.peers_from(0, 2);
        let verdict = scene.verdict();
        assert_eq!((verdict.rule, verdict.level), (12, Level::Attention));
        assert_eq!(verdict.headline, "Few peers");
        assert_eq!(verdict.reason, "2 peers for 9 min");
        assert_eq!(verdict.actions, vec![Action::ShowInLogs("peer".into())]);
        scene.peers_from(0, 1);
        assert_eq!(scene.verdict().reason, "1 peer for 9 min");
        // Three peers are enough; a short dip is not yet few.
        scene.peers_from(0, 3);
        assert_eq!(scene.verdict().rule, 17);
        scene.peers_from(400, 1);
        assert_eq!(scene.verdict().rule, 17);
    }

    #[test]
    fn rule_13_slow_sync() {
        let mut scene = Scene::syncing(600);
        scene.insights = insights(&[
            tracing_started(0, 1),
            progress(
                1,
                590,
                "initial sync is very slow, or estimated tip is wrong. Hint: check your network connection sync_percent=94.5% current_height=Height(2950000)",
            ),
        ]);
        let verdict = scene.verdict();
        assert_eq!((verdict.rule, verdict.level), (13, Level::Attention));
        assert_eq!(verdict.headline, "Syncing slowly");
        assert_eq!(verdict.reason, "94.5 % · 96 blocks/min · about 29 h left");
        assert!(verdict.actions.is_empty());
    }

    #[test]
    fn rule_14_recent_errors() {
        let mut scene = Scene::syncing(600);
        scene.insights = insights(&[
            tracing_started(0, 1),
            line(
                1,
                500,
                LogLevel::Error,
                "zebrad::components::sync",
                "block download failed hash=block::Hash(\"abc\")",
            ),
            line(
                2,
                550,
                LogLevel::Warn,
                "zebra_network::peer_set::set",
                "dropping stalled peer addr=203.0.113.5:8233",
            ),
            LogLine {
                stream: LogStream::Stderr,
                timestamp: None,
                level: None,
                target: None,
                ..line(
                    3,
                    560,
                    LogLevel::Error,
                    "",
                    "thread 'main' panicked at x.rs",
                )
            },
        ]);
        let verdict = scene.verdict();
        assert_eq!((verdict.rule, verdict.level), (14, Level::Attention));
        assert_eq!(
            verdict.headline, "2 errors in the log",
            "WARN does not count"
        );
        assert_eq!(verdict.reason, "thread 'main' panicked at x.rs");
        assert_eq!(
            verdict.actions,
            vec![Action::ShowInLogs("thread 'main' panicked at x.rs".into())]
        );
        // A repeat of a pattern is another error line, as the ×N on its row says.
        scene.insights.ingest(
            &[line(
                4,
                570,
                LogLevel::Error,
                "zebrad::components::sync",
                "block download failed hash=block::Hash(\"def\")",
            )],
            t(0),
        );
        assert_eq!(scene.verdict().headline, "3 errors in the log");
        // Older than ten minutes: nothing.
        scene.now = t(1200);
        scene.stats = Some(stats(1200, 2_951_000, 12));
        scene.history.push(scene.stats.as_ref().unwrap());
        assert_eq!(scene.verdict().rule, 17);
        // One error reads in the singular; errors of the previous run do not count.
        let mut scene = Scene::syncing(600);
        scene.insights = insights(&[
            tracing_started(0, 1),
            line(
                1,
                590,
                LogLevel::Error,
                "zebrad::components::sync",
                "block download failed hash=block::Hash(\"abc\")",
            ),
        ]);
        assert_eq!(scene.verdict().headline, "1 error in the log");
        scene.status.started_at = Some(t(595));
        assert_eq!(scene.verdict().rule, 17);
    }

    #[test]
    fn rule_15_restarted_after_a_crash() {
        let mut scene = Scene::syncing(600);
        scene.status.started_at = Some(t(0));
        scene.status.last_exit = Some(ExitInfo {
            at: t(0) - Duration::from_secs(20 * 60),
            ..exit(ExitClass::Crash, Some(101), 0, false)
        });
        let verdict = scene.verdict();
        assert_eq!((verdict.rule, verdict.level), (15, Level::Attention));
        assert_eq!(verdict.headline, "Restarted after a crash");
        assert_eq!(
            verdict.reason,
            "zebrad crashed 30 min ago · thread 'tokio-rt-worker' panicked at zebrad/src/x.rs:38:27:"
        );
        assert_eq!(
            verdict.actions,
            vec![Action::ShowInLogs(
                "thread 'tokio-rt-worker' panicked at zebrad/src/x.rs:38:27:".into()
            )]
        );
        // A requested stop, a clean exit, or a crash over an hour ago: nothing to say.
        scene.status.last_exit.as_mut().unwrap().requested = true;
        assert_eq!(scene.verdict().rule, 17);
        scene.status.last_exit = Some(ExitInfo {
            at: t(0) - Duration::from_secs(60),
            ..exit(ExitClass::Clean, Some(0), 0, false)
        });
        assert_eq!(scene.verdict().rule, 17);
        scene.status.last_exit = Some(ExitInfo {
            at: t(0) - Duration::from_secs(2 * 3600),
            ..exit(ExitClass::Crash, Some(101), 0, false)
        });
        assert_eq!(scene.verdict().rule, 17);
    }

    #[test]
    fn rule_16_end_of_support_warning() {
        let mut scene = Scene::syncing(600);
        scene.stats.as_mut().unwrap().eos = Some(EosInfo {
            block_height: 3_160_000,
            estimated_time: t(600 + 12 * 86_400),
        });
        let verdict = scene.verdict();
        assert_eq!((verdict.rule, verdict.level), (16, Level::Attention));
        assert_eq!(verdict.headline, "End of support in 12 days");
        assert_eq!(
            verdict.reason,
            "zebrad 7.0.0 stops working at block 3,160,000 (about 4 Nov 2026). Update the launcher before then."
        );
        assert_eq!(verdict.actions, vec![Action::CheckForUpdates]);
        // Outside the window the date is a detail row, not a verdict.
        scene.stats.as_mut().unwrap().eos = Some(EosInfo {
            block_height: 3_160_000,
            estimated_time: t(600 + 40 * 86_400),
        });
        assert_eq!(scene.verdict().rule, 17);
    }

    #[test]
    fn rule_17_syncing() {
        let scene = Scene::syncing(600);
        let verdict = scene.verdict();
        assert_eq!((verdict.rule, verdict.level), (17, Level::Neutral));
        assert_eq!(verdict.title(), "Syncing");
        assert_eq!(verdict.reason, "94.5 % · 96 blocks/min · about 29 h left");
        assert!(verdict.actions.is_empty());
        // Too short a history for a rate.
        let mut scene = Scene::syncing(20);
        scene.status.started_at = Some(t(0));
        assert_eq!(scene.verdict().reason, "94.5 % · estimating…");
        // After five minutes down, a run syncing at 3,000 blocks/min for a minute reads its
        // own pace, not one diluted by the downtime still inside the hero's window.
        let mut scene = Scene::syncing(60);
        for secs in (360..=420).step_by(5) {
            scene
                .history
                .push(&stats(secs, 2_950_088 + (secs - 360) / 5 * 250, 12));
        }
        scene.stats = scene
            .history
            .latest()
            .map(|s| stats(420, s.height, s.peers));
        scene.status.started_at = Some(t(355));
        scene.now = t(420);
        let verdict = scene.verdict();
        assert_eq!(verdict.rule, 17);
        assert_eq!(
            verdict.reason,
            "94.6 % · 3,000 blocks/min · about 55 min left"
        );
        // Too little of the run for a rate: estimating, not the earlier run's pace.
        scene.status.started_at = Some(t(400));
        assert_eq!(scene.verdict().reason, "94.6 % · estimating…");
    }

    #[test]
    fn rule_18_healthy() {
        let scene = Scene::at_tip(630);
        let verdict = scene.verdict();
        assert_eq!((verdict.rule, verdict.level), (18, Level::Healthy));
        assert_eq!(verdict.title(), "Healthy · At the chain tip");
        assert_eq!(
            verdict.reason,
            "12 peers · last block 30 s ago · RPC answered just now"
        );
        assert!(verdict.actions.is_empty());
        assert_eq!(Level::Healthy.tone(), "good");
        assert_eq!(Level::Problem.tone(), "critical");
        assert_eq!(Level::Urgent.tone(), "serious");
        // No height rise sampled yet (the launcher just opened at the tip): zebrad's own words
        // from its progress line, aged to now and in the page's units, as the hero shows them.
        let at_tip_line = |secs, since| {
            progress(
                1,
                secs,
                &format!(
                    "finished initial sync to chain tip, using gossiped blocks sync_percent=100.000% current_height=Height(3120000) time_since_last_state_block={since}"
                ),
            )
        };
        let mut scene = Scene::at_tip(630);
        scene.history = history(300, 630, 3_120_000, 0, 12);
        scene.stats = Some(stats(630, 3_120_000, 12));
        scene.insights = insights(&[tracing_started(0, 1), at_tip_line(590, "1m 20s")]);
        let verdict = scene.verdict();
        assert_eq!(verdict.rule, 18);
        assert_eq!(
            verdict.reason,
            "12 peers · last block 2 min ago · RPC answered just now"
        );
        scene.insights = insights(&[tracing_started(0, 1), at_tip_line(580, "5s")]);
        assert_eq!(
            scene.verdict().reason,
            "12 peers · last block 55 s ago · RPC answered just now"
        );
        // A progress line from before this run says nothing about it.
        scene.status.started_at = Some(t(600));
        assert_eq!(scene.verdict().reason, "12 peers · RPC answered just now");
    }

    #[test]
    fn ordering_puts_the_cause_first() {
        // A failed process with a stale RPC and a stall in the data: the failure.
        let mut scene = Scene::syncing(900);
        scene.frozen_from(0);
        scene.now = t(1200);
        scene.status.phase = Phase::Failed {
            class: ExitClass::Crash,
        };
        assert_eq!(scene.verdict().rule, 2);
        // Running with a stale RPC and no peers in the samples: the silent RPC.
        scene.status.phase = Phase::Running;
        scene.peers_from(0, 0);
        assert_eq!(scene.verdict().rule, 7);
        // Fresh RPC, no peers, stalled: no peers.
        scene.now = t(900);
        assert_eq!(scene.verdict().rule, 8);
        // Peers back: the stall.
        scene.peers_from(0, 12);
        assert_eq!(scene.verdict().rule, 9);
        // A stall beats an urgent end of support, which beats few peers, which beats errors.
        scene.stats.as_mut().unwrap().eos = Some(EosInfo {
            block_height: 3_160_000,
            estimated_time: t(900 + 86_400),
        });
        assert_eq!(scene.verdict().rule, 9);
        let mut scene = Scene::syncing(900);
        scene.stats.as_mut().unwrap().eos = Some(EosInfo {
            block_height: 3_160_000,
            estimated_time: t(900 + 86_400),
        });
        scene.peers_from(0, 1);
        scene.insights = insights(&[
            tracing_started(0, 1),
            line(1, 890, LogLevel::Error, "t", "block download failed"),
        ]);
        assert_eq!(scene.verdict().rule, 11);
        scene.stats.as_mut().unwrap().eos = None;
        assert_eq!(scene.verdict().rule, 12);
        scene.peers_from(0, 12);
        assert_eq!(scene.verdict().rule, 14);
    }

    #[test]
    fn text_helpers() {
        assert_eq!(humantime_secs("5s"), Some(5));
        assert_eq!(humantime_secs("7m 2s"), Some(422));
        assert_eq!(humantime_secs("1h 2m 3s"), Some(3723));
        assert_eq!(humantime_secs("2days 3h"), Some(2 * 86_400 + 3 * 3600));
        assert_eq!(humantime_secs("350ms"), Some(0));
        assert_eq!(humantime_secs(""), None);
        assert_eq!(humantime_secs("soon"), None);
        assert_eq!(humantime_secs("5 s"), None);
        assert_eq!(sentence("zebrad crashed"), "zebrad crashed");
        assert_eq!(sentence("a port zebrad needs"), "A port zebrad needs");
        assert_eq!(sentence(""), "");
        assert_eq!(first_line("one\ntwo"), "one");
        assert_eq!(log_filter(None, "panicked"), "panicked");
        assert_eq!(log_filter(Some("  \n"), "panicked"), "panicked");
        assert_eq!(log_filter(Some(&"x".repeat(80)), ""), "x".repeat(60));
        assert_eq!(peers_text(1), "1 peer");
        assert_eq!(crashes_text(2), "2 crashes");
        // A run's time: everything without a start, else from the start less the slack.
        assert!(this_run(None, t(0)));
        assert!(this_run(Some(t(10)), t(10)));
        assert!(this_run(Some(t(10)), t(8)), "within the slack");
        assert!(!this_run(Some(t(10)), t(7)));
        assert_eq!(Level::Neutral.word(), None);
        assert_eq!(Level::Attention.word(), Some("Attention"));
        assert_eq!(Action::StartKillingStale.label(), "Stop it and start");
    }

    #[test]
    fn summary_lists_the_facts_with_local_times() {
        let mut scene = Scene::syncing(1200);
        scene.status.last_exit = Some(ExitInfo {
            at: t(0) - Duration::from_secs(60),
            ..exit(ExitClass::Crash, Some(101), 0, false)
        });
        scene.stats.as_mut().unwrap().eos = Some(EosInfo {
            block_height: 3_160_000,
            estimated_time: t(1200 + 40 * 86_400),
        });
        scene.metrics = Some(metrics(
            1200,
            &[
                (names::READY_PEERS, 11.0),
                (names::CHECKPOINT_HEIGHT, 2_500_000.0),
                (names::MEMPOOL_BYTES, 44_400.0),
                (names::DISK_BYTES, 312_400_000_000.0),
            ],
        ));
        scene.insights = insights(&[
            tracing_started(0, 1),
            line(
                1,
                3,
                LogLevel::Info,
                "zebrad::commands::start",
                "Starting zebrad",
            ),
            line(
                2,
                500,
                LogLevel::Error,
                "zebrad::components::sync",
                "block download failed hash=block::Hash(\"abc\")",
            ),
            line(
                3,
                510,
                LogLevel::Error,
                "zebrad::components::sync",
                "block download failed hash=block::Hash(\"abc\")",
            ),
        ]);
        let summary = scene.summary();
        let expected = "\
Zebra Launcher · 23 Oct, 19:20
Verdict: Attention · Restarted after a crash
  zebrad crashed 21 min ago · thread 'tokio-rt-worker' panicked at zebrad/src/x.rs:38:27:
Versions: launcher 0.2.0 · zebrad 7.0.0 · chain main
Phase: Running since 23 Oct, 19:00 (pid 4242)
Last exit: zebrad crashed (code 101), 23 Oct, 18:59 · thread 'tokio-rt-worker' panicked at zebrad/src/x.rs:38:27:
Sync: 2,951,912 of 3,120,000 · 94.6 % · 96 blocks/min · about 29 h left · last block 23 Oct, 19:20 · checkpoints verified to 2,500,000 · RPC answered 23 Oct, 19:20
Peers: 12 peers · 11 ready
Mempool: 24 transactions · 44.4 kB
State on disk: 312.4 GB
Ports: RPC 127.0.0.1:8232 · metrics 127.0.0.1:9999
End of support: block 3,160,000, about 2 Dec 2026 (in 40 days)
Recent problems:
  23 Oct, 19:08 ERROR zebrad::components::sync · block download failed hash=block::Hash(\"abc\") ×2
This run:
  23 Oct, 19:08 ERROR: block download failed ×2
  23 Oct, 19:00 zebrad started
";
        assert_eq!(summary, expected);
        // Stopped before any answer: the shorter report.
        let mut scene = Scene::syncing(1200);
        scene.status = Status::default();
        scene.stats = None;
        scene.history = StatsHistory::default();
        scene.metrics = None;
        scene.insights = Insights::default();
        scene.update = None;
        let summary = scene.summary();
        assert_eq!(
            summary,
            "Zebra Launcher · 23 Oct, 19:20\nVerdict: Stopped\n  Not started yet\nPhase: Stopped\nSync: no answer from zebrad's RPC yet\nRecent problems: none in this session\n"
        );
        // Stopped after a short run: the last numbers, with nothing being estimated.
        let mut scene = Scene::syncing(20);
        scene.status = Status::default();
        scene.stats = None;
        scene.metrics = None;
        scene.now = t(1200);
        let sync_line = scene
            .summary()
            .lines()
            .find(|line| line.starts_with("Sync: "))
            .map(str::to_owned)
            .unwrap();
        assert_eq!(
            sync_line,
            "Sync: 2,950,024 of 3,120,000 · 94.5 % · last block 23 Oct, 19:00 · RPC answered 23 Oct, 19:00"
        );
    }
}
