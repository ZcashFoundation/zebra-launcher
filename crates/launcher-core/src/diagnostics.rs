//! The "Copy diagnostics" text: versions, OS, status, settings, the managed
//! `zebrad.toml` and the tails of both logs, ready to paste into an issue.
//!
//! The RPC cookie is a credential and is never read here. URLs in the
//! settings lose any user info before they are printed.

use std::{
    fmt::Write as _,
    fs,
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use launcher_types::{Settings, Status};

use crate::paths::Paths;

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
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<&str> = text.lines().collect();
    // A read that started mid-file starts mid-line.
    if start > 0 && !lines.is_empty() {
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

/// Builds the diagnostics text. `launcher_log_tail` and `zebrad_log_tail`
/// are the last lines of each log (see [`tail_lines`]); the managed
/// `zebrad.toml` is read from `paths`.
pub fn diagnostics_bundle(
    paths: &Paths,
    status: &Status,
    settings: &Settings,
    launcher_log_tail: &str,
    zebrad_log_tail: &str,
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

    section(&mut out, "Status");
    out.push_str(&to_json(status));

    section(&mut out, "Settings");
    let redacted = Settings {
        grafana_url: settings.grafana_url.as_deref().map(redact_url),
        ..settings.clone()
    };
    out.push_str(&to_json(&redacted));

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
    push_block(&mut out, launcher_log_tail);
    section(&mut out, &format!("zebrad log, last {TAIL_LINES} lines"));
    push_block(&mut out, zebrad_log_tail);
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

    #[test]
    fn bundle_has_every_section_and_no_cookie() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::under(dir.path());
        paths.ensure_dirs().expect("dirs");
        fs::write(paths.zebrad_config(), "[network]\nnetwork = \"Mainnet\"\n").expect("config");
        fs::write(paths.rpc_cookie_file(), "__cookie__:supersecretcookie").expect("cookie");
        let settings = Settings {
            grafana_url: Some("http://user:pw@127.0.0.1:3000/d/zebra".into()),
            ..Settings::default()
        };
        let text = diagnostics_bundle(
            &paths,
            &Status::default(),
            &settings,
            "launcher line\n",
            "zebrad line",
        );
        for needle in [
            &format!(
                "Zebra Launcher {LAUNCHER_VERSION} (zebrad {})",
                bundled_zebrad_version()
            ),
            std::env::consts::OS,
            "\"phase\": \"stopped\"",
            "network = \"Mainnet\"",
            "launcher line",
            "zebrad line",
            "http://[redacted]@127.0.0.1:3000/d/zebra",
        ] {
            assert!(text.contains(needle), "missing {needle:?} in:\n{text}");
        }
        assert!(!text.contains("supersecretcookie"));
        assert!(!text.contains("user:pw"));
    }
}
