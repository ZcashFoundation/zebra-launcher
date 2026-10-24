//! Pure helpers for the Configure page's file pane: which lines of the draft differ from the
//! saved file, where a key is written, how far to scroll to show a line, and the words for the
//! summary. Tested natively.

/// One line of the draft as the file pane shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineChange {
    /// 1-based, as the gutter shows it.
    pub number: usize,
    pub text: String,
    /// The line has no match in the saved file: it was added or edited.
    pub changed: bool,
}

/// Above this many cells the line matching falls back to comparing lines by position, so a
/// pathological pair of files cannot take seconds per keystroke.
const LCS_CELL_LIMIT: usize = 1_000_000;

/// The draft's lines against the saved file's, and how many saved lines the draft dropped.
/// Lines are matched as a longest common subsequence, so an inserted line does not mark every
/// line after it as changed; `changed` means the draft line is matched to no saved line, and
/// the count is the saved lines matched to no draft line.
pub fn line_changes(saved: &str, draft: &str) -> (Vec<LineChange>, usize) {
    let saved: Vec<&str> = saved.lines().collect();
    let draft: Vec<&str> = draft.lines().collect();
    let matched = if saved.len().saturating_mul(draft.len()) > LCS_CELL_LIMIT {
        positional_matches(&saved, &draft)
    } else {
        lcs_matches(&saved, &draft)
    };
    let matched_count = matched.iter().filter(|matched| **matched).count();
    let lines = draft
        .iter()
        .zip(matched)
        .enumerate()
        .map(|(index, (text, matched))| LineChange {
            number: index + 1,
            text: (*text).to_owned(),
            changed: !matched,
        })
        .collect();
    (lines, saved.len() - matched_count)
}

/// For each draft line, whether a longest common subsequence of the two files matches it to a
/// saved line.
fn lcs_matches(saved: &[&str], draft: &[&str]) -> Vec<bool> {
    let (n, m) = (saved.len(), draft.len());
    // `table[i][j]`: the length of the longest common subsequence of `saved[i..]` and
    // `draft[j..]`, as a flat `(n + 1) * (m + 1)` array.
    let width = m + 1;
    let mut table = vec![0u32; (n + 1) * width];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i * width + j] = if saved[i] == draft[j] {
                table[(i + 1) * width + j + 1] + 1
            } else {
                table[(i + 1) * width + j].max(table[i * width + j + 1])
            };
        }
    }
    let mut matched = vec![false; m];
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if saved[i] == draft[j] {
            matched[j] = true;
            i += 1;
            j += 1;
        } else if table[(i + 1) * width + j] >= table[i * width + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    matched
}

/// For each draft line, whether the saved line at the same position is the same.
fn positional_matches(saved: &[&str], draft: &[&str]) -> Vec<bool> {
    draft
        .iter()
        .enumerate()
        .map(|(index, line)| saved.get(index) == Some(line))
        .collect()
}

/// The 1-based line where `key` is set inside `[section]`, if any. A header names its table up
/// to the closing bracket, so `[a.b]` is the section `a.b` and not `a`; comments are skipped,
/// and the key may be followed by spaces before the `=`.
pub fn key_line(text: &str, section: &str, key: &str) -> Option<usize> {
    let mut current = "";
    for (index, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if let Some(header) = trimmed.strip_prefix('[') {
            current = header
                .trim_start_matches('[')
                .split(']')
                .next()
                .unwrap_or_default()
                .trim();
            continue;
        }
        if current != section || trimmed.starts_with('#') {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix(key)
            && rest.trim_start().starts_with('=')
        {
            return Some(index + 1);
        }
    }
    None
}

/// The scroll position that brings a line into view with the least movement, or `None` when it
/// is in view already: `top` and `height` are the line's offsets within the scrolling pane,
/// `view_top` and `view_height` the pane's scroll position and visible height.
pub fn scroll_to_reveal(top: i32, height: i32, view_top: i32, view_height: i32) -> Option<i32> {
    if top < view_top {
        Some(top)
    } else if top + height > view_top + view_height {
        Some(top + height - view_height)
    } else {
        None
    }
}

/// The file pane's summary of the draft against the saved file.
pub fn change_summary(changed: usize, removed: usize) -> String {
    let plural = |count: usize| if count == 1 { "line" } else { "lines" };
    match (changed, removed) {
        (0, 0) => "matches the saved file".to_owned(),
        (changed, 0) => format!("{changed} {} changed", plural(changed)),
        (0, removed) => format!("{removed} {} removed", plural(removed)),
        (changed, removed) => format!("{changed} {} changed, {removed} removed", plural(changed)),
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

    const SAVED: &str = "# head\n\n[network]\nnetwork = \"Mainnet\"\nlisten_addr = \"[::]:8233\"\n\n[rpc]\nlisten_addr = \"127.0.0.1:8232\"\n";

    fn changed_numbers(lines: &[LineChange]) -> Vec<usize> {
        lines
            .iter()
            .filter(|line| line.changed)
            .map(|line| line.number)
            .collect()
    }

    #[test]
    fn an_unchanged_draft_has_no_marks() {
        let (lines, removed) = line_changes(SAVED, SAVED);
        assert_eq!(lines.len(), 8);
        assert_eq!(changed_numbers(&lines), Vec::<usize>::new());
        assert_eq!(removed, 0);
        assert_eq!(lines[0].number, 1);
        assert_eq!(lines[2].text, "[network]");
    }

    #[test]
    fn an_inserted_line_marks_only_itself() {
        let draft = SAVED.replace(
            "listen_addr = \"[::]:8233\"\n",
            "listen_addr = \"[::]:8233\"\npeerset_initial_target_size = 30\n",
        );
        let (lines, removed) = line_changes(SAVED, &draft);
        assert_eq!(lines.len(), 9);
        assert_eq!(changed_numbers(&lines), vec![6]);
        assert_eq!(lines[5].text, "peerset_initial_target_size = 30");
        assert_eq!(removed, 0);
    }

    #[test]
    fn an_edited_line_counts_as_changed_and_removed() {
        let draft = SAVED.replace("Mainnet", "Testnet");
        let (lines, removed) = line_changes(SAVED, &draft);
        assert_eq!(changed_numbers(&lines), vec![4]);
        assert_eq!(removed, 1);
    }

    #[test]
    fn a_dropped_line_is_counted_not_drawn() {
        let draft = SAVED.replace("listen_addr = \"[::]:8233\"\n", "");
        let (lines, removed) = line_changes(SAVED, &draft);
        assert_eq!(lines.len(), 7);
        assert!(changed_numbers(&lines).is_empty());
        assert_eq!(removed, 1);
        // A dropped section header and key.
        let draft = SAVED.replace("\n[rpc]\nlisten_addr = \"127.0.0.1:8232\"\n", "");
        let (lines, removed) = line_changes(SAVED, &draft);
        assert_eq!(lines.len(), 5);
        assert_eq!(removed, 3);
    }

    #[test]
    fn empty_texts() {
        assert_eq!(line_changes("", ""), (Vec::new(), 0));
        let (lines, removed) = line_changes("", "a = 1\n");
        assert_eq!(changed_numbers(&lines), vec![1]);
        assert_eq!(removed, 0);
        let (lines, removed) = line_changes("a = 1\nb = 2\n", "");
        assert!(lines.is_empty());
        assert_eq!(removed, 2);
    }

    #[test]
    fn repeated_lines_match_in_order() {
        // Blank lines and repeated keys in different sections must not cross-match in a way
        // that marks the untouched section.
        let draft = SAVED.replace("[::]:8233", "0.0.0.0:8233");
        let (lines, removed) = line_changes(SAVED, &draft);
        assert_eq!(changed_numbers(&lines), vec![5]);
        assert_eq!(removed, 1);
    }

    #[test]
    fn large_inputs_fall_back_to_positions() {
        let saved: String = (0..1100).map(|i| format!("k{i} = {i}\n")).collect();
        let saved_lines: Vec<&str> = saved.lines().collect();
        let draft_lines: Vec<&str> = saved_lines.clone();
        assert!(saved_lines.len() * draft_lines.len() > LCS_CELL_LIMIT);
        let (lines, removed) = line_changes(&saved, &saved);
        assert!(changed_numbers(&lines).is_empty());
        assert_eq!(removed, 0);
        let mut draft = saved.clone();
        draft.insert_str(0, "inserted = true\n");
        let (lines, removed) = line_changes(&saved, &draft);
        // Positional matching marks everything after the insertion; it only has to finish.
        assert_eq!(lines.len(), 1101);
        assert!(lines[0].changed);
        assert_eq!(removed, 1100);
        // The small case keeps the exact matching.
        let positional = positional_matches(&["a", "b"], &["x", "a", "b"]);
        assert_eq!(positional, vec![false, false, false]);
        let exact = lcs_matches(&["a", "b"], &["x", "a", "b"]);
        assert_eq!(exact, vec![false, true, true]);
    }

    #[test]
    fn key_lines_are_found_inside_their_section() {
        assert_eq!(key_line(SAVED, "network", "listen_addr"), Some(5));
        assert_eq!(key_line(SAVED, "rpc", "listen_addr"), Some(8));
        assert_eq!(key_line(SAVED, "network", "network"), Some(4));
        assert_eq!(key_line(SAVED, "rpc", "network"), None);
        assert_eq!(key_line(SAVED, "state", "cache_dir"), None);
    }

    #[test]
    fn key_lines_handle_spacing_comments_and_dotted_headers() {
        let text =
            "[a]\nx=1\n  # x = 2\n  y   = 3\n[a.b]\nx = 4\n[[c]]\nx = 5\n[ d ]\nxy = 6\nx = 7\n";
        assert_eq!(key_line(text, "a", "x"), Some(2));
        assert_eq!(key_line(text, "a", "y"), Some(4));
        assert_eq!(key_line(text, "a.b", "x"), Some(6));
        assert_eq!(key_line(text, "c", "x"), Some(8));
        // `xy` is not `x`.
        assert_eq!(key_line(text, "d", "x"), Some(11));
        // A root key has no section.
        assert_eq!(key_line("title = \"t\"\n[a]\n", "", "title"), Some(1));
        assert_eq!(key_line("", "a", "x"), None);
    }

    #[test]
    fn scrolling_moves_the_least() {
        // In view: nothing to do.
        assert_eq!(scroll_to_reveal(100, 20, 50, 300), None);
        assert_eq!(scroll_to_reveal(50, 20, 50, 300), None);
        assert_eq!(scroll_to_reveal(330, 20, 50, 300), None);
        // Above the view: the line goes to the top edge.
        assert_eq!(scroll_to_reveal(30, 20, 50, 300), Some(30));
        // Below: the line goes to the bottom edge.
        assert_eq!(scroll_to_reveal(340, 20, 50, 300), Some(60));
        assert_eq!(scroll_to_reveal(1000, 18, 0, 300), Some(718));
    }

    #[test]
    fn summaries() {
        assert_eq!(change_summary(0, 0), "matches the saved file");
        assert_eq!(change_summary(1, 0), "1 line changed");
        assert_eq!(change_summary(3, 0), "3 lines changed");
        assert_eq!(change_summary(0, 1), "1 line removed");
        assert_eq!(change_summary(0, 2), "2 lines removed");
        assert_eq!(change_summary(3, 1), "3 lines changed, 1 removed");
        assert_eq!(change_summary(1, 2), "1 line changed, 2 removed");
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
