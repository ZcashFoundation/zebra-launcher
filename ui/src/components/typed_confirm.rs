//! A confirmation that asks the user to type a word: the warning, the field, the action
//! button (disabled until the word is typed) and Cancel. The markup of the Settings page's
//! "Delete node data" and the wizard's alpha.4 clean-up, which the second phase moves onto it.
//!
//! To adopt it: the caller keeps its `confirming` signal and shows `<TypedConfirm>` inside a
//! `<Show>`; `on_confirm` runs the action, `on_cancel` closes it. The children are the
//! explanation paragraphs, and `blocked` is the caller's reason the action cannot run just
//! now (the node is running), which keeps the button disabled whatever was typed. `class`
//! adds to the banner (`delete-confirm` at both call sites); the action button carries
//! `typed-confirm-action` for scripts that drive the UI.

use leptos::prelude::*;

/// A typed confirmation inside a warning banner. `word` must be typed (surrounding spaces
/// ignored) to enable the `action` button, which reads `busy_label` while `busy`.
#[component]
pub fn TypedConfirm(
    word: &'static str,
    action: &'static str,
    busy_label: &'static str,
    #[prop(into)] busy: Signal<bool>,
    #[prop(into, optional)] blocked: Signal<bool>,
    on_confirm: Callback<()>,
    on_cancel: Callback<()>,
    #[prop(optional)] class: &'static str,
    children: Children,
) -> impl IntoView {
    let typed = RwSignal::new(String::new());
    let matches = move || typed.with(|typed| typed.trim() == word);
    view! {
        <div class=format!("typed-confirm banner banner-warn {class}") role="alert">
            {children()}
            <label class="field">
                <span>"Type " <code>{word}</code> " to confirm"</span>
                <input
                    type="text"
                    autocomplete="off"
                    prop:value=move || typed.get()
                    on:input=move |event| typed.set(event_target_value(&event))
                />
            </label>
            <div class="buttons">
                <button
                    type="button"
                    class="danger typed-confirm-action"
                    disabled=move || busy.get() || blocked.get() || !matches()
                    on:click=move |_| {
                        typed.set(String::new());
                        on_confirm.run(());
                    }
                >
                    {move || if busy.get() { busy_label } else { action }}
                </button>
                <button
                    type="button"
                    disabled=move || busy.get()
                    on:click=move |_| {
                        typed.set(String::new());
                        on_cancel.run(());
                    }
                >
                    "Cancel"
                </button>
            </div>
        </div>
    }
}
