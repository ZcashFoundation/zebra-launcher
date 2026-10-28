//! The sentences of the "This run" card: the peers, checkpoints and mempool facts, the run's
//! head and its shutdown, from [`crate::insights`]. Pure Rust, tested natively.

use std::time::SystemTime;

use crate::{
    format::{relative, thousands},
    insights::{Checkpoints, Mempool, Peers, Run},
};

/// `12 peers connected at start, 3 failed`, with the later troubles appended.
pub fn peers_line(peers: &Peers, connected: bool) -> String {
    if !connected {
        return "Waiting for the initial peer connections".to_owned();
    }
    let mut text = format!(
        "{} peers connected at start, {} failed",
        peers.initial_successes, peers.initial_errors
    );
    if peers.failed_connections > 0 {
        text.push_str(&format!(
            " · {} outbound connections failed since",
            peers.failed_connections
        ));
    }
    if peers.stalled_dropped > 0 {
        text.push_str(&format!(
            " · {} stalled peers dropped",
            peers.stalled_dropped
        ));
    }
    if peers.no_ready_peers_events > 0 {
        text.push_str(&format!(
            " · {} times without ready peers",
            peers.no_ready_peers_events
        ));
    }
    text
}

/// `2,900,000 blocks verified by checkpoints · full validation since 14:02`; `since` is the
/// local time of the final checkpoint.
pub fn checkpoints_line(checkpoints: &Checkpoints, since: Option<String>) -> Option<String> {
    if checkpoints.blocks_verified == 0 && since.is_none() {
        return None;
    }
    let mut text = format!(
        "{} blocks verified by checkpoints",
        thousands(checkpoints.blocks_verified)
    );
    if let Some(since) = since {
        text.push_str(&format!(" · full validation since {since}"));
    }
    Some(text)
}

/// `Mempool active since 14:05 at height 2,949,999 · reset twice`.
pub fn mempool_line(mempool: &Mempool, since: Option<String>) -> Option<String> {
    let since = since?;
    let mut text = format!("Mempool active since {since}");
    if let Some(height) = mempool.tip_height {
        text.push_str(&format!(" at height {}", thousands(height)));
    }
    match mempool.resets {
        0 => {}
        1 => text.push_str(" · reset once"),
        n => text.push_str(&format!(" · reset {n} times")),
    }
    Some(text)
}

/// `zebrad 7.0.0 on Mainnet` with whatever the metadata lines gave.
pub fn run_head(run: &Run) -> String {
    match (&run.version, &run.network) {
        (Some(version), Some(network)) => format!("zebrad {version} on {network}"),
        (Some(version), None) => format!("zebrad {version}"),
        (None, Some(network)) => format!("zebrad on {network}"),
        (None, None) => "zebrad".to_owned(),
    }
}

/// `2 earlier runs in this log`, when the log holds more than the current run.
pub fn earlier_runs_line(earlier: usize) -> Option<String> {
    match earlier {
        0 => None,
        1 => Some("1 earlier run in this log".to_owned()),
        n => Some(format!("{n} earlier runs in this log")),
    }
}

/// `Stopped 5 min ago (SIGTERM)`.
pub fn shutdown_line(run: &Run, now: SystemTime) -> Option<String> {
    let stopped = run.stopped_at?;
    let mut text = format!("Stopped {}", relative(stopped, now));
    if let Some(reason) = &run.shutdown_reason {
        text.push_str(&format!(" ({reason})"));
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

    #[test]
    fn sentences() {
        let peers = Peers {
            initial_successes: 12,
            initial_errors: 3,
            outbound: 12,
            failed_connections: 0,
            stalled_dropped: 0,
            no_ready_peers_events: 0,
        };
        assert_eq!(
            peers_line(&peers, true),
            "12 peers connected at start, 3 failed"
        );
        assert_eq!(
            peers_line(&peers, false),
            "Waiting for the initial peer connections"
        );
        let troubled = Peers {
            failed_connections: 2,
            stalled_dropped: 1,
            no_ready_peers_events: 4,
            ..peers
        };
        assert_eq!(
            peers_line(&troubled, true),
            "12 peers connected at start, 3 failed · 2 outbound connections failed since · 1 stalled peers dropped · 4 times without ready peers"
        );
        assert_eq!(checkpoints_line(&Checkpoints::default(), None), None);
        assert_eq!(
            checkpoints_line(
                &Checkpoints {
                    blocks_verified: 2_900_000,
                    final_reached: None
                },
                None
            ),
            Some("2,900,000 blocks verified by checkpoints".into())
        );
        assert_eq!(
            checkpoints_line(
                &Checkpoints {
                    blocks_verified: 2_900_000,
                    final_reached: Some(UNIX_EPOCH)
                },
                Some("14:02".into())
            ),
            Some("2,900,000 blocks verified by checkpoints · full validation since 14:02".into())
        );
        assert_eq!(mempool_line(&Mempool::default(), None), None);
        assert_eq!(
            mempool_line(
                &Mempool {
                    active_since: Some(UNIX_EPOCH),
                    tip_height: Some(2_949_999),
                    resets: 2
                },
                Some("14:05".into())
            ),
            Some("Mempool active since 14:05 at height 2,949,999 · reset 2 times".into())
        );
        assert_eq!(
            mempool_line(
                &Mempool {
                    active_since: Some(UNIX_EPOCH),
                    tip_height: None,
                    resets: 1
                },
                Some("14:05".into())
            ),
            Some("Mempool active since 14:05 · reset once".into())
        );
        let run = Run {
            started_at: UNIX_EPOCH,
            version: Some("7.0.0".into()),
            network: Some("Mainnet".into()),
            milestones: Vec::new(),
            shutdown_reason: Some("SIGTERM".into()),
            stopped_at: Some(UNIX_EPOCH + Duration::from_secs(60)),
        };
        assert_eq!(run_head(&run), "zebrad 7.0.0 on Mainnet");
        assert_eq!(
            run_head(&Run {
                version: None,
                ..run.clone()
            }),
            "zebrad on Mainnet"
        );
        assert_eq!(
            run_head(&Run {
                network: None,
                ..run.clone()
            }),
            "zebrad 7.0.0"
        );
        assert_eq!(
            run_head(&Run {
                version: None,
                network: None,
                ..run.clone()
            }),
            "zebrad"
        );
        let now = UNIX_EPOCH + Duration::from_secs(360);
        assert_eq!(
            shutdown_line(&run, now),
            Some("Stopped 5 min ago (SIGTERM)".into())
        );
        assert_eq!(
            shutdown_line(
                &Run {
                    stopped_at: None,
                    ..run.clone()
                },
                now
            ),
            None
        );
        assert_eq!(
            shutdown_line(
                &Run {
                    shutdown_reason: None,
                    ..run
                },
                now
            ),
            Some("Stopped 5 min ago".into())
        );
        assert_eq!(earlier_runs_line(0), None);
        assert_eq!(
            earlier_runs_line(1),
            Some("1 earlier run in this log".into())
        );
        assert_eq!(
            earlier_runs_line(3),
            Some("3 earlier runs in this log".into())
        );
    }
}
