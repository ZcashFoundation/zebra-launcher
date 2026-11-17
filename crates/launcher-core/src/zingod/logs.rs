//! Reading zingod's output (docs/PUBLIC-SERVICES.md, "zingod's exits").
//!
//! zingod logs logfmt on stdout, without colour off a terminal and with no target:
//!
//! ```text
//! ts=2026-11-17T02:03:49.355008155Z level=info message=Listening endpoint=127.0.0.1:28137 network=regtest tls=off component=Grpc
//! ```
//!
//! [`parse_line`] takes `ts`, `level` and `message`, uses `component` as the target and keeps
//! the other keys after the message, as zebrad's fields are. A start failure is `Error:
//! <message>` on stderr (exit 1, or 2 for `RUST_LOG` and the runtime), and a panic aborts after
//! Rust's report. [`classify`] reads those texts whole, in an order that never takes a held
//! lock or a refused permission for a damaged index: that would offer a rebuild that deletes
//! hours of index.
//!
//! [`redact`] runs before a line reaches the ring or the log file: zingod logs the txid of a
//! transaction that wasn't accepted.
//!
//! The wording matched here is pinned by the zingod fixtures in `tests/fixtures`.

use std::{borrow::Cow, net::SocketAddr};

use launcher_herd::public::zingod::DiskFullKind;
use launcher_types::{ExitClass, LogEvent, LogLevel, LogLine, LogStream};

/// What a txid becomes.
pub const TXID: &str = "[txid]";

/// The panic zingod 1.1.0 can abort with while it drains on `SIGTERM`: header sync's chain
/// watch closes before the follower sees the cancel (`assert!` at
/// `packages/zingod-sync/src/follower.rs:143`, the same one at
/// `packages/zingod-nfs/src/nfs.rs:118`), and its panic hook aborts. The supervisor classes the
/// exit `Crash`, as any panic, with `requested` set; a stop Zeal asked for that ends in it is
/// still a stop (docs/PUBLIC-SERVICES.md, "How Zeal runs zingod").
pub const SIGTERM_RACE: &str = "header sync publishes the verified chain until cancel";

/// `raw` with the value of every `txid=` replaced by [`TXID`]: a logfmt value, quoted or not,
/// and one inside a quoted value.
pub fn redact(raw: &str) -> Cow<'_, str> {
    const KEY: &str = "txid=";
    if !raw.contains(KEY) {
        return Cow::Borrowed(raw);
    }
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(at) = rest.find(KEY) {
        let before = &rest[..at];
        let is_key = before
            .chars()
            .next_back()
            .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_'));
        out.push_str(before);
        out.push_str(KEY);
        rest = &rest[at + KEY.len()..];
        if !is_key {
            continue;
        }
        let end = value_end(rest);
        if end > 0 {
            out.push_str(TXID);
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    Cow::Owned(out)
}

/// Where the value at the start of `text` ends: a quoted value with its quotes, one quoted
/// inside a quoted value (`\"…\"`), or a bare one up to a space or a closing character.
fn value_end(text: &str) -> usize {
    if let Some(inner) = text.strip_prefix("\\\"") {
        return inner.find("\\\"").map_or(text.len(), |end| end + 4);
    }
    if let Some(inner) = text.strip_prefix('"') {
        let mut escaped = false;
        for (i, c) in inner.char_indices() {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => return i + 2,
                _ => {}
            }
        }
        return text.len();
    }
    text.find(|c: char| c.is_whitespace() || matches!(c, '"' | '\\' | ',' | ')' | '}' | ']'))
        .unwrap_or(text.len())
}

/// The `key=value` pairs of a logfmt line, values unquoted and unescaped. A word with no `=`
/// joins the value before it, as a `Debug` value with spaces would print. `None` when the
/// line doesn't start with a pair.
pub fn fields(line: &str) -> Option<Vec<(String, String)>> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut rest = line;
    loop {
        rest = rest.trim_start_matches(' ');
        if rest.is_empty() {
            return Some(out);
        }
        let key_end = rest.find([' ', '=']).unwrap_or(rest.len());
        let key = &rest[..key_end];
        if !rest[key_end..].starts_with('=') || key.is_empty() {
            let (_, value) = out.last_mut()?;
            value.push(' ');
            value.push_str(key);
            rest = &rest[key_end..];
            continue;
        }
        rest = &rest[key_end + 1..];
        let (value, end) = match rest.strip_prefix('"') {
            Some(quoted) => unquote(quoted),
            None => {
                let end = rest.find(' ').unwrap_or(rest.len());
                (rest[..end].to_string(), end)
            }
        };
        out.push((key.to_string(), value));
        rest = &rest[end..];
    }
}

/// The value of a quoted logfmt value whose opening quote is gone, and how many bytes of the
/// original it took with both quotes. zingod's logger quotes with Rust's `escape_debug`.
fn unquote(quoted: &str) -> (String, usize) {
    let mut out = String::new();
    let mut chars = quoted.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return (out, i + 2),
            '\\' => match chars.next() {
                Some((_, 'n')) => out.push('\n'),
                Some((_, 'r')) => out.push('\r'),
                Some((_, 't')) => out.push('\t'),
                Some((_, '0')) => out.push('\0'),
                Some((_, 'u')) => {
                    let mut hex = String::new();
                    for (_, c) in chars.by_ref() {
                        match c {
                            '{' => {}
                            '}' => break,
                            c => hex.push(c),
                        }
                    }
                    out.extend(u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32));
                }
                Some((_, c)) => out.push(c),
                None => {}
            },
            c => out.push(c),
        }
    }
    (out, quoted.len() + 1)
}

/// A value as logfmt writes it: quoted, with Rust's escapes, when it needs to be.
fn quote(value: &str) -> Cow<'_, str> {
    let needs = value.is_empty()
        || value
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '=' | '"' | '\\'));
    if needs {
        Cow::Owned(format!("\"{}\"", value.escape_debug()))
    } else {
        Cow::Borrowed(value)
    }
}

/// Parses one line of zingod output. Never fails: a line that isn't logfmt starting with `ts=`
/// keeps its whole text as the message, with no level or target.
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
        dropped: None,
    };
    if !raw.starts_with("ts=") {
        return unparsed();
    }
    let Some(pairs) = fields(raw) else {
        return unparsed();
    };
    let (mut timestamp, mut level, mut message, mut target) = (None, None, None, None);
    let mut rest = String::new();
    for (key, value) in pairs {
        match key.as_str() {
            "ts" if timestamp.is_none() => timestamp = Some(value),
            "level" if level.is_none() => level = parse_level(&value),
            "message" if message.is_none() => message = Some(value),
            "component" if target.is_none() => target = Some(value),
            _ => {
                if !rest.is_empty() {
                    rest.push(' ');
                }
                rest.push_str(&key);
                rest.push('=');
                rest.push_str(&quote(&value));
            }
        }
    }
    let message = match (message, rest.is_empty()) {
        (Some(message), true) => message,
        (Some(message), false) => format!("{message} {rest}"),
        (None, _) => rest,
    };
    LogLine {
        seq,
        stream,
        timestamp,
        level,
        spans: None,
        target,
        message,
        dropped: None,
    }
}

fn parse_level(level: &str) -> Option<LogLevel> {
    Some(match level {
        "error" => LogLevel::Error,
        "warn" => LogLevel::Warn,
        "info" => LogLevel::Info,
        "debug" => LogLevel::Debug,
        "trace" => LogLevel::Trace,
        _ => return None,
    })
}

/// Detects the events in zingod's output: the gRPC listener, which starts `Running`, and its
/// last words. One detector per run.
#[derive(Debug, Default)]
pub struct Detector {
    /// The previous stderr line was Rust's panic report, so the next non-empty line is the
    /// panic's message.
    pending_panic: bool,
}

impl Detector {
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
            return Some(failure(message));
        }
        if message.starts_with("thread '") && message.contains("panicked at") {
            self.pending_panic = true;
            return None;
        }
        message.strip_prefix("Error: ").map(failure)
    }
}

fn failure(text: &str) -> LogEvent {
    LogEvent::Failure {
        class: classify(text),
        message: text.to_string(),
    }
}

/// `Listening` with `component=Grpc` (`packages/zingod/src/daemon.rs:161`), never the admin
/// listener's: by then the configuration loaded, the open-file limit was raised and the port
/// is bound.
fn detect_stdout(line: &LogLine) -> Option<LogEvent> {
    if line.target.as_deref() != Some("Grpc") {
        return None;
    }
    let rest = line.message.strip_prefix("Listening")?;
    if !(rest.is_empty() || rest.starts_with(' ')) {
        return None;
    }
    let endpoint = fields(rest)?
        .into_iter()
        .find_map(|(key, value)| (key == "endpoint").then_some(value))?;
    Some(LogEvent::IndexerListening {
        addr: endpoint.parse::<SocketAddr>().ok()?,
    })
}

/// Whether `text` matches `pattern`, where each `{}` stands for any text.
fn whole(text: &str, pattern: &str) -> bool {
    let parts: Vec<&str> = pattern.split("{}").collect();
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if parts.len() == 1 {
        return text == pattern;
    }
    if text.len() < first.len() + last.len() || !text.starts_with(first) || !text.ends_with(last) {
        return false;
    }
    let mut rest = &text[first.len()..text.len() - last.len()];
    for middle in &parts[1..parts.len() - 1] {
        match rest.find(middle) {
            Some(at) => rest = &rest[at + middle.len()..],
            None => return false,
        }
    }
    true
}

/// zingod's texts for a configuration it refused, a runtime that couldn't start and an
/// open-file limit too low for it (`packages/zingod/src/main.rs`, `error.rs`), and its
/// validator's address or cookie: all of them in what Zeal wrote or set up, so a restart
/// can't help. zingod starts with a missing or empty cookie and retries, so a cookie it can't
/// read is a folder or a permission.
const CONFIG_TEXTS: [&str; 8] = [
    "configuration error: {}",
    "RUST_LOG: {}",
    "tokio runtime: {}",
    "grpc.max_connections = {} needs {} open files {} and the limit is {}",
    "raising the open-file limit: {}",
    "cannot read validator cookie {}: {}",
    "validator address {} is not host:port: {}",
    "no home directory to find the config file in: pass --config",
];

/// The prefixes of zingod's index I/O errors (`zingod-persistence`): the system's error follows.
const IO_PREFIXES: [&str; 4] = ["index io: ", "manifest io: ", "page io: ", "segment io: "];

/// A commit that failed for a full disk or quota: zingod panics with it.
const COMMIT_FULL: &str = "{} index commit failed: disk {} full";

/// The texts of an index that can't be opened as it is (`packages/zingod-persistence`:
/// `ManifestError` at `manifest.rs:58-79`, `PageError` at `pages.rs:171-177`, the checksum
/// panic at `pages.rs:540`). zingod's `Diverged` (`(resync required)`) is matched anywhere.
const BROKEN_TEXTS: [&str; 12] = [
    "MANIFEST slot does not start with the zingod manifest magic",
    "MANIFEST checksum mismatch",
    "MANIFEST is {} bytes, not this build's two-slot layout: resync the index",
    "MANIFEST body claims {} bytes, {} present",
    "directory holds `{}`, expected `{}`",
    "directory holds format {}, this build reads format {}",
    "directory was built for `{}`, opened for `{}`",
    "{} holds data but the directory has no MANIFEST",
    "{} is {} bytes, the durable state needs {}",
    "{}: tail page fails its checksum",
    "{}: page checksums differ from the digest the manifest committed",
    "{} page {}: checksum mismatch (on-disk corruption: {})",
];

/// The exit class of zingod's last words, `text` being an `Error:` line's message or a
/// panic's. The rules go in this order, each matching whole texts: the configuration, a taken
/// port, a held lock (before any index text), a full disk, a damaged index; anything else,
/// another index I/O error such as a refused permission included, is a crash.
pub fn classify(text: &str) -> ExitClass {
    let text = text.trim();
    if CONFIG_TEXTS.iter().any(|pattern| whole(text, pattern)) {
        ExitClass::ConfigError
    } else if whole(text, "binding the gRPC listener: {}")
        && (text.contains("Address already in use") || text.contains("address already in use"))
    {
        ExitClass::PortInUse
    } else if text.contains("LOCK is held by another process") {
        ExitClass::DbLocked
    } else if disk_full_kind(text).is_some() {
        ExitClass::DiskFull
    } else if BROKEN_TEXTS.iter().any(|pattern| whole(text, pattern))
        || text.contains("(resync required)")
    {
        ExitClass::IndexBroken
    } else {
        ExitClass::Crash
    }
}

/// What a full-disk text names, if `text` is one: the system's `No space left on device` or
/// `Disk quota exceeded` after an index I/O prefix, or zingod's own commit text, which is
/// either.
pub fn disk_full_kind(text: &str) -> Option<DiskFullKind> {
    let text = text.trim();
    if IO_PREFIXES.iter().any(|prefix| text.starts_with(prefix)) {
        if text.contains("Disk quota exceeded") {
            return Some(DiskFullKind::Quota);
        }
        if text.contains("No space left on device") {
            return Some(DiskFullKind::NoSpace);
        }
    }
    whole(text, COMMIT_FULL).then_some(DiskFullKind::Unstated)
}

/// The deterministic exit class `event` implies, if any. The supervisor keeps the first one
/// of a run, which decides the class over the exit code.
pub fn classify_event(event: &LogEvent) -> Option<ExitClass> {
    match event {
        LogEvent::Failure { class, .. } if class.is_deterministic() => Some(*class),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stdout(raw: &str) -> LogLine {
        parse_line(1, LogStream::Stdout, raw)
    }

    #[test]
    fn parses_a_logfmt_line() {
        let line = stdout(
            "ts=2026-11-17T02:03:49.355008155Z level=info message=Listening endpoint=127.0.0.1:28137 network=regtest tls=off component=Grpc",
        );
        assert_eq!(
            line.timestamp.as_deref(),
            Some("2026-11-17T02:03:49.355008155Z")
        );
        assert_eq!(line.level, Some(LogLevel::Info));
        assert_eq!(line.target.as_deref(), Some("Grpc"));
        assert_eq!(line.spans, None);
        assert_eq!(
            line.message,
            "Listening endpoint=127.0.0.1:28137 network=regtest tls=off"
        );
    }

    #[test]
    fn keeps_quoted_values_and_their_quotes() {
        let line = stdout(
            r#"ts=2026-11-16T21:51:58.078636083Z level=warn message="Validator poll failed" validator=127.0.0.1:18299 cause="credentials: cannot read validator cookie /x/.cookie: No such file or directory (os error 2)" component=Traffic"#,
        );
        assert_eq!(line.level, Some(LogLevel::Warn));
        assert_eq!(line.target.as_deref(), Some("Traffic"));
        assert_eq!(
            line.message,
            r#"Validator poll failed validator=127.0.0.1:18299 cause="credentials: cannot read validator cookie /x/.cookie: No such file or directory (os error 2)""#
        );
    }

    #[test]
    fn lines_without_a_component_or_message_still_parse() {
        let line = stdout(
            "ts=2026-11-17T02:03:20.539015666Z level=info message=\"Shutdown signal received\" signal=SIGTERM",
        );
        assert_eq!(line.target, None);
        assert_eq!(line.message, "Shutdown signal received signal=SIGTERM");
        let bare = stdout("ts=2026-11-17T02:03:20Z level=debug a=1 b=two");
        assert_eq!(bare.level, Some(LogLevel::Debug));
        assert_eq!(bare.message, "a=1 b=two");
    }

    #[test]
    fn other_lines_are_kept_whole() {
        for raw in [
            "Error: binding the gRPC listener: Address already in use (os error 98)",
            "thread 'main' (7) panicked at packages/x.rs:1:2:",
            "",
            "level=info message=x",
        ] {
            let line = parse_line(1, LogStream::Stderr, raw);
            assert_eq!(
                (line.timestamp, line.level, line.target),
                (None, None, None),
                "{raw:?}"
            );
            assert_eq!(line.message, raw);
        }
    }

    #[test]
    fn values_are_unescaped_and_spaced_debug_values_kept() {
        assert_eq!(
            fields(r#"a="x \"y\" \\ z\n" b=Some(1) c=Config { x: 1 } d="\u{e9}""#).unwrap(),
            [
                ("a", "x \"y\" \\ z\n"),
                ("b", "Some(1)"),
                ("c", "Config { x: 1 }"),
                ("d", "é"),
            ]
            .map(|(k, v)| (k.to_string(), v.to_string()))
        );
        assert_eq!(fields("words first=1"), None);
        assert_eq!(fields("").unwrap(), []);
        assert_eq!(fields(r#"a="unterminated"#).unwrap()[0].1, "unterminated");
    }

    #[test]
    fn running_starts_at_the_grpc_listener_only() {
        let mut detector = Detector::new();
        let grpc = stdout(
            "ts=2026-11-17T02:03:49Z level=info message=Listening endpoint=127.0.0.1:18137 network=testnet tls=off component=Grpc",
        );
        assert_eq!(
            detector.detect(&grpc),
            Some(LogEvent::IndexerListening {
                addr: "127.0.0.1:18137".parse().unwrap()
            })
        );
        for raw in [
            "ts=2026-11-17T02:03:49Z level=info message=Listening endpoint=127.0.0.1:9999 component=Admin",
            "ts=2026-11-17T02:03:49Z level=info message=Listening endpoint=127.0.0.1:9999",
            "ts=2026-11-17T02:03:49Z level=info message=ListeningSoon endpoint=127.0.0.1:1 component=Grpc",
            "ts=2026-11-17T02:03:49Z level=info message=Listening endpoint=nowhere component=Grpc",
        ] {
            assert_eq!(detector.detect(&stdout(raw)), None, "{raw}");
        }
    }

    #[test]
    fn last_words_are_classified() {
        let cases = [
            (
                "configuration error: parsing configuration: unknown field `bogus`, expected one of `network`",
                ExitClass::ConfigError,
            ),
            ("RUST_LOG: invalid filter directive", ExitClass::ConfigError),
            (
                "tokio runtime: Too many open files (os error 24)",
                ExitClass::ConfigError,
            ),
            (
                "grpc.max_connections = 256 needs 1280 open files (1024 kept for indexes and the validator) and the limit is 1024: raise the hard limit (systemd LimitNOFILE=, docker --ulimit nofile=) or lower grpc.max_connections",
                ExitClass::ConfigError,
            ),
            (
                "raising the open-file limit: Operation not permitted (os error 1)",
                ExitClass::ConfigError,
            ),
            (
                "cannot read validator cookie /mnt/zebra/rpc/.cookie: Is a directory (os error 21)",
                ExitClass::ConfigError,
            ),
            (
                "cannot read validator cookie /mnt/zebra/rpc/.cookie: Permission denied (os error 13)",
                ExitClass::ConfigError,
            ),
            (
                "binding the gRPC listener: Address already in use (os error 98)",
                ExitClass::PortInUse,
            ),
            (
                "binding the gRPC listener: Cannot assign requested address (os error 99)",
                ExitClass::Crash,
            ),
            (
                "manifest io: /idx/indexes/utxo_set/LOCK is held by another process",
                ExitClass::DbLocked,
            ),
            (
                "compact_block index commit failed: disk /mnt/zebra/zingod/testnet/indexes/compact_block full",
                ExitClass::DiskFull,
            ),
            (
                "index io: No space left on device (os error 28)",
                ExitClass::DiskFull,
            ),
            (
                "segment io: Disk quota exceeded (os error 122)",
                ExitClass::DiskFull,
            ),
            ("MANIFEST checksum mismatch", ExitClass::IndexBroken),
            (
                "MANIFEST slot does not start with the zingod manifest magic",
                ExitClass::IndexBroken,
            ),
            (
                "MANIFEST is 12 bytes, not this build's two-slot layout: resync the index",
                ExitClass::IndexBroken,
            ),
            (
                "MANIFEST body claims 99 bytes, 4 present",
                ExitClass::IndexBroken,
            ),
            (
                "directory holds `block_hash`, expected `tree_state`",
                ExitClass::IndexBroken,
            ),
            (
                "directory holds format 3, this build reads format 4",
                ExitClass::IndexBroken,
            ),
            (
                "directory was built for `mainnet`, opened for `testnet`",
                ExitClass::IndexBroken,
            ),
            (
                "/idx/indexes/tree_state/seg-1 holds data but the directory has no MANIFEST",
                ExitClass::IndexBroken,
            ),
            (
                "/idx/indexes/tree_state/log is 4096 bytes, the durable state needs 8192",
                ExitClass::IndexBroken,
            ),
            (
                "/idx/indexes/tree_state/log: tail page fails its checksum",
                ExitClass::IndexBroken,
            ),
            (
                "/idx/indexes/tree_state/log: page checksums differ from the digest the manifest committed",
                ExitClass::IndexBroken,
            ),
            (
                "/idx/indexes/tree_state/log page 7: checksum mismatch (on-disk corruption: stop zingod, run `zingod verify`, resync)",
                ExitClass::IndexBroken,
            ),
            (
                "tree_state committed 0000ab at BlockHeight(10), the trusted validator's chain has 0000cd (resync required)",
                ExitClass::IndexBroken,
            ),
            // A refused permission or an I/O error at open is never a damaged index.
            (
                "manifest io: Permission denied (os error 13)",
                ExitClass::Crash,
            ),
            ("page io: Input/output error (os error 5)", ExitClass::Crash),
            (
                "index io: Read-only file system (os error 30)",
                ExitClass::Crash,
            ),
            ("MANIFEST checksum mismatch, and more", ExitClass::Crash),
            (
                "tokio-rt-worker task ended before shutdown",
                ExitClass::Crash,
            ),
        ];
        for (text, class) in cases {
            assert_eq!(classify(text), class, "{text}");
        }
    }

    #[test]
    fn a_lock_wins_over_every_index_text() {
        assert_eq!(
            classify("manifest io: /idx/indexes/block_hash/LOCK is held by another process"),
            ExitClass::DbLocked
        );
    }

    #[test]
    fn a_full_disk_names_what_ran_out() {
        assert_eq!(
            disk_full_kind("index io: No space left on device (os error 28)"),
            Some(DiskFullKind::NoSpace)
        );
        assert_eq!(
            disk_full_kind("page io: Disk quota exceeded (os error 122)"),
            Some(DiskFullKind::Quota)
        );
        assert_eq!(
            disk_full_kind("block_hash index commit failed: disk /x full"),
            Some(DiskFullKind::Unstated)
        );
        assert_eq!(
            disk_full_kind("No space left on device"),
            None,
            "no index prefix"
        );
        assert_eq!(disk_full_kind("MANIFEST checksum mismatch"), None);
    }

    #[test]
    fn a_panic_is_read_from_the_line_after_its_report() {
        let mut detector = Detector::new();
        let stderr = |raw: &str| parse_line(1, LogStream::Stderr, raw);
        assert_eq!(detector.detect(&stderr("")), None);
        assert_eq!(
            detector.detect(&stderr(
                "thread 'tokio-rt-worker' (812) panicked at packages/zingod-persistence/src/error.rs:41:9:"
            )),
            None
        );
        assert_eq!(detector.detect(&stderr("")), None);
        assert_eq!(
            detector.detect(&stderr(
                "compact_block index commit failed: disk /idx/indexes/compact_block full"
            )),
            Some(LogEvent::Failure {
                class: ExitClass::DiskFull,
                message: "compact_block index commit failed: disk /idx/indexes/compact_block full"
                    .into()
            })
        );
        assert_eq!(
            detector.detect(&stderr(
                "note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace"
            )),
            None
        );
        assert_eq!(
            detector.detect(&stderr("Error: MANIFEST checksum mismatch")),
            Some(LogEvent::Failure {
                class: ExitClass::IndexBroken,
                message: "MANIFEST checksum mismatch".into()
            })
        );
    }

    #[test]
    fn only_deterministic_failures_decide_the_class() {
        let failure = |class| LogEvent::Failure {
            class,
            message: "x".into(),
        };
        assert_eq!(
            classify_event(&failure(ExitClass::IndexBroken)),
            Some(ExitClass::IndexBroken)
        );
        assert_eq!(
            classify_event(&failure(ExitClass::DiskFull)),
            Some(ExitClass::DiskFull)
        );
        assert_eq!(classify_event(&failure(ExitClass::Crash)), None);
        assert_eq!(
            classify_event(&LogEvent::IndexerListening {
                addr: "127.0.0.1:1".parse().unwrap()
            }),
            None
        );
    }

    #[test]
    fn txids_are_redacted() {
        let txid = "5ba81b19c2d6d0b4c8e710554dec4df0f2cbb1752443ff2db84c98426bf93a78";
        let cases = [
            (
                format!("ts=1 level=warn message=\"Transaction not accepted\" txid={txid} attempts=4 component=Submit"),
                "ts=1 level=warn message=\"Transaction not accepted\" txid=[txid] attempts=4 component=Submit".to_string(),
            ),
            (
                format!("ts=1 txid=\"{txid}\" x=1"),
                "ts=1 txid=[txid] x=1".to_string(),
            ),
            (
                format!("ts=1 cause=\"rejected txid=\\\"{txid}\\\" by zebrad\" x=1"),
                "ts=1 cause=\"rejected txid=[txid] by zebrad\" x=1".to_string(),
            ),
            (
                format!("ts=1 entry=Entry {{ txid={txid}, fee: 1 }}"),
                "ts=1 entry=Entry { txid=[txid], fee: 1 }".to_string(),
            ),
            (format!("x txid={txid}"), "x txid=[txid]".to_string()),
            (
                "ts=1 mytxid=abc txid= x".to_string(),
                "ts=1 mytxid=abc txid= x".to_string(),
            ),
        ];
        for (raw, redacted) in cases {
            assert_eq!(redact(&raw), redacted, "{raw}");
        }
        assert!(matches!(redact("ts=1 message=x"), Cow::Borrowed(_)));
    }
}
