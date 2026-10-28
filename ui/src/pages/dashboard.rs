//! The dashboard: the gated end-of-support banners, the status card (health verdict and sync
//! progress), the range row, the six stat tiles, the two charts, the activity row (recent
//! problems, this run, peers), the node and chain details and the Grafana line, in the page
//! specification's order. The cards live in the `dashboard/` modules; this file holds the
//! signals they share, the chart data and the page order. Every slot renders in every state:
//! loading, running, stopped and refetching replace values in place.

mod activity;
mod details;
mod status_card;
mod tiles;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use launcher_types::{EOS_WARNING_WINDOW, EosInfo, ExitClass, MetricsSnapshot, Phase, eos_within};
use leptos::prelude::*;
use wasm_bindgen::JsValue;

use crate::{
    app::{AppState, Tab},
    charts::{ChartData, Range, Series, line_chart::LineChart},
    clock,
    components::{
        icons::{check_icon, clock_icon, cross_icon, warning_icon},
        updates::CheckForUpdate,
    },
    format::{relative, thousands},
    health::{HealthInput, Verdict, health_verdict},
    stats_history::{Field, Sample},
};

use self::{
    activity::ActivityRow, details::Details, status_card::StatusCard, tiles::RangeRow, tiles::Tiles,
};

/// The series of zebrad's metrics page the dashboard reads, by their wire names (zebrad
/// registers them dotted; its exporter writes the dots as underscores).
mod metric {
    pub const DISK_BYTES: &str = "zebra_state_rocksdb_total_disk_size_bytes";
    pub const MEMORY_BYTES: &str = "zebra_state_rocksdb_total_memory_size_bytes";
    pub const MEMPOOL_BYTES: &str = "zcash_mempool_size_bytes";
    pub const READY_PEERS: &str = "pool_num_ready";
    pub const UNREADY_PEERS: &str = "pool_num_unready";
    pub const OBSOLETE_PEERS: &str = "zcash_net_peers_version_obsolete";
    pub const HANDSHAKE_FAILURES: &str = "zcash_net_peer_handshake_failures_total";
    pub const CHECKPOINT_HEIGHT: &str = "checkpoint_verified_height";
    pub const CHAIN_COUNT: &str = "state_memory_chain_count";
}

/// The icons a state is carried by, beside its word; the status colour sits on the icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Icon {
    Check,
    Clock,
    Warning,
    Cross,
}

impl Icon {
    pub fn view(self) -> AnyView {
        match self {
            Icon::Check => check_icon().into_any(),
            Icon::Clock => clock_icon().into_any(),
            Icon::Warning => warning_icon().into_any(),
            Icon::Cross => cross_icon().into_any(),
        }
    }
}

/// The numbers the page reads from one snapshot of zebrad's metrics page. A gauge the page
/// does not carry is `None`; a non-finite or negative value counts as absent.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MetricsFacts {
    pub at: SystemTime,
    pub disk_bytes: Option<u64>,
    pub memory_bytes: Option<u64>,
    pub mempool_bytes: Option<u64>,
    pub ready_peers: Option<u64>,
    pub unready_peers: Option<u64>,
    pub obsolete_peers: Option<u64>,
    pub handshake_failures: Option<u64>,
    pub checkpoint_height: Option<u64>,
    pub chain_count: Option<u64>,
}

impl MetricsFacts {
    pub fn from_snapshot(snapshot: &MetricsSnapshot) -> Self {
        let whole = |value: f64| (value.is_finite() && value >= 0.0).then(|| value.round() as u64);
        let gauge = |name: &str| snapshot.gauge(name).and_then(whole);
        MetricsFacts {
            at: snapshot.at,
            disk_bytes: gauge(metric::DISK_BYTES),
            memory_bytes: gauge(metric::MEMORY_BYTES),
            mempool_bytes: gauge(metric::MEMPOOL_BYTES),
            ready_peers: gauge(metric::READY_PEERS),
            unready_peers: gauge(metric::UNREADY_PEERS),
            obsolete_peers: gauge(metric::OBSOLETE_PEERS),
            handshake_failures: snapshot.counter(metric::HANDSHAKE_FAILURES).and_then(whole),
            checkpoint_height: gauge(metric::CHECKPOINT_HEIGHT),
            chain_count: gauge(metric::CHAIN_COUNT),
        }
    }
}

/// The signals every card reads, created once by the page.
#[derive(Clone, Copy)]
pub(crate) struct Dash {
    pub state: AppState,
    /// The range control's value: it scopes the tile deltas and sparklines and the charts,
    /// never the hero's pace, the current values, the problems or the peers.
    pub range: RwSignal<Range>,
    /// The samples in the range, oldest first.
    pub window: Memo<Vec<Sample>>,
    /// The newest numbers: the published stats, or the last sample once zebrad stopped
    /// publishing, so the hero and tiles keep them.
    pub latest: Memo<Option<Sample>>,
    /// The health verdict, recomputed with every signal it reads.
    pub verdict: Memo<Verdict>,
    /// The facts from the newest metrics snapshot, kept while zebrad is stopped.
    pub metrics: Memo<Option<MetricsFacts>>,
}

impl Dash {
    /// zebrad is in [`Phase::Running`].
    pub fn running(self) -> bool {
        self.state
            .status
            .with(|status| status.phase == Phase::Running)
    }

    /// The range's span, `None` for everything.
    pub fn span(self) -> Option<Duration> {
        self.range.get().span()
    }
}

/// Runs `f` with the verdict's input borrowed from the state. Inside a reactive context every
/// signal read is tracked; in an event handler none is.
pub(crate) fn with_health_input<R>(
    state: AppState,
    now: SystemTime,
    phase_since: SystemTime,
    f: impl FnOnce(&HealthInput<'_>) -> R,
) -> R {
    state.status.with(|status| {
        state.stats.with(|stats| {
            state.history.with(|history| {
                state.metrics.with(|metrics| {
                    state.metrics_history.with(|metrics_history| {
                        state.insights.with(|insights| {
                            state.update.with(|update| {
                                f(&HealthInput {
                                    status,
                                    stats: stats.as_ref(),
                                    history,
                                    metrics: metrics.as_ref(),
                                    metrics_history,
                                    insights,
                                    update: update.as_ref(),
                                    now,
                                    phase_since,
                                })
                            })
                        })
                    })
                })
            })
        })
    })
}

/// A `Date` for `time`.
fn js_date(time: SystemTime) -> js_sys::Date {
    let millis = time
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |since| since.as_millis() as f64);
    js_sys::Date::new(&JsValue::from_f64(millis))
}

/// `Intl` options from `(key, value)` pairs.
fn intl_options(pairs: &[(&str, &str)]) -> JsValue {
    let options = js_sys::Object::new();
    for (key, value) in pairs {
        let _ = js_sys::Reflect::set(&options, &JsValue::from_str(key), &JsValue::from_str(value));
    }
    options.into()
}

/// `time` as a short local date and time, such as `23 Oct, 21:13` (the locale orders the
/// day and month), beside [`clock::local_string`]'s full form.
pub(crate) fn local_short(time: SystemTime) -> String {
    js_date(time)
        .to_locale_string(
            "default",
            &intl_options(&[
                ("day", "numeric"),
                ("month", "short"),
                ("hour", "2-digit"),
                ("minute", "2-digit"),
                ("hourCycle", "h23"),
            ]),
        )
        .into()
}

/// `time` as a short local date, such as `3 Dec 2026`.
pub(crate) fn local_date(time: SystemTime) -> String {
    js_date(time)
        .to_locale_date_string(
            "default",
            &intl_options(&[("day", "numeric"), ("month", "short"), ("year", "numeric")]),
        )
        .into()
}

/// The end-of-support warning: `End of support in 12 days. zebrad 7.0.0 stops working at block
/// 3,160,000 (about 3 Dec 2026). Update the launcher before then.`
pub(crate) fn eos_banner_text(version: &str, eos: &EosInfo, date: &str, now: SystemTime) -> String {
    format!(
        "End of support {}. zebrad {version} stops working at block {} (about {date}). Update the launcher before then.",
        relative(eos.estimated_time, now),
        thousands(eos.block_height)
    )
}

/// The Blocks behind the tip chart: zebrad's estimate of the tip minus the verified height.
pub(crate) fn behind_chart(window: &[Sample]) -> ChartData {
    ChartData {
        times: window.iter().map(|s| s.at).collect(),
        series: vec![Series {
            name: "Behind",
            class: "chart-series-1",
            values: window.iter().map(|s| s.remaining() as f64).collect(),
        }],
    }
}

/// The same chart's table: Behind, then the height and the estimated tip it comes from.
pub(crate) fn behind_table(window: &[Sample]) -> ChartData {
    let mut data = behind_chart(window);
    for (name, field) in [
        ("Height", Field::Height),
        ("Estimated tip", Field::EstimatedHeight),
    ] {
        data.series.push(Series {
            name,
            class: "chart-series-muted",
            values: window.iter().map(|s| s.get(field) as f64).collect(),
        });
    }
    data
}

/// The Peers chart: connected peers, one series until the peer list lands.
pub(crate) fn peers_chart(window: &[Sample]) -> ChartData {
    ChartData::from_samples(window, &[("Peers", "chart-series-1", Field::Peers)])
}

/// Every sample of a drawable window is at the tip: the Behind chart says so in words instead
/// of drawing a flat zero.
pub(crate) fn all_at_tip(window: &[Sample]) -> bool {
    window.len() >= 2 && window.iter().all(Sample::at_tip)
}

/// What a chart's frame says with fewer than two samples: the history has slid past the
/// range, or samples are still being collected.
pub(crate) fn chart_empty_text(has_history: bool, window_empty: bool, range: Range) -> String {
    if has_history && window_empty {
        format!("No samples {}", range.phrase())
    } else {
        "Collecting samples… updates every 5 s".to_owned()
    }
}

/// The Grafana line's sentence.
pub(crate) fn grafana_line_text(has_url: bool) -> &'static str {
    if has_url {
        "Detailed charts of zebrad's metrics live in the Grafana tab."
    } else {
        "Grafana is optional: the Grafana tab explains how to run it."
    }
}

#[component]
pub fn Dashboard() -> impl IntoView {
    let state = AppState::get();

    // The running version and its end of support. The sentence is rebuilt only when they
    // change; the relative time in it follows the clock on its own.
    let eos = Memo::new(move |_| {
        state.stats.with(|stats| {
            let stats = stats.as_ref()?;
            Some((stats.zebrad_version.clone(), stats.eos.clone()?))
        })
    });
    // Only inside the backend's warning window; outside it the date is a Node details row.
    let eos_soon = Memo::new(move |_| {
        let now = state.now.get();
        eos.with(|eos| {
            eos.as_ref()
                .is_some_and(|(_, eos)| eos_within(eos.estimated_time, now, EOS_WARNING_WINDOW))
        })
    });
    let eos_text = move || {
        eos.get().map(|(version, eos)| {
            let date = local_date(eos.estimated_time);
            move || eos_banner_text(&version, &eos, &date, state.now.get())
        })
    };
    let halted = Memo::new(move |_| {
        state.status.with(|status| {
            status.phase
                == Phase::Failed {
                    class: ExitClass::EosHalt,
                }
        })
    });
    let banners = view! {
        <Show when=move || eos_soon.get()>
            <div class="banner banner-warn eos-banner" role="status">
                <span>{eos_text}</span>
                <div class="buttons">
                    <CheckForUpdate />
                </div>
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

    // The window slides with the clock, so it is recomputed every second.
    let range = RwSignal::new(Range::default());
    let window = Memo::new(move |_| {
        let now = state.now.get();
        let span = range.get().span();
        state
            .history
            .with(|history| history.window(span, now).to_vec())
    });
    let latest = Memo::new(move |_| {
        state
            .stats
            .with(|stats| stats.as_ref().map(Sample::from_stats))
            .or_else(|| state.history.with(|history| history.latest().copied()))
    });
    let metrics = Memo::new(move |_| {
        state
            .metrics
            .with(|snapshot| snapshot.as_ref().map(MetricsFacts::from_snapshot))
            .or_else(|| {
                state
                    .metrics_history
                    .with(|history| history.latest().map(MetricsFacts::from_snapshot))
            })
    });
    let verdict = Memo::new(move |_| {
        let now = state.now.get();
        let phase_since = state.phase_since.get();
        with_health_input(state, now, phase_since, health_verdict)
    });
    // "for N min" counts from the moment the verdict's rule or level last changed.
    Effect::new(move |previous: Option<(u8, crate::health::Level)>| {
        let key = verdict.with(|verdict| (verdict.rule, verdict.level));
        if previous.is_some_and(|previous| previous != key) {
            state.phase_since.set(clock::now());
        }
        key
    });
    let dash = Dash {
        state,
        range,
        window,
        latest,
        verdict,
        metrics,
    };

    let behind = Memo::new(move |_| window.with(|window| behind_chart(window)));
    let behind_rows = Memo::new(move |_| window.with(|window| behind_table(window)));
    let peers = Memo::new(move |_| window.with(|window| peers_chart(window)));
    let empty = Signal::derive(move || {
        let has_history = state.history.with(|history| !history.is_empty());
        chart_empty_text(has_history, window.with(Vec::is_empty), range.get())
    });
    let at_tip_note = Signal::derive(move || {
        window
            .with(|window| all_at_tip(window))
            .then(|| "At the tip for the whole range".to_owned())
    });

    let has_grafana_url = move || {
        state.settings.with(|settings| {
            settings
                .as_ref()
                .is_some_and(|settings| settings.grafana_url.is_some())
        })
    };

    view! {
        <div class="dashboard">
            {banners}
            <StatusCard dash />
            <RangeRow dash />
            <Tiles dash />
            <div class="chart-grid">
                <LineChart
                    title="Blocks behind the tip"
                    subtitle="zebrad's estimate of the tip minus the verified height, from the RPC every 5 s"
                    data=behind
                    table_data=behind_rows
                    empty=empty
                    note=at_tip_note
                    from_zero=true
                    area=true
                />
                <LineChart
                    title="Peers"
                    subtitle="Connected peers"
                    data=peers
                    empty=empty
                    from_zero=true
                    area=true
                />
            </div>
            <ActivityRow dash />
            <Details dash />
            <p class="dash-grafana muted">
                <span>{move || grafana_line_text(has_grafana_url())}</span>
                <button class="link" on:click=move |_| state.tab.set(Tab::Grafana)>
                    "Open the Grafana tab"
                </button>
            </p>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use launcher_types::{MetricFamily, MetricKind, MetricSample};

    use super::*;

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn sample(secs: u64, height: u64, estimated: u64, peers: u32) -> Sample {
        Sample {
            at: at(secs),
            height,
            estimated_height: estimated,
            peers,
            mempool_transactions: 3,
            verification_progress: height as f64 / estimated as f64,
            peers_inbound: None,
            peers_outbound: None,
        }
    }

    fn family(name: &str, kind: MetricKind, value: f64) -> MetricFamily {
        MetricFamily {
            name: name.to_owned(),
            kind,
            help: None,
            samples: vec![MetricSample {
                name: name.to_owned(),
                labels: Vec::new(),
                value,
            }],
        }
    }

    #[test]
    fn metrics_facts_read_the_named_series() {
        let snapshot = MetricsSnapshot {
            at: at(100),
            families: vec![
                family(metric::DISK_BYTES, MetricKind::Gauge, 248_300_000_000.0),
                family(metric::MEMPOOL_BYTES, MetricKind::Gauge, 44_400.4),
                family(metric::READY_PEERS, MetricKind::Gauge, 11.0),
                family(metric::UNREADY_PEERS, MetricKind::Gauge, 1.0),
                family(metric::OBSOLETE_PEERS, MetricKind::Gauge, 1.0),
                family(metric::HANDSHAKE_FAILURES, MetricKind::Counter, 3.0),
                family(metric::CHECKPOINT_HEIGHT, MetricKind::Gauge, 2_500_000.0),
                family(metric::CHAIN_COUNT, MetricKind::Gauge, f64::NAN),
            ],
        };
        let facts = MetricsFacts::from_snapshot(&snapshot);
        assert_eq!(facts.at, at(100));
        assert_eq!(facts.disk_bytes, Some(248_300_000_000));
        assert_eq!(facts.memory_bytes, None, "not on this page");
        assert_eq!(facts.mempool_bytes, Some(44_400));
        assert_eq!(facts.ready_peers, Some(11));
        assert_eq!(facts.unready_peers, Some(1));
        assert_eq!(facts.obsolete_peers, Some(1));
        assert_eq!(facts.handshake_failures, Some(3));
        assert_eq!(facts.checkpoint_height, Some(2_500_000));
        assert_eq!(facts.chain_count, None, "NaN reads as absent");
        // A counter is not read as a gauge and the other way round.
        let wrong_kinds = MetricsSnapshot {
            at: at(0),
            families: vec![
                family(metric::DISK_BYTES, MetricKind::Counter, 1.0),
                family(metric::HANDSHAKE_FAILURES, MetricKind::Gauge, 1.0),
            ],
        };
        let facts = MetricsFacts::from_snapshot(&wrong_kinds);
        assert_eq!(facts.disk_bytes, None);
        assert_eq!(facts.handshake_failures, None);
    }

    #[test]
    fn eos_banner_sentence() {
        let now = at(1_000_000);
        let eos = EosInfo {
            block_height: 3_160_000,
            estimated_time: now + Duration::from_secs(12 * 86_400 + 3600),
        };
        assert_eq!(
            eos_banner_text("7.0.0", &eos, "3 Dec 2026", now),
            "End of support in 12 days. zebrad 7.0.0 stops working at block 3,160,000 (about 3 Dec 2026). Update the launcher before then."
        );
    }

    #[test]
    fn behind_chart_is_the_remaining_blocks() {
        let window = [
            sample(0, 2_950_000, 3_120_000, 8),
            sample(5, 2_950_100, 3_120_000, 9),
        ];
        let data = behind_chart(&window);
        assert_eq!(data.times, vec![at(0), at(5)]);
        assert_eq!(data.series.len(), 1);
        assert_eq!(data.series[0].name, "Behind");
        assert_eq!(data.series[0].values, vec![170_000.0, 169_900.0]);
        let table = behind_table(&window);
        let names: Vec<_> = table.series.iter().map(|s| s.name).collect();
        assert_eq!(names, vec!["Behind", "Height", "Estimated tip"]);
        assert_eq!(table.series[1].values, vec![2_950_000.0, 2_950_100.0]);
        assert_eq!(table.series[2].values, vec![3_120_000.0, 3_120_000.0]);
        let peers = peers_chart(&window);
        assert_eq!(peers.series[0].values, vec![8.0, 9.0]);
        assert_eq!(behind_chart(&[]).len(), 0);
    }

    #[test]
    fn at_tip_needs_a_whole_drawable_window() {
        let tip = sample(0, 3_120_000, 3_120_000, 8);
        assert!(!all_at_tip(&[tip]), "one sample draws nothing");
        assert!(all_at_tip(&[tip, sample(5, 3_120_001, 3_120_001, 8)]));
        assert!(!all_at_tip(&[tip, sample(5, 3_100_000, 3_120_000, 8)]));
        assert!(!all_at_tip(&[]));
    }

    #[test]
    fn frame_texts() {
        assert_eq!(
            chart_empty_text(false, true, Range::Hour1),
            "Collecting samples… updates every 5 s"
        );
        assert_eq!(
            chart_empty_text(true, true, Range::Min15),
            "No samples in the last 15 min"
        );
        assert_eq!(
            chart_empty_text(true, false, Range::Hour1),
            "Collecting samples… updates every 5 s",
            "one sample in the window is still collecting"
        );
        assert!(grafana_line_text(true).contains("Grafana tab"));
        assert!(grafana_line_text(false).starts_with("Grafana is optional"));
    }
}
