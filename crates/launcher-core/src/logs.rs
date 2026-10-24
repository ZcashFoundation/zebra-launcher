//! Parsing and classification of zebrad output.
//!
//! zebrad logs with `tracing-subscriber`'s default `Full` format, without colour
//! when stdout is not a terminal:
//!
//! ```text
//! 2026-10-23T18:35:20.128340Z  INFO sig{kind=SignalKind(15) name="SIGTERM"}: zebrad::signal: received SIGTERM, starting shutdown
//! ```
//!
//! That is an RFC 3339 timestamp, the level right-aligned to five characters,
//! zero or more spans each followed by `: `, the target followed by `: `, and
//! the message with its fields. A multi-line message continues on lines with no
//! timestamp. Panics and fatal errors go to stderr in Rust's and abscissa's
//! formats, where a panic's message is on the line after `thread '...' panicked at`.
//!
//! The exact wording matched here is pinned by the fixtures in `tests/fixtures`;
//! re-capture them when the bundled zebrad changes.

use std::net::SocketAddr;

use launcher_types::{ExitClass, LogEvent, LogLevel, LogLine, LogStream};

/// Parses one line of zebrad output.
///
/// Never fails: a line that does not start with a timestamp and level keeps its
/// whole text as the message, with no level or target.
pub fn parse_line(seq: u64, stream: LogStream, raw: &str) -> LogLine {
    let raw = raw.trim_end_matches(['\r', '\n']);
    let unparsed = || LogLine {
        seq,
        stream,
        timestamp: None,
        level: None,
        spans: None,
        target: None,
        message: raw.to_string(),
    };

    let Some((timestamp, rest)) = raw.split_once(' ') else {
        return unparsed();
    };
    if !is_timestamp(timestamp) {
        return unparsed();
    }
    let rest = rest.trim_start_matches(' ');
    let Some((level, rest)) = rest.split_once(' ') else {
        return unparsed();
    };
    let Ok(level) = level.parse::<LogLevel>() else {
        return unparsed();
    };

    let (spans, target, message) = split_prefix(rest);
    LogLine {
        seq,
        stream,
        timestamp: Some(timestamp.to_string()),
        level: Some(level),
        spans,
        target,
        message: message.to_string(),
    }
}

/// Returns true for `YYYY-MM-DDTHH:MM:SS[.fraction]Z`.
fn is_timestamp(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() < 20 || b[b.len() - 1] != b'Z' {
        return false;
    }
    let digits = |range: std::ops::Range<usize>| b[range].iter().all(u8::is_ascii_digit);
    let fixed = digits(0..4)
        && b[4] == b'-'
        && digits(5..7)
        && b[7] == b'-'
        && digits(8..10)
        && b[10] == b'T'
        && digits(11..13)
        && b[13] == b':'
        && digits(14..16)
        && b[16] == b':'
        && digits(17..19);
    if !fixed {
        return false;
    }
    match &b[19..b.len() - 1] {
        [] => true,
        [b'.', fraction @ ..] => !fraction.is_empty() && fraction.iter().all(u8::is_ascii_digit),
        _ => false,
    }
}

/// Splits `[span: ]*target: message` into its parts.
///
/// Separators are `: ` outside braces and quotes, so span fields such as
/// `init{config=Config { checkpoint_sync: true }}` stay intact. A segment ending
/// in `}` is a span. A bare identifier followed by another segment that looks
/// like a target is a span without fields. The first remaining path-like
/// segment is the target; everything after it is the message. If no target is
/// found the whole input is the message.
fn split_prefix(rest: &str) -> (Option<String>, Option<String>, &str) {
    let mut spans: Vec<&str> = Vec::new();
    let mut segment_start = 0;
    let mut cursor = 0;

    while let Some((end, next)) = next_separator(rest, cursor) {
        let segment = &rest[segment_start..end];
        // Spans without fields print as bare names, nested ones joined by single
        // colons (`sync:try_to_sync`). Treat such a segment as a span when a target
        // with a module path follows it.
        let followed_by_target = next_separator(rest, next)
            .is_some_and(|(e, _)| rest[next..e].contains("::") && is_path_like(&rest[next..e]));
        if segment.ends_with('}') || (is_span_chain(segment) && followed_by_target) {
            spans.push(segment);
        } else if is_path_like(segment) {
            let spans = (!spans.is_empty()).then(|| spans.join(": "));
            return (spans, Some(segment.to_string()), &rest[next..]);
        } else {
            break;
        }
        segment_start = next;
        cursor = next;
    }

    (None, None, rest)
}

/// Finds the next `: ` at brace depth zero and outside quotes, starting at `from`.
/// Returns the separator's index and the index just after it.
fn next_separator(s: &str, from: usize) -> Option<(usize, usize)> {
    let b = s.as_bytes();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut i = from;
    while i < b.len() {
        let c = b[i];
        if in_string {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_string = false;
            }
        } else {
            match c {
                b'"' => in_string = true,
                b'{' => depth += 1,
                b'}' => depth = depth.saturating_sub(1),
                b':' if depth == 0 && b.get(i + 1) == Some(&b' ') => return Some((i, i + 2)),
                _ => {}
            }
        }
        i += 1;
    }
    None
}

/// Returns true for span names without fields, nested ones joined by single
/// colons: `checkpoint` or `sync:try_to_sync`.
fn is_span_chain(s: &str) -> bool {
    !s.contains("::") && !s.is_empty() && s.split(':').all(is_identifier)
}

fn is_identifier(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Returns true for a Rust module path such as `zebra_rpc::server`.
fn is_path_like(s: &str) -> bool {
    !s.is_empty()
        && s.split("::").all(|part| {
            !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

/// Detects notable events in a stream of parsed lines.
///
/// Panics span two stderr lines, so detection keeps a little state. Use one
/// detector per zebrad run.
#[derive(Debug, Default)]
pub struct Detector {
    /// The previous stderr line was `thread '...' panicked at ...`, so the next
    /// non-empty line is the panic message.
    pending_panic: bool,
}

impl Detector {
    /// Creates a detector for a new zebrad run.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds one line and returns the event it carries, if any.
    pub fn detect(&mut self, line: &LogLine) -> Option<LogEvent> {
        match line.stream {
            LogStream::Stdout => detect_stdout(line),
            LogStream::Stderr => self.detect_stderr(line),
            LogStream::Launcher => None,
        }
    }

    fn detect_stderr(&mut self, line: &LogLine) -> Option<LogEvent> {
        let message = line.message.trim();
        if self.pending_panic {
            if message.is_empty() {
                return None;
            }
            self.pending_panic = false;
            return Some(classify_panic(message));
        }
        if message.starts_with("thread '") && message.contains("panicked at") {
            self.pending_panic = true;
            return None;
        }
        if message.starts_with("error: Zebra could not load the provided configuration") {
            return Some(LogEvent::ConfigError);
        }
        if let Some(detail) = message.strip_prefix("error: zebrad fatal error: ") {
            return Some(LogEvent::FatalError {
                message: detail.to_string(),
            });
        }
        None
    }
}

fn detect_stdout(line: &LogLine) -> Option<LogEvent> {
    let (Some(target), Some(level)) = (line.target.as_deref(), line.level) else {
        return None;
    };
    let message = line.message.as_str();
    let event = match target {
        "zebrad::components::tracing::component"
            if message.starts_with("started tracing component") =>
        {
            LogEvent::TracingStarted
        }
        "zebrad::components::metrics" => LogEvent::MetricsOpened {
            addr: addr_after(message, "Opened metrics endpoint at ")?,
        },
        "zebra_network::peer_set::initialize" => LogEvent::P2pOpened {
            addr: addr_after(message, "Opened Zcash protocol endpoint at ")?,
        },
        "zebra_rpc::server::cookie" if message == "RPC auth cookie written to disk" => {
            LogEvent::RpcCookieWritten
        }
        "zebra_rpc::server" => LogEvent::RpcOpened {
            addr: addr_after(message, "Opened RPC endpoint at ")?,
        },
        "zebrad::commands::start" if message == "Starting zebrad" => LogEvent::StartingZebrad,
        "zebrad::commands::start" if message == "stopping zebrad" => LogEvent::Stopped,
        "zebrad::signal" => LogEvent::ShutdownRequested {
            reason: message
                .strip_prefix("received ")?
                .strip_suffix(", starting shutdown")?
                .to_string(),
        },
        "zebrad::components::tokio" if message == "shutting down Zebra" => LogEvent::ShuttingDown,
        "zebrad::components::sync::end_of_support"
            if level == LogLevel::Warn && message.starts_with("Your Zebra release is too old") =>
        {
            LogEvent::EosWarn {
                message: message.to_string(),
            }
        }
        _ => return None,
    };
    Some(event)
}

/// Parses the socket address that follows `prefix` in `message`.
fn addr_after(message: &str, prefix: &str) -> Option<SocketAddr> {
    message
        .strip_prefix(prefix)?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

fn classify_panic(message: &str) -> LogEvent {
    let message = message.to_string();
    if message.contains("Database likely already open") {
        LogEvent::DbLocked { message }
    } else if message.contains("already in use") {
        LogEvent::PortInUse { message }
    } else if message.contains("Zebra refuses to run") {
        LogEvent::EosHalt { message }
    } else {
        LogEvent::Panic { message }
    }
}

/// The deterministic exit class `event` implies, if any: a cause that a
/// restart without a change cannot fix. The supervisor keeps the first one
/// seen in a run for [`classify_exit`].
pub fn classify_event(event: &LogEvent) -> Option<ExitClass> {
    Some(match event {
        LogEvent::ConfigError => ExitClass::ConfigError,
        LogEvent::EosHalt { .. } => ExitClass::EosHalt,
        LogEvent::DbLocked { .. } => ExitClass::DbLocked,
        LogEvent::PortInUse { .. } => ExitClass::PortInUse,
        _ => return None,
    })
}

/// Classifies how a run ended from its exit code (`None` when killed by a
/// signal) and the first deterministic cause detected during the run
/// ([`classify_event`]).
///
/// The deterministic cause wins over the exit code, because zebrad exits 1 for
/// a config error and aborts (134 on Unix) for the panics, which alone say
/// little.
pub fn classify_exit(exit_code: Option<i32>, cause: Option<ExitClass>) -> ExitClass {
    cause.unwrap_or(match exit_code {
        Some(0) => ExitClass::Clean,
        _ => ExitClass::Crash,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stdout(raw: &str) -> LogLine {
        parse_line(1, LogStream::Stdout, raw)
    }

    #[test]
    fn parses_a_plain_line() {
        let line =
            stdout("2026-10-23T18:35:01.943882Z  INFO zebrad::commands::start: Starting zebrad");
        assert_eq!(
            line.timestamp.as_deref(),
            Some("2026-10-23T18:35:01.943882Z")
        );
        assert_eq!(line.level, Some(LogLevel::Info));
        assert_eq!(line.spans, None);
        assert_eq!(line.target.as_deref(), Some("zebrad::commands::start"));
        assert_eq!(line.message, "Starting zebrad");
    }

    #[test]
    fn parses_an_anonymous_global_span() {
        let line = stdout(
            "2026-10-23T18:35:01.943840Z  INFO {zebrad=\"ad6528b\" net=\"Regtest\"}: zebrad::components::metrics: Opened metrics endpoint at 127.0.0.1:19999",
        );
        assert_eq!(
            line.spans.as_deref(),
            Some("{zebrad=\"ad6528b\" net=\"Regtest\"}")
        );
        assert_eq!(line.target.as_deref(), Some("zebrad::components::metrics"));
        assert_eq!(line.message, "Opened metrics endpoint at 127.0.0.1:19999");
    }

    #[test]
    fn parses_nested_braces_and_colons_inside_span_fields() {
        let line = stdout(
            "2026-10-23T18:35:02.022628Z  INFO init{config=Config { checkpoint_sync: true } network=Regtest { activation_heights: {Height(0): Genesis} }}: zebra_consensus::router: starting state checkpoint validation",
        );
        assert_eq!(
            line.spans.as_deref(),
            Some(
                "init{config=Config { checkpoint_sync: true } network=Regtest { activation_heights: {Height(0): Genesis} }}"
            )
        );
        assert_eq!(line.target.as_deref(), Some("zebra_consensus::router"));
        assert_eq!(line.message, "starting state checkpoint validation");
    }

    #[test]
    fn parses_a_signal_span_and_keeps_colons_in_the_message() {
        let line = stdout(
            "2026-10-23T18:35:20.128340Z  INFO sig{kind=SignalKind(15) name=\"SIGTERM\"}: zebrad::signal: received SIGTERM, starting shutdown: now",
        );
        assert_eq!(
            line.spans.as_deref(),
            Some("sig{kind=SignalKind(15) name=\"SIGTERM\"}")
        );
        assert_eq!(line.target.as_deref(), Some("zebrad::signal"));
        assert_eq!(line.message, "received SIGTERM, starting shutdown: now");
    }

    #[test]
    fn a_bare_span_name_before_a_target_is_a_span() {
        let line =
            stdout("2026-01-01T00:00:00Z  WARN open_listener: zebra_network::init: oops: twice");
        assert_eq!(line.spans.as_deref(), Some("open_listener"));
        assert_eq!(line.target.as_deref(), Some("zebra_network::init"));
        assert_eq!(line.message, "oops: twice");
    }

    #[test]
    fn nested_spans_without_fields_are_joined_by_single_colons() {
        let line = stdout(
            "2026-10-23T18:35:02.047573Z  INFO sync:try_to_sync: zebrad::components::sync: starting sync, obtaining new tips state_tip=Some(Height(0))",
        );
        assert_eq!(line.spans.as_deref(), Some("sync:try_to_sync"));
        assert_eq!(line.target.as_deref(), Some("zebrad::components::sync"));
        assert_eq!(
            line.message,
            "starting sync, obtaining new tips state_tip=Some(Height(0))"
        );
    }

    #[test]
    fn a_bare_target_without_a_module_path_is_the_target() {
        let line = stdout("2026-01-01T00:00:00Z ERROR zebrad: error: something: failed");
        assert_eq!(line.spans, None);
        assert_eq!(line.target.as_deref(), Some("zebrad"));
        assert_eq!(line.message, "error: something: failed");
    }

    #[test]
    fn a_message_without_a_target_keeps_everything() {
        let line = stdout("2026-01-01T00:00:00Z  INFO just words, no target");
        assert_eq!(line.level, Some(LogLevel::Info));
        assert_eq!(line.target, None);
        assert_eq!(line.message, "just words, no target");
    }

    #[test]
    fn continuation_lines_are_unparsed() {
        for raw in [
            "version: 7.0.0-rc.0+192.gad6528b.modified",
            "",
            "   indented",
            "2026-10-23 not a timestamp",
        ] {
            let line = stdout(raw);
            assert_eq!(line.timestamp, None, "{raw:?}");
            assert_eq!(line.level, None, "{raw:?}");
            assert_eq!(line.message, raw, "{raw:?}");
        }
    }

    #[test]
    fn strips_windows_line_endings() {
        let line =
            stdout("2026-01-01T00:00:00Z  INFO zebrad::commands::start: stopping zebrad\r\n");
        assert_eq!(line.message, "stopping zebrad");
    }

    #[test]
    fn timestamps_need_a_valid_shape() {
        assert!(is_timestamp("2026-10-23T18:35:01.943882Z"));
        assert!(is_timestamp("2026-10-23T18:35:01Z"));
        assert!(!is_timestamp("2026-10-23T18:35:01"));
        assert!(!is_timestamp("2026-10-23T18:35:01.Z"));
        assert!(!is_timestamp("2026-10-23 18:35:01Z"));
        assert!(!is_timestamp("INFO"));
    }

    #[test]
    fn detects_stdout_events() {
        let mut detector = Detector::new();
        let cases = [
            (
                "2026-01-01T00:00:00Z  INFO zebra_rpc::server: Opened RPC endpoint at 127.0.0.1:45221",
                LogEvent::RpcOpened {
                    addr: "127.0.0.1:45221".parse().unwrap(),
                },
            ),
            (
                "2026-01-01T00:00:00Z  INFO sig{kind=SignalKind(2) name=\"SIGINT\"}: zebrad::signal: received SIGINT, starting shutdown",
                LogEvent::ShutdownRequested {
                    reason: "SIGINT".into(),
                },
            ),
            (
                "2026-01-01T00:00:00Z  WARN zebrad::components::sync::end_of_support: Your Zebra release is too old and it will stop running at block 123",
                LogEvent::EosWarn {
                    message: "Your Zebra release is too old and it will stop running at block 123"
                        .into(),
                },
            ),
        ];
        for (raw, expected) in cases {
            assert_eq!(detector.detect(&stdout(raw)), Some(expected), "{raw}");
        }
        assert_eq!(
            detector.detect(&stdout(
                "2026-01-01T00:00:00Z  INFO zebra_rpc::server: something else"
            )),
            None
        );
    }

    #[test]
    fn a_panic_message_is_on_the_line_after_the_header() {
        let mut detector = Detector::new();
        let header = parse_line(
            1,
            LogStream::Stderr,
            "thread 'main' (210466) panicked at zebrad/src/components/metrics.rs:38:27:",
        );
        assert_eq!(detector.detect(&header), None);
        let message = parse_line(
            2,
            LogStream::Stderr,
            "Opening metrics endpoint listener 127.0.0.1:19999 failed: FailedToCreateHTTPListener(\"Address already in use (os error 98)\").",
        );
        assert!(matches!(
            detector.detect(&message),
            Some(LogEvent::PortInUse { .. })
        ));
        // The detector is back to normal afterwards.
        let other = parse_line(3, LogStream::Stderr, "stack backtrace:");
        assert_eq!(detector.detect(&other), None);
    }

    #[test]
    fn only_deterministic_causes_classify_an_event() {
        let deterministic = [
            (LogEvent::ConfigError, ExitClass::ConfigError),
            (
                LogEvent::EosHalt {
                    message: "x".into(),
                },
                ExitClass::EosHalt,
            ),
            (
                LogEvent::DbLocked {
                    message: "x".into(),
                },
                ExitClass::DbLocked,
            ),
            (
                LogEvent::PortInUse {
                    message: "x".into(),
                },
                ExitClass::PortInUse,
            ),
        ];
        for (event, class) in deterministic {
            assert_eq!(classify_event(&event), Some(class), "{event:?}");
            assert!(class.is_deterministic());
        }
        for event in [
            LogEvent::FatalError {
                message: "x".into(),
            },
            LogEvent::Panic {
                message: "x".into(),
            },
            LogEvent::EosWarn {
                message: "x".into(),
            },
            LogEvent::StartingZebrad,
            LogEvent::Stopped,
        ] {
            assert_eq!(classify_event(&event), None, "{event:?}");
        }
    }

    #[test]
    fn exit_classification_prefers_deterministic_causes() {
        assert_eq!(
            classify_exit(Some(1), Some(ExitClass::ConfigError)),
            ExitClass::ConfigError
        );
        assert_eq!(
            classify_exit(None, Some(ExitClass::DbLocked)),
            ExitClass::DbLocked
        );
        assert_eq!(
            classify_exit(Some(134), Some(ExitClass::DbLocked)),
            ExitClass::DbLocked
        );
        assert_eq!(classify_exit(Some(0), None), ExitClass::Clean);
        assert_eq!(classify_exit(Some(1), None), ExitClass::Crash);
        assert_eq!(classify_exit(None, None), ExitClass::Crash);
        assert!(!ExitClass::Crash.is_deterministic());
    }
}
