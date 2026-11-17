//! zingod's reader over output captured from zingod 1.1.0 (tests/fixtures/README.md).

use launcher_core::zingod::logs::{Detector, classify_event, parse_line};
use launcher_types::{ExitClass, LogEvent, LogLevel, LogStream};

fn events(text: &str, stream: LogStream) -> Vec<LogEvent> {
    let mut detector = Detector::new();
    text.lines()
        .enumerate()
        .filter_map(|(seq, raw)| detector.detect(&parse_line(seq as u64, stream, raw)))
        .collect()
}

#[test]
fn a_start_and_stop_runs_from_the_grpc_listener() {
    let log = include_str!("fixtures/zingod-1.1.0-regtest-start-shutdown.stdout.log");
    let lines: Vec<_> = log
        .lines()
        .enumerate()
        .map(|(seq, raw)| parse_line(seq as u64, LogStream::Stdout, raw))
        .collect();
    assert!(
        lines
            .iter()
            .all(|line| line.timestamp.is_some() && line.level.is_some())
    );
    assert_eq!(lines[0].message, "Starting version=1.1.0");
    assert_eq!(lines[0].target.as_deref(), Some("Zingod"));
    assert_eq!(lines[2].level, Some(LogLevel::Warn));
    assert_eq!(
        lines[2].message,
        "In development, not for production mainnet use"
    );
    assert_eq!(
        lines.last().unwrap().message,
        "Shutdown complete",
        "the last line of a clean stop"
    );
    assert_eq!(
        events(log, LogStream::Stdout),
        [LogEvent::IndexerListening {
            addr: "127.0.0.1:28137".parse().unwrap()
        }]
    );
}

#[test]
fn each_captured_failure_has_its_class() {
    for (name, text, class) in [
        (
            "unknown key",
            include_str!("fixtures/zingod-1.1.0-config-error.stderr.log"),
            ExitClass::ConfigError,
        ),
        (
            "cookie is a folder",
            include_str!("fixtures/zingod-1.1.0-cookie-dir.stderr.log"),
            ExitClass::ConfigError,
        ),
        (
            "port taken",
            include_str!("fixtures/zingod-1.1.0-port-in-use.stderr.log"),
            ExitClass::PortInUse,
        ),
        (
            "lock held",
            include_str!("fixtures/zingod-1.1.0-lock-held.stderr.log"),
            ExitClass::DbLocked,
        ),
    ] {
        let events = events(text, LogStream::Stderr);
        let [
            LogEvent::Failure {
                class: got,
                message,
            },
        ] = events.as_slice()
        else {
            panic!("{name}: {events:?}");
        };
        assert_eq!(*got, class, "{name}: {message}");
        assert!(!message.starts_with("Error:"), "{name}: {message}");
        assert_eq!(classify_event(&events[0]), Some(class), "{name}");
    }
}
