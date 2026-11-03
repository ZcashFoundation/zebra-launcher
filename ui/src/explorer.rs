//! The Explorer tab: a block explorer over the selected node's own RPC (docs/EXPLORER.md).
//! Nothing it shows comes from anywhere else, and nothing searched leaves the computer.
//!
//! The shell holds back and forward, the search box, the source line (which node, which
//! network, which tip) and one banner; under it, the page of the node's current route. Each
//! node keeps its own place ([`state::Explorers`]). The tab tells the node's explorer when it
//! is on screen (`Interest`), so the explorer follows the tip only then.

mod block;
mod chain;
mod history;
mod parts;
mod search;
mod state;
pub mod text;
mod tx;

use std::time::{Duration, UNIX_EPOCH};

use launcher_types::{
    Network, NodeId, Phase,
    explorer::{
        ExplorerLive, ExplorerRequest, ExplorerResponse, ExplorerRoute, LiveState, PageKind,
    },
};
use leptos::{ev, html, prelude::*, task::spawn_local};
use wasm_bindgen::JsCast;

use crate::{
    api,
    app::{AppState, Tab},
    components::icons::info_icon,
    format::{local_datetime_short, thousands},
};
use block::BlockPage;
use chain::ChainPage;
use search::{SearchBox, SearchPage};
use state::{Explorers, NodeExplorer};
use tx::TxPage;

/// How long a reorganisation stays in the banner.
const REORG_SHOWN_FOR: Duration = Duration::from_secs(10 * 60);

#[component]
pub fn Explorer() -> impl IntoView {
    let app = AppState::get();
    let explorers = Explorers::new();
    let node_id = Memo::new(move |_| app.node().info.with(|info| info.id.clone()));
    let network = Signal::derive(move || app.node().info.with(|info| info.network));
    let nav = Signal::derive(move || explorers.of(&node_id.get()));
    let tip = Signal::derive(move || {
        nav.get().live.with(|live| {
            live.as_ref()
                .and_then(|live| live.tip.map(|tip| tip.height))
        })
    });

    // The explorers' own news, for every node; a node shown for the first time asks for it.
    spawn_local(async move {
        let result = api::on_explorer_live(
            move |event| explorers.of(&event.node).live.set(Some(event.live)),
            move |message| app.backend_error.set(Some(message)),
        )
        .await;
        if let Err(error) = result {
            app.backend_error
                .set(Some(format!("Listening for the explorer: {error}")));
        }
    });
    Effect::new(move |_| {
        let node = node_id.get();
        let explorer = explorers.of(&node);
        if explorer.live.with_untracked(Option::is_none) {
            spawn_local(async move {
                if let Ok(reply) = api::explorer(&node, ExplorerRequest::Status).await
                    && let ExplorerResponse::Status(live) = reply.response
                {
                    explorer.live.try_update(|current| {
                        current.get_or_insert(live);
                    });
                }
            });
        }
    });

    // On screen: the tab is shown and so is the document. The node's explorer hears of every
    // change, and the node left behind hears that it is no longer shown.
    let document_visible = RwSignal::new(document_visible());
    let visibility = window_event_listener(ev::visibilitychange, move |_| {
        document_visible.set(document_visible_now());
    });
    on_cleanup(move || visibility.remove());
    Effect::new(move |previous: Option<(NodeId, bool, PageKind)>| {
        let shown = app.tab.get() == Tab::Explorer && document_visible.get();
        let node = node_id.get();
        let page = nav.get().route().page();
        let next = (node.clone(), shown, page);
        if previous.as_ref() != Some(&next) {
            if let Some((old, true, old_page)) = &previous
                && *old != node
            {
                send_interest(old.clone(), false, *old_page);
            }
            let was_shown = previous
                .as_ref()
                .is_some_and(|(old, shown, _)| *old == node && *shown);
            if shown || was_shown {
                send_interest(node, shown, page);
            }
        }
        next
    });

    // Ctrl (or Cmd) + K anywhere opens the Explorer with the search focused; `/` does in the
    // Explorer, outside text fields.
    let search_input = NodeRef::<html::Input>::new();
    let keys = window_event_listener(ev::keydown, move |event| {
        if event.default_prevented() {
            return;
        }
        let command = (event.ctrl_key() || event.meta_key()) && !event.alt_key();
        let slash = event.key() == "/"
            && !command
            && app.tab.get_untracked() == Tab::Explorer
            && !in_text_field(&event);
        if (command && event.key().eq_ignore_ascii_case("k")) || slash {
            event.prevent_default();
            app.tab.set(Tab::Explorer);
            if let Some(input) = search_input.get_untracked() {
                let _ = input.focus();
            }
        }
    });
    on_cleanup(move || keys.remove());

    // Nothing is read before the tab is first opened: the pages are mounted with the app.
    let opened = RwSignal::new(false);
    Effect::new(move |_| {
        if app.tab.get() == Tab::Explorer {
            opened.set(true);
        }
    });
    let page = move || {
        if !opened.get() {
            return ().into_any();
        }
        let node = node_id.get();
        let nav = nav.get();
        let network = network.get();
        match nav.route() {
            ExplorerRoute::Chain => view! { <ChainPage node nav network /> }.into_any(),
            ExplorerRoute::Block { id } => view! { <BlockPage node nav network id /> }.into_any(),
            ExplorerRoute::Tx { txid, out } => {
                view! { <TxPage node nav network txid out /> }.into_any()
            }
            ExplorerRoute::Search { query } => view! { <SearchPage node nav query /> }.into_any(),
        }
    };

    view! {
        <div class="explorer">
            <div class="ex-bar">
                <div class="ex-nav">
                    <button
                        type="button"
                        aria-label="Back"
                        title="Back"
                        disabled=move || !nav.get().history.with(|history| history.can_go_back())
                        on:click=move |_| nav.get_untracked().back()
                    >
                        "←"
                    </button>
                    <button
                        type="button"
                        aria-label="Forward"
                        title="Forward"
                        disabled=move || !nav.get().history.with(|history| history.can_go_forward())
                        on:click=move |_| nav.get_untracked().forward()
                    >
                        "→"
                    </button>
                    <button
                        type="button"
                        class="ex-home"
                        aria-current=move || (nav.get().route() == ExplorerRoute::Chain).then_some("page")
                        on:click=move |_| nav.get_untracked().go(ExplorerRoute::Chain)
                    >
                        "Chain"
                    </button>
                </div>
                <SearchBox nav network tip input=search_input />
            </div>
            <SourceLine network tip />
            <Banner nav />
            <div class="ex-page" aria-live="off">{page}</div>
        </div>
    }
}

fn send_interest(node: NodeId, visible: bool, page: PageKind) {
    spawn_local(async move {
        // Interest is advice: a failure is not worth a banner.
        let _ = api::explorer(&node, ExplorerRequest::Interest { visible, page }).await;
    });
}

fn document_visible_now() -> bool {
    web_sys::window()
        .and_then(|window| window.document())
        .is_none_or(|document| document.visibility_state() != web_sys::VisibilityState::Hidden)
}

fn document_visible() -> bool {
    document_visible_now()
}

/// Whether a key went to a field that takes text.
fn in_text_field(event: &web_sys::KeyboardEvent) -> bool {
    let Some(element) = event
        .target()
        .and_then(|target| target.dyn_into::<web_sys::HtmlElement>().ok())
    else {
        return false;
    };
    matches!(element.tag_name().as_str(), "INPUT" | "TEXTAREA" | "SELECT")
        || element.is_content_editable()
}

/// Which node, network and tip the page reads, and the promise that it all stays here.
#[component]
fn SourceLine(network: Signal<Network>, tip: Signal<Option<u32>>) -> impl IntoView {
    let app = AppState::get();
    let parts = move || {
        let node = app.node();
        let mut parts = vec![node.info.with(|info| info.name.clone())];
        if let Some(version) = node
            .stats
            .with(|stats| stats.as_ref().map(|stats| stats.zebrad_version.clone()))
            .filter(|version| !version.is_empty())
        {
            let program = node.info.with(|info| info.kind.program().unwrap_or("node"));
            parts.push(format!("{program} {version}"));
        }
        if let Some(addr) = node.status.with(|status| status.rpc_addr) {
            parts.push(addr.to_string());
        }
        if let Some(tip) = tip.get() {
            parts.push(format!("tip {}", thousands(u64::from(tip))));
        }
        parts.join(" · ")
    };
    let finality = move || {
        tip.get()
            .filter(|tip| *tip >= text::FINAL_DEPTH)
            .map(|tip| format!("final ≤ {}", thousands(u64::from(tip - text::FINAL_DEPTH))))
    };
    view! {
        <div class="ex-source">
            <span class="ex-source-text">{parts}</span>
            {move || finality().map(|chip| view! {
                <span class="chip" title="Zebra treats blocks 1,000 deep as final; a policy, not a consensus rule">
                    {chip}
                </span>
            })}
            <span
                class=move || if network.get() == Network::Testnet { "chip ex-unit ex-unit-test" } else { "chip ex-unit" }
                title="The unit amounts are shown in"
            >
                {move || text::unit(network.get())}
            </span>
            <span class="ex-promise muted">
                {info_icon()}
                "Everything here comes from your own node. Nothing you search leaves this computer."
            </span>
        </div>
    }
}

/// One banner, the most important first: a node that does not run, one that syncs, a recent
/// reorganisation.
#[component]
fn Banner(nav: Signal<NodeExplorer>) -> impl IntoView {
    let app = AppState::get();
    let started = RwSignal::new(None::<String>);
    let banner = move || {
        let node = app.node();
        let phase = node.status.with(|status| status.phase.clone());
        let live: Option<ExplorerLive> = nav.get().live.get();
        if phase != Phase::Running {
            let start = move |_| {
                spawn_local(async move {
                    if let Err(error) = node.start().await {
                        started.set(Some(format!("Could not start: {error}")));
                    }
                });
            };
            return view! {
                <div class="banner banner-warn ex-banner" role="status">
                    <span>"The node is not running, so the Explorer cannot read its chain."</span>
                    <span class="buttons">
                        <button type="button" on:click=start>"Start"</button>
                        {move || started.get()}
                    </span>
                </div>
            }
            .into_any();
        }
        match live.as_ref().map(|live| live.state.clone()) {
            None | Some(LiveState::NotRunning) => view! {
                <div class="banner banner-warn ex-banner" role="status">
                    <span>"Waiting for the node to open its RPC port."</span>
                </div>
            }
            .into_any(),
            Some(LiveState::Syncing { height, estimated }) => view! {
                <div class="banner banner-update ex-banner" role="status">
                    <span>
                        {format!(
                            "Your node has blocks up to {} of about {}. The Explorer shows what \
                             it has, and follows the tip once the node reaches it.",
                            thousands(u64::from(height)),
                            thousands(u64::from(estimated))
                        )}
                    </span>
                </div>
            }
            .into_any(),
            Some(LiveState::AtTip) => {
                let now = app.now.get();
                let recent = live.and_then(|live| live.reorg).filter(|reorg| {
                    now.duration_since(UNIX_EPOCH + Duration::from_secs(reorg.at))
                        .is_ok_and(|since| since < REORG_SHOWN_FOR)
                });
                match recent {
                    Some(reorg) => view! {
                        <div class="banner banner-warn ex-banner" role="status">
                            <span>
                                {format!(
                                    "{} block{} replaced at {}: the node switched to another \
                                     branch above {}.",
                                    reorg.replaced,
                                    if reorg.replaced == 1 { " was" } else { "s were" },
                                    local_datetime_short(UNIX_EPOCH + Duration::from_secs(reorg.at)),
                                    thousands(u64::from(reorg.fork_height))
                                )}
                            </span>
                        </div>
                    }
                    .into_any(),
                    None => ().into_any(),
                }
            }
        }
    };
    view! { {banner} }
}
