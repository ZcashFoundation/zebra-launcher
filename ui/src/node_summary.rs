//! A node's state in a few words, for the sidebar and the overview's cards: the same rule in
//! both places. Pure Rust, tested natively. The dashboard's full verdict is
//! [`crate::health`]; this is the line that fits beside a name.

use std::time::SystemTime;

use launcher_types::{NodeKind, NodeRole, NodeStats, Phase, Status};

use crate::format::{exit_class_text, is_synced, relative, sync_percent};

/// How a node is doing, as a colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// Running and where it should be.
    Ok,
    /// Running and working towards it: syncing, starting, stopping.
    Busy,
    /// Not running, by choice.
    Idle,
    /// Not running, or not well, against the user's wishes.
    Problem,
}

impl Tone {
    /// The CSS class of the status dot.
    pub fn class(self) -> &'static str {
        match self {
            Tone::Ok => "tone-ok",
            Tone::Busy => "tone-busy",
            Tone::Idle => "tone-idle",
            Tone::Problem => "tone-problem",
        }
    }
}

/// A node's state: a sentence for its card and a word or two for the sidebar.
#[derive(Clone, Debug, PartialEq)]
pub struct Summary {
    pub tone: Tone,
    /// "Syncing 94.5 %", "At the tip", "Stopped 2 days ago".
    pub text: String,
    /// "94 %", "at tip", "stopped".
    pub short: String,
    /// The sync progress as a percentage, for a meter: full nodes that are running only.
    pub progress: Option<f64>,
}

/// The summary of a node of `kind` with `status` and its newest `stats`, at `now`.
pub fn summarize(
    kind: NodeKind,
    status: &Status,
    stats: Option<&NodeStats>,
    now: SystemTime,
) -> Summary {
    let summary = |tone, text: String, short: &str| Summary {
        tone,
        text,
        short: short.to_owned(),
        progress: None,
    };
    match &status.phase {
        Phase::Running => running(kind, stats),
        Phase::Starting => summary(Tone::Busy, "Starting".into(), "starting"),
        Phase::Stopping => summary(Tone::Busy, "Stopping".into(), "stopping"),
        Phase::Backoff { .. } => summary(
            Tone::Problem,
            "Restarting after a crash".into(),
            "restarting",
        ),
        Phase::Failed { class } => {
            let mut why = exit_class_text(*class).to_owned();
            if let Some(first) = why.get_mut(0..1) {
                first.make_ascii_uppercase();
            }
            summary(Tone::Problem, why, "failed")
        }
        Phase::Stopped => {
            let text = match &status.last_exit {
                Some(exit) => format!("Stopped {}", relative(exit.at, now)),
                None => "Stopped".to_owned(),
            };
            summary(Tone::Idle, text, "stopped")
        }
    }
}

fn running(kind: NodeKind, stats: Option<&NodeStats>) -> Summary {
    if kind.role() != NodeRole::FullNode {
        let (text, short) = match kind.role() {
            NodeRole::Indexer => ("Serving light wallets", "serving"),
            _ => ("Running", "running"),
        };
        return Summary {
            tone: Tone::Ok,
            text: text.to_owned(),
            short: short.to_owned(),
            progress: None,
        };
    }
    let Some(stats) = stats else {
        return Summary {
            tone: Tone::Busy,
            text: "Running, waiting for its RPC".to_owned(),
            short: "running".to_owned(),
            progress: None,
        };
    };
    let percent = sync_percent(stats.verification_progress);
    if is_synced(stats.verification_progress) {
        Summary {
            tone: Tone::Ok,
            text: "At the tip".to_owned(),
            short: "at tip".to_owned(),
            progress: Some(percent),
        }
    } else {
        Summary {
            tone: Tone::Busy,
            text: format!("Syncing {percent:.1} %"),
            short: format!("{} %", percent.floor()),
            progress: Some(percent),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use launcher_types::{ExitClass, ExitInfo};

    use super::*;

    fn stats(progress: f64) -> NodeStats {
        NodeStats {
            height: 100,
            estimated_height: 120,
            verification_progress: progress,
            peers: 8,
            mempool_transactions: 0,
            chain: "main".into(),
            zebrad_version: "7.0.0".into(),
            last_error: None,
            eos: None,
            updated_at: SystemTime::UNIX_EPOCH,
        }
    }

    fn status(phase: Phase) -> Status {
        Status {
            phase,
            ..Status::default()
        }
    }

    const NOW: SystemTime = SystemTime::UNIX_EPOCH;

    #[test]
    fn a_running_full_node_says_how_far_it_is() {
        let syncing = summarize(
            NodeKind::Zebrad,
            &status(Phase::Running),
            Some(&stats(0.9456)),
            NOW,
        );
        assert_eq!(
            (syncing.tone, syncing.text.as_str()),
            (Tone::Busy, "Syncing 94.6 %")
        );
        assert_eq!(syncing.short, "94 %");
        assert!(syncing.progress.is_some_and(|p| (p - 94.56).abs() < 1e-9));
        let tip = summarize(
            NodeKind::Zakura,
            &status(Phase::Running),
            Some(&stats(1.0)),
            NOW,
        );
        assert_eq!((tip.tone, tip.short.as_str()), (Tone::Ok, "at tip"));
        let silent = summarize(NodeKind::Zebrad, &status(Phase::Running), None, NOW);
        assert_eq!(silent.tone, Tone::Busy);
        assert_eq!(silent.progress, None);
    }

    #[test]
    fn a_service_says_what_it_does() {
        let zaino = summarize(NodeKind::Zaino, &status(Phase::Running), None, NOW);
        assert_eq!(
            (zaino.tone, zaino.text.as_str()),
            (Tone::Ok, "Serving light wallets")
        );
        let custom = summarize(NodeKind::Generic, &status(Phase::Running), None, NOW);
        assert_eq!(custom.short, "running");
    }

    #[test]
    fn a_stopped_or_failed_node_says_why() {
        let mut stopped = status(Phase::Stopped);
        stopped.last_exit = Some(ExitInfo {
            code: Some(0),
            class: ExitClass::Clean,
            detail: None,
            at: NOW,
            requested: true,
        });
        let later = NOW + Duration::from_secs(2 * 86_400 + 60);
        let summary = summarize(NodeKind::Zcashd, &stopped, None, later);
        assert_eq!(
            (summary.tone, summary.text.as_str()),
            (Tone::Idle, "Stopped 2 days ago")
        );
        let failed = summarize(
            NodeKind::Zebrad,
            &status(Phase::Failed {
                class: ExitClass::PortInUse,
            }),
            None,
            NOW,
        );
        assert_eq!(failed.tone, Tone::Problem);
        assert_eq!(failed.text, "A port zebrad needs is already in use");
        assert_eq!(
            summarize(
                NodeKind::Zebrad,
                &status(Phase::Backoff {
                    until: NOW,
                    crashes: 2
                }),
                None,
                NOW
            )
            .short,
            "restarting"
        );
    }
}
