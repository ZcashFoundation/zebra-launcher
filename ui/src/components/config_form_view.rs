//! The form pane of the Configure page: the filter, "Show only changed" and the sections with a
//! row and a tooltip per key, every row bound to one field of the [`ConfigEditor`] draft
//! (`crate::config_editor`). The rows follow the schema in `crate::config_form`; the page, the
//! file view and the action bar are `pages/configure.rs`.

use std::collections::{BTreeMap, BTreeSet};

use leptos::prelude::*;

use crate::{
    components::tooltip::Help,
    config_editor::ConfigEditor,
    config_form::{
        Control, FIELDS, Field, FieldId, FormState, Kind, SECTIONS, Section, display_default,
        fields_of, matches_filter, normalize,
    },
};

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

/// The banner over a disabled form while the draft is not TOML, pointing at the control that
/// is on screen: the text editor while it is open, else the switch that opens it or Discard.
fn invalid_toml_text(text_mode: bool) -> &'static str {
    if text_mode {
        "The file is not valid TOML. Fix it in the text editor."
    } else {
        "The file is not valid TOML. Open it with \"Edit as text\" to fix it, or Discard."
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
    // The remedy names the visible control: the text editor while it is open, else the switch
    // that opens it (a file on disk that is not TOML opens in the lines view).
    let invalid_view = move || {
        editor.parsed.with(|parsed| {
            parsed.as_ref().err().map(|error| {
                view! {
                    <div class="banner banner-error error-panel cfg-invalid" role="alert">
                        <strong>{move || invalid_toml_text(editor.text_mode.get())}</strong>
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
    use crate::config_editor::fixtures::*;

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
        assert!(invalid_toml_text(true).ends_with("Fix it in the text editor."));
        assert!(invalid_toml_text(false).contains("\"Edit as text\""));
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
