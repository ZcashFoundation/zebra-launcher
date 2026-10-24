//! Cards that show what the log stream says: sync stage, recent problems, activity and the
//! startup timeline. Backed by [`crate::insights::Insights`].
//!
//! The chart geometry ([`crate::charts::activity`]) and the sentences
//! ([`crate::insight_text`]) are pure functions, tested natively; the components only wire
//! them to signals.

use std::time::SystemTime;

use leptos::prelude::*;

use crate::{
    app::AppState,
    charts::{
        activity::{
            Kind, LABELLED_COLUMNS, PLOT_BOTTOM, PLOT_LEFT, PLOT_RIGHT, PLOT_TOP, VIEW_HEIGHT,
            VIEW_WIDTH, geometry, segment_path, tooltip_class,
        },
        hhmm, tz_offset_min_at,
    },
    components::{
        icons::{check_icon, clock_icon, cross_icon, warning_icon},
        rows::{Rows, text_rows},
    },
    format::{percent, relative, thousands},
    insight_text::{
        checkpoints_line, earlier_runs_line, mempool_line, peers_line, run_head, shutdown_line,
    },
    insights::{
        ACTIVITY_MINUTES, Problem, ProblemLevel, Run, SyncSnapshot, Tone, elapsed_text, pick_filter,
    },
};

/// `time` as `HH:MM` in the browser's zone at that instant, as the line charts label theirs.
fn local_hhmm(time: SystemTime) -> String {
    hhmm(time, tz_offset_min_at(time))
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
            .map(|(tick, label)| {
                let y = layout.y_of(*tick);
                view! {
                    <line class="insight-grid-line" x1=PLOT_LEFT x2=PLOT_RIGHT y1=y y2=y />
                    <text class="insight-tick" x=PLOT_LEFT - 6.0 y=y + 3.5 text-anchor="end">
                        {label.clone()}
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
        if let Some(progress) = snapshot.percent {
            rows.push(("Progress", percent(progress / 100.0)));
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
