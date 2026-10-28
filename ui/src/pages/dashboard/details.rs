//! The details cards (specification 3.11 Node and 3.12 Chain): label and value rows, each
//! absent when its source is absent, never an em dash. A stopped node keeps its phase, last
//! exit, restarts and last known version; the chain card keeps the last values, muted. The
//! sentences are pure functions, tested natively.

use std::time::{Duration, SystemTime};

use launcher_types::{EosInfo, ExitInfo, NodeStats, OpenTarget, Phase, Status};
use leptos::{prelude::*, task::spawn_local};

use crate::{
    api,
    charts::{byte_rate, hhmm, tz_offset_min_at, uptime_text},
    components::{
        rows::{Row, Rows},
        transient::Transient,
    },
    format::{bytes, exit_class_text, relative, rough_duration, thousands},
    stats_history::Sample,
};

use super::{
    Dash, Icon, MetricsFacts, local_date, local_short, status_card::CHECKPOINT_NEAR_BLOCKS,
};

/// The Phase row: `Running for 2 d 4 h, since 23 Oct, 21:13`, `Restarting in 25 s (crash 2)`,
/// `Failed · zebrad crashed`.
pub fn phase_text(status: &Status, now: SystemTime, since: Option<&str>) -> String {
    let for_ = |started: SystemTime| uptime_text(now.duration_since(started).unwrap_or_default());
    match &status.phase {
        Phase::Running => match (status.started_at, since) {
            (Some(started), Some(since)) => format!("Running for {}, since {since}", for_(started)),
            (Some(started), None) => format!("Running for {}", for_(started)),
            (None, _) => "Running".to_owned(),
        },
        Phase::Starting => match status.started_at {
            Some(started) => format!("Starting, for {}", for_(started)),
            None => "Starting".to_owned(),
        },
        Phase::Stopping => "Stopping".to_owned(),
        Phase::Stopped => "Stopped".to_owned(),
        Phase::Backoff { until, crashes } => {
            let when = match until.duration_since(now) {
                Ok(left) => format!("in {}", rough_duration(left)),
                Err(_) => "now".to_owned(),
            };
            format!("Restarting {when} (crash {crashes})")
        }
        Phase::Failed { class } => format!("Failed · {}", exit_class_text(*class)),
    }
}

/// The End of support row: `block 3,160,000, about 3 Dec 2026 (in 40 days)`.
pub fn eos_text(eos: &EosInfo, date: &str, now: SystemTime) -> String {
    format!(
        "block {}, about {date} ({})",
        thousands(eos.block_height),
        relative(eos.estimated_time, now)
    )
}

/// The Last exit row: `zebrad crashed (code 101), 2 h ago`.
pub fn exit_text(exit: &ExitInfo, now: SystemTime) -> String {
    let code = exit.code.map_or_else(
        || "killed by a signal".to_owned(),
        |code| format!("code {code}"),
    );
    format!(
        "{} ({code}), {}",
        exit_class_text(exit.class),
        relative(exit.at, now)
    )
}

/// The Restarts row, only when there were any.
pub fn restarts_text(restarts: usize) -> Option<String> {
    (restarts > 0).then(|| format!("{restarts} this session"))
}

/// The Data folder row: the chosen folder, or the launcher's own.
pub fn data_folder_text(data_dir: Option<&str>) -> String {
    data_dir.map_or_else(|| "the launcher's data folder".to_owned(), str::to_owned)
}

/// The Network row: `1.2 MB/s in · 300.0 kB/s out`, from bytes per second.
pub fn network_text(in_bps: f64, out_bps: f64) -> String {
    format!("{} in · {} out", byte_rate(in_bps), byte_rate(out_bps))
}

/// The newest minute of snapshots gives the Network row its rate.
pub const NETWORK_RATE_WINDOW: Duration = Duration::from_secs(60);

/// One row of the Chain card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChainRow {
    pub label: &'static str,
    pub text: String,
    /// A fork is being resolved: the row gets a warning icon.
    pub warning: bool,
}

fn chain_row(label: &'static str, text: String) -> ChainRow {
    ChainRow {
        label,
        text,
        warning: false,
    }
}

/// The Verification row: `checkpoints to 2,500,000, then full` while the checkpoint verifier
/// is within [`CHECKPOINT_NEAR_BLOCKS`] of the height, else `full validation since 14:02`
/// when the log said so.
pub fn verification_text(
    checkpoint_height: Option<u64>,
    height: u64,
    full_since: Option<&str>,
) -> Option<String> {
    match checkpoint_height {
        Some(checkpoint) if height.saturating_sub(checkpoint) <= CHECKPOINT_NEAR_BLOCKS => Some(
            format!("checkpoints to {}, then full", thousands(checkpoint)),
        ),
        _ => full_since.map(|since| format!("full validation since {since}")),
    }
}

/// The Chain card's rows from the published stats, or from the last sample once zebrad
/// stopped (no chain name then); empty without either.
pub fn chain_rows(
    stats: Option<&NodeStats>,
    latest: Option<&Sample>,
    facts: Option<&MetricsFacts>,
    full_since: Option<&str>,
) -> Vec<ChainRow> {
    let mut rows = Vec::new();
    let (height, estimated) = match (stats, latest) {
        (Some(stats), _) => {
            rows.push(chain_row("Chain", stats.chain.clone()));
            (stats.height, stats.estimated_height)
        }
        (None, Some(sample)) => (sample.height, sample.estimated_height),
        (None, None) => return rows,
    };
    rows.push(chain_row("Height", thousands(height)));
    rows.push(chain_row("Estimated tip", thousands(estimated)));
    let checkpoint = facts.and_then(|facts| facts.checkpoint_height);
    if let Some(text) = verification_text(checkpoint, height, full_since) {
        rows.push(chain_row("Verification", text));
    }
    if let Some(count) = facts.and_then(|facts| facts.chain_count) {
        rows.push(match count {
            0 | 1 => chain_row("Chains in memory", count.to_string()),
            n => ChainRow {
                label: "Chains in memory",
                text: format!("{n} · a fork is being resolved"),
                warning: true,
            },
        });
    }
    rows
}

/// `time` as `HH:MM` in the browser's zone at that instant.
fn local_hhmm(time: SystemTime) -> String {
    hhmm(time, tz_offset_min_at(time))
}

fn text_cell(text: String) -> AnyView {
    text.into_any()
}

/// The Node and Chain cards.
#[component]
pub(super) fn Details(dash: Dash) -> impl IntoView {
    let state = dash.state;
    let open_error = Transient::<String>::new();
    let open_data_dir = move |_| {
        spawn_local(async move {
            if let Err(error) = api::open_path(OpenTarget::DataDir).await {
                open_error.hold(format!("Could not open the data folder: {error}"));
            }
        });
    };

    // Rebuilt when the status, stats, settings, metrics facts or log facts change; the cells
    // that show a time follow the clock on their own.
    let node_rows = move || {
        let status = state.status.get();
        let live = matches!(
            status.phase,
            Phase::Running | Phase::Starting | Phase::Stopping
        );
        let since = status.started_at.map(local_short);
        let mut rows: Vec<Row> = Vec::new();
        {
            let status = status.clone();
            rows.push((
                "Phase",
                (move || phase_text(&status, state.now.get(), since.as_deref())).into_any(),
            ));
        }
        let version = state
            .stats
            .with(|stats| stats.as_ref().map(|stats| stats.zebrad_version.clone()));
        let last_version = state
            .insights
            .with(|insights| insights.current_run().and_then(|run| run.version.clone()));
        if live {
            if let Some(pid) = status.pid {
                rows.push(("PID", text_cell(pid.to_string())));
            }
            if let Some(addr) = status.rpc_addr {
                rows.push(("RPC", text_cell(addr.to_string())));
            }
            if let Some(addr) = status.metrics_addr {
                rows.push(("Metrics", text_cell(addr.to_string())));
            }
            if let Some(version) = version.or(last_version) {
                rows.push(("zebrad", text_cell(version)));
            }
            let upgrade = state.insights.with(|insights| {
                insights
                    .sync()
                    .and_then(|sync| sync.network_upgrade.clone())
            });
            if let Some(upgrade) = upgrade {
                rows.push(("Network upgrade", text_cell(upgrade)));
            }
            let eos = state
                .stats
                .with(|stats| stats.as_ref().and_then(|stats| stats.eos.clone()));
            if let Some(eos) = eos {
                let date = local_date(eos.estimated_time);
                rows.push((
                    "End of support",
                    (move || eos_text(&eos, &date, state.now.get())).into_any(),
                ));
            }
            let data_dir = state.settings.with(|settings| {
                settings
                    .as_ref()
                    .map(|settings| data_folder_text(settings.data_dir.as_deref()))
            });
            if let Some(folder) = data_dir {
                rows.push((
                    "Data folder",
                    view! {
                        <span class="dash-folder">
                            <span>{folder}</span>
                            <button class="link" type="button" on:click=open_data_dir>"Open"</button>
                            {move || open_error.get().map(|error| view! { <span class="error">{error}</span> })}
                        </span>
                    }
                    .into_any(),
                ));
            }
            if let Some(memory) = dash.metrics.get().and_then(|facts| facts.memory_bytes) {
                rows.push(("State memory", text_cell(bytes(memory))));
            }
            let network = state.metrics_history.with(|history| {
                history.net_rate(Some(NETWORK_RATE_WINDOW), state.now.get_untracked())
            });
            if let Some((in_bps, out_bps)) = network {
                rows.push(("Network", text_cell(network_text(in_bps, out_bps))));
            }
            if !status.stop_is_graceful {
                rows.push((
                    "Stop",
                    text_cell("hard stop: zebrad gets no shutdown".to_owned()),
                ));
            }
        } else if let Some(version) = version.or(last_version) {
            rows.push((
                "zebrad",
                view! { <span class="muted">{version}</span> }.into_any(),
            ));
        }
        let restarts = state
            .insights
            .with(|insights| insights.runs().len().saturating_sub(1));
        if let Some(text) = restarts_text(restarts) {
            rows.push(("Restarts", text_cell(text)));
        }
        if let Some(exit) = status.last_exit {
            let detail = exit.detail.clone();
            rows.push((
                "Last exit",
                (move || exit_text(&exit, state.now.get())).into_any(),
            ));
            if let Some(detail) = detail {
                rows.push(("Exit detail", text_cell(detail)));
            }
        }
        rows
    };

    let chain = Memo::new(move |_| {
        let full_since = state
            .insights
            .with(|insights| insights.checkpoints().final_reached.map(local_hhmm));
        let facts = dash.metrics.get();
        state.stats.with(|stats| {
            chain_rows(
                stats.as_ref(),
                dash.latest.get().as_ref(),
                facts.as_ref(),
                full_since.as_deref(),
            )
        })
    });
    let chain_view = move || {
        let rows: Vec<Row> = chain
            .get()
            .into_iter()
            .map(|row| {
                let cell = if row.warning {
                    view! {
                        <span class="dash-row-warning">
                            {Icon::Warning.view()}
                            <span>{row.text}</span>
                        </span>
                    }
                    .into_any()
                } else {
                    row.text.into_any()
                };
                (row.label, cell)
            })
            .collect();
        view! { <Rows rows /> }
    };

    view! {
        <div class="cards dash-details">
            <div class="card dash-node">
                <h3>"Node"</h3>
                {move || view! { <Rows rows=node_rows() /> }}
            </div>
            <div
                class="card dash-chain"
                class:dash-stopped=move || !dash.running()
            >
                <h3>"Chain"</h3>
                <Show
                    when=move || chain.with(|rows| !rows.is_empty())
                    fallback=|| view! { <p class="muted">"No data from zebrad's RPC yet."</p> }
                >
                    {chain_view}
                </Show>
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use launcher_types::ExitClass;

    use super::*;

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn status(phase: Phase, started_secs: Option<u64>) -> Status {
        Status {
            phase,
            started_at: started_secs.map(at),
            ..Status::default()
        }
    }

    fn stats(height: u64) -> NodeStats {
        NodeStats {
            height,
            estimated_height: 3_120_000,
            verification_progress: height as f64 / 3_120_000.0,
            peers: 8,
            mempool_transactions: 24,
            chain: "main".into(),
            zebrad_version: "7.0.0".into(),
            last_error: None,
            eos: None,
            updated_at: at(0),
        }
    }

    fn facts(checkpoint: Option<u64>, chains: Option<u64>) -> MetricsFacts {
        MetricsFacts {
            at: at(0),
            disk_bytes: None,
            memory_bytes: None,
            mempool_bytes: None,
            ready_peers: None,
            unready_peers: None,
            obsolete_peers: None,
            obsolete_refused: None,
            handshake_failures: None,
            checkpoint_height: checkpoint,
            chain_count: chains,
        }
    }

    #[test]
    fn phase_rows() {
        let now = at(100_000 + 2 * 86_400 + 4 * 3600);
        assert_eq!(
            phase_text(
                &status(Phase::Running, Some(100_000)),
                now,
                Some("23 Oct, 21:13")
            ),
            "Running for 2 d 4 h, since 23 Oct, 21:13"
        );
        assert_eq!(
            phase_text(&status(Phase::Running, Some(100_000)), now, None),
            "Running for 2 d 4 h"
        );
        assert_eq!(
            phase_text(&status(Phase::Running, None), now, None),
            "Running"
        );
        assert_eq!(
            phase_text(&status(Phase::Starting, Some(100_000)), at(100_012), None),
            "Starting, for 12 s"
        );
        assert_eq!(
            phase_text(&status(Phase::Stopping, None), now, None),
            "Stopping"
        );
        assert_eq!(
            phase_text(&status(Phase::Stopped, None), now, None),
            "Stopped"
        );
        let backoff = status(
            Phase::Backoff {
                until: now + Duration::from_secs(25),
                crashes: 2,
            },
            None,
        );
        assert_eq!(
            phase_text(&backoff, now, None),
            "Restarting in 25 s (crash 2)"
        );
        let overdue = status(
            Phase::Backoff {
                until: now - Duration::from_secs(1),
                crashes: 3,
            },
            None,
        );
        assert_eq!(phase_text(&overdue, now, None), "Restarting now (crash 3)");
        let failed = status(
            Phase::Failed {
                class: ExitClass::Crash,
            },
            None,
        );
        assert_eq!(phase_text(&failed, now, None), "Failed · zebrad crashed");
    }

    #[test]
    fn node_sentences() {
        let now = at(1_000_000);
        let eos = EosInfo {
            block_height: 3_160_000,
            estimated_time: now + Duration::from_secs(40 * 86_400),
        };
        assert_eq!(
            eos_text(&eos, "3 Dec 2026", now),
            "block 3,160,000, about 3 Dec 2026 (in 40 days)"
        );
        let exit = ExitInfo {
            code: Some(101),
            class: ExitClass::Crash,
            detail: None,
            at: now - Duration::from_secs(7200),
            requested: false,
        };
        assert_eq!(exit_text(&exit, now), "zebrad crashed (code 101), 2 h ago");
        let killed = ExitInfo { code: None, ..exit };
        assert_eq!(
            exit_text(&killed, now),
            "zebrad crashed (killed by a signal), 2 h ago"
        );
        assert_eq!(restarts_text(0), None);
        assert_eq!(restarts_text(2), Some("2 this session".into()));
        assert_eq!(
            network_text(1_234_567.0, 300_000.0),
            "1.2 MB/s in · 300.0 kB/s out"
        );
        assert_eq!(network_text(-1.0, 0.4), "0 bytes/s in · 0 bytes/s out");
        assert_eq!(data_folder_text(Some("/mnt/zebra")), "/mnt/zebra");
        assert_eq!(data_folder_text(None), "the launcher's data folder");
    }

    #[test]
    fn verification_rows() {
        assert_eq!(
            verification_text(Some(2_500_000), 2_502_000, Some("14:02")),
            Some("checkpoints to 2,500,000, then full".into())
        );
        assert_eq!(
            verification_text(Some(2_500_000), 2_952_680, Some("14:02")),
            Some("full validation since 14:02".into())
        );
        assert_eq!(verification_text(Some(2_500_000), 2_952_680, None), None);
        assert_eq!(verification_text(None, 10, None), None);
    }

    #[test]
    fn chain_rows_from_stats_or_the_last_sample() {
        let rows = chain_rows(
            Some(&stats(2_952_680)),
            None,
            Some(&facts(Some(2_500_000), Some(1))),
            Some("14:02"),
        );
        let texts: Vec<_> = rows.iter().map(|r| (r.label, r.text.as_str())).collect();
        assert_eq!(
            texts,
            vec![
                ("Chain", "main"),
                ("Height", "2,952,680"),
                ("Estimated tip", "3,120,000"),
                ("Verification", "full validation since 14:02"),
                ("Chains in memory", "1"),
            ]
        );
        assert!(rows.iter().all(|r| !r.warning));
        let fork = chain_rows(Some(&stats(10)), None, Some(&facts(None, Some(2))), None);
        let last = fork.last().unwrap();
        assert_eq!(last.text, "2 · a fork is being resolved");
        assert!(last.warning);
        // Stopped: the last sample, without a chain name.
        let sample = Sample::from_stats(&stats(2_952_680));
        let rows = chain_rows(None, Some(&sample), None, None);
        let labels: Vec<_> = rows.iter().map(|r| r.label).collect();
        assert_eq!(labels, vec!["Height", "Estimated tip"]);
        assert!(chain_rows(None, None, None, None).is_empty());
    }
}
