//! The app shell: shared state, backend subscriptions, the header, the five pages and the
//! first-run wizard.

use std::time::{Duration, SystemTime};

use launcher_types::{
    ApplyOutcome, LogLine, MachineResources, Network, Settings, Theme, UpdateHealthFailure,
    UpdateInfo, UpdateState,
};
use leptos::{ev, prelude::*, task::spawn_local};
use wasm_bindgen::JsCast;

use crate::{
    api, clock,
    components::{
        add_node::AddNode,
        herd_header::HerdHeader,
        nav::Nav,
        sidebar::Sidebar,
        stale_banner::StaleBanner,
        status_strip::StatusStrip,
        updates::{HealthFailureBanner, UpdateBanner},
    },
    insights::{
        RunEvent,
        events::{config_outcome_event, config_replaced_event, update_events},
    },
    node_state::{self, NodeState},
    pages::{
        configure::Configure, dashboard::Dashboard, first_run::FirstRun, grafana::Grafana,
        logs::Logs, overview::Overview, settings::SettingsPage,
    },
    theme,
    update_text::update_needs_attention,
};

/// The pages: the five of the tab bar, and the overview of a launcher that runs several nodes,
/// which the sidebar shows instead of the tab bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Overview,
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

    /// The pages about the selected node, as opposed to the overview and the launcher's own.
    pub const NODE: [Tab; 3] = [Tab::Dashboard, Tab::Logs, Tab::Configure];

    pub fn is_node_tab(self) -> bool {
        Tab::NODE.contains(&self)
    }

    pub fn label(self) -> &'static str {
        match self {
            Tab::Overview => "Overview",
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
            Tab::Overview => "overview",
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
            Tab::Overview | Tab::Dashboard | Tab::Grafana | Tab::Configure => None,
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
    /// The node the backend's single-node commands control (`start_zebrad`, `reset_config`,
    /// the data folder): what the launcher's own pages and banners are about, whichever node
    /// is selected.
    pub primary: NodeState,
    /// The network the shell shows when it runs several nodes: the sidebar, the overview and
    /// the node pages list only that network's nodes, and Testnet is marked everywhere.
    pub network: RwSignal<Network>,
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
    /// The computer's CPU and memory, read every 5 s; `None` until the first reading, and on
    /// platforms the backend cannot read.
    pub machine: RwSignal<Option<MachineResources>>,
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

    /// The nodes on `network`, in the backend's order, tracked.
    pub fn nodes_on(&self, network: Network) -> Vec<NodeState> {
        self.nodes.with(|nodes| {
            nodes
                .iter()
                .copied()
                .filter(|node| node.info.with(|info| info.network) == network)
                .collect()
        })
    }

    /// The nodes on the network the shell shows ([`AppState::network`]), tracked.
    pub fn scoped_nodes(&self) -> Vec<NodeState> {
        self.nodes_on(self.network.get())
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
        self.primary
            .record_event(config_outcome_event(outcome, clock::now()));
    }

    /// Folds a batch of `node`'s log lines into its state; problems in it count as seen when
    /// its Logs page is the one shown.
    fn record_logs(&self, node: NodeState, batch: Vec<LogLine>) {
        let on_logs_page = self.tab.get_untracked() == Tab::Logs && self.node_untracked() == node;
        node.record_logs(batch, on_logs_page);
    }

    /// The post-update health failure, from the event or the update info.
    pub fn health_failure(&self) -> Option<UpdateHealthFailure> {
        self.update_health.get().or_else(|| {
            self.update
                .with(|info| info.as_ref().and_then(|info| info.health_failure.clone()))
        })
    }
}

/// The node to show on `scope`: the selected one (`current`, an index into `networks`, the
/// nodes' networks) while it is on that network, else the first node that is, else none.
pub fn pick_in_scope(
    current: Option<usize>,
    networks: &[Network],
    scope: Network,
) -> Option<usize> {
    current
        .filter(|&index| networks.get(index) == Some(&scope))
        .or_else(|| networks.iter().position(|network| *network == scope))
}

/// The network the window shows: the chosen scope with several nodes, the node's own with one.
fn shown_network(state: AppState, herd: bool) -> Network {
    if herd {
        state.network.get()
    } else {
        state.primary.info.with(|info| info.network)
    }
}

/// Feeds an extra node's state from its events (the mock's `?herd` nodes).
pub fn subscribe_extra(state: AppState, extra: NodeState) {
    let id = extra.id();
    api::on_extra_node_status(&id, move |status| extra.record_status(status));
    api::on_extra_node_stats(&id, move |stats| extra.record_stats(stats));
    api::subscribe_extra_node_logs(&id, move |batch| state.record_logs(extra, batch));
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
    // The backend's own node: every single-node event below is about it. The mock's `?herd`
    // adds more, which come with their own subscriptions.
    let node = NodeState::new(node_state::placeholder_primary(), true);
    let extra: Vec<NodeState> = api::extra_nodes()
        .into_iter()
        .map(|info| NodeState::new(info, false))
        .collect();
    // Several nodes open on the overview; one node opens on its dashboard, as before.
    let first_tab = if extra.is_empty() {
        Tab::Dashboard
    } else {
        Tab::Overview
    };
    let state = AppState {
        tab: RwSignal::new(first_tab),
        nodes: RwSignal::new(std::iter::once(node).chain(extra.iter().copied()).collect()),
        selected: RwSignal::new(node),
        primary: node,
        network: RwSignal::new(Network::Mainnet),
        settings: RwSignal::new(None),
        resolved_dark: RwSignal::new(true),
        preview_theme: RwSignal::new(None),
        now: RwSignal::new(clock::now()),
        backend_error: RwSignal::new(None),
        update: RwSignal::new(None),
        update_health: RwSignal::new(None),
        log_filter_request: RwSignal::new(None),
        machine: RwSignal::new(None),
    };
    provide_context(state);
    theme::init(&state);

    set_interval(move || state.now.set(clock::now()), Duration::from_secs(1));

    // The status, the node stats, the resources and the updater each come as a stream plus a
    // snapshot; see `subscribe_then_fetch` for the order.
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
    spawn_local(subscribe_then_fetch(
        state,
        async move |seen| {
            let result = api::on_node_resources(
                move |sample| {
                    seen.mark();
                    node.record_resources(sample);
                },
                state.payload_error(),
            )
            .await;
            state.report("Listening for node resources", result);
        },
        api::get_node_resources,
        "Reading node resources",
        move |sample| node.record_resources(sample),
    ));
    spawn_local(subscribe_then_fetch(
        state,
        async move |seen| {
            let result = api::on_machine_resources(
                move |machine| {
                    seen.mark();
                    state.machine.set(machine);
                },
                state.payload_error(),
            )
            .await;
            state.report("Listening for the computer's resources", result);
        },
        api::get_machine_resources,
        "Reading the computer's resources",
        move |machine| state.machine.set(machine),
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

    // Opening the Logs page clears its badge, and so does picking another node while it is
    // open; `AppState::record_logs` covers the batches that arrive while it is shown. The Logs
    // page itself has no part in the rule.
    Effect::new(move |_| {
        let node = state.node();
        if state.tab.get() == Tab::Logs {
            node.mark_problems_seen();
        }
    });

    // The shell shows the backend node's network once the settings say which it is, and moves
    // with it when the first-run wizard or Settings changes it. A save that keeps the network
    // leaves the network the user picked.
    Effect::new(move |last: Option<Option<Network>>| {
        let network = state
            .settings
            .with(|settings| settings.as_ref().map(|settings| settings.network));
        if let Some(network) = network
            && last.flatten() != Some(network)
        {
            state.network.set(network);
        }
        network
    });
    // A change of network keeps the shell on that network: the selected node becomes the
    // first node there, or the overview shows its empty state.
    Effect::new(move |_| {
        let scope = state.network.get();
        let nodes = state.nodes.get_untracked();
        let selected = state.selected.get_untracked();
        let networks: Vec<Network> = nodes
            .iter()
            .map(|node| node.info.with_untracked(|info| info.network))
            .collect();
        let current = nodes.iter().position(|node| *node == selected);
        match pick_in_scope(current, &networks, scope) {
            Some(index) => {
                if nodes[index] != selected {
                    state.selected.set(nodes[index]);
                }
            }
            None => {
                if state.tab.get_untracked().is_node_tab() && nodes.len() > 1 {
                    state.tab.set(Tab::Overview);
                }
            }
        }
    });
    // And the other way round: a node page shown any other way (a node just added, the
    // backend node after its network changed, a shortcut to a node page while the network
    // shown has no nodes) brings its node's network along, so a node never shows under the
    // other network's tab and band. The network is read untracked: a network the user picks
    // moves the selection (above) and is not undone here.
    Effect::new(move |_| {
        let network = state.node().info.with(|info| info.network);
        if state.tab.get().is_node_tab() && state.network.get_untracked() != network {
            state.network.set(network);
        }
    });

    // The backend's node is named after the network and folder in the settings.
    Effect::new(move |_| {
        let named = state.settings.with(|settings| {
            settings.as_ref().map(|settings| {
                node_state::primary_info(settings.network, settings.data_dir.as_deref())
            })
        });
        if let Some(info) = named {
            node.info.set(info);
        }
    });

    for extra in extra {
        subscribe_extra(state, extra);
    }

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
    // Several nodes: a sidebar of nodes replaces the tab bar (docs/MULTI-NODE.md).
    let herd = Memo::new(move |_| state.nodes.with(|nodes| nodes.len() > 1));
    let adding = RwSignal::new(false);
    // The launcher's own pages are tab panels of the tab bar; with several nodes the sidebar
    // opens them instead, so they are named regions without a tab.
    let panel_role = move || if herd.get() { "region" } else { "tabpanel" };
    let labelled_by = move |tab: Tab| move || (!herd.get()).then(|| tab.button_id());
    let labelled = move |tab: Tab| move || herd.get().then(|| tab.label());
    // What had focus before the add-node dialog opened: the app turns inert under it, which
    // takes focus away, so the opener notes it first.
    let return_focus = StoredValue::new_local(None::<web_sys::HtmlElement>);
    let open_add = Callback::new(move |()| {
        let active = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.active_element())
            .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok());
        return_focus.set_value(active);
        adding.set(true);
    });

    view! {
        <div
            class="app"
            class:app-herd=herd
            class:scope-testnet=move || shown_network(state, herd.get()) == Network::Testnet
            inert=move || first_run_open.get() || adding.get()
        >
            {move || herd.get().then(|| view! { <Sidebar on_add=open_add /> })}
            <div class="app-body">
            <header class="app-header">
                {move || if herd.get() { view! { <HerdHeader /> }.into_any() } else { view! { <Nav /> }.into_any() }}
                <Show when=move || !herd.get() || state.tab.get().is_node_tab()>
                    <StatusStrip />
                </Show>
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
                    class="page page-overview"
                    role="region"
                    id=Tab::Overview.page_id()
                    aria-label="Overview"
                    hidden=hidden(Tab::Overview)
                >
                    <Overview on_add=open_add />
                </section>
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
                    role=panel_role
                    id=Tab::Grafana.page_id()
                    aria-labelledby=labelled_by(Tab::Grafana)
                    aria-label=labelled(Tab::Grafana)
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
                    role=panel_role
                    id=Tab::Settings.page_id()
                    aria-labelledby=labelled_by(Tab::Settings)
                    aria-label=labelled(Tab::Settings)
                    hidden=hidden(Tab::Settings)
                >
                    <SettingsPage />
                </section>
            </main>
            </div>
        </div>
        <FirstRun open=first_run_open />
        <AddNode open=adding return_focus />
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_change_of_network_keeps_or_moves_the_selection() {
        use Network::{Mainnet, Testnet};
        let networks = [Mainnet, Mainnet, Testnet, Testnet];
        // The selected node stays while it is on the network.
        assert_eq!(pick_in_scope(Some(3), &networks, Testnet), Some(3));
        // Otherwise the first node on the network is shown.
        assert_eq!(pick_in_scope(Some(0), &networks, Testnet), Some(2));
        assert_eq!(pick_in_scope(None, &networks, Mainnet), Some(0));
        // A network without nodes has nothing to show.
        assert_eq!(pick_in_scope(Some(1), &[Mainnet, Mainnet], Testnet), None);
    }

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
