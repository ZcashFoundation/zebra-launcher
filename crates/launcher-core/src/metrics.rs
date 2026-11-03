//! zebrad's Prometheus metrics: a parser for the text exposition format its
//! `/metrics` endpoint serves, and the poller that reads the page while zebrad
//! runs ([`poller`]).
//!
//! The format (<https://prometheus.io/docs/instrumenting/exposition_formats/>):
//!
//! ```text
//! # HELP zcash_net_peers The number of connected peers.
//! # TYPE zcash_net_peers gauge
//! zcash_net_peers{network="regtest"} 0
//! sync_block_verify_duration_seconds_bucket{le="+Inf"} 12 1700000000000
//! ```
//!
//! `# HELP` and `# TYPE` describe a family; other comments are ignored. A
//! sample is a name, optional labels and a value, optionally followed by a
//! timestamp in milliseconds, which is dropped. zebrad registers dotted names
//! (`zcash.net.peers`) and its exporter writes them with underscores; the
//! names are kept as they appear on the page. That exporter writes `# TYPE`
//! lines only, renders histograms as summaries, and writes `NaN` for a gauge
//! that was never set. The fixture `tests/fixtures/zebrad-7.0.0-metrics.txt`
//! pins a real page.

use std::{collections::HashMap, time::SystemTime};

use launcher_types::{MetricFamily, MetricKind, MetricSample, MetricsSnapshot};

pub mod poller;

/// A line of the page could not be parsed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("metrics line {line}: {reason}")]
pub struct ParseError {
    /// The line number, counting from 1.
    pub line: usize,
    /// What is wrong with it.
    pub reason: String,
}

/// Parses a `/metrics` page read at `at`.
///
/// Families appear in the order of their first `# HELP`, `# TYPE` or sample
/// line. A sample belongs to the family of its name; `_bucket`, `_sum` and
/// `_count` samples belong to the histogram or summary family they extend;
/// any other sample without a `# TYPE` line gets an [`MetricKind::Untyped`]
/// family of its own. An unknown type is read as `untyped`, so a page from a
/// newer exporter still parses.
pub fn parse(text: &str, at: SystemTime) -> Result<MetricsSnapshot, ParseError> {
    let mut page = Page::default();
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let number = index + 1;
        let result = match line.strip_prefix('#') {
            Some(comment) => page.comment(comment.trim_start()),
            None => parse_sample(line).map(|sample| page.sample(sample)),
        };
        result.map_err(|reason| ParseError {
            line: number,
            reason,
        })?;
    }
    Ok(MetricsSnapshot {
        at,
        families: page.families,
    })
}

/// The families of a page while it is parsed.
#[derive(Default)]
struct Page {
    families: Vec<MetricFamily>,
    /// Family name to its index in `families`.
    index: HashMap<String, usize>,
}

impl Page {
    /// The family called `name`, created as untyped when new.
    fn family(&mut self, name: &str) -> usize {
        if let Some(&index) = self.index.get(name) {
            return index;
        }
        self.families.push(MetricFamily {
            name: name.to_string(),
            kind: MetricKind::Untyped,
            help: None,
            samples: Vec::new(),
        });
        let index = self.families.len() - 1;
        self.index.insert(name.to_string(), index);
        index
    }

    /// Applies a `HELP` or `TYPE` comment; any other comment is ignored.
    fn comment(&mut self, comment: &str) -> Result<(), String> {
        let Some((keyword, rest)) = comment.split_once(char::is_whitespace) else {
            return Ok(());
        };
        match keyword {
            "HELP" => {
                let (name, text) = split_first_word(rest.trim_start());
                check_metric_name(name)?;
                let family = self.family(name);
                self.families[family].help = Some(unescape_help(text));
            }
            "TYPE" => {
                let (name, kind) = split_first_word(rest.trim_start());
                check_metric_name(name)?;
                let family = self.family(name);
                self.families[family].kind = parse_kind(kind);
            }
            _ => {}
        }
        Ok(())
    }

    /// Adds a sample to its family.
    fn sample(&mut self, sample: MetricSample) {
        let family = self.family_for(&sample.name);
        self.families[family].samples.push(sample);
    }

    /// The family a sample called `name` belongs to.
    fn family_for(&mut self, name: &str) -> usize {
        if let Some(&index) = self.index.get(name) {
            return index;
        }
        for suffix in ["_bucket", "_sum", "_count"] {
            if let Some(base) = name.strip_suffix(suffix)
                && let Some(&index) = self.index.get(base)
                && matches!(
                    self.families[index].kind,
                    MetricKind::Histogram | MetricKind::Summary
                )
            {
                return index;
            }
        }
        self.family(name)
    }
}

/// Splits `text` at its first whitespace: the word and the rest, trimmed.
fn split_first_word(text: &str) -> (&str, &str) {
    match text.split_once(char::is_whitespace) {
        Some((word, rest)) => (word, rest.trim()),
        None => (text, ""),
    }
}

/// `counter`, `gauge`, `histogram`, `summary` or `untyped`; anything else
/// reads as untyped.
fn parse_kind(kind: &str) -> MetricKind {
    match kind {
        "counter" => MetricKind::Counter,
        "gauge" => MetricKind::Gauge,
        "histogram" => MetricKind::Histogram,
        "summary" => MetricKind::Summary,
        _ => MetricKind::Untyped,
    }
}

/// Undoes the escapes of a `HELP` text: `\\` and `\n`.
fn unescape_help(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn is_name_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || c == ':'
}

fn is_name_char(c: char) -> bool {
    is_name_start(c) || c.is_ascii_digit()
}

/// Checks a metric name: `[a-zA-Z_:][a-zA-Z0-9_:]*`.
fn check_metric_name(name: &str) -> Result<(), String> {
    let mut chars = name.chars();
    match chars.next() {
        None => return Err("missing metric name".to_string()),
        Some(first) if !is_name_start(first) => {
            return Err(format!("invalid metric name {name:?}"));
        }
        Some(_) => {}
    }
    if chars.all(is_name_char) {
        Ok(())
    } else {
        Err(format!("invalid metric name {name:?}"))
    }
}

/// Parses `name[{labels}] value [timestamp]`.
fn parse_sample(line: &str) -> Result<MetricSample, String> {
    let name_end = line.find(|c: char| !is_name_char(c)).unwrap_or(line.len());
    let (name, rest) = line.split_at(name_end);
    check_metric_name(name)?;
    let rest = rest.trim_start();
    let (labels, rest) = match rest.strip_prefix('{') {
        Some(rest) => parse_labels(rest)?,
        None => (Vec::new(), rest),
    };
    let mut tokens = rest.split_whitespace();
    let value = tokens
        .next()
        .ok_or_else(|| format!("{name} has no value"))?;
    let value = value
        .parse::<f64>()
        .map_err(|_| format!("{name} has an invalid value {value:?}"))?;
    if let Some(timestamp) = tokens.next() {
        let digits = timestamp.strip_prefix('-').unwrap_or(timestamp);
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(format!("{name} has an invalid timestamp {timestamp:?}"));
        }
    }
    if tokens.next().is_some() {
        return Err(format!("unexpected text after the sample {name}"));
    }
    Ok(MetricSample {
        name: name.to_string(),
        labels,
        value,
    })
}

/// Label pairs in page order.
type Labels = Vec<(String, String)>;

/// Parses `name="value",...}` (after the opening brace) and returns the labels
/// and the text after the closing brace.
fn parse_labels(mut text: &str) -> Result<(Labels, &str), String> {
    let mut labels = Vec::new();
    loop {
        text = text.trim_start();
        if let Some(rest) = text.strip_prefix('}') {
            return Ok((labels, rest));
        }
        let name_end = text.find(|c: char| !is_name_char(c)).unwrap_or(text.len());
        let (name, rest) = text.split_at(name_end);
        check_metric_name(name).map_err(|_| match text.chars().next() {
            Some(_) => format!("invalid label name in {text:?}"),
            None => "unterminated labels".to_string(),
        })?;
        let rest = rest
            .trim_start()
            .strip_prefix('=')
            .ok_or_else(|| format!("expected `=` after the label {name}"))?;
        let rest = rest
            .trim_start()
            .strip_prefix('"')
            .ok_or_else(|| format!("expected a quoted value for the label {name}"))?;
        let (value, rest) =
            parse_quoted(rest).ok_or_else(|| format!("unterminated value for the label {name}"))?;
        labels.push((name.to_string(), value));
        let rest = rest.trim_start();
        if let Some(rest) = rest.strip_prefix(',') {
            text = rest;
        } else if let Some(rest) = rest.strip_prefix('}') {
            return Ok((labels, rest));
        } else {
            return Err(format!("expected `,` or `}}` after the label {name}"));
        }
    }
}

/// Parses a label value after its opening quote, undoing `\\`, `\"` and `\n`.
/// Returns the value and the text after the closing quote; `None` when the
/// quote never closes.
fn parse_quoted(text: &str) -> Option<(String, &str)> {
    let mut value = String::new();
    let mut chars = text.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Some((value, &text[i + 1..])),
            '\\' => match chars.next()? {
                (_, 'n') => value.push('\n'),
                (_, '\\') => value.push('\\'),
                (_, '"') => value.push('"'),
                (_, other) => {
                    value.push('\\');
                    value.push(other);
                }
            },
            c => value.push(c),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::time::UNIX_EPOCH;

    use super::*;

    const FIXTURE: &str = include_str!("../tests/fixtures/zebrad-7.0.0-metrics.txt");

    fn parse_ok(text: &str) -> MetricsSnapshot {
        parse(text, UNIX_EPOCH).expect("parses")
    }

    fn labels(sample: &MetricSample) -> Vec<(&str, &str)> {
        sample
            .labels
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect()
    }

    #[test]
    fn parses_the_zebrad_fixture() {
        let snapshot = parse_ok(FIXTURE);
        assert_eq!(snapshot.at, UNIX_EPOCH);
        assert_eq!(snapshot.families.len(), 57);
        let samples: usize = snapshot.families.iter().map(|f| f.samples.len()).sum();
        assert_eq!(samples, 136);
        // Every family has a TYPE line on this page.
        assert!(
            snapshot
                .families
                .iter()
                .all(|f| f.kind != MetricKind::Untyped && f.help.is_none()),
            "{:?}",
            snapshot
                .families
                .iter()
                .map(|f| &f.name)
                .collect::<Vec<_>>()
        );

        // A labelled gauge is still the family's single value.
        let peers = snapshot.family("zcash_net_peers").expect("zcash_net_peers");
        assert_eq!(peers.kind, MetricKind::Gauge);
        assert_eq!(peers.samples.len(), 1);
        assert_eq!(labels(&peers.samples[0]), vec![("network", "regtest")]);
        assert_eq!(snapshot.gauge("zcash_net_peers"), Some(0.0));
        assert_eq!(
            snapshot.counter("zcash_net_peers"),
            None,
            "a gauge is not a counter"
        );

        // Counters, with and without labels, and the total over labels.
        assert_eq!(
            snapshot.counter("zcash_chain_verified_block_total"),
            Some(1.0)
        );
        assert_eq!(snapshot.gauge("zcash_chain_verified_block_total"), None);
        let requests = snapshot.family("state_requests").expect("state_requests");
        assert_eq!(requests.kind, MetricKind::Counter);
        assert_eq!(requests.samples.len(), 8);
        assert_eq!(
            snapshot.counter("state_requests"),
            None,
            "eight labelled samples"
        );
        assert_eq!(snapshot.total("state_requests"), Some(9.0));
        assert_eq!(
            labels(&requests.samples[0]),
            vec![("service", "state"), ("type", "best_chain_block_hash")]
        );
        let build = snapshot.family("zebrad_build_info").expect("build info");
        assert_eq!(labels(&build.samples[0]), vec![("version", "7.0.0-rc.0")]);

        // A gauge zebrad never set.
        assert!(
            snapshot
                .gauge("checkpoint_queued_max_height")
                .is_some_and(f64::is_nan)
        );

        // One gauge per RocksDB column family.
        let disk = snapshot
            .family("zebra_state_rocksdb_cf_disk_size_bytes")
            .expect("cf disk sizes");
        assert_eq!(disk.samples.len(), 30);
        assert_eq!(
            snapshot.total("zebra_state_rocksdb_cf_disk_size_bytes"),
            Some(0.0)
        );
        assert_eq!(
            snapshot.total("zebra_state_rocksdb_cf_memory_size_bytes"),
            Some(30.0 * 2048.0)
        );

        // The summary: seven quantiles, the sum and the count in one family.
        let commits = snapshot
            .family("zebra_state_rocksdb_batch_commit_duration_seconds")
            .expect("summary");
        assert_eq!(commits.kind, MetricKind::Summary);
        assert_eq!(commits.samples.len(), 9);
        let quantiles: Vec<&str> = commits
            .samples
            .iter()
            .filter_map(|s| s.labels.iter().find(|(k, _)| k == "quantile"))
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(quantiles, ["0", "0.5", "0.9", "0.95", "0.99", "0.999", "1"]);
        let count = commits
            .samples
            .iter()
            .find(|s| s.name == "zebra_state_rocksdb_batch_commit_duration_seconds_count")
            .expect("count");
        assert_eq!(count.value, 1.0);
        assert!(count.labels.is_empty());
        assert_eq!(
            snapshot.total("zebra_state_rocksdb_batch_commit_duration_seconds"),
            None,
            "no total for a summary"
        );
        assert!(
            snapshot
                .family("zebra_state_rocksdb_batch_commit_duration_seconds_sum")
                .is_none(),
            "the sum is not a family of its own"
        );

        // Nothing on the page is unknown to the snapshot.
        assert_eq!(snapshot.family("nope"), None);
        assert_eq!(snapshot.gauge("nope"), None);
        assert_eq!(snapshot.total("nope"), None);
    }

    #[test]
    fn histogram_buckets_sum_and_count_join_their_family() {
        let snapshot = parse_ok(
            "# HELP sync_block_verify_duration_seconds Time to verify a block.\n\
             # TYPE sync_block_verify_duration_seconds histogram\n\
             sync_block_verify_duration_seconds_bucket{le=\"0.005\"} 0\n\
             sync_block_verify_duration_seconds_bucket{le=\"0.5\"} 7\n\
             sync_block_verify_duration_seconds_bucket{le=\"+Inf\"} 9\n\
             sync_block_verify_duration_seconds_sum 1.25\n\
             sync_block_verify_duration_seconds_count 9\n",
        );
        assert_eq!(snapshot.families.len(), 1);
        let family = &snapshot.families[0];
        assert_eq!(family.name, "sync_block_verify_duration_seconds");
        assert_eq!(family.kind, MetricKind::Histogram);
        assert_eq!(family.help.as_deref(), Some("Time to verify a block."));
        assert_eq!(family.samples.len(), 5);
        assert_eq!(
            family.samples[2].name,
            "sync_block_verify_duration_seconds_bucket"
        );
        assert_eq!(labels(&family.samples[2]), vec![("le", "+Inf")]);
        assert_eq!(family.samples[2].value, 9.0);
        assert_eq!(
            family.samples[3].name,
            "sync_block_verify_duration_seconds_sum"
        );
        assert_eq!(family.samples[3].value, 1.25);
        assert_eq!(snapshot.gauge("sync_block_verify_duration_seconds"), None);
        assert_eq!(snapshot.total("sync_block_verify_duration_seconds"), None);
    }

    #[test]
    fn suffixes_only_join_histograms_and_summaries() {
        // `_count` on a counter is a name of its own.
        let snapshot = parse_ok(
            "# TYPE sync_downloaded_block counter\n\
             sync_downloaded_block 3\n\
             sync_downloaded_block_count 4\n",
        );
        assert_eq!(snapshot.families.len(), 2);
        assert_eq!(snapshot.families[1].name, "sync_downloaded_block_count");
        assert_eq!(snapshot.families[1].kind, MetricKind::Untyped);
        assert_eq!(snapshot.counter("sync_downloaded_block"), Some(3.0));
        // Untyped families answer both `gauge` and `counter`.
        assert_eq!(snapshot.counter("sync_downloaded_block_count"), Some(4.0));
        assert_eq!(snapshot.gauge("sync_downloaded_block_count"), Some(4.0));
    }

    #[test]
    fn values_timestamps_and_escapes() {
        let snapshot = parse_ok(
            "# a comment\n\
             #\n\
             # HELP odd A \\\\ backslash and a\\nnewline\n\
             # TYPE odd gauge\n\
             odd{path=\"C:\\\\dir\",quote=\"say \\\"hi\\\"\",nl=\"a\\nb\",} +Inf 1700000000000\r\n\
             odd{x=\"\"} -Inf -5\n\
             odd  {  y = \"1\"  }   NaN\n\
             odd{} 1e3\n\
             plain 0.5\n",
        );
        assert_eq!(snapshot.families.len(), 2);
        let odd = &snapshot.families[0];
        assert_eq!(odd.help.as_deref(), Some("A \\ backslash and a\nnewline"));
        assert_eq!(odd.samples.len(), 4);
        assert_eq!(
            labels(&odd.samples[0]),
            vec![("path", "C:\\dir"), ("quote", "say \"hi\""), ("nl", "a\nb")]
        );
        assert_eq!(odd.samples[0].value, f64::INFINITY);
        assert_eq!(odd.samples[1].value, f64::NEG_INFINITY);
        assert_eq!(labels(&odd.samples[1]), vec![("x", "")]);
        assert!(odd.samples[2].value.is_nan());
        assert_eq!(labels(&odd.samples[2]), vec![("y", "1")]);
        assert_eq!(odd.samples[3].value, 1000.0);
        assert!(odd.samples[3].labels.is_empty());
        // Four samples, one of them unlabelled: that one is the family's value.
        assert_eq!(snapshot.gauge("odd"), Some(1000.0));
        assert!(snapshot.total("odd").is_some_and(f64::is_nan));
        assert_eq!(snapshot.families[1].kind, MetricKind::Untyped);
        assert_eq!(snapshot.gauge("plain"), Some(0.5));
    }

    #[test]
    fn type_lines_after_samples_and_unknown_types() {
        let snapshot = parse_ok(
            "late 1\n\
             # TYPE late counter\n\
             # TYPE novel stateset\n\
             novel{s=\"on\"} 1\n",
        );
        assert_eq!(snapshot.families[0].kind, MetricKind::Counter);
        assert_eq!(snapshot.families[0].samples.len(), 1);
        assert_eq!(snapshot.families[1].kind, MetricKind::Untyped);
        assert_eq!(snapshot.families[1].samples.len(), 1);
    }

    #[test]
    fn an_empty_page_has_no_families() {
        assert!(parse_ok("").families.is_empty());
        assert!(parse_ok("\n\n# nothing\n").families.is_empty());
    }

    #[test]
    fn malformed_lines_are_reported_with_their_number() {
        for (text, line, reason) in [
            ("ok 1\nbad\n", 2, "has no value"),
            ("ok 1\n\nbad x\n", 3, "invalid value"),
            ("1bad 1\n", 1, "invalid metric name"),
            ("bad{a=\"1\" 1\n", 1, "expected `,` or `}`"),
            ("bad{a=\"1} 1\n", 1, "unterminated value"),
            ("bad{a=1} 1\n", 1, "expected a quoted value"),
            ("bad{a} 1\n", 1, "expected `=`"),
            ("bad{=\"1\"} 1\n", 1, "invalid label name"),
            ("bad{a=\"1\"\n", 1, "expected `,` or `}`"),
            ("bad{\n", 1, "unterminated labels"),
            ("bad 1 now\n", 1, "invalid timestamp"),
            ("bad 1 2 3\n", 1, "unexpected text"),
            ("# TYPE 1bad gauge\n", 1, "invalid metric name"),
            ("# HELP\n# HELP  \n# HELP 9 x\n", 3, "invalid metric name"),
        ] {
            let err = parse(text, UNIX_EPOCH).expect_err(text);
            assert_eq!(err.line, line, "{text:?}: {err}");
            assert!(err.reason.contains(reason), "{text:?}: {err}");
            assert!(
                err.to_string()
                    .starts_with(&format!("metrics line {line}: "))
            );
        }
    }
}
