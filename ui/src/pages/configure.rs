//! The zebrad config editor: read-only view, Edit, then Discard or Save & Apply.
//!
//! Applying writes the file and restarts zebrad. If zebrad rejects the new config the backend
//! restores the previous file and answers `RolledBack`; if the file changed on disk since it
//! was read the backend refuses with a `conflict` error and nothing is written.

use leptos::{prelude::*, task::spawn_local};

use launcher_types::{ApplyErrorKind, ApplyOutcome, ConfigDocument};

use crate::{api, format::highlight_toml};

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

    // Reloads the file. With `keep_editing`, the draft stays open (after a rollback the user
    // usually wants to fix their edit, against the token of the restored file).
    let load = move |keep_editing: bool| {
        spawn_local(async move {
            match api::read_config().await {
                Ok(fresh) => {
                    document.set(Some(fresh));
                    mode.set(if keep_editing {
                        Mode::Editing
                    } else {
                        Mode::Viewing
                    });
                }
                Err(error) => {
                    notice.set(Some(Notice::Failed(format!(
                        "Could not read the config: {error}"
                    ))));
                    mode.set(if document.with_untracked(Option::is_some) {
                        Mode::Viewing
                    } else {
                        Mode::Loading
                    });
                }
            }
        });
    };
    load(false);

    let edit = move |_| {
        if let Some(doc) = document.get_untracked() {
            draft.set(doc.text);
            notice.set(None);
            mode.set(Mode::Editing);
        }
    };
    let discard = move |_| {
        mode.set(Mode::Viewing);
        notice.set(None);
    };
    let reload = move |_| {
        notice.set(None);
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
            {body}
        </div>
    }
}
