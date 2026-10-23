//! Display helpers. Pure Rust, tested natively.

use std::time::{Duration, SystemTime};

use launcher_types::{ExitClass, Phase, UpdateMode};

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

/// Adds Grafana's `kiosk` query parameter, keeping any existing query and fragment.
pub fn kiosk_url(url: &str) -> String {
    let (base, fragment) = match url.find('#') {
        Some(index) => url.split_at(index),
        None => (url, ""),
    };
    let separator = if base.contains('?') { "&" } else { "?" };
    if base.split_once('?').is_some_and(|(_, query)| {
        query
            .split('&')
            .any(|pair| pair == "kiosk" || pair.starts_with("kiosk="))
    }) {
        return url.to_owned();
    }
    format!("{base}{separator}kiosk{fragment}")
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
    fn kiosk_parameter() {
        assert_eq!(
            kiosk_url("http://127.0.0.1:3000/d/x"),
            "http://127.0.0.1:3000/d/x?kiosk"
        );
        assert_eq!(
            kiosk_url("http://127.0.0.1:3000/d/x?orgId=1#top"),
            "http://127.0.0.1:3000/d/x?orgId=1&kiosk#top"
        );
        assert_eq!(
            kiosk_url("http://localhost/d/x?kiosk"),
            "http://localhost/d/x?kiosk"
        );
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
