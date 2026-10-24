//! The app shell: shared state, backend subscriptions, the header, the four pages and the
//! first-run wizard.

use std::time::{Duration, SystemTime};

use launcher_types::{
    LogLine, NodeStats, Settings, Status, Theme, UpdateHealthFailure, UpdateInfo,
};
use leptos::{ev, prelude::*, task::spawn_local};

use crate::{
    api, clock,
    components::{
        nav::Nav,
        stale_banner::StaleBanner,
        status_strip::StatusStrip,
        updates::{HealthFailureBanner, UpdateBanner},
    },
    format::update_needs_attention,
    insights::Insights,
    log_view::LogBuffer,
    pages::{
        configure::Configure, dashboard::Dashboard, first_run::FirstRun, grafana::Grafana,
        logs::Logs, settings::SettingsPage,
    },
    stats_history::StatsHistory,
    theme,
};

/// The five pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Dashboard,
    Logs,
    Grafana,
    Configure,
    Settings,
}

/// A mark on a tab: something there wants a look.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Badge {
    /// There is something to see, without a number.
    Dot,
    /// How many new things there are.
    // Constructed once the Logs tab's unseen-problem count is wired in `Tab::badge`; remove the
    // attribute then (the build fails to say so).
    #[cfg_attr(
        target_arch = "wasm32",
        expect(
            dead_code,
            reason = "the Logs tab's count is wired by the orchestrator"
        )
    )]
    Count(u32),
}

impl Badge {
    /// The number shown in the badge, if any.
    pub fn text(self) -> Option<String> {
        match self {
            Badge::Dot => None,
            Badge::Count(count) => Some(count.to_string()),
        }
    }

    /// What the badge means, for assistive technology.
    pub fn label(self) -> String {
        match self {
            Badge::Dot => "needs attention".to_owned(),
            Badge::Count(1) => "1 new".to_owned(),
            Badge::Count(count) => format!("{count} new"),
        }
    }
}

impl Tab {
    pub const ALL: [Tab; 5] = [
        Tab::Dashboard,
        Tab::Logs,
        Tab::Grafana,
        Tab::Configure,
        Tab::Settings,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Dashboard => "Dashboard",
            Tab::Logs => "Logs",
            Tab::Grafana => "Grafana",
            Tab::Configure => "Configure",
            Tab::Settings => "Settings",
        }
    }

    /// The tab's position, 1 to 5: its keyboard shortcut is Ctrl (or Cmd) plus this digit.
    pub fn number(self) -> usize {
        Tab::ALL.iter().position(|tab| *tab == self).unwrap_or(0) + 1
    }

    /// The tab for a keyboard shortcut, from a keydown's `key` and `code`: `1` to `5`, or the
    /// layout-independent `Digit1` to `Digit5`. The caller checks the modifier.
    pub fn from_shortcut(key: &str, code: &str) -> Option<Tab> {
        let digit = if key.len() == 1 && key.bytes().all(|b| b.is_ascii_digit()) {
            key
        } else {
            code.strip_prefix("Digit")?
        };
        let number: usize = digit.parse().ok()?;
        Tab::ALL.get(number.checked_sub(1)?).copied()
    }

    /// The mark on the tab, if anything there wants a look.
    pub fn badge(self, state: &AppState) -> Option<Badge> {
        match self {
            Tab::Settings => state
                .update
                .with(|info| {
                    info.as_ref()
                        .is_some_and(|info| update_needs_attention(&info.state))
                })
                .then_some(Badge::Dot),
            // The unseen-problem count from the log insights goes here (wired by the
            // orchestrator once the insights module lands).
            Tab::Logs => None,
            Tab::Dashboard | Tab::Grafana | Tab::Configure => None,
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
    /// What the log stream says about the node, folded in as batches arrive.
    pub insights: RwSignal<Insights>,
    /// The node statistics published since the launcher opened, for charts and rates.
    pub history: RwSignal<StatsHistory>,
    /// Whether the resolved appearance is dark (the setting, or the system preference when
    /// the setting follows it). Embedded pages pick their theme from it.
    pub resolved_dark: RwSignal<bool>,
    /// The appearance chosen in the Settings form while it differs from the stored one, so the
    /// choice shows at once; `None` otherwise. Set by the Settings page, read by `theme`.
    pub preview_theme: RwSignal<Option<Theme>>,
    /// Ticks every second, for countdowns and "5 s ago" texts.
    pub now: RwSignal<SystemTime>,
    /// The last error from loading or subscribing, shown under the header.
    pub backend_error: RwSignal<Option<String>>,
    /// What the updater is doing, `None` until loaded.
    pub update: RwSignal<Option<UpdateInfo>>,
    /// An `update_health_failed` event. The same failure is usually in
    /// `update.health_failure` too; see [`AppState::health_failure`].
    pub update_health: RwSignal<Option<UpdateHealthFailure>>,
    /// Bumped whenever `zebrad.toml` was replaced outside the Configure page's own save (a
    /// `reset_config` from any page), so the Configure page (always mounted) re-reads it.
    pub config_changed: RwSignal<u64>,
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

    /// Publishes new node statistics and keeps them in the history.
    fn record_stats(&self, stats: Option<NodeStats>) {
        if let Some(stats) = &stats {
            self.history.update(|history| history.push(stats));
        }
        self.stats.set(stats);
    }

    /// Folds a batch of log lines into the insights and the log buffer.
    fn record_logs(&self, batch: Vec<LogLine>) {
        let now = clock::now();
        self.insights
            .update(|insights| insights.ingest(&batch, now));
        self.logs.update(|logs| logs.merge(batch));
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
        insights: RwSignal::new(Insights::default()),
        history: RwSignal::new(StatsHistory::default()),
        resolved_dark: RwSignal::new(true),
        preview_theme: RwSignal::new(None),
        now: RwSignal::new(clock::now()),
        backend_error: RwSignal::new(None),
        update: RwSignal::new(None),
        update_health: RwSignal::new(None),
        config_changed: RwSignal::new(0),
    };
    provide_context(state);
    theme::init(&state);

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
                state.record_stats(stats);
            },
            state.payload_error(),
        )
        .await;
        state.report("Listening for node stats", result);
        match api::get_node_stats().await {
            Ok(stats) if !event_seen.get_value() => state.record_stats(stats),
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
        let result =
            api::subscribe_logs(move |batch| state.record_logs(batch), state.payload_error()).await;
        state.report("Subscribing to logs", result);
        match api::log_history(0).await {
            Ok(history) => state.record_logs(history),
            Err(error) => state.report("Reading log history", Err(error)),
        }
    });

    // Ctrl (or Cmd) + 1..5 switch tabs. The modifier keeps the shortcut out of the way of typing,
    // so it works while an input has focus too; a handler that already claimed the key wins.
    let shortcuts = window_event_listener(ev::keydown, move |event| {
        if event.default_prevented() || !(event.ctrl_key() || event.meta_key()) || event.alt_key() {
            return;
        }
        if let Some(tab) = Tab::from_shortcut(&event.key(), &event.code()) {
            event.prevent_default();
            state.tab.set(tab);
        }
    });
    on_cleanup(move || shortcuts.remove());

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
                <section class="page page-grafana" hidden=hidden(Tab::Grafana)>
                    <Grafana />
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_are_numbered_in_order() {
        assert_eq!(Tab::Dashboard.number(), 1);
        assert_eq!(Tab::Settings.number(), 5);
    }

    #[test]
    fn shortcuts_map_digits_to_tabs() {
        assert_eq!(Tab::from_shortcut("1", "Digit1"), Some(Tab::Dashboard));
        assert_eq!(Tab::from_shortcut("5", "Digit5"), Some(Tab::Settings));
        // Layouts where Ctrl changes the key: the code still names the digit.
        assert_eq!(Tab::from_shortcut("&", "Digit1"), Some(Tab::Dashboard));
        assert_eq!(Tab::from_shortcut("0", "Digit0"), None);
        assert_eq!(Tab::from_shortcut("6", "Digit6"), None);
        assert_eq!(Tab::from_shortcut("a", "KeyA"), None);
        assert_eq!(Tab::from_shortcut("Enter", "Enter"), None);
        assert_eq!(Tab::from_shortcut("1", "Numpad1"), Some(Tab::Dashboard));
    }

    #[test]
    fn badge_texts() {
        assert_eq!(Badge::Dot.text(), None);
        assert_eq!(Badge::Count(3).text(), Some("3".into()));
        assert_eq!(Badge::Dot.label(), "needs attention");
        assert_eq!(Badge::Count(1).label(), "1 new");
        assert_eq!(Badge::Count(12).label(), "12 new");
    }
}
