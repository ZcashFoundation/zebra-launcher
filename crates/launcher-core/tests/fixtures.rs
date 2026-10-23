//! Parser and detector checks against output captured from a real zebrad.
//! See `tests/fixtures/README.md` for how the files were produced.

use launcher_core::logs::{Detector, classify_exit, parse_line};
use launcher_types::{ExitClass, LogEvent, LogLine, LogStream};

fn parse_all(stream: LogStream, text: &str) -> Vec<LogLine> {
    text.lines()
        .enumerate()
        .map(|(i, raw)| parse_line(i as u64, stream, raw))
        .collect()
}

fn detect_all(lines: &[LogLine]) -> Vec<LogEvent> {
    let mut detector = Detector::new();
    lines
        .iter()
        .filter_map(|line| detector.detect(line))
        .collect()
}

#[test]
fn regtest_start_and_graceful_shutdown() {
    let text = include_str!("fixtures/zebrad-7.0.0-regtest-start-shutdown.stdout.log");
    let lines = parse_all(LogStream::Stdout, text);
    assert_eq!(lines.len(), 101);

    // The "Diagnostic metadata" block is the only multi-line message in a start.
    let continuation: Vec<_> = lines.iter().filter(|l| l.level.is_none()).collect();
    assert_eq!(continuation.len(), 13, "{continuation:#?}");
    assert_eq!(
        continuation[0].message,
        "version: 7.0.0-rc.0+192.gad6528b.modified"
    );
    assert_eq!(continuation[12].message, "debug checks: true");

    // Every other line has a timestamp, a level and a target.
    for line in lines.iter().filter(|l| l.level.is_some()) {
        assert!(line.timestamp.is_some(), "{line:?}");
        assert!(line.target.is_some(), "{line:?}");
    }

    let events = detect_all(&lines);
    assert_eq!(
        events,
        vec![
            LogEvent::TracingStarted,
            LogEvent::MetricsOpened {
                addr: "127.0.0.1:19999".parse().unwrap()
            },
            LogEvent::StartingZebrad,
            LogEvent::P2pOpened {
                addr: "127.0.0.1:46417".parse().unwrap()
            },
            LogEvent::RpcCookieWritten,
            LogEvent::RpcOpened {
                addr: "127.0.0.1:45221".parse().unwrap()
            },
            LogEvent::ShutdownRequested {
                reason: "SIGTERM".into()
            },
            LogEvent::ShuttingDown,
            LogEvent::Stopped,
        ]
    );
    assert_eq!(classify_exit(Some(0), &events), ExitClass::Clean);
}

#[test]
fn config_error_is_reported_on_stderr_with_exit_code_1() {
    for fixture in [
        include_str!("fixtures/zebrad-7.0.0-config-error.stderr.log"),
        include_str!("fixtures/zebrad-7.0.0-missing-config.stderr.log"),
    ] {
        let lines = parse_all(LogStream::Stderr, fixture);
        let events = detect_all(&lines);
        assert_eq!(events.len(), 2, "{events:#?}");
        assert_eq!(events[0], LogEvent::ConfigError);
        let LogEvent::FatalError { message } = &events[1] else {
            panic!("expected a fatal error, got {:?}", events[1]);
        };
        assert!(message.contains("Configuration error:"), "{message}");
        assert_eq!(classify_exit(Some(1), &events), ExitClass::ConfigError);
    }
}

#[test]
fn database_lock_conflict_is_a_deterministic_failure() {
    let lines = parse_all(
        LogStream::Stderr,
        include_str!("fixtures/zebrad-7.0.0-db-locked.stderr.log"),
    );
    let events = detect_all(&lines);
    assert_eq!(events.len(), 1, "{events:#?}");
    let LogEvent::DbLocked { message } = &events[0] else {
        panic!("expected DbLocked, got {:?}", events[0]);
    };
    assert!(
        message.contains("<DATA_DIR>/state/state/v29/regtest"),
        "{message}"
    );
    assert_eq!(classify_exit(Some(134), &events), ExitClass::DbLocked);
}

#[test]
fn metrics_port_in_use_is_a_deterministic_failure() {
    let lines = parse_all(
        LogStream::Stderr,
        include_str!("fixtures/zebrad-7.0.0-port-in-use.stderr.log"),
    );
    let events = detect_all(&lines);
    assert_eq!(events.len(), 1, "{events:#?}");
    assert!(
        matches!(&events[0], LogEvent::PortInUse { message } if message.contains("127.0.0.1:19999"))
    );
    assert_eq!(classify_exit(None, &events), ExitClass::PortInUse);
}
