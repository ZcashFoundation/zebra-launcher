//! A label/value list, `<dl class="rows">`: the dashboard's Node and Chain cards.

use leptos::prelude::*;

#[component]
pub fn Rows(rows: Signal<Vec<(&'static str, String)>>) -> impl IntoView {
    view! {
        <dl class="rows">
            {move || {
                rows.get()
                    .into_iter()
                    .map(|(label, value)| view! {
                        <dt>{label}</dt>
                        <dd>{value}</dd>
                    })
                    .collect_view()
            }}
        </dl>
    }
}
