//! The Explorer tab: a block explorer over the selected node's own RPC (docs/EXPLORER.md).
//! Nothing it shows comes from anywhere else, and nothing searched leaves the computer.
//!
//! The shell holds back and forward, the search box, the source line (which node, which
//! network, which tip, the unit and time toggles and Show RPC) and one banner; under it, the
//! page of the node's current route, and the hover card of the link the pointer or focus rests
//! on ([`cards`]). Each node keeps its own place ([`state::Explorers`]). The tab tells the
//! node's explorer when it is on screen (`Interest`), so the explorer follows the tip only
//! then.

mod block;
mod cards;
mod chain;
mod history;
mod parts;
mod prefs;
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
    charts::widgets::focus_sibling,
    components::{icons::info_icon, nav::is_mac},
    format::{controls, relative, thousands},
};
use block::BlockPage;
use cards::{HoverCard, HoverCards};
use chain::ChainPage;
use prefs::Prefs;
use search::{SearchBox, SearchPage};
use state::{Explorers, NodeExplorer};
use text::TimeFormat;
use tx::TxPage;

/// How long a reorganisation stays in the banner.
const REORG_SHOWN_FOR: Duration = Duration::from_secs(10 * 60);

#[component]
pub fn Explorer() -> impl IntoView {
    let app = AppState::get();
    let explorers = Explorers::new();
    Prefs::provide();
    let node_id = Memo::new(move |_| app.node().info.with(|info| info.id.clone()));
    let network = Signal::derive(move || app.node().info.with(|info| info.network));
    let nav = Signal::derive(move || explorers.of(&node_id.get()));
    let cards = HoverCards::provide(node_id.into());
    // A page that changes takes its links' card with it.
    Effect::new(move |_| {
        nav.get().route();
        cards.leave();
    });
    let tip = Signal::derive(move || {
        nav.get().live.with(|live| {
            live.as_ref()
                .and_then(|live| live.tip.map(|tip| tip.height))
        })
    });

    // The explorers' own news, for every node; a node shown for the first time asks for it,
    // and keeps the reason when its explorer would not answer.
    spawn_local(async move {
        let result = api::on_explorer_live(
            move |event| {
                let explorer = explorers.of(&event.node);
                explorer.unavailable.set(None);
                explorer.live.set(Some(event.live));
            },
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
                match api::explorer(&node, ExplorerRequest::Status).await {
                    Ok(reply) => {
                        if let ExplorerResponse::Status(live) = reply.response {
                            explorer.unavailable.try_set(None);
                            explorer.live.try_update(|current| {
                                current.get_or_insert(live);
                            });
                        }
                    }
                    Err(error) => {
                        explorer.unavailable.try_set(Some(error));
                    }
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

    // Ctrl+K (Cmd+K on macOS) opens the selected node's Explorer with the search focused, and
    // `/` does in the Explorer. Neither is taken from a text field, where the key may be the
    // field's own (Ctrl+K deletes to the end of the line in macOS and Emacs-style fields). A
    // node without an Explorer has no search to open, so the key is left alone there too.
    let search_input = NodeRef::<html::Input>::new();
    let mac = is_mac();
    let keys = window_event_listener(ev::keydown, move |event| {
        if event.default_prevented() || in_text_field(&event) {
            return;
        }
        let key = event.key();
        let modifiers = Modifiers {
            ctrl: event.ctrl_key(),
            meta: event.meta_key(),
            alt: event.alt_key(),
        };
        let wanted = search_shortcut(&key, modifiers, mac)
            || key == "/"
                && !(modifiers.ctrl || modifiers.meta)
                && app.tab.get_untracked() == Tab::Explorer;
        let has_explorer = app
            .node()
            .info
            .with_untracked(|info| info.kind.has_explorer());
        if wanted && has_explorer {
            event.prevent_default();
            app.tab.set(Tab::Explorer);
            // From another tab the Explorer is still hidden here, and a hidden field takes no
            // focus: focus it once the tab shows.
            request_animation_frame(move || {
                if let Some(input) = search_input.get_untracked() {
                    let _ = input.focus();
                    input.select();
                }
            });
        }
    });
    on_cleanup(move || keys.remove());

    // A link followed inside a page goes with the page it was on, and focus with it. It moves
    // to the new page's heading (or the page, while it loads), so the keyboard carries on
    // there and not from the top of the window. Focus kept elsewhere, in the search box or on
    // Back, stays put.
    let page_ref = NodeRef::<html::Div>::new();
    Effect::new(move |previous: Option<(NodeId, ExplorerRoute)>| {
        let next = (node_id.get(), nav.get().route());
        if previous
            .as_ref()
            .is_some_and(|(node, route)| *node == next.0 && *route != next.1)
            && app.tab.get_untracked() == Tab::Explorer
        {
            request_animation_frame(move || {
                if let Some(page) = page_ref.get_untracked()
                    && focus_lost()
                {
                    focus_page(&page);
                }
            });
        }
        next
    });

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
            <SourceLine nav network tip />
            <Banner nav />
            <div class="ex-page" aria-live="off" tabindex="-1" node_ref=page_ref>{page}</div>
            <HoverCard />
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

/// The modifier keys held with a key.
#[derive(Clone, Copy, Debug, Default)]
struct Modifiers {
    ctrl: bool,
    meta: bool,
    alt: bool,
}

/// Whether `key` with `modifiers` is the search shortcut: Cmd+K on macOS, Ctrl+K elsewhere, the
/// modifier the tab shortcuts' labels name (`shortcut_label`). On macOS Ctrl+K is the text
/// fields' delete to the end of the line, and elsewhere the Windows or Super key belongs to
/// the system.
fn search_shortcut(key: &str, modifiers: Modifiers, mac: bool) -> bool {
    let held = if mac {
        modifiers.meta && !modifiers.ctrl
    } else {
        modifiers.ctrl && !modifiers.meta
    };
    held && !modifiers.alt && key.eq_ignore_ascii_case("k")
}

/// Whether focus is nowhere: on the document's body, as after the focused element left the
/// page.
fn focus_lost() -> bool {
    web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.active_element())
        .is_none_or(|element| element.tag_name() == "BODY")
}

/// Focuses the page's first heading, made focusable for this. A page still loading has none
/// yet: the page itself takes focus, and hands it to the heading once the heading is drawn,
/// however long the page reads, unless focus has moved on meanwhile.
fn focus_page(page: &web_sys::HtmlElement) {
    if !focus_heading(page) {
        let _ = page.focus();
        focus_heading_once_drawn(page.clone());
    }
}

fn focus_heading_once_drawn(page: web_sys::HtmlElement) {
    let still_on_page = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.active_element())
        .is_some_and(|active| active.is_same_node(Some(&page)));
    if !still_on_page || focus_heading(&page) {
        return;
    }
    // A page shows `parts::Loading` until its answer comes. One that answered without a
    // heading, as a failure does with its alert, keeps focus on the page.
    if page.query_selector(".ex-loading").ok().flatten().is_some() {
        request_animation_frame(move || focus_heading_once_drawn(page));
    }
}

/// Focuses the page's first heading, if it has one.
fn focus_heading(page: &web_sys::HtmlElement) -> bool {
    let Some(heading) = page
        .query_selector("h2")
        .ok()
        .flatten()
        .and_then(|heading| heading.dyn_into::<web_sys::HtmlElement>().ok())
    else {
        return false;
    };
    let _ = heading.set_attribute("tabindex", "-1");
    let _ = heading.focus();
    true
}

/// Whether a key went to a field that takes text.
fn in_text_field(event: &web_sys::KeyboardEvent) -> bool {
    let Some(element) = event
        .target()
        .and_then(|target| target.dyn_into::<web_sys::HtmlElement>().ok())
    else {
        return false;
    };
    takes_text(
        &element.tag_name(),
        element.get_attribute("type").as_deref(),
    ) || element.is_content_editable()
}

/// Whether an element named `tag`, with the `type` attribute `kind`, takes text: a text area,
/// a select, or an input that is typed into. A checkbox, a radio button, a slider, a colour or
/// file picker and a button do not.
fn takes_text(tag: &str, kind: Option<&str>) -> bool {
    match tag {
        "TEXTAREA" | "SELECT" => true,
        "INPUT" => !kind.is_some_and(|kind| {
            matches!(
                kind.to_ascii_lowercase().as_str(),
                "checkbox"
                    | "radio"
                    | "range"
                    | "button"
                    | "submit"
                    | "reset"
                    | "color"
                    | "file"
                    | "image"
            )
        }),
        _ => false,
    }
}

/// Which node, network and tip the page reads, and the promise that it all stays here. While
/// the explorer does not read this node (another node, or a zebrad still on the other
/// network), the zebrad's version and address are left out: they are not this page's.
#[component]
fn SourceLine(
    nav: Signal<NodeExplorer>,
    network: Signal<Network>,
    tip: Signal<Option<u32>>,
) -> impl IntoView {
    let app = AppState::get();
    let parts = move || {
        let node = app.node();
        let mut parts = vec![node.info.with(|info| info.name.clone())];
        let nav = nav.get();
        let elsewhere = nav.live.with(|live| {
            live.as_ref()
                .is_some_and(|live| matches!(live.state, LiveState::OtherNetwork { .. }))
        });
        if nav.unavailable.with(Option::is_some) || elsewhere {
            return parts.join(" · ");
        }
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
            // How old the tip block is, by its time; the clock's tick keeps it current.
            if let Some(time) = nav
                .live
                .with(|live| live.as_ref().and_then(|live| live.tip_time))
            {
                parts.push(relative(
                    UNIX_EPOCH + Duration::from_secs(time),
                    app.now.get(),
                ));
            }
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
            <Toggles network />
            <ShowRpc />
            <span class="ex-promise muted">
                {info_icon()}
                "Everything here comes from your own node. Nothing you search leaves this computer."
            </span>
        </div>
    }
}

/// The unit and time toggles: amounts in the network's coin (TAZ in the warning tone, as
/// Testnet is everywhere) or in zatoshis; times local, in UTC or as Unix seconds. Each is a
/// radio group, as the dashboard's range control is: one Tab stop, the arrow keys choose. The
/// choices are remembered ([`Prefs`]).
#[component]
fn Toggles(network: Signal<Network>) -> impl IntoView {
    let prefs = Prefs::get();
    let checked = |on: bool| if on { "true" } else { "false" };
    let tabindex = |on: bool| if on { "0" } else { "-1" };
    // Two choices: either arrow picks the other.
    let unit_keys = move |ev: web_sys::KeyboardEvent| {
        if arrow_step(&ev).is_some() {
            prefs.set_zat(!prefs.zat.get_untracked());
        }
    };
    view! {
        <span class="ex-toggle" role="radiogroup" aria-label="Amounts in">
            <button
                type="button"
                role="radio"
                class:ex-unit-test=move || network.get() == Network::Testnet
                aria-checked=move || checked(!prefs.zat.get())
                tabindex=move || tabindex(!prefs.zat.get())
                on:click=move |_| prefs.set_zat(false)
                on:keydown=unit_keys
            >
                {move || text::unit(network.get())}
            </button>
            <button
                type="button"
                role="radio"
                title="Zatoshis: 100,000,000 to the coin"
                aria-checked=move || checked(prefs.zat.get())
                tabindex=move || tabindex(prefs.zat.get())
                on:click=move |_| prefs.set_zat(true)
                on:keydown=unit_keys
            >
                "zat"
            </button>
        </span>
        <span class="ex-toggle" role="radiogroup" aria-label="Times in">
            {TimeFormat::ALL
                .into_iter()
                .map(|format| view! {
                    <button
                        type="button"
                        role="radio"
                        aria-checked=move || checked(prefs.time.get() == format)
                        tabindex=move || tabindex(prefs.time.get() == format)
                        on:click=move |_| prefs.set_time(format)
                        on:keydown=move |ev: web_sys::KeyboardEvent| {
                            if let Some(by) = arrow_step(&ev) {
                                prefs.set_time(format.step(by));
                            }
                        }
                    >
                        {format.label()}
                    </button>
                })
                .collect_view()}
        </span>
    }
}

/// Show RPC: a toggle that makes each page list the JSON-RPC requests its answer took. Kept
/// with the other choices ([`Prefs`]).
#[component]
fn ShowRpc() -> impl IntoView {
    let prefs = Prefs::get();
    view! {
        <button
            type="button"
            class="ex-rpc-toggle"
            aria-pressed=move || if prefs.rpc.get() { "true" } else { "false" }
            title="List the JSON-RPC requests each page made to your node"
            on:click=move |_| prefs.set_rpc(!prefs.rpc.get_untracked())
        >
            "Show RPC"
        </button>
    }
}

/// A radio group's arrow key: the step it moves the choice by, with focus moved to that radio
/// and the key's default (scrolling) prevented. `None` for other keys.
fn arrow_step(ev: &web_sys::KeyboardEvent) -> Option<i32> {
    let by = match ev.key().as_str() {
        "ArrowRight" | "ArrowDown" => 1,
        "ArrowLeft" | "ArrowUp" => -1,
        _ => return None,
    };
    ev.prevent_default();
    focus_sibling(ev, by);
    Some(by)
}

/// One banner, the most important first: a node the explorer does not serve, one that does not
/// run, one that syncs, a recent reorganisation. Start shows where the status strip offers it
/// too.
#[component]
fn Banner(nav: Signal<NodeExplorer>) -> impl IntoView {
    let app = AppState::get();
    let prefs = Prefs::get();
    let started = RwSignal::new(None::<String>);
    let starting = RwSignal::new(false);
    let banner = move || {
        let node = app.node();
        // The explorer said why it would not answer (it serves one node only, say): starting
        // the node would not change that, so that reason is the one banner in every phase.
        if let Some(error) = nav.get().unavailable.get() {
            return view! {
                <div class="banner banner-warn ex-banner" role="status">
                    <span>{error.message}</span>
                </div>
            }
            .into_any();
        }
        let phase = node.status.with(|status| status.phase.clone());
        let live: Option<ExplorerLive> = nav.get().live.get();
        if let Some(words) = text::not_running_text(&phase) {
            let start = move |_| {
                starting.set(true);
                spawn_local(async move {
                    let result = node.start().await;
                    starting.try_set(false);
                    started.try_set(
                        result
                            .err()
                            .map(|error| format!("Could not start: {error}")),
                    );
                });
            };
            let tone = match phase {
                Phase::Starting | Phase::Backoff { .. } => "banner banner-update ex-banner",
                _ => "banner banner-warn ex-banner",
            };
            return view! {
                <div class=tone role="status">
                    <span>{words}</span>
                    {controls(&phase).start.then(|| view! {
                        <span class="buttons">
                            <button type="button" disabled=move || starting.get() on:click=start>
                                "Start"
                            </button>
                            {move || started.get()}
                        </span>
                    })}
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
            Some(LiveState::OtherNetwork { network }) => {
                let ours = node.info.with(|info| info.network);
                view! {
                    <div class="banner banner-warn ex-banner" role="status">
                        <span>{text::other_network_text(network, ours)}</span>
                    </div>
                }
                .into_any()
            }
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
                                {text::reorg_text(&reorg, &text::time_text(reorg.at, prefs.time.get()))}
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

#[cfg(test)]
mod tests {
    use super::{Modifiers, search_shortcut, takes_text};

    #[test]
    fn only_fields_typed_into_keep_the_search_shortcut() {
        for kind in [
            None,
            Some("text"),
            Some("search"),
            Some("number"),
            Some("Password"),
        ] {
            assert!(takes_text("INPUT", kind), "{kind:?}");
        }
        for kind in [
            "checkbox", "radio", "RANGE", "button", "submit", "color", "file",
        ] {
            assert!(!takes_text("INPUT", Some(kind)), "{kind}");
        }
        assert!(takes_text("TEXTAREA", None));
        assert!(takes_text("SELECT", None));
        assert!(!takes_text("BUTTON", Some("button")));
        assert!(!takes_text("DIV", None));
    }

    #[test]
    fn the_search_shortcut_is_the_platforms() {
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        let cmd = Modifiers {
            meta: true,
            ..Modifiers::default()
        };
        assert!(search_shortcut("k", ctrl, false));
        assert!(search_shortcut("K", ctrl, false), "with Shift or Caps Lock");
        assert!(
            !search_shortcut("k", cmd, false),
            "the Windows or Super key"
        );
        assert!(search_shortcut("k", cmd, true));
        assert!(
            !search_shortcut("k", ctrl, true),
            "macOS fields' delete to the line's end"
        );
        let ctrl_alt = Modifiers { alt: true, ..ctrl };
        assert!(!search_shortcut("k", ctrl_alt, false));
        assert!(!search_shortcut("j", ctrl, false));
        assert!(!search_shortcut("k", Modifiers::default(), false));
    }
}
