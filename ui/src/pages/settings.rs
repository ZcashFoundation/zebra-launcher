//! The launcher settings form, plus the data directory, diagnostics and "Delete node data".
//! Save sends the whole settings object to `set_settings`; the data directory is changed only
//! through `choose_data_dir`, so Save always sends the stored one.

use launcher_types::{Network, Settings, is_loopback_http_url};
use leptos::{prelude::*, task::spawn_local};

use crate::{
    api,
    app::AppState,
    clipboard::copy_to_clipboard,
    components::update_mode_choice::UpdateModeChoice,
    format::{bytes, data_change_allowed, is_low_space, state_dir},
};

/// The word to type before node data is deleted.
const DELETE_WORD: &str = "delete";

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

/// Checks the Grafana URL field. Empty means "no dashboard"; anything else must pass the rule
/// the backend also enforces, [`is_loopback_http_url`], which mirrors the CSP's `frame-src`.
fn parse_grafana_url(input: &str) -> Result<Option<String>, String> {
    let url = input.trim();
    if url.is_empty() {
        Ok(None)
    } else if is_loopback_http_url(url) {
        Ok(Some(url.to_owned()))
    } else {
        Err(
            "Grafana must run on this computer: use http://127.0.0.1 or http://localhost, \
             with an optional port and path."
                .into(),
        )
    }
}

/// A change in flight on this page. Only one runs at a time: Save sends the whole settings
/// object, so a Save racing a data folder change could store the folder it read before the
/// change, and neither should run while node data is being deleted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mutation {
    Save,
    DataDir,
    DeleteNodeData,
}

#[component]
pub fn SettingsPage() -> impl IntoView {
    let state = AppState::get();
    let draft = RwSignal::new(None::<Settings>);
    let fields = RwSignal::new(TextFields::default());
    let busy = RwSignal::new(None::<Mutation>);
    let saving = move || busy.get() == Some(Mutation::Save);
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
    // The form's current values as settings, for comparing with what is stored.
    let edited = move || {
        let mut edited = draft.get()?;
        let f = fields.get();
        edited.grafana_url = parse_grafana_url(&f.grafana_url).ok().flatten();
        edited.stop_grace_secs = parse_stop_grace(&f.stop_grace_secs).unwrap_or(0);
        Some(edited)
    };
    // Fill the form when the settings arrive, and again when they change elsewhere (the
    // first-run flow, the data folder) while the form is not edited. The data folder is not a
    // form field, so it does not count as an edit.
    Effect::new(move |previous: Option<Option<Settings>>| {
        let loaded = state.settings.get();
        let untouched = match previous.flatten() {
            None => true,
            Some(previous) => untrack(edited).is_none_or(|mut edited| {
                edited.data_dir = previous.data_dir.clone();
                edited == previous
            }),
        };
        if loaded.is_some() && untouched {
            reset();
        }
        loaded
    });

    let grafana_error =
        Memo::new(move |_| fields.with(|f| parse_grafana_url(&f.grafana_url).err()));
    let grace_error =
        Memo::new(move |_| fields.with(|f| parse_stop_grace(&f.stop_grace_secs).err()));
    let dirty = move || {
        let Some(edited) = edited() else {
            return false;
        };
        state
            .settings
            .with(|loaded| loaded.as_ref() != Some(&edited))
    };

    let save = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        if busy.get_untracked().is_some() {
            return;
        }
        let Some(mut settings) = draft.get_untracked() else {
            return;
        };
        let f = fields.get_untracked();
        let (grafana_url, stop_grace_secs) = match (
            parse_grafana_url(&f.grafana_url),
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
        // `set_settings` keeps the stored `data_dir` whatever is sent; send that one anyway.
        settings.data_dir = state
            .settings
            .with_untracked(|stored| stored.as_ref().and_then(|s| s.data_dir.clone()));
        busy.set(Some(Mutation::Save));
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
            busy.set(None);
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
    view! {
        <Show
            when=move || draft.with(Option::is_some)
            fallback=|| view! { <p class="muted">"Loading settings…"</p> }
        >
            <form class="settings" on:submit=save>
                <h2>"Settings"</h2>

                <fieldset>
                    <legend>"Updates"</legend>
                    <UpdateModeChoice
                        name="update-mode"
                        selected=Signal::derive(move || {
                            draft.with(|d| d.as_ref().map(|d| d.update_mode))
                        })
                        on_select=Callback::new(move |mode| {
                            draft.update(|d| {
                                if let Some(d) = d {
                                    d.update_mode = mode;
                                }
                            });
                            result.set(None);
                        })
                    />
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
                    <DataDirRow draft busy />
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
                            "Shown on the dashboard. Only Grafana on this computer (http://127.0.0.1 or http://localhost, any port) can be embedded; leave empty to hide it."
                        </span>
                        {move || grafana_error.get().map(|e| view! { <span class="error">{e}</span> })}
                    </label>
                </fieldset>

                <div class="buttons">
                    <button
                        type="button"
                        disabled=move || busy.get().is_some() || !dirty()
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
                            busy.get().is_some() || !dirty() || grafana_error.get().is_some()
                                || grace_error.get().is_some()
                        }
                    >
                        {move || if saving() { "Saving…" } else { "Save" }}
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
            <section class="settings">
                <Diagnostics />
                <DeleteNodeData busy />
            </section>
        </Show>
    }
}

/// True while the backend accepts data changes (zebrad stopped or failed).
fn data_change_open(state: AppState) -> impl Fn() -> bool + Copy {
    move || {
        state
            .status
            .with(|status| data_change_allowed(&status.phase))
    }
}

/// The data directory, with "Change…" (folder picker, then `choose_data_dir`). It is saved
/// right away, not by the form's Save; `draft` follows so the form does not look edited.
#[component]
fn DataDirRow(
    draft: RwSignal<Option<Settings>>,
    busy: RwSignal<Option<Mutation>>,
) -> impl IntoView {
    let state = AppState::get();
    let allowed = data_change_open(state);
    let result = RwSignal::new(None::<Result<Option<u64>, String>>);

    let change = move |_| {
        if busy.get_untracked().is_some() {
            return;
        }
        busy.set(Some(Mutation::DataDir));
        result.set(None);
        spawn_local(async move {
            let outcome = match api::pick_data_dir().await {
                Ok(None) => None,
                Ok(Some(path)) => Some(match api::choose_data_dir(path).await {
                    Ok(choice) => {
                        let data_dir = choice.settings.data_dir.clone();
                        state.settings.set(Some(choice.settings));
                        draft.update(|draft| {
                            if let Some(draft) = draft {
                                draft.data_dir = data_dir;
                            }
                        });
                        Ok(choice.free_bytes)
                    }
                    Err(error) => Err(format!("Could not use that folder: {error}")),
                }),
                Err(error) => Some(Err(format!("Could not open the folder picker: {error}"))),
            };
            result.set(outcome);
            busy.set(None);
        });
    };

    view! {
        <div class="field data-dir-row">
            <span>"Data folder"</span>
            <code class="readonly">
                {move || {
                    state
                        .settings
                        .with(|s| s.as_ref().and_then(|s| s.data_dir.clone()))
                        .unwrap_or_else(|| "default (the launcher's data folder)".into())
                }}
            </code>
            <div class="buttons">
                <button
                    type="button"
                    disabled=move || busy.get().is_some() || !allowed()
                    on:click=change
                >
                    "Change…"
                </button>
                <span class="muted">
                    {move || {
                        if allowed() {
                            "The chain state goes into its state folder. Changing it does not move the state already synced; that stays in the old folder until you delete it."
                        } else {
                            "Stop the node to change it."
                        }
                    }}
                </span>
            </div>
            {move || {
                result
                    .get()
                    .map(|result| match result {
                        Ok(free) => {
                            view! {
                                <span class="ok">
                                    {match free {
                                        Some(free) => format!("Saved. {} free there.", bytes(free)),
                                        None => "Saved. The free space there could not be read.".into(),
                                    }}
                                </span>
                                {is_low_space(free)
                                    .then(|| {
                                        view! {
                                            <span class="error">
                                                "Mainnet needs hundreds of GB; this drive may fill up."
                                            </span>
                                        }
                                    })}
                            }
                                .into_any()
                        }
                        Err(error) => view! { <span class="error">{error}</span> }.into_any(),
                    })
            }}
        </div>
    }
}

/// "Copy diagnostics": collects the text and copies it, or shows it when the clipboard is
/// denied.
#[component]
fn Diagnostics() -> impl IntoView {
    let busy = RwSignal::new(false);
    let note = RwSignal::new(None::<Result<String, String>>);
    let fallback = RwSignal::new(None::<String>);

    let copy = move |_| {
        busy.set(true);
        note.set(None);
        fallback.set(None);
        spawn_local(async move {
            match api::copy_diagnostics().await {
                Ok(text) => match copy_to_clipboard(&text).await {
                    Ok(()) => note.set(Some(Ok(
                        "Copied to the clipboard, ready to paste into an issue.".into(),
                    ))),
                    Err(error) => {
                        note.set(Some(Err(format!(
                            "Could not copy to the clipboard ({error}). Select the text below and copy it by hand."
                        ))));
                        fallback.set(Some(text));
                    }
                },
                Err(error) => {
                    note.set(Some(Err(format!("Could not collect diagnostics: {error}"))))
                }
            }
            busy.set(false);
        });
    };

    view! {
        <fieldset>
            <legend>"Diagnostics"</legend>
            <p class="muted">
                "Versions, the node status, the settings, zebrad.toml and the last lines of the logs, for a bug report. The RPC cookie is never included."
            </p>
            <div class="buttons">
                <button type="button" disabled=move || busy.get() on:click=copy>
                    "Copy diagnostics"
                </button>
                {move || {
                    note.get()
                        .map(|note| match note {
                            Ok(text) => {
                                view! { <span class="ok diagnostics-note">{text}</span> }.into_any()
                            }
                            Err(text) => {
                                view! { <span class="error diagnostics-note">{text}</span> }
                                    .into_any()
                            }
                        })
                }}
            </div>
            {move || {
                fallback
                    .get()
                    .map(|text| {
                        let lines = text.lines().count();
                        view! {
                            <details class="diagnostics" open>
                                <summary>{format!("Diagnostics ({lines} lines)")}</summary>
                                <pre>{text}</pre>
                            </details>
                        }
                    })
            }}
        </fieldset>
    }
}

/// "Delete node data": deletes the chain state after the user types [`DELETE_WORD`]. Only while
/// zebrad is stopped or failed.
#[component]
fn DeleteNodeData(busy: RwSignal<Option<Mutation>>) -> impl IntoView {
    let state = AppState::get();
    let allowed = data_change_open(state);
    let confirming = RwSignal::new(false);
    let typed = RwSignal::new(String::new());
    let deleting = move || busy.get() == Some(Mutation::DeleteNodeData);
    let result = RwSignal::new(None::<Result<String, String>>);

    let state_folder = move || {
        state
            .settings
            .with(|s| s.as_ref().and_then(|s| s.data_dir.clone()))
            .map(|dir| state_dir(&dir))
            .unwrap_or_else(|| "the state folder in the launcher's data folder".into())
    };
    let delete = move |_| {
        if busy.get_untracked().is_some() {
            return;
        }
        busy.set(Some(Mutation::DeleteNodeData));
        result.set(None);
        spawn_local(async move {
            result.set(Some(match api::delete_node_data().await {
                Ok(freed) => Ok(format!("Deleted the node data, freeing {}.", bytes(freed))),
                Err(error) => Err(format!("Could not delete the node data: {error}")),
            }));
            busy.set(None);
            confirming.set(false);
            typed.set(String::new());
        });
    };

    view! {
        <fieldset>
            <legend>"Node data"</legend>
            <div class="buttons">
                <button
                    type="button"
                    disabled=move || busy.get().is_some() || confirming.get() || !allowed()
                    on:click=move |_| {
                        result.set(None);
                        confirming.set(true);
                    }
                >
                    "Delete node data…"
                </button>
                <span class="muted">
                    {move || {
                        if allowed() {
                            "Frees the disk space the chain uses. The next start syncs from scratch."
                        } else {
                            "Stop the node to delete its data."
                        }
                    }}
                </span>
            </div>
            <Show when=move || confirming.get()>
                <div class="delete-confirm banner banner-warn">
                    <p>
                        "This deletes the chain state in " <code>{state_folder}</code>
                        " and nothing else: zebrad.toml, the launcher settings, the logs and the RPC cookie stay. The next start syncs the chain from scratch, which takes a day or more on Mainnet."
                    </p>
                    <label class="field">
                        <span>"Type " <code>{DELETE_WORD}</code> " to confirm"</span>
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
                            class="danger"
                            disabled=move || {
                                busy.get().is_some() || !allowed()
                                    || typed.with(|t| t.trim() != DELETE_WORD)
                            }
                            on:click=delete
                        >
                            {move || if deleting() { "Deleting…" } else { "Delete" }}
                        </button>
                        <button
                            type="button"
                            disabled=deleting
                            on:click=move |_| {
                                confirming.set(false);
                                typed.set(String::new());
                            }
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
                        Ok(text) => view! { <p class="ok delete-result">{text}</p> }.into_any(),
                        Err(text) => view! { <p class="error delete-result">{text}</p> }.into_any(),
                    })
            }}
        </fieldset>
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

    #[test]
    fn grafana_url_uses_the_shared_rule() {
        assert_eq!(parse_grafana_url(""), Ok(None));
        assert_eq!(parse_grafana_url("  "), Ok(None));
        assert_eq!(
            parse_grafana_url(" http://127.0.0.1:3000/d/zebra-overview "),
            Ok(Some("http://127.0.0.1:3000/d/zebra-overview".into()))
        );
        assert_eq!(
            parse_grafana_url("http://LOCALHOST:3000?orgId=1"),
            Ok(Some("http://LOCALHOST:3000?orgId=1".into()))
        );
        for bad in [
            "https://127.0.0.1:3000",
            "http://[::1]:3000",
            "http://grafana.example.com",
            "http://example.com@127.0.0.1:3000",
            "127.0.0.1:3000",
        ] {
            assert!(parse_grafana_url(bad).is_err(), "{bad} was accepted");
        }
    }
}
