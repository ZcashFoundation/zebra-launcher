//! A short history of the node statistics the RPC monitor publishes, for the dashboard's
//! charts and rates. Pure Rust, tested natively.

use launcher_types::NodeStats;

/// The samples kept since the launcher opened, oldest first.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatsHistory {
    samples: Vec<NodeStats>,
}

impl StatsHistory {
    /// Records a published statistics value.
    pub fn push(&mut self, stats: &NodeStats) {
        self.samples.push(stats.clone());
    }
}
