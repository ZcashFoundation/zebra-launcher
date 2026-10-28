//! The range row (specification 3.4): the range control and the freshness sentence; and the
//! tile row (3.5): Height, Blocks per minute, Peers, Mempool, Disk and Uptime, each a
//! [`StatTile`] with the tone the specification gives. The sentences are pure functions,
//! tested natively.

use std::time::{Duration, SystemTime};

use launcher_types::{ExitInfo, Phase};
use leptos::prelude::*;

use crate::{
    charts::{
        MAX_POINTS, Range, compact, downsample, phase_note, signed, signed_bytes, uptime_text,
        widgets::{RangeControl, StatTile, Tone},
    },
    clock,
    format::{bytes, exit_class_text, local_datetime_short, relative, thousands},
    health::STALE_AFTER,
    stats_history::Field,
};

use super::{Dash, Icon};

/// The verdict rules that drive the Peers tile's warning tone, so tile and verdict agree.
pub const PEER_RULES: [u8; 2] = [8, 12];

/// What the freshness sentence says and whether it warns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Freshness {
    pub icon: Option<Icon>,
    pub warning: bool,
    pub text: String,
}

/// The freshness sentence: when the RPC, the metrics page and the log last answered, against
/// `now`; a warning once the RPC is silent for [`STALE_AFTER`] while zebrad runs (the limit of
/// the verdict's rule 7, so the two agree). In any other phase, that phase's note and the
/// last answer.
pub fn freshness(
    phase: &Phase,
    rpc_at: Option<SystemTime>,
    metrics_at: Option<SystemTime>,
    log_at: Option<SystemTime>,
    now: SystemTime,
) -> Freshness {
    let plain = |text: String| Freshness {
        icon: None,
        warning: false,
        text,
    };
    if let Some(note) = phase_note(phase) {
        return plain(match rpc_at {
            Some(at) => format!("{note} · last answer {}", relative(at, now)),
            None => note.to_owned(),
        });
    }
    let Some(rpc_at) = rpc_at else {
        return plain("Waiting for the first answer".to_owned());
    };
    let age = now.duration_since(rpc_at).unwrap_or(Duration::ZERO);
    if age > STALE_AFTER {
        return Freshness {
            icon: Some(Icon::Warning),
            warning: true,
            text: format!("No RPC answer for {}", crate::format::rough_duration(age)),
        };
    }
    let mut parts = vec![format!("RPC answered {}", relative(rpc_at, now))];
    if let Some(at) = metrics_at {
        parts.push(format!("metrics {}", relative(at, now)));
    }
    if let Some(at) = log_at {
        parts.push(format!("log {}", relative(at, now)));
    }
    plain(parts.join(" · "))
}

/// A blocks-per-minute figure: whole with separators from 10 up, one decimal below (the
/// chain's own pace at the tip is under one block a minute).
pub fn rate_text(rate: f64) -> String {
    let rate = rate.max(0.0);
    if rate >= 9.95 {
        thousands(rate.round() as u64)
    } else {
        format!("{rate:.1}")
    }
}

/// `+2,640 in the last hour`, or an em dash without two samples.
pub fn delta_text(delta: Option<i64>, range: Range) -> String {
    delta.map_or_else(
        || "—".to_owned(),
        |delta| format!("{} {}", signed(delta), range.phrase()),
    )
}

/// `last seen 12 min ago`: a tile's detail while zebrad is stopped.
pub fn last_seen_text(at: SystemTime, now: SystemTime) -> String {
    format!("last seen {}", relative(at, now))
}

/// The Blocks per minute tile's detail: the average's window, or the rate zebrad's own log
/// reports while the window is too short to estimate.
pub fn rate_detail(rate: Option<f64>, from_logs: Option<f64>, range: Range) -> String {
    match (rate, from_logs) {
        (Some(_), _) => format!("average {}", range.phrase()),
        (None, Some(logged)) => format!("from logs: {}", rate_text(logged)),
        (None, None) => format!("average {}", range.phrase()),
    }
}

/// The mempool is not active yet: zebrad activates it near the tip, so an empty mempool far
/// from the tip in a run without an activation line is expected, not empty.
pub fn mempool_inactive(active_since: Option<SystemTime>, at_tip: bool, count: u64) -> bool {
    active_since.is_none() && !at_tip && count == 0
}

/// `1.2 MB · +14 in the last hour`.
pub fn mempool_detail(size: Option<u64>, delta: Option<i64>, range: Range) -> String {
    match size {
        Some(size) => format!("{} · {}", bytes(size), delta_text(delta, range)),
        None => delta_text(delta, range),
    }
}

/// `+1.1 GB in the last hour`; without a delta yet, how the number arrives.
pub fn disk_detail(delta: Option<i64>, range: Range) -> String {
    match delta {
        Some(delta) => format!("{} {}", signed_bytes(delta), range.phrase()),
        None => "from zebrad's metrics, every 10 s".to_owned(),
    }
}

/// The Peers tile's detail: `8 outbound · 4 inbound` from the peer list of the newest poll,
/// or the delta over the range until a list lands.
pub fn peers_detail(split: Option<(u32, u32)>, delta: Option<i64>, range: Range) -> String {
    match split {
        Some((inbound, outbound)) => format!("{outbound} outbound · {inbound} inbound"),
        None => delta_text(delta, range),
    }
}

/// `since 23 Oct, 21:13 · 2 restarts`; the restarts only when there were any.
pub fn uptime_detail(since: &str, restarts: usize) -> String {
    match restarts {
        0 => format!("since {since}"),
        1 => format!("since {since} · 1 restart"),
        n => format!("since {since} · {n} restarts"),
    }
}

/// The Uptime tile's detail while zebrad is stopped: how the last run ended.
pub fn stopped_detail(exit: Option<&ExitInfo>, now: SystemTime) -> String {
    match exit {
        Some(exit) => {
            let code = exit.code.map_or_else(
                || "killed by a signal".to_owned(),
                |code| format!("code {code}"),
            );
            format!(
                "{} {} ({code})",
                exit_class_text(exit.class),
                relative(exit.at, now)
            )
        }
        None => "not started in this session".to_owned(),
    }
}

/// The range control with the freshness sentence at its right.
#[component]
pub(super) fn RangeRow(dash: Dash) -> impl IntoView {
    let state = dash.state;
    let fresh = Memo::new(move |_| {
        let now = state.now.get();
        // A sample from before this run says nothing about this run's RPC: until the first
        // answer the sentence waits, as the verdict's rule 7 does.
        let started = state.status.with(|status| status.started_at);
        let rpc_at = dash
            .latest
            .get()
            .map(|sample| sample.at)
            .filter(|at| started.is_none_or(|started| *at >= started));
        let metrics_at = dash.metrics.get().map(|facts| facts.at);
        let log_at = state.insights.with(|insights| insights.last_at());
        state
            .status
            .with(|status| freshness(&status.phase, rpc_at, metrics_at, log_at, now))
    });
    view! {
        <div class="dash-range-row">
            <RangeControl value=dash.range />
            <p class=move || {
                if fresh.with(|f| f.warning) { "dash-fresh dash-fresh-warning" } else { "dash-fresh" }
            }>
                {move || fresh.with(|f| f.icon).map(Icon::view)}
                <span>{move || fresh.with(|f| f.text.clone())}</span>
            </p>
        </div>
    }
}

/// The six tiles.
#[component]
pub(super) fn Tiles(dash: Dash) -> impl IntoView {
    let state = dash.state;
    let latest = dash.latest;
    let range = dash.range;
    let running = Signal::derive(move || dash.running());
    let stopped_tone = move |tone: Tone| if running.get() { tone } else { Tone::Muted };

    let delta = move |field: Field| {
        let now = state.now.get();
        state
            .history
            .with(|history| history.delta(field, dash.span(), now))
    };
    // A counted field's detail: the delta over the range while running, when the last sample
    // was taken once zebrad stopped.
    let counted_detail = move |field: Field| {
        Signal::derive(move || match latest.get() {
            Some(sample) if !running.get() => last_seen_text(sample.at, state.now.get()),
            _ => delta_text(delta(field), range.get()),
        })
    };
    let spark = move |field: Field| {
        Signal::derive(move || {
            dash.window.with(|window| {
                downsample(window, MAX_POINTS)
                    .iter()
                    .map(|s| s.get(field) as f64)
                    .collect::<Vec<f64>>()
            })
        })
    };
    let dash_or = move |pick: fn(&crate::stats_history::Sample) -> String| {
        Signal::derive(move || latest.get().map_or_else(|| "—".to_owned(), |s| pick(&s)))
    };
    let value_tone = Signal::derive(move || {
        if latest.get().is_none() {
            Tone::Muted
        } else {
            stopped_tone(Tone::Normal)
        }
    });

    // Blocks per minute over the range, from zebrad's own verified-blocks counter when the
    // metrics page gives one (gossiped blocks count too), else from the RPC heights; the
    // hero's pace is the status card's.
    let rate = Memo::new(move |_| {
        let now = state.now.get();
        let span = dash.span();
        state
            .metrics_history
            .with(|metrics| metrics.verified_block_rate(span, now))
            .or_else(|| state.history.with(|history| history.block_rate(span, now)))
    });
    let rate_value = Signal::derive(move || {
        if !running.get() {
            "—".to_owned()
        } else {
            rate.get()
                .map_or_else(|| "estimating…".to_owned(), rate_text)
        }
    });
    let rate_tone = Signal::derive(move || {
        if running.get() && rate.get().is_some() {
            Tone::Normal
        } else {
            Tone::Muted
        }
    });
    // The log's own rate stands in while the window is too short; a stopped node's log rate
    // is a stale fact, so the detail names the window instead.
    let rate_detail = Signal::derive(move || {
        let from_logs = if running.get() {
            state
                .insights
                .with(|insights| insights.sync_rate_from_logs())
        } else {
            None
        };
        rate_detail(rate.get(), from_logs, range.get())
    });

    // Peers: by direction once the peer list landed; the warning tone follows the verdict's
    // peer rules.
    let peers_detail = Signal::derive(move || match latest.get() {
        Some(sample) if !running.get() => last_seen_text(sample.at, state.now.get()),
        Some(sample) => peers_detail(sample.peer_split(), delta(Field::Peers), range.get()),
        None => delta_text(None, range.get()),
    });
    let peers_tone = Signal::derive(move || {
        if latest.get().is_none() {
            Tone::Muted
        } else if running.get() && dash.verdict.with(|v| PEER_RULES.contains(&v.rule)) {
            Tone::Warning
        } else {
            stopped_tone(Tone::Normal)
        }
    });

    // Mempool: muted with its reason while zebrad has not activated it yet.
    let mempool_inactive = Memo::new(move |_| {
        let Some(sample) = latest.get() else {
            return false;
        };
        let active_since = state
            .insights
            .with(|insights| insights.mempool().active_since);
        mempool_inactive(active_since, sample.at_tip(), sample.mempool_transactions)
    });
    let mempool_tone = Signal::derive(move || {
        if latest.get().is_none() || mempool_inactive.get() {
            Tone::Muted
        } else {
            stopped_tone(Tone::Normal)
        }
    });
    let mempool_detail = Signal::derive(move || {
        if mempool_inactive.get() {
            return "activates near the chain tip".to_owned();
        }
        match latest.get() {
            Some(sample) if !running.get() => last_seen_text(sample.at, state.now.get()),
            _ => {
                // The bytes from the poll that gave the count; the metrics gauge otherwise.
                let size = dash
                    .details
                    .with(|details| details.as_ref().and_then(|details| details.mempool_bytes))
                    .or_else(|| dash.metrics.get().and_then(|facts| facts.mempool_bytes));
                mempool_detail(size, delta(Field::Mempool), range.get())
            }
        }
    });

    // Disk: the state database's size from the metrics page, its growth over the range.
    let disk_bytes = Memo::new(move |_| dash.metrics.get().and_then(|facts| facts.disk_bytes));
    let disk_value = Signal::derive(move || disk_bytes.get().map_or_else(|| "—".to_owned(), bytes));
    let disk_tone = Signal::derive(move || {
        if disk_bytes.get().is_none() {
            Tone::Muted
        } else {
            stopped_tone(Tone::Normal)
        }
    });
    let disk_detail = Signal::derive(move || {
        if disk_bytes.get().is_none() {
            return "waiting for zebrad's metrics".to_owned();
        }
        let now = state.now.get();
        let delta = state
            .metrics_history
            .with(|history| history.disk_delta(dash.span(), now));
        disk_detail(delta, range.get())
    });

    // Uptime: since the run was spawned; stopped, how the last run ended.
    let started = Memo::new(move |_| state.status.with(|status| status.started_at));
    let uptime_value = Signal::derive(move || match started.get() {
        Some(started) => {
            let now = state.now.get();
            uptime_text(now.duration_since(started).unwrap_or(Duration::ZERO))
        }
        None => "—".to_owned(),
    });
    let uptime_tone = Signal::derive(move || {
        if started.get().is_some() {
            Tone::Normal
        } else {
            Tone::Muted
        }
    });
    let since = Memo::new(move |_| started.get().map(local_datetime_short));
    // The exact start as the value's tooltip, beside the short form in the detail.
    let started_full = Signal::derive(move || {
        started.get().map_or_else(String::new, |started| {
            format!("started {}", clock::local_string(started))
        })
    });
    let uptime_detail = Signal::derive(move || match since.get() {
        Some(since) => {
            let restarts = state
                .insights
                .with(|insights| insights.runs().len().saturating_sub(1));
            uptime_detail(&since, restarts)
        }
        None => {
            let now = state.now.get();
            state
                .status
                .with(|status| stopped_detail(status.last_exit.as_ref(), now))
        }
    });

    view! {
        <div class="tile-row" role="group" aria-label="Node statistics">
            <StatTile
                label="Height"
                value=dash_or(|s| compact(s.height))
                full_value=dash_or(|s| thousands(s.height))
                detail=counted_detail(Field::Height)
                spark=spark(Field::Height)
                tone=value_tone
            />
            <StatTile label="Blocks per minute" value=rate_value detail=rate_detail tone=rate_tone />
            <StatTile
                label="Peers"
                value=dash_or(|s| s.peers.to_string())
                detail=peers_detail
                spark=spark(Field::Peers)
                tone=peers_tone
            />
            <StatTile
                label="Mempool"
                value=dash_or(|s| compact(s.mempool_transactions))
                full_value=dash_or(|s| thousands(s.mempool_transactions))
                detail=mempool_detail
                spark=spark(Field::Mempool)
                tone=mempool_tone
            />
            <StatTile label="Disk" value=disk_value detail=disk_detail tone=disk_tone />
            <StatTile
                label="Uptime"
                value=uptime_value
                full_value=started_full
                detail=uptime_detail
                tone=uptime_tone
            />
        </div>
    }
}

#[cfg(test)]
mod tests {
    use std::time::UNIX_EPOCH;

    use launcher_types::ExitClass;

    use super::*;

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn freshness_sentences() {
        let now = at(1000);
        let running = Phase::Running;
        let fresh = freshness(&running, Some(at(997)), Some(at(992)), Some(at(999)), now);
        assert_eq!(
            fresh,
            Freshness {
                icon: None,
                warning: false,
                text: "RPC answered 3 s ago · metrics 8 s ago · log just now".into()
            }
        );
        // Sources that have not answered are left out.
        assert_eq!(
            freshness(&running, Some(at(997)), None, None, now).text,
            "RPC answered 3 s ago"
        );
        let stale = freshness(&running, Some(at(958)), Some(at(995)), None, now);
        assert!(stale.warning);
        assert_eq!(stale.icon, Some(Icon::Warning));
        assert_eq!(stale.text, "No RPC answer for 42 s");
        assert_eq!(
            freshness(&running, Some(at(970)), None, None, now).text,
            "RPC answered 30 s ago",
            "exactly the limit is not stale yet"
        );
        assert_eq!(
            freshness(&running, None, None, None, now).text,
            "Waiting for the first answer"
        );
        let stopped = freshness(&Phase::Stopped, Some(at(280)), None, None, now);
        assert!(!stopped.warning);
        assert_eq!(stopped.text, "zebrad is stopped · last answer 12 min ago");
        assert_eq!(
            freshness(&Phase::Stopped, None, None, None, now).text,
            "zebrad is stopped"
        );
        assert_eq!(
            freshness(&Phase::Starting, None, None, None, now).text,
            "zebrad is starting"
        );
        assert_eq!(
            freshness(&Phase::Stopping, Some(at(999)), None, None, now).text,
            "zebrad is stopping · last answer just now"
        );
    }

    #[test]
    fn rates_and_deltas() {
        assert_eq!(rate_text(3_747.4), "3,747");
        assert_eq!(rate_text(10.0), "10");
        assert_eq!(rate_text(9.96), "10");
        assert_eq!(rate_text(0.84), "0.8");
        assert_eq!(rate_text(-2.0), "0.0");
        assert_eq!(
            delta_text(Some(2_640), Range::Hour1),
            "+2,640 in the last hour"
        );
        assert_eq!(delta_text(Some(-3), Range::Min15), "-3 in the last 15 min");
        assert_eq!(delta_text(None, Range::All), "—");
        assert_eq!(
            rate_detail(Some(3_747.0), Some(997.0), Range::Hour1),
            "average in the last hour"
        );
        assert_eq!(
            rate_detail(None, Some(997.4), Range::Hour1),
            "from logs: 997"
        );
        assert_eq!(
            rate_detail(None, None, Range::Hour4),
            "average in the last 4 h"
        );
        assert_eq!(last_seen_text(at(280), at(1000)), "last seen 12 min ago");
    }

    #[test]
    fn mempool_words() {
        assert!(mempool_inactive(None, false, 0));
        assert!(!mempool_inactive(Some(at(0)), false, 0));
        assert!(!mempool_inactive(None, true, 0));
        assert!(!mempool_inactive(None, false, 3));
        assert_eq!(
            mempool_detail(Some(1_234_000), Some(14), Range::Hour1),
            "1.2 MB · +14 in the last hour"
        );
        assert_eq!(
            mempool_detail(None, Some(14), Range::Hour1),
            "+14 in the last hour"
        );
        assert_eq!(mempool_detail(Some(0), None, Range::Hour1), "0 bytes · —");
    }

    #[test]
    fn disk_and_uptime_words() {
        assert_eq!(
            disk_detail(Some(1_100_000_000), Range::Hour1),
            "+1.1 GB in the last hour"
        );
        assert_eq!(
            disk_detail(Some(-200_000_000), Range::Min15),
            "-200.0 MB in the last 15 min"
        );
        assert_eq!(
            disk_detail(None, Range::Hour1),
            "from zebrad's metrics, every 10 s"
        );
        assert_eq!(
            disk_detail(Some(0), Range::Hour1),
            "0 bytes in the last hour"
        );
        assert_eq!(
            peers_detail(Some((4, 8)), Some(6), Range::Hour1),
            "8 outbound · 4 inbound"
        );
        assert_eq!(
            peers_detail(None, Some(6), Range::Hour1),
            "+6 in the last hour"
        );
        assert_eq!(peers_detail(None, None, Range::Hour1), "—");
        assert_eq!(uptime_detail("23 Oct, 21:13", 0), "since 23 Oct, 21:13");
        assert_eq!(
            uptime_detail("23 Oct, 21:13", 1),
            "since 23 Oct, 21:13 · 1 restart"
        );
        assert_eq!(
            uptime_detail("23 Oct, 21:13", 2),
            "since 23 Oct, 21:13 · 2 restarts"
        );
        let now = at(1000);
        let exit = ExitInfo {
            code: Some(0),
            class: ExitClass::Clean,
            detail: None,
            at: at(280),
            requested: true,
        };
        assert_eq!(
            stopped_detail(Some(&exit), now),
            "stopped cleanly 12 min ago (code 0)"
        );
        let killed = ExitInfo {
            code: None,
            class: ExitClass::Crash,
            ..exit
        };
        assert_eq!(
            stopped_detail(Some(&killed), now),
            "zebrad crashed 12 min ago (killed by a signal)"
        );
        assert_eq!(stopped_detail(None, now), "not started in this session");
    }
}
