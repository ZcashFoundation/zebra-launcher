//! The zebrad config editor: read-only view, Edit, then Discard or Save & Apply.
//!
//! Applying writes the file and restarts zebrad. If zebrad rejects the new config the backend
//! restores the previous file and answers `RolledBack`; if the file changed on disk since it
//! was read the backend refuses with a `conflict` error and nothing is written.
//!
//! "Reset to managed defaults" (only while zebrad is stopped or failed) replaces the file with
//! the launcher's minimal one, keeping the old file as `zebrad.toml.bak`.

use leptos::{prelude::*, task::spawn_local};

use launcher_types::{ApplyErrorKind, ApplyOutcome, ConfigDocument};

use crate::{
    api, app::AppState, components::reset_config::ResetConfigButton, format::highlight_toml,
};

/// What the editor area shows.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Mode {
    Loading,
    Viewing,
    Editing,
    Applying,
}

/// The message above the editor after an action.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Notice {
    Applied,
    RolledBack(String),
    Conflict(String),
    Failed(String),
}

#[component]
pub fn Configure() -> impl IntoView {
    let document = RwSignal::new(None::<ConfigDocument>);
    let draft = RwSignal::new(String::new());
    let mode = RwSignal::new(Mode::Loading);
    let notice = RwSignal::new(None::<Notice>);
    // Kept apart from `notice`, so a failed reload after a rollback shows next to the rollback
    // message instead of replacing it.
    let load_error = RwSignal::new(None::<String>);

    // Reloads the file. With `keep_editing`, the draft stays open (after a rollback the user
    // usually wants to fix their edit, against the token of the restored file).
    let load = move |keep_editing: bool| {
        spawn_local(async move {
            match api::read_config().await {
                Ok(fresh) => {
                    load_error.set(None);
                    document.set(Some(fresh));
                    mode.set(if keep_editing {
                        Mode::Editing
                    } else {
                        Mode::Viewing
                    });
                }
                Err(error) => {
                    load_error.set(Some(format!("Could not read the config: {error}")));
                    // After a rollback the draft stays open even though the reload failed.
                    mode.set(if keep_editing {
                        Mode::Editing
                    } else if document.with_untracked(Option::is_some) {
                        Mode::Viewing
                    } else {
                        Mode::Loading
                    });
                }
            }
        });
    };
    load(false);

    // A reset, here or in the update recovery panel, replaces the file: re-read it. An open
    // edit keeps its token, so saving it is refused as a conflict, and Discard re-reads; a save
    // in progress re-reads when it finishes.
    let state = AppState::get();
    Effect::new(move |previous: Option<u64>| {
        let changed = state.config_changed.get();
        if previous.is_some_and(|previous| previous != changed) {
            match mode.get_untracked() {
                Mode::Viewing | Mode::Loading => {
                    notice.set(None);
                    load_error.set(None);
                    load(false);
                }
                Mode::Editing => notice.set(Some(Notice::Conflict(
                    "zebrad.toml was reset while you were editing; saving this edit is refused."
                        .into(),
                ))),
                Mode::Applying => {}
            }
        }
        changed
    });

    let retry_load = move |_| {
        let keep_editing = mode.get_untracked() == Mode::Editing;
        load_error.set(None);
        if !keep_editing {
            mode.set(Mode::Loading);
        }
        load(keep_editing);
    };
    let edit = move |_| {
        if let Some(doc) = document.get_untracked() {
            draft.set(doc.text);
            notice.set(None);
            load_error.set(None);
            mode.set(Mode::Editing);
        }
    };
    // Re-reads the file, so a reset or an outside change made while the edit was open shows.
    let discard = move |_| {
        mode.set(Mode::Loading);
        notice.set(None);
        load_error.set(None);
        load(false);
    };
    let reload = move |_| {
        notice.set(None);
        load_error.set(None);
        mode.set(Mode::Loading);
        load(false);
    };
    let save = move |_| {
        let Some(token) = document.with_untracked(|doc| doc.as_ref().map(|doc| doc.token.clone()))
        else {
            return;
        };
        let text = draft.get_untracked();
        mode.set(Mode::Applying);
        notice.set(None);
        load_error.set(None);
        spawn_local(async move {
            match api::apply_config(text, token).await {
                Ok(ApplyOutcome::Applied) => {
                    notice.set(Some(Notice::Applied));
                    load(false);
                }
                Ok(ApplyOutcome::RolledBack { error }) => {
                    notice.set(Some(Notice::RolledBack(error)));
                    load(true);
                }
                Err(error) if error.kind == ApplyErrorKind::Conflict => {
                    notice.set(Some(Notice::Conflict(error.message)));
                    mode.set(Mode::Editing);
                }
                Err(error) => {
                    notice.set(Some(Notice::Failed(error.message)));
                    mode.set(Mode::Editing);
                }
            }
        });
    };

    let notice_view = move || {
        notice.get().map(|notice| match notice {
            Notice::Applied => view! {
                <div class="banner banner-ok" role="status">
                    "Saved. zebrad restarted with the new config."
                </div>
            }
            .into_any(),
            Notice::RolledBack(error) => view! {
                <div class="banner banner-error error-panel" role="alert">
                    <strong>"zebrad rejected the new config, so the launcher rolled back to the previous config."</strong>
                    <p>"Your edit is still open below. zebrad said:"</p>
                    <pre>{error}</pre>
                </div>
            }
            .into_any(),
            Notice::Conflict(message) => view! {
                <div class="banner banner-warn" role="alert">
                    <strong>"The file changed on disk, reload. "</strong>
                    <span>{message}</span>
                    " "
                    <button class="link" on:click=reload>"Reload (discards your edit)"</button>
                </div>
            }
            .into_any(),
            Notice::Failed(message) => view! {
                <div class="banner banner-error" role="alert">{message}</div>
            }
            .into_any(),
        })
    };

    let load_error_view = move || {
        load_error.get().map(|message| {
            let editing = mode.get() == Mode::Editing;
            view! {
                <div class="banner banner-error" role="alert">
                    <span>{message}</span>
                    {editing
                        .then_some(
                            " Your edit is still open; retry before saving, or the save may be refused because the file changed.",
                        )}
                    " "
                    <button class="link" on:click=retry_load>"Retry"</button>
                </div>
            }
        })
    };

    let body = move || match mode.get() {
        Mode::Loading => view! { <p class="muted">"Loading the config…"</p> }.into_any(),
        Mode::Viewing => {
            let spans = document.with(|doc| {
                doc.as_ref()
                    .map(|doc| highlight_toml(&doc.text))
                    .unwrap_or_default()
            });
            view! {
                <pre class="config-view">
                    {spans
                        .into_iter()
                        .map(|(class, text)| view! { <span class=class.css()>{text}</span> })
                        .collect_view()}
                </pre>
            }
            .into_any()
        }
        Mode::Editing => view! {
            <textarea
                class="config-edit"
                spellcheck="false"
                aria-label="zebrad.toml"
                prop:value=move || draft.get()
                on:input=move |event| draft.set(event_target_value(&event))
            ></textarea>
        }
        .into_any(),
        Mode::Applying => view! {
            <p class="applying" role="status">"Saving and restarting Zebra…"</p>
        }
        .into_any(),
    };

    let buttons = move || match mode.get() {
        Mode::Viewing => view! { <button on:click=edit>"Edit"</button> }.into_any(),
        Mode::Editing => view! {
            <button on:click=discard>"Discard"</button>
            <button class="primary" on:click=save>"Save & Apply"</button>
        }
        .into_any(),
        Mode::Loading | Mode::Applying => ().into_any(),
    };

    view! {
        <div class="configure">
            <div class="page-head">
                <div>
                    <h2>"zebrad configuration"</h2>
                    <p class="muted path">
                        {move || document.with(|doc| doc.as_ref().map(|doc| doc.path.clone()))}
                    </p>
                </div>
                <div class="buttons">{buttons}</div>
            </div>
            {notice_view}
            {load_error_view}
            {body}
            <section class="config-reset">
                // Also when the file cannot be read, the main case a reset is for.
                <ResetConfigButton disabled=Signal::derive(move || match mode.get() {
                    Mode::Viewing => false,
                    Mode::Loading => load_error.with(Option::is_none),
                    Mode::Editing | Mode::Applying => true,
                }) />
            </section>
        </div>
    }
}
