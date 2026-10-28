//! A radio group: one `<label class="radio">` per option, each with its label and a hint. The
//! one markup for the theme radios (Settings), the network radios (first run) and the update
//! modes (`UpdateModeChoice`), which the second phase moves onto it.
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
#[component]
pub fn RadioChoice<T>(
    name: &'static str,
    options: Vec<(T, &'static str, &'static str)>,
    #[prop(into)] selected: Signal<Option<T>>,
    on_select: Callback<T>,
    #[prop(optional)] value: Option<fn(T) -> &'static str>,
) -> impl IntoView
where
    T: Copy + PartialEq + Send + Sync + 'static,
{
    options
        .into_iter()
        .map(|(option, label, hint)| {
            view! {
                <label class="radio">
                    <input
                        type="radio"
                        name=name
                        value=value.map(|value| value(option))
                        prop:checked=move || selected.get() == Some(option)
                        on:change=move |_| on_select.run(option)
                    />
                    <span>{label}</span>
                    {(!hint.is_empty()).then(|| view! { <span class="muted">{hint}</span> })}
                </label>
            }
        })
        .collect_view()
}
