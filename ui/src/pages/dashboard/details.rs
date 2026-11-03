//! The details cards (specification 3.11 Node and 3.12 Chain): label and value rows, each
//! absent when its source is absent, never an em dash. A stopped node keeps its phase, last
//! exit, restarts and last known version; the chain card keeps the last values, muted. The
//! sentences are pure functions, tested natively.

use std::time::{Duration, SystemTime};

use launcher_types::{
    EosInfo, ExitInfo, NodeDetails, NodeStats, OpenTarget, Phase, Status, ValuePool,
};
use leptos::{prelude::*, task::spawn_local};

use crate::{
    api,
    charts::{byte_rate, compact, hhmm, tz_offset_min_at, uptime_text},
    clipboard::copy_to_clipboard,
    components::{
        rows::{Row, Rows},
        transient::Transient,
    },
    format::{
        bytes, exit_class_text, local_date_short, local_datetime_short, relative, rough_duration,
        thousands,
    },
    health::CHECKPOINT_NEAR_BLOCKS,
    stats_history::Sample,
};

/// How long "Copied" stays beside the best block's Copy button.
const COPIED_FOR: Duration = Duration::from_secs(2);

use super::{Dash, Icon, MetricsFacts};

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

/// The zebrad row: `7.0.0 · /Zebra:7.0.0/`, the version with the P2P user agent when the
/// details carry one.
pub fn zebrad_text(version: &str, subversion: Option<&str>) -> String {
    match subversion {
        Some(subversion) if subversion != version => format!("{version} · {subversion}"),
        _ => version.to_owned(),
    }
}

/// A block hash cut to its ends: `0000…a3f1`.
pub fn short_hash(hash: &str) -> String {
    const ENDS: usize = 4;
    let count = hash.chars().count();
    if count <= 2 * ENDS + 1 {
        return hash.to_owned();
    }
    let start: String = hash.chars().take(ENDS).collect();
    let end: String = hash.chars().skip(count - ENDS).collect();
    format!("{start}…{end}")
}

/// The network difficulty: whole with separators from a thousand up, two decimals below
/// (a test network's difficulty is about one).
pub fn difficulty_text(difficulty: f64) -> String {
    if !difficulty.is_finite() || difficulty < 0.0 {
        return "0".to_owned();
    }
    if difficulty >= 1000.0 {
        thousands(difficulty.round() as u64)
    } else {
        format!("{difficulty:.2}")
    }
}

/// An amount in ZEC with separators and two decimals: `21,234,567.89 ZEC`.
pub fn zec_text(zec: f64) -> String {
    let zec = if zec.is_finite() { zec.max(0.0) } else { 0.0 };
    let hundredths = (zec * 100.0).round() as u64;
    format!(
        "{}.{:02} ZEC",
        thousands(hundredths / 100),
        hundredths % 100
    )
}

/// The value pools as text, in zebrad's order: `transparent 14.0M · sapling 512K · orchard
/// 1.65M ZEC`.
pub fn pools_text(pools: &[ValuePool]) -> Option<String> {
    if pools.is_empty() {
        return None;
    }
    let parts: Vec<String> = pools
        .iter()
        .map(|pool| {
            let zec = if pool.zec.is_finite() {
                pool.zec.max(0.0).round() as u64
            } else {
                0
            };
            format!("{} {}", pool.id, compact(zec))
        })
        .collect();
    Some(format!("{} ZEC", parts.join(" · ")))
}

/// The Mempool row: `24 transactions, 1.2 MB`; the size only when the details carry it.
pub fn mempool_text(transactions: u64, size: Option<u64>) -> String {
    let count = match transactions {
        1 => "1 transaction".to_owned(),
        n => format!("{} transactions", thousands(n)),
    };
    match size {
        Some(size) => format!("{count}, {}", bytes(size)),
        None => count,
    }
}

/// The newest minute of snapshots gives the Network row its rate.
pub const NETWORK_RATE_WINDOW: Duration = Duration::from_secs(60);

/// One row of the Chain card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChainRow {
    pub label: &'static str,
    pub text: String,
    /// The full text behind a shortened one (the whole block hash), as the cell's title and
    /// what the Copy button copies.
    pub full: Option<String>,
    /// A fork is being resolved: the row gets a warning icon.
    pub warning: bool,
}

fn chain_row(label: &'static str, text: String) -> ChainRow {
    ChainRow {
        label,
        text,
        full: None,
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
/// stopped (no chain name then); empty without either. The details behind the stats add the
/// headers, the best block, the difficulty, the size on disk, the mempool's bytes, the
/// supply and the pools, each left out while absent.
pub fn chain_rows(
    stats: Option<&NodeStats>,
    latest: Option<&Sample>,
    details: Option<&NodeDetails>,
    facts: Option<&MetricsFacts>,
    full_since: Option<&str>,
) -> Vec<ChainRow> {
    let mut rows = Vec::new();
    let (height, estimated, mempool) = match (stats, latest) {
        (Some(stats), _) => {
            rows.push(chain_row("Chain", stats.chain.clone()));
            (
                stats.height,
                stats.estimated_height,
                stats.mempool_transactions,
            )
        }
        (None, Some(sample)) => (
            sample.height,
            sample.estimated_height,
            sample.mempool_transactions,
        ),
        (None, None) => return rows,
    };
    rows.push(chain_row("Height", thousands(height)));
    rows.push(chain_row("Estimated tip", thousands(estimated)));
    if let Some(headers) = details.and_then(|details| details.headers) {
        rows.push(chain_row("Headers", thousands(headers)));
    }
    if let Some(hash) = details.and_then(|details| details.best_block_hash.as_deref()) {
        rows.push(ChainRow {
            label: "Best block",
            text: short_hash(hash),
            full: Some(hash.to_owned()),
            warning: false,
        });
    }
    if let Some(difficulty) = details.and_then(|details| details.difficulty) {
        rows.push(chain_row("Difficulty", difficulty_text(difficulty)));
    }
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
                full: None,
                warning: true,
            },
        });
    }
    if let Some(size) = details.and_then(|details| details.size_on_disk) {
        rows.push(chain_row("Size on disk", bytes(size)));
    }
    rows.push(chain_row(
        "Mempool",
        mempool_text(mempool, details.and_then(|details| details.mempool_bytes)),
    ));
    if let Some(supply) = details.and_then(|details| details.chain_supply_zec) {
        rows.push(chain_row("Chain supply", zec_text(supply)));
    }
    if let Some(text) = details.and_then(|details| pools_text(&details.value_pools)) {
        rows.push(chain_row("Value pools", text));
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
        let node = state.node_untracked().id();
        spawn_local(async move {
            if let Err(error) = api::open_path(OpenTarget::DataDir, Some(&node)).await {
                open_error.hold(format!("Could not open the data folder: {error}"));
            }
        });
    };
    // The best block's Copy button: "Copied" for a moment, or the failure in its place.
    let copied = Transient::<Result<(), String>>::new();
    let copy = move |text: String| {
        spawn_local(async move {
            match copy_to_clipboard(&text).await {
                Ok(()) => copied.show_for(Ok(()), COPIED_FOR),
                Err(error) => copied.hold(Err(format!("Could not copy: {error}"))),
            }
        });
    };

    // Rebuilt when the status, stats, settings, metrics facts or log facts change; the cells
    // that show a time follow the clock on their own.
    let node_rows = move || {
        let status = state.node().status.get();
        let live = matches!(
            status.phase,
            Phase::Running | Phase::Starting | Phase::Stopping
        );
        let since = status.started_at.map(local_datetime_short);
        let mut rows: Vec<Row> = Vec::new();
        {
            let status = status.clone();
            rows.push((
                "Phase",
                (move || phase_text(&status, state.now.get(), since.as_deref())).into_any(),
            ));
        }
        let version = state
            .node()
            .stats
            .with(|stats| stats.as_ref().map(|stats| stats.zebrad_version.clone()));
        let last_version = state
            .node()
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
            let subversion = dash.details.with(|details| {
                details
                    .as_ref()
                    .and_then(|details| details.subversion.clone())
            });
            if let Some(version) = version.or(last_version) {
                rows.push((
                    "zebrad",
                    text_cell(zebrad_text(&version, subversion.as_deref())),
                ));
            }
            // The active upgrade from the RPC, else the one zebrad's progress log names.
            let upgrade = dash
                .details
                .with(|details| {
                    details
                        .as_ref()
                        .and_then(|details| details.active_upgrade.clone())
                })
                .or_else(|| {
                    state.node().insights.with(|insights| {
                        insights
                            .sync()
                            .and_then(|sync| sync.network_upgrade.clone())
                    })
                });
            if let Some(upgrade) = upgrade {
                rows.push(("Network upgrade", text_cell(upgrade)));
            }
            let eos = state
                .node()
                .stats
                .with(|stats| stats.as_ref().and_then(|stats| stats.eos.clone()));
            if let Some(eos) = eos {
                let date = local_date_short(eos.estimated_time);
                rows.push((
                    "End of support",
                    (move || eos_text(&eos, &date, state.now.get())).into_any(),
                ));
            }
            let data_dir = (state.nodes_loaded.get()
                && state.default_data_dir.with(Option::is_some))
            .then(|| data_folder_text(state.chosen_data_dir(state.node()).as_deref()));
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
            let network = state.node().metrics_history.with(|history| {
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
            .node()
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
            .node()
            .insights
            .with(|insights| insights.checkpoints().final_reached.map(local_hhmm));
        let facts = dash.metrics.get();
        state.node().stats.with(|stats| {
            dash.details.with(|details| {
                chain_rows(
                    stats.as_ref(),
                    dash.latest.get().as_ref(),
                    details.as_ref(),
                    facts.as_ref(),
                    full_since.as_deref(),
                )
            })
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
                } else if let Some(full) = row.full {
                    let to_copy = full.clone();
                    view! {
                        <span class="dash-row-copy">
                            <span class="dash-hash" title=full>{row.text}</span>
                            <button class="link" type="button" on:click=move |_| copy(to_copy.clone())>
                                "Copy"
                            </button>
                            <span class="dash-copy-note" role="status">
                                {move || {
                                    copied.get().map(|outcome| match outcome {
                                        Ok(()) => view! { <span>"Copied"</span> }.into_any(),
                                        Err(error) => view! { <span class="error">{error}</span> }.into_any(),
                                    })
                                }}
                            </span>
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
                {move || view! { <Rows rows=node_rows() wide=&["Exit detail"] /> }}
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

    fn details() -> NodeDetails {
        NodeDetails {
            peers: Vec::new(),
            best_block_hash: Some(
                "00000000012d1f2a7b4e7a0d0ffb3a2b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e0a3f1".into(),
            ),
            headers: Some(2_952_681),
            difficulty: Some(118_462_377.5),
            size_on_disk: Some(248_300_000_000),
            chain_supply_zec: Some(21_234_567.891),
            value_pools: vec![
                ValuePool {
                    id: "transparent".into(),
                    zec: 13_980_123.45,
                },
                ValuePool {
                    id: "sapling".into(),
                    zec: 512_345.67,
                },
                ValuePool {
                    id: "orchard".into(),
                    zec: 1_654_321.0,
                },
            ],
            active_upgrade: Some("NU6.1".into()),
            subversion: Some("/Zebra:7.0.0/".into()),
            protocol_version: Some(170_120),
            mempool_bytes: Some(44_400),
            mempool_usage: None,
            updated_at: at(0),
        }
    }

    #[test]
    fn detail_sentences() {
        assert_eq!(
            zebrad_text("7.0.0", Some("/Zebra:7.0.0/")),
            "7.0.0 · /Zebra:7.0.0/"
        );
        assert_eq!(zebrad_text("7.0.0", None), "7.0.0");
        assert_eq!(zebrad_text("7.0.0", Some("7.0.0")), "7.0.0");
        assert_eq!(short_hash("00000000abcd1234a3f1"), "0000…a3f1");
        assert_eq!(short_hash("abcdef"), "abcdef", "too short to cut");
        assert_eq!(difficulty_text(118_462_377.5), "118,462,378");
        assert_eq!(difficulty_text(1.0), "1.00");
        assert_eq!(difficulty_text(f64::NAN), "0");
        assert_eq!(zec_text(21_234_567.891), "21,234,567.89 ZEC");
        assert_eq!(zec_text(0.5), "0.50 ZEC");
        assert_eq!(zec_text(-1.0), "0.00 ZEC");
        assert_eq!(
            pools_text(&details().value_pools),
            Some("transparent 14.0M · sapling 512K · orchard 1.65M ZEC".into())
        );
        assert_eq!(pools_text(&[]), None);
        assert_eq!(mempool_text(24, Some(1_234_000)), "24 transactions, 1.2 MB");
        assert_eq!(mempool_text(1, None), "1 transaction");
        assert_eq!(mempool_text(1_200, None), "1,200 transactions");
    }

    #[test]
    fn chain_rows_from_stats_or_the_last_sample() {
        let rows = chain_rows(
            Some(&stats(2_952_680)),
            None,
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
                ("Mempool", "24 transactions"),
            ]
        );
        assert!(rows.iter().all(|r| !r.warning && r.full.is_none()));
        let fork = chain_rows(
            Some(&stats(10)),
            None,
            None,
            Some(&facts(None, Some(2))),
            None,
        );
        let forked = fork.iter().find(|r| r.label == "Chains in memory").unwrap();
        assert_eq!(forked.text, "2 · a fork is being resolved");
        assert!(forked.warning);
        // With the details: the whole card, in the specification's order.
        let details = details();
        let rows = chain_rows(Some(&stats(2_952_680)), None, Some(&details), None, None);
        let texts: Vec<_> = rows.iter().map(|r| (r.label, r.text.as_str())).collect();
        assert_eq!(
            texts,
            vec![
                ("Chain", "main"),
                ("Height", "2,952,680"),
                ("Estimated tip", "3,120,000"),
                ("Headers", "2,952,681"),
                ("Best block", "0000…a3f1"),
                ("Difficulty", "118,462,378"),
                ("Size on disk", "248.3 GB"),
                ("Mempool", "24 transactions, 44.4 kB"),
                ("Chain supply", "21,234,567.89 ZEC"),
                (
                    "Value pools",
                    "transparent 14.0M · sapling 512K · orchard 1.65M ZEC"
                ),
            ]
        );
        let best = rows.iter().find(|r| r.label == "Best block").unwrap();
        assert_eq!(best.full.as_deref(), details.best_block_hash.as_deref());
        // Stopped: the last sample, without a chain name.
        let sample = Sample::from_stats(&stats(2_952_680));
        let rows = chain_rows(None, Some(&sample), None, None, None);
        let labels: Vec<_> = rows.iter().map(|r| r.label).collect();
        assert_eq!(labels, vec!["Height", "Estimated tip", "Mempool"]);
        assert!(chain_rows(None, None, None, None, None).is_empty());
    }
}
