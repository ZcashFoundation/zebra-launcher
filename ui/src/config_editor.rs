//! The zebrad configuration editor's model: one draft of `zebrad.toml`, shared by the three
//! parts of the Configure page. [`ConfigEditor`] holds the draft and the form's typing state
//! and does the reads, saves and reloads; the form pane is `components::config_form_view`, and
//! `pages/configure.rs` lays out the panes, the file view and the action bar. What each part
//! shows follows the model: the page shows [`Load`] and [`Notice`] (the outcome of a read, a
//! save or a reload), the form pane the draft's parse and render errors, and each row its own
//! field's validation error.
//!
//! The draft text is what the file pane shows and what Save writes. The form's edits (`edits`)
//! hold every field's text as typed, where a text that is empty once normalised the way the
//! file is written (blank, or a list of blank lines) means "use the default"; [`normalised`]
//! strips those. Each form edit is rendered into the draft at once (`render_config`, so comments
//! and unknown keys stay), a field whose text is invalid keeping its last valid value in the
//! file; a text edit replaces the draft and the form reads its values from it again. Changes are
//! counted against the saved file, so an edit made as text counts too. The outcomes of a save
//! follow the backend's: a rollback or a conflict keeps the draft.

use std::{collections::BTreeMap, time::Duration};

use launcher_types::{ApplyError, ApplyErrorKind, ApplyOutcome, ConfigDocument, Phase};
use leptos::{prelude::*, task::spawn_local};

use crate::{
    api,
    app::{AppState, Tab},
    config_form::{
        Control, FIELDS, Field, FieldId, FormState, ParsedConfig, field_named, normalize,
        parse_config, render_config, validate,
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
    /// The file was re-read because it changed on disk; `kept` says whether the draft was kept
    /// as it was (it is edited as text, or holds more than form values) or its field edits were
    /// put back on top of the fresh file.
    Conflict {
        kept: bool,
    },
    Failed(String),
}

/// What a reload does with the draft.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reload {
    /// The fresh file replaces the draft.
    Replace,
    /// The draft stays as it is; only the saved file (and its token) is refreshed: after a
    /// rollback, or when the draft holds anything a field edit cannot put back (it is edited as
    /// text, or carries a comment, an unknown key or other text edit).
    KeepDraft,
    /// The changed fields are set again on top of the fresh file: after a conflict, or when the
    /// file is re-read while the draft is the saved file with the form's edits and nothing more.
    ReapplyPending,
}

impl Reload {
    /// The reload of an edited draft: kept as it is, or its field edits put back.
    fn of_edited(kept: bool) -> Self {
        if kept {
            Self::KeepDraft
        } else {
            Self::ReapplyPending
        }
    }
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

pub fn saved_message(restarted: bool) -> &'static str {
    if restarted {
        "Saved. zebrad restarted with the new configuration."
    } else {
        "Saved."
    }
}

/// The values of a config text, [`normalised`]; empty when the text is not TOML.
fn values_of(text: &str) -> FormState {
    parse_config(text)
        .map(|parsed| normalised(FIELDS.iter(), &parsed.form))
        .unwrap_or_default()
}

/// The draft `reset_draft` makes of a saved text: the text with its forbidden keys dropped, or
/// the text itself when it is not TOML or cannot be rendered.
fn pristine_text(text: &str) -> String {
    render_config(text, &values_of(text)).unwrap_or_else(|_| text.to_owned())
}

/// True when `draft` is `saved` with `values` written in and nothing more (no comment, unknown
/// key or other text edit), so writing those values into a fresh file loses nothing.
fn draft_is_form_only(saved: &str, draft: &str, values: &FormState) -> bool {
    render_config(saved, values).is_ok_and(|rendered| rendered == draft)
}

/// The first forbidden key `current` (the draft's values) sets and `saved` (the file's) does
/// not: one typed as text, which the draft keeps and names rather than drops quietly.
fn typed_forbidden<'a>(
    fields: impl Iterator<Item = &'a Field>,
    saved: &FormState,
    current: &FormState,
) -> Option<FieldId> {
    removed_on_save(fields, current)
        .into_iter()
        .find(|id| saved.get(*id).is_none())
}

/// What a re-read does with the draft, and what it tells the user. A draft without edits is
/// replaced quietly. An edited one is kept as it is when a field edit cannot put it back
/// (`keeps_draft`, asked only then), else its field edits are set again on top of the fresh
/// file; both ways the user is told how it was treated, once the file is seen to differ.
fn reload_plan(edited: bool, keeps_draft: impl FnOnce() -> bool) -> (Reload, Option<Notice>) {
    if !edited {
        return (Reload::Replace, None);
    }
    let kept = keeps_draft();
    (Reload::of_edited(kept), Some(Notice::Conflict { kept }))
}

/// True when a re-read must keep the draft as it is rather than set its changed fields again
/// on the fresh file: it is edited as text, it is not TOML (`current` is `None`), or it is
/// more than the saved file with the form's values written in (a comment, an unknown key, a
/// forbidden key or another text edit). `values` and `errors` are the form's; a field whose
/// text is invalid holds the draft's value in the file, as `sync_draft` rendered it.
fn draft_must_be_kept(
    text_mode: bool,
    current: Option<&FormState>,
    saved: &str,
    draft: &str,
    values: &FormState,
    errors: &BTreeMap<FieldId, String>,
) -> bool {
    if text_mode {
        return true;
    }
    let Some(current) = current else {
        return true;
    };
    !draft_is_form_only(saved, draft, &render_values(values, errors, current))
}

/// The reload to do once the file is read, given the one asked for: a `Replace` would lose a
/// draft edited while the file was read, so it becomes the re-read `plan` for the edited draft
/// (asked only then); `KeepDraft` and `ReapplyPending` already keep the edit.
fn reload_override(
    requested: Reload,
    edited_meanwhile: bool,
    plan: impl FnOnce() -> Reload,
) -> Reload {
    if requested == Reload::Replace && edited_meanwhile {
        plan()
    } else {
        requested
    }
}

/// What a save's outcome means for the draft and the user. Applied: the fresh file replaces the
/// draft. Rolled back: the draft stays, against the restored file. Conflict: the file is re-read
/// around the draft as an edited draft's re-read is (`keeps_draft`, asked only then). Any other
/// failure leaves the file and the draft as they are. `restarted` is whether the save was to
/// restart zebrad.
fn after_apply(
    result: Result<ApplyOutcome, ApplyError>,
    restarted: bool,
    keeps_draft: impl FnOnce() -> bool,
) -> (Option<Reload>, Notice) {
    match result {
        Ok(ApplyOutcome::Applied) => (Some(Reload::Replace), Notice::Saved { restarted }),
        Ok(ApplyOutcome::RolledBack { error }) => {
            (Some(Reload::KeepDraft), Notice::RolledBack(error))
        }
        Err(error) if error.kind == ApplyErrorKind::Conflict => {
            let kept = keeps_draft();
            (Some(Reload::of_edited(kept)), Notice::Conflict { kept })
        }
        Err(error) => (None, Notice::Failed(error.message)),
    }
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
    pub edits: RwSignal<FormState>,
    /// The draft read as a config, `Err` while it is not TOML.
    pub parsed: Memo<Result<ParsedConfig, String>>,
    /// The values the draft sets, normalised; those of the saved file while the draft is not
    /// TOML, so nothing counts as changed then.
    pub current: Memo<FormState>,
    /// The values the saved file sets, normalised like `values`, so that padding in the file
    /// does not count as a change.
    pub saved: Memo<FormState>,
    /// `edits` [`normalised`]: what the rows compare and what is rendered into the draft.
    pub values: Memo<FormState>,
    /// The message of every field whose text [`validate`] rejects.
    pub errors: Memo<BTreeMap<FieldId, String>>,
    /// The user fields whose value in the draft differs from the saved file.
    pub changes: Memo<Vec<FieldId>>,
    /// The forbidden keys in the saved file, which the draft drops.
    pub removals: Memo<Vec<FieldId>>,
    /// The draft differs from the saved file as text (a comment edit counts): there is
    /// something to save.
    pub dirty: Memo<bool>,
    /// The draft holds something a reload or a reset would lose: it differs from what Discard
    /// gives. A forbidden key the draft drops is not an edit, so it does not count here.
    pub edited: Memo<bool>,
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
        let pristine = Memo::new(move |_| {
            document.with(|document| {
                document
                    .as_ref()
                    .map(|document| pristine_text(&document.text))
                    .unwrap_or_default()
            })
        });
        let edited = Memo::new(move |_| {
            document.with(Option::is_some)
                && draft.with(|draft| pristine.with(|pristine| *draft != *pristine))
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
            edited,
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
                spawn_local(editor.reload_pending());
            }
            changed
        });
        // Opening the page re-reads the file while there is nothing to lose.
        Effect::new(move |previous: Option<Tab>| {
            let tab = state.tab.get();
            if tab == Tab::Configure
                && previous.is_some_and(|previous| previous != tab)
                && !editor.saving.get_untracked()
                && !editor.edited.get_untracked()
                && editor.load.with_untracked(|load| *load == Load::Ready)
            {
                spawn_local(editor.reload(Reload::Replace));
            }
            tab
        });
        editor
    }

    /// The saved file's text, tracked; empty until the first read.
    pub fn saved_text(self) -> String {
        self.document
            .with(|document| document.as_ref().map(|document| document.text.clone()))
            .unwrap_or_default()
    }

    /// [`Self::saved_text`] without tracking, for the editor's own steps.
    fn saved_text_untracked(self) -> String {
        self.document
            .with_untracked(|document| document.as_ref().map(|document| document.text.clone()))
            .unwrap_or_default()
    }

    /// The saved file's compare-and-swap token, untracked; `None` until the first read.
    fn saved_token(self) -> Option<String> {
        self.document
            .with_untracked(|document| document.as_ref().map(|document| document.token.clone()))
    }

    /// The saved file's path, tracked, for display; `None` until the first read.
    pub fn path(self) -> Option<String> {
        self.document
            .with(|document| document.as_ref().map(|document| document.path.clone()))
    }

    /// How to re-read the file around the current draft, and what to tell the user:
    /// [`reload_plan`] over the draft's state.
    fn pending_reload(self) -> (Reload, Option<Notice>) {
        reload_plan(self.edited.get_untracked(), || self.keeps_draft())
    }

    /// True when a re-read must keep the draft as it is: [`draft_must_be_kept`] over the
    /// draft's state.
    fn keeps_draft(self) -> bool {
        let current = self.parsed.with_untracked(|parsed| {
            parsed
                .as_ref()
                .ok()
                .map(|parsed| normalised(FIELDS.iter(), &parsed.form))
        });
        let saved = self.saved_text_untracked();
        let draft = self.draft.get_untracked();
        self.values.with_untracked(|values| {
            self.errors.with_untracked(|errors| {
                draft_must_be_kept(
                    self.text_mode.get_untracked(),
                    current.as_ref(),
                    &saved,
                    &draft,
                    values,
                    errors,
                )
            })
        })
    }

    /// Re-reads the file around the draft as [`Self::pending_reload`] says and, when the file
    /// differs from the one the draft was edited against, says how the draft was treated. A
    /// failed read leaves the draft and shows the error instead.
    async fn reload_pending(self) {
        let (mode, notice) = self.pending_reload();
        let before = self.saved_text_untracked();
        self.reload(mode).await;
        if let Some(notice) = notice
            && self.saved_text_untracked() != before
            && self.load.with_untracked(|load| *load == Load::Ready)
        {
            self.show_notice(notice);
        }
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
        let mode = reload_override(mode, edited_meanwhile, || self.pending_reload().0);
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
        let text = self.saved_text_untracked();
        self.edits.set(values_of(&text));
        self.draft.set(text);
        self.render_error.set(None);
        // A forbidden key in the file is dropped at once, so the draft shows (and a save makes)
        // the file the launcher accepts.
        self.sync_draft();
    }

    /// Writes the form's values into the draft. Nothing happens while the draft is not TOML (the
    /// text is then authoritative); a field whose text is invalid keeps the draft's value. A
    /// forbidden key typed as text is kept and named, not dropped quietly: only the saved file's
    /// forbidden keys are removed on save (`removals`), and Save is refused until it is removed.
    fn sync_draft(self) {
        let current = match self.parsed.get_untracked() {
            Ok(parsed) => normalised(FIELDS.iter(), &parsed.form),
            Err(_) => return,
        };
        let typed = self
            .saved
            .with_untracked(|saved| typed_forbidden(FIELDS.iter(), saved, &current));
        if let Some((section, key)) = typed {
            self.render_error.set(Some(format!(
                "`{section}.{key}` is not allowed by the launcher; remove it from the file."
            )));
            return;
        }
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
    pub fn set_field(self, id: FieldId, text: Option<String>) {
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
    /// written in first; a forbidden key typed as text stays in the draft and is named in the
    /// form pane until it is removed.
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

    /// Applies the draft and treats the outcome as [`after_apply`] says.
    pub fn save(self) {
        if !untrack(|| self.can_save()) {
            return;
        }
        self.sync_draft();
        let Some(token) = self.saved_token() else {
            return;
        };
        let text = self.draft.get_untracked();
        let restarted = untrack(|| self.restarts());
        self.saving.set(true);
        self.notice.set(None);
        spawn_local(async move {
            let result = api::apply_config(text, token).await;
            let (reload, notice) = after_apply(result, restarted, || self.keeps_draft());
            if let Some(reload) = reload {
                self.reload(reload).await;
            }
            self.saving.set(false);
            self.show_notice(notice);
        });
    }

    /// Drops the draft for the saved file.
    pub fn discard(self) {
        self.reset_draft();
        self.notice.set(None);
    }

    /// Reads the file again after a failed read. The draft is kept across the failure (after a
    /// rollback or a conflict it was to be kept), so it is kept or put back on top of the fresh
    /// file as a re-read does, and the user is told when that file differs.
    pub fn retry(self) {
        if self.document.with_untracked(Option::is_none) {
            self.load.set(Load::Loading);
        }
        spawn_local(self.reload_pending());
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

/// The schema fields the editor's and the form's tests share.
#[cfg(test)]
pub(crate) mod fixtures {
    use crate::config_form::{Control, Field, FieldId, FormState, Kind};

    pub(crate) const PORT: Field = Field {
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
    pub(crate) const ADDR: Field = Field {
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
    pub(crate) const PEERS: Field = Field {
        section: "network",
        key: "initial_mainnet_peers",
        label: "Initial Mainnet peers",
        kind: Kind::StringList,
        default: Some("dnsseed.z.cash:8233"),
        doc: "The seeders the node contacts first.",
        control: Control::User,
    };
    pub(crate) const BARS: Field = Field {
        section: "tracing",
        key: "progress_bar",
        label: "Progress bars",
        kind: Kind::Choice(&["summary", "detailed"]),
        default: None,
        doc: "Shows progress bars in the terminal.",
        control: Control::Forbidden { note: "" },
    };

    pub(crate) fn with(values: &[(FieldId, &str)]) -> FormState {
        let mut form = FormState::default();
        for (id, text) in values {
            form.set(*id, Some((*text).to_owned()));
        }
        form
    }
}

#[cfg(test)]
mod tests {
    use super::{fixtures::*, *};

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
    fn the_pristine_text_drops_the_files_forbidden_keys_only() {
        // The real schema: `rpc.parallel_cpu_threads` is a user field, `tracing.progress_bar`
        // a forbidden one.
        let plain = "# mine\n\n[rpc]\nparallel_cpu_threads = 4\n";
        assert_eq!(pristine_text(plain), plain);
        let with_forbidden = format!("{plain}\n[tracing]\nprogress_bar = \"summary\"\n");
        assert_eq!(pristine_text(&with_forbidden), plain);
        // Not TOML: the text itself, so a broken file does not count as edited.
        assert_eq!(pristine_text("not = [toml"), "not = [toml");
    }

    #[test]
    fn a_draft_is_form_only_when_it_is_the_file_with_the_values_written_in() {
        let saved = "# Managed\n\n[rpc]\nparallel_cpu_threads = 4\n";
        let mut values = values_of(saved);
        assert!(draft_is_form_only(saved, saved, &values));
        values.set(PORT.id(), Some("8".to_owned()));
        let draft = render_config(saved, &values).expect("renders");
        assert!(draft.contains("parallel_cpu_threads = 8"), "{draft}");
        assert!(draft_is_form_only(saved, &draft, &values));
        // A comment, an unknown key or a forbidden key typed as text is more than the form's
        // values, which setting the changed fields again cannot put back.
        assert!(!draft_is_form_only(
            saved,
            &format!("# tuned for my box\n{draft}"),
            &values
        ));
        assert!(!draft_is_form_only(
            saved,
            &format!("{draft}\n[mining]\nminer_address = \"t1\"\n"),
            &values
        ));
        assert!(!draft_is_form_only(
            saved,
            &format!("{draft}\n[tracing]\nprogress_bar = \"summary\"\n"),
            &values
        ));
        // A forbidden key in the saved file is dropped by the draft and the rendering alike.
        let with_forbidden = format!("{saved}\n[tracing]\nprogress_bar = \"summary\"\n");
        assert!(draft_is_form_only(
            &with_forbidden,
            &pristine_text(&with_forbidden),
            &values_of(&with_forbidden)
        ));
        // A draft that is not TOML is never form-only.
        assert!(!draft_is_form_only(
            "not = [toml",
            "not = [toml",
            &FormState::default()
        ));
    }

    #[test]
    fn a_forbidden_key_typed_as_text_is_found() {
        let fields = [PORT, ADDR, PEERS, BARS];
        let saved = with(&[(PORT.id(), "4")]);
        let typed = with(&[(PORT.id(), "4"), (BARS.id(), "summary")]);
        assert_eq!(
            typed_forbidden(fields.iter(), &saved, &typed),
            Some(BARS.id())
        );
        // One the file already holds is removed on save, not typed.
        assert_eq!(typed_forbidden(fields.iter(), &typed, &typed), None);
        assert_eq!(typed_forbidden(fields.iter(), &saved, &saved), None);
    }

    #[test]
    fn texts() {
        assert_eq!(changes_text(0, false), "No changes");
        assert_eq!(changes_text(0, true), "No setting changes");
        assert_eq!(changes_text(1, false), "1 change");
        assert_eq!(changes_text(3, true), "3 changes");
        assert!(text_only_note(0, true).is_some_and(|note| note.contains("comments")));
        assert_eq!(text_only_note(0, false), None);
        assert_eq!(text_only_note(2, true), None);
        assert_eq!(save_label(true), "Save and restart zebrad");
        assert_eq!(save_label(false), "Save");
        assert!(saved_message(true).contains("restarted"));
        assert_eq!(saved_message(false), "Saved.");
    }

    /// `ApplyError` of a kind, with a message naming it.
    fn apply_error(kind: ApplyErrorKind) -> ApplyError {
        ApplyError {
            kind,
            message: format!("{kind:?}"),
        }
    }

    #[test]
    fn a_reread_replaces_an_unedited_draft_quietly_and_names_what_it_did_to_an_edited_one() {
        // Nothing to lose: the fresh file replaces the draft and nothing is said. Whether the
        // draft could be rebuilt is not even asked.
        assert_eq!(
            reload_plan(false, || unreachable!("asked for an unedited draft")),
            (Reload::Replace, None)
        );
        // Edited and rebuildable: the field edits go back on top of the fresh file.
        assert_eq!(
            reload_plan(true, || false),
            (
                Reload::ReapplyPending,
                Some(Notice::Conflict { kept: false })
            )
        );
        // Edited as text, or more than the form's values: kept as it is.
        assert_eq!(
            reload_plan(true, || true),
            (Reload::KeepDraft, Some(Notice::Conflict { kept: true }))
        );
    }

    #[test]
    fn a_draft_is_kept_unless_it_is_the_file_with_the_forms_values() {
        // The real schema: `rpc.parallel_cpu_threads` is a user field.
        let saved = "# Managed\n\n[rpc]\nparallel_cpu_threads = 4\n";
        let mut values = values_of(saved);
        values.set(PORT.id(), Some("8".to_owned()));
        let draft = render_config(saved, &values).expect("renders");
        let current = values_of(&draft);
        let no_errors = BTreeMap::new();
        // The file with the form's edits and nothing more: the edits can be set again.
        assert!(!draft_must_be_kept(
            false,
            Some(&current),
            saved,
            &draft,
            &values,
            &no_errors
        ));
        // Edited as text: kept whatever it holds.
        assert!(draft_must_be_kept(
            true,
            Some(&current),
            saved,
            &draft,
            &values,
            &no_errors
        ));
        // Not TOML: kept, there are no values to set again.
        assert!(draft_must_be_kept(
            false,
            None,
            saved,
            "not = [toml",
            &values,
            &no_errors
        ));
        // A comment typed as text is more than the form's values.
        let commented = format!("# tuned for my box\n{draft}");
        assert!(draft_must_be_kept(
            false,
            Some(&values_of(&commented)),
            saved,
            &commented,
            &values,
            &no_errors
        ));
        // A field whose text is invalid holds the draft's value in the file, so the draft is
        // still the file with the form's values: not kept, the edit is set again (and stays
        // invalid in the form).
        let mut typing = values.clone();
        typing.set(PORT.id(), Some("abc".to_owned()));
        let errors = validation_errors([PORT].iter(), &typing);
        assert!(
            errors.contains_key(&PORT.id()),
            "the test needs an invalid text"
        );
        assert!(!draft_must_be_kept(
            false,
            Some(&current),
            saved,
            &draft,
            &typing,
            &errors
        ));
    }

    #[test]
    fn a_reload_replaces_the_draft_only_when_nothing_was_typed_meanwhile() {
        // Nothing typed while the file was read: the reload asked for happens.
        assert_eq!(
            reload_override(Reload::Replace, false, || unreachable!("no plan needed")),
            Reload::Replace
        );
        // Typed meanwhile: a replace would lose it, so the edited draft's plan is used.
        assert_eq!(
            reload_override(Reload::Replace, true, || Reload::ReapplyPending),
            Reload::ReapplyPending
        );
        assert_eq!(
            reload_override(Reload::Replace, true, || Reload::KeepDraft),
            Reload::KeepDraft
        );
        // The reloads that keep the draft are not second-guessed.
        for requested in [Reload::KeepDraft, Reload::ReapplyPending] {
            assert_eq!(
                reload_override(requested, true, || unreachable!("already keeps the draft")),
                requested
            );
        }
    }

    #[test]
    fn a_saves_outcome_decides_the_reload_and_the_notice() {
        for restarted in [false, true] {
            assert_eq!(
                after_apply(Ok(ApplyOutcome::Applied), restarted, || unreachable!()),
                (Some(Reload::Replace), Notice::Saved { restarted })
            );
        }
        assert_eq!(
            after_apply(
                Ok(ApplyOutcome::RolledBack {
                    error: "bad port".to_owned()
                }),
                true,
                || unreachable!()
            ),
            (
                Some(Reload::KeepDraft),
                Notice::RolledBack("bad port".to_owned())
            )
        );
        // A conflict re-reads the file around the draft as an edited draft's re-read does.
        assert_eq!(
            after_apply(Err(apply_error(ApplyErrorKind::Conflict)), true, || true),
            (Some(Reload::KeepDraft), Notice::Conflict { kept: true })
        );
        assert_eq!(
            after_apply(Err(apply_error(ApplyErrorKind::Conflict)), true, || false),
            (
                Some(Reload::ReapplyPending),
                Notice::Conflict { kept: false }
            )
        );
        // Every other failure wrote nothing: no reload, the message as it is.
        for kind in [
            ApplyErrorKind::InvalidToml,
            ApplyErrorKind::Forbidden,
            ApplyErrorKind::ApplyInProgress,
            ApplyErrorKind::Io,
            ApplyErrorKind::Busy,
            ApplyErrorKind::Closed,
            ApplyErrorKind::Internal,
        ] {
            assert_eq!(
                after_apply(Err(apply_error(kind)), true, || unreachable!("{kind:?}")),
                (None, Notice::Failed(format!("{kind:?}")))
            );
        }
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
}
