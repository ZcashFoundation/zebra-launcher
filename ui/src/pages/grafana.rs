//! The Grafana tab: the user's Grafana dashboards embedded in the app, behind a toolbar with a
//! dashboard picker, time range and refresh interval. While no Grafana URL is set, a setup card
//! explains the shipped observability stack and saves a URL into the settings.

use std::time::Duration;

use launcher_types::is_loopback_http_url;
use leptos::{prelude::*, task::spawn_local};

use crate::{
    api,
    app::{AppState, Tab},
    clipboard::copy_to_clipboard,
    grafana_url::{
        DASHBOARDS, DEFAULT_UID, Range, Refresh, custom_uid, embed_url, host_in, uid_in,
    },
};

/// Where the shipped compose file publishes Grafana.
const SUGGESTED_URL: &str = "http://127.0.0.1:3000";

/// Starts the shipped Prometheus and Grafana stack (Docker Desktop; Linux adds an override).
const COMPOSE_COMMAND: &str = "docker compose -f docs/observability/docker-compose.yml up -d";

/// The one rule for the URL, shared with the backend: it mirrors the CSP's `frame-src`.
const URL_ERROR: &str = "Only http:// URLs on 127.0.0.1 or localhost can be embedded.";

/// `localStorage` keys for the remembered picker choices.
const STORED_DASHBOARD: &str = "grafana.dashboard";
const STORED_RANGE: &str = "grafana.range";
const STORED_REFRESH: &str = "grafana.refresh";

/// How long the copy result stays visible.
const NOTE_DURATION: Duration = Duration::from_secs(3);

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

/// A remembered choice, `None` when storage is unavailable or has none.
fn remembered(key: &str) -> Option<String> {
    storage()?.get_item(key).ok().flatten()
}

/// Remembers a choice. Failures (storage disabled or full) are ignored: the choice still
/// applies for this session.
fn remember(key: &str, value: &str) {
    if let Some(storage) = storage() {
        let _ = storage.set_item(key, value);
    }
}

#[component]
pub fn Grafana() -> impl IntoView {
    let state = AppState::get();
    // `None` while the settings load, then the Grafana URL or `None` when there is none. Only a
    // change of the URL itself re-creates the page below.
    let url = Memo::new(move |_| {
        state.settings.with(|settings| {
            settings
                .as_ref()
                .map(|settings| settings.grafana_url.clone())
        })
    });
    view! {
        {move || match url.get() {
            None => view! { <p class="muted grafana-loading">"Loading…"</p> }.into_any(),
            Some(None) => view! { <Setup /> }.into_any(),
            Some(Some(url)) => view! { <Embedded url /> }.into_any(),
        }}
    }
}

/// The card shown while no Grafana URL is set: how to start the shipped stack, and a URL field
/// that saves into the settings.
#[component]
fn Setup() -> impl IntoView {
    let state = AppState::get();
    let input = RwSignal::new(SUGGESTED_URL.to_owned());
    let saving = RwSignal::new(false);
    let save_error = RwSignal::new(None::<String>);
    let copy_note = RwSignal::new(None::<String>);
    // Bumped by every copy, so the timer of an earlier copy does not clear a newer note.
    let copy_generation = StoredValue::new(0u64);

    let valid = Memo::new(move |_| input.with(|input| is_loopback_http_url(input.trim())));
    // The rule, shown as soon as something invalid is typed; an empty field just disables Save.
    let url_error = Memo::new(move |_| {
        let empty = input.with(|input| input.trim().is_empty());
        (!empty && !valid.get()).then_some(URL_ERROR)
    });

    let copy_command = move |_| {
        spawn_local(async move {
            let note = match copy_to_clipboard(COMPOSE_COMMAND).await {
                Ok(()) => "Copied".to_owned(),
                Err(error) => format!("Copy failed: {error}"),
            };
            copy_generation.update_value(|generation| *generation += 1);
            let mine = copy_generation.get_value();
            copy_note.set(Some(note));
            set_timeout(
                move || {
                    if copy_generation.get_value() == mine {
                        copy_note.try_set(None);
                    }
                },
                NOTE_DURATION,
            );
        });
    };

    let use_this = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        let url = input.with_untracked(|input| input.trim().to_owned());
        if saving.get_untracked() || !is_loopback_http_url(&url) {
            return;
        }
        let Some(mut settings) = state.settings.get_untracked() else {
            return;
        };
        settings.grafana_url = Some(url);
        saving.set(true);
        save_error.set(None);
        spawn_local(async move {
            match api::set_settings(settings).await {
                // Storing the URL swaps this card for the dashboards, which disposes its signals.
                Ok(stored) => state.settings.set(Some(stored)),
                Err(error) => {
                    save_error.try_set(Some(format!("Could not save: {error}")));
                }
            }
            saving.try_set(false);
        });
    };

    view! {
        <div class="grafana-setup-wrap">
            <div class="card grafana-setup">
                <h2>"Connect Grafana"</h2>
                <p>
                    "zebrad exports Prometheus metrics while it runs. This repository ships a Prometheus and Grafana stack with 14 Zebra dashboards; start it with Docker from the repository root:"
                </p>
                <div class="grafana-command">
                    <pre>{COMPOSE_COMMAND}</pre>
                    <button type="button" on:click=copy_command>"Copy"</button>
                    {move || copy_note.get().map(|note| view! { <span class="muted">{note}</span> })}
                </div>
                <p class="muted grafana-linux">
                    "On Linux, add " <code>"-f docs/observability/docker-compose.linux.yml"</code>
                    " before " <code>"up"</code> "."
                </p>
                <p>
                    "Grafana must allow embedding. The shipped compose file sets "
                    <code>"GF_SECURITY_ALLOW_EMBEDDING=true"</code>
                    " and an anonymous viewer, so the dashboards load here without a login."
                </p>
                <form class="grafana-connect" novalidate on:submit=use_this>
                    <label class="grafana-field">
                        <span>"Grafana URL"</span>
                        <input
                            type="url"
                            spellcheck="false"
                            autocomplete="off"
                            class:invalid=move || url_error.get().is_some()
                            prop:value=move || input.get()
                            on:input=move |event| {
                                input.set(event_target_value(&event));
                                save_error.set(None);
                            }
                        />
                        {move || url_error.get().map(|error| view! { <span class="error">{error}</span> })}
                    </label>
                    <div class="buttons">
                        <button
                            type="submit"
                            class="primary"
                            disabled=move || saving.get() || !valid.get()
                        >
                            {move || if saving.get() { "Saving…" } else { "Use this Grafana" }}
                        </button>
                        {move || save_error.get().map(|error| view! { <span class="error">{error}</span> })}
                    </div>
                </form>
            </div>
        </div>
    }
}

/// The toolbar and the embedded dashboard for one Grafana URL. Mounted anew when the URL
/// changes, so everything derived from it is plain data here.
#[component]
fn Embedded(url: String) -> impl IntoView {
    let state = AppState::get();
    let host = host_in(&url);
    // The user's own dashboard first, when the URL names one this repository does not ship.
    let options: Vec<(String, String)> = custom_uid(&url)
        .into_iter()
        .map(|uid| (uid.clone(), format!("From your URL: {uid}")))
        .chain(
            DASHBOARDS
                .iter()
                .map(|dashboard| (dashboard.uid.to_owned(), dashboard.title.to_owned())),
        )
        .collect();

    // What the user picked here (remembered across launches); the URL's own dashboard and the
    // default stand in while there is no valid pick.
    let pick = RwSignal::new(remembered(STORED_DASHBOARD));
    let range = RwSignal::new(
        remembered(STORED_RANGE)
            .and_then(|key| Range::parse(&key))
            .unwrap_or_default(),
    );
    let refresh = RwSignal::new(
        remembered(STORED_REFRESH)
            .and_then(|key| Refresh::parse(&key))
            .unwrap_or_default(),
    );
    // Bumped by Reload: the frame is re-created with the same src.
    let reloads = RwSignal::new(0u64);
    let open_error = RwSignal::new(None::<String>);

    let selected = {
        let url = url.clone();
        let options = options.clone();
        Memo::new(move |_| {
            pick.get()
                .filter(|pick| options.iter().any(|(uid, _)| uid == pick))
                .or_else(|| uid_in(&url))
                .unwrap_or_else(|| DEFAULT_UID.to_owned())
        })
    };
    // A memo, so only a change of these inputs reloads the frame.
    let src = {
        let url = url.clone();
        Memo::new(move |_| {
            embed_url(
                &url,
                &selected.get(),
                range.get(),
                refresh.get(),
                state.resolved_dark.get(),
            )
        })
    };

    let pick_dashboard = move |event| {
        let uid = event_target_value(&event);
        remember(STORED_DASHBOARD, &uid);
        pick.set(Some(uid));
    };
    let pick_range = move |event| {
        if let Some(picked) = Range::parse(&event_target_value(&event)) {
            remember(STORED_RANGE, picked.key());
            range.set(picked);
        }
    };
    let pick_refresh = move |event| {
        if let Some(picked) = Refresh::parse(&event_target_value(&event)) {
            remember(STORED_REFRESH, picked.key());
            refresh.set(picked);
        }
    };
    let open_in_browser = move |_| {
        open_error.set(None);
        spawn_local(async move {
            if let Err(error) = api::open_path("grafana").await {
                open_error.try_set(Some(format!("Could not open the browser: {error}")));
            }
        });
    };

    view! {
        <div class="grafana-toolbar">
            <select aria-label="Dashboard" prop:value=move || selected.get() on:change=pick_dashboard>
                {options
                    .iter()
                    .map(|(uid, title)| view! { <option value=uid.clone()>{title.clone()}</option> })
                    .collect_view()}
            </select>
            <select aria-label="Time range" prop:value=move || range.get().key() on:change=pick_range>
                {Range::ALL
                    .into_iter()
                    .map(|range| view! { <option value=range.key()>{range.label()}</option> })
                    .collect_view()}
            </select>
            <select aria-label="Refresh" prop:value=move || refresh.get().key() on:change=pick_refresh>
                {Refresh::ALL
                    .into_iter()
                    .map(|refresh| view! { <option value=refresh.key()>{refresh.label()}</option> })
                    .collect_view()}
            </select>
            <button type="button" on:click=move |_| reloads.update(|count| *count += 1)>
                "Reload"
            </button>
            <button type="button" on:click=open_in_browser>"Open in browser"</button>
            {move || open_error.get().map(|error| view! { <span class="error">{error}</span> })}
            <span class="grafana-host">
                <span class="muted">{host}</span>
                <button type="button" class="link" on:click=move |_| state.tab.set(Tab::Settings)>
                    "Change"
                </button>
            </span>
        </div>
        <p class="muted grafana-hint">
            "Blank? Make sure Grafana is running at " <code>{url}</code>
            " and allows embedding (see docs/GRAFANA.md)."
        </p>
        // Keyed on the reload counter, so Reload replaces the frame (a rebuilt frame with the
        // same src would be left alone); a pick or theme change updates the src in place.
        <For
            each=move || [reloads.get()]
            key=|generation| *generation
            children=move |_| {
                view! {
                    <iframe
                        class="grafana-frame"
                        title="Grafana dashboard"
                        src=move || src.get()
                        sandbox="allow-scripts allow-same-origin allow-forms"
                        referrerpolicy="no-referrer"
                    ></iframe>
                }
            }
        />
    }
}
