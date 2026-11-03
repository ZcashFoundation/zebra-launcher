//! Pieces the Explorer's pages share: a hash with Copy, links between pages, pool chips, and
//! what a page shows while it loads or after it failed.

use std::time::Duration;

use launcher_types::explorer::{ExplorerError, ExplorerErrorKind, ExplorerRoute, Hash32, Pool};
use leptos::{prelude::*, task::spawn_local};

use super::state::NodeExplorer;
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
