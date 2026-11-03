//! The search box and the search results page. Every keystroke is classified in the page (no
//! call): the hint says what the query is, and a key or a recovery phrase is cleared from the
//! field before it is kept anywhere. Enter goes straight to a height; a hash goes to the
//! results page, which asks the node and replaces itself with the page it found.

use explorer_model::classify::{Classified, classify_query};
use launcher_types::{
    Network, NodeId,
    explorer::{
        BlockId, ExplorerRequest, ExplorerResponse, ExplorerRoute, NotFoundHint, Resolution,
        SecretKind,
    },
};
use leptos::{ev, html, prelude::*};

use super::{
    parts::{Failure, Loading, RouteLink},
    state::{Loaded, NodeExplorer, load},
    text,
};
use crate::{
    app::{AppState, Tab},
    format::short_hash,
};

/// The search box of the shell.
#[component]
pub fn SearchBox(
    nav: Signal<NodeExplorer>,
    network: Signal<Network>,
    tip: Signal<Option<u32>>,
    input: NodeRef<html::Input>,
) -> impl IntoView {
    let query = RwSignal::new(String::new());
    let refused = RwSignal::new(None::<SecretKind>);
    let classified = Memo::new(move |_| classify_query(&query.get(), network.get()));
    let on_input = move |event: ev::Event| {
        let value = event_target_value(&event);
        // A secret never reaches a signal: the field is cleared and only the refusal stays.
        if let Classified::Secret(kind) = classify_query(&value, network.get_untracked()) {
            if let Some(field) = input.get_untracked() {
                field.set_value("");
            }
            query.set(String::new());
            refused.set(Some(kind));
            return;
        }
        refused.set(None);
        query.set(value);
    };
    let submit = move |event: ev::SubmitEvent| {
        event.prevent_default();
        let text = query.get_untracked();
        let nav = nav.get_untracked();
        let route = match classify_query(&text, network.get_untracked()) {
            Classified::Height(height) => Some(ExplorerRoute::Block {
                id: BlockId::Height(height),
            }),
            Classified::BelowTip(back) => Some(match tip.get_untracked() {
                Some(tip) => ExplorerRoute::Block {
                    id: BlockId::Height(tip.saturating_sub(back)),
                },
                None => ExplorerRoute::Search {
                    query: text.trim().to_owned(),
                },
            }),
            Classified::Hash(_) | Classified::Outpoint(_) => Some(ExplorerRoute::Search {
                query: text.trim().to_owned(),
            }),
            Classified::Route(route) => Some(route),
            // The hint says why there is nowhere to go.
            Classified::Secret(_)
            | Classified::Later(_)
            | Classified::Empty
            | Classified::Unrecognised => None,
        };
        if let Some(route) = route {
            nav.go(route);
            query.set(String::new());
            if let Some(field) = input.get_untracked() {
                field.set_value("");
            }
        }
    };
    let hint = move || match refused.get() {
        Some(kind) => text::refusal(kind).to_owned(),
        None => text::hint(&classified.get(), tip.get(), network.get()),
    };
    view! {
        <form class="ex-search" role="search" on:submit=submit>
            <label class="visually-hidden" for="ex-search-input">
                "Search your node's chain"
            </label>
            <input
                id="ex-search-input"
                type="search"
                node_ref=input
                placeholder="Height, block hash or transaction id"
                autocomplete="off"
                spellcheck="false"
                aria-describedby="ex-search-hint"
                on:input=on_input
            />
            <span class="ex-keycap" aria-hidden="true">"/"</span>
            <p
                id="ex-search-hint"
                class="ex-hint"
                class:ex-hint-refused=move || refused.get().is_some()
                aria-live="polite"
            >
                {hint}
            </p>
        </form>
    }
}

/// The results of a search the box could not settle by itself: a hash, which may be a block,
/// a transaction or both.
#[component]
pub fn SearchPage(node: NodeId, nav: NodeExplorer, query: String) -> impl IntoView {
    let app = AppState::get();
    let asked = query.clone();
    let answer = load(node, nav, move || {
        Some((
            ExplorerRequest::Resolve {
                query: asked.clone(),
            },
            0,
        ))
    });
    // One page found: it takes this one's place, so going back skips the search.
    Effect::new(move |_| {
        if let Loaded::Ready(reply) = answer.value.get()
            && let ExplorerResponse::Resolve(Resolution::Go { route }) = reply.response
        {
            nav.replace(route);
        }
    });
    let shown_query = short_hash(&query);
    view! {
        <section class="ex-section" aria-labelledby="ex-search-title">
            <h2 id="ex-search-title">"Search"</h2>
            <p class="muted"><code>{shown_query}</code></p>
            {move || match answer.value.get() {
                Loaded::Loading => view! { <Loading /> }.into_any(),
                Loaded::Failed(error) => view! { <Failure error /> }.into_any(),
                Loaded::Ready(reply) => match reply.response {
                    ExplorerResponse::Resolve(Resolution::Choose { routes }) => view! {
                        <p>"This hash is both a block and a transaction on your node:"</p>
                        <ul class="ex-choices">
                            {routes
                                .into_iter()
                                .map(|route| {
                                    let label = match &route {
                                        ExplorerRoute::Block { .. } => "The block",
                                        ExplorerRoute::Tx { .. } => "The transaction",
                                        _ => "This page",
                                    };
                                    view! {
                                        <li><RouteLink nav route>{label}</RouteLink></li>
                                    }
                                })
                                .collect_view()}
                        </ul>
                    }
                    .into_any(),
                    ExplorerResponse::Resolve(Resolution::Explain { hint }) => {
                        let to_dashboard = matches!(hint, NotFoundHint::PastNodeTip { .. });
                        view! {
                            <p>{text::not_found_text(&hint)}</p>
                            {to_dashboard.then(|| view! {
                                <button type="button" on:click=move |_| app.tab.set(Tab::Dashboard)>
                                    "See the sync on the Dashboard"
                                </button>
                            })}
                        }
                        .into_any()
                    }
                    // A single page replaces this one at once.
                    _ => view! { <Loading /> }.into_any(),
                },
            }}
        </section>
    }
}
