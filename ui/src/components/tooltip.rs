//! A "?" icon that explains a setting on hover, focus or tap.

use leptos::prelude::*;

/// A help icon with `text` in a tooltip. `id` must be unique on the page: it links the icon
/// to its tooltip for assistive technology.
#[component]
pub fn Help(id: String, text: &'static str) -> impl IntoView {
    let tooltip_id = format!("{id}-tip");
    view! {
        <span class="help">
            <button
                type="button"
                class="help-icon"
                aria-label="What this setting does"
                aria-describedby=tooltip_id.clone()
            >
                "?"
            </button>
            <span role="tooltip" id=tooltip_id class="help-tip">
                {text}
            </span>
        </span>
    }
}
