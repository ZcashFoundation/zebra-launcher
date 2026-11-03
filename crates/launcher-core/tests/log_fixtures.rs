//! Parser and detector checks against output captured from a real zebrad.
//! See `tests/fixtures/README.md` for how the files were produced.

use launcher_core::logs::{Detector, classify_event, classify_exit, parse_line};
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

/// The first deterministic cause among `events`, as the supervisor keeps it.
fn cause(events: &[LogEvent]) -> Option<ExitClass> {
    events.iter().find_map(classify_event)
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
    assert_eq!(classify_exit(Some(0), cause(&events)), ExitClass::Clean);
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
        assert_eq!(
            classify_exit(Some(1), cause(&events)),
            ExitClass::ConfigError
        );
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
    assert_eq!(
        classify_exit(Some(134), cause(&events)),
        ExitClass::DbLocked
    );
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
    assert_eq!(classify_exit(None, cause(&events)), ExitClass::PortInUse);
}

#[test]
fn the_light_wallet_endpoint_reports_its_address() {
    let text = include_str!("fixtures/zebrad-7.0.0-lightwalletd-start-shutdown.stdout.log");
    let lines = parse_all(LogStream::Stdout, text);
    let opened = lines
        .iter()
        .find(|line| line.message.starts_with("Opened lightwalletd"))
        .expect("the endpoint line");
    assert_eq!(opened.spans.as_deref(), Some("init"));
    assert_eq!(
        opened.target.as_deref(),
        Some("zebra_rpc::lightwalletd::server")
    );

    let events = detect_all(&lines);
    let endpoint: Vec<_> = events
        .iter()
        .filter(|event| matches!(event, LogEvent::LightwalletdOpened { .. }))
        .collect();
    assert_eq!(
        endpoint,
        [&LogEvent::LightwalletdOpened {
            addr: "127.0.0.1:39067".parse().unwrap()
        }]
    );
    // The endpoint opens after the RPC endpoint, and the run ends cleanly.
    let rpc = events
        .iter()
        .position(|event| matches!(event, LogEvent::RpcOpened { .. }))
        .expect("RPC opened");
    let lwd = events
        .iter()
        .position(|event| matches!(event, LogEvent::LightwalletdOpened { .. }))
        .expect("endpoint opened");
    assert!(rpc < lwd, "{events:#?}");
    assert_eq!(classify_exit(Some(0), cause(&events)), ExitClass::Clean);
}

#[test]
fn a_taken_light_wallet_port_is_a_deterministic_failure() {
    // zebrad returns the bind error from its start command: an ERROR line on stdout and
    // exit code 1, not a panic.
    let lines = parse_all(
        LogStream::Stdout,
        include_str!("fixtures/zebrad-7.0.0-lightwalletd-port-in-use.stdout.log"),
    );
    let events = detect_all(&lines);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, LogEvent::LightwalletdOpened { .. })),
        "{events:#?}"
    );
    let in_use: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            LogEvent::PortInUse { message } => Some(message.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        in_use,
        ["lightwalletd gRPC endpoint at 127.0.0.1:39067: Address already in use (os error 98)"]
    );
    assert_eq!(classify_exit(Some(1), cause(&events)), ExitClass::PortInUse);
}
