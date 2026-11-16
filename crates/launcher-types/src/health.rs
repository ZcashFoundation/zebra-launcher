//! The health levels and thresholds the backend and the UI share.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// A statistics sample older than this, while zebrad runs, means the RPC is not answering.
pub const STALE_AFTER: Duration = Duration::from_secs(30);
/// No peers this long is a problem.
pub const NO_PEERS_FOR: Duration = Duration::from_secs(2 * 60);

/// From this verification progress on the chain is at its tip, as far as the bar can show.
pub const SYNCED_PROGRESS: f64 = 0.9999;

/// How the verdict reads. The icon and the word carry it; a status colour only accompanies.
/// The order is gravity: [`Level::Problem`] is the greatest. On the wire it is the snake_case
/// name (`healthy`, `attention`, ...).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    /// At the tip with peers, nothing to do.
    Healthy,
    /// A normal state that is not "healthy": syncing, starting, stopping, stopped.
    Neutral,
    /// Something to look at.
    Attention,
    /// Attention with a deadline (end of support in days, an install counting down).
    Urgent,
    /// The node is not doing its job.
    Problem,
}

impl Level {
    /// The chip's word. A neutral verdict has none of its own: its headline (Syncing,
    /// Starting, Stopping, Stopped, Running) is the word.
    pub fn word(self) -> Option<&'static str> {
        match self {
            Level::Healthy => Some("Healthy"),
            Level::Neutral => None,
            Level::Attention | Level::Urgent => Some("Attention"),
            Level::Problem => Some("Problem"),
        }
    }

    /// The status tone's name, for a class or token suffix: `good`, `neutral`, `warning`,
    /// `serious` or `critical`.
    pub fn tone(self) -> &'static str {
        match self {
            Level::Healthy => "good",
            Level::Neutral => "neutral",
            Level::Attention => "warning",
            Level::Urgent => "serious",
            Level::Problem => "critical",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_order_by_gravity_and_travel_by_name() {
        assert!(Level::Healthy < Level::Neutral);
        assert!(Level::Attention < Level::Urgent);
        assert_eq!(
            [Level::Attention, Level::Problem, Level::Healthy]
                .into_iter()
                .max(),
            Some(Level::Problem)
        );
        assert_eq!(
            serde_json::to_string(&Level::Attention).unwrap(),
            "\"attention\""
        );
        assert_eq!(
            serde_json::from_str::<Level>("\"problem\"").unwrap(),
            Level::Problem
        );
    }
}
