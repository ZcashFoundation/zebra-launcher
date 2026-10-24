//! The Grafana tab: the user's Grafana dashboards embedded in the app.

use leptos::prelude::*;

use crate::app::AppState;

#[component]
pub fn Grafana() -> impl IntoView {
    let state = AppState::get();
    view! {
        <div class="grafana-page" data-dark=move || state.resolved_dark.get().to_string()>
            <p class="muted">"Grafana dashboards appear here."</p>
        </div>
    }
}
