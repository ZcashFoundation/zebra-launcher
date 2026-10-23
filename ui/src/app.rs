//! The app shell: shared state, backend subscriptions, the header, the four pages and the
//! first-run wizard.

use std::time::{Duration, SystemTime};

use launcher_types::{
    ConfigDocument, NodeStats, Settings, Status, UpdateHealthFailure, UpdateInfo,
};
use leptos::{prelude::*, task::spawn_local};

use crate::{
    api, clock,
    components::{
        nav::Nav,
        stale_banner::StaleBanner,
        status_strip::StatusStrip,
        updates::{HealthFailureBanner, UpdateBanner},
    },
    log_view::LogBuffer,
    pages::{
        configure::Configure, dashboard::Dashboard, first_run::FirstRun, logs::Logs,
        settings::SettingsPage,
    },
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
    /// What the updater is doing, `None` until loaded.
    pub update: RwSignal<Option<UpdateInfo>>,
    /// An `update_health_failed` event. The same failure is usually in
    /// `update.health_failure` too; see [`AppState::health_failure`].
    pub update_health: RwSignal<Option<UpdateHealthFailure>>,
    /// The `zebrad.toml` written by the last `reset_config`, from whichever page reset it, so
    /// the Configure page (always mounted) shows it.
    pub config_reset: RwSignal<Option<ConfigDocument>>,
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

    /// The post-update health failure, from the event or the update info.
    pub fn health_failure(&self) -> Option<UpdateHealthFailure> {
        self.update_health.get().or_else(|| {
            self.update
                .with(|info| info.as_ref().and_then(|info| info.health_failure.clone()))
        })
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
        update: RwSignal::new(None),
        update_health: RwSignal::new(None),
        config_reset: RwSignal::new(None),
    };
    provide_context(state);

    set_interval(move || state.now.set(clock::now()), Duration::from_secs(1));

    // Subscribe before fetching, so no update falls between the fetch and the subscription. An
    // event that arrives while the fetch is in flight is newer than the fetched snapshot, so the
    // snapshot is only applied when no event has arrived yet.
    spawn_local(async move {
        let event_seen = StoredValue::new(false);
        let result = api::on_status(
            move |status| {
                event_seen.set_value(true);
                state.status.set(status);
            },
            state.payload_error(),
        )
        .await;
        state.report("Listening for status", result);
        // `stale_process` may have been emitted before this listener existed; the snapshot's
        // `stale` covers that case.
        let result = api::on_stale_process(
            move |stale| state.status.update(|status| status.stale = Some(stale)),
            state.payload_error(),
        )
        .await;
        state.report("Listening for stale processes", result);
        match api::get_status().await {
            Ok(status) if !event_seen.get_value() => state.status.set(status),
            Ok(_) => {}
            Err(error) => state.report("Reading status", Err(error)),
        }
    });
    spawn_local(async move {
        let event_seen = StoredValue::new(false);
        let result = api::on_node_stats(
            move |stats| {
                event_seen.set_value(true);
                state.stats.set(stats);
            },
            state.payload_error(),
        )
        .await;
        state.report("Listening for node stats", result);
        match api::get_node_stats().await {
            Ok(stats) if !event_seen.get_value() => state.stats.set(stats),
            Ok(_) => {}
            Err(error) => state.report("Reading node stats", Err(error)),
        }
    });
    // The same rule for the updater: `update_info` carries the state too, and the backend sends
    // it right after each `update_state`, so the info event and the snapshot are enough. An
    // `update_state` only moves a loaded info along a little earlier.
    spawn_local(async move {
        let info_seen = StoredValue::new(false);
        let result = api::on_update_info(
            move |info| {
                info_seen.set_value(true);
                state.update.set(Some(info));
            },
            state.payload_error(),
        )
        .await;
        state.report("Listening for updates", result);
        let result = api::on_update_state(
            move |update_state| {
                if state.update.with_untracked(Option::is_some) {
                    state.update.update(|info| {
                        if let Some(info) = info {
                            info.state = update_state;
                        }
                    });
                }
            },
            state.payload_error(),
        )
        .await;
        state.report("Listening for update states", result);
        let result = api::on_update_health_failed(
            move |failure| state.update_health.set(Some(failure)),
            state.payload_error(),
        )
        .await;
        state.report("Listening for update health", result);
        match api::get_update_info().await {
            Ok(info) if !info_seen.get_value() => state.update.set(Some(info)),
            Ok(_) => {}
            Err(error) => state.report("Reading the update state", Err(error)),
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
    // While the first-run wizard is open, the app behind it takes no input.
    let first_run_open = RwSignal::new(false);

    view! {
        <div class="app" inert=move || first_run_open.get()>
            <header class="app-header">
                <Nav />
                <StatusStrip />
                <StaleBanner />
                <UpdateBanner />
                <HealthFailureBanner />
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
        <FirstRun open=first_run_open />
    }
}
