//! The update-mode radio buttons, shared by the first-run wizard and Settings.

use launcher_types::UpdateMode;
use leptos::prelude::*;

use crate::format::update_mode_text;

/// One radio button per [`UpdateMode`], each with its explanation. `selected` is the checked
/// mode (`None`: none checked); `on_select` gets the mode the user picked. `name` groups the
/// buttons and must be unique on the page.
#[component]
pub fn UpdateModeChoice(
    name: &'static str,
    #[prop(into)] selected: Signal<Option<UpdateMode>>,
    on_select: Callback<UpdateMode>,
) -> impl IntoView {
    [UpdateMode::Off, UpdateMode::Notify, UpdateMode::Auto]
        .into_iter()
        .map(|mode| {
            let (label, hint) = update_mode_text(mode);
            view! {
                <label class="radio">
                    <input
                        type="radio"
                        name=name
                        prop:checked=move || selected.get() == Some(mode)
                        on:change=move |_| on_select.run(mode)
                    />
                    <span>{label}</span>
                    <span class="muted">{hint}</span>
                </label>
            }
        })
        .collect_view()
}
