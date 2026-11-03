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

use std::time::{Duration, SystemTime};

use launcher_types::{EOS_WARNING_WINDOW, EosInfo, ExitClass, NodeDetails, Phase, eos_within};
use leptos::prelude::*;

use crate::{
    app::{AppState, Tab},
    charts::{ChartData, Range, Series, line_chart::LineChart},
    clock,
    components::{
        icons::{check_icon, clock_icon, cross_icon, info_icon, warning_icon},
        public_services::PublicCard,
        updates::CheckForUpdate,
    },
    format::{local_date_short, relative, thousands},
    health::{HealthInput, Level, Verdict, health_verdict},
    metrics_history::{MetricsHistory, names},
    node_state::NodeState,
    stats_history::{Field, Sample},
};

use self::{
    activity::ActivityRow, details::Details, status_card::StatusCard, tiles::RangeRow, tiles::Tiles,
};

/// The icons a state is carried by, beside its word; the status colour sits on the icon. The
/// info mark explains (nothing to fix) and wears no status colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Icon {
    Check,
    Clock,
    Warning,
    Cross,
    Info,
}

impl Icon {
    pub fn view(self) -> AnyView {
        match self {
            Icon::Check => check_icon().into_any(),
            Icon::Clock => clock_icon().into_any(),
            Icon::Warning => warning_icon().into_any(),
            Icon::Cross => cross_icon().into_any(),
            Icon::Info => info_icon().into_any(),
        }
    }

    /// The class that colours the icon by what it means, as the insight cards do.
    pub fn tone_class(self) -> &'static str {
        match self {
            Icon::Check => "insight-tone-good",
            Icon::Clock => "insight-tone-neutral",
            Icon::Warning => "insight-tone-warning",
            Icon::Cross => "insight-tone-critical",
            Icon::Info => "insight-tone-info",
        }
    }
}

/// The numbers the page reads from the newest snapshot of zebrad's metrics page, through
/// [`MetricsHistory`]'s accessors (a gauge the snapshot lacks, or a non-finite value, is
/// `None`), kept while zebrad is stopped.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MetricsFacts {
    pub at: SystemTime,
    pub disk_bytes: Option<u64>,
    pub memory_bytes: Option<u64>,
    pub mempool_bytes: Option<u64>,
    pub ready_peers: Option<u64>,
    pub unready_peers: Option<u64>,
    pub obsolete_peers: Option<u64>,
    pub obsolete_refused: Option<u64>,
    pub handshake_failures: Option<u64>,
    pub checkpoint_height: Option<u64>,
    pub chain_count: Option<u64>,
}

impl MetricsFacts {
    /// The facts of the history's newest snapshot; `None` before the first one.
    pub fn from_history(history: &MetricsHistory) -> Option<Self> {
        Some(MetricsFacts {
            at: history.latest()?.at,
            disk_bytes: history.disk_bytes(),
            memory_bytes: history.memory_bytes(),
            mempool_bytes: history.mempool_bytes(),
            ready_peers: history.ready_peers(),
            unready_peers: history.unready_peers(),
            obsolete_peers: history.obsolete_peers(),
            obsolete_refused: history.obsolete_refused(),
            handshake_failures: history.handshake_failures(),
            checkpoint_height: history.checkpoint_height(),
            chain_count: history.chain_count(),
        })
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
    /// The details behind the statistics (the peer list, the best block, the chain's supply
    /// and pools): the published ones, or the last ones once zebrad stopped publishing, so
    /// the Chain card keeps its last values. The peer table reads the live `state.node().details`.
    pub details: Memo<Option<NodeDetails>>,
}

impl Dash {
    /// zebrad is in [`Phase::Running`].
    pub fn running(self) -> bool {
        self.state
            .node()
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
    state.node().status.with(|status| {
        state.node().stats.with(|stats| {
            state.node().history.with(|history| {
                state.node().metrics.with(|metrics| {
                    state.node().metrics_history.with(|metrics_history| {
                        state.node().insights.with(|insights| {
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
    ChartData::from_samples(window, &[("Behind", "chart-series-1", Field::Behind)])
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

/// The Peers chart: connected peers by direction, Outbound and Inbound, once a peer list has
/// landed in the window (`split`); one series of the total until then.
pub(crate) fn peers_chart(window: &[Sample], split: bool) -> ChartData {
    if split {
        ChartData::from_samples(
            window,
            &[
                ("Outbound", "chart-series-1", Field::PeersOutbound),
                ("Inbound", "chart-series-2", Field::PeersInbound),
            ],
        )
    } else {
        ChartData::from_samples(window, &[("Peers", "chart-series-1", Field::Peers)])
    }
}

/// The Peers chart's table: the plotted series, their total when they are two, then the peer
/// set's ready and unready counts from the metrics page (the newest reading at or before
/// each sample, a dash before the first) while the history has any.
pub(crate) fn peers_table(window: &[Sample], split: bool, metrics: &MetricsHistory) -> ChartData {
    let mut data = peers_chart(window, split);
    if split {
        data.series.push(Series {
            name: "Total",
            class: "chart-series-muted",
            values: window.iter().map(|s| f64::from(s.peers)).collect(),
        });
    }
    for (name, metric) in [
        ("Ready", names::READY_PEERS),
        ("Unready", names::UNREADY_PEERS),
    ] {
        let values: Vec<f64> = window
            .iter()
            .map(|s| metrics.value_at(metric, s.at).unwrap_or(f64::NAN))
            .collect();
        if values.iter().any(|v| v.is_finite()) {
            data.series.push(Series {
                name,
                class: "chart-series-muted",
                values,
            });
        }
    }
    data
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
        state.node().stats.with(|stats| {
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
            let date = local_date_short(eos.estimated_time);
            move || eos_banner_text(&version, &eos, &date, state.now.get())
        })
    };
    let halted = Memo::new(move |_| {
        state.node().status.with(|status| {
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
            .node()
            .history
            .with(|history| history.window(span, now).to_vec())
    });
    let latest = Memo::new(move |_| {
        state.node().stats.with(|stats| {
            state
                .node()
                .history
                .with(|history| history.sample_for(stats.as_ref()))
        })
    });
    let metrics = Memo::new(move |_| {
        state
            .node()
            .metrics_history
            .with(MetricsFacts::from_history)
    });
    // A stopped node keeps showing its last details; another node's never stand in.
    let kept_details = Memo::new(move |previous: Option<&(NodeState, Option<NodeDetails>)>| {
        let node = state.node();
        let kept = previous
            .filter(|(was, _)| *was == node)
            .and_then(|(_, details)| details.clone());
        (node, node.details.get().or(kept))
    });
    let details = Memo::new(move |_| kept_details.with(|(_, details)| details.clone()));
    // Until the first status the launcher is still connecting; the rules need a status.
    let verdict = Memo::new(move |_| {
        if !state.node().status_loaded.get() {
            return Verdict::connecting();
        }
        let now = state.now.get();
        let phase_since = state.node().phase_since.get();
        with_health_input(state, now, phase_since, health_verdict)
    });
    // "for N min" counts from the moment the verdict's rule or level last changed: the one
    // writer of `phase_since` after the first status seeded it (see `NodeState::phase_since`).
    // The step from connecting to the first real verdict keeps the seed.
    Effect::new(move |previous: Option<(u8, Level)>| {
        let key = verdict.with(|verdict| (verdict.rule, verdict.level));
        if previous.is_some_and(|previous| previous != key && previous.0 != 0) {
            state.node_untracked().phase_since.set(clock::now());
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
        details,
    };

    let behind = Memo::new(move |_| window.with(|window| behind_chart(window)));
    let behind_rows = Memo::new(move |_| window.with(|window| behind_table(window)));
    let peer_split = Memo::new(move |_| {
        let now = state.now.get();
        state
            .node()
            .history
            .with(|history| history.has_peer_split(range.get().span(), now))
    });
    let peers = Memo::new(move |_| window.with(|window| peers_chart(window, peer_split.get())));
    let peers_rows = Memo::new(move |_| {
        state
            .node()
            .metrics_history
            .with(|metrics| window.with(|window| peers_table(window, peer_split.get(), metrics)))
    });
    let empty = Signal::derive(move || {
        let has_history = state.node().history.with(|history| !history.is_empty());
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
                    subtitle="Connected peers, by who opened the connection"
                    data=peers
                    table_data=peers_rows
                    empty=empty
                    from_zero=true
                    area=true
                />
            </div>
            <ActivityRow dash />
            <Details dash />
            <PublicCard />
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
    use std::time::UNIX_EPOCH;

    use launcher_types::{MetricFamily, MetricKind, MetricSample, MetricsSnapshot};

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
        assert_eq!(MetricsFacts::from_history(&MetricsHistory::default()), None);
        let snapshot = MetricsSnapshot {
            at: at(100),
            families: vec![
                family(names::DISK_BYTES, MetricKind::Gauge, 248_300_000_000.0),
                family(names::MEMPOOL_BYTES, MetricKind::Gauge, 44_400.4),
                family(names::READY_PEERS, MetricKind::Gauge, 11.0),
                family(names::UNREADY_PEERS, MetricKind::Gauge, 1.0),
                family(names::OBSOLETE_PEERS, MetricKind::Gauge, 1.0),
                family(names::HANDSHAKE_FAILURES, MetricKind::Counter, 3.0),
                family(names::CHECKPOINT_HEIGHT, MetricKind::Gauge, 2_500_000.0),
                family(names::CHAIN_COUNT, MetricKind::Gauge, f64::NAN),
            ],
        };
        let mut history = MetricsHistory::default();
        history.push(&snapshot);
        let facts = MetricsFacts::from_history(&history).unwrap();
        assert_eq!(facts.at, at(100));
        assert_eq!(facts.disk_bytes, Some(248_300_000_000));
        assert_eq!(facts.memory_bytes, None, "not on this page");
        assert_eq!(facts.mempool_bytes, Some(44_400));
        assert_eq!(facts.ready_peers, Some(11));
        assert_eq!(facts.unready_peers, Some(1));
        assert_eq!(facts.obsolete_peers, Some(1));
        assert_eq!(facts.obsolete_refused, None, "no counter in the page");
        assert_eq!(facts.handshake_failures, Some(3));
        assert_eq!(facts.checkpoint_height, Some(2_500_000));
        assert_eq!(facts.chain_count, None, "NaN reads as absent");
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
        assert_eq!(behind_chart(&[]).len(), 0);
    }

    #[test]
    fn peers_chart_splits_by_direction_once_a_list_landed() {
        let mut window = [
            sample(0, 2_950_000, 3_120_000, 8),
            sample(5, 2_950_100, 3_120_000, 9),
        ];
        let total = peers_chart(&window, false);
        assert_eq!(total.series.len(), 1);
        assert_eq!(total.series[0].name, "Peers");
        assert_eq!(total.series[0].values, vec![8.0, 9.0]);
        window[1].peers_inbound = Some(3);
        window[1].peers_outbound = Some(6);
        let split = peers_chart(&window, true);
        let names: Vec<_> = split.series.iter().map(|s| (s.name, s.class)).collect();
        assert_eq!(
            names,
            vec![
                ("Outbound", "chart-series-1"),
                ("Inbound", "chart-series-2")
            ]
        );
        // A sample without a list reads 0 for both, as `Sample::get` does.
        assert_eq!(split.series[0].values, vec![0.0, 6.0]);
        assert_eq!(split.series[1].values, vec![0.0, 3.0]);
        // The table adds the total and, with metrics, the peer set's readiness per sample:
        // the newest reading at or before the sample, none before the first.
        let plain = peers_table(&window, false, &MetricsHistory::default());
        let names: Vec<_> = plain.series.iter().map(|s| s.name).collect();
        assert_eq!(names, vec!["Peers"]);
        let mut metrics = MetricsHistory::default();
        metrics.push(&MetricsSnapshot {
            at: at(3),
            families: vec![
                family(names::READY_PEERS, MetricKind::Gauge, 7.0),
                family(names::UNREADY_PEERS, MetricKind::Gauge, 1.0),
            ],
        });
        let table = peers_table(&window, true, &metrics);
        let names: Vec<_> = table.series.iter().map(|s| s.name).collect();
        assert_eq!(
            names,
            vec!["Outbound", "Inbound", "Total", "Ready", "Unready"]
        );
        assert_eq!(table.series[2].values, vec![8.0, 9.0]);
        assert!(table.series[3].values[0].is_nan());
        assert_eq!(table.series[3].values[1], 7.0);
        assert_eq!(table.series[4].values[1], 1.0);
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
