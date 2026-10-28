//! The launcher's clock, formatted the way zebrad formats its log lines:
//! RFC 3339 UTC with microseconds, `tracing-subscriber`'s default timer.
//! The launcher's own lines in the log stream carry these timestamps, so
//! they sort and read like zebrad's.

use std::time::{SystemTime, UNIX_EPOCH};

/// `at` as `YYYY-MM-DDTHH:MM:SS.ffffffZ`. A time before the epoch formats
/// as the epoch.
pub fn rfc3339(at: SystemTime) -> String {
    let since_epoch = at.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = since_epoch.as_secs();
    let micros = since_epoch.subsec_micros();
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (year, month, day) = civil_from_days(days as i64);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{micros:06}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Howard Hinnant's days-to-civil algorithm.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn at(secs: u64, micros: u32) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs) + Duration::from_micros(u64::from(micros))
    }

    #[test]
    fn formats_known_instants() {
        assert_eq!(rfc3339(UNIX_EPOCH), "1970-01-01T00:00:00.000000Z");
        // The metrics fixture was captured at this second (the capture log).
        assert_eq!(rfc3339(at(1_793_210_144, 0)), "2026-10-28T17:55:44.000000Z");
        // A leap day, with microseconds.
        assert_eq!(
            rfc3339(at(1_582_934_400, 123_456)),
            "2020-02-29T00:00:00.123456Z"
        );
        // The last second of a year.
        assert_eq!(
            rfc3339(at(1_767_225_599, 999_999)),
            "2025-12-31T23:59:59.999999Z"
        );
        // Before the epoch: clamped, never a panic.
        assert_eq!(
            rfc3339(UNIX_EPOCH - Duration::from_secs(5)),
            "1970-01-01T00:00:00.000000Z"
        );
    }

    #[test]
    fn output_parses_as_a_zebrad_timestamp() {
        let line = crate::logs::parse_line(
            0,
            launcher_types::LogStream::Launcher,
            &format!(
                "{}  INFO launcher_core::clock: tick",
                rfc3339(at(1_793_210_144, 7))
            ),
        );
        assert_eq!(
            line.timestamp.as_deref(),
            Some("2026-10-28T17:55:44.000007Z")
        );
        assert_eq!(line.message, "tick");
    }
}
