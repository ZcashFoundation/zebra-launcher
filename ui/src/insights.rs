//! Analysis of zebrad's log stream: sync progress, problems, startup milestones and activity
//! counts, folded line by line as batches arrive. Pure Rust, tested natively.
//!
//! [`Insights::ingest`] is called from the app shell for every batch; the Logs page and the
//! dashboard read the accessors. Everything here is plain data: no interior mutability, no
//! clocks (a line without a timestamp takes the time of the timestamped line before it; the
//! ingest `now` stands in only until one has been seen).

pub mod events;
pub mod fields;
pub mod text;

use std::{
    cmp::Reverse,
    collections::VecDeque,
    time::{Duration, SystemTime},
};

use launcher_types::{LogLevel, LogLine, LogStream};

use fields::{
    epoch_minute, field, height_of, message_text, minute_start, parse_timestamp, pattern_of,
    percent_of, u64_of,
};

/// How many minute buckets are kept: the 60 shown plus the one rolling in.
const BUCKETS_KEPT: usize = 61;

/// How many minutes [`Insights::activity`] returns.
pub const ACTIVITY_MINUTES: usize = 60;

/// How many distinct problem patterns are kept.
const PROBLEMS_KEPT: usize = 50;

/// How many `(time, height)` points from progress lines are kept for the rate.
const PROGRESS_POINTS_KEPT: usize = 240;

/// How many runs are kept.
const RUNS_KEPT: usize = 5;

/// How many characters [`pick_filter`] returns at most.
const FILTER_CHARS: usize = 60;

/// How many of the launcher's own notes, as events, are kept.
const LAUNCHER_EVENTS_KEPT: usize = 40;

/// How many status transitions the UI observed are kept.
const SESSION_EVENTS_KEPT: usize = 40;

/// A launcher start note this far from a run's first line belongs to that run.
const START_NOTE_SLACK: Duration = Duration::from_secs(30);

/// Where zebrad's progress task logs.
const PROGRESS_TARGET: &str = "zebrad::components::sync::progress";

/// What the log stream so far says about the node.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Insights {
    /// The highest `seq` folded in; lines at or below it are duplicates.
    last_seq: Option<u64>,
    /// The time of the last timestamped line. Continuation lines and stderr lines carry no
    /// timestamp and take it, since they follow the line they belong to.
    last_at: Option<SystemTime>,
    totals: LevelTotals,
    lines_total: u64,
    dropped_lines: u64,
    unseen_problems: u32,
    /// Oldest first, consecutive minutes, at most [`BUCKETS_KEPT`].
    buckets: VecDeque<MinuteBucket>,
    problems: Vec<Problem>,
    sync: Option<SyncSnapshot>,
    /// `(time, height)` from progress lines, oldest first.
    progress_points: VecDeque<(SystemTime, u64)>,
    checkpoints: Checkpoints,
    peers: Peers,
    mempool: Mempool,
    /// Oldest first, at most [`RUNS_KEPT`].
    runs: Vec<Run>,
    /// The launcher's own notes as timeline rows, oldest first, at most
    /// [`LAUNCHER_EVENTS_KEPT`].
    launcher_events: VecDeque<RunEvent>,
    /// Status transitions the UI observed ([`Insights::record_session_event`]), oldest first,
    /// at most [`SESSION_EVENTS_KEPT`].
    session_events: VecDeque<RunEvent>,
    /// The percentage of the current run's first progress line.
    first_progress_percent: Option<f64>,
    /// The last leveled line was `Diagnostic metadata:`, so continuation lines carry the
    /// version and network of the current run.
    in_metadata: bool,
}

/// How many lines of each level arrived since the stream began.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LevelTotals {
    pub trace: u64,
    pub debug: u64,
    pub info: u64,
    pub warn: u64,
    pub error: u64,
    /// Lines from zebrad's stderr (panics, fatal errors).
    pub stderr: u64,
}

impl LevelTotals {
    /// Leveled lines at `level` or above.
    pub fn at_or_above(&self, level: LogLevel) -> u64 {
        [
            (LogLevel::Trace, self.trace),
            (LogLevel::Debug, self.debug),
            (LogLevel::Info, self.info),
            (LogLevel::Warn, self.warn),
            (LogLevel::Error, self.error),
        ]
        .into_iter()
        .filter(|(l, _)| *l >= level)
        .map(|(_, count)| count)
        .sum()
    }
}

/// Lines per minute by kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MinuteBucket {
    pub minute_start: SystemTime,
    pub info: u32,
    pub warn: u32,
    /// ERROR lines and stderr lines.
    pub error: u32,
    /// TRACE, DEBUG and continuation lines, and launcher notes without a level.
    pub other: u32,
}

impl MinuteBucket {
    fn empty(minute: u64) -> Self {
        Self {
            minute_start: minute_start(minute),
            info: 0,
            warn: 0,
            error: 0,
            other: 0,
        }
    }

    pub fn total(&self) -> u32 {
        self.info + self.warn + self.error + self.other
    }
}

/// The severity of a [`Problem`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProblemLevel {
    Warn,
    Error,
    /// A line on zebrad's stderr: a panic or a fatal error.
    Stderr,
}

impl ProblemLevel {
    pub fn label(self) -> &'static str {
        match self {
            ProblemLevel::Warn => "WARN",
            ProblemLevel::Error => "ERROR",
            ProblemLevel::Stderr => "stderr",
        }
    }

    /// The level of a problem line: stderr, or a leveled WARN or ERROR line from zebrad or the
    /// launcher alike, so the problems agree with the level totals.
    fn of(line: &LogLine) -> Option<Self> {
        match (line.stream, line.level) {
            (LogStream::Stderr, _) => Some(ProblemLevel::Stderr),
            (_, Some(LogLevel::Error)) => Some(ProblemLevel::Error),
            (_, Some(LogLevel::Warn)) => Some(ProblemLevel::Warn),
            _ => None,
        }
    }
}

/// WARN, ERROR and stderr lines that read the same once their numbers and names are blanked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub level: ProblemLevel,
    pub target: Option<String>,
    /// The message with variable parts blanked, see [`fields::pattern_of`].
    pub pattern: String,
    /// The most recent message with this pattern.
    pub sample: String,
    pub count: u32,
    /// The first appearance in the session.
    pub first: SystemTime,
    /// The first appearance in the current run: the time of the row on the This run timeline.
    pub run_first: SystemTime,
    pub last: SystemTime,
    pub last_seq: u64,
}

/// A text filter for the Logs page that finds lines like `problem`: the message text before
/// its fields, cut to 60 characters.
pub fn pick_filter(problem: &Problem) -> String {
    let text = message_text(&problem.sample);
    let text = if text.is_empty() {
        problem.sample.as_str()
    } else {
        text
    };
    text.chars()
        .take(FILTER_CHARS)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// Where the initial sync is, from zebrad's progress task.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncStage {
    WaitingForGenesis,
    GenesisStalled,
    Syncing,
    Slow,
    NearTip,
    AtTip,
    Stalled,
}

/// One line of the dashboard's "This run" timeline, from [`Insights::events`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunEvent {
    pub at: SystemTime,
    pub kind: EventKind,
    /// The sentence shown, such as `zebrad started (pid 4242)`.
    pub text: String,
    pub tone: Tone,
}

/// What kind of thing happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    /// zebrad was spawned (`zebrad started (pid 4242)`), or a stale zebrad was stopped for it.
    Started,
    /// A startup step of the current run (`State database opened`).
    Milestone,
    /// The launcher asked zebrad to stop.
    StopRequested,
    /// zebrad exited (`zebrad exited: crash (code 101)`).
    Exited,
    /// A restart is scheduled (`Restarting in 5 s (crash 2)`), or was cancelled.
    Restarting,
    /// `zebrad.toml` was applied or replaced.
    ConfigApplied,
    /// zebrad rejected a new `zebrad.toml` and the old one is back.
    ConfigRolledBack,
    /// A step of the launcher's updater.
    Update,
    /// Something went wrong: an ERROR or stderr line's first appearance, a launcher failure, a
    /// run the launcher gave up on, a stale zebrad.
    Problem,
    /// The one line an earlier run gets (`Run 2 ended: SIGTERM`).
    EarlierRun,
}

/// How a stage reads: paired with an icon and text, never colour alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Good,
    Neutral,
    Warning,
    Critical,
}

impl SyncStage {
    fn classify(message: &str) -> Option<Self> {
        const PREFIXES: [(&str, SyncStage); 7] = [
            ("estimated progress to chain tip", SyncStage::Syncing),
            ("close to finishing initial sync", SyncStage::NearTip),
            ("finished initial sync to chain tip", SyncStage::AtTip),
            ("initial sync is very slow", SyncStage::Slow),
            (
                "initial sync is waiting to download the genesis block",
                SyncStage::WaitingForGenesis,
            ),
            ("chain updates have stalled", SyncStage::Stalled),
            (
                "initial sync can't download and verify the genesis block",
                SyncStage::GenesisStalled,
            ),
        ];
        PREFIXES
            .into_iter()
            .find(|(prefix, _)| message.starts_with(prefix))
            .map(|(_, stage)| stage)
    }

    pub fn stage_text(self) -> &'static str {
        match self {
            SyncStage::WaitingForGenesis => "Waiting for the genesis block",
            SyncStage::GenesisStalled => "Cannot download the genesis block",
            SyncStage::Syncing => "Syncing",
            SyncStage::Slow => "Syncing slowly",
            SyncStage::NearTip => "Close to the chain tip",
            SyncStage::AtTip => "At the chain tip",
            SyncStage::Stalled => "Sync stalled",
        }
    }

    pub fn stage_tone(self) -> Tone {
        match self {
            SyncStage::AtTip => Tone::Good,
            SyncStage::Syncing | SyncStage::NearTip | SyncStage::WaitingForGenesis => Tone::Neutral,
            SyncStage::Slow => Tone::Warning,
            SyncStage::Stalled | SyncStage::GenesisStalled => Tone::Critical,
        }
    }
}

/// The latest progress line, parsed.
#[derive(Clone, Debug, PartialEq)]
pub struct SyncSnapshot {
    pub stage: SyncStage,
    pub percent: Option<f64>,
    pub height: Option<u64>,
    pub remaining: Option<u64>,
    pub network_upgrade: Option<String>,
    /// zebrad's own words, such as `5s` or `1m 2s`.
    pub since_last_block: Option<String>,
    pub at: SystemTime,
}

/// What the checkpoint verifier reported in the current run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Checkpoints {
    pub blocks_verified: u64,
    /// When the final checkpoint was verified and full validation began.
    pub final_reached: Option<SystemTime>,
}

/// Peer facts from the current run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Peers {
    pub initial_successes: u32,
    pub initial_errors: u32,
    pub outbound: u32,
    pub failed_connections: u32,
    pub stalled_dropped: u32,
    pub no_ready_peers_events: u32,
}

/// Mempool facts from the current run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mempool {
    pub active_since: Option<SystemTime>,
    pub tip_height: Option<u64>,
    pub resets: u32,
}

/// A startup step, in the order zebrad usually reaches them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Milestone {
    Tracing,
    StateOpened,
    P2pListening,
    Rpc,
    Metrics,
    Started,
    PeersConnected,
    FirstProgress,
    MempoolActive,
    FullValidation,
}

impl Milestone {
    pub fn label(self) -> &'static str {
        match self {
            Milestone::Tracing => "Tracing started",
            Milestone::StateOpened => "State database opened",
            Milestone::P2pListening => "Listening for peers",
            Milestone::Rpc => "RPC endpoint opened",
            Milestone::Metrics => "Metrics endpoint opened",
            Milestone::Started => "zebrad started",
            Milestone::PeersConnected => "Initial peers connected",
            Milestone::FirstProgress => "First progress report",
            Milestone::MempoolActive => "Mempool active",
            Milestone::FullValidation => "Full validation started",
        }
    }
}

/// One zebrad run, from its first tracing line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    pub started_at: SystemTime,
    pub version: Option<String>,
    pub network: Option<String>,
    /// In the order reached; each milestone at most once.
    pub milestones: Vec<(Milestone, SystemTime)>,
    pub shutdown_reason: Option<String>,
    pub stopped_at: Option<SystemTime>,
}

impl Run {
    fn reach(&mut self, milestone: Milestone, at: SystemTime) {
        if !self.milestones.iter().any(|(m, _)| *m == milestone) {
            self.milestones.push((milestone, at));
        }
    }
}

impl Insights {
    /// Folds a batch of lines in. A line without a timestamp takes the time of the timestamped
    /// line before it, in this batch or an earlier one, or else of the first timestamped line
    /// in the batch; `now` stands in only when there is neither. A replayed history so keeps
    /// its own times instead of the reload time, which would wipe the minute buckets.
    pub fn ingest(&mut self, lines: &[LogLine], now: SystemTime) {
        let first_stamp = lines
            .iter()
            .find_map(|line| line.timestamp.as_deref().and_then(parse_timestamp));
        let mut last_at = self.last_at.or(first_stamp).unwrap_or(now);
        for line in lines {
            if self.last_seq.is_some_and(|seen| line.seq <= seen) {
                continue;
            }
            self.last_seq = Some(line.seq);
            if let Some(dropped) = line.dropped {
                self.dropped_lines += dropped;
                continue;
            }
            let at = match line.timestamp.as_deref().and_then(parse_timestamp) {
                Some(stamp) => {
                    last_at = stamp;
                    self.last_at = Some(stamp);
                    stamp
                }
                None => last_at,
            };
            self.lines_total += 1;
            self.count_line(line, at);
            self.note_problem(line, at);
            self.read_facts(line, at);
        }
    }

    fn count_line(&mut self, line: &LogLine, at: SystemTime) {
        match (line.stream, line.level) {
            (LogStream::Stderr, _) => self.totals.stderr += 1,
            (_, Some(LogLevel::Trace)) => self.totals.trace += 1,
            (_, Some(LogLevel::Debug)) => self.totals.debug += 1,
            (_, Some(LogLevel::Info)) => self.totals.info += 1,
            (_, Some(LogLevel::Warn)) => self.totals.warn += 1,
            (_, Some(LogLevel::Error)) => self.totals.error += 1,
            (_, None) => {}
        }
        let minute = epoch_minute(at);
        let Some(bucket) = self.bucket_for(minute) else {
            return;
        };
        match (line.stream, line.level) {
            (LogStream::Stderr, _) | (_, Some(LogLevel::Error)) => bucket.error += 1,
            (_, Some(LogLevel::Warn)) => bucket.warn += 1,
            (_, Some(LogLevel::Info)) => bucket.info += 1,
            _ => bucket.other += 1,
        }
    }

    /// The bucket for an epoch minute, extending the deque forward as minutes pass. A line
    /// older than the oldest kept minute is not counted.
    fn bucket_for(&mut self, minute: u64) -> Option<&mut MinuteBucket> {
        match self.buckets.back() {
            None => self.buckets.push_back(MinuteBucket::empty(minute)),
            Some(newest) => {
                let newest_minute = epoch_minute(newest.minute_start);
                if minute > newest_minute {
                    if minute - newest_minute >= BUCKETS_KEPT as u64 {
                        self.buckets.clear();
                        self.buckets.push_back(MinuteBucket::empty(minute));
                    } else {
                        for m in newest_minute + 1..=minute {
                            self.buckets.push_back(MinuteBucket::empty(m));
                        }
                    }
                }
            }
        }
        while self.buckets.len() > BUCKETS_KEPT {
            self.buckets.pop_front();
        }
        let oldest = epoch_minute(self.buckets.front()?.minute_start);
        let index = usize::try_from(minute.checked_sub(oldest)?).ok()?;
        self.buckets.get_mut(index)
    }

    fn note_problem(&mut self, line: &LogLine, at: SystemTime) {
        let Some(level) = ProblemLevel::of(line) else {
            return;
        };
        self.unseen_problems = self.unseen_problems.saturating_add(1);
        let pattern = pattern_of(&line.message);
        let run_start = self.runs.last().map(|run| run.started_at);
        if let Some(problem) = self.problems.iter_mut().find(|p| p.pattern == pattern) {
            problem.count = problem.count.saturating_add(1);
            problem.last = problem.last.max(at);
            problem.last_seq = line.seq;
            problem.sample = line.message.clone();
            problem.level = level;
            // A pattern from an earlier run recurring in this one: this run's first time.
            if run_start.is_some_and(|start| problem.run_first < start && at >= start) {
                problem.run_first = at;
            }
            return;
        }
        if self.problems.len() >= PROBLEMS_KEPT
            && let Some(oldest) = self
                .problems
                .iter()
                .enumerate()
                .min_by_key(|(_, p)| (p.last, p.last_seq))
                .map(|(index, _)| index)
        {
            self.problems.swap_remove(oldest);
        }
        self.problems.push(Problem {
            level,
            target: line.target.clone(),
            pattern,
            sample: line.message.clone(),
            count: 1,
            first: at,
            run_first: at,
            last: at,
            last_seq: line.seq,
        });
    }

    fn read_facts(&mut self, line: &LogLine, at: SystemTime) {
        let message = line.message.as_str();
        if line.stream == LogStream::Launcher {
            // The launcher's own note that zebrad is gone, however it went.
            if message.starts_with("zebrad exited")
                && let Some(run) = self.runs.last_mut()
            {
                run.stopped_at.get_or_insert(at);
                run.shutdown_reason
                    .get_or_insert_with(|| message.to_owned());
            }
            if let Some(event) = events::launcher_event(message, at) {
                self.launcher_events.push_back(event);
                while self.launcher_events.len() > LAUNCHER_EVENTS_KEPT {
                    self.launcher_events.pop_front();
                }
            }
            return;
        }
        if line.level.is_none() {
            if self.in_metadata
                && line.stream == LogStream::Stdout
                && let Some(run) = self.runs.last_mut()
            {
                if let Some(version) = message.strip_prefix("version:") {
                    run.version = Some(version.trim().to_owned());
                } else if let Some(network) = message.strip_prefix("Zcash network:") {
                    run.network = Some(network.trim().to_owned());
                }
            }
            return;
        }
        self.in_metadata = false;
        let target = line.target.as_deref().unwrap_or("");

        if target == "zebrad::components::tracing::component"
            && message.starts_with("started tracing component")
        {
            self.start_run(at);
            return;
        }
        if target == "zebrad::application" && message.starts_with("Diagnostic metadata:") {
            self.in_metadata = true;
            return;
        }
        if (target == PROGRESS_TARGET || target.is_empty())
            && let Some(stage) = SyncStage::classify(message)
        {
            self.read_progress(stage, message, at);
            return;
        }
        if target == "zebra_consensus::checkpoint" {
            if message.starts_with("verified checkpoint range") {
                if let Some(count) = field(message, "block_count").and_then(u64_of) {
                    self.checkpoints.blocks_verified =
                        self.checkpoints.blocks_verified.saturating_add(count);
                }
            } else if message.starts_with("verified final checkpoint") {
                self.checkpoints.final_reached.get_or_insert(at);
                self.reach(Milestone::FullValidation, at);
            }
            return;
        }
        if target == "zebra_network::peer_set::initialize" {
            if message.starts_with("finished connecting to initial seed and disk cache peers") {
                let number = |key| field(message, key).and_then(u64_of).unwrap_or(0) as u32;
                self.peers.initial_successes = number("handshake_success_total");
                self.peers.initial_errors = number("handshake_error_total");
                self.peers.outbound = number("outbound_connections");
                self.reach(Milestone::PeersConnected, at);
            } else if message.starts_with("Opened Zcash protocol endpoint") {
                self.reach(Milestone::P2pListening, at);
            } else if message.starts_with("an initial peer connection failed")
                || message.starts_with("failed to make outbound connection")
            {
                self.peers.failed_connections = self.peers.failed_connections.saturating_add(1);
            }
            return;
        }
        if target == "zebra_network::peer_set::set" {
            if message.starts_with("dropping stalled peer") {
                self.peers.stalled_dropped = self.peers.stalled_dropped.saturating_add(1);
            } else if message.starts_with("network request with no ready peers") {
                self.peers.no_ready_peers_events =
                    self.peers.no_ready_peers_events.saturating_add(1);
            }
            return;
        }
        if target == "zebrad::components::mempool" {
            if message.starts_with("activating mempool") {
                self.mempool.active_since.get_or_insert(at);
                self.mempool.tip_height = field(message, "tip_height").and_then(height_of);
                self.reach(Milestone::MempoolActive, at);
            } else if message.starts_with("resetting mempool") {
                self.mempool.resets = self.mempool.resets.saturating_add(1);
                if let Some(height) = field(message, "tip_height").and_then(height_of) {
                    self.mempool.tip_height = Some(height);
                }
            }
            return;
        }
        match target {
            "zebrad::components::metrics" if message.starts_with("Opened metrics endpoint") => {
                self.reach(Milestone::Metrics, at);
            }
            "zebra_state::service::finalized_state::disk_db"
                if message.starts_with("Opened Zebra state cache") =>
            {
                self.reach(Milestone::StateOpened, at);
            }
            "zebra_rpc::server" if message.starts_with("Opened RPC endpoint") => {
                self.reach(Milestone::Rpc, at);
            }
            "zebrad::commands::start" if message.starts_with("Starting zebrad") => {
                self.reach(Milestone::Started, at);
            }
            "zebrad::commands::start" if message.starts_with("stopping zebrad") => {
                if let Some(run) = self.runs.last_mut() {
                    run.stopped_at = Some(at);
                }
            }
            "zebrad::signal" => {
                let reason = message
                    .strip_suffix(", starting shutdown")
                    .map(|reason| reason.strip_prefix("received ").unwrap_or(reason));
                if let (Some(reason), Some(run)) = (reason, self.runs.last_mut()) {
                    run.shutdown_reason = Some(reason.to_owned());
                }
            }
            _ => {}
        }
    }

    fn start_run(&mut self, at: SystemTime) {
        // The launcher's start note is stamped when zebrad was spawned, a moment before
        // zebrad's first line; the run begins at that line, so the note moves to it.
        if let Some(note) = self.launcher_events.back_mut()
            && note.kind == EventKind::Started
            && note.at < at
        {
            note.at = at;
        }
        self.runs.push(Run {
            started_at: at,
            version: None,
            network: None,
            milestones: vec![(Milestone::Tracing, at)],
            shutdown_reason: None,
            stopped_at: None,
        });
        if self.runs.len() > RUNS_KEPT {
            self.runs.remove(0);
        }
        // The facts below describe the current run.
        self.checkpoints = Checkpoints::default();
        self.peers = Peers::default();
        self.mempool = Mempool::default();
        self.progress_points.clear();
        self.first_progress_percent = None;
    }

    fn reach(&mut self, milestone: Milestone, at: SystemTime) {
        if let Some(run) = self.runs.last_mut() {
            run.reach(milestone, at);
        }
    }

    fn read_progress(&mut self, stage: SyncStage, message: &str, at: SystemTime) {
        let height = field(message, "current_height").and_then(height_of);
        let percent = field(message, "sync_percent").and_then(percent_of);
        if self.first_progress_percent.is_none() {
            self.first_progress_percent = percent;
        }
        self.sync = Some(SyncSnapshot {
            stage,
            percent,
            height,
            remaining: field(message, "remaining_sync_blocks").and_then(u64_of),
            network_upgrade: field(message, "network_upgrade").map(str::to_owned),
            since_last_block: field(message, "time_since_last_state_block").map(str::to_owned),
            at,
        });
        if let Some(height) = height {
            self.progress_points.push_back((at, height));
            while self.progress_points.len() > PROGRESS_POINTS_KEPT {
                self.progress_points.pop_front();
            }
        }
        self.reach(Milestone::FirstProgress, at);
    }

    /// Lines of each level since the stream began.
    pub fn totals(&self) -> LevelTotals {
        self.totals
    }

    /// Lines folded in, gap markers excluded.
    pub fn lines_total(&self) -> u64 {
        self.lines_total
    }

    /// Lines the backend reported as dropped.
    pub fn dropped_lines(&self) -> u64 {
        self.dropped_lines
    }

    /// WARN, ERROR and stderr lines since [`Insights::mark_seen`].
    pub fn unseen_problems(&self) -> u32 {
        self.unseen_problems
    }

    /// The user has looked at the problems.
    pub fn mark_seen(&mut self) {
        self.unseen_problems = 0;
    }

    /// Lines per minute for the 60 minutes ending at `now`'s minute, oldest first.
    pub fn activity(&self, now: SystemTime) -> Vec<MinuteBucket> {
        let end = epoch_minute(now);
        let start = end.saturating_sub(ACTIVITY_MINUTES as u64 - 1);
        let oldest = self.buckets.front().map(|b| epoch_minute(b.minute_start));
        (start..=end)
            .map(|minute| {
                oldest
                    .and_then(|oldest| usize::try_from(minute.checked_sub(oldest)?).ok())
                    .and_then(|index| self.buckets.get(index))
                    .copied()
                    .unwrap_or_else(|| MinuteBucket::empty(minute))
            })
            .collect()
    }

    /// The distinct problems, most recently seen first, at most `limit`.
    pub fn problems(&self, limit: usize) -> Vec<Problem> {
        let mut problems = self.problems.clone();
        problems.sort_by_key(|problem| std::cmp::Reverse((problem.last, problem.last_seq)));
        problems.truncate(limit);
        problems
    }

    /// The latest progress line, if any arrived.
    pub fn sync(&self) -> Option<&SyncSnapshot> {
        self.sync.as_ref()
    }

    /// Blocks per minute between the last two progress heights with distinct timestamps.
    pub fn sync_rate_from_logs(&self) -> Option<f64> {
        let (last_at, last_height) = *self.progress_points.back()?;
        let (earlier_at, earlier_height) = *self
            .progress_points
            .iter()
            .rev()
            .find(|(at, _)| *at != last_at)?;
        let elapsed = last_at.duration_since(earlier_at).ok()?;
        if last_height < earlier_height {
            return None;
        }
        let minutes = elapsed.as_secs_f64() / 60.0;
        (minutes > 0.0).then(|| (last_height - earlier_height) as f64 / minutes)
    }

    pub fn checkpoints(&self) -> Checkpoints {
        self.checkpoints
    }

    pub fn peers(&self) -> Peers {
        self.peers
    }

    pub fn mempool(&self) -> Mempool {
        self.mempool
    }

    /// The runs seen, oldest first, at most five.
    pub fn runs(&self) -> &[Run] {
        &self.runs
    }

    /// The most recent run.
    pub fn current_run(&self) -> Option<&Run> {
        self.runs.last()
    }

    /// Keeps a status transition the UI observed ([`events::status_events`] and the like), so
    /// [`Insights::events`] can merge it with what the log says.
    pub fn record_session_event(&mut self, event: RunEvent) {
        self.session_events.push_back(event);
        while self.session_events.len() > SESSION_EVENTS_KEPT {
            self.session_events.pop_front();
        }
    }

    /// The "This run" timeline: the newest `limit` events, newest first. The current run's
    /// milestones (those within 2 s of each other on one row), the launcher's own notes since
    /// the previous run stopped, the first appearance of each ERROR and stderr problem of the
    /// run, the status transitions the UI observed, and one line per earlier run. A session
    /// event that repeats a logged one is shown once ([`events::merge_events`]).
    pub fn events(&self, limit: usize) -> Vec<RunEvent> {
        let mut log = Vec::new();
        let current = self.runs.last();
        // Events before the previous run stopped belong to that run's one line; a run's own
        // exit is that line too.
        let earlier = self
            .runs
            .len()
            .checked_sub(2)
            .map(|index| &self.runs[index]);
        let cutoff = earlier
            .and_then(|run| run.stopped_at)
            .or_else(|| current.map(|run| run.started_at));
        let run_start = current.map(|run| run.started_at);
        let keeps = |event: &RunEvent| {
            cutoff.is_none_or(|cutoff| event.at >= cutoff)
                && !(event.kind == EventKind::Exited
                    && run_start.is_some_and(|start| event.at < start))
        };
        if let Some(run) = current {
            log.extend(events::collapse_milestones(self.milestone_events(run)));
        }
        for (index, window) in self.runs.windows(2).enumerate() {
            let (run, next) = (&window[0], &window[1]);
            log.push(events::earlier_run_event(
                index + 1,
                run.shutdown_reason.as_deref(),
                run.stopped_at.unwrap_or(next.started_at),
            ));
        }
        log.extend(self.launcher_events.iter().filter(|e| keeps(e)).cloned());
        log.extend(
            self.problems
                .iter()
                .filter(|problem| problem.level != ProblemLevel::Warn)
                .filter(|problem| run_start.is_none_or(|start| problem.run_first >= start))
                .map(events::problem_event),
        );
        let session: Vec<RunEvent> = self
            .session_events
            .iter()
            .filter(|e| keeps(e))
            .cloned()
            .collect();
        events::merge_events(log, &session, limit)
    }

    /// The current run's startup steps as rows, oldest first. The launcher's start note stands
    /// for the run's first line; without one (a log without launcher notes) the first line
    /// itself does. `Starting zebrad` is the launcher's cue, not a step the reader needs.
    fn milestone_events(&self, run: &Run) -> Vec<RunEvent> {
        let started_note = self.launcher_events.iter().any(|event| {
            event.kind == EventKind::Started
                && event.at
                    >= run
                        .started_at
                        .checked_sub(START_NOTE_SLACK)
                        .unwrap_or(run.started_at)
                && event.at <= run.started_at + START_NOTE_SLACK
        });
        run.milestones
            .iter()
            .filter_map(|&(milestone, at)| {
                let (kind, text) = match milestone {
                    Milestone::Tracing if started_note => return None,
                    Milestone::Tracing => (EventKind::Started, "zebrad started".to_owned()),
                    Milestone::Started => return None,
                    Milestone::StateOpened => {
                        (EventKind::Milestone, "State database opened".to_owned())
                    }
                    Milestone::P2pListening => {
                        (EventKind::Milestone, "Listening for peers".to_owned())
                    }
                    Milestone::Rpc => (EventKind::Milestone, "RPC opened".to_owned()),
                    Milestone::Metrics => (EventKind::Milestone, "Metrics opened".to_owned()),
                    Milestone::PeersConnected => (
                        EventKind::Milestone,
                        format!(
                            "Initial peers connected ({} of {})",
                            self.peers.initial_successes,
                            self.peers.initial_successes + self.peers.initial_errors
                        ),
                    ),
                    Milestone::FirstProgress => (
                        EventKind::Milestone,
                        match self.first_progress_percent {
                            Some(percent) => format!("First progress report ({percent:.1} %)"),
                            None => "First progress report".to_owned(),
                        },
                    ),
                    Milestone::MempoolActive => (EventKind::Milestone, "Mempool active".to_owned()),
                    Milestone::FullValidation => {
                        (EventKind::Milestone, "Full validation started".to_owned())
                    }
                };
                Some(RunEvent {
                    at,
                    kind,
                    text,
                    tone: Tone::Neutral,
                })
            })
            .collect()
    }

    /// When the newest timestamped line was logged.
    pub fn last_at(&self) -> Option<SystemTime> {
        self.last_at
    }
}

/// Orders problems newest first by the minute they were last seen, errors and stderr ahead of
/// warnings within a minute, then by the exact time.
pub fn order_problems(problems: &mut [Problem]) {
    let rank = |level: ProblemLevel| match level {
        ProblemLevel::Error | ProblemLevel::Stderr => 0,
        ProblemLevel::Warn => 1,
    };
    problems.sort_by_key(|problem| {
        (
            Reverse(epoch_minute(problem.last)),
            rank(problem.level),
            Reverse((problem.last, problem.last_seq)),
        )
    });
}

/// `+0.08 s`, `+1.2 s`, `+45 s`, `+3 min`: a step's time since the run started.
pub fn elapsed_text(elapsed: Duration) -> String {
    let secs = elapsed.as_secs_f64();
    if secs < 1.0 {
        format!("+{secs:.2} s")
    } else if secs < 10.0 {
        format!("+{secs:.1} s")
    } else {
        format!("+{}", crate::format::rough_duration(elapsed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: &str = "2026-10-23T18:35:01.937583Z";

    fn at(timestamp: &str) -> SystemTime {
        parse_timestamp(timestamp).expect("test timestamp")
    }

    fn line(seq: u64, level: LogLevel, target: &str, message: &str) -> LogLine {
        stamped(seq, T0, level, target, message)
    }

    fn stamped(seq: u64, timestamp: &str, level: LogLevel, target: &str, message: &str) -> LogLine {
        LogLine {
            seq,
            stream: LogStream::Stdout,
            timestamp: Some(timestamp.into()),
            level: Some(level),
            spans: None,
            target: Some(target.into()),
            message: message.into(),
            dropped: None,
        }
    }

    fn continuation(seq: u64, message: &str) -> LogLine {
        LogLine {
            seq,
            stream: LogStream::Stdout,
            timestamp: None,
            level: None,
            spans: None,
            target: None,
            message: message.into(),
            dropped: None,
        }
    }

    fn stderr(seq: u64, message: &str) -> LogLine {
        LogLine {
            stream: LogStream::Stderr,
            ..continuation(seq, message)
        }
    }

    /// A launcher note as the supervisor writes it: stamped, at INFO, from its own target.
    fn launcher_at(seq: u64, timestamp: &str, message: &str) -> LogLine {
        LogLine {
            stream: LogStream::Launcher,
            ..stamped(
                seq,
                timestamp,
                LogLevel::Info,
                "launcher_core::supervisor",
                message,
            )
        }
    }

    fn launcher(seq: u64, message: &str) -> LogLine {
        launcher_at(seq, T0, message)
    }

    fn progress(seq: u64, timestamp: &str, height: u64) -> LogLine {
        stamped(
            seq,
            timestamp,
            LogLevel::Info,
            PROGRESS_TARGET,
            &format!(
                "estimated progress to chain tip sync_percent=12.345% current_height=Height({height}) \
                 network_upgrade=Nu6 remaining_sync_blocks=2000000 time_since_last_state_block=5s"
            ),
        )
    }

    /// The startup of a real run, in the shapes zebrad prints.
    fn startup(first_seq: u64, stamp: &str) -> Vec<LogLine> {
        // `(target, message)`; no target means a continuation line.
        let specs: [(Option<&str>, &str); 22] = [
            (
                Some("zebrad::components::tracing::component"),
                r#"started tracing component filter="info" TRACING_STATIC_MAX_LEVEL=LevelFilter::INFO LOG_STATIC_MAX_LEVEL=Info"#,
            ),
            (Some("zebrad::application"), "Diagnostic metadata:"),
            (None, "version: 7.0.0-rc.0+192.gad6528b.modified"),
            (None, "Zcash network: Regtest"),
            (None, "running state version: 29.0.0"),
            (
                Some("zebrad::components::metrics"),
                "Opened metrics endpoint at 127.0.0.1:9999",
            ),
            (Some("zebrad::commands::start"), "Starting zebrad"),
            (Some("zebrad::commands::start"), "initializing node state"),
            (
                Some("zebra_state::service::finalized_state::disk_db"),
                "Opened Zebra state cache at /data/state/v29/mainnet",
            ),
            (
                Some("zebra_state::service"),
                "loaded Zebra state cache chain_tip=Some(Height(123))",
            ),
            (
                Some("zebra_network::peer_set::initialize"),
                "Opened Zcash protocol endpoint at [::]:8233",
            ),
            (
                Some("zebra_network::peer_set::initialize"),
                "connecting to initial peer set ipv4_peer_count=14 ipv6_peer_count=1",
            ),
            (
                Some("zebra_network::peer_set::initialize"),
                "an initial peer connection failed successes=11 errors=1 addr=203.0.113.77:8233 e=connection timed out",
            ),
            (
                Some("zebra_network::peer_set::initialize"),
                "finished connecting to initial seed and disk cache peers handshake_success_total=12 handshake_error_total=3 outbound_connections=12",
            ),
            (
                Some("zebra_rpc::server"),
                "Opened RPC endpoint at 127.0.0.1:8232",
            ),
            (
                Some("zebra_consensus::checkpoint"),
                "verified checkpoint range block_count=400 current_range=(Unbounded, Included(Height(399)))",
            ),
            (
                Some("zebra_consensus::checkpoint"),
                "verified checkpoint range block_count=400 current_range=(Excluded(Height(399)), Included(Height(799)))",
            ),
            (
                Some("zebra_consensus::checkpoint"),
                "verified final checkpoint: starting full validation final_checkpoint_height=Height(2900000)",
            ),
            (
                Some("zebrad::commands::start"),
                "spawned initial Zebra tasks",
            ),
            (
                Some("zebrad::components::mempool"),
                "activating mempool: Zebra is close to the tip tip_height=Height(2949999)",
            ),
            (
                Some("zebrad::components::sync::end_of_support"),
                "Starting end of support task",
            ),
            (
                Some("zebrad::components::sync"),
                "starting sync, obtaining new tips state_tip=Some(Height(2949999))",
            ),
        ];
        specs
            .into_iter()
            .enumerate()
            .map(|(index, (target, message))| {
                let seq = first_seq + index as u64;
                match target {
                    Some(target) => stamped(seq, stamp, LogLevel::Info, target, message),
                    None => continuation(seq, message),
                }
            })
            .collect()
    }

    #[test]
    fn dedup_by_seq_and_totals() {
        let mut insights = Insights::default();
        let now = at(T0);
        let batch = vec![
            line(
                0,
                LogLevel::Info,
                "zebrad::commands::start",
                "Starting zebrad",
            ),
            line(1, LogLevel::Warn, "zebrad::commands::start", "careful"),
            continuation(2, "detail"),
            stderr(3, "thread 'main' panicked"),
        ];
        insights.ingest(&batch, now);
        // History refetch repeats the lines and adds one.
        let mut again = batch.clone();
        again.push(line(4, LogLevel::Error, "zebrad::components::sync", "bad"));
        insights.ingest(&again, now);
        assert_eq!(insights.lines_total(), 5);
        let totals = insights.totals();
        assert_eq!(
            (totals.info, totals.warn, totals.error, totals.stderr),
            (1, 1, 1, 1)
        );
        assert_eq!(totals.at_or_above(LogLevel::Warn), 2);
        assert_eq!(totals.at_or_above(LogLevel::Trace), 3);
        assert_eq!(insights.unseen_problems(), 3);
        insights.mark_seen();
        assert_eq!(insights.unseen_problems(), 0);
        // Older lines arriving late are duplicates too.
        insights.ingest(&[line(2, LogLevel::Info, "x", "late")], now);
        assert_eq!(insights.lines_total(), 5);
    }

    #[test]
    fn gap_markers_count_dropped_lines_and_are_not_analysed() {
        let mut insights = Insights::default();
        insights.ingest(&[LogLine::gap_marker(7, 9)], at(T0));
        assert_eq!(insights.dropped_lines(), 7);
        assert_eq!(insights.lines_total(), 0);
        assert!(insights.problems(10).is_empty());
        assert_eq!(insights.unseen_problems(), 0);
        // A launcher note that reads like a marker is an ordinary line.
        insights.ingest(&[launcher(10, "7 lines dropped")], at(T0));
        assert_eq!(insights.dropped_lines(), 7);
        assert_eq!(insights.lines_total(), 1);
    }

    #[test]
    fn activity_buckets_roll_by_minute() {
        let mut insights = Insights::default();
        let batch = vec![
            stamped(0, "2026-10-23T18:35:01Z", LogLevel::Info, "t", "a"),
            stamped(1, "2026-10-23T18:35:59Z", LogLevel::Warn, "t", "b"),
            stamped(2, "2026-10-23T18:37:00Z", LogLevel::Error, "t", "c"),
            stamped(3, "2026-10-23T18:37:30Z", LogLevel::Debug, "t", "d"),
            stderr(4, "panicked"),
            continuation(5, "more"),
        ];
        // Lines without a timestamp take the time of the stamped line before them.
        let now = at("2026-10-23T18:38:10Z");
        insights.ingest(&batch, now);
        let activity = insights.activity(now);
        assert_eq!(activity.len(), ACTIVITY_MINUTES);
        assert_eq!(
            activity.last().unwrap().minute_start,
            at("2026-10-23T18:38:00Z")
        );
        assert_eq!(activity[0].minute_start, at("2026-10-23T17:39:00Z"));
        let by_minute = |minute: &str| {
            let start = at(minute);
            *activity.iter().find(|b| b.minute_start == start).unwrap()
        };
        let m35 = by_minute("2026-10-23T18:35:00Z");
        assert_eq!((m35.info, m35.warn, m35.error, m35.other), (1, 1, 0, 0));
        assert_eq!(by_minute("2026-10-23T18:36:00Z").total(), 0);
        let m37 = by_minute("2026-10-23T18:37:00Z");
        assert_eq!((m37.info, m37.warn, m37.error, m37.other), (0, 0, 2, 2));
        assert_eq!(by_minute("2026-10-23T18:38:00Z").total(), 0);

        // Two hours later the old minutes have rolled out and the deque stays bounded.
        let later = at("2026-10-23T20:40:00Z");
        insights.ingest(
            &[stamped(6, "2026-10-23T20:40:00Z", LogLevel::Info, "t", "e")],
            later,
        );
        assert!(insights.buckets.len() <= BUCKETS_KEPT);
        let activity = insights.activity(later);
        assert_eq!(activity.iter().map(MinuteBucket::total).sum::<u32>(), 1);
        // A minute at a time for a while keeps exactly the bounded window.
        for i in 1..=90u64 {
            let minutes = 40 + i;
            let stamp = format!(
                "2026-10-23T{:02}:{:02}:00Z",
                20 + minutes / 60,
                minutes % 60
            );
            insights.ingest(&[stamped(6 + i, &stamp, LogLevel::Info, "t", "f")], later);
        }
        assert_eq!(insights.buckets.len(), BUCKETS_KEPT);
        // A line older than the window is dropped from the counts, not misfiled.
        let before = insights.clone();
        insights.ingest(
            &[stamped(
                500,
                "2026-10-23T18:35:01Z",
                LogLevel::Info,
                "t",
                "old",
            )],
            later,
        );
        assert_eq!(insights.buckets, before.buckets);
        assert_eq!(insights.lines_total(), before.lines_total() + 1);
    }

    #[test]
    fn leveled_launcher_lines_count_like_zebrad_lines() {
        // The launcher's own WARN (a pid file it could not write) counts in the totals, the
        // minute bucket and the problems alike; its INFO note in the totals and the bucket.
        let mut insights = Insights::default();
        let now = at("2026-10-23T18:36:00Z");
        let warn = LogLine {
            level: Some(LogLevel::Warn),
            ..launcher_at(
                0,
                "2026-10-23T18:35:10Z",
                "could not write the zebrad pid file err=denied",
            )
        };
        insights.ingest(
            &[
                warn,
                launcher_at(1, "2026-10-23T18:35:11Z", "zebrad started (pid 1)"),
            ],
            now,
        );
        let totals = insights.totals();
        assert_eq!((totals.info, totals.warn, totals.error), (1, 1, 0));
        let minute = insights
            .activity(now)
            .into_iter()
            .find(|b| b.minute_start == at("2026-10-23T18:35:00Z"))
            .unwrap();
        assert_eq!(
            (minute.info, minute.warn, minute.error, minute.other),
            (1, 1, 0, 0),
            "the buckets agree with the totals"
        );
        let problems = insights.problems(5);
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].level, ProblemLevel::Warn);
        assert_eq!(
            problems[0].target.as_deref(),
            Some("launcher_core::supervisor")
        );
        // A launcher line without a level is one of the other lines, not a problem.
        let plain = LogLine {
            level: None,
            timestamp: None,
            ..launcher_at(2, T0, "7 lines dropped")
        };
        insights.ingest(&[plain], now);
        let minute = insights
            .activity(now)
            .into_iter()
            .find(|b| b.minute_start == at("2026-10-23T18:35:00Z"))
            .unwrap();
        assert_eq!(minute.other, 1);
        assert_eq!(insights.problems(5).len(), 1);
    }

    #[test]
    fn problems_dedupe_count_and_order() {
        let mut insights = Insights::default();
        let now = at("2026-10-23T18:36:00Z");
        let stalled = |seq, stamp, addr: &str| {
            stamped(
                seq,
                stamp,
                LogLevel::Warn,
                "zebra_network::peer_set::set",
                &format!(
                    "dropping stalled peer: exceeded FindBlocks/FindHeaders stall threshold addr={addr}"
                ),
            )
        };
        insights.ingest(
            &[
                stalled(0, "2026-10-23T18:35:01Z", "203.0.113.5:8233"),
                line(
                    1,
                    LogLevel::Error,
                    "zebrad::components::sync",
                    "block download failed hash=block::Hash(\"abc123\")",
                ),
                stalled(2, "2026-10-23T18:35:05Z", "203.0.113.9:8233"),
                stderr(
                    3,
                    "thread 'main' (210466) panicked at zebrad/src/components/metrics.rs:38:27:",
                ),
                line(4, LogLevel::Info, "x", "fine"),
            ],
            now,
        );
        let problems = insights.problems(10);
        assert_eq!(problems.len(), 3);
        // stderr took the time of the line before it, the newest; the two stalled peers are
        // one problem.
        assert_eq!(problems[0].level, ProblemLevel::Stderr);
        assert_eq!(problems[0].first, at("2026-10-23T18:35:05Z"));
        assert_eq!(
            problems[1].pattern,
            "dropping stalled peer: exceeded FindBlocks/FindHeaders stall threshold addr=_"
        );
        assert_eq!(problems[1].count, 2);
        assert_eq!(problems[1].first, at("2026-10-23T18:35:01Z"));
        assert_eq!(problems[1].last, at("2026-10-23T18:35:05Z"));
        assert_eq!(problems[1].last_seq, 2);
        assert!(problems[1].sample.ends_with("203.0.113.9:8233"));
        assert_eq!(
            problems[1].target.as_deref(),
            Some("zebra_network::peer_set::set")
        );
        assert_eq!(problems[2].level, ProblemLevel::Error);
        assert_eq!(insights.problems(1).len(), 1);
        assert_eq!(insights.unseen_problems(), 4);
        assert_eq!(
            pick_filter(&problems[1]),
            "dropping stalled peer: exceeded FindBlocks/FindHeaders stall"
        );
        assert_eq!(pick_filter(&problems[2]), "block download failed");
        let fields_only = Problem {
            sample: "height=Height(0) hash=x".into(),
            ..problems[2].clone()
        };
        assert_eq!(pick_filter(&fields_only), "height=Height(0) hash=x");
    }

    fn problem(level: ProblemLevel, last_secs: u64, seq: u64) -> Problem {
        let at = |secs| std::time::UNIX_EPOCH + Duration::from_secs(secs);
        Problem {
            level,
            target: None,
            pattern: format!("p{seq}"),
            sample: String::new(),
            count: 1,
            first: at(0),
            run_first: at(0),
            last: at(last_secs),
            last_seq: seq,
        }
    }

    #[test]
    fn problems_order_errors_first_within_a_minute() {
        let mut problems = vec![
            problem(ProblemLevel::Warn, 125, 1),
            problem(ProblemLevel::Error, 121, 2),
            problem(ProblemLevel::Warn, 59, 3),
            problem(ProblemLevel::Stderr, 122, 4),
            problem(ProblemLevel::Warn, 123, 5),
            problem(ProblemLevel::Error, 180, 6),
        ];
        order_problems(&mut problems);
        let order: Vec<u64> = problems.iter().map(|p| p.last_seq).collect();
        // Minute 3 first; in minute 2 the stderr (122) and error (121) ahead of the warnings,
        // newest first within each group; minute 0 last.
        assert_eq!(order, vec![6, 4, 2, 1, 5, 3]);
    }

    #[test]
    fn problems_evict_the_oldest_pattern() {
        let mut insights = Insights::default();
        let lines: Vec<LogLine> = (0..PROBLEMS_KEPT as u64 + 5)
            .map(|i| {
                stamped(
                    i,
                    &format!("2026-10-23T18:{:02}:00Z", i % 60),
                    LogLevel::Warn,
                    "t",
                    &format!("problem kind {}", "x".repeat(i as usize + 1)),
                )
            })
            .collect();
        insights.ingest(&lines, at(T0));
        let problems = insights.problems(100);
        assert_eq!(problems.len(), PROBLEMS_KEPT);
        assert!(
            problems.iter().all(|p| p.last_seq >= 5),
            "the five oldest were evicted"
        );
    }

    #[test]
    fn sync_stages_from_progress_lines() {
        let cases = [
            (
                "estimated progress to chain tip sync_percent=12.345% current_height=Height(1234567) network_upgrade=Nu6 remaining_sync_blocks=2000000 time_since_last_state_block=5s",
                SyncStage::Syncing,
                Tone::Neutral,
            ),
            (
                "close to finishing initial sync, confirming using syncer and gossiped blocks sync_percent=99.9% current_height=Height(2)",
                SyncStage::NearTip,
                Tone::Neutral,
            ),
            (
                "finished initial sync to chain tip, using gossiped blocks sync_percent=100.000% current_height=Height(3)",
                SyncStage::AtTip,
                Tone::Good,
            ),
            (
                "initial sync is very slow, or estimated tip is wrong. Hint: check your network connection sync_percent=50% current_height=Height(4)",
                SyncStage::Slow,
                Tone::Warning,
            ),
            (
                "initial sync is waiting to download the genesis block sync_percent=0.000% current_height=None",
                SyncStage::WaitingForGenesis,
                Tone::Neutral,
            ),
            (
                "chain updates have stalled, state height has not increased for 7 minutes. Hint: check your network connection, and your computer clock and time zone sync_percent=12.345% current_height=Height(1234567) network_upgrade=Nu6 time_since_last_state_block=7m 2s target_block_spacing=75s max_block_spacing=None is_syncer_stopped=false",
                SyncStage::Stalled,
                Tone::Critical,
            ),
            (
                "initial sync can't download and verify the genesis block. Hint: check your network connection, and your computer clock and time zone sync_percent=0.000 % current_height=None",
                SyncStage::GenesisStalled,
                Tone::Critical,
            ),
        ];
        for (seq, (message, stage, tone)) in cases.into_iter().enumerate() {
            let mut insights = Insights::default();
            insights.ingest(
                &[line(seq as u64, LogLevel::Info, PROGRESS_TARGET, message)],
                at(T0),
            );
            let sync = insights.sync().unwrap_or_else(|| panic!("{message}"));
            assert_eq!(sync.stage, stage, "{message}");
            assert_eq!(stage.stage_tone(), tone);
            assert!(!stage.stage_text().is_empty());
            assert_eq!(sync.at, at(T0));
        }
        let mut insights = Insights::default();
        insights.ingest(
            &[line(0, LogLevel::Warn, PROGRESS_TARGET, cases[5].0)],
            at(T0),
        );
        let sync = insights.sync().unwrap();
        assert_eq!(sync.percent, Some(12.345));
        assert_eq!(sync.height, Some(1_234_567));
        assert_eq!(sync.network_upgrade.as_deref(), Some("Nu6"));
        assert_eq!(
            sync.since_last_block.as_deref(),
            Some("7m 2s"),
            "humantime value kept whole"
        );
        assert_eq!(sync.remaining, None);
        insights.ingest(
            &[line(1, LogLevel::Info, PROGRESS_TARGET, cases[0].0)],
            at(T0),
        );
        assert_eq!(insights.sync().unwrap().remaining, Some(2_000_000));
        insights.ingest(
            &[line(2, LogLevel::Warn, PROGRESS_TARGET, cases[6].0)],
            at(T0),
        );
        let sync = insights.sync().unwrap();
        assert_eq!(sync.percent, Some(0.0));
        assert_eq!(sync.height, None);
        // Other lines from the progress target, and progress-like words elsewhere, are ignored.
        insights.ingest(
            &[line(
                3,
                LogLevel::Warn,
                PROGRESS_TARGET,
                "chain tip metrics channel closed err=SendError { .. }",
            )],
            at(T0),
        );
        assert_eq!(insights.sync().unwrap().stage, SyncStage::GenesisStalled);
        insights.ingest(
            &[line(
                4,
                LogLevel::Info,
                "zebrad::other",
                "finished initial sync to chain tip (quoted)",
            )],
            at(T0),
        );
        assert_eq!(insights.sync().unwrap().stage, SyncStage::GenesisStalled);
    }

    #[test]
    fn sync_rate_from_two_progress_heights() {
        let mut insights = Insights::default();
        assert_eq!(insights.sync_rate_from_logs(), None);
        insights.ingest(&[progress(0, "2026-10-23T18:35:00Z", 1_000_000)], at(T0));
        assert_eq!(insights.sync_rate_from_logs(), None, "one point");
        // The same timestamp twice is not a rate.
        insights.ingest(&[progress(1, "2026-10-23T18:35:00Z", 1_000_100)], at(T0));
        assert_eq!(insights.sync_rate_from_logs(), None);
        insights.ingest(&[progress(2, "2026-10-23T18:37:00Z", 1_000_900)], at(T0));
        assert_eq!(insights.sync_rate_from_logs(), Some(400.0));
        // A height going backwards (restart with another state) gives no rate.
        insights.ingest(&[progress(3, "2026-10-23T18:38:00Z", 5)], at(T0));
        assert_eq!(insights.sync_rate_from_logs(), None);
        // The point list is bounded.
        for i in 0..300u64 {
            insights.ingest(
                &[progress(
                    10 + i,
                    &format!("2026-10-24T{:02}:{:02}:00Z", i / 60, i % 60),
                    2_000_000 + i,
                )],
                at(T0),
            );
        }
        assert_eq!(insights.progress_points.len(), PROGRESS_POINTS_KEPT);
        assert_eq!(insights.sync_rate_from_logs(), Some(1.0));
    }

    #[test]
    fn runs_milestones_peers_checkpoints_and_mempool() {
        let mut insights = Insights::default();
        let mut lines = startup(0, "2026-10-23T18:35:01.000Z");
        let n = lines.len() as u64;
        lines.push(progress(n, "2026-10-23T18:36:01.500Z", 2_950_000));
        lines.push(stamped(n + 1, "2026-10-23T18:36:02Z", LogLevel::Warn, "zebra_network::peer_set::set",
            "dropping stalled peer: exceeded FindBlocks/FindHeaders stall threshold addr=203.0.113.5:8233"));
        lines.push(stamped(n + 2, "2026-10-23T18:36:03Z", LogLevel::Info, "zebra_network::peer_set::set",
            "network request with no ready peers: finding more peers, waiting for 1 peers to answer requests"));
        lines.push(stamped(n + 3, "2026-10-23T18:36:04Z", LogLevel::Info, "zebra_network::peer_set::initialize",
            "failed to make outbound connection to peer error=timeout candidate.addr=203.0.113.9:8233"));
        lines.push(stamped(n + 4, "2026-10-23T18:36:05Z", LogLevel::Info, "zebrad::components::mempool",
            "resetting mempool: switched best chain, skipped blocks, or activated network upgrade tip_height=Height(2950001)"));
        insights.ingest(&lines, at(T0));

        let run = insights.current_run().expect("a run");
        assert_eq!(insights.runs().len(), 1);
        assert_eq!(run.started_at, at("2026-10-23T18:35:01.000Z"));
        assert_eq!(
            run.version.as_deref(),
            Some("7.0.0-rc.0+192.gad6528b.modified")
        );
        assert_eq!(run.network.as_deref(), Some("Regtest"));
        let milestones: Vec<Milestone> = run.milestones.iter().map(|(m, _)| *m).collect();
        assert_eq!(
            milestones,
            [
                Milestone::Tracing,
                Milestone::Metrics,
                Milestone::Started,
                Milestone::StateOpened,
                Milestone::P2pListening,
                Milestone::PeersConnected,
                Milestone::Rpc,
                Milestone::FullValidation,
                Milestone::MempoolActive,
                Milestone::FirstProgress,
            ]
        );
        assert_eq!(
            run.milestones.last().unwrap().1,
            at("2026-10-23T18:36:01.500Z")
        );
        assert_eq!(run.shutdown_reason, None);
        assert_eq!(run.stopped_at, None);
        for milestone in milestones {
            assert!(!milestone.label().is_empty());
        }

        let peers = insights.peers();
        assert_eq!(peers.initial_successes, 12);
        assert_eq!(peers.initial_errors, 3);
        assert_eq!(peers.outbound, 12);
        assert_eq!(peers.failed_connections, 2);
        assert_eq!(peers.stalled_dropped, 1);
        assert_eq!(peers.no_ready_peers_events, 1);

        let checkpoints = insights.checkpoints();
        assert_eq!(checkpoints.blocks_verified, 800);
        assert_eq!(
            checkpoints.final_reached,
            Some(at("2026-10-23T18:35:01.000Z"))
        );

        let mempool = insights.mempool();
        assert_eq!(mempool.active_since, Some(at("2026-10-23T18:35:01.000Z")));
        assert_eq!(mempool.tip_height, Some(2_950_001));
        assert_eq!(mempool.resets, 1);
    }

    #[test]
    fn runs_split_on_a_second_tracing_line_and_record_shutdowns() {
        let mut insights = Insights::default();
        let mut lines = startup(0, "2026-10-23T18:35:01Z");
        let n = lines.len() as u64;
        lines.push(LogLine {
            spans: Some(r#"sig{kind=SignalKind(15) name="SIGTERM"}"#.into()),
            ..stamped(
                n,
                "2026-10-23T18:40:00Z",
                LogLevel::Info,
                "zebrad::signal",
                "received SIGTERM, starting shutdown",
            )
        });
        lines.push(stamped(
            n + 1,
            "2026-10-23T18:40:01Z",
            LogLevel::Info,
            "zebrad::components::tokio",
            "shutting down Zebra",
        ));
        lines.push(stamped(
            n + 2,
            "2026-10-23T18:40:02Z",
            LogLevel::Info,
            "zebrad::commands::start",
            "stopping zebrad",
        ));
        lines.push(launcher_at(
            n + 3,
            "2026-10-23T18:40:03Z",
            "zebrad exited with code 0",
        ));
        lines.extend(startup(n + 4, "2026-10-23T18:41:00Z"));
        let m = n + 4 + startup(0, "2026-10-23T18:41:00Z").len() as u64;
        lines.push(stamped(
            m,
            "2026-10-23T18:41:30Z",
            LogLevel::Info,
            "zebrad::signal",
            "stdin closed, starting shutdown",
        ));
        lines.push(launcher_at(
            m + 1,
            "2026-10-23T18:41:32Z",
            "zebrad exited with code 1",
        ));
        insights.ingest(&lines, at("2026-10-23T18:42:00Z"));

        assert_eq!(insights.runs().len(), 2);
        let first = &insights.runs()[0];
        assert_eq!(first.shutdown_reason.as_deref(), Some("SIGTERM"));
        assert_eq!(
            first.stopped_at,
            Some(at("2026-10-23T18:40:02Z")),
            "zebrad's own stop line, not the exit note after it"
        );
        let second = insights.current_run().unwrap();
        assert_eq!(second.started_at, at("2026-10-23T18:41:00Z"));
        assert_eq!(second.shutdown_reason.as_deref(), Some("stdin closed"));
        // No `stopping zebrad`: the launcher's exit note, stamped by the supervisor, gives the
        // stop time.
        assert_eq!(second.stopped_at, Some(at("2026-10-23T18:41:32Z")));
        // Per-run facts were reset by the second run and refilled by its startup.
        assert_eq!(insights.peers().initial_successes, 12);
        assert_eq!(insights.checkpoints().blocks_verified, 800);

        // Only the last five runs are kept.
        let mut seq = m + 2;
        for i in 0..6 {
            let batch = startup(seq, &format!("2026-10-23T19:{i:02}:00Z"));
            seq += batch.len() as u64;
            insights.ingest(&batch, at("2026-10-23T20:00:00Z"));
        }
        assert_eq!(insights.runs().len(), RUNS_KEPT);
        assert_eq!(
            insights.current_run().unwrap().started_at,
            at("2026-10-23T19:05:00Z")
        );
    }

    #[test]
    fn a_crash_without_a_tracing_line_has_no_run() {
        let mut insights = Insights::default();
        insights.ingest(
            &[
                launcher_at(0, "2026-10-23T18:36:00Z", "zebrad exited with code 101"),
                stderr(1, "zebrad fatal error: boom"),
            ],
            at(T0),
        );
        assert_eq!(insights.current_run(), None);
        assert_eq!(insights.problems(5).len(), 1);
        assert_eq!(insights.problems(5)[0].level, ProblemLevel::Stderr);
        // The stderr line takes the time of the stamped note before it, not the ingest time.
        assert_eq!(insights.problems(5)[0].first, at("2026-10-23T18:36:00Z"));
    }

    #[test]
    fn a_replayed_history_keeps_its_own_times() {
        // The webview reloads two hours into a run and the history is replayed: the launcher's
        // note comes first and continuation and stderr lines sit between stamped ones. None
        // may take the reload time, which would wipe the minute buckets and date the problems.
        let mut insights = Insights::default();
        let now = at("2026-10-23T20:35:00Z");
        insights.ingest(
            &[
                launcher(0, "zebrad started (pid 1)"),
                stamped(
                    1,
                    "2026-10-23T18:35:01Z",
                    LogLevel::Info,
                    "t",
                    "Diagnostic metadata:",
                ),
                continuation(2, "version: 7.0.0"),
                stamped(
                    3,
                    "2026-10-23T20:05:10Z",
                    LogLevel::Warn,
                    "t",
                    "block download failed",
                ),
                stderr(4, "thread 'main' panicked"),
                stamped(5, "2026-10-23T20:34:00Z", LogLevel::Info, "t", "fine"),
            ],
            now,
        );
        assert_eq!(insights.lines_total(), 6);
        let activity = insights.activity(now);
        let by_minute = |minute: &str| {
            let start = at(minute);
            *activity.iter().find(|b| b.minute_start == start).unwrap()
        };
        let m2005 = by_minute("2026-10-23T20:05:00Z");
        assert_eq!(
            (m2005.warn, m2005.error),
            (1, 1),
            "the WARN line and the stderr after it"
        );
        assert_eq!(by_minute("2026-10-23T20:34:00Z").info, 1);
        assert_eq!(
            by_minute("2026-10-23T20:35:00Z").total(),
            0,
            "nothing dated at the reload"
        );
        // The 18:35 bucket rolled out, more than an hour before the next line; the oldest kept
        // minute is a line's own, not the reload minute.
        assert_eq!(
            insights.buckets.front().map(|b| b.minute_start),
            Some(at("2026-10-23T20:05:00Z"))
        );
        let problems = insights.problems(5);
        assert_eq!(problems.len(), 2);
        assert!(
            problems
                .iter()
                .all(|p| p.first == at("2026-10-23T20:05:10Z")),
            "{problems:?}"
        );
        // A later batch without a timestamp still follows the last stamped line, not `now`.
        insights.ingest(&[continuation(6, "more")], at("2026-10-23T20:36:00Z"));
        let activity = insights.activity(now);
        let m2034 = activity
            .iter()
            .find(|b| b.minute_start == at("2026-10-23T20:34:00Z"));
        assert_eq!(m2034.map(|b| b.other), Some(1));
    }

    #[test]
    fn last_at_is_the_newest_stamp() {
        let mut insights = Insights::default();
        assert_eq!(insights.last_at(), None);
        insights.ingest(&[launcher(0, "zebrad started (pid 1)")], at(T0));
        assert_eq!(
            insights.last_at(),
            Some(at(T0)),
            "a note carries its own stamp"
        );
        insights.ingest(
            &[
                stamped(1, "2026-10-23T18:35:01Z", LogLevel::Info, "t", "a"),
                continuation(2, "more"),
            ],
            at(T0),
        );
        assert_eq!(insights.last_at(), Some(at("2026-10-23T18:35:01Z")));
    }

    /// The real stream of a run's start: the launcher's note, stamped when zebrad was spawned,
    /// then zebrad's startup lines from `stamp` on.
    fn full_run(first_seq: u64, note_stamp: &str, stamp: &str) -> Vec<LogLine> {
        let mut lines = vec![launcher_at(
            first_seq,
            note_stamp,
            "zebrad started (pid 4242)",
        )];
        lines.extend(startup(first_seq + 1, stamp));
        lines
    }

    #[test]
    fn events_tell_the_run_newest_first() {
        let mut insights = Insights::default();
        let mut lines = full_run(0, "2026-10-23T18:35:00Z", "2026-10-23T18:35:01Z");
        let n = lines.len() as u64;
        lines.push(progress(n, "2026-10-23T18:36:01Z", 2_950_000));
        lines.push(stamped(
            n + 1,
            "2026-10-23T18:36:30Z",
            LogLevel::Warn,
            "zebra_network::peer_set::set",
            "dropping stalled peer: exceeded FindBlocks/FindHeaders stall threshold addr=203.0.113.5:8233",
        ));
        lines.push(stamped(
            n + 2,
            "2026-10-23T18:37:00Z",
            LogLevel::Error,
            "zebrad::components::sync",
            "block download failed hash=block::Hash(\"abc\")",
        ));
        lines.push(stamped(
            n + 3,
            "2026-10-23T18:37:05Z",
            LogLevel::Error,
            "zebrad::components::sync",
            "block download failed hash=block::Hash(\"def\")",
        ));
        insights.ingest(&lines, at("2026-10-23T18:38:00Z"));

        let events = insights.events(20);
        let rows: Vec<(EventKind, &str)> =
            events.iter().map(|e| (e.kind, e.text.as_str())).collect();
        assert_eq!(
            rows,
            [
                (EventKind::Problem, "ERROR: block download failed ×2"),
                (EventKind::Milestone, "First progress report (12.3 %)"),
                // The startup lines share one stamp: one row of milestones, in the order reached,
                // `Starting zebrad` and the tracing line left out.
                (
                    EventKind::Milestone,
                    "Metrics opened · state database opened · listening for peers · initial peers connected (12 of 15) · RPC opened · full validation started · mempool active"
                ),
                (EventKind::Started, "zebrad started (pid 4242)"),
            ]
        );
        // The start note, stamped a second before zebrad's first line, took that line's time.
        assert_eq!(events[3].at, at("2026-10-23T18:35:01Z"));
        assert_eq!(
            events[0].at,
            at("2026-10-23T18:37:00Z"),
            "the problem's first time"
        );
        assert_eq!(events[0].tone, Tone::Critical);
        // The WARN problem is on the Recent problems card, not the timeline.
        assert!(events.iter().all(|e| !e.text.starts_with("WARN")));
        assert_eq!(insights.events(2).len(), 2);
        assert_eq!(
            insights.events(2)[0].text,
            "ERROR: block download failed ×2"
        );
        assert!(insights.events(0).is_empty());
    }

    #[test]
    fn events_without_a_start_note_use_the_first_line() {
        let mut insights = Insights::default();
        insights.ingest(&startup(0, "2026-10-23T18:35:01Z"), at(T0));
        let events = insights.events(10);
        let started = events.last().unwrap();
        assert_eq!(started.kind, EventKind::Started);
        assert_eq!(started.text, "zebrad started");
        assert_eq!(started.at, at("2026-10-23T18:35:01Z"));
        assert_eq!(Insights::default().events(10), Vec::new());
    }

    #[test]
    fn events_fold_earlier_runs_into_one_line_and_keep_the_gap() {
        let mut insights = Insights::default();
        let mut lines = full_run(0, "2026-10-23T18:35:00Z", "2026-10-23T18:35:01Z");
        let n = lines.len() as u64;
        lines.push(stamped(
            n,
            "2026-10-23T18:40:00Z",
            LogLevel::Info,
            "zebrad::components::sync",
            "extending tips",
        ));
        lines.push(stderr(
            n + 1,
            "thread 'main' panicked at zebrad/src/x.rs:1:1:",
        ));
        lines.push(stderr(n + 2, "boom"));
        lines.push(launcher_at(
            n + 3,
            "2026-10-23T18:40:01Z",
            "zebrad exited (exit code 101, Crash)",
        ));
        lines.push(launcher_at(
            n + 4,
            "2026-10-23T18:40:01Z",
            "restarting zebrad in 5s",
        ));
        lines.extend(full_run(
            n + 5,
            "2026-10-23T18:40:06Z",
            "2026-10-23T18:40:07Z",
        ));
        insights.ingest(&lines, at("2026-10-23T18:41:00Z"));

        let events = insights.events(20);
        let rows: Vec<(EventKind, &str)> =
            events.iter().map(|e| (e.kind, e.text.as_str())).collect();
        assert_eq!(
            rows,
            [
                (
                    EventKind::Milestone,
                    "Metrics opened · state database opened · listening for peers · initial peers connected (12 of 15) · RPC opened · full validation started · mempool active"
                ),
                (EventKind::Started, "zebrad started (pid 4242)"),
                // The launcher's decision between the runs stays; the exit note is the run's line.
                (EventKind::Restarting, "Restarting in 5 s"),
                (EventKind::EarlierRun, "Run 1 ended: crash (code 101)"),
            ]
        );
        assert_eq!(
            events[1].at,
            at("2026-10-23T18:40:07Z"),
            "re-dated to the run's first line"
        );
        assert_eq!(
            events[3].at,
            at("2026-10-23T18:40:01Z"),
            "the exit note's own stamp"
        );
        assert_eq!(events[3].tone, Tone::Critical);
        // The first run's stderr problem is not this run's.
        assert!(events.iter().all(|e| e.kind != EventKind::Problem));
        assert_eq!(
            insights.runs()[0].shutdown_reason.as_deref(),
            Some("zebrad exited (exit code 101, Crash)")
        );
    }

    #[test]
    fn a_problem_recurring_in_the_next_run_is_on_its_timeline() {
        let mut insights = Insights::default();
        let failed = |seq, stamp, hash| {
            stamped(
                seq,
                stamp,
                LogLevel::Error,
                "zebrad::components::sync",
                &format!("block download failed hash=block::Hash(\"{hash}\")"),
            )
        };
        let mut lines = full_run(0, "2026-10-23T18:35:00Z", "2026-10-23T18:35:01Z");
        let n = lines.len() as u64;
        lines.push(failed(n, "2026-10-23T18:37:00Z", "abc"));
        lines.push(launcher_at(
            n + 1,
            "2026-10-23T18:40:01Z",
            "zebrad exited (exit code 101, Crash)",
        ));
        lines.extend(full_run(
            n + 2,
            "2026-10-23T18:40:06Z",
            "2026-10-23T18:40:07Z",
        ));
        let m = lines.len() as u64;
        lines.push(failed(m, "2026-10-23T18:45:00Z", "def"));
        insights.ingest(&lines, at("2026-10-23T18:46:00Z"));

        // Recent problems spans the session: one pattern, seen twice, first in run 1.
        let problems = insights.problems(5);
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].count, 2);
        assert_eq!(problems[0].first, at("2026-10-23T18:37:00Z"));
        assert_eq!(problems[0].run_first, at("2026-10-23T18:45:00Z"));
        // This run's timeline shows the recurrence at this run's time.
        let events = insights.events(20);
        let problem_rows: Vec<&RunEvent> = events
            .iter()
            .filter(|e| e.kind == EventKind::Problem)
            .collect();
        assert_eq!(problem_rows.len(), 1);
        assert_eq!(problem_rows[0].text, "ERROR: block download failed ×2");
        assert_eq!(problem_rows[0].at, at("2026-10-23T18:45:00Z"));
    }

    #[test]
    fn session_events_merge_with_the_log() {
        let mut insights = Insights::default();
        insights.ingest(
            &full_run(0, "2026-10-23T18:35:00Z", "2026-10-23T18:35:01Z"),
            at(T0),
        );
        // The UI saw the same start two seconds later, and a stop the log has not caught up with.
        insights.record_session_event(RunEvent {
            at: at("2026-10-23T18:35:03Z"),
            kind: EventKind::Started,
            text: "zebrad started (pid 4242)".into(),
            tone: Tone::Neutral,
        });
        insights.record_session_event(RunEvent {
            at: at("2026-10-23T18:45:00Z"),
            kind: EventKind::StopRequested,
            text: "Stop requested (graceful)".into(),
            tone: Tone::Neutral,
        });
        let events = insights.events(10);
        assert_eq!(events[0].text, "Stop requested (graceful)");
        assert_eq!(
            events
                .iter()
                .filter(|e| e.kind == EventKind::Started)
                .count(),
            1,
            "{events:#?}"
        );
        assert_eq!(
            events.last().unwrap().at,
            at("2026-10-23T18:35:01Z"),
            "the log's time"
        );
        // Session events older than the run's start are not this run's.
        insights.record_session_event(RunEvent {
            at: at("2026-10-23T18:00:00Z"),
            kind: EventKind::Update,
            text: "Launcher updated to 0.2.0".into(),
            tone: Tone::Good,
        });
        assert!(
            insights
                .events(10)
                .iter()
                .all(|e| e.kind != EventKind::Update)
        );
        // Without a run, every session event shows.
        let mut fresh = Insights::default();
        for i in 0..(SESSION_EVENTS_KEPT as u64 + 5) {
            fresh.record_session_event(RunEvent {
                at: at(T0) + Duration::from_secs(i),
                kind: EventKind::Update,
                text: format!("event {i}"),
                tone: Tone::Neutral,
            });
        }
        let events = fresh.events(100);
        assert_eq!(events.len(), SESSION_EVENTS_KEPT, "bounded");
        assert_eq!(events[0].text, format!("event {}", SESSION_EVENTS_KEPT + 4));
    }

    #[test]
    fn text_helpers() {
        assert_eq!(elapsed_text(Duration::from_millis(80)), "+0.08 s");
        assert_eq!(elapsed_text(Duration::from_millis(1_234)), "+1.2 s");
        assert_eq!(elapsed_text(Duration::from_secs(45)), "+45 s");
        assert_eq!(elapsed_text(Duration::from_secs(190)), "+3 min");
    }
}
