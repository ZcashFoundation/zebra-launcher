//! Display helpers. Pure Rust, tested natively.

use std::time::{Duration, SystemTime};

use launcher_types::{ExitClass, Phase, Theme, UpdateMode, UpdateState};

/// A short label for a phase, for the badge.
pub fn phase_label(phase: &Phase) -> &'static str {
    match phase {
        Phase::Stopped => "Stopped",
        Phase::Starting => "Starting",
        Phase::Running => "Running",
        Phase::Stopping => "Stopping",
        Phase::Backoff { .. } => "Restarting",
        Phase::Failed { .. } => "Failed",
    }
}

/// The CSS class that colours a phase badge.
pub fn phase_class(phase: &Phase) -> &'static str {
    match phase {
        Phase::Stopped => "phase phase-stopped",
        Phase::Starting | Phase::Stopping => "phase phase-transition",
        Phase::Running => "phase phase-running",
        Phase::Backoff { .. } => "phase phase-backoff",
        Phase::Failed { .. } => "phase phase-failed",
    }
}

/// Why zebrad stopped, in words.
pub fn exit_class_text(class: ExitClass) -> &'static str {
    match class {
        ExitClass::Clean => "stopped cleanly",
        ExitClass::ConfigError => "zebrad rejected its configuration",
        ExitClass::EosHalt => "this zebrad release is past its end of support",
        ExitClass::DbLocked => "another zebrad is using the state database",
        ExitClass::PortInUse => "a port zebrad needs is already in use",
        ExitClass::SpawnFailed => "zebrad could not be started",
        ExitClass::Crash => "zebrad crashed",
    }
}

/// Which of Start, Stop and Restart make sense in a phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Controls {
    pub start: bool,
    pub stop: bool,
    pub restart: bool,
}

pub fn controls(phase: &Phase) -> Controls {
    let (start, stop, restart) = match phase {
        Phase::Stopped | Phase::Failed { .. } => (true, false, false),
        Phase::Starting => (false, true, false),
        Phase::Running => (false, true, true),
        Phase::Stopping => (false, false, false),
        // Start now instead of waiting, or Stop to cancel the scheduled restart.
        Phase::Backoff { .. } => (true, true, false),
    };
    Controls {
        start,
        stop,
        restart,
    }
}

/// `2950000` becomes `2,950,000`.
pub fn thousands(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// A verification progress fraction as a percentage, such as `94.53 %`.
pub fn percent(fraction: f64) -> String {
    let fraction = if fraction.is_finite() {
        fraction.clamp(0.0, 1.0)
    } else {
        0.0
    };
    format!("{:.2} %", fraction * 100.0)
}

/// A rough human duration: `45 s`, `12 min`, `5 h`, `41 days`.
pub fn rough_duration(duration: Duration) -> String {
    let secs = duration.as_secs();
    match secs {
        0..60 => format!("{secs} s"),
        60..3600 => format!("{} min", secs / 60),
        3600..172_800 => format!("{} h", secs / 3600),
        _ => format!("{} days", secs / 86_400),
    }
}

/// `in 41 days` or `5 h ago`, relative to `now`.
pub fn relative(time: SystemTime, now: SystemTime) -> String {
    const JUST_NOW: Duration = Duration::from_secs(2);
    match time.duration_since(now) {
        Ok(ahead) if ahead < JUST_NOW => "just now".to_owned(),
        Ok(ahead) => format!("in {}", rough_duration(ahead)),
        Err(behind) if behind.duration() < JUST_NOW => "just now".to_owned(),
        Err(behind) => format!("{} ago", rough_duration(behind.duration())),
    }
}

/// Below this much free space the data location gets a warning: Mainnet state is hundreds of GB.
pub const LOW_SPACE_BYTES: u64 = 300_000_000_000;

/// A size in decimal units, as disks are sold, with one decimal: `512 bytes`, `1.5 kB`,
/// `12.3 MB`, `1,834.5 GB`.
pub fn bytes(value: u64) -> String {
    const UNITS: [(u128, &str); 3] = [(1_000_000_000, "GB"), (1_000_000, "MB"), (1_000, "kB")];
    for (unit, name) in UNITS {
        let tenths = (u128::from(value) * 10 + unit / 2) / unit;
        if tenths >= 10 {
            let whole = u64::try_from(tenths / 10).unwrap_or(u64::MAX);
            return format!("{}.{} {name}", thousands(whole), tenths % 10);
        }
    }
    match value {
        1 => "1 byte".to_owned(),
        _ => format!("{value} bytes"),
    }
}

/// True when the free space is known and below [`LOW_SPACE_BYTES`].
pub fn is_low_space(free_bytes: Option<u64>) -> bool {
    free_bytes.is_some_and(|free| free < LOW_SPACE_BYTES)
}

/// Where the chain state goes for a data directory, as the backend derives it.
pub fn state_dir(data_dir: &str) -> String {
    let separator = if data_dir.contains('\\') { '\\' } else { '/' };
    format!("{}{separator}state", data_dir.trim_end_matches(['/', '\\']))
}

/// Whether the data directory may change or the node data be deleted: the backend allows it
/// only while zebrad is stopped or failed.
pub fn data_change_allowed(phase: &Phase) -> bool {
    matches!(phase, Phase::Stopped | Phase::Failed { .. })
}

/// The label and explanation of an update mode, shared by the first-run flow and Settings.
pub fn update_mode_text(mode: UpdateMode) -> (&'static str, &'static str) {
    match mode {
        UpdateMode::Off => (
            "Off",
            "Never check for launcher updates. The launcher makes no update requests.",
        ),
        UpdateMode::Notify => (
            "Notify",
            "Check for launcher updates and tell you when one is available. \
             Nothing is installed until you click Install.",
        ),
        UpdateMode::Auto => (
            "Auto",
            "Download updates in the background and install when this release is near its \
             end of support or when you click.",
        ),
    }
}

/// Download progress: `12.3 MB of 84.0 MB (14 %)`, or `12.3 MB` when the size is unknown.
pub fn download_progress(downloaded: u64, total: Option<u64>) -> String {
    match total {
        Some(total) if total > 0 => format!(
            "{} of {} ({} %)",
            bytes(downloaded),
            bytes(total),
            (u128::from(downloaded.min(total)) * 100 / u128::from(total))
        ),
        _ => bytes(downloaded),
    }
}

/// At most this many characters of release notes are shown in the header banner.
pub const NOTES_EXCERPT_CHARS: usize = 280;

/// The start of the release notes for the header banner: the first paragraph, cut at
/// `max_chars` characters (on a word boundary when there is one) with an ellipsis. Paragraphs
/// end at blank lines (with `\n` or `\r\n` line ends) and at Markdown headings; headings
/// before the first paragraph are skipped. `None` when there is no paragraph.
pub fn notes_excerpt(notes: &str, max_chars: usize) -> Option<String> {
    let is_break = |line: &str| line.is_empty() || line.starts_with('#');
    // `lines` drops a `\r` before each `\n`; a lone `\r` is trimmed with the other blanks.
    let mut lines = notes
        .lines()
        .map(str::trim)
        .skip_while(|line| is_break(line));
    let first = lines
        .by_ref()
        .take_while(|line| !is_break(line))
        .collect::<Vec<_>>()
        .join("\n");
    if first.is_empty() {
        return None;
    }
    let first = first.as_str();
    let cut = first.char_indices().nth(max_chars).map(|(index, _)| index);
    let more = cut.is_some() || lines.any(|line| !line.is_empty());
    let mut excerpt = match cut {
        Some(index) => {
            let (head, rest) = first.split_at(index);
            // Cut inside a word: drop the partial word, if there is an earlier space.
            let word_cut = !rest.starts_with(char::is_whitespace);
            match head.rfind(char::is_whitespace) {
                Some(space) if word_cut => &head[..space],
                _ => head,
            }
            .trim_end()
        }
        None => first,
    }
    .to_owned();
    if more {
        excerpt.push('…');
    }
    Some(excerpt)
}

/// The updater's state in one sentence, for Settings and the result of "Check for updates".
pub fn update_state_text(state: &UpdateState, now: SystemTime) -> String {
    match state {
        UpdateState::Idle => "Not checked yet.".to_owned(),
        UpdateState::Checking => "Checking for updates…".to_owned(),
        UpdateState::Available { version, .. } => {
            format!("Zebra Launcher {version} is available.")
        }
        UpdateState::Downloading {
            version,
            downloaded,
            total,
        } => format!(
            "Downloading Zebra Launcher {version}: {}.",
            download_progress(*downloaded, *total)
        ),
        UpdateState::ReadyToInstall {
            version,
            countdown_secs: None,
        } => format!("Zebra Launcher {version} is downloaded and verified, ready to install."),
        UpdateState::ReadyToInstall {
            version,
            countdown_secs: Some(secs),
        } => format!("Zebra Launcher {version} installs in {secs} s."),
        UpdateState::Installing { version } => {
            format!("Installing Zebra Launcher {version}; the launcher restarts next.")
        }
        UpdateState::UpToDate { checked_at } => {
            format!("Up to date. Last checked {}.", relative(*checked_at, now))
        }
        UpdateState::Error { message, at } => {
            format!(
                "The last update attempt failed ({}): {message}",
                relative(*at, now)
            )
        }
    }
}

/// True while the updater wants a look: a release is available, on its way, ready, or failed.
/// The Settings tab wears a badge then.
pub fn update_needs_attention(state: &UpdateState) -> bool {
    matches!(
        state,
        UpdateState::Available { .. }
            | UpdateState::Downloading { .. }
            | UpdateState::ReadyToInstall { .. }
            | UpdateState::Error { .. }
    )
}

/// The label and explanation of an appearance, for the Settings radios.
pub fn theme_text(theme: Theme) -> (&'static str, &'static str) {
    match theme {
        Theme::System => (
            "System",
            "Follow the operating system's light or dark setting.",
        ),
        Theme::Light => ("Light", "Always light."),
        Theme::Dark => ("Dark", "Always dark."),
    }
}

/// The `value` of an appearance radio, the same word as on the wire.
pub fn theme_value(theme: Theme) -> &'static str {
    match theme {
        Theme::System => "system",
        Theme::Light => "light",
        Theme::Dark => "dark",
    }
}

/// The shortcut for the tab at `number` (1 to 5), as a tooltip: `Ctrl+1`, or `⌘1` on a Mac.
pub fn shortcut_label(number: usize, mac: bool) -> String {
    if mac {
        format!("\u{2318}{number}")
    } else {
        format!("Ctrl+{number}")
    }
}

/// From this verification progress on the chain is at its tip, as far as the bar can show.
pub const SYNCED_PROGRESS: f64 = 0.9999;

/// A verification progress fraction as a percentage from 0 to 100, for a progress bar's width
/// and value. Not-a-number counts as nothing done.
pub fn sync_percent(fraction: f64) -> f64 {
    if fraction.is_finite() {
        fraction.clamp(0.0, 1.0) * 100.0
    } else {
        0.0
    }
}

/// True when the node is at the chain tip ([`SYNCED_PROGRESS`] or more).
pub fn is_synced(fraction: f64) -> bool {
    fraction >= SYNCED_PROGRESS
}

/// `0.1.0` as `v0.1.0`; a version that already starts with `v` is left alone.
pub fn version_tag(version: &str) -> String {
    if version.starts_with('v') {
        version.to_owned()
    } else {
        format!("v{version}")
    }
}

/// A highlighting class for a span of TOML text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TomlClass {
    Plain,
    Comment,
    Section,
    Key,
    String,
    Literal,
}

impl TomlClass {
    pub fn css(self) -> &'static str {
        match self {
            TomlClass::Plain => "",
            TomlClass::Comment => "toml-comment",
            TomlClass::Section => "toml-section",
            TomlClass::Key => "toml-key",
            TomlClass::String => "toml-string",
            TomlClass::Literal => "toml-literal",
        }
    }
}

/// Splits TOML text into classified spans, line by line, for the read-only config view.
/// Deliberately simple: it only has to look right for config files, not parse them.
pub fn highlight_toml(text: &str) -> Vec<(TomlClass, String)> {
    let mut spans = Vec::new();
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            spans.push((TomlClass::Comment, line.to_owned()));
        } else if trimmed.starts_with('[') {
            spans.push((TomlClass::Section, line.to_owned()));
        } else if let Some((key, value)) = line.split_once('=') {
            spans.push((TomlClass::Key, key.to_owned()));
            spans.push((TomlClass::Plain, "=".to_owned()));
            let value_start = value.len() - value.trim_start().len();
            spans.push((TomlClass::Plain, value[..value_start].to_owned()));
            let value = &value[value_start..];
            let class = match value.trim_end() {
                v if v.starts_with('"') || v.starts_with('\'') => TomlClass::String,
                "true" | "false" => TomlClass::Literal,
                v if v.starts_with(|c: char| c.is_ascii_digit() || c == '-' || c == '+') => {
                    TomlClass::Literal
                }
                _ => TomlClass::Plain,
            };
            spans.push((class, value.to_owned()));
        } else {
            spans.push((TomlClass::Plain, line.to_owned()));
        }
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controls_follow_the_phase() {
        let all = [
            (Phase::Stopped, (true, false, false)),
            (Phase::Starting, (false, true, false)),
            (Phase::Running, (false, true, true)),
            (Phase::Stopping, (false, false, false)),
            (
                Phase::Failed {
                    class: ExitClass::ConfigError,
                },
                (true, false, false),
            ),
        ];
        for (phase, (start, stop, restart)) in all {
            assert_eq!(
                controls(&phase),
                Controls {
                    start,
                    stop,
                    restart
                },
                "{phase:?}"
            );
        }
    }

    #[test]
    fn numbers() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(2_950_000), "2,950,000");
        assert_eq!(percent(0.94531), "94.53 %");
        assert_eq!(percent(1.2), "100.00 %");
        assert_eq!(percent(f64::NAN), "0.00 %");
    }

    #[test]
    fn durations() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        assert_eq!(
            relative(now + Duration::from_secs(41 * 86_400), now),
            "in 41 days"
        );
        assert_eq!(
            relative(now - Duration::from_secs(5 * 3600), now),
            "5 h ago"
        );
        assert_eq!(relative(now + Duration::from_millis(300), now), "just now");
        assert_eq!(relative(now - Duration::from_secs(1), now), "just now");
        assert_eq!(rough_duration(Duration::from_secs(59)), "59 s");
        assert_eq!(rough_duration(Duration::from_secs(120)), "2 min");
    }

    #[test]
    fn byte_sizes() {
        assert_eq!(bytes(0), "0 bytes");
        assert_eq!(bytes(1), "1 byte");
        assert_eq!(bytes(949), "949 bytes");
        assert_eq!(bytes(1_500), "1.5 kB");
        assert_eq!(bytes(12_345_678), "12.3 MB");
        assert_eq!(bytes(999_999_999), "1.0 GB");
        assert_eq!(bytes(299_960_000_000), "300.0 GB");
        assert_eq!(bytes(1_834_512_000_000), "1,834.5 GB");
        assert_eq!(bytes(u64::MAX), "18,446,744,073.7 GB");
    }

    #[test]
    fn low_space() {
        assert!(!is_low_space(None));
        assert!(is_low_space(Some(LOW_SPACE_BYTES - 1)));
        assert!(!is_low_space(Some(LOW_SPACE_BYTES)));
    }

    #[test]
    fn state_dir_is_inside_the_data_dir() {
        assert_eq!(state_dir("/mnt/zebra"), "/mnt/zebra/state");
        assert_eq!(state_dir("/mnt/zebra/"), "/mnt/zebra/state");
        assert_eq!(state_dir(r"D:\Zebra"), r"D:\Zebra\state");
    }

    #[test]
    fn data_changes_need_a_stopped_node() {
        assert!(data_change_allowed(&Phase::Stopped));
        assert!(data_change_allowed(&Phase::Failed {
            class: ExitClass::DbLocked
        }));
        for phase in [Phase::Starting, Phase::Running, Phase::Stopping] {
            assert!(!data_change_allowed(&phase), "{phase:?}");
        }
    }

    #[test]
    fn download_progress_text() {
        assert_eq!(
            download_progress(12_345_678, Some(84_000_000)),
            "12.3 MB of 84.0 MB (14 %)"
        );
        assert_eq!(
            download_progress(84_000_000, Some(84_000_000)),
            "84.0 MB of 84.0 MB (100 %)"
        );
        assert_eq!(download_progress(1_500, None), "1.5 kB");
        assert_eq!(download_progress(1_500, Some(0)), "1.5 kB");
    }

    #[test]
    fn release_notes_excerpt() {
        assert_eq!(notes_excerpt("  ", 10), None);
        assert_eq!(
            notes_excerpt("Fixes a crash.", 280),
            Some("Fixes a crash.".into())
        );
        assert_eq!(
            notes_excerpt("Fixes a crash.\n\n## Details\nMore.", 280),
            Some("Fixes a crash.…".into())
        );
        assert_eq!(
            notes_excerpt("Bumps zebrad to 7.1.0 and more", 12),
            Some("Bumps zebrad…".into())
        );
        assert_eq!(
            notes_excerpt("Bumps zebrad to 7.1.0", 9),
            Some("Bumps…".into())
        );
        assert_eq!(notes_excerpt("ééééé", 3), Some("ééé…".into()));
        // CRLF line ends, as release notes written on Windows have, and Markdown headings.
        let crlf = "## Zebra Launcher 0.2.0\r\n\r\nBumps zebrad to 7.1.0.\r\nFixes a crash.\r\n\r\n### Details\r\nMore.\r\n";
        assert_eq!(
            notes_excerpt(crlf, 280),
            Some("Bumps zebrad to 7.1.0.\nFixes a crash.…".into())
        );
        assert_eq!(
            notes_excerpt("Fixes a crash.\r\n \r\nMore.", 280),
            Some("Fixes a crash.…".into())
        );
        assert_eq!(
            notes_excerpt("Only this.\r\n\r\n", 280),
            Some("Only this.".into())
        );
        assert_eq!(notes_excerpt("# Heading only\r\n", 280), None);
    }

    #[test]
    fn update_state_sentences() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        assert_eq!(
            update_state_text(&UpdateState::Idle, now),
            "Not checked yet."
        );
        assert_eq!(
            update_state_text(
                &UpdateState::UpToDate {
                    checked_at: now - Duration::from_secs(300)
                },
                now
            ),
            "Up to date. Last checked 5 min ago."
        );
        assert_eq!(
            update_state_text(
                &UpdateState::ReadyToInstall {
                    version: "0.2.0".into(),
                    countdown_secs: Some(42)
                },
                now
            ),
            "Zebra Launcher 0.2.0 installs in 42 s."
        );
        assert_eq!(
            update_state_text(
                &UpdateState::Downloading {
                    version: "0.2.0".into(),
                    downloaded: 1_000_000,
                    total: Some(4_000_000)
                },
                now
            ),
            "Downloading Zebra Launcher 0.2.0: 1.0 MB of 4.0 MB (25 %)."
        );
    }

    #[test]
    fn update_states_that_need_attention() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        for wants in [
            UpdateState::Available {
                version: "0.2.0".into(),
                notes: None,
                pub_date: None,
            },
            UpdateState::Downloading {
                version: "0.2.0".into(),
                downloaded: 1,
                total: None,
            },
            UpdateState::ReadyToInstall {
                version: "0.2.0".into(),
                countdown_secs: None,
            },
            UpdateState::Error {
                message: "offline".into(),
                at: now,
            },
        ] {
            assert!(update_needs_attention(&wants), "{wants:?}");
        }
        for quiet in [
            UpdateState::Idle,
            UpdateState::Checking,
            UpdateState::Installing {
                version: "0.2.0".into(),
            },
            UpdateState::UpToDate { checked_at: now },
        ] {
            assert!(!update_needs_attention(&quiet), "{quiet:?}");
        }
    }

    #[test]
    fn theme_words() {
        assert_eq!(theme_text(Theme::System).0, "System");
        assert_eq!(theme_value(Theme::System), "system");
        assert_eq!(theme_value(Theme::Light), "light");
        assert_eq!(theme_value(Theme::Dark), "dark");
    }

    #[test]
    fn shortcut_labels() {
        assert_eq!(shortcut_label(1, false), "Ctrl+1");
        assert_eq!(shortcut_label(5, true), "\u{2318}5");
    }

    #[test]
    fn sync_bar_values() {
        assert_eq!(sync_percent(0.0), 0.0);
        assert_eq!(sync_percent(0.5), 50.0);
        assert_eq!(sync_percent(1.5), 100.0);
        assert_eq!(sync_percent(-0.1), 0.0);
        assert_eq!(sync_percent(f64::NAN), 0.0);
        assert!(!is_synced(0.9998));
        assert!(is_synced(0.9999));
        assert!(is_synced(1.0));
    }

    #[test]
    fn version_tags() {
        assert_eq!(version_tag("0.1.0"), "v0.1.0");
        assert_eq!(version_tag("v0.2.0"), "v0.2.0");
    }

    #[test]
    fn toml_highlighting_round_trips() {
        let text = "# comment\n[rpc]\nlisten_addr = \"127.0.0.1:8232\"\nenable_cookie_auth = true\npeers = [\n  \"a\",\n]\n";
        let spans = highlight_toml(text);
        let joined: String = spans.iter().map(|(_, text)| text.as_str()).collect();
        assert_eq!(joined, text);
        assert_eq!(spans[0].0, TomlClass::Comment);
        assert_eq!(spans[1].0, TomlClass::Section);
        assert_eq!(spans[2], (TomlClass::Key, "listen_addr ".into()));
        assert_eq!(spans[5], (TomlClass::String, "\"127.0.0.1:8232\"\n".into()));
        assert_eq!(spans[9], (TomlClass::Literal, "true\n".into()));
    }
}
