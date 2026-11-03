//! Tolerant readers for the pieces of a zebrad log line: `key=value` fields, the value shapes
//! zebrad prints (`Height(123)`, `12.345%`, `Some(Height(5))`), RFC 3339 timestamps, and the
//! pattern that stands for a message once its numbers and names are blanked out.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The value of `key` in a `tracing` message, where the message text comes first and the
/// `key=value` fields follow, separated by single spaces.
///
/// A value runs up to the next ` otherkey=` token (a space, an identifier, `=`), so a humantime
/// value with a space inside (`time_since_last_state_block=1m 2s target_block_spacing=75s`)
/// comes back whole as `1m 2s`. The key may start the message. `None` when the key is absent.
pub fn field<'a>(message: &'a str, key: &str) -> Option<&'a str> {
    if key.is_empty() {
        return None;
    }
    for (index, _) in message.match_indices(key) {
        let at_boundary = index == 0 || message.as_bytes()[index - 1] == b' ';
        let value_start = index + key.len();
        if !at_boundary || message.as_bytes().get(value_start) != Some(&b'=') {
            continue;
        }
        let rest = &message[value_start + 1..];
        let end = next_key_start(rest).unwrap_or(rest.len());
        return Some(rest[..end].trim_end());
    }
    None
}

/// The byte offset in `text` of the space that begins the next ` key=` token, if any.
fn next_key_start(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut index = 0;
    while let Some(space) = text[index..].find(' ') {
        let space = index + space;
        let mut cursor = space + 1;
        let starts_identifier = bytes
            .get(cursor)
            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_');
        if starts_identifier {
            while bytes
                .get(cursor)
                .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.'))
            {
                cursor += 1;
            }
            if bytes.get(cursor) == Some(&b'=') {
                return Some(space);
            }
        }
        index = space + 1;
    }
    None
}

/// The height in `Height(123)` or `Some(Height(123))`. `None`, `Some(None)` and anything else
/// give `None`.
pub fn height_of(value: &str) -> Option<u64> {
    let value = value.trim();
    let value = value
        .strip_prefix("Some(")
        .and_then(|inner| inner.strip_suffix(')'))
        .unwrap_or(value);
    value
        .strip_prefix("Height(")
        .and_then(|inner| inner.strip_suffix(')'))
        .and_then(|digits| digits.trim().parse().ok())
}

/// The number in `12.345%`, also tolerating the `0.000 %` older builds printed.
pub fn percent_of(value: &str) -> Option<f64> {
    let digits = value.trim().strip_suffix('%').unwrap_or(value).trim();
    digits
        .parse::<f64>()
        .ok()
        .filter(|number| number.is_finite())
}

/// A plain integer field such as `remaining_sync_blocks=2000000`.
pub fn u64_of(value: &str) -> Option<u64> {
    value.trim().parse().ok()
}

/// The instant of an RFC 3339 UTC timestamp as zebrad prints it:
/// `2026-10-23T18:35:01.937583Z`. Fractions are optional; anything else is `None`.
pub fn parse_timestamp(timestamp: &str) -> Option<SystemTime> {
    let (date, time) = timestamp.split_once('T')?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let time = time.trim_end_matches('Z');
    let (clock, fraction) = match time.split_once('.') {
        Some((clock, fraction)) => (clock, Some(fraction)),
        None => (time, None),
    };
    let mut clock_parts = clock.split(':');
    let hour: u64 = clock_parts.next()?.parse().ok()?;
    let minute: u64 = clock_parts.next()?.parse().ok()?;
    let second: u64 = clock_parts.next().map_or(Some(0), |s| s.parse().ok())?;
    if clock_parts.next().is_some() || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    if days < 0 {
        return None;
    }
    let secs = days as u64 * 86_400 + hour * 3600 + minute * 60 + second;
    let nanos = match fraction {
        Some(fraction) if !fraction.is_empty() => {
            if !fraction.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            // Pad or cut to nine digits.
            let digits: String = fraction
                .chars()
                .chain(std::iter::repeat('0'))
                .take(9)
                .collect();
            digits.parse::<u32>().ok()?
        }
        _ => 0,
    };
    Some(UNIX_EPOCH + Duration::new(secs, nanos))
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = i64::from(if month > 2 { month - 3 } else { month + 9 });
    let day_of_year = (153 * month_index + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Whole minutes since the Unix epoch.
pub fn epoch_minute(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() / 60)
}

/// The start of an epoch minute.
pub fn minute_start(minute: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(minute * 60)
}

/// How many characters a problem pattern keeps.
pub const PATTERN_CHARS: usize = 200;

/// The message with its variable parts blanked: decimal numbers, hex runs of six digits or
/// more, quoted strings and IP addresses each become `_`, and the result is cut to
/// [`PATTERN_CHARS`] characters. Two lines about the same thing share one pattern.
pub fn pattern_of(message: &str) -> String {
    let mut pattern = String::with_capacity(message.len());
    let bytes = message.as_bytes();
    let mut index = 0;
    let mut previous_alphanumeric = false;
    while index < message.len() {
        let byte = bytes[index];
        if byte == b'"' {
            // A quoted string, to its closing quote (or the end).
            let close = message[index + 1..]
                .find('"')
                .map_or(message.len(), |offset| index + 1 + offset + 1);
            pattern.push('_');
            index = close;
            previous_alphanumeric = false;
            continue;
        }
        let bracketed_address = byte == b'['
            && bytes
                .get(index + 1)
                .is_some_and(|b| b.is_ascii_hexdigit() || *b == b':');
        if bracketed_address || (byte.is_ascii_hexdigit() && !previous_alphanumeric) {
            // A run of hex digits, dots and colons at a word boundary: a number, an address,
            // a hash or a version. It is blanked when it holds a digit; a plain word such as
            // `faced` is kept. `[2001:db8::1]:8233` is one run, brackets included.
            let mut end = if bracketed_address { index + 1 } else { index };
            let mut seen_digit = false;
            loop {
                while let Some(b) = bytes
                    .get(end)
                    .filter(|b| b.is_ascii_hexdigit() || matches!(b, b'.' | b':'))
                {
                    seen_digit |= b.is_ascii_digit();
                    end += 1;
                }
                if bracketed_address && bytes.get(end) == Some(&b']') {
                    end += 1;
                    continue;
                }
                break;
            }
            while end > index && matches!(bytes[end - 1], b'.' | b':') {
                end -= 1;
            }
            if seen_digit {
                pattern.push('_');
            } else {
                pattern.push_str(&message[index..end]);
            }
            index = end;
            previous_alphanumeric = true;
            continue;
        }
        if byte.is_ascii_digit() {
            // Digits inside a word (`Nu6`, `v29`).
            let mut end = index;
            while bytes.get(end).is_some_and(u8::is_ascii_digit) {
                end += 1;
            }
            pattern.push('_');
            index = end;
            previous_alphanumeric = true;
            continue;
        }
        let ch = message[index..].chars().next().unwrap_or('\u{fffd}');
        pattern.push(ch);
        index += ch.len_utf8();
        previous_alphanumeric = ch.is_alphanumeric();
    }
    if pattern.chars().count() > PATTERN_CHARS {
        pattern = pattern.chars().take(PATTERN_CHARS).collect();
    }
    pattern
}

/// The message text before its first `key=value` field, trimmed.
pub fn message_text(message: &str) -> &str {
    if starts_with_key(message) {
        return "";
    }
    let end = next_key_start(message).unwrap_or(message.len());
    message[..end].trim()
}

/// Whether `text` begins with a `key=` token.
fn starts_with_key(text: &str) -> bool {
    let bytes = text.as_bytes();
    if !bytes
        .first()
        .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
    {
        return false;
    }
    let end = bytes
        .iter()
        .position(|b| !(b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.')))
        .unwrap_or(bytes.len());
    bytes.get(end) == Some(&b'=')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_stop_at_the_next_key() {
        let message = "chain updates have stalled, state height has not increased for 7 minutes. \
                       Hint: check your clock sync_percent=12.345% current_height=Height(1234567) \
                       network_upgrade=Nu6 time_since_last_state_block=1m 2s target_block_spacing=75s \
                       max_block_spacing=None is_syncer_stopped=false";
        assert_eq!(field(message, "sync_percent"), Some("12.345%"));
        assert_eq!(field(message, "current_height"), Some("Height(1234567)"));
        assert_eq!(field(message, "time_since_last_state_block"), Some("1m 2s"));
        assert_eq!(field(message, "target_block_spacing"), Some("75s"));
        assert_eq!(field(message, "is_syncer_stopped"), Some("false"));
        assert_eq!(field(message, "height"), None, "a key inside another key");
        assert_eq!(field(message, "percent"), None);
        assert_eq!(field(message, ""), None);
    }

    #[test]
    fn fields_at_the_start_and_with_odd_values() {
        assert_eq!(
            field(
                "height=Height(0) hash=block::Hash(\"029f11d8\") log_msg=\"sending\"",
                "hash"
            ),
            Some("block::Hash(\"029f11d8\")")
        );
        assert_eq!(
            field("height=Height(0) hash=x", "height"),
            Some("Height(0)")
        );
        assert_eq!(
            field(
                "verified checkpoint range block_count=400 current_range=(Unbounded, Included(Height(0)))",
                "current_range"
            ),
            Some("(Unbounded, Included(Height(0)))")
        );
        assert_eq!(
            field(
                "an initial peer connection failed successes=11 errors=1 addr=203.0.113.77:8233 e=connection timed out",
                "e"
            ),
            Some("connection timed out")
        );
        assert_eq!(
            field(
                "failed to make outbound connection to peer error=timeout candidate.addr=203.0.113.9:8233",
                "candidate.addr"
            ),
            Some("203.0.113.9:8233")
        );
        assert_eq!(
            field(
                "initial sync is waiting sync_percent=0.000 % current_height=None",
                "sync_percent"
            ),
            Some("0.000 %")
        );
        assert_eq!(
            field("tip_height=Height(5) ", "tip_height"),
            Some("Height(5)")
        );
    }

    #[test]
    fn value_shapes() {
        assert_eq!(height_of("Height(1234567)"), Some(1_234_567));
        assert_eq!(height_of("Some(Height(5))"), Some(5));
        assert_eq!(height_of("None"), None);
        assert_eq!(height_of("Some(None)"), None);
        assert_eq!(height_of("Height(x)"), None);
        assert_eq!(percent_of("12.345%"), Some(12.345));
        assert_eq!(percent_of("0.000 %"), Some(0.0));
        assert_eq!(percent_of("100%"), Some(100.0));
        assert_eq!(percent_of("NaN%"), None);
        assert_eq!(percent_of("lots"), None);
        assert_eq!(u64_of("2000000"), Some(2_000_000));
        assert_eq!(u64_of(" 7 "), Some(7));
        assert_eq!(u64_of("-1"), None);
    }

    #[test]
    fn timestamps() {
        let parsed = parse_timestamp("2026-10-23T18:35:01.937583Z").expect("parses");
        let since = parsed.duration_since(UNIX_EPOCH).unwrap();
        assert_eq!(since.as_secs(), 1_792_780_501);
        assert_eq!(since.subsec_nanos(), 937_583_000);
        assert_eq!(
            parse_timestamp("1970-01-01T00:00:00Z"),
            Some(UNIX_EPOCH),
            "epoch"
        );
        assert_eq!(
            parse_timestamp("2000-03-01T00:00Z").map(epoch_minute),
            Some(951_868_800 / 60),
            "leap-year boundary, no seconds"
        );
        assert_eq!(
            parse_timestamp("1969-12-31T23:59:59Z"),
            None,
            "before the epoch"
        );
        assert_eq!(parse_timestamp("garbage"), None);
        assert_eq!(parse_timestamp("2026-13-01T00:00:00Z"), None);
        assert_eq!(parse_timestamp("2026-10-23T25:00:00Z"), None);
        assert_eq!(parse_timestamp("2026-10-23T18:35:01.9x7Z"), None);
        assert_eq!(parse_timestamp("2026-10-23"), None);
        let minute = epoch_minute(parsed);
        assert_eq!(
            minute_start(minute),
            UNIX_EPOCH + Duration::from_secs(1_792_780_500)
        );
    }

    #[test]
    fn patterns_blank_the_variable_parts() {
        assert_eq!(
            pattern_of(
                "dropping stalled peer: exceeded FindBlocks/FindHeaders stall threshold addr=203.0.113.9:8233"
            ),
            "dropping stalled peer: exceeded FindBlocks/FindHeaders stall threshold addr=_"
        );
        assert_eq!(
            pattern_of(
                "dropping stalled peer: exceeded FindBlocks/FindHeaders stall threshold addr=203.0.113.9:8233"
            ),
            pattern_of(
                "dropping stalled peer: exceeded FindBlocks/FindHeaders stall threshold addr=[2001:db8::1]:8233"
            ),
        );
        assert_eq!(
            pattern_of(
                "estimated progress to chain tip sync_percent=12.345% current_height=Height(1234567) network_upgrade=Nu6 remaining_sync_blocks=2000000 time_since_last_state_block=5s"
            ),
            "estimated progress to chain tip sync_percent=_% current_height=Height(_) network_upgrade=Nu_ remaining_sync_blocks=_ time_since_last_state_block=_s"
        );
        assert_eq!(
            pattern_of(
                r#"height=Height(0) hash=block::Hash("029f11d80ef9765602235e1bc9727e3eb6ba20839319f761fee920d63401e327") log_msg="sending committed block broadcast""#
            ),
            "height=Height(_) hash=block::Hash(_) log_msg=_"
        );
        assert_eq!(pattern_of("git commit: ad6528b"), "git commit: _");
        assert_eq!(
            pattern_of("version: 7.0.0-rc.0+192.gad6528b.modified"),
            "version: _-rc._+_.gad_b.modified"
        );
        assert_eq!(
            pattern_of("faced a decade of bad beef"),
            "faced a decade of bad beef"
        );
        assert_eq!(pattern_of("ended at 5."), "ended at _.");
        assert_eq!(pattern_of("unterminated \"quote"), "unterminated _");
        assert_eq!(pattern_of("héllo 42 wörld"), "héllo _ wörld");
        let long = "x".repeat(300);
        assert_eq!(pattern_of(&long).chars().count(), PATTERN_CHARS);
        let long_multibyte = "é".repeat(300);
        assert_eq!(pattern_of(&long_multibyte).chars().count(), PATTERN_CHARS);
    }

    #[test]
    fn message_text_stops_before_the_fields() {
        assert_eq!(
            message_text(
                "estimated progress to chain tip sync_percent=12.345% current_height=Height(1)"
            ),
            "estimated progress to chain tip"
        );
        assert_eq!(message_text("Starting zebrad"), "Starting zebrad");
        assert_eq!(message_text("height=Height(0) hash=x"), "");
        assert_eq!(
            message_text(
                "chain updates have stalled, state height has not increased for 7 minutes. Hint: check your network connection, and your computer clock and time zone sync_percent=1%"
            ),
            "chain updates have stalled, state height has not increased for 7 minutes. Hint: check your network connection, and your computer clock and time zone"
        );
    }
}
