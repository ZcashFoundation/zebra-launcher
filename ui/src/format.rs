//! Display helpers for the node and its numbers: phase labels and controls, thousands, sizes,
//! durations and relative times, the data directory and the sync bar. Pure Rust, tested
//! natively. The words for launcher updates are in [`crate::update_text`], the appearance's
//! in [`crate::theme`], the TOML highlighter in [`crate::config_view`] and the chart texts in
//! [`crate::charts`].

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use launcher_types::{ExitClass, ExitInfo, Phase};

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

/// `code 101`, `killed by a signal`, or `no exit code` when zebrad never ran.
pub fn exit_code_text(exit: &ExitInfo) -> String {
    exit_code_words(exit.code, Some(exit.class))
}

/// [`exit_code_text`] from what a log note gives: the code and, when the note named it, the
/// class. Without a class, a missing code reads `no exit code`: nothing says a signal did it.
pub fn exit_code_words(code: Option<i32>, class: Option<ExitClass>) -> String {
    match (code, class) {
        (Some(code), _) => format!("code {code}"),
        (None, Some(ExitClass::SpawnFailed) | None) => "no exit code".to_owned(),
        (None, Some(_)) => "killed by a signal".to_owned(),
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

/// A fraction as a percentage with two decimals, such as `94.53 %`: the verification progress
/// in the status strip and the sync stage card. The hero's figure is [`crate::charts::percent_1`].
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

/// Three-letter month names, for the short dates.
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// The civil date of a day count since 1970-01-01 (negative before it): `(year, month, day)`.
/// Howard Hinnant's `civil_from_days`.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// The date and time of day at `time` in the zone `tz_offset_min` minutes behind UTC (the
/// `Date.getTimezoneOffset()` convention; 0 is UTC): `(year, month, day, hour, minute)`.
fn local_parts(time: SystemTime, tz_offset_min: i32) -> (i64, u32, u32, u32, u32) {
    let utc_secs = time
        .duration_since(UNIX_EPOCH)
        .map(|since| i64::try_from(since.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0);
    let secs = utc_secs.saturating_sub(i64::from(tz_offset_min) * 60);
    let (year, month, day) = civil_from_days(secs.div_euclid(86_400));
    let day_secs = secs.rem_euclid(86_400) as u32;
    (year, month, day, day_secs / 3600, day_secs % 3600 / 60)
}

/// `23 Oct, 21:13`: a short date and time for "since 23 Oct, 21:13", in the zone
/// `tz_offset_min` minutes behind UTC. [`local_datetime_short`] picks the user's zone.
pub fn datetime_short(time: SystemTime, tz_offset_min: i32) -> String {
    let (_, month, day, hour, minute) = local_parts(time, tz_offset_min);
    format!(
        "{day} {}, {hour:02}:{minute:02}",
        MONTHS[(month - 1) as usize]
    )
}

/// `3 Dec 2026`: a short date for an estimate ("about 3 Dec 2026"), in the zone
/// `tz_offset_min` minutes behind UTC. [`local_date_short`] picks the user's zone.
pub fn date_short(time: SystemTime, tz_offset_min: i32) -> String {
    let (year, month, day, _, _) = local_parts(time, tz_offset_min);
    format!("{day} {} {year}", MONTHS[(month - 1) as usize])
}

/// The browser's zone at `time`, as minutes behind UTC; UTC natively, where no browser runs.
fn tz_offset_min_at(time: SystemTime) -> i32 {
    #[cfg(target_arch = "wasm32")]
    {
        crate::charts::tz_offset_min_at(time)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = time;
        0
    }
}

/// [`datetime_short`] in the user's zone: `23 Oct, 21:13`. Beside [`crate::clock::local_string`]
/// (`10/23/2026, 9:13:58 PM`) for the dashboard's shorter texts.
pub fn local_datetime_short(time: SystemTime) -> String {
    datetime_short(time, tz_offset_min_at(time))
}

/// [`date_short`] in the user's zone: `3 Dec 2026`.
pub fn local_date_short(time: SystemTime) -> String {
    date_short(time, tz_offset_min_at(time))
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
    fn exit_code_words_by_class() {
        let exit = |code, class| ExitInfo {
            code,
            class,
            detail: None,
            at: SystemTime::UNIX_EPOCH,
            requested: false,
        };
        assert_eq!(
            exit_code_text(&exit(Some(101), ExitClass::Crash)),
            "code 101"
        );
        assert_eq!(
            exit_code_text(&exit(None, ExitClass::Crash)),
            "killed by a signal"
        );
        // zebrad never ran: nothing was killed.
        assert_eq!(
            exit_code_text(&exit(None, ExitClass::SpawnFailed)),
            "no exit code"
        );
        assert_eq!(exit_code_words(Some(0), None), "code 0");
        assert_eq!(exit_code_words(None, None), "no exit code");
        assert_eq!(
            exit_code_words(None, Some(ExitClass::Clean)),
            "killed by a signal"
        );
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
    fn short_dates() {
        // 2001-09-09 01:46:40 UTC.
        let time = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        assert_eq!(datetime_short(time, 0), "9 Sep, 01:46");
        assert_eq!(date_short(time, 0), "9 Sep 2001");
        assert_eq!(
            datetime_short(time, 300),
            "8 Sep, 20:46",
            "UTC-5 is the evening before"
        );
        assert_eq!(datetime_short(time, -330), "9 Sep, 07:16", "UTC+5:30");
        // 2026-10-23 19:13 UTC is 21:13 in UTC+2.
        let evening = SystemTime::UNIX_EPOCH + Duration::from_secs(1_792_782_780);
        assert_eq!(datetime_short(evening, -120), "23 Oct, 21:13");
        assert_eq!(datetime_short(evening, 0), "23 Oct, 19:13");
        // A leap day, a day boundary and a year boundary, and the epoch itself.
        let leap = SystemTime::UNIX_EPOCH + Duration::from_secs(1_709_208_000);
        assert_eq!(date_short(leap, 0), "29 Feb 2024");
        // 2026-12-03 23:30 UTC.
        let late = SystemTime::UNIX_EPOCH + Duration::from_secs(1_796_340_600);
        assert_eq!(date_short(late, -60), "4 Dec 2026");
        assert_eq!(datetime_short(late, 0), "3 Dec, 23:30");
        let y2k = SystemTime::UNIX_EPOCH + Duration::from_secs(946_684_800);
        assert_eq!(date_short(y2k, 0), "1 Jan 2000");
        assert_eq!(date_short(y2k, 1), "31 Dec 1999");
        assert_eq!(datetime_short(SystemTime::UNIX_EPOCH, 0), "1 Jan, 00:00");
        // Natively the user's zone is UTC.
        assert_eq!(local_datetime_short(evening), "23 Oct, 19:13");
        assert_eq!(local_date_short(leap), "29 Feb 2024");
    }
}
