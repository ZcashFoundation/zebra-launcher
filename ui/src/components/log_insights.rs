//! Cards that show what the log stream says: sync stage, recent problems, activity and the
//! startup timeline. Backed by [`crate::insights::Insights`].
//!
//! The chart geometry and the sentences are pure functions, tested natively; the components
//! only wire them to signals.

use std::time::{SystemTime, UNIX_EPOCH};

use leptos::prelude::*;
use wasm_bindgen::JsValue;

use crate::{
    app::AppState,
    charts::activity::{
        Kind, LABELLED_COLUMNS, PLOT_BOTTOM, PLOT_LEFT, PLOT_RIGHT, PLOT_TOP, VIEW_HEIGHT,
        VIEW_WIDTH, geometry, segment_path, tooltip_class,
    },
    components::{
        icons::{check_icon, clock_icon, cross_icon, warning_icon},
        rows::{Rows, text_rows},
    },
    format::{relative, thousands},
    insights::{
        ACTIVITY_MINUTES, Checkpoints, Mempool, Peers, Problem, ProblemLevel, Run, SyncSnapshot,
        Tone, compact, elapsed_text, pick_filter,
    },
};

/// `time` as `HH:MM` in the user's time zone.
fn local_hhmm(time: SystemTime) -> String {
    let millis = time
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |since| since.as_millis() as f64);
    let date = js_sys::Date::new(&JsValue::from_f64(millis));
    format!("{:02}:{:02}", date.get_hours(), date.get_minutes())
}

fn tone_class(tone: Tone) -> &'static str {
    match tone {
        Tone::Good => "insight-stage insight-tone-good",
        Tone::Neutral => "insight-stage insight-tone-neutral",
        Tone::Warning => "insight-stage insight-tone-warning",
        Tone::Critical => "insight-stage insight-tone-critical",
    }
}

/// The icon that goes with a tone, so the state never rests on colour alone.
fn tone_icon(tone: Tone) -> AnyView {
    match tone {
        Tone::Good => check_icon().into_any(),
        Tone::Neutral => clock_icon().into_any(),
        Tone::Warning => warning_icon().into_any(),
        Tone::Critical => cross_icon().into_any(),
    }
}

/// `12.345` as `12.35 %`.
fn percent_text(percent: f64) -> String {
    format!("{percent:.2} %")
}

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

/// A column chart of the lines per minute over the last hour: INFO, WARN, ERROR (with
/// stderr) and other lines stacked, with a tooltip per column, keyboard Left and Right, and
/// a table view.
#[component]
pub fn LogActivityChart() -> impl IntoView {
    let state = AppState::get();
    let buckets = Memo::new(move |_| {
        let now = state.now.get();
        state.insights.with(|insights| insights.activity(now))
    });
    let empty = Memo::new(move |_| buckets.with(|b| b.iter().all(|bucket| bucket.total() == 0)));
    let show_table = RwSignal::new(false);
    let picked = RwSignal::new(None::<usize>);

    let move_pick = move |delta: isize| {
        let last = ACTIVITY_MINUTES - 1;
        let next = match picked.get_untracked() {
            None => last,
            Some(index) => (index as isize + delta).clamp(0, last as isize) as usize,
        };
        picked.set(Some(next));
    };
    let on_key = move |event: web_sys::KeyboardEvent| {
        match event.key().as_str() {
            "ArrowLeft" => move_pick(-1),
            "ArrowRight" => move_pick(1),
            "Home" => picked.set(Some(0)),
            "End" => picked.set(Some(ACTIVITY_MINUTES - 1)),
            "Escape" => picked.set(None),
            _ => return,
        }
        event.prevent_default();
    };

    let chart = move || {
        let buckets = buckets.get();
        let layout = geometry(&buckets);
        let grid = layout
            .ticks
            .iter()
            .map(|tick| {
                let y = layout.y_of(*tick);
                view! {
                    <line class="insight-grid-line" x1=PLOT_LEFT x2=PLOT_RIGHT y1=y y2=y />
                    <text class="insight-tick" x=PLOT_LEFT - 6.0 y=y + 3.5 text-anchor="end">
                        {compact(u64::from(*tick))}
                    </text>
                }
            })
            .collect_view();
        let columns = layout
            .columns
            .iter()
            .enumerate()
            .map(|(index, column)| {
                let segments = column
                    .segments
                    .iter()
                    .map(|segment| {
                        view! {
                            <path
                                class=segment.kind.class()
                                d=segment_path(column.x, segment.y, column.width, segment.height, segment.top)
                            />
                        }
                    })
                    .collect_view();
                view! {
                    <g class="insight-column" class:insight-picked=move || picked.get() == Some(index)>
                        {segments}
                        <rect
                            class="insight-hit"
                            x=column.slot_x
                            y=PLOT_TOP
                            width=column.slot_width
                            height=PLOT_BOTTOM - PLOT_TOP
                            on:pointerenter=move |_| picked.set(Some(index))
                        />
                    </g>
                }
            })
            .collect_view();
        let labels = LABELLED_COLUMNS
            .into_iter()
            .filter_map(|index| {
                let column = layout.columns.get(index)?;
                let bucket = buckets.get(index)?;
                let anchor = match index {
                    0 => "start",
                    i if i + 1 == layout.columns.len() => "end",
                    _ => "middle",
                };
                let x = match anchor {
                    "start" => column.slot_x,
                    "end" => column.slot_x + column.slot_width,
                    _ => column.center(),
                };
                Some(view! {
                    <text class="insight-tick" x=x y=VIEW_HEIGHT - 4.0 text-anchor=anchor>
                        {local_hhmm(bucket.minute_start)}
                    </text>
                })
            })
            .collect_view();
        view! {
            {grid}
            <line class="insight-axis" x1=PLOT_LEFT x2=PLOT_RIGHT y1=PLOT_BOTTOM y2=PLOT_BOTTOM />
            {columns}
            {labels}
        }
    };

    let tooltip = move || {
        let index = picked.get()?;
        let buckets = buckets.get();
        let bucket = *buckets.get(index)?;
        let layout = geometry(&buckets);
        let center = layout.columns.get(index)?.center();
        // A fraction of the tooltip layer, which is exactly as wide as the svg.
        let left = format!("{:.1}%", center / VIEW_WIDTH * 100.0);
        let class = tooltip_class(center);
        let rows = Kind::LEGEND
            .into_iter()
            .map(|kind| {
                view! {
                    <div class="insight-tooltip-row">
                        <span class=kind.key_class() aria-hidden="true"></span>
                        <strong>{kind.count(&bucket).to_string()}</strong>
                        <span>{kind.label()}</span>
                    </div>
                }
            })
            .collect_view();
        Some(view! {
            <div class=class style:left=left>
                <div class="insight-tooltip-head">
                    {local_hhmm(bucket.minute_start)}
                    " · "
                    {bucket.total().to_string()}
                    " lines"
                </div>
                {rows}
            </div>
        })
    };

    let table = move || {
        let buckets = buckets.get();
        let rows = buckets
            .iter()
            .filter(|bucket| bucket.total() > 0)
            .map(|bucket| {
                view! {
                    <tr>
                        <th scope="row">{local_hhmm(bucket.minute_start)}</th>
                        <td>{bucket.info.to_string()}</td>
                        <td>{bucket.warn.to_string()}</td>
                        <td>{bucket.error.to_string()}</td>
                        <td>{bucket.other.to_string()}</td>
                        <td>{bucket.total().to_string()}</td>
                    </tr>
                }
            })
            .collect_view();
        view! {
            <table class="insight-table">
                <caption class="visually-hidden">"Log lines per minute, last 60 minutes"</caption>
                <thead>
                    <tr>
                        <th scope="col">"Minute"</th>
                        <th scope="col">"INFO"</th>
                        <th scope="col">"WARN"</th>
                        <th scope="col">"ERROR and stderr"</th>
                        <th scope="col">"Other"</th>
                        <th scope="col">"Total"</th>
                    </tr>
                </thead>
                <tbody>{rows}</tbody>
            </table>
        }
    };

    let legend = Kind::LEGEND
        .into_iter()
        .map(|kind| {
            view! {
                <span class="insight-legend-item">
                    <span class=kind.key_class() aria-hidden="true"></span>
                    {kind.label()}
                </span>
            }
        })
        .collect_view();

    view! {
        <div class="insight-chart">
            <div class="insight-chart-bar">
                <div class="insight-legend" aria-hidden="true">{legend}</div>
                <button
                    class="insight-toggle"
                    aria-pressed=move || show_table.get().to_string()
                    on:click=move |_| show_table.update(|table| *table = !*table)
                >
                    "Table"
                </button>
            </div>
            <Show
                when=move || !empty.get()
                fallback=|| view! { <p class="muted insight-empty">"No lines yet"</p> }
            >
                <Show when=move || !show_table.get() fallback=table>
                    <div class="insight-plot" on:pointerleave=move |_| picked.set(None)>
                        <svg
                            viewBox=format!("0 0 {VIEW_WIDTH} {VIEW_HEIGHT}")
                            role="img"
                            aria-label="Log lines per minute over the last 60 minutes. Use the left and right arrow keys to read one minute, or switch to the table."
                            tabindex="0"
                            on:keydown=on_key
                            on:blur=move |_| picked.set(None)
                        >
                            {chart}
                        </svg>
                        <div class="insight-tooltip-layer" role="status">{tooltip}</div>
                    </div>
                </Show>
            </Show>
        </div>
    }
}

/// Where the initial sync is, from zebrad's progress log.
#[component]
pub fn SyncStageCard() -> impl IntoView {
    let state = AppState::get();
    let sync = Memo::new(move |_| state.insights.with(|insights| insights.sync().cloned()));
    let rate = Memo::new(move |_| {
        state
            .insights
            .with(|insights| insights.sync_rate_from_logs())
    });

    let body = move |snapshot: SyncSnapshot| {
        let stage = snapshot.stage;
        let tone = stage.stage_tone();
        let mut rows: Vec<(&'static str, String)> = Vec::new();
        if let Some(upgrade) = &snapshot.network_upgrade {
            rows.push(("Network upgrade", upgrade.clone()));
        }
        if let Some(height) = snapshot.height {
            rows.push(("Height", thousands(height)));
        }
        if let Some(remaining) = snapshot.remaining {
            rows.push(("Remaining blocks", thousands(remaining)));
        }
        if let Some(percent) = snapshot.percent {
            rows.push(("Progress", percent_text(percent)));
        }
        if let Some(since) = &snapshot.since_last_block {
            rows.push(("Last block", format!("{since} ago")));
        }
        if let Some(rate) = rate.get() {
            rows.push(("Blocks per minute", format!("{rate:.0} (from logs)")));
        }
        let at = snapshot.at;
        view! {
            <div class=tone_class(tone)>
                {tone_icon(tone)}
                <strong>{stage.stage_text()}</strong>
            </div>
            <Rows rows=text_rows(rows) class="insight-rows" />
            <p class="muted insight-foot">
                "From zebrad's progress log, updated every minute · last "
                {move || relative(at, state.now.get())}
            </p>
        }
    };

    view! {
        <div class="card insight-card insight-sync">
            <h3>"Sync stage"</h3>
            <Show
                when=move || sync.with(Option::is_some)
                fallback=|| view! {
                    <p class="muted insight-empty">
                        "No progress report yet. zebrad logs its first one within a minute of starting."
                    </p>
                }
            >
                {move || sync.get().map(body)}
            </Show>
        </div>
    }
}

/// The distinct warnings, errors and stderr lines, newest first. Clicking one calls `on_pick`
/// with a text filter for the Logs page.
#[component]
pub fn RecentProblems(
    #[prop(default = 8)] limit: usize,
    on_pick: Callback<String>,
) -> impl IntoView {
    let state = AppState::get();
    let problems = Memo::new(move |_| state.insights.with(|insights| insights.problems(limit)));

    let row = move |problem: Problem| {
        let pattern = problem.pattern.clone();
        // The row's own copy follows later lines with the same pattern (count, last seen).
        let current = Memo::new(move |_| {
            problems.with(|problems| problems.iter().find(|p| p.pattern == pattern).cloned())
        });
        let level_class = match problem.level {
            ProblemLevel::Warn => "lvl lvl-warn",
            ProblemLevel::Error => "lvl lvl-error",
            ProblemLevel::Stderr => "lvl lvl-stderr",
        };
        let target = problem.target.clone();
        let pick = move |_| {
            if let Some(problem) = current.get_untracked() {
                on_pick.run(pick_filter(&problem));
            }
        };
        view! {
            <li>
                <button class="insight-problem" title="Show lines like this one" on:click=pick>
                    <span class=level_class>{problem.level.label()}</span>
                    <span class="insight-problem-target">{target}</span>
                    <span class="insight-problem-msg">
                        {move || current.get().map(|p| p.sample).unwrap_or_default()}
                    </span>
                    <span class="insight-problem-count">
                        {move || current.get().map_or(String::new(), |p| format!("×{}", p.count))}
                    </span>
                    <span class="insight-problem-when">
                        {move || {
                            let now = state.now.get();
                            current.get().map_or(String::new(), |p| relative(p.last, now))
                        }}
                    </span>
                </button>
            </li>
        }
    };

    view! {
        <div class="card insight-card insight-problems">
            <h3>"Recent problems"</h3>
            <Show
                when=move || problems.with(|problems| !problems.is_empty())
                fallback=|| view! {
                    <p class="muted insight-empty">"No warnings or errors in this session"</p>
                }
            >
                <ul class="insight-problem-list">
                    <For each=move || problems.get() key=|problem| problem.pattern.clone() children=row />
                </ul>
            </Show>
        </div>
    }
}

/// The current run: version and network, the startup milestones with their times since the
/// run began, and the peer, checkpoint, mempool and shutdown facts.
#[component]
pub fn StartupTimeline() -> impl IntoView {
    let state = AppState::get();
    let run = Memo::new(move |_| {
        state
            .insights
            .with(|insights| insights.current_run().cloned())
    });
    let facts = Memo::new(move |_| {
        state
            .insights
            .with(|insights| (insights.peers(), insights.checkpoints(), insights.mempool()))
    });
    let earlier_runs = Memo::new(move |_| {
        state
            .insights
            .with(|insights| insights.runs().len().saturating_sub(1))
    });

    let body = move |run: Run| {
        let started = run.started_at;
        let milestones = run
            .milestones
            .iter()
            .map(|(milestone, at)| {
                let elapsed = at.duration_since(started).unwrap_or_default();
                view! {
                    <li>
                        <span class="insight-ms-label">{milestone.label()}</span>
                        <span class="insight-ms-at">{elapsed_text(elapsed)}</span>
                    </li>
                }
            })
            .collect_view();
        let connected = run
            .milestones
            .iter()
            .any(|(m, _)| *m == crate::insights::Milestone::PeersConnected);
        let (peers, checkpoints, mempool) = facts.get();
        let mut lines = vec![peers_line(&peers, connected)];
        lines.extend(checkpoints_line(
            &checkpoints,
            checkpoints.final_reached.map(local_hhmm),
        ));
        lines.extend(mempool_line(&mempool, mempool.active_since.map(local_hhmm)));
        lines.extend(earlier_runs_line(earlier_runs.get()));
        let shutdown = run.clone();
        view! {
            <p class="insight-run-head">
                <strong>{run_head(&run)}</strong>
                ", started "
                {move || relative(started, state.now.get())}
            </p>
            <ol class="insight-milestones">{milestones}</ol>
            <ul class="insight-facts">
                {lines.into_iter().map(|line| view! { <li>{line}</li> }).collect_view()}
                {move || {
                    shutdown_line(&shutdown, state.now.get())
                        .map(|line| view! { <li class="insight-shutdown">{line}</li> })
                }}
            </ul>
        }
    };

    view! {
        <div class="card insight-card insight-timeline">
            <h3>"This run"</h3>
            <Show
                when=move || run.with(Option::is_some)
                fallback=|| view! {
                    <p class="muted insight-empty">
                        "No start of a run in the log yet. The timeline fills from zebrad's first line."
                    </p>
                }
            >
                {move || run.get().map(body)}
            </Show>
        </div>
    }
}

/// The four cards in a responsive grid: the Logs page's analysis panel.
#[component]
pub fn LogInsightsPanel(on_pick: Callback<String>) -> impl IntoView {
    view! {
        <div class="insight-grid">
            <SyncStageCard />
            <RecentProblems limit=8 on_pick=on_pick />
            <StartupTimeline />
            <div class="card insight-card insight-activity">
                <h3>"Lines per minute"</h3>
                <LogActivityChart />
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

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
        assert_eq!(percent_text(12.345), "12.35 %");
    }
}
