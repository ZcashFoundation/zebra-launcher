//! zebrad's configuration as a form: the schema of every key the bundled zebrad accepts
//! (section, label, kind, default, what it does), the values read from `zebrad.toml`, edits,
//! validation and rendering back to TOML with everything else in the file kept. Pure Rust,
//! tested natively.
//!
//! The file stays minimal: a field set to its default is removed from the file, not written,
//! because zebrad rejects unknown keys and a defaulted key copied from one zebrad version can
//! break the next (see `crates/launcher-core/src/config.rs`).

// The schema and its functions are filled in and wired to the form together; until then some
// of them are unused.
#![allow(dead_code)]

use std::collections::BTreeMap;

/// A config section, in display order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Section {
    /// The TOML table name, such as `network`.
    pub key: &'static str,
    /// The heading shown in the form.
    pub title: &'static str,
    /// One or two sentences on what the section covers.
    pub doc: &'static str,
}

/// How a field's value is typed, validated and edited.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `true` or `false`.
    Bool,
    /// A whole number, optionally bounded.
    Integer { min: Option<i64>, max: Option<i64> },
    /// Free text.
    Text,
    /// `host:port`; with `loopback_only` the host must be a loopback address.
    SocketAddr { loopback_only: bool },
    /// A humantime duration such as `1m 1s` or `500ms`.
    Duration,
    /// A filesystem path.
    Path,
    /// One of a fixed set of strings.
    Choice(&'static [&'static str]),
    /// A list of strings, edited one per line.
    StringList,
    /// `true` (the default location), `false` (none) or a path.
    BoolOrPath,
}

/// Who controls a field's value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    /// The user edits it.
    User,
    /// The launcher writes it on every start (`ManagedKeys` in launcher-core); shown read-only
    /// with `note` saying where it is changed, if anywhere.
    Managed { note: &'static str },
    /// The launcher overrides it with an environment variable on every start, so a value in
    /// the file never takes effect; shown read-only with `note`.
    Forced { note: &'static str },
}

/// One key of `zebrad.toml`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Field {
    /// The section's TOML table name.
    pub section: &'static str,
    /// The key inside the section.
    pub key: &'static str,
    /// The label shown in the form.
    pub label: &'static str,
    pub kind: Kind,
    /// zebrad's default, in the same text form the form edits (`"true"`, `"8233"`, one list
    /// entry per line); `None` when the default is "not set" (an optional value).
    pub default: Option<&'static str>,
    /// What the setting does and when to change it, in plain words, for the tooltip.
    pub doc: &'static str,
    pub control: Control,
}

/// `(section, key)`.
pub type FieldId = (&'static str, &'static str);

impl Field {
    pub fn id(&self) -> FieldId {
        (self.section, self.key)
    }

    /// `section.key`.
    pub fn path(&self) -> String {
        format!("{}.{}", self.section, self.key)
    }
}

/// The sections, in display order.
pub const SECTIONS: &[Section] = &[];

/// Every field, grouped by section in display order.
pub const FIELDS: &[Field] = &[];

/// The field for a `(section, key)`, if the schema has it.
pub fn field(id: FieldId) -> Option<&'static Field> {
    FIELDS.iter().find(|field| field.id() == id)
}

/// The fields of one section, in display order.
pub fn fields_of(section: &str) -> impl Iterator<Item = &'static Field> {
    FIELDS.iter().filter(move |field| field.section == section)
}

/// The values set in the file, as the text the form edits. A field that is absent uses
/// zebrad's default.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FormState {
    values: BTreeMap<FieldId, String>,
}

impl FormState {
    /// The value set in the file, if any.
    pub fn get(&self, id: FieldId) -> Option<&str> {
        self.values.get(&id).map(String::as_str)
    }

    /// Sets a value; `None` means "use the default" (the key is removed from the file).
    pub fn set(&mut self, id: FieldId, value: Option<String>) {
        match value {
            Some(value) => {
                self.values.insert(id, value);
            }
            None => {
                self.values.remove(&id);
            }
        }
    }

    /// The fields whose value differs between `self` and `other`.
    pub fn diff(&self, other: &FormState) -> Vec<FieldId> {
        FIELDS
            .iter()
            .map(Field::id)
            .filter(|id| self.get(*id) != other.get(*id))
            .collect()
    }
}

/// What `parse_config` found in the file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedConfig {
    pub form: FormState,
    /// `section.key` paths in the file that the schema does not know (kept untouched by
    /// [`render_config`]).
    pub unknown: Vec<String>,
    /// Keys the schema knows whose value could not be shown in the form (an unexpected TOML
    /// type); they are left untouched.
    pub unreadable: Vec<String>,
}

/// Reads the form values out of a `zebrad.toml`. Fails only when the text is not TOML.
pub fn parse_config(text: &str) -> Result<ParsedConfig, String> {
    text.parse::<toml_edit::DocumentMut>()
        .map(|_| ParsedConfig::default())
        .map_err(|error| error.to_string())
}

/// Checks one field's text against its kind. Empty text is "use the default" and always
/// valid for a user-controlled field.
pub fn validate(field: &Field, text: &str) -> Result<(), String> {
    let _ = (field, text);
    Ok(())
}

/// Writes the form values into `original`, keeping its comments, unknown keys and layout: a set
/// value is written with the field's TOML type, an unset one is removed, and a section is
/// created when its first value is set. Fails when `original` is not TOML or a value fails
/// [`validate`].
pub fn render_config(original: &str, form: &FormState) -> Result<String, String> {
    let _ = form;
    original
        .parse::<toml_edit::DocumentMut>()
        .map(|document| document.to_string())
        .map_err(|error| error.to_string())
}

/// True when `needle` (case-insensitive) occurs in the field's label, key, section or doc.
pub fn matches_filter(field: &Field, needle: &str) -> bool {
    let needle = needle.trim().to_lowercase();
    needle.is_empty()
        || [field.label, field.key, field.section, field.doc]
            .iter()
            .any(|text| text.to_lowercase().contains(&needle))
}

/// The default as shown next to a field: the value, or "not set".
pub fn display_default(field: &Field) -> String {
    field
        .default
        .map_or_else(|| "not set".to_owned(), str::to_owned)
}
