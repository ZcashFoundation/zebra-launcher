//! The tab bar.

use leptos::prelude::*;

use crate::app::{AppState, Tab};

#[component]
pub fn Nav() -> impl IntoView {
    let state = AppState::get();
    view! {
        <nav class="tabs" role="tablist">
            {Tab::ALL
                .into_iter()
                .map(|tab| {
                    let selected = move || state.tab.get() == tab;
                    view! {
                        <button
                            role="tab"
                            class="tab"
                            class:active=selected
                            aria-selected=move || selected().to_string()
                            on:click=move |_| state.tab.set(tab)
                        >
                            {tab.label()}
                        </button>
                    }
                })
                .collect_view()}
        </nav>
    }
}
