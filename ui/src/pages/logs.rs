//! The log viewer: zebrad's output with level colours, filters, follow-at-bottom and copy.

use std::time::Duration;

use launcher_types::{LogLevel, LogLine, LogStream};
use leptos::{html, prelude::*, task::spawn_local};

use crate::{
    app::{AppState, Tab},
    log_view::{LogEntry, LogFilter, entries_text, time_of_day},
};

/// How close to the bottom, in pixels, still counts as "at the bottom".
const BOTTOM_SLACK: i32 = 8;

const LEVEL_CHOICES: [(&str, &str, Option<LogLevel>); 6] = [
    ("all", "All levels", None),
    ("trace", "TRACE and above", Some(LogLevel::Trace)),
    ("debug", "DEBUG and above", Some(LogLevel::Debug)),
    ("info", "INFO and above", Some(LogLevel::Info)),
    ("warn", "WARN and above", Some(LogLevel::Warn)),
    ("error", "ERROR only", Some(LogLevel::Error)),
];

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

fn entry_view(entry: LogEntry) -> AnyView {
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
                    <span class="msg">{line.message}</span>
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
    let text = RwSignal::new(String::new());
    let following = RwSignal::new(true);
    let copy_note = RwSignal::new(None::<String>);
    // Bumped by every copy, so the timer of an earlier copy does not clear a newer note.
    let copy_generation = StoredValue::new(0u64);
    let scroller = NodeRef::<html::Div>::new();

    let entries = Memo::new(move |_| {
        let filter = LogFilter {
            min_level: min_level.get(),
            text: text.get(),
        };
        state.logs.with(|logs| logs.entries(&filter))
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

    view! {
        <div class="toolbar">
            <select
                aria-label="Minimum level"
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
                    .map(|(key, label, _)| view! { <option value=*key>{*label}</option> })
                    .collect_view()}
            </select>
            <input
                type="search"
                placeholder="Filter text"
                aria-label="Filter text"
                prop:value=move || text.get()
                on:input=move |event| text.set(event_target_value(&event))
            />
            <button on:click=copy_visible>"Copy visible"</button>
            {move || copy_note.get().map(|note| view! { <span class="muted">{note}</span> })}
            <span class="spacer"></span>
            <span class="muted">
                {move || {
                    let shown = entries.with(|entries| {
                        entries.iter().filter(|e| matches!(e, LogEntry::Line(_))).count()
                    });
                    format!("{shown} of {} lines", state.logs.with(|logs| logs.len()))
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
        <div class="log-scroll" node_ref=scroller on:scroll=on_scroll>
            <Show
                when=move || !state.logs.with(|logs| logs.is_empty())
                fallback=|| view! { <div class="log-row muted">"Waiting for zebrad output…"</div> }
            >
                <For each=move || entries.get() key=LogEntry::key children=entry_view />
            </Show>
        </div>
    }
}

pub async fn copy_to_clipboard(text: &str) -> Result<(), String> {
    let window = web_sys::window().ok_or("no window")?;
    let promise = window.navigator().clipboard().write_text(text);
    wasm_bindgen_futures::JsFuture::from(promise)
        .await
        .map(drop)
        .map_err(|error| crate::api::ApiError::from(error).0)
}
