//! The dashboard: the end-of-support banners, the sync hero, the range control, stat tiles,
//! charts of the node statistics, and the node details.

use std::time::Duration;

use launcher_types::{EOS_WARNING_WINDOW, ExitClass, Phase, eos_within};
use leptos::prelude::*;

use crate::{
    app::{AppState, Tab},
    charts::{
        ChartData, MAX_POINTS, Range, compact, downsample,
        line_chart::LineChart,
        phase_note, signed, sync_summary, uptime_text,
        widgets::{Meter, RangeControl, StatTile, Tone, check_icon},
    },
    clock,
    components::{
        log_insights::{LogActivityChart, RecentProblems, SyncStageCard},
        rows::Rows,
        status_strip::phase_detail,
        updates::CheckForUpdate,
    },
    format::{exit_class_text, phase_label, relative, thousands},
    stats_history::{Field, Sample},
};

/// A last RPC update older than this gets a warning while zebrad runs.
const STALE_AFTER: Duration = Duration::from_secs(30);

type Fields = &'static [(&'static str, &'static str, Field)];

const HEIGHT_SERIES: Fields = &[
    ("Height", "chart-series-1", Field::Height),
    (
        "Estimated height",
        "chart-series-muted",
        Field::EstimatedHeight,
    ),
];
const PEER_SERIES: Fields = &[("Peers", "chart-series-1", Field::Peers)];
const MEMPOOL_SERIES: Fields = &[("Mempool transactions", "chart-series-1", Field::Mempool)];

#[component]
pub fn Dashboard() -> impl IntoView {
    let state = AppState::get();

    let eos_text = move || {
        let (version, eos) = state.stats.with(|stats| {
            let stats = stats.as_ref()?;
            Some((stats.zebrad_version.clone(), stats.eos.clone()?))
        })?;
        let when = relative(eos.estimated_time, state.now.get());
        Some(format!(
            "zebrad {version} stops working at block {} (estimated {}, {when}). Update the launcher before then.",
            thousands(eos.block_height),
            clock::local_string(eos.estimated_time),
        ))
    };
    let eos_soon = Memo::new(move |_| {
        let now = state.now.get();
        state.stats.with(|stats| {
            stats
                .as_ref()
                .and_then(|stats| stats.eos.as_ref())
                // Inside the backend's warning window, the banner offers "Check for updates",
                // whatever the update mode.
                .is_some_and(|eos| eos_within(eos.estimated_time, now, EOS_WARNING_WINDOW))
        })
    });
    let halted = Memo::new(move |_| {
        state.status.with(|status| {
            status.phase
                == Phase::Failed {
                    class: ExitClass::EosHalt,
                }
        })
    });
    // Separate from the text, which changes every second, so the button keeps its state.
    let eos_banner = view! {
        <Show when=move || eos_text().is_some()>
            <div class="banner banner-warn eos-banner" role="status">
                <span>
                    <strong>"End of support approaching. "</strong>
                    {eos_text}
                </span>
                <Show when=move || eos_soon.get()>
                    <div class="buttons">
                        <CheckForUpdate />
                    </div>
                </Show>
            </div>
        </Show>
        <Show when=move || halted.get()>
            <div class="banner banner-error eos-banner" role="alert">
                <span>
                    <strong>"This zebrad release is past its end of support. "</strong>
                    "It refuses to run, and restarting cannot help. A launcher update brings a newer zebrad."
                </span>
                <div class="buttons">
                    <CheckForUpdate />
                </div>
            </div>
        </Show>
    };

    // The range scopes the hero's rate and ETA, the tiles' deltas and sparklines, and the
    // charts. The window slides with the clock, so it is recomputed every second.
    let range = RwSignal::new(Range::default());
    let span = move || range.get().span();
    let window = Memo::new(move |_| {
        let now = state.now.get();
        let span = span();
        state
            .history
            .with(|history| history.window(span, now).to_vec())
    });
    let rate = Memo::new(move |_| {
        let now = state.now.get();
        state
            .history
            .with(|history| history.block_rate(span(), now))
    });
    let eta = Memo::new(move |_| {
        let now = state.now.get();
        state
            .history
            .with(|history| history.eta_to_tip(span(), now))
    });
    // The newest numbers: the published stats, or the last sample once zebrad stopped
    // publishing, so the hero and tiles keep them.
    let latest = Memo::new(move |_| {
        state
            .stats
            .with(|stats| stats.as_ref().map(Sample::from_stats))
            .or_else(|| state.history.with(|history| history.latest().copied()))
    });
    let hero = Memo::new(move |_| {
        let note = state.status.with(|status| phase_note(&status.phase));
        sync_summary(latest.get().as_ref(), rate.get(), eta.get(), note)
    });

    let value = move |pick: fn(&Sample) -> String| {
        Signal::derive(move || latest.get().map_or_else(|| "—".to_owned(), |s| pick(&s)))
    };
    let delta = move |field: Field| {
        Signal::derive(move || {
            let now = state.now.get();
            state
                .history
                .with(|history| history.delta(field, span(), now))
                .map_or_else(
                    || "—".to_owned(),
                    |delta| format!("{} {}", signed(delta), range.get().phrase()),
                )
        })
    };
    let spark = move |field: Field| {
        Signal::derive(move || {
            window.with(|window| {
                downsample(window, MAX_POINTS)
                    .iter()
                    .map(|s| s.get(field) as f64)
                    .collect::<Vec<f64>>()
            })
        })
    };

    let rate_value = Signal::derive(move || {
        rate.get().map_or_else(
            || "estimating…".to_owned(),
            |rate| thousands(rate.round().max(0.0) as u64),
        )
    });
    let rate_tone = Signal::derive(move || {
        if rate.get().is_some() {
            Tone::Normal
        } else {
            Tone::Muted
        }
    });
    let rate_detail = Signal::derive(move || format!("average {}", range.get().phrase()));

    let started = move || state.status.with(|status| status.started_at);
    let uptime_value = Signal::derive(move || match started() {
        Some(started) => {
            let now = state.now.get();
            uptime_text(now.duration_since(started).unwrap_or(Duration::ZERO))
        }
        None => "—".to_owned(),
    });
    let uptime_tone = Signal::derive(move || {
        if started().is_some() {
            Tone::Normal
        } else {
            Tone::Muted
        }
    });
    let uptime_detail = Signal::derive(move || match started() {
        Some(started) => format!("started {}", clock::local_string(started)),
        None => "zebrad is not running".to_owned(),
    });

    let running = move || state.status.with(|status| status.phase == Phase::Running);
    let stale = move || {
        running()
            && latest.get().is_some_and(|sample| {
                state
                    .now
                    .get()
                    .duration_since(sample.at)
                    .is_ok_and(|age| age > STALE_AFTER)
            })
    };
    let update_value = Signal::derive(move || {
        latest
            .get()
            .map_or_else(|| "—".to_owned(), |s| relative(s.at, state.now.get()))
    });
    let update_tone = Signal::derive(move || if stale() { Tone::Warning } else { Tone::Muted });
    let update_detail = Signal::derive(move || {
        if latest.get().is_none() {
            "waiting for the first answer".to_owned()
        } else if stale() {
            "no answer for over 30 s".to_owned()
        } else if running() {
            "polled every 5 s".to_owned()
        } else {
            "zebrad is not running".to_owned()
        }
    });

    let chart = move |fields: Fields| {
        Memo::new(move |_| window.with(|window| ChartData::from_samples(window, fields)))
    };
    let empty = Signal::derive(move || {
        let has_history = state.history.with(|history| !history.is_empty());
        if has_history && window.with(Vec::is_empty) {
            format!("No samples {}", range.get().phrase())
        } else {
            "Collecting samples… updates every 5 s".to_owned()
        }
    });

    let node_rows = move || {
        let now = state.now.get();
        state.status.with(|status| {
            let mut rows = vec![(
                "Phase",
                match phase_detail(status, now) {
                    Some(detail) => format!("{} — {detail}", phase_label(&status.phase)),
                    None => phase_label(&status.phase).to_owned(),
                },
            )];
            if let Some(pid) = status.pid {
                rows.push(("PID", pid.to_string()));
            }
            if let Some(started) = status.started_at {
                rows.push(("Started", relative(started, now)));
            }
            if let Some(addr) = status.rpc_addr {
                rows.push(("RPC", addr.to_string()));
            }
            if let Some(addr) = status.metrics_addr {
                rows.push(("Metrics", addr.to_string()));
            }
            if let Some(exit) = &status.last_exit {
                let code = exit.code.map_or_else(
                    || "killed by a signal".to_owned(),
                    |code| format!("code {code}"),
                );
                rows.push((
                    "Last exit",
                    format!(
                        "{} ({code}), {}",
                        exit_class_text(exit.class),
                        relative(exit.at, now)
                    ),
                ));
                if let Some(detail) = &exit.detail {
                    rows.push(("Exit detail", detail.clone()));
                }
            }
            rows
        })
    };

    // Only what the tiles do not show. One derived signal, so the rows update in place.
    let chain_rows = Signal::derive(move || {
        let now = state.now.get();
        state.stats.with(|stats| {
            stats.as_ref().map_or_else(Vec::new, |s| {
                vec![
                    ("Chain", s.chain.clone()),
                    ("Estimated height", thousands(s.estimated_height)),
                    ("zebrad", s.zebrad_version.clone()),
                    ("Updated", relative(s.updated_at, now)),
                ]
            })
        })
    });

    // A problem picked here is shown filtered on the Logs page.
    let show_in_logs = Callback::new(move |filter: String| {
        state.log_filter_request.set(Some(filter));
        state.tab.set(Tab::Logs);
    });

    let grafana = view! {
        <div class="card hint">
            <h3>"Grafana"</h3>
            <p>"Detailed charts of zebrad's Prometheus metrics live in the Grafana tab."</p>
            <button on:click=move |_| state.tab.set(Tab::Grafana)>"Open the Grafana tab"</button>
        </div>
    };

    view! {
        <div class="dashboard">
            {eos_banner}
            <section class="card dash-hero" aria-labelledby="dash-hero-label">
                <div class="dash-hero-head">
                    <h3 id="dash-hero-label">"Sync progress"</h3>
                    <span class=move || {
                        if hero.with(|hero| hero.waiting) { "dash-hero-figure muted" } else { "dash-hero-figure" }
                    }>
                        {move || hero.with(|hero| hero.figure.clone())}
                    </span>
                </div>
                <Meter fraction=Signal::derive(move || hero.with(|hero| hero.fraction)) label="Sync progress" />
                <p class=move || {
                    if hero.with(|hero| hero.waiting) { "dash-hero-line muted" } else { "dash-hero-line" }
                }>
                    {move || hero.with(|hero| hero.at_tip).then(check_icon)}
                    <span>{move || hero.with(|hero| hero.line.clone())}</span>
                </p>
            </section>
            <RangeControl value=range />
            <div class="tile-row" role="group" aria-label="Node statistics">
                <StatTile
                    label="Height"
                    value=value(|s| compact(s.height))
                    full_value=value(|s| thousands(s.height))
                    detail=delta(Field::Height)
                    spark=spark(Field::Height)
                />
                <StatTile
                    label="Peers"
                    value=value(|s| s.peers.to_string())
                    detail=delta(Field::Peers)
                    spark=spark(Field::Peers)
                />
                <StatTile
                    label="Mempool"
                    value=value(|s| compact(s.mempool_transactions))
                    full_value=value(|s| thousands(s.mempool_transactions))
                    detail=delta(Field::Mempool)
                    spark=spark(Field::Mempool)
                />
                <StatTile label="Blocks per minute" value=rate_value detail=rate_detail tone=rate_tone />
                <StatTile label="Uptime" value=uptime_value detail=uptime_detail tone=uptime_tone />
                <StatTile label="Last RPC update" value=update_value detail=update_detail tone=update_tone />
            </div>
            <div class="chart-grid">
                <LineChart
                    title="Block height"
                    subtitle="Verified blocks and zebrad's estimate of the chain tip"
                    data=chart(HEIGHT_SERIES)
                    empty=empty
                />
                <LineChart
                    title="Peers"
                    subtitle="Connected peers"
                    data=chart(PEER_SERIES)
                    empty=empty
                    from_zero=true
                    area=true
                />
                <LineChart
                    title="Mempool transactions"
                    subtitle="Transactions waiting in zebrad's mempool"
                    data=chart(MEMPOOL_SERIES)
                    empty=empty
                    from_zero=true
                    area=true
                />
            </div>
            <section class="dash-insights" aria-label="From zebrad's log">
                <SyncStageCard />
                <RecentProblems limit=5 on_pick=show_in_logs />
                <div class="card insight-card insight-activity">
                    <h3>"Lines per minute"</h3>
                    <LogActivityChart />
                </div>
            </section>
            <div class="cards">
                <div class="card">
                    <h3>"Node"</h3>
                    <Rows rows=Signal::derive(node_rows) />
                </div>
                <div class="card">
                    <h3>"Chain"</h3>
                    <Show
                        when=move || state.stats.with(Option::is_some)
                        fallback=|| view! { <p class="muted">"No data from zebrad's RPC yet."</p> }
                    >
                        <Rows rows=chain_rows />
                    </Show>
                </div>
            </div>
            {grafana}
        </div>
    }
}
