//! Analysis of zebrad's log stream: sync progress, problems, startup milestones and activity
//! counts, folded line by line as batches arrive. Pure Rust, tested natively.

use std::time::SystemTime;

use launcher_types::LogLine;

/// What the log stream so far says about the node.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Insights {
    /// The highest `seq` folded in; lines at or below it are duplicates.
    last_seq: Option<u64>,
}

impl Insights {
    /// Folds a batch of lines in. `now` stands in for lines without a timestamp.
    pub fn ingest(&mut self, lines: &[LogLine], now: SystemTime) {
        let _ = now;
        for line in lines {
            if self.last_seq.is_some_and(|seen| line.seq <= seen) {
                continue;
            }
            self.last_seq = Some(line.seq);
        }
    }
}
