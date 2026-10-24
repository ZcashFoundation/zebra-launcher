//! The zebrad configuration form in Settings: every key of `zebrad.toml` with a tooltip, read
//! from and written back to the file through `read_config` and `apply_config`.
//!
//! The file's values (`loaded`) and the user's edits (`edits`) are two [`FormState`]s. The
//! edits hold the text as typed, where an empty text means "use the default"; [`normalised`]
//! strips those, and the result is what Save writes and what counts as a change against the
//! file. Saving renders the edits into the loaded text (`render_config`, so comments and
//! unknown keys stay) and applies it; the outcomes follow the Configure page's handling, with
//! the pending edits kept across a rollback or a conflict.

use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

use launcher_types::{ApplyErrorKind, ApplyOutcome, ConfigDocument, Phase};
use leptos::{prelude::*, task::spawn_local};

use crate::{
    api,
    app::{AppState, Tab},
    components::tooltip::Help,
    config_form::{
        Control, FIELDS, Field, FieldId, FormState, Kind, SECTIONS, Section, display_default,
        fields_of, matches_filter, parse_config, render_config, validate,
    },
};

/// How long the "Saved" message stays.
const SAVED_FOR: Duration = Duration::from_secs(6);

/// Whether the file is shown.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Load {
    Loading,
    Ready,
    /// The file could not be read, or is not TOML.
    Failed(String),
}

/// The message after a save.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Notice {
    Saved { restarted: bool },
    RolledBack(String),
    Conflict,
    Failed(String),
}

/// What `parse_config` could not put in the form.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Notes {
    unknown: Vec<String>,
    unreadable: Vec<String>,
}

/// What a reload does with the user's edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reload {
    /// The fresh values replace the edits.
    Replace,
    /// The edits stay as they are (after a rollback: the file was restored).
    KeepEdits,
    /// The changed fields are set again on top of the fresh values (after a conflict).
    ReapplyPending,
}

/// The signals the field rows read and write.
#[derive(Clone, Copy)]
struct Form {
    /// The values in the file as last read.
    loaded: RwSignal<FormState>,
    /// The values as edited, text as typed.
    edits: RwSignal<FormState>,
    /// `edits` with empty texts unset: what Save writes and what counts as a change.
    values: Memo<FormState>,
    /// The message of every field whose text [`validate`] rejects.
    errors: Memo<BTreeMap<FieldId, String>>,
}

/// Everything a reload touches.
#[derive(Clone, Copy)]
struct Doc {
    document: RwSignal<Option<ConfigDocument>>,
    load: RwSignal<Load>,
    notes: RwSignal<Notes>,
    form: Form,
}

/// `edits` with empty texts unset. An empty text means "use the default" ([`validate`] accepts
/// it), so it must not reach [`render_config`] as a value.
fn normalised<'a>(fields: impl Iterator<Item = &'a Field>, edits: &FormState) -> FormState {
    let mut values = FormState::default();
    for field in fields {
        if let Some(text) = edits.get(field.id()).filter(|text| !text.is_empty()) {
            values.set(field.id(), Some(text.to_owned()));
        }
    }
    values
}

/// The message of every field whose edited text [`validate`] rejects.
fn validation_errors<'a>(
    fields: impl Iterator<Item = &'a Field>,
    edits: &FormState,
) -> BTreeMap<FieldId, String> {
    fields
        .filter_map(|field| {
            let text = edits.get(field.id())?;
            validate(field, text)
                .err()
                .map(|message| (field.id(), message))
        })
        .collect()
}

/// The fields a filter and the "only changed" toggle leave visible: those matching the filter
/// that are set in the file or edited, or all matching ones when the toggle is off.
fn shown_fields<'a>(
    fields: impl Iterator<Item = &'a Field>,
    filter: &str,
    only_changed: bool,
    loaded: &FormState,
    values: &FormState,
) -> BTreeSet<FieldId> {
    fields
        .filter(|field| {
            let id = field.id();
            matches_filter(field, filter)
                && (!only_changed || loaded.get(id).is_some() || values.get(id) != loaded.get(id))
        })
        .map(Field::id)
        .collect()
}

/// The changed fields with their edited text (`None`: reset to the default), to set again on
/// fresh values.
fn pending_edits(changes: &[FieldId], edits: &FormState) -> Vec<(FieldId, Option<String>)> {
    changes
        .iter()
        .map(|id| (*id, edits.get(*id).map(str::to_owned)))
        .collect()
}

/// `fresh` with `pending` set on top.
fn reapply(mut fresh: FormState, pending: Vec<(FieldId, Option<String>)>) -> FormState {
    for (id, text) in pending {
        fresh.set(id, text);
    }
    fresh
}

/// What a control shows: the edited text, or the default when unset.
fn shown_text(raw: Option<&str>, default: Option<&str>) -> String {
    raw.or(default).unwrap_or_default().to_owned()
}

/// The `<select>` option of a BoolOrPath field: `true` and `false` are the two fixed options;
/// any other text, the empty one of a just-picked "Custom folder…" included, is a custom path.
/// Unset with no default counts as the default location.
fn bool_or_path_choice(raw: Option<&str>, default: Option<&str>) -> &'static str {
    match raw.or(default) {
        None | Some("true") => "true",
        Some("false") => "false",
        Some(_) => "custom",
    }
}

/// The custom path of a BoolOrPath field, if its text is one.
fn custom_path(raw: Option<&str>) -> String {
    raw.filter(|raw| !matches!(*raw, "true" | "false"))
        .unwrap_or_default()
        .to_owned()
}

/// The rows of a list textarea: one per entry plus a spare one, between 3 and 6.
fn textarea_rows(text: &str) -> usize {
    (text.lines().count() + 1).clamp(3, 6)
}

/// `3 set, 2 edited` with the zero parts left out; empty when both are zero.
fn counts_text(set: usize, edited: usize) -> String {
    let mut parts = Vec::new();
    if set > 0 {
        parts.push(format!("{set} set"));
    }
    if edited > 0 {
        parts.push(format!("{edited} edited"));
    }
    parts.join(", ")
}

/// `No changes`, `1 change`, `3 changes`.
fn changes_text(count: usize) -> String {
    match count {
        0 => "No changes".to_owned(),
        1 => "1 change".to_owned(),
        count => format!("{count} changes"),
    }
}

/// True while saving restarts zebrad: in every phase but stopped and failed, where the backend
/// only writes the file.
fn restarts_zebrad(phase: &Phase) -> bool {
    !matches!(phase, Phase::Stopped | Phase::Failed { .. })
}

fn save_label(restarts: bool) -> &'static str {
    if restarts {
        "Save and restart zebrad"
    } else {
        "Save"
    }
}

fn saved_text(restarted: bool) -> &'static str {
    if restarted {
        "Saved. zebrad restarted with the new configuration."
    } else {
        "Saved."
    }
}

/// The note on keys the schema does not know, if any.
fn unknown_note(keys: &[String]) -> Option<String> {
    match keys {
        [] => None,
        [key] => Some(format!(
            "1 key in the file is not shown here and is kept as it is: {key}"
        )),
        keys => Some(format!(
            "{} keys in the file are not shown here and are kept as they are: {}",
            keys.len(),
            keys.join(", ")
        )),
    }
}

/// The note on known keys whose value the form cannot show, if any.
fn unreadable_note(keys: &[String]) -> Option<String> {
    match keys {
        [] => None,
        [key] => Some(format!(
            "1 key in the file has a value this form cannot show and is kept as it is: {key}"
        )),
        keys => Some(format!(
            "{} keys in the file have values this form cannot show and are kept as they are: {}",
            keys.len(),
            keys.join(", ")
        )),
    }
}

/// Reads the file again and updates the form as `mode` says. A failure leaves the values as
/// they are and shows the error in place of the fields.
async fn reload(doc: Doc, mode: Reload) {
    let fresh = match api::read_config().await {
        Ok(fresh) => fresh,
        Err(error) => {
            doc.load
                .set(Load::Failed(format!("Could not read zebrad.toml: {error}")));
            return;
        }
    };
    let parsed = match parse_config(&fresh.text) {
        Ok(parsed) => parsed,
        Err(error) => {
            doc.load.set(Load::Failed(format!(
                "zebrad.toml is not valid TOML ({error}). Edit it as text on the Configure page."
            )));
            return;
        }
    };
    let form = doc.form;
    match mode {
        Reload::Replace => form.edits.set(parsed.form.clone()),
        Reload::KeepEdits => {}
        Reload::ReapplyPending => {
            let pending = form.values.with_untracked(|values| {
                form.loaded.with_untracked(|loaded| {
                    form.edits
                        .with_untracked(|edits| pending_edits(&values.diff(loaded), edits))
                })
            });
            form.edits.set(reapply(parsed.form.clone(), pending));
        }
    }
    form.loaded.set(parsed.form);
    doc.notes.set(Notes {
        unknown: parsed.unknown,
        unreadable: parsed.unreadable,
    });
    doc.document.set(Some(fresh));
    doc.load.set(Load::Ready);
}

#[component]
pub fn ZebradConfigForm() -> impl IntoView {
    let state = AppState::get();
    let document = RwSignal::new(None::<ConfigDocument>);
    let load = RwSignal::new(Load::Loading);
    let notes = RwSignal::new(Notes::default());
    let loaded = RwSignal::new(FormState::default());
    let edits = RwSignal::new(FormState::default());
    let values = Memo::new(move |_| edits.with(|edits| normalised(FIELDS.iter(), edits)));
    let errors = Memo::new(move |_| edits.with(|edits| validation_errors(FIELDS.iter(), edits)));
    let form = Form {
        loaded,
        edits,
        values,
        errors,
    };
    let doc = Doc {
        document,
        load,
        notes,
        form,
    };
    let changes =
        Memo::new(move |_| values.with(|values| loaded.with(|loaded| values.diff(loaded))));
    let filter = RwSignal::new(String::new());
    let only_changed = RwSignal::new(false);
    let shown = Memo::new(move |_| {
        filter.with(|filter| {
            loaded.with(|loaded| {
                values.with(|values| {
                    shown_fields(FIELDS.iter(), filter, only_changed.get(), loaded, values)
                })
            })
        })
    });
    // The sections folded by the user; every section starts open.
    let closed = RwSignal::new(BTreeSet::<&'static str>::new());
    let saving = RwSignal::new(false);
    let notice = RwSignal::new(None::<Notice>);
    // Bumped by every notice, so the timer of an earlier "Saved" does not clear a newer one.
    let notice_generation = StoredValue::new(0u64);
    // The `config_changed` value this form's own save produced, which needs no second reload.
    let own_bump = StoredValue::new(None::<u64>);

    spawn_local(reload(doc, Reload::Replace));

    // The file was replaced elsewhere (a reset): re-read it, keeping pending edits on top.
    Effect::new(move |previous: Option<u64>| {
        let changed = state.config_changed.get();
        if previous.is_some_and(|previous| previous != changed)
            && own_bump.get_value() != Some(changed)
        {
            let mode = if changes.with_untracked(Vec::is_empty) {
                Reload::Replace
            } else {
                Reload::ReapplyPending
            };
            spawn_local(reload(doc, mode));
        }
        changed
    });
    // The Configure page's own save does not bump `config_changed`: re-read when the page is
    // opened, while there is nothing to lose.
    Effect::new(move |previous: Option<Tab>| {
        let tab = state.tab.get();
        if tab == Tab::Settings
            && previous.is_some_and(|previous| previous != tab)
            && !saving.get_untracked()
            && changes.with_untracked(Vec::is_empty)
            && load.with_untracked(|load| *load == Load::Ready)
        {
            spawn_local(reload(doc, Reload::Replace));
        }
        tab
    });

    let show_notice = move |next: Notice| {
        notice_generation.update_value(|generation| *generation += 1);
        let mine = notice_generation.get_value();
        let transient = matches!(next, Notice::Saved { .. });
        notice.set(Some(next));
        if transient {
            set_timeout(
                move || {
                    if notice_generation.get_value() == mine {
                        notice.set(None);
                    }
                },
                SAVED_FOR,
            );
        }
    };
    let restarts = move || state.status.with(|status| restarts_zebrad(&status.phase));
    let can_save = move || {
        load.with(|load| *load == Load::Ready)
            && !saving.get()
            && changes.with(|changes| !changes.is_empty())
            && errors.with(BTreeMap::is_empty)
    };
    let save = move || {
        if !untrack(can_save) {
            return;
        }
        let Some((original, token)) = document.with_untracked(|document| {
            document
                .as_ref()
                .map(|document| (document.text.clone(), document.token.clone()))
        }) else {
            return;
        };
        let text = match values.with_untracked(|values| render_config(&original, values)) {
            Ok(text) => text,
            Err(error) => {
                show_notice(Notice::Failed(format!(
                    "Could not write the configuration: {error}"
                )));
                return;
            }
        };
        let restarted = untrack(restarts);
        saving.set(true);
        notice.set(None);
        spawn_local(async move {
            let next = match api::apply_config(text, token).await {
                Ok(ApplyOutcome::Applied) => {
                    reload(doc, Reload::Replace).await;
                    let bump = state.config_changed.get_untracked() + 1;
                    own_bump.set_value(Some(bump));
                    state.config_changed.set(bump);
                    Notice::Saved { restarted }
                }
                Ok(ApplyOutcome::RolledBack { error }) => {
                    reload(doc, Reload::KeepEdits).await;
                    Notice::RolledBack(error)
                }
                Err(error) if error.kind == ApplyErrorKind::Conflict => {
                    reload(doc, Reload::ReapplyPending).await;
                    Notice::Conflict
                }
                Err(error) => Notice::Failed(error.message),
            };
            saving.set(false);
            show_notice(next);
        });
    };
    let discard = move |_| {
        edits.set(loaded.get_untracked());
        notice.set(None);
    };
    let retry = move |_| {
        if document.with_untracked(Option::is_none) {
            load.set(Load::Loading);
        }
        spawn_local(reload(doc, Reload::Replace));
    };
    // Ctrl (or Cmd) + S saves, from anywhere inside the form.
    let on_key = move |event: web_sys::KeyboardEvent| {
        if (event.ctrl_key() || event.meta_key())
            && !event.alt_key()
            && event.key().eq_ignore_ascii_case("s")
        {
            event.prevent_default();
            save();
        }
    };

    let ready = move || load.with(|load| *load == Load::Ready);
    let path =
        move || document.with(|document| document.as_ref().map(|document| document.path.clone()));
    let notes_view = move || {
        notes.with(|notes| {
            [
                unknown_note(&notes.unknown),
                unreadable_note(&notes.unreadable),
            ]
            .into_iter()
            .flatten()
            .map(|note| view! { <p class="muted cfg-notes">{note}</p> })
            .collect_view()
        })
    };
    let load_view = move || match load.get() {
        Load::Loading => view! { <p class="muted">"Loading zebrad.toml…"</p> }.into_any(),
        Load::Failed(message) => view! {
            <div class="banner banner-error" role="alert">
                <span>{message}</span>
                " "
                <button type="button" class="link" on:click=retry>
                    "Retry"
                </button>
            </div>
        }
        .into_any(),
        Load::Ready => ().into_any(),
    };
    let sections = SECTIONS
        .iter()
        .map(|section| section_view(section, form, shown, changes, closed))
        .collect_view();
    let nothing_shown = move || shown.with(BTreeSet::is_empty) && !FIELDS.is_empty();
    let notice_view = move || {
        notice.get().and_then(|notice| match notice {
            Notice::Saved { .. } => None,
            Notice::RolledBack(error) => Some(
                view! {
                    <div class="banner banner-error error-panel" role="alert">
                        <strong>
                            "zebrad rejected the new configuration and was restarted with the previous one."
                        </strong>
                        <p>"Your edits are kept below. zebrad said:"</p>
                        <pre>{error}</pre>
                    </div>
                }
                .into_any(),
            ),
            Notice::Conflict => Some(
                view! {
                    <div class="banner banner-warn" role="alert">
                        "The file changed outside this form (for example on the Configure page). Your edits were kept; check them and save again."
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
        notice.get().and_then(|notice| match notice {
            Notice::Saved { restarted } => Some(saved_text(restarted)),
            _ => None,
        })
    };
    let changed_paths = move || {
        changes.with(|changes| {
            changes
                .iter()
                .map(|(section, key)| format!("{section}.{key}"))
                .collect::<Vec<_>>()
                .join(", ")
        })
    };

    view! {
        <section class="cfg-form" aria-labelledby="cfg-heading" on:keydown=on_key>
            <div class="cfg-head">
                <div class="cfg-intro">
                    <h2 id="cfg-heading">"zebrad configuration"</h2>
                    <p class="muted">
                        "Settings for the node itself, written to zebrad.toml. Saving restarts zebrad. "
                        {move || path().map(|path| view! { <code>{path}</code> })}
                    </p>
                </div>
                <div class="cfg-tools">
                    <input
                        type="search"
                        class="cfg-filter"
                        aria-label="Filter settings"
                        placeholder="Filter settings"
                        prop:value=move || filter.get()
                        on:input=move |event| filter.set(event_target_value(&event))
                    />
                    <label class="cfg-toggle">
                        <input
                            type="checkbox"
                            prop:checked=move || only_changed.get()
                            on:change=move |event| only_changed.set(event_target_checked(&event))
                        />
                        "Show only changed"
                    </label>
                </div>
            </div>
            {notes_view}
            {load_view}
            <fieldset class="cfg-fields" disabled=move || saving.get() hidden=move || !ready()>
                {sections}
                <p class="muted cfg-empty" hidden=move || !nothing_shown()>
                    "No setting matches"
                </p>
            </fieldset>
            <div class="cfg-footer" hidden=move || !ready()>
                {notice_view}
                <div class="cfg-bar">
                    <div class="cfg-changes">
                        <span>{move || changes_text(changes.with(Vec::len))}</span>
                        <span class="muted cfg-changed-list">{changed_paths}</span>
                    </div>
                    <p class="cfg-status ok" role="status">{saved_view}</p>
                    <div class="buttons">
                        <button
                            type="button"
                            disabled=move || saving.get() || changes.with(Vec::is_empty)
                            on:click=discard
                        >
                            "Discard"
                        </button>
                        <button
                            type="button"
                            class="primary"
                            disabled=move || !can_save()
                            on:click=move |_| save()
                        >
                            {move || if saving.get() { "Saving…" } else { save_label(restarts()) }}
                        </button>
                    </div>
                </div>
            </div>
        </section>
    }
}

/// One section: a `<details>` with its fields in a grid, hidden when the filter leaves none of
/// them visible.
fn section_view(
    section: &'static Section,
    form: Form,
    shown: Memo<BTreeSet<FieldId>>,
    changes: Memo<Vec<FieldId>>,
    closed: RwSignal<BTreeSet<&'static str>>,
) -> impl IntoView {
    let key = section.key;
    let counts = move || {
        let set = form.loaded.with(|loaded| {
            fields_of(key)
                .filter(|field| loaded.get(field.id()).is_some())
                .count()
        });
        let edited = changes.with(|changes| changes.iter().filter(|id| id.0 == key).count());
        counts_text(set, edited)
    };
    let on_toggle = move |event: web_sys::Event| {
        let open = event_target::<web_sys::Element>(&event).has_attribute("open");
        closed.update(|closed| {
            if open {
                closed.remove(key);
            } else {
                closed.insert(key);
            }
        });
    };
    let rows = fields_of(key)
        .map(|field| field_row(field, form, shown))
        .collect_view();
    view! {
        <details
            class="cfg-section"
            open=move || !closed.with(|closed| closed.contains(key))
            hidden=move || shown.with(|shown| !shown.iter().any(|id| id.0 == key))
            on:toggle=on_toggle
        >
            <summary>
                <span class="cfg-section-title">{section.title}</span>
                <span class="muted cfg-section-count">{counts}</span>
                <span class="muted cfg-section-doc">{section.doc}</span>
            </summary>
            <div class="cfg-grid">{rows}</div>
        </details>
    }
}

/// One field: label, key, help, the control and its default, reset and validation line.
fn field_row(field: &'static Field, form: Form, shown: Memo<BTreeSet<FieldId>>) -> impl IntoView {
    let id = field.id();
    let input_id = format!("cfg-{}-{}", field.section, field.key);
    let hint_id = format!("{input_id}-hint");
    let raw = Signal::derive(move || form.edits.with(|edits| edits.get(id).map(str::to_owned)));
    let in_file =
        Signal::derive(move || form.loaded.with(|loaded| loaded.get(id).map(str::to_owned)));
    let value =
        Signal::derive(move || form.values.with(|values| values.get(id).map(str::to_owned)));
    let edited = Signal::derive(move || value.get() != in_file.get());
    let error = Signal::derive(move || form.errors.with(|errors| errors.get(&id).cloned()));
    let set = move |text: Option<String>| form.edits.update(|edits| edits.set(id, text));
    let user = matches!(field.control, Control::User);
    let control = match field.control {
        Control::User => user_control(
            field,
            input_id.clone(),
            hint_id,
            Values { raw, value, error },
            set,
        )
        .into_any(),
        Control::Managed { note } => locked_control(
            field,
            input_id.clone(),
            hint_id,
            in_file,
            "Managed by the launcher",
            note,
        )
        .into_any(),
        Control::Forced { note } => locked_control(
            field,
            input_id.clone(),
            hint_id,
            in_file,
            "Set by the launcher",
            note,
        )
        .into_any(),
    };
    view! {
        <div
            class="cfg-field"
            class:cfg-edited=move || edited.get()
            hidden=move || shown.with(|shown| !shown.contains(&id))
        >
            <div class="cfg-field-head">
                <label for=input_id.clone()>{field.label}</label>
                <Help id=format!("{input_id}-help") text=field.doc />
                {user
                    .then(|| {
                        view! {
                            <span class="cfg-badge" hidden=move || value.get().is_some()>
                                "default"
                            </span>
                            <span class="cfg-badge cfg-badge-edited" hidden=move || !edited.get()>
                                "edited"
                            </span>
                        }
                    })}
                <code class="muted cfg-key">{field.path()}</code>
            </div>
            {control}
        </div>
    }
}

/// A user field's signals.
#[derive(Clone, Copy)]
struct Values {
    /// The edited text, as typed.
    raw: Signal<Option<String>>,
    /// The edited text normalised: `None` when empty.
    value: Signal<Option<String>>,
    error: Signal<Option<String>>,
}

/// The control of a user-editable field by its kind, with the default, reset and validation
/// line under it.
fn user_control(
    field: &'static Field,
    input_id: String,
    hint_id: String,
    values: Values,
    set: impl Fn(Option<String>) + Copy + 'static,
) -> impl IntoView {
    let Values { raw, value, error } = values;
    let default = field.default;
    let shown = Signal::derive(move || shown_text(raw.get().as_deref(), default));
    let invalid = move || error.get().is_some();
    let on_text = move |event: web_sys::Event| set(Some(event_target_value(&event)));
    // A cleared text control means "use the default": when it loses focus the value is unset
    // and the control shows the default again.
    let unset_if_empty = move |_| {
        if raw.with(|raw| raw.as_deref() == Some("")) {
            set(None);
        }
    };
    let control = match field.kind {
        Kind::Bool => view! {
            <div class="cfg-check">
                <input
                    type="checkbox"
                    id=input_id
                    aria-describedby=hint_id.clone()
                    prop:checked=move || shown.get() == "true"
                    on:change=move |event| set(Some(event_target_checked(&event).to_string()))
                />
                <span class="cfg-mono">{move || shown.get()}</span>
            </div>
        }
        .into_any(),
        Kind::Integer { min, max } => view! {
            <input
                type="number"
                inputmode="numeric"
                id=input_id
                aria-describedby=hint_id.clone()
                min=min.map(|min| min.to_string())
                max=max.map(|max| max.to_string())
                class:invalid=invalid
                prop:value=move || shown.get()
                on:input=on_text
                on:blur=unset_if_empty
            />
        }
        .into_any(),
        Kind::Text => view! {
            <input
                type="text"
                id=input_id
                aria-describedby=hint_id.clone()
                class:invalid=invalid
                prop:value=move || shown.get()
                on:input=on_text
                on:blur=unset_if_empty
            />
        }
        .into_any(),
        Kind::Path => view! {
            <input
                type="text"
                class="cfg-mono"
                spellcheck="false"
                id=input_id
                aria-describedby=hint_id.clone()
                class:invalid=invalid
                prop:value=move || shown.get()
                on:input=on_text
                on:blur=unset_if_empty
            />
        }
        .into_any(),
        Kind::SocketAddr { .. } => view! {
            <input
                type="text"
                class="cfg-mono"
                spellcheck="false"
                placeholder="127.0.0.1:8232"
                id=input_id
                aria-describedby=hint_id.clone()
                class:invalid=invalid
                prop:value=move || shown.get()
                on:input=on_text
                on:blur=unset_if_empty
            />
        }
        .into_any(),
        Kind::Duration => view! {
            <input
                type="text"
                placeholder=default.unwrap_or("30s")
                id=input_id
                aria-describedby=hint_id.clone()
                class:invalid=invalid
                prop:value=move || shown.get()
                on:input=on_text
                on:blur=unset_if_empty
            />
        }
        .into_any(),
        Kind::Choice(options) => view! {
            <select
                id=input_id
                aria-describedby=hint_id.clone()
                class:invalid=invalid
                prop:value=move || raw.get().unwrap_or_default()
                on:change=move |event| {
                    let picked = event_target_value(&event);
                    set((!picked.is_empty()).then_some(picked));
                }
            >
                <option value="">{format!("Default ({})", display_default(field))}</option>
                {options
                    .iter()
                    .map(|option| view! { <option value=*option>{*option}</option> })
                    .collect_view()}
            </select>
        }
        .into_any(),
        Kind::StringList => view! {
            <textarea
                class="cfg-mono cfg-list"
                id=input_id
                aria-describedby=hint_id.clone()
                class:invalid=invalid
                rows=move || textarea_rows(&shown.get()).to_string()
                prop:value=move || shown.get()
                on:input=on_text
                on:blur=unset_if_empty
            ></textarea>
        }
        .into_any(),
        Kind::BoolOrPath => {
            let choice = Signal::derive(move || bool_or_path_choice(raw.get().as_deref(), default));
            view! {
                <select
                    id=input_id.clone()
                    aria-describedby=hint_id.clone()
                    prop:value=move || choice.get()
                    on:change=move |event| {
                        set(Some(match event_target_value(&event).as_str() {
                            "true" => "true".to_owned(),
                            "false" => "false".to_owned(),
                            _ => String::new(),
                        }))
                    }
                >
                    <option value="true">"Default location"</option>
                    <option value="false">"Off"</option>
                    <option value="custom">"Custom folder…"</option>
                </select>
                <input
                    type="text"
                    class="cfg-mono"
                    spellcheck="false"
                    id=format!("{input_id}-path")
                    aria-label=format!("{}: custom folder", field.label)
                    aria-describedby=hint_id.clone()
                    hidden=move || choice.get() != "custom"
                    class:invalid=invalid
                    prop:value=move || custom_path(raw.get().as_deref())
                    on:input=on_text
                />
            }
            .into_any()
        }
    };
    view! {
        {control}
        <div class="cfg-meta" id=hint_id>
            <span class="muted">{format!("Default: {}", display_default(field))}</span>
            {(field.kind == Kind::Duration)
                .then(|| view! { <span class="muted">"for example 30s, 5m, 1h"</span> })}
            <button
                type="button"
                class="link cfg-reset"
                hidden=move || value.get().is_none()
                on:click=move |_| set(None)
            >
                "Reset to default"
            </button>
            {move || error.get().map(|message| view! { <span class="error cfg-error">{message}</span> })}
        </div>
    }
}

/// A field the launcher controls: its value from the file, read-only, with a badge and the
/// schema's note.
fn locked_control(
    field: &'static Field,
    input_id: String,
    hint_id: String,
    in_file: Signal<Option<String>>,
    badge: &'static str,
    note: &'static str,
) -> impl IntoView {
    view! {
        <div class="cfg-locked">
            <input
                type="text"
                class="cfg-mono"
                id=input_id
                aria-describedby=hint_id.clone()
                readonly=true
                prop:value=move || in_file.get().unwrap_or_else(|| display_default(field))
            />
            <span class="cfg-badge">{badge}</span>
        </div>
        <div class="cfg-meta" id=hint_id>
            <span class="muted">{format!("Default: {}", display_default(field))}</span>
            {(!note.is_empty()).then(|| view! { <span class="muted">{note}</span> })}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PORT: Field = Field {
        section: "rpc",
        key: "parallel_cpu_threads",
        label: "Parallel CPU threads",
        kind: Kind::Integer {
            min: Some(0),
            max: None,
        },
        default: Some("0"),
        doc: "How many threads answer RPC requests.",
        control: Control::User,
    };
    const ADDR: Field = Field {
        section: "rpc",
        key: "listen_addr",
        label: "Listen address",
        kind: Kind::SocketAddr {
            loopback_only: true,
        },
        default: None,
        doc: "Where the RPC server listens.",
        control: Control::Managed { note: "" },
    };
    const PEERS: Field = Field {
        section: "network",
        key: "initial_mainnet_peers",
        label: "Initial Mainnet peers",
        kind: Kind::StringList,
        default: Some("dnsseed.z.cash:8233"),
        doc: "The seeders the node contacts first.",
        control: Control::User,
    };

    fn with(values: &[(FieldId, &str)]) -> FormState {
        let mut form = FormState::default();
        for (id, text) in values {
            form.set(*id, Some((*text).to_owned()));
        }
        form
    }

    #[test]
    fn normalising_unsets_empty_texts() {
        let edits = with(&[(PORT.id(), ""), (ADDR.id(), "127.0.0.1:8232")]);
        let values = normalised([PORT, ADDR, PEERS].iter(), &edits);
        assert_eq!(values.get(PORT.id()), None);
        assert_eq!(values.get(ADDR.id()), Some("127.0.0.1:8232"));
        assert_eq!(values.get(PEERS.id()), None);
    }

    #[test]
    fn validation_errors_cover_set_fields_only() {
        // The scaffold's `validate` accepts everything; the map still only holds set fields.
        let edits = with(&[(PORT.id(), "abc")]);
        let errors = validation_errors([PORT, ADDR].iter(), &edits);
        assert!(errors.len() <= 1);
        assert!(!errors.contains_key(&ADDR.id()));
    }

    #[test]
    fn filter_and_only_changed_pick_fields() {
        let fields = [PORT, ADDR, PEERS];
        let loaded = with(&[(ADDR.id(), "127.0.0.1:8232")]);
        let values = with(&[(ADDR.id(), "127.0.0.1:8232"), (PORT.id(), "4")]);
        let all = shown_fields(fields.iter(), "", false, &loaded, &values);
        assert_eq!(all.len(), 3);
        let changed = shown_fields(fields.iter(), "", true, &loaded, &values);
        assert_eq!(
            changed.into_iter().collect::<Vec<_>>(),
            vec![ADDR.id(), PORT.id()]
        );
        let filtered = shown_fields(fields.iter(), "PEER", false, &loaded, &values);
        assert_eq!(filtered.into_iter().collect::<Vec<_>>(), vec![PEERS.id()]);
        let both = shown_fields(fields.iter(), "peer", true, &loaded, &values);
        assert!(both.is_empty());
    }

    #[test]
    fn pending_edits_survive_a_fresh_file() {
        let edits = with(&[(PORT.id(), "4")]);
        let pending = pending_edits(&[PORT.id(), PEERS.id()], &edits);
        assert_eq!(
            pending,
            vec![(PORT.id(), Some("4".to_owned())), (PEERS.id(), None)]
        );
        let fresh = with(&[(PEERS.id(), "a\nb"), (ADDR.id(), "127.0.0.1:1")]);
        let merged = reapply(fresh, pending);
        assert_eq!(merged.get(PORT.id()), Some("4"));
        assert_eq!(merged.get(PEERS.id()), None);
        assert_eq!(merged.get(ADDR.id()), Some("127.0.0.1:1"));
    }

    #[test]
    fn controls_show_the_default_when_unset() {
        assert_eq!(shown_text(None, Some("0")), "0");
        assert_eq!(shown_text(Some(""), Some("0")), "");
        assert_eq!(shown_text(Some("8"), Some("0")), "8");
        assert_eq!(shown_text(None, None), "");
    }

    #[test]
    fn bool_or_path_options() {
        assert_eq!(bool_or_path_choice(None, Some("true")), "true");
        assert_eq!(bool_or_path_choice(None, None), "true");
        assert_eq!(bool_or_path_choice(Some("false"), Some("true")), "false");
        assert_eq!(bool_or_path_choice(Some("/tmp/x"), Some("true")), "custom");
        // "Custom folder…" was just picked and no path typed yet.
        assert_eq!(bool_or_path_choice(Some(""), Some("true")), "custom");
        assert_eq!(custom_path(Some("/tmp/x")), "/tmp/x");
        assert_eq!(custom_path(Some("true")), "");
        assert_eq!(custom_path(None), "");
    }

    #[test]
    fn textarea_grows_with_the_list() {
        assert_eq!(textarea_rows(""), 3);
        assert_eq!(textarea_rows("a\nb"), 3);
        assert_eq!(textarea_rows("a\nb\nc"), 4);
        assert_eq!(textarea_rows("a\nb\nc\nd\ne\nf\ng"), 6);
    }

    #[test]
    fn texts() {
        assert_eq!(counts_text(0, 0), "");
        assert_eq!(counts_text(3, 0), "3 set");
        assert_eq!(counts_text(0, 2), "2 edited");
        assert_eq!(counts_text(3, 2), "3 set, 2 edited");
        assert_eq!(changes_text(0), "No changes");
        assert_eq!(changes_text(1), "1 change");
        assert_eq!(changes_text(3), "3 changes");
        assert_eq!(save_label(true), "Save and restart zebrad");
        assert_eq!(save_label(false), "Save");
        assert!(saved_text(true).contains("restarted"));
        assert_eq!(saved_text(false), "Saved.");
    }

    #[test]
    fn saving_restarts_unless_stopped_or_failed() {
        assert!(restarts_zebrad(&Phase::Running));
        assert!(restarts_zebrad(&Phase::Starting));
        assert!(!restarts_zebrad(&Phase::Stopped));
        assert!(!restarts_zebrad(&Phase::Failed {
            class: launcher_types::ExitClass::ConfigError,
        }));
    }

    #[test]
    fn notes_name_the_keys() {
        assert_eq!(unknown_note(&[]), None);
        assert_eq!(
            unknown_note(&["mining.miner_address".to_owned()]),
            Some(
                "1 key in the file is not shown here and is kept as it is: mining.miner_address"
                    .to_owned()
            )
        );
        assert_eq!(
            unknown_note(&["a.b".to_owned(), "c.d".to_owned()]),
            Some(
                "2 keys in the file are not shown here and are kept as they are: a.b, c.d"
                    .to_owned()
            )
        );
        assert_eq!(unreadable_note(&[]), None);
        assert!(
            unreadable_note(&["a.b".to_owned()])
                .is_some_and(|note| note.starts_with("1 key") && note.ends_with("a.b"))
        );
        assert!(
            unreadable_note(&["a.b".to_owned(), "c.d".to_owned()])
                .is_some_and(|note| note.starts_with("2 keys"))
        );
    }
}
