//! Display helpers. Pure Rust, tested natively.

use std::time::{Duration, SystemTime};

use launcher_types::{ExitClass, Phase, Theme};

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

// Re-exported for pages/configure.rs until it imports from `config_view`.
pub use crate::config_view::highlight_toml;
// Re-exported for components/updates.rs, components/update_mode_choice.rs and
// pages/first_run.rs until they import from `update_text`.
pub use crate::update_text::{
    NOTES_EXCERPT_CHARS, download_progress, notes_excerpt, update_mode_text, update_state_text,
};

/// `0.1.0` as `v0.1.0`; a version that already starts with `v` is left alone.
pub fn version_tag(version: &str) -> String {
    if version.starts_with('v') {
        version.to_owned()
    } else {
        format!("v{version}")
    }
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
}
