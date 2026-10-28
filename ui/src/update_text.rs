//! The words for launcher updates: the update modes, the updater's state, the download
//! progress and the release-notes excerpt. Pure Rust, tested natively.

use std::time::SystemTime;

use launcher_types::{UpdateMode, UpdateState};

use crate::format::{bytes, relative};

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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

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
}
