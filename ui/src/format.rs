//! Display helpers and input validation. Pure Rust, tested natively.

use std::{
    net::Ipv4Addr,
    time::{Duration, SystemTime},
};

use launcher_types::{ExitClass, Phase};

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

/// Checks a Grafana URL from the settings form. Empty means "no dashboard".
///
/// Only `http://127.0.0.1[:port]` and `http://localhost[:port]` are accepted: the app's CSP
/// allows exactly those as frame sources, and Grafana must not be loaded from another host.
pub fn validate_grafana_url(input: &str) -> Result<Option<String>, String> {
    let url = input.trim();
    if url.is_empty() {
        return Ok(None);
    }
    let Some(rest) = url.strip_prefix("http://") else {
        return Err("The URL must start with http:// (Grafana runs on this computer).".into());
    };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    if authority.contains('@') {
        return Err("The URL must not contain a user name or password.".into());
    }
    let (host, port) = split_host_port(authority)?;
    if !is_allowed_host(host) {
        return Err("Grafana must run on this computer: use 127.0.0.1 or localhost.".into());
    }
    if let Some(port) = port
        && !matches!(port.parse::<u16>(), Ok(1..))
    {
        return Err(format!("{port:?} is not a valid port."));
    }
    if url.chars().any(char::is_whitespace) {
        return Err("The URL must not contain spaces.".into());
    }
    Ok(Some(url.to_owned()))
}

fn split_host_port(authority: &str) -> Result<(&str, Option<&str>), String> {
    if let Some(bracketed) = authority.strip_prefix('[') {
        let Some((host, after)) = bracketed.split_once(']') else {
            return Err("The URL has an unterminated IPv6 address.".into());
        };
        return match after {
            "" => Ok((host, None)),
            _ => match after.strip_prefix(':') {
                Some(port) => Ok((host, Some(port))),
                None => Err("The URL has an invalid host.".into()),
            },
        };
    }
    Ok(match authority.split_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (authority, None),
    })
}

fn is_allowed_host(host: &str) -> bool {
    // Keep in step with the CSP's frame-src allow-list (`http://127.0.0.1:* http://localhost:*`).
    host.eq_ignore_ascii_case("localhost") || host.parse::<Ipv4Addr>() == Ok(Ipv4Addr::LOCALHOST)
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
    fn grafana_url_must_be_loopback_http() {
        let ok = |url: &str| validate_grafana_url(url).unwrap();
        assert_eq!(ok(""), None);
        assert_eq!(ok("  "), None);
        assert_eq!(
            ok(" http://127.0.0.1:3000/d/zebra-overview "),
            Some("http://127.0.0.1:3000/d/zebra-overview".into())
        );
        assert_eq!(ok("http://localhost"), Some("http://localhost".into()));
        assert_eq!(
            ok("http://LOCALHOST:3000?orgId=1"),
            Some("http://LOCALHOST:3000?orgId=1".into())
        );
        for bad in [
            "https://127.0.0.1:3000",
            "http://grafana.example.com",
            "http://127.0.0.1.example.com:3000",
            "http://example.com@127.0.0.1:3000",
            "http://127.0.0.1@example.com",
            "http://10.0.0.1:3000",
            "http://127.0.0.2:3000",
            "http://[::1]:3000",
            "http://127.0.0.1:0",
            "http://127.0.0.1:99999",
            "http://127.0.0.1:abc",
            "javascript:alert(1)",
            "127.0.0.1:3000",
        ] {
            assert!(validate_grafana_url(bad).is_err(), "{bad} was accepted");
        }
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
