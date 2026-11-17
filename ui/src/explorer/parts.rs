//! Pieces the Explorer's pages share: a hash with Copy, links between pages (with hover cards),
//! amounts, pool chips, kind badges, the mark of a hidden amount, and Show RPC's list of the
//! reads an answer took.

use std::time::Duration;

use launcher_types::explorer::{
    CardTarget, ExplorerError, ExplorerErrorKind, ExplorerRoute, Hash32, Pool, ReadOutcome,
    SourceCall, TxKind,
};
use leptos::{prelude::*, task::spawn_local};

use super::{
    cards::{CARD_ID, hover_handlers},
    prefs::Prefs,
    state::NodeExplorer,
    text,
};
use crate::{clipboard::copy_to_clipboard, components::transient::Transient, format::short_hash};

/// How long "Copied" stays beside a Copy button.
const COPIED_FOR: Duration = Duration::from_secs(2);

/// 64 hex digits in groups of eight, so they can be read aloud and compared.
pub fn grouped(hex: &str) -> String {
    hex.as_bytes()
        .chunks(8)
        .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// A hash in monospace, shortened to its ends unless `full`, with a Copy button.
#[component]
pub fn HashText(hash: Hash32, #[prop(optional)] full: bool) -> impl IntoView {
    let hex = hash.to_hex();
    let shown = if full {
        grouped(&hex)
    } else {
        short_hash(&hex)
    };
    let copied = Transient::<Result<(), String>>::new();
    let to_copy = hex.clone();
    let copy = move |_| {
        let text = to_copy.clone();
        spawn_local(async move {
            match copy_to_clipboard(&text).await {
                Ok(()) => copied.show_for(Ok(()), COPIED_FOR),
                Err(error) => copied.hold(Err(format!("Could not copy: {error}"))),
            }
        });
    };
    view! {
        <span class="ex-hash">
            <code title=hex.clone()>{shown}</code>
            <button type="button" class="ex-copy" on:click=copy aria-label="Copy the hash">
                "Copy"
            </button>
            <span class="ex-copied muted" role="status">
                {move || {
                    copied
                        .get()
                        .map(|outcome| match outcome {
                            Ok(()) => "Copied".to_owned(),
                            Err(error) => error,
                        })
                }}
            </span>
        </span>
    }
}

/// A link to another page of the same node's Explorer. No URL is opened: it is a button. A
/// link to a block or a transaction has a hover card.
#[component]
pub fn RouteLink(nav: NodeExplorer, route: ExplorerRoute, children: Children) -> impl IntoView {
    let target = card_target(&route);
    let has_card = target.is_some();
    let target = StoredValue::new(target);
    let (enter, leave) = hover_handlers(move || target.get_value());
    view! {
        <button
            type="button"
            class="link ex-link"
            aria-describedby=has_card.then_some(CARD_ID)
            on:click=move |_| nav.go(route.clone())
            on:mouseenter=move |event| enter(event.into())
            on:focus=move |event| enter(event.into())
            on:mouseleave=move |event| leave(event.into())
            on:blur=move |event| leave(event.into())
        >
            {children()}
        </button>
    }
}

/// What a hover card on a link to `route` is about: a block or a transaction.
pub fn card_target(route: &ExplorerRoute) -> Option<CardTarget> {
    match route {
        ExplorerRoute::Block { id } => Some(CardTarget::Block { id: *id }),
        ExplorerRoute::Tx { txid, .. } => Some(CardTarget::Tx { txid: *txid }),
        ExplorerRoute::Chain | ExplorerRoute::Search { .. } => None,
    }
}

/// Show RPC: the reads an answer took, as the requests named them, while Show RPC is on.
/// `sources` is `None` while the answer is on its way. A `folded` list opens on request, under
/// a count of its reads. Only the count and the list follow `sources`, so an open list stays
/// open, and keeps focus, while reads are added to it.
#[component]
pub fn RpcCalls(
    #[prop(into)] sources: Signal<Option<Vec<SourceCall>>>,
    /// What the list is of.
    title: &'static str,
    #[prop(optional)] folded: bool,
) -> impl IntoView {
    let prefs = Prefs::get();
    let count = move || {
        sources.with(|sources| {
            sources.as_ref().map(|calls| {
                let cached = calls.iter().filter(|call| call.cached).count();
                format!(
                    "{} read{}, {cached} from memory",
                    calls.len(),
                    if calls.len() == 1 { "" } else { "s" }
                )
            })
        })
    };
    let body = move || match sources.get() {
        None => view! { <p class="muted">"Reading…"</p> }.into_any(),
        Some(calls) if calls.is_empty() => view! {
            <p class="muted">"No reads: the explorer had everything."</p>
        }
        .into_any(),
        Some(calls) => view! {
            <ol class="ex-rpc-calls">
                {calls
                    .into_iter()
                    .map(|call| {
                        let cost = text::source_cost(&call);
                        let failed =
                            matches!(call.outcome, ReadOutcome::Error | ReadOutcome::NoAnswer);
                        view! {
                            <li class:ex-rpc-cached=call.cached class:ex-rpc-failed=failed>
                                <code>{text::source_text(&call)}</code>
                                <span class="muted">{cost}</span>
                            </li>
                        }
                    })
                    .collect_view()}
            </ol>
        }
        .into_any(),
    };
    move || {
        prefs.rpc.get().then(|| {
            if folded {
                view! {
                    <details class="ex-rpc">
                        <summary>
                            <h3>{title}</h3>
                            <span class="muted">{count}</span>
                        </summary>
                        {body}
                    </details>
                }
                .into_any()
            } else {
                view! {
                    <div class="ex-rpc">
                        <h3>{title}</h3>
                        {body}
                    </div>
                }
                .into_any()
            }
        })
    }
}

/// A pool's name with its colour.
#[component]
pub fn PoolChip(pool: Pool) -> impl IntoView {
    view! {
        <span class=format!("ex-pool ex-pool-{}", pool.id())>
            <span class="ex-swatch" aria-hidden="true"></span>
            {pool.label()}
        </span>
    }
}

/// A transaction kind's badge.
#[component]
pub fn KindBadge(kind: TxKind) -> impl IntoView {
    let shielded = !matches!(kind, TxKind::Transparent | TxKind::Coinbase { .. });
    view! {
        <span class="ex-kind" class:ex-kind-shielded=shielded>
            {text::kind_label(&kind)}
        </span>
    }
}

/// An amount the chain hides: a lock and "hidden", never a number.
#[component]
pub fn Hidden() -> impl IntoView {
    view! {
        <span class="ex-hidden" title="Hidden by design: only the owner's keys can read it">
            <svg class="icon" viewBox="0 0 16 16" aria-hidden="true">
                <path class="icon-frame" d="M4 7.5h8v6H4zM5.5 7.5V5a2.5 2.5 0 0 1 5 0v2.5" />
            </svg>
            "hidden"
        </span>
    }
}

/// A failed answer: what went wrong, in the explorer's words. A node that does not run, or one
/// the explorer does not serve, is the shell's banner to tell, so the page says nothing more.
#[component]
pub fn Failure(error: ExplorerError) -> impl IntoView {
    let shell_tells = matches!(
        error.kind,
        ExplorerErrorKind::NodeNotRunning | ExplorerErrorKind::UnknownNode
    );
    (!shell_tells).then(|| {
        view! {
            <div class="banner banner-warn ex-failure" role="alert">
                <span>{error.message}</span>
            </div>
        }
    })
}

/// The placeholder a page shows before its first answer. While it shows, the shell waits for
/// the page's heading to hand focus to (`focus_page` in `explorer.rs`).
#[component]
pub fn Loading() -> impl IntoView {
    view! { <p class="muted ex-loading">"Reading from your node…"</p> }
}
