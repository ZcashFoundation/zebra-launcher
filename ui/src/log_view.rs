//! The log page's data model: a capped, de-duplicated buffer of lines in `seq` order, markers
//! for dropped lines, and the level and text filters. Pure Rust, tested natively.

use std::collections::VecDeque;

use launcher_types::{LogLevel, LogLine, LogStream};

/// How many lines the UI keeps. The backend keeps more; this bounds the DOM.
pub const MAX_LINES: usize = 1000;

/// The most recent [`MAX_LINES`] lines, ordered by `seq` with no duplicates.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LogBuffer {
    lines: VecDeque<LogLine>,
}

impl LogBuffer {
    /// Adds a batch from the live stream or from `log_history`. Lines already present are
    /// ignored, so overlapping history and stream batches merge cleanly.
    pub fn merge(&mut self, batch: Vec<LogLine>) {
        let Some(first) = batch.first() else {
            return;
        };
        let after_newest = self
            .lines
            .back()
            .is_none_or(|newest| first.seq > newest.seq);
        let in_order = batch.windows(2).all(|pair| pair[0].seq < pair[1].seq);
        if after_newest && in_order {
            // The common case: a live batch that starts after everything kept.
            self.lines.extend(batch);
        } else {
            // History overlapping the live stream, or an unordered batch. The sort is stable
            // and the kept lines come first, so a kept line wins over a later copy.
            let mut all: Vec<LogLine> = self.lines.drain(..).chain(batch).collect();
            all.sort_by_key(|line| line.seq);
            all.dedup_by(|later, kept| {
                if later.seq != kept.seq {
                    return false;
                }
                // A real line from `log_history` replaces the gap marker that took its seq.
                if is_gap_marker(kept) && !is_gap_marker(later) {
                    std::mem::swap(later, kept);
                }
                true
            });
            self.lines.extend(all);
        }
        let excess = self.lines.len().saturating_sub(MAX_LINES);
        self.lines.drain(..excess);
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// The lines that pass `filter`, with a [`LogEntry::Gap`] wherever `seq` jumps between two
    /// buffered lines or the backend sent a gap marker (see [`is_gap_marker`]).
    pub fn entries(&self, filter: &LogFilter) -> Vec<LogEntry> {
        let needle = filter.text.trim().to_lowercase();
        let mut entries = Vec::new();
        let mut previous_seq: Option<u64> = None;
        // Continuation lines (no level) follow the visibility of the line they continue.
        let mut parent_visible = true;
        for line in &self.lines {
            if is_gap_marker(line) {
                // The marker takes the seq of the last dropped line, so it and any jump in
                // `seq` before it are one gap. A marker with nothing before it says nothing.
                if let Some(previous) = previous_seq {
                    entries.push(LogEntry::Gap {
                        before_seq: line.seq,
                        dropped: line.seq - previous,
                    });
                }
                previous_seq = Some(line.seq);
                continue;
            }
            if let Some(previous) = previous_seq
                && line.seq > previous + 1
            {
                entries.push(LogEntry::Gap {
                    before_seq: line.seq,
                    dropped: line.seq - previous - 1,
                });
            }
            previous_seq = Some(line.seq);

            let level_ok = match (line.level, line.stream) {
                (Some(level), _) => {
                    parent_visible = filter.min_level.is_none_or(|min| level >= min);
                    parent_visible
                }
                (None, LogStream::Stdout) => parent_visible,
                // stderr and launcher notes are always worth seeing.
                (None, LogStream::Stderr | LogStream::Launcher) => true,
            };
            if level_ok && (needle.is_empty() || matches_text(line, &needle)) {
                entries.push(LogEntry::Line(line.clone()));
            }
        }
        entries
    }
}

/// What the log page shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LogFilter {
    /// Hide leveled lines below this level. `None` shows everything.
    pub min_level: Option<LogLevel>,
    /// Show only lines containing this text, ignoring case. Empty shows everything.
    pub text: String,
}

/// One row of the log view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogEntry {
    Line(LogLine),
    /// `dropped` lines are missing before the line with `before_seq`.
    Gap {
        before_seq: u64,
        dropped: u64,
    },
}

impl LogEntry {
    /// A key that is unique within one [`LogBuffer::entries`] result and stable across updates.
    pub fn key(&self) -> u64 {
        match self {
            LogEntry::Line(line) => line.seq.saturating_mul(2),
            LogEntry::Gap { before_seq, .. } => before_seq.saturating_mul(2).saturating_sub(1),
        }
    }
}

/// Whether `line` is the backend's stand-in for lines it dropped (`bridge::gap_marker` in
/// `src-tauri`): a `Launcher` line reading "N lines dropped" that takes the seq of the last
/// dropped line.
pub fn is_gap_marker(line: &LogLine) -> bool {
    line.stream == LogStream::Launcher
        && line.timestamp.is_none()
        && line
            .message
            .strip_suffix(" lines dropped")
            .is_some_and(|count| !count.is_empty() && count.bytes().all(|b| b.is_ascii_digit()))
}

fn matches_text(line: &LogLine, needle_lowercase: &str) -> bool {
    [
        Some(line.message.as_str()),
        line.target.as_deref(),
        line.spans.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|field| field.to_lowercase().contains(needle_lowercase))
}

/// The line roughly as zebrad printed it, for copying.
pub fn line_text(line: &LogLine) -> String {
    let mut text = String::new();
    if let Some(timestamp) = &line.timestamp {
        text.push_str(timestamp);
        text.push(' ');
    }
    if let Some(level) = line.level {
        text.push_str(&format!("{:>5} ", level.as_str()));
    }
    if let Some(spans) = &line.spans {
        text.push_str(spans);
        text.push_str(": ");
    }
    if let Some(target) = &line.target {
        text.push_str(target);
        text.push_str(": ");
    }
    text.push_str(&line.message);
    text
}

/// The text of the visible entries, one per line, for the "Copy visible" button.
pub fn entries_text(entries: &[LogEntry]) -> String {
    entries
        .iter()
        .map(|entry| match entry {
            LogEntry::Line(line) => line_text(line),
            LogEntry::Gap { dropped, .. } => format!("… {dropped} lines dropped"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The time of day of an RFC 3339 timestamp, to millisecond precision:
/// `2026-10-23T18:35:01.937583Z` becomes `18:35:01.937`. Other text is returned unchanged.
pub fn time_of_day(timestamp: &str) -> &str {
    let Some((_, time)) = timestamp.split_once('T') else {
        return timestamp;
    };
    let time = time.trim_end_matches('Z');
    let Some(dot) = time.find('.') else {
        return time;
    };
    // The dot and three characters after it, counted in characters so that a non-ASCII
    // timestamp cannot split a code point.
    let end = time[dot..]
        .char_indices()
        .nth(4)
        .map_or(time.len(), |(offset, _)| dot + offset);
    &time[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(seq: u64, level: Option<LogLevel>, message: &str) -> LogLine {
        LogLine {
            seq,
            stream: LogStream::Stdout,
            timestamp: level.map(|_| "2026-10-23T18:35:01.937583Z".into()),
            level,
            spans: None,
            target: level.map(|_| "zebrad::commands::start".into()),
            message: message.into(),
        }
    }

    fn seqs(buffer: &LogBuffer) -> Vec<u64> {
        buffer.lines.iter().map(|line| line.seq).collect()
    }

    #[test]
    fn merge_appends_dedups_and_sorts() {
        let mut buffer = LogBuffer::default();
        buffer.merge(vec![line(5, None, "e"), line(6, None, "f")]);
        // History overlapping and preceding the live stream.
        buffer.merge((2..=6).map(|seq| line(seq, None, "h")).collect());
        assert_eq!(seqs(&buffer), [2, 3, 4, 5, 6]);
        // The live line wins over the history copy of the same seq.
        assert_eq!(buffer.lines[3].message, "e");
        buffer.merge(vec![line(9, None, "i")]);
        assert_eq!(seqs(&buffer), [2, 3, 4, 5, 6, 9]);
    }

    #[test]
    fn merge_ignores_an_empty_batch() {
        let mut buffer = LogBuffer::default();
        buffer.merge(Vec::new());
        assert!(buffer.is_empty());
        buffer.merge(vec![line(3, None, "c")]);
        let before = buffer.clone();
        buffer.merge(Vec::new());
        assert_eq!(buffer, before);
    }

    #[test]
    fn merge_appends_a_batch_after_the_newest_line() {
        let mut buffer = LogBuffer::default();
        buffer.merge(vec![line(1, None, "a"), line(2, None, "b")]);
        // A jump in seq is kept as is: it is a gap, not an overlap.
        buffer.merge(vec![line(7, None, "g"), line(8, None, "h")]);
        assert_eq!(seqs(&buffer), [1, 2, 7, 8]);
        assert_eq!(buffer.lines[2].message, "g");
    }

    #[test]
    fn merge_sorts_an_unordered_batch() {
        let mut buffer = LogBuffer::default();
        buffer.merge(vec![line(1, None, "a")]);
        buffer.merge(vec![
            line(4, None, "d"),
            line(3, None, "c"),
            line(4, None, "x"),
        ]);
        assert_eq!(seqs(&buffer), [1, 3, 4]);
        assert_eq!(buffer.lines[2].message, "d");
        // A batch entirely older than the kept lines is sorted in front of them.
        buffer.merge(vec![line(0, None, "z")]);
        assert_eq!(seqs(&buffer), [0, 1, 3, 4]);
    }

    #[test]
    fn merge_caps_the_buffer() {
        let mut buffer = LogBuffer::default();
        for start in (0..1500).step_by(100) {
            buffer.merge(
                (start..start + 100)
                    .map(|seq| line(seq, None, "x"))
                    .collect(),
            );
        }
        assert_eq!(buffer.len(), MAX_LINES);
        assert_eq!(buffer.lines.front().map(|line| line.seq), Some(500));
    }

    #[test]
    fn entries_mark_gaps_with_unique_keys() {
        let mut buffer = LogBuffer::default();
        buffer.merge(vec![line(10, None, "a"), line(11, None, "b")]);
        buffer.merge(vec![line(15, None, "c")]);
        let entries = buffer.entries(&LogFilter::default());
        assert_eq!(entries.len(), 4);
        assert_eq!(
            entries[2],
            LogEntry::Gap {
                before_seq: 15,
                dropped: 3
            }
        );
        let mut keys: Vec<u64> = entries.iter().map(LogEntry::key).collect();
        keys.dedup();
        assert_eq!(keys.len(), 4);
        assert!(keys.windows(2).all(|pair| pair[0] < pair[1]));
    }

    fn gap_marker(seq: u64, dropped: u64) -> LogLine {
        LogLine {
            seq,
            stream: LogStream::Launcher,
            timestamp: None,
            level: Some(LogLevel::Warn),
            spans: None,
            target: None,
            message: format!("{dropped} lines dropped"),
        }
    }

    #[test]
    fn a_gap_marker_and_the_jump_before_it_are_one_gap() {
        // What the bridge sends after lagging by 3: lines 2, 3 and 4 are lost and the marker
        // takes seq 4.
        let mut buffer = LogBuffer::default();
        buffer.merge(vec![line(0, None, "a"), line(1, None, "b")]);
        buffer.merge(vec![gap_marker(4, 3), line(5, None, "f")]);
        let entries = buffer.entries(&LogFilter::default());
        assert_eq!(entries.len(), 4);
        assert_eq!(
            entries[2],
            LogEntry::Gap {
                before_seq: 4,
                dropped: 3
            }
        );
        assert!(matches!(&entries[3], LogEntry::Line(line) if line.seq == 5));

        // log_history fills in 3 and 4: the real line 4 replaces the marker and one line is
        // still missing.
        buffer.merge(vec![line(3, None, "d"), line(4, None, "e")]);
        assert_eq!(seqs(&buffer), [0, 1, 3, 4, 5]);
        assert_eq!(buffer.lines[3].message, "e");
        let entries = buffer.entries(&LogFilter::default());
        assert_eq!(
            entries[2],
            LogEntry::Gap {
                before_seq: 3,
                dropped: 1
            }
        );
        assert_eq!(entries.len(), 6);
    }

    #[test]
    fn only_dropped_line_notes_are_gap_markers() {
        assert!(is_gap_marker(&gap_marker(9, 7)));
        let mut note = gap_marker(9, 7);
        note.message = "zebrad exited with code 0".into();
        assert!(!is_gap_marker(&note));
        let mut stdout = gap_marker(9, 7);
        stdout.stream = LogStream::Stdout;
        assert!(!is_gap_marker(&stdout));
        note.message = " lines dropped".into();
        assert!(!is_gap_marker(&note));
    }

    #[test]
    fn level_filter_hides_continuations_of_hidden_lines() {
        let mut buffer = LogBuffer::default();
        let mut stderr = line(4, None, "panicked");
        stderr.stream = LogStream::Stderr;
        buffer.merge(vec![
            line(0, Some(LogLevel::Info), "Diagnostic metadata:"),
            line(1, None, "version: 7.0.0"),
            line(2, Some(LogLevel::Warn), "careful"),
            line(3, None, "more detail"),
            stderr,
        ]);
        let filter = LogFilter {
            min_level: Some(LogLevel::Warn),
            text: String::new(),
        };
        let shown: Vec<u64> = buffer
            .entries(&filter)
            .iter()
            .map(|entry| match entry {
                LogEntry::Line(line) => line.seq,
                LogEntry::Gap { .. } => u64::MAX,
            })
            .collect();
        assert_eq!(shown, [2, 3, 4]);
    }

    #[test]
    fn text_filter_ignores_case_and_searches_target() {
        let mut buffer = LogBuffer::default();
        buffer.merge(vec![
            line(0, Some(LogLevel::Info), "Starting zebrad"),
            line(1, Some(LogLevel::Info), "opening database"),
        ]);
        let filter = |text: &str| LogFilter {
            min_level: None,
            text: text.into(),
        };
        assert_eq!(buffer.entries(&filter("STARTING")).len(), 1);
        assert_eq!(buffer.entries(&filter("commands::start")).len(), 2);
        assert_eq!(buffer.entries(&filter("nothing")).len(), 0);
    }

    #[test]
    fn line_text_and_time_of_day() {
        let line = line(0, Some(LogLevel::Info), "Starting zebrad");
        assert_eq!(
            line_text(&line),
            "2026-10-23T18:35:01.937583Z  INFO zebrad::commands::start: Starting zebrad"
        );
        assert_eq!(time_of_day("2026-10-23T18:35:01.937583Z"), "18:35:01.937");
        assert_eq!(time_of_day("2026-10-23T18:35:01Z"), "18:35:01");
        assert_eq!(time_of_day("garbage"), "garbage");
        assert_eq!(time_of_day("2026-10-23T18:35:01.9Z"), "18:35:01.9");
        // Multi-byte characters right after the dot must not panic.
        assert_eq!(time_of_day("xT01.éèêëZ"), "01.éèê");
        assert_eq!(time_of_day("T.€"), ".€");
        assert_eq!(time_of_day("日本T時.間"), "時.間");
    }
}
