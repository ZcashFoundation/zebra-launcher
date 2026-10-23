//! The app shell: shared state, backend subscriptions, the header and the four pages.

use std::time::{Duration, SystemTime};

use launcher_types::{NodeStats, Settings, Status};
use leptos::{prelude::*, task::spawn_local};

use crate::{
    api, clock,
    components::{nav::Nav, status_strip::StatusStrip},
    log_view::LogBuffer,
    pages::{configure::Configure, dashboard::Dashboard, logs::Logs, settings::SettingsPage},
};

/// The four pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Dashboard,
    Logs,
    Configure,
    Settings,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Dashboard, Tab::Logs, Tab::Configure, Tab::Settings];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Dashboard => "Dashboard",
            Tab::Logs => "Logs",
            Tab::Configure => "Configure",
            Tab::Settings => "Settings",
        }
    }
}

/// State shared by every page, provided as context.
#[derive(Clone, Copy)]
pub struct AppState {
    pub tab: RwSignal<Tab>,
    pub status: RwSignal<Status>,
    pub stats: RwSignal<Option<NodeStats>>,
    /// `None` until loaded.
    pub settings: RwSignal<Option<Settings>>,
    pub logs: RwSignal<LogBuffer>,
    /// Ticks every second, for countdowns and "5 s ago" texts.
    pub now: RwSignal<SystemTime>,
    /// The last error from loading or subscribing, shown under the header.
    pub backend_error: RwSignal<Option<String>>,
}

impl AppState {
    pub fn get() -> Self {
        expect_context()
    }

    fn report(&self, what: &str, result: api::ApiResult<()>) {
        if let Err(error) = result {
            self.backend_error.set(Some(format!("{what}: {error}")));
        }
    }

    /// A callback for payloads that do not deserialize: shows the message under the header.
    fn payload_error(self) -> impl Fn(String) + 'static {
        move |message| self.backend_error.set(Some(message))
    }
}

#[component]
pub fn App() -> impl IntoView {
    let state = AppState {
        tab: RwSignal::new(Tab::Dashboard),
        status: RwSignal::new(Status::default()),
        stats: RwSignal::new(None),
        settings: RwSignal::new(None),
        logs: RwSignal::new(LogBuffer::default()),
        now: RwSignal::new(clock::now()),
        backend_error: RwSignal::new(None),
    };
    provide_context(state);

    set_interval(move || state.now.set(clock::now()), Duration::from_secs(1));

    // Subscribe before fetching, so no update falls between the fetch and the subscription.
    spawn_local(async move {
        let result = api::on_status(
            move |status| state.status.set(status),
            state.payload_error(),
        )
        .await;
        state.report("Listening for status", result);
        match api::get_status().await {
            Ok(status) => state.status.set(status),
            Err(error) => state.report("Reading status", Err(error)),
        }
    });
    spawn_local(async move {
        let result =
            api::on_node_stats(move |stats| state.stats.set(stats), state.payload_error()).await;
        state.report("Listening for node stats", result);
        match api::get_node_stats().await {
            Ok(stats) => state.stats.set(stats),
            Err(error) => state.report("Reading node stats", Err(error)),
        }
    });
    spawn_local(async move {
        match api::get_settings().await {
            Ok(settings) => state.settings.set(Some(settings)),
            Err(error) => state.report("Reading settings", Err(error)),
        }
    });
    spawn_local(async move {
        let result = api::subscribe_logs(
            move |batch| state.logs.update(|logs| logs.merge(batch)),
            state.payload_error(),
        )
        .await;
        state.report("Subscribing to logs", result);
        match api::log_history(0).await {
            Ok(history) => state.logs.update(|logs| logs.merge(history)),
            Err(error) => state.report("Reading log history", Err(error)),
        }
    });

    // Pages stay mounted while hidden, so a half-edited config or the log scroll position
    // survive switching tabs.
    let hidden = move |tab: Tab| move || state.tab.get() != tab;

    view! {
        <div class="app">
            <header class="app-header">
                <Nav />
                <StatusStrip />
                {move || {
                    state
                        .backend_error
                        .get()
                        .map(|error| {
                            view! {
                                <div class="banner banner-error" role="alert">
                                    <span>{error}</span>
                                    <button
                                        class="link"
                                        on:click=move |_| state.backend_error.set(None)
                                    >
                                        "Dismiss"
                                    </button>
                                </div>
                            }
                        })
                }}
            </header>
            <main class="app-main">
                <section class="page" hidden=hidden(Tab::Dashboard)>
                    <Dashboard />
                </section>
                <section class="page page-logs" hidden=hidden(Tab::Logs)>
                    <Logs />
                </section>
                <section class="page" hidden=hidden(Tab::Configure)>
                    <Configure />
                </section>
                <section class="page" hidden=hidden(Tab::Settings)>
                    <SettingsPage />
                </section>
            </main>
        </div>
    }
}
