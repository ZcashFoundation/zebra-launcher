//! The "Copy diagnostics" text: versions, OS, the zebrad binary, the stop
//! grace, status, settings, node statistics, the updater's records, the
//! managed `zebrad.toml` and the tails of both logs, ready to paste into an
//! issue.
//!
//! The RPC cookie is a credential and is never read here. URLs in the
//! settings lose any user info before they are printed.

use std::{
    fmt::Write as _,
    fs,
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::Duration,
};

use launcher_types::{NodeStats, Settings, Status};

use crate::{paths::Paths, updates};

/// The launcher version.
pub const LAUNCHER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The `ZEBRAD_VERSION` file at the repository root, read at build time.
const ZEBRAD_VERSION_FILE: &str = include_str!("../../../ZEBRAD_VERSION");

/// How many lines of each log the bundle includes.
pub const TAIL_LINES: usize = 200;

/// At most this many bytes are read from the end of a log for its tail.
const TAIL_MAX_BYTES: u64 = 1024 * 1024;

/// The zebrad version the launcher bundles.
pub fn bundled_zebrad_version() -> &'static str {
    ZEBRAD_VERSION_FILE.trim()
}

/// The last `n` lines of `path`, reading at most the last 1 MiB. Invalid
/// UTF-8 is replaced.
pub fn tail_lines(path: &Path, n: usize) -> io::Result<String> {
    let mut file = fs::File::open(path)?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(TAIL_MAX_BYTES);
    // One byte before the window tells whether it starts at a line start.
    let before = start.min(1);
    file.seek(SeekFrom::Start(start - before))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let mid_line = before == 1 && bytes.first().is_some_and(|&b| b != b'\n');
    let window = bytes.get(before as usize..).unwrap_or_default();
    let text = String::from_utf8_lossy(window);
    let mut lines: Vec<&str> = text.lines().collect();
    // A read that started mid-line drops that partial line.
    if mid_line && !lines.is_empty() {
        lines.remove(0);
    }
    let skip = lines.len().saturating_sub(n);
    let mut tail = lines[skip..].join("\n");
    if !tail.is_empty() {
        tail.push('\n');
    }
    Ok(tail)
}

/// The newest of the daily launcher logs that `tracing-appender` writes for
/// `log_file` (`launcher.log` becomes `launcher.<date>.log`), or `log_file`
/// itself if it exists and there are no dated files.
pub fn latest_launcher_log(log_file: &Path) -> Option<PathBuf> {
    let dir = log_file.parent()?;
    let stem = log_file.file_stem()?.to_str()?;
    let ext = log_file.extension()?.to_str()?;
    let prefix = format!("{stem}.");
    let suffix = format!(".{ext}");
    let newest = fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.starts_with(&prefix) && name.ends_with(&suffix))
        .filter(|name| name.len() > prefix.len() + suffix.len())
        // ISO dates sort lexicographically.
        .max();
    match newest {
        Some(name) => Some(dir.join(name)),
        None => log_file.is_file().then(|| log_file.to_path_buf()),
    }
}

/// The tail of the log at `path` for the bundle: its last [`TAIL_LINES`]
/// lines, or a note when there is no log file yet or it cannot be read.
pub fn log_tail(path: Option<PathBuf>) -> String {
    let Some(path) = path else {
        return "(no log file yet)\n".to_string();
    };
    match tail_lines(&path, TAIL_LINES) {
        Ok(tail) => tail,
        Err(err) => format!("(could not read {}: {err})\n", path.display()),
    }
}

/// `url` with any `user:password@` removed from its authority.
pub fn redact_url(url: &str) -> String {
    let (scheme, rest) = match url.find("://") {
        Some(i) => url.split_at(i + 3),
        None => ("", url),
    };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    match rest[..authority_end].rfind('@') {
        Some(at) => format!("{scheme}[redacted]@{}", &rest[at + 1..]),
        None => url.to_string(),
    }
}

/// Builds the diagnostics text, reading everything it includes from `paths`:
/// the managed `zebrad.toml`, the updater record and the pending-update
/// marker (see [`updates`]), and the tails of the newest launcher log and of
/// the zebrad log (see [`log_tail`]).
///
/// `stats` are the last node statistics, if zebrad has answered; `sidecar`
/// is the zebrad binary the launcher resolved, if known; `stop_grace` is the
/// grace period the running supervisor applies, which the settings may have
/// changed since launch.
pub fn bundle(
    paths: &Paths,
    status: &Status,
    settings: &Settings,
    stats: Option<&NodeStats>,
    sidecar: Option<&Path>,
    stop_grace: Duration,
) -> String {
    let mut out = String::new();
    let section = |out: &mut String, title: &str| {
        let _ = write!(out, "\n== {title} ==\n");
    };

    let _ = writeln!(out, "Zebra Launcher diagnostics");
    let _ = writeln!(
        out,
        "Zebra Launcher {LAUNCHER_VERSION} (zebrad {})",
        bundled_zebrad_version()
    );
    let _ = writeln!(
        out,
        "OS: {} {} ({})",
        std::env::consts::OS,
        std::env::consts::ARCH,
        std::env::consts::FAMILY
    );
    let _ = writeln!(out, "Config dir: {}", paths.config_dir().display());
    let _ = writeln!(out, "Data dir: {}", paths.data_dir().display());
    let _ = writeln!(out, "Log dir: {}", paths.log_dir().display());
    let _ = writeln!(
        out,
        "zebrad binary: {}",
        match sidecar {
            Some(path) if path.is_file() => path.display().to_string(),
            Some(path) => format!("{} (missing)", path.display()),
            None => "(unknown)".to_string(),
        }
    );
    // The supervisor's grace is fixed at launch; a changed setting applies
    // to the next one.
    let _ = writeln!(
        out,
        "Stop grace: {} s in effect, {} s in the settings",
        stop_grace.as_secs(),
        settings.stop_grace_secs
    );

    section(&mut out, "Status");
    out.push_str(&to_json(status));

    section(&mut out, "Stale zebrad from an earlier launcher run");
    match &status.stale {
        Some(stale) => {
            let _ = writeln!(out, "pid {} ({})", stale.pid, stale.binary);
        }
        None => out.push_str("(none)\n"),
    }

    section(&mut out, "Node stats");
    match stats {
        Some(stats) => out.push_str(&to_json(stats)),
        None => out.push_str("(none: zebrad is not running or has not answered yet)\n"),
    }

    section(&mut out, "Settings");
    let redacted = Settings {
        grafana_url: settings.grafana_url.as_deref().map(redact_url),
        ..settings.clone()
    };
    out.push_str(&to_json(&redacted));

    section(&mut out, "Updater record");
    let record = updates::record_path(paths);
    let _ = writeln!(out, "# {}", record.display());
    out.push_str(&to_json(&updates::load_record(&record)));

    section(&mut out, "Pending update marker");
    let marker = updates::marker_path(paths);
    let _ = writeln!(out, "# {}", marker.display());
    match updates::read_marker(&marker) {
        Ok(Some(pending)) => out.push_str(&to_json(&pending)),
        Ok(None) => out.push_str("(none)\n"),
        Err(err) => {
            let _ = writeln!(out, "(could not read it: {err})");
        }
    }

    section(&mut out, "zebrad.toml");
    let config = paths.zebrad_config();
    let _ = writeln!(out, "# {}", config.display());
    match fs::read_to_string(&config) {
        Ok(text) => push_block(&mut out, &text),
        Err(err) => {
            let _ = writeln!(out, "(could not read it: {err})");
        }
    }

    section(&mut out, &format!("launcher log, last {TAIL_LINES} lines"));
    push_block(
        &mut out,
        &log_tail(latest_launcher_log(&paths.launcher_log())),
    );
    section(&mut out, &format!("zebrad log, last {TAIL_LINES} lines"));
    push_block(&mut out, &log_tail(Some(paths.zebrad_log())));
    out
}

fn to_json(value: &impl serde::Serialize) -> String {
    match serde_json::to_string_pretty(value) {
        Ok(mut json) => {
            json.push('\n');
            json
        }
        Err(err) => format!("(could not encode: {err})\n"),
    }
}

fn push_block(out: &mut String, text: &str) {
    if text.is_empty() {
        out.push_str("(empty)\n");
    } else {
        out.push_str(text);
        if !text.ends_with('\n') {
            out.push('\n');
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use launcher_types::{EosInfo, StaleProcessInfo};

    use super::*;

    #[test]
    fn versions_are_filled_in() {
        let zebrad = bundled_zebrad_version();
        assert!(!zebrad.is_empty());
        assert!(!zebrad.contains(char::is_whitespace), "{zebrad:?}");
        assert!(zebrad.split('.').count() >= 3, "{zebrad:?}");
    }

    #[test]
    fn tail_keeps_the_last_lines() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("zebrad.log");
        let text: String = (0..500).map(|i| format!("line {i}\n")).collect();
        fs::write(&path, text).expect("write");
        let tail = tail_lines(&path, 200).expect("tail");
        let lines: Vec<&str> = tail.lines().collect();
        assert_eq!(lines.len(), 200);
        assert_eq!(lines[0], "line 300");
        assert_eq!(lines[199], "line 499");

        fs::write(&path, "only\n").expect("write");
        assert_eq!(tail_lines(&path, 200).expect("tail"), "only\n");
        fs::write(&path, "").expect("write");
        assert_eq!(tail_lines(&path, 200).expect("tail"), "");
    }

    #[test]
    fn tail_of_a_large_file_drops_the_partial_first_line() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("big.log");
        let line = format!("{}\n", "y".repeat(999));
        let text = line.repeat(2000);
        fs::write(&path, text).expect("write");
        let tail = tail_lines(&path, 5000).expect("tail");
        assert!(tail.lines().all(|l| l.len() == 999));
        assert!(tail.lines().count() < 2000);
    }

    #[test]
    fn tail_keeps_a_first_line_that_starts_the_window() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("boundary.log");
        // 1024 lines of 1024 bytes fill the window exactly; the line before
        // it ends right where the window starts.
        let window: String = (0..1024).map(|i| format!("{i:01023}\n")).collect();
        assert_eq!(window.len() as u64, TAIL_MAX_BYTES);
        fs::write(&path, format!("before\n{window}")).expect("write");
        let tail = tail_lines(&path, 5000).expect("tail");
        assert_eq!(tail.lines().count(), 1024);
        assert_eq!(tail.lines().next(), Some(format!("{:01023}", 0).as_str()));

        // One byte more and the window starts inside the first line.
        fs::write(&path, format!("before!{window}")).expect("write");
        let tail = tail_lines(&path, 5000).expect("tail");
        assert_eq!(tail.lines().count(), 1023);
        assert_eq!(tail.lines().next(), Some(format!("{:01023}", 1).as_str()));
    }

    #[test]
    fn finds_the_newest_dated_launcher_log() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("launcher.log");
        assert_eq!(latest_launcher_log(&log), None);
        fs::write(&log, "plain").expect("write");
        assert_eq!(latest_launcher_log(&log), Some(log.clone()));
        for name in [
            "launcher.2026-10-21.log",
            "launcher.2026-10-23.log",
            "launcher.2026-10-22.log",
            "zebrad.log",
        ] {
            fs::write(dir.path().join(name), "x").expect("write");
        }
        assert_eq!(
            latest_launcher_log(&log),
            Some(dir.path().join("launcher.2026-10-23.log"))
        );
    }

    #[test]
    fn log_tail_notes_a_missing_or_unreadable_log() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(log_tail(None), "(no log file yet)\n");
        let missing = dir.path().join("missing.log");
        let note = log_tail(Some(missing.clone()));
        assert!(
            note.starts_with(&format!("(could not read {}: ", missing.display())),
            "{note:?}"
        );
        let log = dir.path().join("zebrad.log");
        fs::write(&log, "a\nb\n").expect("write");
        assert_eq!(log_tail(Some(log)), "a\nb\n");
    }

    #[test]
    fn urls_lose_their_credentials() {
        assert_eq!(
            redact_url("http://admin:secret@127.0.0.1:3000/d/x?kiosk"),
            "http://[redacted]@127.0.0.1:3000/d/x?kiosk"
        );
        assert_eq!(
            redact_url("http://127.0.0.1:3000/d/x?a=b@c"),
            "http://127.0.0.1:3000/d/x?a=b@c"
        );
        assert_eq!(redact_url("localhost:3000"), "localhost:3000");
    }

    fn stats() -> NodeStats {
        NodeStats {
            height: 2_500_000,
            estimated_height: 3_000_000,
            verification_progress: 0.8333,
            peers: 8,
            mempool_transactions: 42,
            chain: "main".into(),
            zebrad_version: "7.0.0".into(),
            last_error: None,
            eos: Some(EosInfo {
                block_height: 3_706_000,
                estimated_time: SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000),
            }),
            updated_at: SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_000_000),
        }
    }

    #[test]
    fn bundle_has_every_section_and_no_cookie() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::under(dir.path());
        paths.ensure_dirs().expect("dirs");
        fs::write(paths.zebrad_config(), "[network]\nnetwork = \"Mainnet\"\n").expect("config");
        fs::write(paths.rpc_cookie_file(), "__cookie__:supersecretcookie").expect("cookie");
        // The newest dated launcher log is the one included, not the plain file.
        fs::write(paths.launcher_log(), "plain launcher line\n").expect("launcher log");
        fs::write(
            paths.log_dir().join("launcher.2026-10-23.log"),
            "dated launcher line\n",
        )
        .expect("dated launcher log");
        fs::write(paths.zebrad_log(), "zebrad line").expect("zebrad log");
        updates::save_record(
            &updates::record_path(&paths),
            &updates::UpdaterRecord {
                last_successful_check: Some(
                    SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000),
                ),
            },
        )
        .expect("record");
        updates::write_marker(
            &updates::marker_path(&paths),
            &updates::PendingUpdate {
                from_version: "0.1.0".into(),
                to_version: "0.2.0".into(),
                zebrad_was_running: true,
                written_at: SystemTime::UNIX_EPOCH,
            },
        )
        .expect("marker");
        let sidecar = dir.path().join("zebra-launcher-zebrad");
        fs::write(&sidecar, b"zebrad").expect("sidecar");

        let settings = Settings {
            grafana_url: Some("http://user:pw@127.0.0.1:3000/d/zebra".into()),
            stop_grace_secs: 90,
            ..Settings::default()
        };
        let status = Status {
            stale: Some(StaleProcessInfo {
                pid: 4242,
                binary: "/old/zebrad".into(),
            }),
            ..Status::default()
        };
        let text = bundle(
            &paths,
            &status,
            &settings,
            Some(&stats()),
            Some(&sidecar),
            Duration::from_secs(60),
        );
        for needle in [
            &format!(
                "Zebra Launcher {LAUNCHER_VERSION} (zebrad {})",
                bundled_zebrad_version()
            ),
            std::env::consts::OS,
            &format!("zebrad binary: {}", sidecar.display()),
            "Stop grace: 60 s in effect, 90 s in the settings",
            "\"phase\": \"stopped\"",
            "pid 4242 (/old/zebrad)",
            "\"height\": 2500000",
            "http://[redacted]@127.0.0.1:3000/d/zebra",
            "\"last_successful_check\": {",
            "\"to_version\": \"0.2.0\"",
            "network = \"Mainnet\"",
            "dated launcher line",
            "zebrad line",
        ] {
            assert!(text.contains(needle), "missing {needle:?} in:\n{text}");
        }
        assert!(!text.contains("plain launcher line"), "{text}");
        assert!(!text.contains("supersecretcookie"));
        assert!(!text.contains("user:pw"));
        assert!(!text.contains(".cookie"), "{text}");
    }

    #[test]
    fn bundle_says_what_is_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::under(dir.path());
        let missing_sidecar = dir.path().join("zebra-launcher-zebrad");
        let text = bundle(
            &paths,
            &Status::default(),
            &Settings::default(),
            None,
            Some(&missing_sidecar),
            Duration::from_secs(60),
        );
        for needle in [
            &format!("zebrad binary: {} (missing)", missing_sidecar.display()),
            "== Stale zebrad from an earlier launcher run ==\n(none)",
            "== Node stats ==\n(none: zebrad is not running or has not answered yet)",
            "\"last_successful_check\": null",
            "== Pending update marker ==",
            "(no log file yet)",
        ] {
            assert!(text.contains(needle), "missing {needle:?} in:\n{text}");
        }
        // No launcher log was ever written; the zebrad log has a fixed path.
        assert_eq!(text.matches("(no log file yet)").count(), 1, "{text}");
        assert!(
            text.contains(&format!(
                "(could not read {}: ",
                paths.zebrad_log().display()
            )),
            "{text}"
        );
        assert_eq!(
            text.matches("(none)\n").count(),
            2,
            "stale and marker: {text}"
        );
        assert!(text.contains("== zebrad.toml ==\n#"), "{text}");
        assert!(text.contains("(could not read it: "), "{text}");

        let text = bundle(
            &paths,
            &Status::default(),
            &Settings::default(),
            None,
            None,
            Duration::from_secs(60),
        );
        assert!(text.contains("zebrad binary: (unknown)"), "{text}");
    }
}
