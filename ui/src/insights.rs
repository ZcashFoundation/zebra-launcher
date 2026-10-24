//! Analysis of zebrad's log stream: sync progress, problems, startup milestones and activity
//! counts, folded line by line as batches arrive. Pure Rust, tested natively.
//!
//! [`Insights::ingest`] is called from the app shell for every batch; the Logs page and the
//! dashboard read the accessors. Everything here is plain data: no interior mutability, no
//! clocks (a line without a timestamp takes the time of the timestamped line before it; the
//! ingest `now` stands in only until one has been seen).

pub mod fields;

use std::{
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

/// Where zebrad's progress task logs.
const PROGRESS_TARGET: &str = "zebrad::components::sync::progress";

/// What the log stream so far says about the node.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Insights {
    /// The highest `seq` folded in; lines at or below it are duplicates.
    last_seq: Option<u64>,
    /// The time of the last timestamped line. Continuation lines, stderr lines and launcher
    /// notes carry no timestamp and take it, since they follow the line they belong to.
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
    /// TRACE, DEBUG, continuation and launcher lines.
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

    fn of(line: &LogLine) -> Option<Self> {
        match (line.stream, line.level) {
            (LogStream::Stderr, _) => Some(ProblemLevel::Stderr),
            (LogStream::Stdout, Some(LogLevel::Error)) => Some(ProblemLevel::Error),
            (LogStream::Stdout, Some(LogLevel::Warn)) => Some(ProblemLevel::Warn),
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
    pub first: SystemTime,
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
            (LogStream::Stderr, _) | (LogStream::Stdout, Some(LogLevel::Error)) => {
                bucket.error += 1
            }
            (LogStream::Stdout, Some(LogLevel::Warn)) => bucket.warn += 1,
            (LogStream::Stdout, Some(LogLevel::Info)) => bucket.info += 1,
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
        if let Some(problem) = self.problems.iter_mut().find(|p| p.pattern == pattern) {
            problem.count = problem.count.saturating_add(1);
            problem.last = problem.last.max(at);
            problem.last_seq = line.seq;
            problem.sample = line.message.clone();
            problem.level = level;
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
    }

    fn reach(&mut self, milestone: Milestone, at: SystemTime) {
        if let Some(run) = self.runs.last_mut() {
            run.reach(milestone, at);
        }
    }

    fn read_progress(&mut self, stage: SyncStage, message: &str, at: SystemTime) {
        let height = field(message, "current_height").and_then(height_of);
        self.sync = Some(SyncSnapshot {
            stage,
            percent: field(message, "sync_percent").and_then(percent_of),
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

    fn launcher(seq: u64, message: &str) -> LogLine {
        LogLine {
            stream: LogStream::Launcher,
            ..continuation(seq, message)
        }
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
        lines.push(launcher(n + 3, "zebrad exited with code 0"));
        lines.extend(startup(n + 4, "2026-10-23T18:41:00Z"));
        let m = n + 4 + startup(0, "2026-10-23T18:41:00Z").len() as u64;
        lines.push(stamped(
            m,
            "2026-10-23T18:41:30Z",
            LogLevel::Info,
            "zebrad::signal",
            "stdin closed, starting shutdown",
        ));
        lines.push(launcher(m + 1, "zebrad exited with code 1"));
        insights.ingest(&lines, at("2026-10-23T18:42:00Z"));

        assert_eq!(insights.runs().len(), 2);
        let first = &insights.runs()[0];
        assert_eq!(first.shutdown_reason.as_deref(), Some("SIGTERM"));
        assert_eq!(first.stopped_at, Some(at("2026-10-23T18:40:02Z")));
        let second = insights.current_run().unwrap();
        assert_eq!(second.started_at, at("2026-10-23T18:41:00Z"));
        assert_eq!(second.shutdown_reason.as_deref(), Some("stdin closed"));
        // No `stopping zebrad`: the launcher's exit note gives the stop time, dated by the
        // line before it rather than by the ingest time.
        assert_eq!(second.stopped_at, Some(at("2026-10-23T18:41:30Z")));
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
                launcher(0, "zebrad exited with code 101"),
                stderr(1, "zebrad fatal error: boom"),
            ],
            at(T0),
        );
        assert_eq!(insights.current_run(), None);
        assert_eq!(insights.problems(5).len(), 1);
        assert_eq!(insights.problems(5)[0].level, ProblemLevel::Stderr);
        // With no timestamped line anywhere, the ingest time is all there is.
        assert_eq!(insights.problems(5)[0].first, at(T0));
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
    fn text_helpers() {
        assert_eq!(elapsed_text(Duration::from_millis(80)), "+0.08 s");
        assert_eq!(elapsed_text(Duration::from_millis(1_234)), "+1.2 s");
        assert_eq!(elapsed_text(Duration::from_secs(45)), "+45 s");
        assert_eq!(elapsed_text(Duration::from_secs(190)), "+3 min");
    }
}
