//! The app shell: shared state, backend subscriptions, the header, the five pages and the
//! first-run wizard.

use std::time::{Duration, SystemTime};

use launcher_types::{
    ApplyOutcome, LogLine, Settings, Theme, UpdateHealthFailure, UpdateInfo, UpdateState,
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
    insights::{
        RunEvent,
        events::{config_outcome_event, config_replaced_event, update_events},
    },
    node_state::NodeState,
    pages::{
        configure::Configure, dashboard::Dashboard, first_run::FirstRun, grafana::Grafana,
        logs::Logs, settings::SettingsPage,
    },
    theme,
    update_text::update_needs_attention,
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

    /// The tab `by` places to the right (to the left when negative), wrapping around the ends:
    /// the arrow keys in the tab bar.
    pub fn step(self, by: i32) -> Tab {
        let len = Tab::ALL.len() as i32;
        let index = (self.number() as i32 - 1 + by).rem_euclid(len);
        Tab::ALL[index as usize]
    }

    /// The tab's name in element ids.
    pub fn slug(self) -> &'static str {
        match self {
            Tab::Dashboard => "dashboard",
            Tab::Logs => "logs",
            Tab::Grafana => "grafana",
            Tab::Configure => "configure",
            Tab::Settings => "settings",
        }
    }

    /// The id of the tab's button in the tab bar; its page names it in `aria-labelledby`.
    pub fn button_id(self) -> String {
        format!("tab-{}", self.slug())
    }

    /// The id of the tab's page; the button names it in `aria-controls`.
    pub fn page_id(self) -> String {
        format!("page-{}", self.slug())
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
            // Warnings, errors and stderr lines that arrived while the Logs page was not shown.
            Tab::Logs => {
                let unseen = state
                    .node()
                    .insights
                    .with(|insights| insights.unseen_problems());
                (unseen > 0).then_some(Badge::Count(unseen))
            }
            Tab::Dashboard | Tab::Grafana | Tab::Configure => None,
        }
    }
}

/// State shared by every page, provided as context.
#[derive(Clone, Copy)]
pub struct AppState {
    pub tab: RwSignal<Tab>,
    /// The nodes the backend runs, in its order. One until it runs several.
    pub nodes: RwSignal<Vec<NodeState>>,
    /// The node the node pages (Dashboard, Logs, Configure) show; read it with
    /// [`AppState::node`].
    pub selected: RwSignal<NodeState>,
    /// `None` until loaded.
    pub settings: RwSignal<Option<Settings>>,
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
    /// A text filter another page asks the Logs page to apply (a problem picked on the
    /// dashboard). The Logs page takes it and clears it.
    pub log_filter_request: RwSignal<Option<String>>,
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

    /// The node the node pages show, tracked: a page that reads it follows a change of the
    /// selected node.
    pub fn node(&self) -> NodeState {
        self.selected.get()
    }

    /// [`AppState::node`] without tracking, for event handlers and the shell's own steps.
    pub fn node_untracked(&self) -> NodeState {
        self.selected.get_untracked()
    }

    /// Keeps an event that concerns every node (a launcher update stops and restarts them
    /// all) on each node's timeline.
    fn record_event_everywhere(&self, event: RunEvent) {
        self.nodes.with_untracked(|nodes| {
            for node in nodes {
                node.record_event(event.clone());
            }
        });
    }

    /// Publishes what the updater reports, turning its steps into session events
    /// ([`update_events`]).
    fn record_update(&self, info: UpdateInfo) {
        let now = clock::now();
        self.update.with_untracked(|previous| {
            for event in update_events(previous.as_ref(), &info, now) {
                self.record_event_everywhere(event);
            }
        });
        self.update.set(Some(info));
    }

    /// An `update_state` event: the loaded info moves along; before the info is loaded the
    /// state alone is not shown, since the info that follows carries it too.
    fn record_update_state(&self, update_state: UpdateState) {
        let info = self.update.with_untracked(|info| {
            info.as_ref().map(|info| UpdateInfo {
                state: update_state,
                ..info.clone()
            })
        });
        if let Some(info) = info {
            self.record_update(info);
        }
    }

    /// The Configure page saved the selected node's config: the outcome joins its timeline
    /// ("Configuration applied", or rolled back when zebrad rejected it).
    pub fn record_config_outcome(&self, outcome: &ApplyOutcome) {
        self.node_untracked()
            .record_event(config_outcome_event(outcome, clock::now()));
    }

    /// Folds a batch of `node`'s log lines into its state; problems in it count as seen when
    /// its Logs page is the one shown.
    fn record_logs(&self, node: NodeState, batch: Vec<LogLine>) {
        let on_logs_page = self.tab.get_untracked() == Tab::Logs && self.node_untracked() == node;
        node.record_logs(batch, on_logs_page);
    }

    /// Clears the Logs tab's badge: the user is looking at the selected node's Logs page. The
    /// shell calls it when the Logs tab opens; [`AppState::record_logs`] covers the batches
    /// that arrive while the tab is open. The Logs page itself has no part in the rule.
    pub fn mark_problems_seen(&self) {
        self.node_untracked().mark_problems_seen();
    }

    /// The post-update health failure, from the event or the update info.
    pub fn health_failure(&self) -> Option<UpdateHealthFailure> {
        self.update_health.get().or_else(|| {
            self.update
                .with(|info| info.as_ref().and_then(|info| info.health_failure.clone()))
        })
    }
}

/// Whether a subscription of [`subscribe_then_fetch`] has delivered an event yet.
#[derive(Clone, Copy)]
struct EventSeen(StoredValue<bool>);

impl EventSeen {
    fn mark(self) {
        self.0.set_value(true);
    }
}

/// The rule for a subscription that comes with a snapshot: subscribe before fetching, so no
/// update falls between the fetch and the subscription; then apply the snapshot only when no
/// event has arrived yet, since an event that arrives while the fetch is in flight is newer
/// than the fetched snapshot. `subscribe` sets up the listeners (reporting their own errors)
/// and calls `seen.mark()` from the one whose events the snapshot duplicates; `fetch` reads
/// the snapshot, `apply` stores it, and `reading` names the fetch in the error if it fails.
async fn subscribe_then_fetch<T, F>(
    state: AppState,
    subscribe: impl AsyncFnOnce(EventSeen),
    fetch: impl FnOnce() -> F,
    reading: &'static str,
    apply: impl FnOnce(T),
) where
    F: Future<Output = api::ApiResult<T>>,
{
    let seen = EventSeen(StoredValue::new(false));
    subscribe(seen).await;
    match fetch().await {
        Ok(snapshot) if !seen.0.get_value() => apply(snapshot),
        Ok(_) => {}
        Err(error) => state.report(reading, Err(error)),
    }
}

#[component]
pub fn App() -> impl IntoView {
    // The backend runs one node; every node event below is about it.
    let node = NodeState::new();
    let state = AppState {
        tab: RwSignal::new(Tab::Dashboard),
        nodes: RwSignal::new(vec![node]),
        selected: RwSignal::new(node),
        settings: RwSignal::new(None),
        resolved_dark: RwSignal::new(true),
        preview_theme: RwSignal::new(None),
        now: RwSignal::new(clock::now()),
        backend_error: RwSignal::new(None),
        update: RwSignal::new(None),
        update_health: RwSignal::new(None),
        log_filter_request: RwSignal::new(None),
    };
    provide_context(state);
    theme::init(&state);

    set_interval(move || state.now.set(clock::now()), Duration::from_secs(1));

    // The status, the node stats and the updater each come as a stream plus a snapshot; see
    // `subscribe_then_fetch` for the order.
    spawn_local(subscribe_then_fetch(
        state,
        async move |seen| {
            let result = api::on_status(
                move |status| {
                    seen.mark();
                    node.record_status(status);
                },
                state.payload_error(),
            )
            .await;
            state.report("Listening for status", result);
            // `stale_process` may have been emitted before this listener existed; the
            // snapshot's `stale` covers that case.
            let result =
                api::on_stale_process(move |stale| node.record_stale(stale), state.payload_error())
                    .await;
            state.report("Listening for stale processes", result);
        },
        api::get_status,
        "Reading status",
        move |status| node.record_status(status),
    ));
    spawn_local(subscribe_then_fetch(
        state,
        async move |seen| {
            let result = api::on_node_stats(
                move |stats| {
                    seen.mark();
                    node.record_stats(stats);
                },
                state.payload_error(),
            )
            .await;
            state.report("Listening for node stats", result);
        },
        api::get_node_stats,
        "Reading node stats",
        move |stats| node.record_stats(stats),
    ));
    spawn_local(subscribe_then_fetch(
        state,
        async move |seen| {
            let result = api::on_node_details(
                move |details| {
                    seen.mark();
                    node.record_details(details);
                },
                state.payload_error(),
            )
            .await;
            state.report("Listening for node details", result);
        },
        api::get_node_details,
        "Reading node details",
        move |details| node.record_details(details),
    ));
    spawn_local(subscribe_then_fetch(
        state,
        async move |seen| {
            let result = api::on_metrics(
                move |snapshot| {
                    seen.mark();
                    node.record_metrics(snapshot);
                },
                state.payload_error(),
            )
            .await;
            state.report("Listening for metrics", result);
        },
        api::get_metrics,
        "Reading metrics",
        move |snapshot| node.record_metrics(snapshot),
    ));
    // `update_info` carries the state too, and the backend sends it right after each
    // `update_state`, so the info event and the snapshot are enough. An `update_state` only
    // moves a loaded info along a little earlier.
    spawn_local(subscribe_then_fetch(
        state,
        async move |seen| {
            let result = api::on_update_info(
                move |info| {
                    seen.mark();
                    state.record_update(info);
                },
                state.payload_error(),
            )
            .await;
            state.report("Listening for updates", result);
            let result = api::on_update_state(
                move |update_state| state.record_update_state(update_state),
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
        },
        api::get_update_info,
        "Reading the update state",
        move |info| state.record_update(info),
    ));
    spawn_local(async move {
        match api::get_settings().await {
            Ok(settings) => state.settings.set(Some(settings)),
            Err(error) => state.report("Reading settings", Err(error)),
        }
    });
    // The subscription streams the retained history before the live lines, so no separate
    // history request is needed.
    spawn_local(async move {
        let result = api::subscribe_logs(
            move |batch| state.record_logs(node, batch),
            state.payload_error(),
        )
        .await;
        state.report("Subscribing to logs", result);
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

    // Opening the Logs page clears its badge.
    Effect::new(move |_| {
        if state.tab.get() == Tab::Logs {
            state.mark_problems_seen();
        }
    });

    // A reset of the node's config from any page is an event of its run.
    Effect::new(move |previous: Option<u64>| {
        let changed = node.config_changed.get();
        if previous.is_some_and(|previous| previous != changed) {
            node.record_event(config_replaced_event(clock::now()));
        }
        changed
    });

    // Pages stay mounted while hidden, so a half-edited config or the log scroll position
    // survive switching tabs. Each page is the tab panel of its tab button in the header.
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
                <section
                    class="page"
                    role="tabpanel"
                    id=Tab::Dashboard.page_id()
                    aria-labelledby=Tab::Dashboard.button_id()
                    hidden=hidden(Tab::Dashboard)
                >
                    <Dashboard />
                </section>
                <section
                    class="page page-logs"
                    role="tabpanel"
                    id=Tab::Logs.page_id()
                    aria-labelledby=Tab::Logs.button_id()
                    hidden=hidden(Tab::Logs)
                >
                    <Logs />
                </section>
                <section
                    class="page page-grafana"
                    role="tabpanel"
                    id=Tab::Grafana.page_id()
                    aria-labelledby=Tab::Grafana.button_id()
                    hidden=hidden(Tab::Grafana)
                >
                    <Grafana />
                </section>
                <section
                    class="page"
                    role="tabpanel"
                    id=Tab::Configure.page_id()
                    aria-labelledby=Tab::Configure.button_id()
                    hidden=hidden(Tab::Configure)
                >
                    <Configure />
                </section>
                <section
                    class="page"
                    role="tabpanel"
                    id=Tab::Settings.page_id()
                    aria-labelledby=Tab::Settings.button_id()
                    hidden=hidden(Tab::Settings)
                >
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
    fn steps_wrap_around_the_tab_bar() {
        assert_eq!(Tab::Dashboard.step(1), Tab::Logs);
        assert_eq!(Tab::Grafana.step(-1), Tab::Logs);
        // Past either end, the bar wraps.
        assert_eq!(Tab::Settings.step(1), Tab::Dashboard);
        assert_eq!(Tab::Dashboard.step(-1), Tab::Settings);
        assert_eq!(Tab::Logs.step(0), Tab::Logs);
        assert_eq!(Tab::Logs.step(5), Tab::Logs);
        assert_eq!(Tab::Logs.step(-7), Tab::Settings);
    }

    #[test]
    fn ids_link_each_tab_to_its_page() {
        for tab in Tab::ALL {
            assert_eq!(tab.slug(), tab.label().to_ascii_lowercase());
            assert_eq!(tab.button_id(), format!("tab-{}", tab.slug()));
            assert_eq!(tab.page_id(), format!("page-{}", tab.slug()));
        }
        assert_eq!(Tab::Grafana.button_id(), "tab-grafana");
        assert_eq!(Tab::Grafana.page_id(), "page-grafana");
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
