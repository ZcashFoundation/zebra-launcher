//! The banner for a zebrad left behind by an earlier launcher run of the selected node, with a
//! button that terminates it and starts the node.

use leptos::{prelude::*, task::spawn_local};

use crate::{api, app::AppState};

#[component]
pub fn StaleBanner() -> impl IntoView {
    let state = AppState::get();
    let busy = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);

    let kill_and_start = move |_| {
        busy.set(true);
        error.set(None);
        spawn_local(async move {
            let node = state.node_untracked().id();
            if let Err(failure) = api::start_node_killing_stale(&node).await {
                error.set(Some(format!("Could not stop it and start: {failure}")));
            }
            busy.set(false);
        });
    };

    move || {
        let stale = state.node().status.with(|status| status.stale.clone())?;
        Some(view! {
            <div class="banner banner-warn" role="alert">
                <span>
                    {format!(
                        "A zebrad from an earlier launcher run is still running (pid {}, {}), so the launcher cannot start its own. ",
                        stale.pid,
                        stale.binary,
                    )}
                </span>
                <button class="primary" disabled=move || busy.get() on:click=kill_and_start>
                    {move || if busy.get() { "Stopping it…" } else { "Stop it and start" }}
                </button>
                {move || error.get().map(|error| view! { <span class="error">{error}</span> })}
            </div>
        })
    }
}
