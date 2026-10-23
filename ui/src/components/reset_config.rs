//! "Reset to managed defaults": replaces `zebrad.toml` with the minimal file the launcher
//! writes, after a confirmation. Used on the Configure page and in the post-update recovery
//! panel.

use leptos::{prelude::*, task::spawn_local};

use crate::{api, app::AppState, format::data_change_allowed};

/// The reset button, its confirmation and the outcome. The backend only resets while zebrad
/// is stopped or failed, so the button is disabled otherwise. The new file goes to
/// [`AppState::config_reset`]; `disabled` blocks the button while the caller is busy (an edit
/// or a save in progress).
#[component]
pub fn ResetConfigButton(#[prop(into, optional)] disabled: Signal<bool>) -> impl IntoView {
    let state = AppState::get();
    let allowed = move || {
        state
            .status
            .with(|status| data_change_allowed(&status.phase))
    };
    let confirming = RwSignal::new(false);
    let busy = RwSignal::new(false);
    let result = RwSignal::new(None::<Result<String, String>>);

    let reset = move |_| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        result.set(None);
        spawn_local(async move {
            result.set(Some(match api::reset_config().await {
                Ok(document) => {
                    let message =
                        format!("Reset. The previous file is kept as {}.bak.", document.path);
                    state.config_reset.set(Some(document));
                    Ok(message)
                }
                Err(error) => Err(format!("Could not reset the config: {error}")),
            }));
            busy.set(false);
            confirming.set(false);
        });
    };

    view! {
        <div class="reset-config">
            <div class="buttons">
                <button
                    type="button"
                    class="reset-config-start"
                    disabled=move || busy.get() || confirming.get() || disabled.get() || !allowed()
                    on:click=move |_| {
                        result.set(None);
                        confirming.set(true);
                    }
                >
                    "Reset to managed defaults…"
                </button>
                <span class="muted">
                    {move || {
                        if allowed() {
                            "Replaces zebrad.toml with the launcher's minimal file; the keys you added are removed."
                        } else {
                            "Stop the node to reset its config."
                        }
                    }}
                </span>
            </div>
            <Show when=move || confirming.get()>
                <div class="reset-confirm banner banner-warn" role="alert">
                    <p>
                        "This removes every key you added to zebrad.toml and keeps only the ones the launcher manages. The current file is kept as "
                        <code>"zebrad.toml.bak"</code>
                        ", replacing an earlier backup."
                    </p>
                    <div class="buttons">
                        <button
                            type="button"
                            class="danger reset-config-confirm"
                            disabled=move || busy.get() || !allowed()
                            on:click=reset
                        >
                            {move || if busy.get() { "Resetting…" } else { "Reset zebrad.toml" }}
                        </button>
                        <button
                            type="button"
                            disabled=move || busy.get()
                            on:click=move |_| confirming.set(false)
                        >
                            "Cancel"
                        </button>
                    </div>
                </div>
            </Show>
            {move || {
                result
                    .get()
                    .map(|result| match result {
                        Ok(text) => view! { <p class="ok reset-result">{text}</p> }.into_any(),
                        Err(text) => view! { <p class="error reset-result">{text}</p> }.into_any(),
                    })
            }}
        </div>
    }
}
