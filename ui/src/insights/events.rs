//! The sentences of the "This run" timeline that do not come from zebrad's milestones: the
//! launcher's own notes in the log stream, the status transitions the UI observes, the
//! updater's steps and the configuration changes; and the merge that puts the two sources
//! (the log and the session) into one list without saying the same thing twice. Pure Rust,
//! tested natively.

use std::{
    cmp::Reverse,
    mem::discriminant,
    time::{Duration, SystemTime},
};

use launcher_types::{ApplyOutcome, ExitClass, ExitInfo, Phase, Status, UpdateInfo, UpdateState};

use crate::format::{exit_class_text, exit_code_words, rough_duration};

use super::{EventKind, Problem, ProblemLevel, RunEvent, Tone, fields::message_text};

/// A session event and a log event of the same kind this close together describe one
/// happening. The supervisor stamps its notes, so the two differ by the exit drain and the
/// status delivery, seconds at most; the window stays well under the shortest restart backoff,
/// so a replayed restart decision is not mistaken for the next crash's.
pub const DEDUPE_WINDOW: Duration = Duration::from_secs(10);

/// Milestones this close together share one row.
pub const COLLAPSE_WINDOW: Duration = Duration::from_secs(2);

/// How many characters of a problem's message the timeline keeps.
const PROBLEM_CHARS: usize = 100;

/// The kinds where the status transition the UI saw says more than the launcher's note: the
/// exit carries the backend's own exit time, the backoff its crash count.
const SESSION_PREFERRED: [EventKind; 2] = [EventKind::Exited, EventKind::Restarting];

fn event(at: SystemTime, kind: EventKind, text: impl Into<String>, tone: Tone) -> RunEvent {
    RunEvent {
        at,
        kind,
        text: text.into(),
        tone,
    }
}

/// What a launcher exit note says: `zebrad exited (exit code 101, Crash, stop requested)`, or
/// the older `zebrad exited with code 0`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExitNote {
    pub code: Option<i32>,
    pub class: Option<ExitClass>,
    pub requested: bool,
}

/// Reads an exit note; `None` for any other text.
pub fn parse_exit_note(message: &str) -> Option<ExitNote> {
    let rest = message.strip_prefix("zebrad exited")?.trim();
    if let Some(inner) = rest.strip_prefix('(').and_then(|r| r.strip_suffix(')')) {
        let mut parts = inner.split(", ");
        let code = match parts.next()? {
            "no exit code" => None,
            code => Some(code.strip_prefix("exit code ")?.trim().parse().ok()?),
        };
        let class = parts.next().and_then(exit_class_named);
        let requested = parts.next() == Some("stop requested");
        return Some(ExitNote {
            code,
            class,
            requested,
        });
    }
    let code = rest.strip_prefix("with code ")?.trim().parse().ok();
    Some(ExitNote {
        code,
        class: None,
        requested: false,
    })
}

/// The exit class the launcher wrote with `{class:?}`.
fn exit_class_named(name: &str) -> Option<ExitClass> {
    Some(match name {
        "Clean" => ExitClass::Clean,
        "ConfigError" => ExitClass::ConfigError,
        "EosHalt" => ExitClass::EosHalt,
        "DbLocked" => ExitClass::DbLocked,
        "PortInUse" => ExitClass::PortInUse,
        "SpawnFailed" => ExitClass::SpawnFailed,
        "Crash" => ExitClass::Crash,
        _ => return None,
    })
}

/// A short name for how a run ended, for `zebrad exited: crash (code 101)`.
pub fn exit_words(class: ExitClass) -> &'static str {
    match class {
        ExitClass::Clean => "clean exit",
        ExitClass::ConfigError => "configuration rejected",
        ExitClass::EosHalt => "end of support reached",
        ExitClass::DbLocked => "state database locked",
        ExitClass::PortInUse => "port in use",
        ExitClass::SpawnFailed => "could not start",
        ExitClass::Crash => "crash",
    }
}

/// `clean exit (code 0)`, `crash (code 101)`, `crash (killed by a signal)`, `exit (code 1)`.
pub fn ending_words(class: Option<ExitClass>, code: Option<i32>) -> String {
    let code = exit_code_words(code, class);
    match class {
        Some(class) => format!("{} ({code})", exit_words(class)),
        None => format!("exit ({code})"),
    }
}

/// A clean exit is neutral, a crash during a requested stop a warning, any other exit critical.
fn exit_tone(class: Option<ExitClass>, code: Option<i32>, requested: bool) -> Tone {
    match class {
        Some(ExitClass::Clean) => Tone::Neutral,
        None if code == Some(0) => Tone::Neutral,
        _ if requested => Tone::Warning,
        _ => Tone::Critical,
    }
}

/// The row for how the last run ended, from the status.
pub fn exit_event(exit: &ExitInfo) -> RunEvent {
    event(
        exit.at,
        EventKind::Exited,
        format!(
            "zebrad exited: {}",
            ending_words(Some(exit.class), exit.code)
        ),
        exit_tone(Some(exit.class), exit.code, exit.requested),
    )
}

/// The one line an earlier run gets: `Run 2 ended: crash (code 101)`, `Run 1 ended: SIGTERM`.
pub fn earlier_run_event(number: usize, reason: Option<&str>, ended_at: SystemTime) -> RunEvent {
    let (text, tone) = match reason {
        Some(reason) => match parse_exit_note(reason) {
            Some(note) => (
                format!(
                    "Run {number} ended: {}",
                    ending_words(note.class, note.code)
                ),
                exit_tone(note.class, note.code, note.requested),
            ),
            None => (format!("Run {number} ended: {reason}"), Tone::Neutral),
        },
        None => (format!("Run {number} ended"), Tone::Neutral),
    };
    event(ended_at, EventKind::EarlierRun, text, tone)
}

/// A `Duration` as the launcher wrote it with `{:?}`: `5s`, `1.5s`, `500ms`, `10µs`, `7ns`.
fn debug_duration(text: &str) -> Option<Duration> {
    let text = text.trim();
    let (number, scale) = if let Some(n) = text.strip_suffix("ms") {
        (n, 1e-3)
    } else if let Some(n) = text.strip_suffix("µs") {
        (n, 1e-6)
    } else if let Some(n) = text.strip_suffix("ns") {
        (n, 1e-9)
    } else {
        (text.strip_suffix('s')?, 1.0)
    };
    let number: f64 = number.parse().ok()?;
    Duration::try_from_secs_f64(number * scale).ok()
}

fn started_text(pid: Option<u32>) -> String {
    match pid {
        Some(pid) => format!("zebrad started (pid {pid})"),
        None => "zebrad started".to_owned(),
    }
}

fn stop_requested_event(graceful: bool, at: SystemTime) -> RunEvent {
    if graceful {
        event(
            at,
            EventKind::StopRequested,
            "Stop requested (graceful)",
            Tone::Neutral,
        )
    } else {
        event(
            at,
            EventKind::StopRequested,
            "Stop requested (hard stop: zebrad gets no shutdown)",
            Tone::Warning,
        )
    }
}

/// The typed event of a launcher note, the lines the supervisor adds to the log stream
/// (`LogStream::Launcher`) for its own decisions. `None` for a note the timeline does not show.
pub fn launcher_event(message: &str, at: SystemTime) -> Option<RunEvent> {
    let message = message.trim();
    if let Some(rest) = message.strip_prefix("zebrad started") {
        let pid = rest
            .trim()
            .strip_prefix("(pid ")
            .and_then(|r| r.strip_suffix(')'))
            .and_then(|digits| digits.trim().parse().ok());
        return Some(event(
            at,
            EventKind::Started,
            started_text(pid),
            Tone::Neutral,
        ));
    }
    if let Some(note) = parse_exit_note(message) {
        return Some(event(
            at,
            EventKind::Exited,
            format!("zebrad exited: {}", ending_words(note.class, note.code)),
            exit_tone(note.class, note.code, note.requested),
        ));
    }
    if let Some(rest) = message.strip_prefix("stop requested") {
        let rest = rest.trim();
        if rest.starts_with("after zebrad exited") {
            return Some(event(
                at,
                EventKind::StopRequested,
                "Stop requested; zebrad is not restarted",
                Tone::Neutral,
            ));
        }
        let graceful = !rest.starts_with("(terminated");
        return Some(stop_requested_event(graceful, at));
    }
    if let Some(rest) = message.strip_prefix("restarting zebrad in ") {
        let delay = debug_duration(rest).unwrap_or_default();
        return Some(event(
            at,
            EventKind::Restarting,
            format!("Restarting in {}", rough_duration(delay)),
            Tone::Warning,
        ));
    }
    if let Some(rest) = message.strip_prefix("not restarting zebrad") {
        let class = rest
            .trim()
            .strip_prefix('(')
            .and_then(|r| r.strip_suffix(')'))
            .and_then(exit_class_named);
        let why = class.map_or("zebrad exited", exit_class_text);
        return Some(event(
            at,
            EventKind::Problem,
            format!("Not restarting: {why}"),
            Tone::Critical,
        ));
    }
    if message == "scheduled restart cancelled" {
        return Some(event(
            at,
            EventKind::Restarting,
            "Scheduled restart cancelled",
            Tone::Neutral,
        ));
    }
    if let Some(rest) = message.strip_prefix("terminated a zebrad left behind by an earlier run") {
        return Some(event(
            at,
            EventKind::Started,
            format!("Stopped an earlier zebrad {}", rest.trim())
                .trim_end()
                .to_owned(),
            Tone::Neutral,
        ));
    }
    if let Some(error) = message.strip_prefix("could not start zebrad:") {
        return Some(event(
            at,
            EventKind::Problem,
            format!("zebrad could not be started: {}", error.trim()),
            Tone::Critical,
        ));
    }
    if let Some(rest) = message.strip_prefix("zebrad did not stop within ") {
        let grace = rest
            .split_once(';')
            .and_then(|(grace, _)| debug_duration(grace))
            .unwrap_or_default();
        return Some(event(
            at,
            EventKind::Problem,
            format!(
                "zebrad did not stop within {} and was killed",
                rough_duration(grace)
            ),
            Tone::Warning,
        ));
    }
    if let Some(error) = message.strip_prefix("could not kill zebrad:") {
        return Some(event(
            at,
            EventKind::Problem,
            format!("zebrad could not be killed: {}", error.trim()),
            Tone::Critical,
        ));
    }
    if message.starts_with("could not ask zebrad to stop") {
        return Some(event(
            at,
            EventKind::StopRequested,
            "Stop requested; zebrad could not be asked to stop and was killed",
            Tone::Warning,
        ));
    }
    None
}

/// The events of a status change the UI observed: a new phase, a new last exit, a stale zebrad
/// found. `now` dates them, except the exit, which the backend dated itself.
pub fn status_events(previous: &Status, next: &Status, now: SystemTime) -> Vec<RunEvent> {
    let mut events = Vec::new();
    if let Some(exit) = &next.last_exit
        && previous.last_exit.as_ref() != Some(exit)
    {
        events.push(exit_event(exit));
    }
    if discriminant(&previous.phase) != discriminant(&next.phase) {
        match &next.phase {
            Phase::Starting => events.push(event(
                now,
                EventKind::Started,
                started_text(next.pid),
                Tone::Neutral,
            )),
            Phase::Running => {}
            Phase::Stopping => events.push(stop_requested_event(next.stop_is_graceful, now)),
            Phase::Stopped => {
                if matches!(previous.phase, Phase::Backoff { .. }) {
                    events.push(event(
                        now,
                        EventKind::Restarting,
                        "Scheduled restart cancelled",
                        Tone::Neutral,
                    ));
                }
            }
            Phase::Backoff { until, crashes } => {
                let delay = until.duration_since(now).unwrap_or_default();
                events.push(event(
                    now,
                    EventKind::Restarting,
                    format!("Restarting in {} (crash {crashes})", rough_duration(delay)),
                    Tone::Warning,
                ));
            }
            Phase::Failed { class } => events.push(event(
                now,
                EventKind::Problem,
                format!("Not restarting: {}", exit_class_text(*class)),
                Tone::Critical,
            )),
        }
    }
    if let Some(stale) = &next.stale
        && previous.stale.is_none()
    {
        events.push(event(
            now,
            EventKind::Problem,
            format!("An earlier zebrad is still running (pid {})", stale.pid),
            Tone::Warning,
        ));
    }
    events
}

/// The events of a change in what the updater reports: a step of the updater (found,
/// downloading, ready, installing, failed) or a new launcher version running. The first info
/// after loading describes a state, not a change, so `previous` `None` gives nothing.
pub fn update_events(
    previous: Option<&UpdateInfo>,
    next: &UpdateInfo,
    now: SystemTime,
) -> Vec<RunEvent> {
    let Some(previous) = previous else {
        return Vec::new();
    };
    let mut events = Vec::new();
    if previous.current_version != next.current_version {
        events.push(event(
            now,
            EventKind::Update,
            format!("Launcher updated to {}", next.current_version),
            Tone::Good,
        ));
    }
    if discriminant(&previous.state) == discriminant(&next.state) {
        return events;
    }
    match &next.state {
        UpdateState::Idle | UpdateState::Checking | UpdateState::UpToDate { .. } => {}
        UpdateState::Available { version, .. } => events.push(event(
            now,
            EventKind::Update,
            format!("Launcher update available: {version}"),
            Tone::Neutral,
        )),
        UpdateState::Downloading { version, .. } => events.push(event(
            now,
            EventKind::Update,
            format!("Downloading launcher {version}"),
            Tone::Neutral,
        )),
        UpdateState::ReadyToInstall { version, .. } => events.push(event(
            now,
            EventKind::Update,
            format!("Launcher {version} ready to install"),
            Tone::Neutral,
        )),
        UpdateState::Installing { version } => events.push(event(
            now,
            EventKind::Update,
            format!("Installing launcher {version}"),
            Tone::Neutral,
        )),
        UpdateState::Error { message, at } => events.push(event(
            *at,
            EventKind::Update,
            format!("Launcher update failed: {message}"),
            Tone::Warning,
        )),
    }
    events
}

/// `zebrad.toml` was replaced outside the Configure page's own save (a reset).
pub fn config_replaced_event(now: SystemTime) -> RunEvent {
    event(
        now,
        EventKind::ConfigApplied,
        "Configuration replaced",
        Tone::Neutral,
    )
}

/// The outcome of a save on the Configure page.
pub fn config_outcome_event(outcome: &ApplyOutcome, now: SystemTime) -> RunEvent {
    match outcome {
        ApplyOutcome::Applied => event(
            now,
            EventKind::ConfigApplied,
            "Configuration applied",
            Tone::Neutral,
        ),
        ApplyOutcome::RolledBack { .. } => event(
            now,
            EventKind::ConfigRolledBack,
            "Configuration rolled back: zebrad rejected it",
            Tone::Warning,
        ),
    }
}

/// A problem's first appearance in the current run as a timeline row:
/// `ERROR: block download failed ×3`.
pub fn problem_event(problem: &Problem) -> RunEvent {
    let text = message_text(&problem.sample);
    let text = if text.is_empty() {
        problem.sample.as_str()
    } else {
        text
    };
    // Only a cut message gets the ellipsis; the fields left off are not a cut.
    let truncated = text.chars().count() > PROBLEM_CHARS;
    let mut text: String = text.chars().take(PROBLEM_CHARS).collect();
    if truncated {
        text.push('…');
    }
    let mut text = format!("{}: {}", problem.level.label(), text.trim());
    if problem.count > 1 {
        text.push_str(&format!(" ×{}", problem.count));
    }
    let tone = match problem.level {
        ProblemLevel::Warn => Tone::Warning,
        ProblemLevel::Error | ProblemLevel::Stderr => Tone::Critical,
    };
    event(problem.run_first, EventKind::Problem, text, tone)
}

/// `Metrics opened` as `metrics opened` for the middle of a row; an acronym such as `RPC
/// opened` keeps its case.
pub fn lower_first(text: &str) -> String {
    let first_word = text.split(' ').next().unwrap_or(text);
    let acronym = first_word.chars().skip(1).any(char::is_uppercase);
    let mut chars = text.chars();
    match chars.next() {
        Some(first) if !acronym => first.to_lowercase().chain(chars).collect(),
        _ => text.to_owned(),
    }
}

/// Folds milestones within [`COLLAPSE_WINDOW`] of a row's first into that row:
/// `Listening for peers · RPC opened · metrics opened`. `events` is oldest first; other kinds
/// pass through and end a group.
pub fn collapse_milestones(events: Vec<RunEvent>) -> Vec<RunEvent> {
    let mut out: Vec<RunEvent> = Vec::with_capacity(events.len());
    for event in events {
        if event.kind == EventKind::Milestone
            && let Some(last) = out.last_mut()
            && last.kind == EventKind::Milestone
            && event
                .at
                .duration_since(last.at)
                .is_ok_and(|gap| gap <= COLLAPSE_WINDOW)
        {
            last.text.push_str(" · ");
            last.text.push_str(&lower_first(&event.text));
            continue;
        }
        out.push(event);
    }
    out
}

/// Whether two events of the same kind this close together are one happening. Problems match
/// by text too, since several different ones can be close together.
fn same_happening(a: &RunEvent, b: &RunEvent) -> bool {
    if a.kind != b.kind {
        return false;
    }
    let gap = match a.at.duration_since(b.at) {
        Ok(gap) => gap,
        Err(behind) => behind.duration(),
    };
    gap <= DEDUPE_WINDOW && (a.kind != EventKind::Problem || a.text == b.text)
}

/// The timeline: the log's events and the session's, newest first, at most `limit`. A session
/// event that repeats a log event is dropped, except for the kinds in [`SESSION_PREFERRED`],
/// where the log's copy goes instead.
pub fn merge_events(log: Vec<RunEvent>, session: &[RunEvent], limit: usize) -> Vec<RunEvent> {
    let mut all: Vec<RunEvent> = log
        .iter()
        .filter(|event| {
            !(SESSION_PREFERRED.contains(&event.kind)
                && session.iter().any(|other| same_happening(event, other)))
        })
        .cloned()
        .collect();
    all.extend(
        session
            .iter()
            .filter(|event| {
                SESSION_PREFERRED.contains(&event.kind)
                    || !log.iter().any(|other| same_happening(event, other))
            })
            .cloned(),
    );
    // Newest first; events sharing a stamp (a start note and the startup lines, an exit note
    // and the restart decision) go in the order they happen in a run.
    all.sort_by_key(|event| Reverse((event.at, sequence_rank(event.kind))));
    all.truncate(limit);
    all
}

/// Where a kind falls in a run when several events share one instant: an earlier run's line,
/// then the exit and the restart decision, the start and its milestones, and the rest.
fn sequence_rank(kind: EventKind) -> u8 {
    match kind {
        EventKind::EarlierRun => 0,
        EventKind::Exited => 1,
        EventKind::Restarting => 2,
        EventKind::Started => 3,
        EventKind::Milestone => 4,
        EventKind::ConfigApplied | EventKind::ConfigRolledBack => 5,
        EventKind::Update => 6,
        EventKind::StopRequested => 7,
        EventKind::Problem => 8,
    }
}

#[cfg(test)]
mod tests {
    use std::time::UNIX_EPOCH;

    use launcher_types::{StaleProcessInfo, UpdateMode};

    use super::*;

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn ev(secs: u64, kind: EventKind, text: &str) -> RunEvent {
        event(at(secs), kind, text, Tone::Neutral)
    }

    #[test]
    fn exit_notes_parse_in_both_shapes() {
        assert_eq!(
            parse_exit_note("zebrad exited (exit code 101, Crash)"),
            Some(ExitNote {
                code: Some(101),
                class: Some(ExitClass::Crash),
                requested: false
            })
        );
        assert_eq!(
            parse_exit_note("zebrad exited (exit code 0, Clean, stop requested)"),
            Some(ExitNote {
                code: Some(0),
                class: Some(ExitClass::Clean),
                requested: true
            })
        );
        assert_eq!(
            parse_exit_note("zebrad exited (no exit code, Crash)"),
            Some(ExitNote {
                code: None,
                class: Some(ExitClass::Crash),
                requested: false
            })
        );
        assert_eq!(
            parse_exit_note("zebrad exited with code 1"),
            Some(ExitNote {
                code: Some(1),
                class: None,
                requested: false
            })
        );
        assert_eq!(parse_exit_note("zebrad exited (exit code x, Crash)"), None);
        assert_eq!(parse_exit_note("zebrad started (pid 1)"), None);
        for class in [
            ExitClass::Clean,
            ExitClass::ConfigError,
            ExitClass::EosHalt,
            ExitClass::DbLocked,
            ExitClass::PortInUse,
            ExitClass::SpawnFailed,
            ExitClass::Crash,
        ] {
            assert_eq!(exit_class_named(&format!("{class:?}")), Some(class));
            assert!(!exit_words(class).is_empty());
        }
        assert_eq!(exit_class_named("Weird"), None);
    }

    #[test]
    fn launcher_notes_become_typed_events() {
        let now = at(100);
        let cases: [(&str, EventKind, &str, Tone); 15] = [
            (
                "zebrad started (pid 4242)",
                EventKind::Started,
                "zebrad started (pid 4242)",
                Tone::Neutral,
            ),
            (
                "stop requested (stdin closed)",
                EventKind::StopRequested,
                "Stop requested (graceful)",
                Tone::Neutral,
            ),
            (
                "stop requested (SIGTERM)",
                EventKind::StopRequested,
                "Stop requested (graceful)",
                Tone::Neutral,
            ),
            (
                "stop requested (terminated: this zebrad cannot stop gracefully on this platform)",
                EventKind::StopRequested,
                "Stop requested (hard stop: zebrad gets no shutdown)",
                Tone::Warning,
            ),
            (
                "stop requested after zebrad exited; it will not be restarted",
                EventKind::StopRequested,
                "Stop requested; zebrad is not restarted",
                Tone::Neutral,
            ),
            (
                "could not ask zebrad to stop (broken pipe); killed it",
                EventKind::StopRequested,
                "Stop requested; zebrad could not be asked to stop and was killed",
                Tone::Warning,
            ),
            (
                "zebrad exited (exit code 0, Clean, stop requested)",
                EventKind::Exited,
                "zebrad exited: clean exit (code 0)",
                Tone::Neutral,
            ),
            (
                "zebrad exited (exit code 101, Crash)",
                EventKind::Exited,
                "zebrad exited: crash (code 101)",
                Tone::Critical,
            ),
            (
                "zebrad exited (no exit code, Crash, stop requested)",
                EventKind::Exited,
                "zebrad exited: crash (killed by a signal)",
                Tone::Warning,
            ),
            (
                "zebrad exited with code 0",
                EventKind::Exited,
                "zebrad exited: exit (code 0)",
                Tone::Neutral,
            ),
            (
                "restarting zebrad in 5s",
                EventKind::Restarting,
                "Restarting in 5 s",
                Tone::Warning,
            ),
            (
                "not restarting zebrad (ConfigError)",
                EventKind::Problem,
                "Not restarting: zebrad rejected its configuration",
                Tone::Critical,
            ),
            (
                "scheduled restart cancelled",
                EventKind::Restarting,
                "Scheduled restart cancelled",
                Tone::Neutral,
            ),
            (
                "terminated a zebrad left behind by an earlier run (pid 1234)",
                EventKind::Started,
                "Stopped an earlier zebrad (pid 1234)",
                Tone::Neutral,
            ),
            (
                "could not start zebrad: No such file or directory (os error 2)",
                EventKind::Problem,
                "zebrad could not be started: No such file or directory (os error 2)",
                Tone::Critical,
            ),
        ];
        for (message, kind, text, tone) in cases {
            let event = launcher_event(message, now).unwrap_or_else(|| panic!("{message}"));
            assert_eq!(event.kind, kind, "{message}");
            assert_eq!(event.text, text, "{message}");
            assert_eq!(event.tone, tone, "{message}");
            assert_eq!(event.at, now);
        }
        let killed = launcher_event("zebrad did not stop within 60s; killed it", now).unwrap();
        assert_eq!(killed.kind, EventKind::Problem);
        assert_eq!(
            killed.text,
            "zebrad did not stop within 1 min and was killed"
        );
        assert_eq!(killed.tone, Tone::Warning);
        let unkillable = launcher_event("could not kill zebrad: permission denied", now).unwrap();
        assert_eq!(unkillable.kind, EventKind::Problem);
        assert_eq!(
            unkillable.text,
            "zebrad could not be killed: permission denied"
        );
        assert_eq!(unkillable.tone, Tone::Critical);
        // Notes the timeline does not show, and a start without a pid.
        assert_eq!(launcher_event("7 lines dropped", now), None);
        assert_eq!(launcher_event("", now), None);
        assert_eq!(
            launcher_event("zebrad started", now).unwrap().text,
            "zebrad started"
        );
        assert_eq!(
            launcher_event("restarting zebrad in 1.5s", now)
                .unwrap()
                .text,
            "Restarting in 1 s"
        );
    }

    #[test]
    fn debug_durations() {
        assert_eq!(debug_duration("5s"), Some(Duration::from_secs(5)));
        assert_eq!(debug_duration("1.5s"), Some(Duration::from_millis(1500)));
        assert_eq!(debug_duration("500ms"), Some(Duration::from_millis(500)));
        assert_eq!(debug_duration("10µs"), Some(Duration::from_micros(10)));
        assert_eq!(debug_duration("7ns"), Some(Duration::from_nanos(7)));
        assert_eq!(debug_duration("soon"), None);
        assert_eq!(debug_duration("-1s"), None);
    }

    fn running(pid: u32) -> Status {
        Status {
            phase: Phase::Running,
            pid: Some(pid),
            started_at: Some(at(10)),
            rpc_addr: "127.0.0.1:8232".parse().ok(),
            metrics_addr: None,
            lightwalletd_addr: None,
            last_exit: None,
            stale: None,
            stop_is_graceful: true,
            restarts: 0,
            last_stop: None,
        }
    }

    fn crashed(at_secs: u64) -> ExitInfo {
        ExitInfo {
            code: Some(101),
            class: ExitClass::Crash,
            detail: Some("thread 'main' panicked".into()),
            at: at(at_secs),
            requested: false,
        }
    }

    #[test]
    fn status_transitions_become_events() {
        let now = at(100);
        let stopped = Status::default();
        // Loading: nothing to say for the same phase and no exit.
        assert!(status_events(&stopped, &stopped, now).is_empty());
        // Stopped to Starting.
        let starting = Status {
            phase: Phase::Starting,
            pid: Some(7),
            started_at: Some(now),
            ..Status::default()
        };
        let events = status_events(&stopped, &starting, now);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, EventKind::Started);
        assert_eq!(events[0].text, "zebrad started (pid 7)");
        assert_eq!(events[0].at, now);
        // Starting to Running: the milestones tell that story.
        assert!(status_events(&starting, &running(7), now).is_empty());
        // Running to Stopping, graceful and hard.
        let stopping = Status {
            phase: Phase::Stopping,
            ..running(7)
        };
        let events = status_events(&running(7), &stopping, now);
        assert_eq!(events[0].text, "Stop requested (graceful)");
        assert_eq!(events[0].tone, Tone::Neutral);
        let hard = Status {
            stop_is_graceful: false,
            ..stopping.clone()
        };
        let events = status_events(&running(7), &hard, now);
        assert_eq!(
            events[0].text,
            "Stop requested (hard stop: zebrad gets no shutdown)"
        );
        assert_eq!(events[0].tone, Tone::Warning);
        // A crash: the exit (dated by the backend) and the backoff (dated now).
        let backoff = Status {
            phase: Phase::Backoff {
                until: at(125),
                crashes: 2,
            },
            pid: None,
            started_at: None,
            last_exit: Some(crashed(99)),
            ..Status::default()
        };
        let events = status_events(&running(7), &backoff, now);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, EventKind::Exited);
        assert_eq!(events[0].text, "zebrad exited: crash (code 101)");
        assert_eq!(events[0].tone, Tone::Critical);
        assert_eq!(events[0].at, at(99));
        assert_eq!(events[1].kind, EventKind::Restarting);
        assert_eq!(events[1].text, "Restarting in 25 s (crash 2)");
        assert_eq!(events[1].tone, Tone::Warning);
        // The same exit again, with the backoff ticking, is nothing new.
        let later = Status {
            phase: Phase::Backoff {
                until: at(125),
                crashes: 2,
            },
            ..backoff.clone()
        };
        assert!(status_events(&backoff, &later, at(101)).is_empty());
        // Backoff cancelled.
        let cancelled = Status {
            phase: Phase::Stopped,
            ..backoff.clone()
        };
        let events = status_events(&backoff, &cancelled, now);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].text, "Scheduled restart cancelled");
        // Giving up.
        let failed = Status {
            phase: Phase::Failed {
                class: ExitClass::ConfigError,
            },
            last_exit: Some(ExitInfo {
                code: Some(1),
                class: ExitClass::ConfigError,
                detail: None,
                at: at(99),
                requested: false,
            }),
            ..Status::default()
        };
        let events = status_events(&running(7), &failed, now);
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0].text,
            "zebrad exited: configuration rejected (code 1)"
        );
        assert_eq!(events[1].kind, EventKind::Problem);
        assert_eq!(
            events[1].text,
            "Not restarting: zebrad rejected its configuration"
        );
        assert_eq!(events[1].tone, Tone::Critical);
        // A clean, requested stop.
        let clean = Status {
            last_exit: Some(ExitInfo {
                code: Some(0),
                class: ExitClass::Clean,
                detail: None,
                at: at(99),
                requested: true,
            }),
            ..Status::default()
        };
        let events = status_events(&stopping, &clean, now);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].text, "zebrad exited: clean exit (code 0)");
        assert_eq!(events[0].tone, Tone::Neutral);
        // A stale zebrad found; its removal says nothing.
        let stale = Status {
            stale: Some(StaleProcessInfo {
                pid: 1234,
                binary: "/opt/zebrad".into(),
            }),
            ..Status::default()
        };
        let events = status_events(&stopped, &stale, now);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, EventKind::Problem);
        assert_eq!(
            events[0].text,
            "An earlier zebrad is still running (pid 1234)"
        );
        assert_eq!(events[0].tone, Tone::Warning);
        assert!(status_events(&stale, &stopped, now).is_empty());
    }

    fn info(state: UpdateState, version: &str) -> UpdateInfo {
        UpdateInfo {
            state,
            mode: UpdateMode::Notify,
            current_version: version.into(),
            last_successful_check: None,
            stale_check_warning: false,
            can_install: true,
            releases_url: String::new(),
            health_failure: None,
        }
    }

    #[test]
    fn updater_steps_become_events() {
        let now = at(100);
        let idle = info(UpdateState::Idle, "0.1.0");
        let available = info(
            UpdateState::Available {
                version: "0.2.0".into(),
                notes: None,
                pub_date: None,
            },
            "0.1.0",
        );
        // The first info is a state, not a change.
        assert!(update_events(None, &available, now).is_empty());
        assert!(update_events(Some(&idle), &info(UpdateState::Checking, "0.1.0"), now).is_empty());
        let events = update_events(Some(&idle), &available, now);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, EventKind::Update);
        assert_eq!(events[0].text, "Launcher update available: 0.2.0");
        let downloading = |downloaded| {
            info(
                UpdateState::Downloading {
                    version: "0.2.0".into(),
                    downloaded,
                    total: Some(100),
                },
                "0.1.0",
            )
        };
        let events = update_events(Some(&available), &downloading(0), now);
        assert_eq!(events[0].text, "Downloading launcher 0.2.0");
        // Progress keeps the state: no event per chunk.
        assert!(update_events(Some(&downloading(0)), &downloading(50), now).is_empty());
        let ready = |countdown| {
            info(
                UpdateState::ReadyToInstall {
                    version: "0.2.0".into(),
                    countdown_secs: countdown,
                },
                "0.1.0",
            )
        };
        let events = update_events(Some(&downloading(100)), &ready(Some(60)), now);
        assert_eq!(events[0].text, "Launcher 0.2.0 ready to install");
        assert!(update_events(Some(&ready(Some(60))), &ready(Some(59)), now).is_empty());
        let installing = info(
            UpdateState::Installing {
                version: "0.2.0".into(),
            },
            "0.1.0",
        );
        let events = update_events(Some(&ready(None)), &installing, now);
        assert_eq!(events[0].text, "Installing launcher 0.2.0");
        let failed = info(
            UpdateState::Error {
                message: "could not fetch latest.json".into(),
                at: at(95),
            },
            "0.1.0",
        );
        let events = update_events(Some(&info(UpdateState::Checking, "0.1.0")), &failed, now);
        assert_eq!(
            events[0].text,
            "Launcher update failed: could not fetch latest.json"
        );
        assert_eq!(events[0].tone, Tone::Warning);
        assert_eq!(events[0].at, at(95));
        // The new version running: the update landed.
        let events = update_events(Some(&installing), &info(UpdateState::Idle, "0.2.0"), now);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].text, "Launcher updated to 0.2.0");
        assert_eq!(events[0].tone, Tone::Good);
        assert!(
            update_events(
                Some(&idle),
                &info(UpdateState::UpToDate { checked_at: now }, "0.1.0"),
                now
            )
            .is_empty()
        );
    }

    #[test]
    fn config_and_problem_events() {
        let now = at(100);
        let replaced = config_replaced_event(now);
        assert_eq!(replaced.kind, EventKind::ConfigApplied);
        assert_eq!(replaced.text, "Configuration replaced");
        let applied = config_outcome_event(&ApplyOutcome::Applied, now);
        assert_eq!(applied.kind, EventKind::ConfigApplied);
        assert_eq!(applied.text, "Configuration applied");
        let rolled_back = config_outcome_event(
            &ApplyOutcome::RolledBack {
                error: "bad key".into(),
            },
            now,
        );
        assert_eq!(rolled_back.kind, EventKind::ConfigRolledBack);
        assert_eq!(
            rolled_back.text,
            "Configuration rolled back: zebrad rejected it"
        );
        assert_eq!(rolled_back.tone, Tone::Warning);

        let problem = Problem {
            level: ProblemLevel::Error,
            target: Some("zebrad::components::sync".into()),
            pattern: "block download failed hash=_".into(),
            sample: "block download failed hash=block::Hash(\"abc\")".into(),
            count: 3,
            first: at(20),
            run_first: at(50),
            last: at(90),
            last_seq: 9,
        };
        let event = problem_event(&problem);
        assert_eq!(event.kind, EventKind::Problem);
        assert_eq!(event.text, "ERROR: block download failed ×3");
        assert_eq!(event.tone, Tone::Critical);
        assert_eq!(
            event.at,
            at(50),
            "dated at the first appearance in this run"
        );
        // A short message with long fields is whole: no ellipsis for the fields left off.
        let long_fields = Problem {
            sample: format!(
                "block download failed hash=block::Hash(\"{}\") height=Height(2950000) error=timeout",
                "0".repeat(64)
            ),
            ..problem.clone()
        };
        assert_eq!(
            problem_event(&long_fields).text,
            "ERROR: block download failed ×3"
        );
        let stderr = Problem {
            level: ProblemLevel::Stderr,
            sample: "thread 'main' panicked at x.rs:1:1:".into(),
            count: 1,
            ..problem.clone()
        };
        assert_eq!(
            problem_event(&stderr).text,
            "stderr: thread 'main' panicked at x.rs:1:1:"
        );
        let warn = Problem {
            level: ProblemLevel::Warn,
            sample: format!("{} tail=1", "w".repeat(150)),
            count: 1,
            ..problem
        };
        let event = problem_event(&warn);
        assert_eq!(event.tone, Tone::Warning);
        assert!(event.text.starts_with("WARN: www"));
        assert!(event.text.ends_with('…'), "{}", event.text);
        assert_eq!(
            event.text.chars().count(),
            "WARN: ".len() + PROBLEM_CHARS + 1
        );
    }

    #[test]
    fn earlier_runs_get_one_line() {
        let event = earlier_run_event(2, Some("zebrad exited (exit code 101, Crash)"), at(5));
        assert_eq!(event.kind, EventKind::EarlierRun);
        assert_eq!(event.text, "Run 2 ended: crash (code 101)");
        assert_eq!(event.tone, Tone::Critical);
        assert_eq!(event.at, at(5));
        let event = earlier_run_event(1, Some("SIGTERM"), at(5));
        assert_eq!(event.text, "Run 1 ended: SIGTERM");
        assert_eq!(event.tone, Tone::Neutral);
        assert_eq!(earlier_run_event(3, None, at(5)).text, "Run 3 ended");
        assert_eq!(
            earlier_run_event(
                4,
                Some("zebrad exited (exit code 0, Clean, stop requested)"),
                at(5)
            )
            .text,
            "Run 4 ended: clean exit (code 0)"
        );
    }

    #[test]
    fn milestones_collapse_within_two_seconds() {
        assert_eq!(lower_first("Metrics opened"), "metrics opened");
        assert_eq!(lower_first("RPC opened"), "RPC opened");
        assert_eq!(lower_first("zebrad started"), "zebrad started");
        assert_eq!(lower_first(""), "");
        let events = vec![
            ev(0, EventKind::Started, "zebrad started (pid 1)"),
            ev(1, EventKind::Milestone, "State database opened"),
            ev(4, EventKind::Milestone, "Listening for peers"),
            ev(5, EventKind::Milestone, "RPC opened"),
            ev(6, EventKind::Milestone, "Metrics opened"),
            ev(
                7,
                EventKind::Milestone,
                "Initial peers connected (12 of 15)",
            ),
            ev(8, EventKind::Problem, "ERROR: x"),
            ev(8, EventKind::Milestone, "Mempool active"),
        ];
        let rows: Vec<String> = collapse_milestones(events)
            .into_iter()
            .map(|e| {
                format!(
                    "{} {}",
                    e.at.duration_since(UNIX_EPOCH).unwrap().as_secs(),
                    e.text
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                "0 zebrad started (pid 1)",
                "1 State database opened",
                // 6 is two seconds after 4; 7 is not.
                "4 Listening for peers · RPC opened · metrics opened",
                "7 Initial peers connected (12 of 15)",
                "8 ERROR: x",
                "8 Mempool active",
            ]
        );
    }

    #[test]
    fn merge_drops_repeats_and_prefers_the_right_source() {
        let log = vec![
            ev(10, EventKind::Started, "zebrad started (pid 1)"),
            ev(11, EventKind::Milestone, "State database opened"),
            ev(300, EventKind::Exited, "zebrad exited: crash (code 101)"),
            ev(300, EventKind::Restarting, "Restarting in 5 s"),
            ev(300, EventKind::Problem, "Not restarting: zebrad crashed"),
        ];
        let session = vec![
            // The UI saw the start a moment later: the same happening.
            ev(12, EventKind::Started, "zebrad started (pid 1)"),
            // The backend's exit time and the crash count: preferred over the log's copies.
            ev(305, EventKind::Exited, "zebrad exited: crash (code 101)"),
            ev(306, EventKind::Restarting, "Restarting in 5 s (crash 2)"),
            // A problem with other words is another problem.
            ev(
                306,
                EventKind::Problem,
                "An earlier zebrad is still running (pid 9)",
            ),
            // The same problem in the same words is not.
            ev(306, EventKind::Problem, "Not restarting: zebrad crashed"),
            // Far enough apart to be a different stop.
            ev(1000, EventKind::StopRequested, "Stop requested (graceful)"),
        ];
        let merged = merge_events(log, &session, 10);
        let texts: Vec<&str> = merged.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "Stop requested (graceful)",
                // The same second: a problem is listed after a restart decision.
                "An earlier zebrad is still running (pid 9)",
                "Restarting in 5 s (crash 2)",
                "zebrad exited: crash (code 101)",
                "Not restarting: zebrad crashed",
                "State database opened",
                "zebrad started (pid 1)",
            ]
        );
        assert_eq!(merged[3].at, at(305), "the session's exit time");
        assert_eq!(merged[6].at, at(10), "the log's start time");
        // The limit keeps the newest.
        let two = merge_events(Vec::new(), &session, 2);
        assert_eq!(two.len(), 2);
        assert_eq!(two[0].at, at(1000));
        // Session events alone and log events alone pass through.
        assert_eq!(merge_events(Vec::new(), &session, 10).len(), 6);
        let only_log = vec![ev(1, EventKind::Exited, "zebrad exited: crash (code 101)")];
        assert_eq!(merge_events(only_log, &[], 10).len(), 1);
        // A restart decision only the log knows (a replayed history) is not the next crash's
        // restart: the window is shorter than the shortest backoff.
        let replayed = vec![ev(300, EventKind::Restarting, "Restarting in 10 s")];
        let next = vec![ev(
            325,
            EventKind::Restarting,
            "Restarting in 20 s (crash 3)",
        )];
        assert_eq!(merge_events(replayed, &next, 10).len(), 2);
    }
}
