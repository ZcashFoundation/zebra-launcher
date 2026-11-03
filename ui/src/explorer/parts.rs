//! Pieces the Explorer's pages share: a hash with Copy, links between pages, amounts, pool
//! chips, kind badges and the mark of a hidden amount.

use std::time::Duration;

use launcher_types::explorer::{
    ExplorerError, ExplorerErrorKind, ExplorerRoute, Hash32, Pool, TxKind,
};
use leptos::{prelude::*, task::spawn_local};

use super::{state::NodeExplorer, text};
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

/// A link to another page of the same node's Explorer. No URL is opened: it is a button.
#[component]
pub fn RouteLink(nav: NodeExplorer, route: ExplorerRoute, children: Children) -> impl IntoView {
    view! {
        <button type="button" class="link ex-link" on:click=move |_| nav.go(route.clone())>
            {children()}
        </button>
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

/// A failed answer: what went wrong, in the explorer's words. A node that does not run is the
/// shell's banner to tell, so the page says nothing more.
#[component]
pub fn Failure(error: ExplorerError) -> impl IntoView {
    (error.kind != ExplorerErrorKind::NodeNotRunning).then(|| {
        view! {
            <div class="banner banner-warn ex-failure" role="alert">
                <span>{error.message}</span>
            </div>
        }
    })
}

/// The placeholder a page shows before its first answer.
#[component]
pub fn Loading() -> impl IntoView {
    view! { <p class="muted ex-loading">"Reading from your node…"</p> }
}
