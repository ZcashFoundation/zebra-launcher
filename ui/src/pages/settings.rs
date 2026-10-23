//! The launcher settings form. Save sends the whole settings object to `set_settings`.

use leptos::{prelude::*, task::spawn_local};

use crate::{
    api,
    app::AppState,
    format::validate_grafana_url,
    types::{Network, Settings, UpdateMode},
};

const STOP_GRACE_RANGE: std::ops::RangeInclusive<u64> = 5..=3600;

/// Form fields that are typed as text and validated on Save.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct TextFields {
    grafana_url: String,
    stop_grace_secs: String,
}

impl TextFields {
    fn from_settings(settings: &Settings) -> Self {
        Self {
            grafana_url: settings.grafana_url.clone().unwrap_or_default(),
            stop_grace_secs: settings.stop_grace_secs.to_string(),
        }
    }
}

fn parse_stop_grace(text: &str) -> Result<u64, String> {
    match text.trim().parse::<u64>() {
        Ok(secs) if STOP_GRACE_RANGE.contains(&secs) => Ok(secs),
        _ => Err(format!(
            "Enter a whole number of seconds from {} to {}.",
            STOP_GRACE_RANGE.start(),
            STOP_GRACE_RANGE.end()
        )),
    }
}

#[component]
pub fn SettingsPage() -> impl IntoView {
    let state = AppState::get();
    let draft = RwSignal::new(None::<Settings>);
    let fields = RwSignal::new(TextFields::default());
    let saving = RwSignal::new(false);
    let result = RwSignal::new(None::<Result<(), String>>);

    let reset = move || {
        let loaded = state.settings.get_untracked();
        fields.set(
            loaded
                .as_ref()
                .map(TextFields::from_settings)
                .unwrap_or_default(),
        );
        draft.set(loaded);
    };
    // Fill the form once the settings arrive.
    Effect::new(move |_| {
        if state.settings.with(Option::is_some) && draft.with_untracked(Option::is_none) {
            reset();
        }
    });

    let grafana_error =
        Memo::new(move |_| fields.with(|f| validate_grafana_url(&f.grafana_url).err()));
    let grace_error =
        Memo::new(move |_| fields.with(|f| parse_stop_grace(&f.stop_grace_secs).err()));
    let dirty = move || {
        let Some(mut edited) = draft.get() else {
            return false;
        };
        let f = fields.get();
        edited.grafana_url = validate_grafana_url(&f.grafana_url).ok().flatten();
        edited.stop_grace_secs = parse_stop_grace(&f.stop_grace_secs).unwrap_or(0);
        state
            .settings
            .with(|loaded| loaded.as_ref() != Some(&edited))
    };

    let save = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        let Some(mut settings) = draft.get_untracked() else {
            return;
        };
        let f = fields.get_untracked();
        let (grafana_url, stop_grace_secs) = match (
            validate_grafana_url(&f.grafana_url),
            parse_stop_grace(&f.stop_grace_secs),
        ) {
            (Ok(url), Ok(secs)) => (url, secs),
            _ => {
                result.set(Some(Err("Fix the highlighted fields first.".into())));
                return;
            }
        };
        settings.grafana_url = grafana_url;
        settings.stop_grace_secs = stop_grace_secs;
        saving.set(true);
        result.set(None);
        spawn_local(async move {
            match api::set_settings(settings).await {
                Ok(stored) => {
                    state.settings.set(Some(stored));
                    reset();
                    result.set(Some(Ok(())));
                }
                Err(error) => result.set(Some(Err(format!("Could not save: {error}")))),
            }
            saving.set(false);
        });
    };

    let update = move |change: fn(&mut Settings, bool), value: bool| {
        draft.update(|draft| {
            if let Some(draft) = draft {
                change(draft, value);
            }
        });
        result.set(None);
    };
    let read = move |pick: fn(&Settings) -> bool| {
        move || draft.with(|draft| draft.as_ref().is_some_and(pick))
    };
    let update_mode_radio = move |mode: UpdateMode, label: &'static str, hint: &'static str| {
        view! {
            <label class="radio">
                <input
                    type="radio"
                    name="update-mode"
                    prop:checked=move || {
                        draft.with(|d| d.as_ref().is_some_and(|d| d.update_mode == mode))
                    }
                    on:change=move |_| {
                        draft.update(|d| {
                            if let Some(d) = d {
                                d.update_mode = mode;
                            }
                        });
                        result.set(None);
                    }
                />
                <span>{label}</span>
                <span class="muted">{hint}</span>
            </label>
        }
    };

    view! {
        <Show
            when=move || draft.with(Option::is_some)
            fallback=|| view! { <p class="muted">"Loading settings…"</p> }
        >
            <form class="settings" on:submit=save>
                <h2>"Settings"</h2>

                <fieldset>
                    <legend>"Updates"</legend>
                    {update_mode_radio(UpdateMode::Off, "Off", "never check for launcher updates")}
                    {update_mode_radio(UpdateMode::Notify, "Notify", "check, and ask before installing")}
                    {update_mode_radio(UpdateMode::Auto, "Automatic", "install updates when they are published")}
                </fieldset>

                <fieldset>
                    <legend>"Behaviour"</legend>
                    <label class="check">
                        <input
                            type="checkbox"
                            prop:checked=read(|s| s.close_to_tray)
                            on:change=move |event| {
                                update(|s, v| s.close_to_tray = v, event_target_checked(&event))
                            }
                        />
                        "Keep running in the tray when the window is closed"
                    </label>
                    <label class="check">
                        <input
                            type="checkbox"
                            prop:checked=read(|s| s.start_node_on_launch)
                            on:change=move |event| {
                                update(
                                    |s, v| s.start_node_on_launch = v,
                                    event_target_checked(&event),
                                )
                            }
                        />
                        "Start the node when the launcher opens"
                    </label>
                    <label class="field">
                        <span>"Stop grace period (seconds)"</span>
                        <input
                            type="number"
                            min=*STOP_GRACE_RANGE.start()
                            max=*STOP_GRACE_RANGE.end()
                            class:invalid=move || grace_error.get().is_some()
                            prop:value=move || fields.with(|f| f.stop_grace_secs.clone())
                            on:input=move |event| {
                                fields.update(|f| f.stop_grace_secs = event_target_value(&event));
                                result.set(None);
                            }
                        />
                        <span class="muted">"How long zebrad gets to shut down before it is killed."</span>
                        {move || grace_error.get().map(|e| view! { <span class="error">{e}</span> })}
                    </label>
                </fieldset>

                <fieldset>
                    <legend>"Node"</legend>
                    <label class="field">
                        <span>"Network"</span>
                        <select
                            prop:value=move || {
                                draft.with(|d| match d.as_ref().map(|d| d.network) {
                                    Some(Network::Testnet) => "testnet",
                                    _ => "mainnet",
                                })
                            }
                            on:change=move |event| {
                                let network = match event_target_value(&event).as_str() {
                                    "testnet" => Network::Testnet,
                                    _ => Network::Mainnet,
                                };
                                draft.update(|d| {
                                    if let Some(d) = d {
                                        d.network = network;
                                    }
                                });
                                result.set(None);
                            }
                        >
                            <option value="mainnet">"Mainnet"</option>
                            <option value="testnet">"Testnet"</option>
                        </select>
                    </label>
                    <div class="field">
                        <span>"Data directory"</span>
                        <code class="readonly">
                            {move || {
                                draft
                                    .with(|d| d.as_ref().and_then(|d| d.data_dir.clone()))
                                    .unwrap_or_else(|| "the default location".into())
                            }}
                        </code>
                    </div>
                </fieldset>

                <fieldset>
                    <legend>"Metrics"</legend>
                    <label class="field">
                        <span>"Grafana dashboard URL"</span>
                        <input
                            type="url"
                            placeholder="http://127.0.0.1:3000/d/zebra-overview"
                            class:invalid=move || grafana_error.get().is_some()
                            prop:value=move || fields.with(|f| f.grafana_url.clone())
                            on:input=move |event| {
                                fields.update(|f| f.grafana_url = event_target_value(&event));
                                result.set(None);
                            }
                        />
                        <span class="muted">
                            "Shown on the dashboard. Only Grafana on this computer (127.0.0.1 or localhost) can be embedded; leave empty to hide it."
                        </span>
                        {move || grafana_error.get().map(|e| view! { <span class="error">{e}</span> })}
                    </label>
                </fieldset>

                <div class="buttons">
                    <button
                        type="button"
                        disabled=move || saving.get() || !dirty()
                        on:click=move |_| {
                            reset();
                            result.set(None);
                        }
                    >
                        "Revert"
                    </button>
                    <button
                        type="submit"
                        class="primary"
                        disabled=move || {
                            saving.get() || !dirty() || grafana_error.get().is_some()
                                || grace_error.get().is_some()
                        }
                    >
                        {move || if saving.get() { "Saving…" } else { "Save" }}
                    </button>
                    {move || {
                        result
                            .get()
                            .map(|result| match result {
                                Ok(()) => view! { <span class="ok">"Saved."</span> }.into_any(),
                                Err(error) => view! { <span class="error">{error}</span> }.into_any(),
                            })
                    }}
                </div>
            </form>
        </Show>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_grace_must_be_in_range() {
        assert_eq!(parse_stop_grace(" 60 "), Ok(60));
        assert!(parse_stop_grace("4").is_err());
        assert!(parse_stop_grace("3601").is_err());
        assert!(parse_stop_grace("1.5").is_err());
        assert!(parse_stop_grace("").is_err());
    }
}
