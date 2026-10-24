//! The zebrad configuration editor: one draft of `zebrad.toml` with two views on the Configure
//! page, the form on the left and the file on the right. [`ConfigEditor`] holds the draft and
//! the form's typing state and does the reads, saves and reloads; [`ZebradConfigForm`] is the
//! form pane (the filter, "Show only changed" and the sections with a row and a tooltip per
//! key); `pages/configure.rs` lays out the panes, the file view and the action bar.
//!
//! The draft text is what the file pane shows and what Save writes. The form's edits (`edits`)
//! hold every field's text as typed, where a text that is empty once normalised the way the
//! file is written (blank, or a list of blank lines) means "use the default"; [`normalised`]
//! strips those. Each form edit is rendered into the draft at once (`render_config`, so comments
//! and unknown keys stay), a field whose text is invalid keeping its last valid value in the
//! file; a text edit replaces the draft and the form reads its values from it again. Changes are
//! counted against the saved file, so an edit made as text counts too. The outcomes of a save
//! follow the backend's: a rollback or a conflict keeps the draft.

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
        Control, FIELDS, Field, FieldId, FormState, Kind, ParsedConfig, SECTIONS, Section,
        display_default, field_named, fields_of, matches_filter, normalize, parse_config,
        render_config, validate,
    },
};

/// How long the "Saved" message stays.
const SAVED_FOR: Duration = Duration::from_secs(6);

/// Whether the file was read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Load {
    Loading,
    Ready,
    /// The file could not be read. An earlier read may still be shown.
    Failed(String),
}

/// The message after a save, or after the file changed under the draft.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    Saved {
        restarted: bool,
    },
    RolledBack(String),
    /// The file was re-read because it changed on disk; `as_text` says whether the draft was
    /// being edited as text (and kept as it was) or in the form (and put back on top).
    Conflict {
        as_text: bool,
    },
    Failed(String),
}

/// What a reload does with the draft.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reload {
    /// The fresh file replaces the draft.
    Replace,
    /// The draft stays as it is; only the saved file (and its token) is refreshed: after a
    /// rollback, or when the draft is edited as text.
    KeepDraft,
    /// The changed fields are set again on top of the fresh file: after a conflict, or when the
    /// file is re-read while the form has edits.
    ReapplyPending,
}

/// `edits` with every text [`normalize`]d as the file is written, and the texts that are then
/// empty unset. Such a text means "use the default" ([`validate`] accepts it, [`render_config`]
/// removes the key), so it must not count as a value: a space typed into an unset field, or a
/// blank line added to a list, is not a change and must not restart zebrad for an unchanged
/// file.
fn normalised<'a>(fields: impl Iterator<Item = &'a Field>, edits: &FormState) -> FormState {
    let mut values = FormState::default();
    for field in fields {
        if let Some(text) = edits
            .get(field.id())
            .map(|text| normalize(field.kind, text))
            .filter(|text| !text.is_empty())
        {
            values.set(field.id(), Some(text));
        }
    }
    values
}

/// The message of every user-editable field whose edited text [`validate`] rejects. Locked
/// fields are never written ([`render_config`] skips them and the backend rewrites the managed
/// keys), and their rows show no error, so a value of theirs the form cannot read must not
/// block Save.
fn validation_errors<'a>(
    fields: impl Iterator<Item = &'a Field>,
    edits: &FormState,
) -> BTreeMap<FieldId, String> {
    fields
        .filter(|field| field.control == Control::User)
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

/// The user-editable fields whose value differs between two states. Locked fields are left
/// out: the draft never writes them, and the forbidden keys it drops are [`removed_on_save`].
fn user_diff(a: &FormState, b: &FormState) -> Vec<FieldId> {
    a.diff(b)
        .into_iter()
        .filter(|(section, key)| {
            field_named(section, key).is_some_and(|field| field.control == Control::User)
        })
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

/// The forbidden keys set in the file. [`render_config`] drops them, so a save removes them
/// whatever else is edited: that counts as a change and enables Save, but it is not an edit to
/// discard or to keep across a reload.
fn removed_on_save<'a>(
    fields: impl Iterator<Item = &'a Field>,
    loaded: &FormState,
) -> Vec<FieldId> {
    fields
        .filter(|field| matches!(field.control, Control::Forbidden { .. }))
        .map(Field::id)
        .filter(|id| loaded.get(*id).is_some())
        .collect()
}

/// The values to render into the draft: `values`, with every field whose text is invalid
/// holding the value the draft has (`current`) instead, so the file keeps the last valid one
/// while the user types.
fn render_values(
    values: &FormState,
    errors: &BTreeMap<FieldId, String>,
    current: &FormState,
) -> FormState {
    let mut values = values.clone();
    for id in errors.keys() {
        values.set(*id, current.get(*id).map(str::to_owned));
    }
    values
}

/// The list under the change count: the `section.key` paths of the edited fields, then the
/// keys a save removes.
pub fn changed_list(changes: &[FieldId], removals: &[FieldId]) -> String {
    changes
        .iter()
        .map(|(section, key)| format!("{section}.{key}"))
        .chain(
            removals
                .iter()
                .map(|(section, key)| format!("{section}.{key} (removed on save)")),
        )
        .collect::<Vec<_>>()
        .join(", ")
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

/// The "Default:" line under a control; a list's entries go on one line, comma-separated.
fn default_text(field: &Field) -> String {
    format!("Default: {}", display_default(field).replace('\n', ", "))
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

/// `No changes`, `1 change`, `3 changes`; `No setting changes` when only the text differs
/// from the saved file (a comment, or formatting).
pub fn changes_text(count: usize, text_differs: bool) -> String {
    match count {
        0 if text_differs => "No setting changes".to_owned(),
        0 => "No changes".to_owned(),
        1 => "1 change".to_owned(),
        count => format!("{count} changes"),
    }
}

/// The note under the count when the text differs from the saved file without a setting
/// changing.
pub fn text_only_note(count: usize, text_differs: bool) -> Option<&'static str> {
    (count == 0 && text_differs)
        .then_some("The text differs from the saved file: comments or formatting.")
}

/// True while saving restarts zebrad: it is starting, running, or about to be restarted after a
/// crash. Otherwise (stopped, stopping, failed) the backend only writes the file, and a stopping
/// zebrad is left stopped; this mirrors `was_running` in the `apply_config` command.
fn restarts_zebrad(phase: &Phase) -> bool {
    matches!(
        phase,
        Phase::Starting | Phase::Running | Phase::Backoff { .. }
    )
}

pub fn save_label(restarts: bool) -> &'static str {
    if restarts {
        "Save and restart zebrad"
    } else {
        "Save"
    }
}

pub fn saved_text(restarted: bool) -> &'static str {
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

/// The values of a config text, [`normalised`]; empty when the text is not TOML.
fn values_of(text: &str) -> FormState {
    parse_config(text)
        .map(|parsed| normalised(FIELDS.iter(), &parsed.form))
        .unwrap_or_default()
}

/// The draft of `zebrad.toml` and everything that reads or writes it. Every field is a signal
/// or a memo, so the struct is `Copy` and is shared by the form pane, the file pane and the
/// action bar.
#[derive(Clone, Copy)]
pub struct ConfigEditor {
    state: AppState,
    /// The file as last read, `None` until the first read succeeds.
    pub document: RwSignal<Option<ConfigDocument>>,
    pub load: RwSignal<Load>,
    /// The draft: the saved text with the form's edits rendered in, or the text as typed.
    pub draft: RwSignal<String>,
    /// The form's texts as typed.
    edits: RwSignal<FormState>,
    /// The draft read as a config, `Err` while it is not TOML.
    pub parsed: Memo<Result<ParsedConfig, String>>,
    /// The values the draft sets, normalised; those of the saved file while the draft is not
    /// TOML, so nothing counts as changed then.
    current: Memo<FormState>,
    /// The values the saved file sets, normalised like `values`, so that padding in the file
    /// does not count as a change.
    saved: Memo<FormState>,
    /// `edits` [`normalised`]: what the rows compare and what is rendered into the draft.
    values: Memo<FormState>,
    /// The message of every field whose text [`validate`] rejects.
    errors: Memo<BTreeMap<FieldId, String>>,
    /// The user fields whose value in the draft differs from the saved file.
    pub changes: Memo<Vec<FieldId>>,
    /// The forbidden keys in the saved file, which the draft drops.
    pub removals: Memo<Vec<FieldId>>,
    /// The draft differs from the saved file as text (a comment edit counts).
    pub dirty: Memo<bool>,
    /// Why the last form edit could not be written into the draft, if it could not.
    pub render_error: RwSignal<Option<String>>,
    pub saving: RwSignal<bool>,
    pub notice: RwSignal<Option<Notice>>,
    /// The file pane shows the draft as a text editor; the form follows the text.
    pub text_mode: RwSignal<bool>,
    /// The field last edited in the form, set on every edit, for the file pane to show its
    /// line.
    pub last_edited: RwSignal<Option<FieldId>>,
    /// Bumped by every notice, so the timer of an earlier "Saved" does not clear a newer one.
    notice_generation: StoredValue<u64>,
}

impl ConfigEditor {
    /// Creates the editor, reads the file, and keeps it re-read: when the file is replaced
    /// elsewhere (a reset, which bumps `config_changed`) and when the Configure tab is opened
    /// without pending changes.
    pub fn new() -> Self {
        let state = AppState::get();
        let document = RwSignal::new(None::<ConfigDocument>);
        let draft = RwSignal::new(String::new());
        let edits = RwSignal::new(FormState::default());
        let parsed = Memo::new(move |_| draft.with(|draft| parse_config(draft)));
        let saved = Memo::new(move |_| {
            document.with(|document| {
                document
                    .as_ref()
                    .map(|document| values_of(&document.text))
                    .unwrap_or_default()
            })
        });
        let current = Memo::new(move |_| {
            parsed.with(|parsed| match parsed {
                Ok(parsed) => normalised(FIELDS.iter(), &parsed.form),
                Err(_) => saved.get(),
            })
        });
        let values = Memo::new(move |_| edits.with(|edits| normalised(FIELDS.iter(), edits)));
        let errors =
            Memo::new(move |_| edits.with(|edits| validation_errors(FIELDS.iter(), edits)));
        let changes = Memo::new(move |_| {
            current.with(|current| saved.with(|saved| user_diff(current, saved)))
        });
        let removals =
            Memo::new(move |_| saved.with(|saved| removed_on_save(FIELDS.iter(), saved)));
        let dirty = Memo::new(move |_| {
            document.with(|document| {
                document
                    .as_ref()
                    .is_some_and(|document| draft.with(|draft| *draft != document.text))
            })
        });
        let editor = Self {
            state,
            document,
            load: RwSignal::new(Load::Loading),
            draft,
            edits,
            parsed,
            current,
            saved,
            values,
            errors,
            changes,
            removals,
            dirty,
            render_error: RwSignal::new(None),
            saving: RwSignal::new(false),
            notice: RwSignal::new(None),
            text_mode: RwSignal::new(false),
            last_edited: RwSignal::new(None),
            notice_generation: StoredValue::new(0),
        };

        spawn_local(editor.reload(Reload::Replace));

        // The file was replaced elsewhere (a reset from the update recovery panel, or the one
        // below the panes): re-read it, keeping a pending draft.
        Effect::new(move |previous: Option<u64>| {
            let changed = state.config_changed.get();
            if previous.is_some_and(|previous| previous != changed) {
                let (mode, notice) = editor.pending_reload();
                spawn_local(async move {
                    editor.reload(mode).await;
                    if let Some(notice) = notice {
                        editor.show_notice(notice);
                    }
                });
            }
            changed
        });
        // Opening the page re-reads the file while there is nothing to lose.
        Effect::new(move |previous: Option<Tab>| {
            let tab = state.tab.get();
            if tab == Tab::Configure
                && previous.is_some_and(|previous| previous != tab)
                && !editor.saving.get_untracked()
                && !editor.dirty.get_untracked()
                && editor.load.with_untracked(|load| *load == Load::Ready)
            {
                spawn_local(editor.reload(Reload::Replace));
            }
            tab
        });
        editor
    }

    /// How to re-read the file around the current draft, and what to tell the user: a draft
    /// edited as text is kept as it is, one edited in the form is put back on top of the fresh
    /// file, and no draft is replaced quietly.
    fn pending_reload(self) -> (Reload, Option<Notice>) {
        if !self.dirty.get_untracked() {
            return (Reload::Replace, None);
        }
        let as_text = self.text_mode.get_untracked();
        let mode = if as_text {
            Reload::KeepDraft
        } else {
            Reload::ReapplyPending
        };
        (mode, Some(Notice::Conflict { as_text }))
    }

    /// Reads the file again and updates the draft as `mode` says. A failure leaves the draft
    /// as it is and shows the error.
    async fn reload(self, mode: Reload) {
        // The panes stay enabled while the file is read (a reload is not a save), so an edit
        // made meanwhile must not be replaced by the fresh file.
        let draft_before = self.draft.get_untracked();
        let fresh = match api::read_config().await {
            Ok(fresh) => fresh,
            Err(error) => {
                self.load
                    .set(Load::Failed(format!("Could not read zebrad.toml: {error}")));
                return;
            }
        };
        let edited_meanwhile = self.draft.with_untracked(|draft| *draft != draft_before);
        let mode = if mode == Reload::Replace && edited_meanwhile {
            self.pending_reload().0
        } else {
            mode
        };
        match mode {
            Reload::Replace => {
                self.document.set(Some(fresh));
                self.reset_draft();
            }
            Reload::KeepDraft => self.document.set(Some(fresh)),
            Reload::ReapplyPending => {
                let pending = self.values.with_untracked(|values| {
                    self.saved.with_untracked(|saved| {
                        self.edits
                            .with_untracked(|edits| pending_edits(&user_diff(values, saved), edits))
                    })
                });
                self.document.set(Some(fresh));
                self.reset_draft();
                self.edits
                    .update(|edits| *edits = reapply(edits.clone(), pending));
                self.sync_draft();
            }
        }
        self.load.set(Load::Ready);
    }

    /// Makes the draft the saved file again, the form reading its values from it.
    fn reset_draft(self) {
        let text = self
            .document
            .with_untracked(|document| document.as_ref().map(|document| document.text.clone()))
            .unwrap_or_default();
        self.edits.set(values_of(&text));
        self.draft.set(text);
        self.render_error.set(None);
        // A forbidden key in the file is dropped at once, so the draft shows (and a save makes)
        // the file the launcher accepts.
        self.sync_draft();
    }

    /// Writes the form's values into the draft. Nothing happens while the draft is not TOML (the
    /// text is then authoritative); a field whose text is invalid keeps the draft's value.
    fn sync_draft(self) {
        let current = match self.parsed.get_untracked() {
            Ok(parsed) => normalised(FIELDS.iter(), &parsed.form),
            Err(_) => return,
        };
        let values = self.values.with_untracked(|values| {
            self.errors
                .with_untracked(|errors| render_values(values, errors, &current))
        });
        match self
            .draft
            .with_untracked(|draft| render_config(draft, &values))
        {
            Ok(text) => {
                if self.draft.with_untracked(|draft| *draft != text) {
                    self.draft.set(text);
                }
                self.render_error.set(None);
            }
            Err(error) => self.render_error.set(Some(error)),
        }
    }

    /// A form edit: `None` resets the field to its default.
    fn set_field(self, id: FieldId, text: Option<String>) {
        self.edits.update(|edits| edits.set(id, text));
        self.sync_draft();
        self.last_edited.set(Some(id));
    }

    /// A text edit: the text replaces the draft and the form follows it, where it is TOML.
    pub fn set_text(self, text: String) {
        self.draft.set(text);
        if let Ok(parsed) = self.parsed.get_untracked() {
            self.edits.set(normalised(FIELDS.iter(), &parsed.form));
            self.render_error.set(None);
        }
    }

    /// Shows the draft as text, or as marked lines again. Both ways the form's values are
    /// written in first, which drops a forbidden key typed as text.
    pub fn set_text_mode(self, on: bool) {
        self.sync_draft();
        self.text_mode.set(on);
    }

    /// True while saving restarts zebrad.
    pub fn restarts(self) -> bool {
        self.state
            .status
            .with(|status| restarts_zebrad(&status.phase))
    }

    pub fn can_save(self) -> bool {
        self.load.with(|load| *load == Load::Ready)
            && !self.saving.get()
            && self.dirty.get()
            && self.parsed.with(Result::is_ok)
            && self.errors.with(BTreeMap::is_empty)
            && self.render_error.with(Option::is_none)
    }

    /// Applies the draft. Applied: the file is re-read and becomes the draft. Rolled back: the
    /// draft stays, against the restored file. Conflict: the file is re-read and the draft kept
    /// (as text) or put back on top of it (in the form).
    pub fn save(self) {
        if !untrack(|| self.can_save()) {
            return;
        }
        self.sync_draft();
        let Some(token) = self
            .document
            .with_untracked(|document| document.as_ref().map(|document| document.token.clone()))
        else {
            return;
        };
        let text = self.draft.get_untracked();
        let restarted = untrack(|| self.restarts());
        self.saving.set(true);
        self.notice.set(None);
        spawn_local(async move {
            let next = match api::apply_config(text, token).await {
                Ok(ApplyOutcome::Applied) => {
                    self.reload(Reload::Replace).await;
                    Notice::Saved { restarted }
                }
                Ok(ApplyOutcome::RolledBack { error }) => {
                    self.reload(Reload::KeepDraft).await;
                    Notice::RolledBack(error)
                }
                Err(error) if error.kind == ApplyErrorKind::Conflict => {
                    let as_text = self.text_mode.get_untracked();
                    self.reload(if as_text {
                        Reload::KeepDraft
                    } else {
                        Reload::ReapplyPending
                    })
                    .await;
                    Notice::Conflict { as_text }
                }
                Err(error) => Notice::Failed(error.message),
            };
            self.saving.set(false);
            self.show_notice(next);
        });
    }

    /// Drops the draft for the saved file.
    pub fn discard(self) {
        self.reset_draft();
        self.notice.set(None);
    }

    /// Reads the file again after a failed read. The draft is kept across the failure (after a
    /// rollback or a conflict it was to be kept), so it goes back on top of the fresh file.
    pub fn retry(self) {
        if self.document.with_untracked(Option::is_none) {
            self.load.set(Load::Loading);
        }
        let (mode, _) = self.pending_reload();
        spawn_local(self.reload(mode));
    }

    fn show_notice(self, next: Notice) {
        self.notice_generation
            .update_value(|generation| *generation += 1);
        let mine = self.notice_generation.get_value();
        let transient = matches!(next, Notice::Saved { .. });
        self.notice.set(Some(next));
        if transient {
            set_timeout(
                move || {
                    if self.notice_generation.get_value() == mine {
                        self.notice.set(None);
                    }
                },
                SAVED_FOR,
            );
        }
    }
}

/// The form pane: the filter and "Show only changed", the notes on keys the form cannot show,
/// and every section with a row per key. Disabled while a save runs or the draft is not TOML.
#[component]
pub fn ZebradConfigForm(editor: ConfigEditor) -> impl IntoView {
    let filter = RwSignal::new(String::new());
    let only_changed = RwSignal::new(false);
    let shown = Memo::new(move |_| {
        filter.with(|filter| {
            editor.saved.with(|saved| {
                editor.values.with(|values| {
                    shown_fields(FIELDS.iter(), filter, only_changed.get(), saved, values)
                })
            })
        })
    });
    // The sections folded by the user; every section starts open.
    let closed = RwSignal::new(BTreeSet::<&'static str>::new());

    let notes_view = move || {
        editor.parsed.with(|parsed| {
            parsed.as_ref().ok().map(|parsed| {
                [
                    unknown_note(&parsed.unknown),
                    unreadable_note(&parsed.unreadable),
                ]
                .into_iter()
                .flatten()
                .map(|note| view! { <p class="muted cfg-notes">{note}</p> })
                .collect_view()
            })
        })
    };
    let invalid_view = move || {
        editor.parsed.with(|parsed| {
            parsed.as_ref().err().map(|error| {
                view! {
                    <div class="banner banner-error error-panel cfg-invalid" role="alert">
                        <strong>"The file is not valid TOML. Fix it in the text editor."</strong>
                        <pre>{error.clone()}</pre>
                    </div>
                }
            })
        })
    };
    // A field's own error shows on its row; this is for the rest (a section that is not a
    // table, say).
    let render_error_view = move || {
        editor
            .render_error
            .get()
            .filter(|_| editor.errors.with(BTreeMap::is_empty))
            .map(|error| {
                view! {
                    <div class="banner banner-error" role="alert">
                        "Could not write the setting into the file: "
                        {error}
                    </div>
                }
            })
    };
    let disabled = move || editor.saving.get() || editor.parsed.with(Result::is_err);
    let sections = SECTIONS
        .iter()
        .map(|section| section_view(section, editor, shown, closed))
        .collect_view();
    let nothing_shown = move || shown.with(BTreeSet::is_empty) && !FIELDS.is_empty();

    view! {
        <section class="cfg-form" aria-label="zebrad settings form">
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
            {notes_view}
            {invalid_view}
            {render_error_view}
            <fieldset class="cfg-fields" disabled=disabled>
                {sections}
                <p class="muted cfg-empty" hidden=move || !nothing_shown()>
                    "No setting matches"
                </p>
            </fieldset>
        </section>
    }
}

/// One section: a `<details>` with its fields in a grid, hidden when the filter leaves none of
/// them visible.
fn section_view(
    section: &'static Section,
    editor: ConfigEditor,
    shown: Memo<BTreeSet<FieldId>>,
    closed: RwSignal<BTreeSet<&'static str>>,
) -> impl IntoView {
    let key = section.key;
    let counts = move || {
        let set = editor.saved.with(|saved| {
            fields_of(key)
                .filter(|field| saved.get(field.id()).is_some())
                .count()
        });
        let edited = editor
            .changes
            .with(|changes| changes.iter().filter(|id| id.0 == key).count());
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
        .map(|field| field_row(field, editor, shown))
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
fn field_row(
    field: &'static Field,
    editor: ConfigEditor,
    shown: Memo<BTreeSet<FieldId>>,
) -> impl IntoView {
    let id = field.id();
    let input_id = format!("cfg-{}-{}", field.section, field.key);
    let hint_id = format!("{input_id}-hint");
    let raw = Signal::derive(move || editor.edits.with(|edits| edits.get(id).map(str::to_owned)));
    let in_file =
        Signal::derive(move || editor.saved.with(|saved| saved.get(id).map(str::to_owned)));
    let value = Signal::derive(move || {
        editor
            .values
            .with(|values| values.get(id).map(str::to_owned))
    });
    // The row is marked when the draft differs from the file; a forbidden key set in the file is
    // removed on save, so its row counts as changed.
    let edited = Signal::derive(move || match field.control {
        Control::Forbidden { .. } => in_file.get().is_some(),
        _ => editor
            .current
            .with(|current| current.get(id).map(str::to_owned) != in_file.get()),
    });
    let error = Signal::derive(move || editor.errors.with(|errors| errors.get(&id).cloned()));
    let set = move |text: Option<String>| editor.set_field(id, text);
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
            Signal::derive(move || in_file.get().unwrap_or_else(|| display_default(field))),
            "Managed by the launcher",
            note,
        )
        .into_any(),
        // The value the launcher forces, not the one in the file, which never takes effect.
        Control::Forced { note, value } => locked_control(
            field,
            input_id.clone(),
            hint_id,
            Signal::derive(move || value.unwrap_or("not set").to_owned()),
            "Set by the launcher",
            note,
        )
        .into_any(),
        Control::Forbidden { note } => locked_control(
            field,
            input_id.clone(),
            hint_id,
            Signal::derive(move || in_file.get().unwrap_or_else(|| "not set".to_owned())),
            "Not allowed by the launcher",
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
    // A cleared (or blank) text control means "use the default": when it loses focus the value
    // is unset and the control shows the default again.
    let unset_if_empty = move |_| {
        if raw.with(|raw| {
            raw.as_deref()
                .is_some_and(|raw| normalize(field.kind, raw).is_empty())
        }) {
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
        // A text input, not a number one: a number input's value is "" while it holds a
        // not-yet-valid number such as a lone "-", and writing that "" back as the controlled
        // value erases the minus sign as it is typed. `validate` checks the range instead.
        Kind::Integer { .. } => view! {
            <input
                type="text"
                inputmode="numeric"
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
            <span class="muted">{default_text(field)}</span>
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

/// A field the launcher controls: `shown` read-only, with a badge and the schema's note.
fn locked_control(
    field: &'static Field,
    input_id: String,
    hint_id: String,
    shown: Signal<String>,
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
                prop:value=move || shown.get()
            />
            <span class="cfg-badge">{badge}</span>
        </div>
        <div class="cfg-meta" id=hint_id>
            <span class="muted">{default_text(field)}</span>
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
    const BARS: Field = Field {
        section: "tracing",
        key: "progress_bar",
        label: "Progress bars",
        kind: Kind::Choice(&["summary", "detailed"]),
        default: None,
        doc: "Shows progress bars in the terminal.",
        control: Control::Forbidden { note: "" },
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
    fn normalising_treats_blank_texts_as_unset_and_trims_the_rest() {
        // A space typed into an unset field, or a list of blank lines, is not a value: the
        // writer drops it too, so counting it as a change would restart zebrad for nothing.
        let edits = with(&[
            (PORT.id(), " "),
            (ADDR.id(), " 127.0.0.1:8232 "),
            (PEERS.id(), "a\n\n b \n"),
        ]);
        let values = normalised([PORT, ADDR, PEERS].iter(), &edits);
        assert_eq!(values.get(PORT.id()), None);
        assert_eq!(values.get(ADDR.id()), Some("127.0.0.1:8232"));
        assert_eq!(values.get(PEERS.id()), Some("a\nb"));
        let blank_list = with(&[(PEERS.id(), "\n  \n")]);
        assert_eq!(
            normalised([PEERS].iter(), &blank_list).get(PEERS.id()),
            None
        );
        // A padded value in the file is the same value, so the loaded state is normalised too
        // and a set list with a trailing blank line added is not a change.
        let loaded = normalised([PEERS].iter(), &with(&[(PEERS.id(), " a\nb ")]));
        let edited = normalised([PEERS].iter(), &with(&[(PEERS.id(), "a\nb\n")]));
        assert!(edited.diff(&loaded).is_empty());
    }

    #[test]
    fn validation_errors_cover_set_user_fields_only() {
        let edits = with(&[(PORT.id(), "abc")]);
        let errors = validation_errors([PORT, ADDR, PEERS].iter(), &edits);
        assert_eq!(errors.keys().collect::<Vec<_>>(), vec![&PORT.id()]);
        // A locked field's value from the file (here a non-loopback address in a managed key,
        // as a hand edit can leave it) is not written, so it does not block Save.
        let edits = with(&[(PORT.id(), "abc"), (ADDR.id(), "0.0.0.0:8232")]);
        let errors = validation_errors([PORT, ADDR].iter(), &edits);
        assert!(errors.contains_key(&PORT.id()));
        assert!(!errors.contains_key(&ADDR.id()));
        assert!(
            validate(&ADDR, "0.0.0.0:8232").is_err(),
            "the test needs a rejected value"
        );
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
    fn forbidden_keys_in_the_file_are_removed_on_save() {
        // The file sets a forbidden key: the save drops it, which is listed as a change even
        // though the edits equal the file, and enables Save.
        let loaded = with(&[(BARS.id(), "summary"), (PORT.id(), "4")]);
        let fields = [PORT, ADDR, PEERS, BARS];
        assert_eq!(removed_on_save(fields.iter(), &loaded), vec![BARS.id()]);
        assert!(normalised(fields.iter(), &loaded).diff(&loaded).is_empty());
        assert_eq!(
            changed_list(&[], &[BARS.id()]),
            "tracing.progress_bar (removed on save)"
        );
        assert_eq!(
            changed_list(&[PORT.id()], &[BARS.id()]),
            "rpc.parallel_cpu_threads, tracing.progress_bar (removed on save)"
        );
        assert_eq!(changed_list(&[PORT.id()], &[]), "rpc.parallel_cpu_threads");
        // A file without the key, or a user field, has nothing to remove.
        let clean = with(&[(PORT.id(), "4")]);
        assert!(removed_on_save(fields.iter(), &clean).is_empty());
        assert_eq!(changed_list(&[], &[]), "");
    }

    #[test]
    fn user_diff_leaves_locked_fields_out() {
        let saved = with(&[
            (ADDR.id(), "127.0.0.1:8232"),
            (BARS.id(), "summary"),
            (PORT.id(), "4"),
        ]);
        // The draft drops the forbidden key, and a managed address differs: neither is the
        // user's change.
        let current = with(&[
            (ADDR.id(), "127.0.0.1:1"),
            (PORT.id(), "8"),
            (PEERS.id(), "a:1"),
        ]);
        assert_eq!(user_diff(&current, &saved), vec![PEERS.id(), PORT.id()]);
        assert!(user_diff(&saved, &saved).is_empty());
    }

    #[test]
    fn invalid_texts_keep_the_drafts_value() {
        let values = with(&[(PORT.id(), "abc"), (PEERS.id(), "a:1")]);
        let errors = validation_errors([PORT, PEERS].iter(), &values);
        assert_eq!(errors.keys().collect::<Vec<_>>(), vec![&PORT.id()]);
        let current = with(&[(PORT.id(), "4")]);
        let rendered = render_values(&values, &errors, &current);
        assert_eq!(rendered.get(PORT.id()), Some("4"));
        assert_eq!(rendered.get(PEERS.id()), Some("a:1"));
        // A field the draft does not set stays unset while its text is invalid.
        let rendered = render_values(&values, &errors, &FormState::default());
        assert_eq!(rendered.get(PORT.id()), None);
        assert_eq!(rendered.get(PEERS.id()), Some("a:1"));
    }

    #[test]
    fn values_of_a_text_are_normalised_or_empty() {
        let values = values_of("[rpc]\nparallel_cpu_threads = 4\n[network]\n");
        assert_eq!(values.get(PORT.id()), Some("4"));
        assert_eq!(values.get(PEERS.id()), None);
        assert_eq!(values_of("not = [toml"), FormState::default());
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
    fn default_line_puts_a_list_on_one_line() {
        assert_eq!(default_text(&PORT), "Default: 0");
        assert_eq!(default_text(&ADDR), "Default: not set");
        let list = Field {
            default: Some("a:8233\nb:8233"),
            ..PEERS
        };
        assert_eq!(default_text(&list), "Default: a:8233, b:8233");
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
        assert_eq!(changes_text(0, false), "No changes");
        assert_eq!(changes_text(0, true), "No setting changes");
        assert_eq!(changes_text(1, false), "1 change");
        assert_eq!(changes_text(3, true), "3 changes");
        assert!(text_only_note(0, true).is_some_and(|note| note.contains("comments")));
        assert_eq!(text_only_note(0, false), None);
        assert_eq!(text_only_note(2, true), None);
        assert_eq!(save_label(true), "Save and restart zebrad");
        assert_eq!(save_label(false), "Save");
        assert!(saved_text(true).contains("restarted"));
        assert_eq!(saved_text(false), "Saved.");
    }

    #[test]
    fn saving_restarts_unless_stopped_stopping_or_failed() {
        assert!(restarts_zebrad(&Phase::Running));
        assert!(restarts_zebrad(&Phase::Starting));
        assert!(restarts_zebrad(&Phase::Backoff {
            until: std::time::UNIX_EPOCH,
            crashes: 1,
        }));
        // The backend leaves a stopping zebrad stopped and only writes the file.
        assert!(!restarts_zebrad(&Phase::Stopping));
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
