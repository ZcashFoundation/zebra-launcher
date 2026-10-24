//! The log viewer: zebrad's output with level colours, filters, follow-at-bottom and copy,
//! and the analysis panel built from [`crate::insights::Insights`].

use std::time::Duration;

use launcher_types::{LogLevel, LogLine, LogStream};
use leptos::{html, prelude::*, task::spawn_local};

use crate::{
    app::{AppState, Tab},
    clipboard::copy_to_clipboard,
    components::log_insights::LogInsightsPanel,
    log_view::{LogEntry, LogFilter, entries_text, split_matches, time_of_day},
};

/// How close to the bottom, in pixels, still counts as "at the bottom".
const BOTTOM_SLACK: i32 = 8;

/// How many targets the target filter offers.
const TARGET_CHOICES: usize = 25;

const LEVEL_CHOICES: [(&str, &str, Option<LogLevel>); 6] = [
    ("all", "All levels", None),
    ("trace", "TRACE and above", Some(LogLevel::Trace)),
    ("debug", "DEBUG and above", Some(LogLevel::Debug)),
    ("info", "INFO and above", Some(LogLevel::Info)),
    ("warn", "WARN and above", Some(LogLevel::Warn)),
    ("error", "ERROR only", Some(LogLevel::Error)),
];

fn level_key(level: Option<LogLevel>) -> &'static str {
    LEVEL_CHOICES
        .iter()
        .find(|(_, _, choice)| *choice == level)
        .map_or("all", |(key, _, _)| key)
}

fn level_class(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Trace => "lvl lvl-trace",
        LogLevel::Debug => "lvl lvl-debug",
        LogLevel::Info => "lvl lvl-info",
        LogLevel::Warn => "lvl lvl-warn",
        LogLevel::Error => "lvl lvl-error",
    }
}

fn row_class(line: &LogLine) -> &'static str {
    match line.stream {
        LogStream::Stdout => "log-row",
        LogStream::Stderr => "log-row log-stderr",
        LogStream::Launcher => "log-row log-launcher",
    }
}

/// The message with the current text filter's matches wrapped in `<mark>`.
fn message_view(message: String, text: RwSignal<String>) -> impl IntoView {
    move || {
        let needle = text.get();
        split_matches(&message, &needle)
            .into_iter()
            .map(|(piece, hit)| {
                if hit {
                    view! { <mark>{piece}</mark> }.into_any()
                } else {
                    piece.into_any()
                }
            })
            .collect_view()
    }
}

fn entry_view(entry: LogEntry, text: RwSignal<String>) -> AnyView {
    match entry {
        LogEntry::Gap { dropped, .. } => view! {
            <div class="log-row log-gap">{format!("… {dropped} lines dropped")}</div>
        }
        .into_any(),
        LogEntry::Line(line) => {
            let class = row_class(&line);
            let time = line
                .timestamp
                .as_deref()
                .map(|ts| time_of_day(ts).to_owned());
            let level = line.level.map(|level| {
                view! { <span class=level_class(level)>{level.as_str()}</span> }
            });
            let origin = match (&line.spans, &line.target) {
                (Some(spans), Some(target)) => Some(format!("{spans}: {target}:")),
                (None, Some(target)) => Some(format!("{target}:")),
                (Some(spans), None) => Some(format!("{spans}:")),
                (None, None) => None,
            };
            view! {
                <div class=class>
                    <span class="ts" title=line.timestamp>{time}</span>
                    {level}
                    {origin.map(|origin| view! { <span class="target">{origin}</span> })}
                    <span class="msg">{message_view(line.message, text)}</span>
                </div>
            }
            .into_any()
        }
    }
}

#[component]
pub fn Logs() -> impl IntoView {
    let state = AppState::get();
    let min_level = RwSignal::new(None::<LogLevel>);
    let target = RwSignal::new(None::<String>);
    let text = RwSignal::new(String::new());
    let following = RwSignal::new(true);
    let analysis_open = RwSignal::new(true);
    let nowrap = RwSignal::new(false);
    let copy_note = RwSignal::new(None::<String>);
    // Bumped by every copy, so the timer of an earlier copy does not clear a newer note.
    let copy_generation = StoredValue::new(0u64);
    let scroller = NodeRef::<html::Div>::new();

    let entries = Memo::new(move |_| {
        let filter = LogFilter {
            min_level: min_level.get(),
            text: text.get(),
            target: target.get(),
        };
        state.logs.with(|logs| logs.entries(&filter))
    });
    // The targets on offer: the most frequent in the buffer, plus the chosen one so the
    // select never shows a target that is not in its list.
    let targets = Memo::new(move |_| {
        let mut targets: Vec<String> = state.logs.with(|logs| {
            logs.targets_by_count(TARGET_CHOICES)
                .into_iter()
                .map(|(name, _)| name)
                .collect()
        });
        if let Some(chosen) = target.get()
            && !targets.contains(&chosen)
        {
            targets.push(chosen);
        }
        targets
    });
    let problems_only = Memo::new(move |_| min_level.get() == Some(LogLevel::Warn));
    let level_count = move |level: Option<LogLevel>| {
        state.insights.with(|insights| match level {
            None => insights.lines_total(),
            Some(level) => insights.totals().at_or_above(level),
        })
    };

    // The Logs tab shows the problems, so the badge count resets while it is visible.
    Effect::new(move |_| {
        let visible = state.tab.get() == Tab::Logs;
        let unseen = state.insights.with(|insights| insights.unseen_problems());
        if visible && unseen > 0 {
            state.insights.update(|insights| insights.mark_seen());
        }
    });

    let scroll_to_bottom = move || {
        if let Some(element) = scroller.get_untracked() {
            element.set_scroll_top(element.scroll_height());
        }
    };

    // After new lines render, or when the page is shown again, keep the newest line in view
    // unless the user has scrolled up.
    Effect::new(move |_| {
        entries.track();
        let visible = state.tab.get() == Tab::Logs;
        if visible && following.get_untracked() {
            request_animation_frame(scroll_to_bottom);
        }
    });

    let on_scroll = move |_| {
        if let Some(element) = scroller.get_untracked() {
            let at_bottom = element.scroll_top() + element.client_height()
                >= element.scroll_height() - BOTTOM_SLACK;
            if following.get_untracked() != at_bottom {
                following.set(at_bottom);
            }
        }
    };

    let jump_to_latest = move |_| {
        following.set(true);
        scroll_to_bottom();
    };

    let copy_visible = move |_| {
        let entries = entries.get_untracked();
        let count = entries.len();
        let text = entries_text(&entries);
        spawn_local(async move {
            let note = match copy_to_clipboard(&text).await {
                Ok(()) => format!("Copied {count} lines"),
                Err(error) => format!("Copy failed: {error}"),
            };
            copy_generation.update_value(|generation| *generation += 1);
            let mine = copy_generation.get_value();
            copy_note.set(Some(note));
            set_timeout(
                move || {
                    if copy_generation.get_value() == mine {
                        copy_note.set(None);
                    }
                },
                Duration::from_secs(3),
            );
        });
    };

    let on_pick = Callback::new(move |filter: String| text.set(filter));

    view! {
        <div class="toolbar logs-toolbar">
            <button
                class="logs-analysis-toggle"
                aria-expanded=move || analysis_open.get().to_string()
                aria-controls="logs-analysis"
                on:click=move |_| analysis_open.update(|open| *open = !*open)
            >
                "Analysis"
            </button>
            <select
                aria-label="Minimum level"
                prop:value=move || level_key(min_level.get())
                on:change=move |event| {
                    let value = event_target_value(&event);
                    let level = LEVEL_CHOICES
                        .iter()
                        .find(|(key, _, _)| *key == value)
                        .and_then(|(_, _, level)| *level);
                    min_level.set(level);
                }
            >
                {LEVEL_CHOICES
                    .iter()
                    .map(|(key, label, level)| {
                        let level = *level;
                        view! {
                            <option value=*key>
                                {move || format!("{label} ({})", level_count(level))}
                            </option>
                        }
                    })
                    .collect_view()}
            </select>
            <select
                aria-label="Target"
                class="logs-target"
                prop:value=move || target.get().unwrap_or_default()
                on:change=move |event| {
                    let value = event_target_value(&event);
                    target.set((!value.is_empty()).then_some(value));
                }
            >
                <option value="">"All targets"</option>
                <For
                    each=move || targets.get()
                    key=|name| name.clone()
                    children=|name| {
                        let value = name.clone();
                        view! { <option value=value>{name}</option> }
                    }
                />
            </select>
            <input
                type="search"
                placeholder="Filter text"
                aria-label="Filter text"
                prop:value=move || text.get()
                on:input=move |event| text.set(event_target_value(&event))
            />
            <button
                aria-pressed=move || problems_only.get().to_string()
                class:active=move || problems_only.get()
                on:click=move |_| {
                    min_level.set(if problems_only.get_untracked() {
                        None
                    } else {
                        Some(LogLevel::Warn)
                    });
                }
            >
                "Problems only"
            </button>
            <button
                aria-pressed=move || (!nowrap.get()).to_string()
                class:active=move || !nowrap.get()
                on:click=move |_| nowrap.update(|nowrap| *nowrap = !*nowrap)
            >
                "Wrap"
            </button>
            <button on:click=copy_visible>"Copy visible"</button>
            {move || copy_note.get().map(|note| view! { <span class="muted">{note}</span> })}
            <span class="spacer"></span>
            <span class="muted">
                {move || {
                    let shown = entries.with(|entries| {
                        entries.iter().filter(|e| matches!(e, LogEntry::Line(_))).count()
                    });
                    let dropped = state.insights.with(|insights| insights.dropped_lines());
                    let mut text =
                        format!("{shown} of {} lines", state.logs.with(|logs| logs.len()));
                    if dropped > 0 {
                        text.push_str(&format!(" · {dropped} dropped"));
                    }
                    text
                }}
            </span>
            {move || {
                (!following.get())
                    .then(|| {
                        view! {
                            <button class="primary" on:click=jump_to_latest>
                                "Paused — jump to latest"
                            </button>
                        }
                    })
            }}
        </div>
        <div id="logs-analysis" class="logs-analysis" hidden=move || !analysis_open.get()>
            <LogInsightsPanel on_pick=on_pick />
        </div>
        <div
            class="log-scroll"
            class:logs-nowrap=move || nowrap.get()
            node_ref=scroller
            on:scroll=on_scroll
        >
            <Show
                when=move || !state.logs.with(|logs| logs.is_empty())
                fallback=|| view! { <div class="log-row muted">"Waiting for zebrad output…"</div> }
            >
                <For
                    each=move || entries.get()
                    key=LogEntry::key
                    children=move |entry| entry_view(entry, text)
                />
            </Show>
        </div>
    }
}
