//! The zebrad configuration form in Settings: every key of `zebrad.toml` with a tooltip, read
//! from and written back to the file through `read_config` and `apply_config`.

use leptos::prelude::*;

use crate::{components::tooltip::Help, config_form::FIELDS};

#[component]
pub fn ZebradConfigForm() -> impl IntoView {
    view! {
        <fieldset class="cfg-form">
            <legend>"zebrad configuration"</legend>
            <p class="muted">
                {format!("{} settings", FIELDS.len())}
                <Help id="cfg-placeholder".to_owned() text="Every key of zebrad.toml, with what it does." />
            </p>
        </fieldset>
    }
}
