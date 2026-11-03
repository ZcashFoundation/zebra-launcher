//! Pieces the Explorer's pages share: links between pages, and what a page shows while it
//! loads or after it failed.

use launcher_types::explorer::{ExplorerError, ExplorerErrorKind, ExplorerRoute};
use leptos::prelude::*;

use super::state::NodeExplorer;

/// A link to another page of the same node's Explorer. No URL is opened: it is a button.
#[component]
pub fn RouteLink(nav: NodeExplorer, route: ExplorerRoute, children: Children) -> impl IntoView {
    view! {
        <button type="button" class="link ex-link" on:click=move |_| nav.go(route.clone())>
            {children()}
        </button>
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
