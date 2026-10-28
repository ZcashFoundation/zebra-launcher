//! The outcome of an action in a few words: `Ok(text)` in the ok colour, `Err(text)` in the
//! error colour, nothing while there is none.
//!
//! The note is one element that stays in the page, empty or not, so it is a live region before
//! its text changes and assistive technology announces the outcome. Pair it with
//! [`crate::components::transient::Transient::report`], which keeps a success for a while and
//! an error until the next outcome: the one policy for every save, copy, reset and delete.
//!
//! Used by the copy notes on the Logs and Grafana pages. The second phase moves the
//! `RwSignal<Option<Result<_, String>>>` outcomes of the Settings page (`Save`, `DataDirRow`,
//! `Diagnostics`, `DeleteNodeData`), `ResetConfigButton` and `CheckForUpdate` onto it, each
//! with its own `Transient<Result<String, String>>`.

use leptos::prelude::*;

/// A note for an outcome: a `<span>` beside the button that caused it, or with `block` a
/// `<p>` under it.
#[component]
pub fn OutcomeNote(
    #[prop(into)] outcome: Signal<Option<Result<String, String>>>,
    #[prop(optional)] block: bool,
) -> impl IntoView {
    let class = move || {
        outcome.with(|outcome| match outcome {
            None => "outcome-note",
            Some(Ok(_)) => "outcome-note ok",
            Some(Err(_)) => "outcome-note error",
        })
    };
    let text = move || {
        outcome.with(|outcome| {
            outcome.as_ref().map(|outcome| match outcome {
                Ok(text) | Err(text) => text.clone(),
            })
        })
    };
    if block {
        view! { <p class=class role="status">{text}</p> }.into_any()
    } else {
        view! { <span class=class role="status">{text}</span> }.into_any()
    }
}
