//! The Configure page: zebrad's configuration as one draft with two views, the form beside
//! `zebrad.toml` (under it when the window is narrow). A change in the form is written into the
//! file at once, its line marked and brought into view; the file can also be edited as text,
//! and the form follows. One action bar saves or discards the draft.
//!
//! Saving writes the file and restarts a running zebrad. If zebrad rejects the new config the
//! backend restores the previous file and answers `RolledBack`; if the file changed on disk
//! since it was read the backend refuses with a `conflict` error and nothing is written. Both
//! keep the draft.
//!
//! "Reset to managed defaults" (only while zebrad is stopped or failed) replaces the file with
//! the launcher's minimal one, keeping the old file as `zebrad.toml.bak`.

use std::time::Duration;

use leptos::{html, prelude::*};
use wasm_bindgen::{JsCast, closure::Closure};

use crate::{
    components::{
        config_form::{
            ConfigEditor, Load, Notice, ZebradConfigForm, changed_list, changes_text, save_label,
            saved_text, text_only_note,
        },
        reset_config::ResetConfigButton,
    },
    config_view::{LineChange, change_summary, key_line, line_changes, scroll_to_reveal},
    format::highlight_toml,
};

/// How long the line last edited in the form stays highlighted.
const FLASH_FOR: Duration = Duration::from_millis(1200);

#[component]
pub fn Configure() -> impl IntoView {
    let editor = ConfigEditor::new();
    let path = move || {
        editor
            .document
            .with(|document| document.as_ref().map(|document| document.path.clone()))
    };
    let has_document = move || editor.document.with(Option::is_some);
    // Ctrl (or Cmd) + S saves, from anywhere on the page.
    let on_key = move |event: web_sys::KeyboardEvent| {
        if (event.ctrl_key() || event.meta_key())
            && !event.alt_key()
            && event.key().eq_ignore_ascii_case("s")
        {
            event.prevent_default();
            editor.save();
        }
    };

    let load_view = move || {
        match editor.load.get() {
        Load::Loading => view! { <p class="muted">"Loading zebrad.toml…"</p> }.into_any(),
        Load::Failed(message) => view! {
            <div class="banner banner-error" role="alert">
                <span>{message}</span>
                {editor
                    .edited
                    .get()
                    .then_some(
                        " Your draft is kept; retry before saving, or the save may be refused because the file changed.",
                    )}
                " "
                <button type="button" class="link" on:click=move |_| editor.retry()>
                    "Retry"
                </button>
            </div>
        }
        .into_any(),
        Load::Ready => ().into_any(),
    }
    };
    let notice_view = move || {
        editor.notice.get().and_then(|notice| match notice {
            Notice::Saved { .. } => None,
            Notice::RolledBack(error) => Some(
                view! {
                    <div class="banner banner-error error-panel" role="alert">
                        <strong>
                            "zebrad rejected the new configuration and was restarted with the previous one."
                        </strong>
                        <p>"Your draft is kept. zebrad said:"</p>
                        <pre>{error}</pre>
                    </div>
                }
                .into_any(),
            ),
            Notice::Conflict { kept } => Some(
                view! {
                    <div class="banner banner-warn" role="alert">
                        <strong>"The file changed on disk and was read again. "</strong>
                        {if kept {
                            "Your draft is kept as it is; the marked lines are those that differ from the file now (turn off Edit as text to see them). Save again to replace it."
                        } else {
                            "Your edits were put back on top of it; check them and save again."
                        }}
                    </div>
                }
                .into_any(),
            ),
            Notice::Failed(message) => {
                Some(view! { <div class="banner banner-error" role="alert">{message}</div> }.into_any())
            }
        })
    };
    let saved_view = move || {
        editor.notice.get().and_then(|notice| match notice {
            Notice::Saved { restarted } => Some(saved_text(restarted)),
            _ => None,
        })
    };
    let change_count = move || editor.changes.with(Vec::len) + editor.removals.with(Vec::len);
    let changes_detail = move || {
        let dirty = editor.dirty.get();
        text_only_note(change_count(), dirty)
            .map(str::to_owned)
            .unwrap_or_else(|| {
                editor.changes.with(|changes| {
                    editor
                        .removals
                        .with(|removals| changed_list(changes, removals))
                })
            })
    };
    // Also while the file cannot be read, the main case a reset is for; not over an unsaved
    // draft, which Discard clears first. A forbidden key the draft drops is not such a draft.
    let reset_blocked = Signal::derive(move || {
        editor.saving.get()
            || editor.edited.get()
            || editor.load.with(|load| *load == Load::Loading)
    });
    // The file pane's height budget: the scrolling page's height minus the stuck footer's (and
    // its 8px gap), measured, so the header's and the footer's banners shrink the pane instead
    // of hiding its bottom under the footer. A hidden tab measures 0 and is skipped.
    let footer = NodeRef::<html::Div>::new();
    let pane_room = RwSignal::new(None::<i32>);
    Effect::new(move |_| {
        let Some(footer) = footer.get() else {
            return;
        };
        let Some(page) = footer.closest(".page").ok().flatten() else {
            return;
        };
        let measure = {
            let (footer, page) = (footer.clone(), page.clone());
            move || {
                let height = page.client_height();
                if height > 0 {
                    pane_room.set(Some(height - footer.offset_height() - 8));
                }
            }
        };
        // The page is mounted once for the app's lifetime (tabs are hidden, not dropped), so the
        // observer lives as long as its targets and the closure is deliberately leaked, as the
        // theme's media-query listener is.
        let callback = Closure::<dyn FnMut()>::new(measure);
        let Ok(observer) = web_sys::ResizeObserver::new(callback.as_ref().unchecked_ref()) else {
            return;
        };
        observer.observe(&footer);
        observer.observe(&page);
        callback.forget();
    });

    view! {
        <div class="configure cfg-page" on:keydown=on_key>
            <div class="page-head">
                <div>
                    <h2>"zebrad configuration"</h2>
                    <p class="muted path">
                        {move || path().map(|path| view! { <code>{path}</code> })}
                    </p>
                    <p class="muted cfg-lead">
                        "Change a setting in the form and zebrad.toml follows, with the changed lines marked. The file can also be edited as text."
                    </p>
                </div>
            </div>
            {load_view}
            <div
                class="cfg-panes"
                hidden=move || !has_document()
                style=(
                    "--cfg-room",
                    move || pane_room.get().map_or(String::new(), |px| format!("{px}px")),
                )
            >
                <div class="cfg-pane-form">
                    <ZebradConfigForm editor />
                </div>
                <FilePane editor />
            </div>
            // Sticky at the page's bottom under both panes, so the outcome of a save is seen
            // from wherever Save was pressed.
            <div class="cfg-footer" hidden=move || !has_document() node_ref=footer>
                {notice_view}
                <div class="cfg-bar">
                    <div class="cfg-changes">
                        <span>{move || changes_text(change_count(), editor.dirty.get())}</span>
                        <span class="muted cfg-changed-list">{changes_detail}</span>
                    </div>
                    <p class="cfg-status ok" role="status">{saved_view}</p>
                    <div class="buttons">
                        <button
                            type="button"
                            disabled=move || editor.saving.get() || !editor.edited.get()
                            on:click=move |_| editor.discard()
                        >
                            "Discard"
                        </button>
                        <button
                            type="button"
                            class="primary"
                            disabled=move || !editor.can_save()
                            on:click=move |_| editor.save()
                        >
                            {move || {
                                if editor.saving.get() {
                                    "Saving…"
                                } else {
                                    save_label(editor.restarts())
                                }
                            }}
                        </button>
                    </div>
                </div>
            </div>
            <section class="config-reset">
                <ResetConfigButton disabled=reset_blocked />
            </section>
        </div>
    }
}

/// The file pane: the draft as numbered lines with the changed ones marked, or as a text
/// editor, with the count of changed lines and the switch between the two.
#[component]
fn FilePane(editor: ConfigEditor) -> impl IntoView {
    let body = NodeRef::<html::Div>::new();
    // The line last edited in the form, highlighted for a moment.
    let flash = RwSignal::new(None::<usize>);
    // Bumped by every flash, so the timer of an earlier one does not end a newer one.
    let flash_generation = StoredValue::new(0u64);
    let lines = Memo::new(move |_| {
        let saved = editor.document.with(|document| {
            document
                .as_ref()
                .map(|document| document.text.clone())
                .unwrap_or_default()
        });
        editor.draft.with(|draft| line_changes(&saved, draft))
    });
    let summary = move || {
        lines.with(|(lines, removed)| {
            change_summary(lines.iter().filter(|line| line.changed).count(), *removed)
        })
    };
    let path = move || {
        editor
            .document
            .with(|document| document.as_ref().map(|document| document.path.clone()))
    };

    // A form edit brings its key's line into view and flashes it. The lines are rendered by
    // the time the next frame is drawn.
    Effect::new(move |_| {
        let Some((section, key)) = editor.last_edited.get() else {
            return;
        };
        if editor.text_mode.get_untracked() {
            return;
        }
        let Some(number) = editor
            .draft
            .with_untracked(|draft| key_line(draft, section, key))
        else {
            return;
        };
        flash_generation.update_value(|generation| *generation += 1);
        let mine = flash_generation.get_value();
        flash.set(Some(number));
        set_timeout(
            move || {
                if flash_generation.get_value() == mine {
                    flash.set(None);
                }
            },
            FLASH_FOR,
        );
        request_animation_frame(move || reveal_line(body, number));
    });

    let toggle = move |_| editor.set_text_mode(!editor.text_mode.get_untracked());
    let body_view = move || {
        if editor.text_mode.get() {
            view! {
                <textarea
                    class="config-edit cfg-text"
                    spellcheck="false"
                    aria-label="zebrad.toml"
                    aria-invalid=move || editor.parsed.with(Result::is_err).then_some("true")
                    disabled=move || editor.saving.get()
                    prop:value=move || editor.draft.get()
                    on:input=move |event| editor.set_text(event_target_value(&event))
                ></textarea>
                // Next to the typing; the form pane says the same with `role="alert"`.
                {move || {
                    editor
                        .parsed
                        .with(|parsed| {
                            parsed
                                .as_ref()
                                .err()
                                .map(|error| view! { <pre class="cfg-text-error">{error.clone()}</pre> })
                        })
                }}
            }
            .into_any()
        } else {
            view! {
                <div class="cfg-lines">
                    {move || {
                        lines
                            .with(|(lines, _)| {
                                lines.iter().map(|line| line_view(line, flash)).collect_view()
                            })
                    }}
                </div>
            }
            .into_any()
        }
    };

    view! {
        <aside class="cfg-pane-file" aria-labelledby="cfg-file-title">
            <div class="cfg-file-head">
                <span class="cfg-file-title" id="cfg-file-title" title=path>
                    "zebrad.toml"
                </span>
                <span class="muted cfg-file-summary">{summary}</span>
                <button
                    type="button"
                    class="cfg-text-toggle"
                    aria-pressed=move || editor.text_mode.get().to_string()
                    disabled=move || editor.saving.get()
                    on:click=toggle
                >
                    {move || if editor.text_mode.get() { "Done editing" } else { "Edit as text" }}
                </button>
            </div>
            <div class="cfg-file-body" node_ref=body>
                {body_view}
            </div>
        </aside>
    }
}

/// The element id of a line of the file view.
fn line_id(number: usize) -> String {
    format!("cfg-line-{number}")
}

/// One line of the file view: the number, a mark on a changed line, and the highlighted text.
/// The view owns what it shows (`use<>`), so the lines can be rebuilt from a borrowed memo.
fn line_view(line: &LineChange, flash: RwSignal<Option<usize>>) -> impl IntoView + use<> {
    let number = line.number;
    let spans = highlight_toml(&line.text)
        .into_iter()
        .map(|(class, text)| view! { <span class=class.css()>{text}</span> })
        .collect_view();
    view! {
        <div
            class="cfg-line"
            id=line_id(number)
            class:cfg-line-changed=line.changed
            class:cfg-line-flash=move || flash.get() == Some(number)
        >
            <span class="cfg-line-no" aria-hidden="true">
                {number}
            </span>
            <span class="cfg-line-mark">
                {line
                    .changed
                    .then(|| {
                        view! {
                            <span aria-hidden="true">"+"</span>
                            <span class="cfg-sr-only">"changed: "</span>
                        }
                    })}
            </span>
            <span class="cfg-line-text">{spans}</span>
        </div>
    }
}

/// Scrolls the file pane the least that brings a line into view.
fn reveal_line(body: NodeRef<html::Div>, number: usize) {
    let Some(body) = body.get_untracked() else {
        return;
    };
    let Some(line) = document()
        .get_element_by_id(&line_id(number))
        .and_then(|line| line.dyn_into::<web_sys::HtmlElement>().ok())
    else {
        return;
    };
    if let Some(top) = scroll_to_reveal(
        line.offset_top(),
        line.offset_height(),
        body.scroll_top(),
        body.client_height(),
    ) {
        body.set_scroll_top(top);
    }
}
