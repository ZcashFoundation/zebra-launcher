//! A radio group: one `<label class="radio">` per option, each with its label and a hint. The
//! one markup for the theme radios (Settings), the network radios (first run), the light client
//! server's backends (its consent screen) and the update modes (`UpdateModeChoice`), which the
//! second phase moves onto it.
//!
//! To adopt it: `UpdateModeChoice` becomes `RadioChoice` over `[Off, Notify, Auto]` with
//! `update_mode_text` as the rows; Settings passes `THEMES` with `theme_text` and
//! `value=theme_value` (`Theme::as_str` once launcher-types has it); the wizard passes the two
//! networks with their hints and `Network::as_str`.

use leptos::prelude::*;

/// A radio group over the `options`: `(value, label, hint)` rows in order. `name` groups the
/// buttons and must be unique on the page. `selected` is the checked option (`None`: none
/// checked, as while settings load); `on_select` gets the option the user picked. `value`
/// gives each radio's `value` attribute (the wire name), when the markup should carry one.
/// `disabled` lists the options that can't be picked now, and `notes` gives an option a line
/// that changes, such as why it is disabled (the light client server's backends).
#[component]
pub fn RadioChoice<T>(
    name: &'static str,
    options: Vec<(T, &'static str, &'static str)>,
    #[prop(into)] selected: Signal<Option<T>>,
    on_select: Callback<T>,
    #[prop(optional)] value: Option<fn(T) -> &'static str>,
    #[prop(optional)] disabled: Option<Signal<Vec<T>>>,
    #[prop(optional)] notes: Option<Signal<Vec<(T, String)>>>,
) -> impl IntoView
where
    T: Copy + PartialEq + Send + Sync + 'static,
{
    let off = move |option: T| disabled.is_some_and(|disabled| disabled.get().contains(&option));
    options
        .into_iter()
        .map(|(option, label, hint)| {
            let note = move || {
                notes.and_then(|notes| {
                    notes
                        .get()
                        .into_iter()
                        .find_map(|(of, note)| (of == option).then_some(note))
                })
            };
            view! {
                <label class="radio" class:disabled=move || off(option)>
                    <input
                        type="radio"
                        name=name
                        value=value.map(|value| value(option))
                        disabled=move || off(option)
                        prop:checked=move || selected.get() == Some(option)
                        on:change=move |_| on_select.run(option)
                    />
                    <span>{label}</span>
                    {(!hint.is_empty()).then(|| view! { <span class="muted">{hint}</span> })}
                    {move || note().map(|note| view! { <span class="muted radio-note">{note}</span> })}
                </label>
            }
        })
        .collect_view()
}
