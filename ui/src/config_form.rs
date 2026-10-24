//! zebrad's configuration as a form: the schema of every key the bundled zebrad accepts
//! (section, label, kind, default, what it does), the values read from `zebrad.toml`, edits,
//! validation and rendering back to TOML with everything else in the file kept. Pure Rust,
//! tested natively.
//!
//! The file stays minimal: a field set to its default is removed from the file, not written,
//! because zebrad rejects unknown keys and a defaulted key copied from one zebrad version can
//! break the next (see `crates/launcher-core/src/config.rs`).
//!
//! The schema itself lives in [`schema`]; it says which keys of the pinned zebrad are left out
//! and why.

mod schema;

use std::{
    collections::BTreeMap,
    net::{IpAddr, Ipv6Addr, SocketAddr},
};

use toml_edit::{Array, DocumentMut, Item, RawString, Table, Value, value};

pub use schema::{FIELDS, SECTIONS, SKIPPED_SECTIONS};

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
    /// `ip:port`. With `loopback_only` the IP must be a loopback address (`LOOPBACK_KEYS` in
    /// launcher-core require the port); without it the port may be left out, as zebrad then
    /// uses the network's default port.
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
    /// the file never takes effect; shown read-only as `value` (`None`: the launcher forces
    /// the key unset) with `note`.
    Forced {
        note: &'static str,
        value: Option<&'static str>,
    },
    /// The launcher refuses a file that sets it (`validate` in launcher-core), so a value found
    /// in the file is removed on save; shown read-only with `note`.
    Forbidden { note: &'static str },
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

/// The field for a `(section, key)`, if the schema has it.
#[cfg(test)]
pub fn field(id: FieldId) -> Option<&'static Field> {
    field_named(id.0, id.1)
}

/// The field for a section and key read from a file, if the schema has it.
pub fn field_named(section: &str, key: &str) -> Option<&'static Field> {
    FIELDS
        .iter()
        .find(|field| field.section == section && field.key == key)
}

/// The fields of one section, in display order.
pub fn fields_of(section: &str) -> impl Iterator<Item = &'static Field> {
    FIELDS.iter().filter(move |field| field.section == section)
}

/// The section a field belongs to.
#[cfg(test)]
pub fn section_of(field: &Field) -> Option<&'static Section> {
    SECTIONS.iter().find(|section| section.key == field.section)
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
    /// [`render_config`]). Keys of [`SKIPPED_SECTIONS`] are not listed: they are left out of
    /// the schema on purpose and kept untouched too.
    pub unknown: Vec<String>,
    /// Keys the schema knows whose value could not be shown in the form: an unexpected TOML
    /// type, or an empty value such as `[]` or `""`, which the form would read as "use the
    /// default". They are left untouched.
    pub unreadable: Vec<String>,
}

/// Reads the form values out of a `zebrad.toml`. Fails only when the text is not TOML.
///
/// Every key one level inside a top-level table is looked up in the schema; a nested table
/// counts as one key. Top-level entries that are not tables are ignored.
pub fn parse_config(text: &str) -> Result<ParsedConfig, String> {
    let doc = parse_document(text)?;
    let mut parsed = ParsedConfig::default();
    for (section, item) in doc.as_table().iter() {
        if SKIPPED_SECTIONS.contains(&section) {
            continue;
        }
        let Some(table) = item.as_table_like() else {
            continue;
        };
        for (key, item) in table.iter() {
            let path = format!("{section}.{key}");
            match field_named(section, key) {
                None => parsed.unknown.push(path),
                Some(field) => match item_text(field.kind, item) {
                    Some(text) => parsed.form.set(field.id(), Some(text)),
                    None => parsed.unreadable.push(path),
                },
            }
        }
    }
    parsed.unknown.sort();
    parsed.unreadable.sort();
    Ok(parsed)
}

fn parse_document(text: &str) -> Result<DocumentMut, String> {
    text.parse::<DocumentMut>()
        .map_err(|error| error.to_string())
}

/// The form text of a TOML item read as `kind`, or `None` when the item has another type or an
/// empty value (`[]`, `""`): empty text means "use the default" in the form, so such a value
/// cannot be shown and is left in the file untouched.
fn item_text(kind: Kind, item: &Item) -> Option<String> {
    let text = match kind {
        Kind::Bool => item.as_bool().map(|flag| flag.to_string()),
        Kind::Integer { .. } => item.as_integer().map(|number| number.to_string()),
        Kind::Text | Kind::Path | Kind::Choice(_) | Kind::SocketAddr { .. } | Kind::Duration => {
            item.as_str().map(str::to_owned)
        }
        Kind::StringList => item
            .as_array()
            .and_then(|array| {
                array
                    .iter()
                    .map(|entry| entry.as_str().map(str::to_owned))
                    .collect::<Option<Vec<_>>>()
            })
            .map(|entries| entries.join("\n")),
        Kind::BoolOrPath => item
            .as_bool()
            .map(|flag| flag.to_string())
            .or_else(|| item.as_str().map(str::to_owned)),
    };
    text.filter(|text| !normalize(kind, text).is_empty())
}

/// The text as it is written to the file: trimmed, and for a list one trimmed entry per line
/// with blank lines dropped. Empty means "use the default".
pub fn normalize(kind: Kind, text: &str) -> String {
    match kind {
        Kind::StringList => list_entries(text).collect::<Vec<_>>().join("\n"),
        _ => text.trim().to_owned(),
    }
}

/// The non-blank, trimmed lines of a list.
fn list_entries(text: &str) -> impl Iterator<Item = &str> {
    text.lines().map(str::trim).filter(|line| !line.is_empty())
}

/// Checks one field's text against its kind. Empty text is "use the default" and always
/// valid: a user-controlled field is then removed from the file, and managed or forced fields
/// are never written anyway.
pub fn validate(field: &Field, text_as_typed: &str) -> Result<(), String> {
    let normalized = normalize(field.kind, text_as_typed);
    if normalized.is_empty() {
        return Ok(());
    }
    let text = normalized.as_str();
    match field.kind {
        Kind::Bool => match text {
            "true" | "false" => Ok(()),
            _ => Err("Enter true or false.".to_owned()),
        },
        Kind::Integer { min, max } => validate_integer(text, min, max),
        // Any non-empty text is a path or free text.
        Kind::Text | Kind::Path | Kind::BoolOrPath => Ok(()),
        Kind::SocketAddr { loopback_only } => validate_socket_addr(text, loopback_only),
        Kind::Duration => validate_duration(text),
        Kind::Choice(choices) => {
            if choices.contains(&text) {
                Ok(())
            } else {
                Err(format!("Choose one of: {}.", choices.join(", ")))
            }
        }
        // The text as typed, so the line numbers in the message match the textarea rows.
        Kind::StringList => validate_list(field, text_as_typed),
    }
}

fn validate_integer(text: &str, min: Option<i64>, max: Option<i64>) -> Result<(), String> {
    let message = || match (min, max) {
        (Some(min), Some(max)) => format!("Enter a whole number from {min} to {max}."),
        (Some(min), None) => format!("Enter a whole number of at least {min}."),
        (None, Some(max)) => format!("Enter a whole number of at most {max}."),
        (None, None) => "Enter a whole number.".to_owned(),
    };
    let number: i64 = text.parse().map_err(|_| message())?;
    if min.is_some_and(|min| number < min) || max.is_some_and(|max| number > max) {
        return Err(message());
    }
    Ok(())
}

/// `ip:port`, or for a public address also a bare IP, which zebrad 7.0.0 completes with the
/// network's default port (it does not resolve host names here).
fn validate_socket_addr(text: &str, loopback_only: bool) -> Result<(), String> {
    let ip = match text.parse::<SocketAddr>() {
        Ok(addr) => addr.ip(),
        Err(_) if !loopback_only => ip_without_port(text).ok_or_else(|| {
            "Enter an IP address, with or without a port, such as 0.0.0.0:8233 or 0.0.0.0; \
             put an IPv6 address in brackets, such as [::]:8233."
                .to_owned()
        })?,
        Err(_) => {
            return Err(
                "Enter an IP address and port such as 127.0.0.1:8232; put an IPv6 address in \
                 brackets, such as [::1]:8232."
                    .to_owned(),
            );
        }
    };
    if loopback_only && !ip.is_loopback() {
        return Err(
            "Must be a loopback address such as 127.0.0.1:8232, so the node is not \
                    reachable from other machines."
                .to_owned(),
        );
    }
    Ok(())
}

/// An address without a port as zebrad accepts it: `0.0.0.0`, `::` or `[::]`.
fn ip_without_port(text: &str) -> Option<IpAddr> {
    text.parse::<IpAddr>().ok().or_else(|| {
        text.strip_prefix('[')?
            .strip_suffix(']')?
            .parse::<Ipv6Addr>()
            .ok()
            .map(IpAddr::V6)
    })
}

/// The units humantime 2.3.0 (pinned by zebrad 7.0.0) accepts.
const DURATION_UNITS: &[&str] = &[
    "ns", "nsec", "nanos", "us", "usec", "µs", "ms", "msec", "millis", "s", "sec", "secs",
    "second", "seconds", "m", "min", "mins", "minute", "minutes", "h", "hr", "hrs", "hour",
    "hours", "d", "day", "days", "w", "wk", "wks", "week", "weeks", "M", "month", "months", "y",
    "yr", "yrs", "year", "years",
];

/// A humantime 2.3.0 duration: `0`, or one or more `<number>[.<fraction>]<unit>` parts such as
/// `1m 1s`, `500ms` or `1.5h`.
fn validate_duration(text: &str) -> Result<(), String> {
    const MESSAGE: &str =
        "Enter a duration such as 1m 1s or 500ms, using the units ms, s, m, h or d.";
    if text == "0" {
        return Ok(());
    }
    let mut rest = text;
    let mut parts = 0;
    while !rest.is_empty() {
        let digits_end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        if rest[..digits_end].parse::<u64>().is_err() {
            return Err(MESSAGE.to_owned());
        }
        rest = &rest[digits_end..];
        // A fraction has at least one digit after the dot. humantime also refuses a fraction
        // that does not come to whole nanoseconds (such as `1.0000000001s`); that is left to
        // zebrad, which rejects the file when it starts.
        if let Some(fraction) = rest.strip_prefix('.') {
            let fraction_end = fraction
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(fraction.len());
            if fraction_end == 0 {
                return Err(MESSAGE.to_owned());
            }
            rest = &fraction[fraction_end..];
        }
        rest = rest.trim_start();
        // `is_alphabetic`, not `is_ascii_alphabetic`, so `µs` reaches the lookup.
        let unit_end = rest
            .find(|c: char| !c.is_alphabetic())
            .unwrap_or(rest.len());
        if !DURATION_UNITS.contains(&&rest[..unit_end]) {
            return Err(MESSAGE.to_owned());
        }
        rest = rest[unit_end..].trim_start();
        parts += 1;
    }
    if parts == 0 {
        return Err(MESSAGE.to_owned());
    }
    Ok(())
}

/// A peer list (`initial_*_peers`) holds `host:port` entries; other lists take any text. Blank
/// lines are skipped but counted, so "Line N" is the row of the text as typed.
fn validate_list(field: &Field, text: &str) -> Result<(), String> {
    if !field.key.ends_with("_peers") {
        return Ok(());
    }
    for (index, line) in text.lines().enumerate() {
        let entry = line.trim();
        if !entry.is_empty() && !is_host_port(entry) {
            return Err(format!(
                "Line {}: enter a host name or IP address with a port, such as \
                 dnsseed.z.cash:8233.",
                index + 1
            ));
        }
    }
    Ok(())
}

/// `ip:port` (IPv6 in brackets) or `hostname:port`.
fn is_host_port(entry: &str) -> bool {
    if entry.parse::<SocketAddr>().is_ok() {
        return true;
    }
    let Some((host, port)) = entry.rsplit_once(':') else {
        return false;
    };
    port.parse::<u16>().is_ok() && is_host_name(host)
}

/// Dot-separated labels of letters, digits and inner hyphens.
fn is_host_name(host: &str) -> bool {
    !host.is_empty()
        && host.split('.').all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
}

/// Writes the form values into `original`, keeping its comments, unknown keys and layout: a set
/// value is written with the field's TOML type, an unset one is removed, and a section is
/// created when its first value is set. Fails when `original` is not TOML or a value that
/// differs from the file's fails [`validate`].
///
/// Only [`Control::User`] fields are written; managed and forced fields, unknown keys and
/// values of an unexpected type are left as they are. A value equal to the one in the file is
/// not rewritten (nor checked), so rendering a [`parse_config`] result gives back the original
/// text. A section emptied by a removal is dropped too, unless a comment is attached to its
/// header.
pub fn render_config(original: &str, form: &FormState) -> Result<String, String> {
    let mut doc = parse_document(original)?;
    for field in FIELDS {
        match field.control {
            Control::User => {}
            // A key zebrad.toml must not carry, whatever its value: dropped so the launcher
            // accepts the file.
            Control::Forbidden { .. } => {
                remove_key(&mut doc, field);
                continue;
            }
            Control::Managed { .. } | Control::Forced { .. } => continue,
        }
        let text = form
            .get(field.id())
            .map(|text| normalize(field.kind, text))
            .filter(|text| !text.is_empty());
        match text {
            Some(text) => set_value(&mut doc, field, &text)?,
            None => remove_value(&mut doc, field),
        }
    }
    Ok(doc.to_string())
}

fn set_value(doc: &mut DocumentMut, field: &Field, text: &str) -> Result<(), String> {
    let root = doc.as_table_mut();
    if !root.contains_key(field.section) {
        let mut table = Table::new();
        table.set_implicit(false);
        root.insert(field.section, Item::Table(table));
    }
    let table = root
        .get_mut(field.section)
        .and_then(Item::as_table_like_mut)
        .ok_or_else(|| format!("`{}` in zebrad.toml is not a table.", field.section))?;
    let existing = table.get(field.key);
    let unchanged = existing
        .and_then(|item| item_text(field.kind, item))
        .is_some_and(|current| normalize(field.kind, &current) == text);
    if unchanged {
        return Ok(());
    }
    // A value the file already holds is left as it is, whatever `validate` says of it: a
    // hand-edited file can carry one the form rejects, and that must not block the other
    // fields. Only a new value is checked.
    validate(field, text).map_err(|message| format!("{}: {message}", field.path()))?;
    // Keep the spacing and any comment after the old value.
    let decor = existing
        .and_then(Item::as_value)
        .map(|old| old.decor().clone());
    let mut item = typed_item(field.kind, text)?;
    if let (Some(decor), Some(new)) = (decor, item.as_value_mut()) {
        *new.decor_mut() = decor;
    }
    table.insert(field.key, item);
    Ok(())
}

/// Removes a readable value and, when that empties a section with no comment on its header,
/// the section. A value the form could not show (see [`item_text`]) is left alone: the user
/// never saw it, so unsetting the field is not a request to drop it.
fn remove_value(doc: &mut DocumentMut, field: &Field) {
    let readable = doc
        .as_table()
        .get(field.section)
        .and_then(Item::as_table_like)
        .and_then(|table| table.get(field.key))
        .is_some_and(|item| item_text(field.kind, item).is_some());
    if readable {
        remove_key(doc, field);
    }
}

/// Removes a key whatever its value and, when that empties a section with no comment on its
/// header, the section.
fn remove_key(doc: &mut DocumentMut, field: &Field) {
    let root = doc.as_table_mut();
    let Some(item) = root.get_mut(field.section) else {
        return;
    };
    let Some(table) = item.as_table_like_mut() else {
        return;
    };
    if table.remove(field.key).is_none() {
        return;
    }
    let emptied = table.is_empty();
    if emptied && !has_comment(item) {
        remove_section(root, field.section);
    }
}

/// Removes a top-level table. The blank lines before it replace those before the table that
/// follows, so removing the first section does not leave the file starting with blank lines;
/// a comment on the following header stays.
fn remove_section(root: &mut Table, section: &str) {
    let tables: Vec<String> = root
        .iter()
        .filter(|(_, item)| item.is_table())
        .map(|(key, _)| key.to_owned())
        .collect();
    let next = tables
        .iter()
        .position(|key| key == section)
        .and_then(|index| tables.get(index + 1));
    // Blank lines only: a table with a comment on its header is never removed.
    let blank = root.remove(section).and_then(|item| {
        item.as_table().and_then(|table| {
            table
                .decor()
                .prefix()
                .and_then(RawString::as_str)
                .map(str::to_owned)
        })
    });
    if let (Some(next), Some(blank)) = (next, blank)
        && let Some(table) = root.get_mut(next).and_then(Item::as_table_mut)
        && let Some(kept) = table.decor().prefix().map_or(Some(""), RawString::as_str)
    {
        let prefix = format!("{blank}{}", kept.trim_start());
        table.decor_mut().set_prefix(prefix);
    }
}

/// True when a comment is attached to a table's header.
fn has_comment(item: &Item) -> bool {
    item.as_table().is_some_and(|table| {
        table
            .decor()
            .prefix()
            .is_some_and(|prefix| prefix.as_str().is_none_or(|prefix| prefix.contains('#')))
    })
}

/// The TOML item for a validated, normalized form text.
fn typed_item(kind: Kind, text: &str) -> Result<Item, String> {
    Ok(match kind {
        Kind::Bool => value(text == "true"),
        Kind::Integer { .. } => value(text.parse::<i64>().map_err(|error| error.to_string())?),
        Kind::StringList => Item::Value(Value::Array(list_array(text.lines()))),
        Kind::BoolOrPath => match text {
            "true" => value(true),
            "false" => value(false),
            path => value(path),
        },
        Kind::Text | Kind::Path | Kind::Choice(_) | Kind::SocketAddr { .. } | Kind::Duration => {
            value(text)
        }
    })
}

/// A list formatted like `zebrad generate` writes it: one entry per line, or `[]` when empty.
fn list_array<'a>(entries: impl Iterator<Item = &'a str>) -> Array {
    let mut array = Array::new();
    for entry in entries {
        array.push_formatted(Value::from(entry).decorated("\n    ", ""));
    }
    if !array.is_empty() {
        array.set_trailing("\n");
        array.set_trailing_comma(true);
    }
    array
}

/// True when `needle` (case-insensitive) occurs in the field's label, key, `section.key` path
/// or doc.
pub fn matches_filter(field: &Field, needle: &str) -> bool {
    let needle = needle.trim().to_lowercase();
    needle.is_empty()
        || [field.label, field.key, field.doc]
            .iter()
            .any(|text| text.to_lowercase().contains(&needle))
        || field.path().to_lowercase().contains(&needle)
}

/// The default as shown next to a field: the value (list entries separated by commas), or
/// "not set".
pub fn display_default(field: &Field) -> String {
    match (field.default, field.kind) {
        (None, _) => "not set".to_owned(),
        (Some(default), Kind::StringList) => default.lines().collect::<Vec<_>>().join(", "),
        (Some(default), _) => default.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    /// `zebrad generate` output at the bundled version: every non-`None` default.
    const FIXTURE: &str =
        include_str!("../../crates/launcher-core/tests/fixtures/zebrad-7.0.0-generate.toml");

    /// Keys whose generate default is a per-user path, so the schema default is `None`.
    const PER_USER_PATHS: [FieldId; 2] = [("rpc", "cookie_dir"), ("state", "cache_dir")];

    fn fixture() -> DocumentMut {
        FIXTURE.parse().expect("the fixture is TOML")
    }

    fn get(id: FieldId) -> &'static Field {
        field(id).unwrap_or_else(|| panic!("{}.{} is in the schema", id.0, id.1))
    }

    fn form_with(id: FieldId, text: &str) -> FormState {
        let mut form = FormState::default();
        form.set(id, Some(text.to_owned()));
        form
    }

    // Schema integrity.

    #[test]
    fn fixture_keys_are_in_the_schema_with_matching_defaults() {
        let mut seen = 0;
        for (section, item) in fixture().as_table().iter() {
            if SKIPPED_SECTIONS.contains(&section) {
                continue;
            }
            let table = item.as_table().expect("fixture sections are tables");
            for (key, item) in table.iter() {
                let field = field_named(section, key)
                    .unwrap_or_else(|| panic!("{section}.{key} is missing from the schema"));
                let text = item_text(field.kind, item)
                    .unwrap_or_else(|| panic!("{section}.{key} has another type in the fixture"));
                if PER_USER_PATHS.contains(&field.id()) {
                    assert_eq!(field.default, None, "{section}.{key}");
                } else {
                    assert_eq!(field.default, Some(text.as_str()), "{section}.{key}");
                }
                seen += 1;
            }
        }
        assert!(seen >= 35, "{seen} fixture keys checked");
    }

    #[test]
    fn defaults_match_the_fixture_presence() {
        let doc = fixture();
        for field in FIELDS {
            let in_fixture = doc
                .as_table()
                .get(field.section)
                .and_then(Item::as_table)
                .is_some_and(|table| table.contains_key(field.key));
            let path = field.path();
            match field.default {
                Some(_) => assert!(in_fixture, "{path} has a default but is not generated"),
                None => assert!(
                    !in_fixture || PER_USER_PATHS.contains(&field.id()),
                    "{path} is generated but has no default"
                ),
            }
        }
    }

    #[test]
    fn ids_are_unique_and_grouped_by_section() {
        let ids: BTreeSet<FieldId> = FIELDS.iter().map(Field::id).collect();
        assert_eq!(ids.len(), FIELDS.len(), "duplicate field ids");
        let sections: Vec<&str> = SECTIONS.iter().map(|section| section.key).collect();
        let unique: BTreeSet<&str> = sections.iter().copied().collect();
        assert_eq!(unique.len(), sections.len(), "duplicate sections");
        let mut last = 0;
        for field in FIELDS {
            let index = sections
                .iter()
                .position(|section| *section == field.section)
                .unwrap_or_else(|| panic!("{} is not in SECTIONS", field.path()));
            assert!(index >= last, "{} is out of section order", field.path());
            last = index;
        }
        for section in SECTIONS {
            assert!(
                fields_of(section.key).next().is_some(),
                "{} is empty",
                section.key
            );
        }
        for field in FIELDS {
            assert_eq!(
                section_of(field).map(|section| section.key),
                Some(field.section)
            );
        }
        assert!(!sections.contains(&"zcashd_compat"));
        assert_eq!(SKIPPED_SECTIONS, &["zcashd_compat"]);
    }

    fn assert_sentence(text: &str, what: &str) {
        assert!(text.ends_with('.'), "{what}: no full stop: {text:?}");
        assert!(
            !text.starts_with(char::is_whitespace),
            "{what}: leading space: {text:?}"
        );
        assert!(
            text.starts_with(|c: char| c.is_ascii_uppercase()),
            "{what}: {text:?}"
        );
        assert!(!text.contains("  "), "{what}: double space: {text:?}");
        assert!(!text.contains('\n'), "{what}: line break: {text:?}");
        assert!(!text.contains('!'), "{what}: exclamation mark: {text:?}");
    }

    #[test]
    fn docs_and_notes_are_plain_sentences() {
        for section in SECTIONS {
            assert_sentence(section.doc, section.key);
        }
        for field in FIELDS {
            assert_sentence(field.doc, &field.path());
            if let Control::Managed { note }
            | Control::Forced { note, .. }
            | Control::Forbidden { note } = field.control
            {
                assert_sentence(note, &field.path());
            }
        }
    }

    #[test]
    fn labels_and_titles_are_sentence_case() {
        let labels = FIELDS
            .iter()
            .map(|field| field.label)
            .chain(SECTIONS.iter().map(|section| section.title));
        for label in labels {
            assert!(
                label.starts_with(|c: char| c.is_ascii_uppercase()),
                "{label:?}"
            );
            assert!(!label.ends_with('.'), "{label:?}");
            let words: Vec<&str> = label.split(' ').collect();
            if words.len() > 1 {
                assert!(
                    words[1..]
                        .iter()
                        .any(|word| word.starts_with(|c: char| c.is_ascii_lowercase())),
                    "{label:?} looks like title case"
                );
            }
        }
    }

    #[test]
    fn managed_and_forced_keys_match_the_launcher() {
        let managed: BTreeSet<FieldId> = FIELDS
            .iter()
            .filter(|field| matches!(field.control, Control::Managed { .. }))
            .map(Field::id)
            .collect();
        assert_eq!(
            managed,
            BTreeSet::from([
                ("network", "network"),
                ("rpc", "listen_addr"),
                ("rpc", "enable_cookie_auth"),
                ("rpc", "cookie_dir"),
                ("metrics", "endpoint_addr"),
                ("state", "cache_dir"),
            ])
        );
        let forced: BTreeSet<FieldId> = FIELDS
            .iter()
            .filter(|field| matches!(field.control, Control::Forced { .. }))
            .map(Field::id)
            .collect();
        assert_eq!(
            forced,
            BTreeSet::from([("tracing", "use_color"), ("tracing", "force_use_color")])
        );
        let forbidden: BTreeSet<FieldId> = FIELDS
            .iter()
            .filter(|field| matches!(field.control, Control::Forbidden { .. }))
            .map(Field::id)
            .collect();
        assert_eq!(
            forbidden,
            BTreeSet::from([("tracing", "progress_bar"), ("tracing", "log_file")])
        );
    }

    #[test]
    fn loopback_keys_match_the_launcher() {
        let listeners = |loopback| -> BTreeSet<FieldId> {
            FIELDS
                .iter()
                .filter(|field| {
                    field.kind
                        == Kind::SocketAddr {
                            loopback_only: loopback,
                        }
                })
                .map(Field::id)
                .collect()
        };
        assert_eq!(
            listeners(true),
            BTreeSet::from([
                ("rpc", "listen_addr"),
                ("rpc", "indexer_listen_addr"),
                ("rpc", "lightwalletd_listen_addr"),
                ("metrics", "endpoint_addr"),
                ("health", "listen_addr"),
                ("tracing", "endpoint_addr"),
            ])
        );
        assert_eq!(
            listeners(false),
            BTreeSet::from([("network", "listen_addr"), ("network", "external_addr")])
        );
    }

    #[test]
    fn schema_size() {
        assert_eq!(SECTIONS.len(), 11);
        assert_eq!(FIELDS.len(), 56);
        assert_eq!(fields_of("sync").count(), 4);
    }

    // Parsing.

    #[test]
    fn fixture_parses_cleanly() {
        let parsed = parse_config(FIXTURE).expect("parses");
        assert!(parsed.unknown.is_empty(), "{:?}", parsed.unknown);
        assert!(parsed.unreadable.is_empty(), "{:?}", parsed.unreadable);
        assert_eq!(
            parsed.form.get(("network", "listen_addr")),
            Some("[::]:8233")
        );
        assert_eq!(
            parsed.form.get(("network", "crawl_new_peer_interval")),
            Some("1m 1s")
        );
        assert_eq!(parsed.form.get(("network", "cache_dir")), Some("true"));
        assert_eq!(parsed.form.get(("sync", "parallel_cpu_threads")), Some("0"));
        assert_eq!(
            parsed.form.get(("consensus", "checkpoint_sync")),
            Some("true")
        );
        assert_eq!(
            parsed.form.get(("network", "initial_testnet_peers")),
            Some(
                "dnsseed.testnet.z.cash:18233\nseeder.testnet.zec.rocks:18233\n\
                 testnet.seeder.zfnd.org:18233"
            )
        );
        assert_eq!(parsed.form.get(("rpc", "indexer_listen_addr")), None);
    }

    #[test]
    fn managed_keys_are_read() {
        let form = parse_config(FIXTURE).expect("parses").form;
        assert_eq!(form.get(("network", "network")), Some("Mainnet"));
        assert_eq!(form.get(("rpc", "enable_cookie_auth")), Some("true"));
        assert_eq!(
            form.get(("rpc", "cookie_dir")),
            Some("/home/ar/.cache/zebra")
        );
        assert_eq!(
            form.get(("state", "cache_dir")),
            Some("/home/ar/.cache/zebra")
        );
        assert_eq!(form.get(("tracing", "use_color")), Some("true"));
    }

    #[test]
    fn unknown_and_unreadable_keys_are_reported() {
        let text = "\
loose = 1

[network]
bogus = 1
listen_addr = 8233
cache_dir = \"/peers\"

[network.testnet_parameters]
network_name = \"custom\"

[sync]
download_concurrency_limit = \"fifty\"
full_verify_concurrency_limit = 30

[rpc]
foo.bar = 2
listen_addr = { host = 1 }

[zcashd_compat]
enabled = false
";
        let parsed = parse_config(text).expect("parses");
        assert_eq!(
            parsed.unknown,
            ["network.bogus", "network.testnet_parameters", "rpc.foo"]
        );
        assert_eq!(
            parsed.unreadable,
            [
                "network.listen_addr",
                "rpc.listen_addr",
                "sync.download_concurrency_limit"
            ]
        );
        assert_eq!(parsed.form.get(("network", "cache_dir")), Some("/peers"));
        assert_eq!(
            parsed.form.get(("sync", "full_verify_concurrency_limit")),
            Some("30")
        );
        assert_eq!(
            parsed.form.get(("sync", "download_concurrency_limit")),
            None
        );
    }

    #[test]
    fn empty_values_in_the_file_are_kept() {
        let text = "[network]\ninitial_mainnet_peers = []\n\n[mining]\nminer_memo = \" \"\n\n\
                    [tracing]\nfilter = \"\"\n";
        let parsed = parse_config(text).expect("parses");
        // Not read as "" (which the form takes for "use the default"), so no phantom change.
        assert_eq!(parsed.form, FormState::default());
        assert_eq!(
            parsed.unreadable,
            [
                "mining.miner_memo",
                "network.initial_mainnet_peers",
                "tracing.filter"
            ]
        );
        // Untouched on save, not removed.
        assert_eq!(render_config(text, &parsed.form).expect("renders"), text);
        // A value typed into the form still replaces it.
        let form = form_with(("network", "initial_mainnet_peers"), "my-node.example:8233");
        let rendered = render_config(text, &form).expect("renders");
        assert!(
            rendered.starts_with(
                "[network]\ninitial_mainnet_peers = [\n    \"my-node.example:8233\",\n]\n"
            ),
            "{rendered}"
        );
    }

    #[test]
    fn inline_and_dotted_sections_are_read() {
        let text = "sync = { parallel_cpu_threads = 2 }\nhealth.min_connected_peers = 3\n";
        let parsed = parse_config(text).expect("parses");
        assert!(parsed.unknown.is_empty());
        assert_eq!(parsed.form.get(("sync", "parallel_cpu_threads")), Some("2"));
        assert_eq!(
            parsed.form.get(("health", "min_connected_peers")),
            Some("3")
        );
    }

    #[test]
    fn non_table_sections_are_ignored() {
        let text = "network = 1\n\n[[rpc]]\nlisten_addr = \"x\"\n";
        let parsed = parse_config(text).expect("parses");
        assert_eq!(parsed, ParsedConfig::default());
    }

    #[test]
    fn non_toml_fails() {
        assert!(parse_config("[network\n").is_err());
        assert!(render_config("= 1", &FormState::default()).is_err());
    }

    // Validation.

    #[test]
    fn empty_text_is_the_default_for_every_field() {
        for field in FIELDS {
            assert_eq!(validate(field, ""), Ok(()), "{}", field.path());
            assert_eq!(validate(field, " \n "), Ok(()), "{}", field.path());
        }
    }

    #[test]
    fn bools() {
        let field = get(("state", "ephemeral"));
        assert_eq!(validate(field, "true"), Ok(()));
        assert_eq!(validate(field, " false "), Ok(()));
        assert_eq!(
            validate(field, "yes"),
            Err("Enter true or false.".to_owned())
        );
        assert_eq!(
            validate(field, "True"),
            Err("Enter true or false.".to_owned())
        );
    }

    #[test]
    fn integers_within_bounds() {
        let size = get(("rpc", "max_response_body_size"));
        assert_eq!(validate(size, "0"), Ok(()));
        assert_eq!(validate(size, "4294967295"), Ok(()));
        let message = Err("Enter a whole number from 0 to 4294967295.".to_owned());
        assert_eq!(validate(size, "4294967296"), message);
        assert_eq!(validate(size, "-1"), message);
        assert_eq!(validate(size, "12.5"), message);
        assert_eq!(validate(size, "lots"), message);

        let per_ip = get(("network", "max_connections_per_ip"));
        assert_eq!(validate(per_ip, "1"), Ok(()));
        assert_eq!(
            validate(per_ip, "0"),
            Err("Enter a whole number of at least 1.".to_owned())
        );

        let threads = get(("sync", "parallel_cpu_threads"));
        assert_eq!(validate(threads, "0"), Ok(()));
        assert_eq!(validate(threads, "9223372036854775807"), Ok(()));
        assert_eq!(
            validate(threads, "9223372036854775808"),
            Err("Enter a whole number of at least 0.".to_owned())
        );

        let behind = get(("health", "ready_max_blocks_behind"));
        assert_eq!(validate(behind, "-5"), Ok(()));
        assert_eq!(
            validate(behind, "x"),
            Err("Enter a whole number.".to_owned())
        );

        let percent = get(("tracing", "opentelemetry_sample_percent"));
        assert_eq!(validate(percent, "100"), Ok(()));
        assert_eq!(
            validate(percent, "101"),
            Err("Enter a whole number from 0 to 100.".to_owned())
        );
    }

    #[test]
    fn socket_addrs() {
        let loopback = get(("rpc", "indexer_listen_addr"));
        assert_eq!(validate(loopback, "127.0.0.1:8230"), Ok(()));
        assert_eq!(validate(loopback, "127.1.2.3:8230"), Ok(()));
        assert_eq!(validate(loopback, "[::1]:8232"), Ok(()));
        let not_loopback = Err(
            "Must be a loopback address such as 127.0.0.1:8232, so the node \
                                is not reachable from other machines."
                .to_owned(),
        );
        assert_eq!(validate(loopback, "0.0.0.0:8230"), not_loopback);
        assert_eq!(validate(loopback, "192.168.1.5:8230"), not_loopback);
        assert_eq!(validate(loopback, "[::]:8230"), not_loopback);
        let malformed = Err(
            "Enter an IP address and port such as 127.0.0.1:8232; put an IPv6 \
                             address in brackets, such as [::1]:8232."
                .to_owned(),
        );
        assert_eq!(validate(loopback, "localhost:8230"), malformed);
        assert_eq!(validate(loopback, "127.0.0.1"), malformed);
        assert_eq!(validate(loopback, "::1:8232"), malformed);

        let public = get(("network", "listen_addr"));
        assert_eq!(validate(public, "0.0.0.0:8233"), Ok(()));
        assert_eq!(validate(public, "[::]:8233"), Ok(()));
        assert_eq!(validate(public, "203.0.113.9:8233"), Ok(()));
        // zebrad adds the network's default port to an address without one.
        assert_eq!(validate(public, "203.0.113.9"), Ok(()));
        assert_eq!(validate(public, "0.0.0.0"), Ok(()));
        assert_eq!(validate(public, "[::]"), Ok(()));
        assert_eq!(validate(public, "::"), Ok(()));
        let public_malformed = Err(
            "Enter an IP address, with or without a port, such as 0.0.0.0:8233 or 0.0.0.0; \
             put an IPv6 address in brackets, such as [::]:8233."
                .to_owned(),
        );
        // zebrad 7.0.0 does not resolve host names for these keys, despite its hint.
        assert_eq!(validate(public, "myhost.example:8233"), public_malformed);
        assert_eq!(validate(public, "myhost.example"), public_malformed);
        assert_eq!(validate(public, "[::]:"), public_malformed);
        assert_eq!(validate(public, "[0.0.0.0]"), public_malformed);
        assert_eq!(validate(public, "0.0.0.0:99999"), public_malformed);
        assert_eq!(
            validate(get(("network", "external_addr")), "198.51.100.7"),
            Ok(())
        );
    }

    #[test]
    fn durations() {
        let field = get(("network", "crawl_new_peer_interval"));
        // Everything humantime 2.3.0 (pinned by zebrad 7.0.0) parses.
        for ok in [
            "1m 1s",
            "500ms",
            "1h",
            "2 days",
            "1m1s",
            "3 h",
            "1h 30m 15s",
            "1w",
            "0",
            "1.5h",
            "2.5m",
            "31.1ms",
            "1.50h 2s",
            "90millis",
            "7hrs",
            "163µs",
            "2wks",
            "1yr",
            "10nanos",
        ] {
            assert_eq!(validate(field, ok), Ok(()), "{ok:?}");
        }
        let message = Err(
            "Enter a duration such as 1m 1s or 500ms, using the units ms, s, m, h or d.".to_owned(),
        );
        for bad in [
            "5",
            "5 parsecs",
            "1m,1s",
            "m",
            "1.s",
            ".5s",
            "1.5",
            "1,5h",
            "99999999999999999999s",
            "1m -1s",
            "1μs",
        ] {
            assert_eq!(validate(field, bad), message, "{bad:?}");
        }
    }

    #[test]
    fn paths_text_and_bool_or_path() {
        assert_eq!(
            validate(get(("tracing", "flamegraph")), "/tmp/flame"),
            Ok(())
        );
        assert_eq!(
            validate(get(("tracing", "filter")), "warn,zebrad=info"),
            Ok(())
        );
        let cache = get(("network", "cache_dir"));
        assert_eq!(validate(cache, "true"), Ok(()));
        assert_eq!(validate(cache, "false"), Ok(()));
        assert_eq!(validate(cache, "/var/cache/zebra"), Ok(()));
    }

    #[test]
    fn choices_are_exact() {
        let network = get(("network", "network"));
        assert_eq!(validate(network, "Mainnet"), Ok(()));
        assert_eq!(validate(network, "Testnet"), Ok(()));
        assert_eq!(
            validate(network, "mainnet"),
            Err("Choose one of: Mainnet, Testnet.".to_owned())
        );
        let bars = get(("tracing", "progress_bar"));
        assert_eq!(validate(bars, "summary"), Ok(()));
        assert_eq!(validate(bars, "detailed"), Ok(()));
        assert_eq!(
            validate(bars, "Summary"),
            Err("Choose one of: summary, detailed.".to_owned())
        );
    }

    #[test]
    fn peer_lists() {
        let field = get(("network", "initial_mainnet_peers"));
        assert_eq!(
            validate(field, "dnsseed.z.cash:8233\n\n  seeder.zec.rocks:8233  \n"),
            Ok(())
        );
        assert_eq!(
            validate(field, "[::1]:8233\n1.2.3.4:8233\nmy-node:8233"),
            Ok(())
        );
        let line = |n: usize| {
            Err(format!(
                "Line {n}: enter a host name or IP address with a port, such as \
                 dnsseed.z.cash:8233."
            ))
        };
        assert_eq!(validate(field, "dnsseed.z.cash"), line(1));
        // The row in the textarea, blank lines included.
        assert_eq!(validate(field, "a.example:8233\n\nb.example"), line(3));
        assert_eq!(validate(field, "\n\nbad"), line(3));
        assert_eq!(validate(field, "  a.example:8233 \n  bad \n"), line(2));
        assert_eq!(validate(field, "bad_host:8233"), line(1));
        assert_eq!(validate(field, "host:99999"), line(1));
        assert_eq!(validate(field, "-host.example:8233"), line(1));
        assert_eq!(validate(field, "host..example:8233"), line(1));
        assert_eq!(validate(field, "::1:8233"), line(1));
    }

    // Rendering.

    #[test]
    fn round_trip_of_the_fixture_is_identical() {
        let form = parse_config(FIXTURE).expect("parses").form;
        assert_eq!(render_config(FIXTURE, &form).expect("renders"), FIXTURE);
    }

    #[test]
    fn setting_a_value_creates_the_section() {
        let original = "# zebrad configuration managed by Zebra Launcher.\n\n[network]\nnetwork = \"Mainnet\"\n";
        let form = form_with(("sync", "download_concurrency_limit"), " 100 ");
        let rendered = render_config(original, &form).expect("renders");
        assert_eq!(
            rendered,
            "# zebrad configuration managed by Zebra Launcher.\n\n[network]\nnetwork = \"Mainnet\"\n\n[sync]\ndownload_concurrency_limit = 100\n"
        );
        let parsed = parse_config(&rendered).expect("parses");
        assert_eq!(
            parsed.form.get(("sync", "download_concurrency_limit")),
            Some("100")
        );
    }

    #[test]
    fn unsetting_removes_the_key_and_a_created_section() {
        let original = "[network]\nnetwork = \"Mainnet\"\n";
        let with_sync = render_config(original, &form_with(("sync", "parallel_cpu_threads"), "4"))
            .expect("renders");
        assert!(with_sync.contains("[sync]\nparallel_cpu_threads = 4\n"));

        let mut form = parse_config(&with_sync).expect("parses").form;
        form.set(("sync", "parallel_cpu_threads"), None);
        assert_eq!(render_config(&with_sync, &form).expect("renders"), original);
        assert_eq!(render_config(original, &form).expect("renders"), original);
    }

    #[test]
    fn whitespace_only_text_removes_the_key() {
        let original = "[sync]\nparallel_cpu_threads = 4\n\n[mempool]\ntx_cost_limit = 1\n";
        let mut form = parse_config(original).expect("parses").form;
        form.set(("sync", "parallel_cpu_threads"), Some("  \n".to_owned()));
        assert_eq!(
            render_config(original, &form).expect("renders"),
            "[mempool]\ntx_cost_limit = 1\n"
        );
    }

    #[test]
    fn removing_a_section_keeps_the_next_headers_comment() {
        let original =
            "[sync]\nparallel_cpu_threads = 4\n\n# mempool tuning\n[mempool]\ntx_cost_limit = 1\n";
        let mut form = parse_config(original).expect("parses").form;
        form.set(("sync", "parallel_cpu_threads"), None);
        assert_eq!(
            render_config(original, &form).expect("renders"),
            "# mempool tuning\n[mempool]\ntx_cost_limit = 1\n"
        );

        let original = "[network]\npeerset_initial_target_size = 30\n\n[sync]\n\
                        parallel_cpu_threads = 4\n\n# mempool tuning\n[mempool]\ntx_cost_limit = 1\n";
        let mut form = parse_config(original).expect("parses").form;
        form.set(("sync", "parallel_cpu_threads"), None);
        assert_eq!(
            render_config(original, &form).expect("renders"),
            "[network]\npeerset_initial_target_size = 30\n\n# mempool tuning\n[mempool]\n\
             tx_cost_limit = 1\n"
        );
    }

    #[test]
    fn an_emptied_section_with_a_comment_is_kept() {
        let original = "# tuned for a slow link\n[sync]\ndownload_concurrency_limit = 10\n";
        let form = FormState::default();
        assert_eq!(
            render_config(original, &form).expect("renders"),
            "# tuned for a slow link\n[sync]\n"
        );
    }

    #[test]
    fn comments_and_unknown_keys_survive_a_change() {
        let original = "\
# header comment

[network] # the network section
bogus = 1 # unknown, kept
listen_addr = \"[::]:8233\" # all interfaces
peerset_initial_target_size = 25

[zcashd_compat]
enabled = false
";
        let mut form = parse_config(original).expect("parses").form;
        form.set(("network", "listen_addr"), Some("0.0.0.0:8233".to_owned()));
        form.set(
            ("network", "peerset_initial_target_size"),
            Some("40".to_owned()),
        );
        assert_eq!(
            render_config(original, &form).expect("renders"),
            "\
# header comment

[network] # the network section
bogus = 1 # unknown, kept
listen_addr = \"0.0.0.0:8233\" # all interfaces
peerset_initial_target_size = 40

[zcashd_compat]
enabled = false
"
        );
    }

    #[test]
    fn managed_and_forced_fields_in_the_form_are_ignored() {
        let mut form = parse_config(FIXTURE).expect("parses").form;
        form.set(("network", "network"), Some("Testnet".to_owned()));
        form.set(("rpc", "listen_addr"), Some("0.0.0.0:1".to_owned()));
        form.set(("rpc", "enable_cookie_auth"), Some("false".to_owned()));
        form.set(("state", "cache_dir"), None);
        form.set(("tracing", "use_color"), Some("false".to_owned()));
        form.set(("tracing", "log_file"), Some("/tmp/zebrad.log".to_owned()));
        assert_eq!(render_config(FIXTURE, &form).expect("renders"), FIXTURE);
    }

    #[test]
    fn forbidden_keys_are_read_for_display_and_removed_on_render() {
        let original = "[tracing]\nlog_file = \"/tmp/z.log\"\nprogress_bar = 1\n\n\
                        [sync]\nparallel_cpu_threads = 4\n";
        let parsed = parse_config(original).expect("parses");
        // Still read, so the locked control can show what the file holds.
        assert_eq!(parsed.form.get(("tracing", "log_file")), Some("/tmp/z.log"));
        assert_eq!(parsed.unreadable, ["tracing.progress_bar"]);
        // Removed whatever the value's type: the launcher refuses the file while the key is
        // present. The emptied section goes too.
        assert_eq!(
            render_config(original, &parsed.form).expect("renders"),
            "[sync]\nparallel_cpu_threads = 4\n"
        );
        assert_eq!(
            render_config(
                "[tracing]\nlog_file = \"/tmp/z.log\"\n",
                &FormState::default()
            )
            .expect("renders"),
            ""
        );
        // Other tracing keys stay.
        assert_eq!(
            render_config(
                "[tracing]\nfilter = \"debug\"\nprogress_bar = \"summary\"\n",
                &parse_config("[tracing]\nfilter = \"debug\"\n")
                    .expect("parses")
                    .form
            )
            .expect("renders"),
            "[tracing]\nfilter = \"debug\"\n"
        );
    }

    #[test]
    fn a_set_list_renders_one_entry_per_line() {
        let form = form_with(
            ("network", "initial_testnet_peers"),
            "a.example:18233\n\n  b.example:18233  \n",
        );
        assert_eq!(
            render_config("", &form).expect("renders"),
            "[network]\ninitial_testnet_peers = [\n    \"a.example:18233\",\n    \"b.example:18233\",\n]\n"
        );
    }

    #[test]
    fn a_changed_list_keeps_the_generate_layout() {
        let mut form = parse_config(FIXTURE).expect("parses").form;
        let peers = form
            .get(("network", "initial_testnet_peers"))
            .expect("in the fixture")
            .to_owned();
        form.set(
            ("network", "initial_testnet_peers"),
            Some(format!("{peers}\nmy-node.example:18233")),
        );
        let rendered = render_config(FIXTURE, &form).expect("renders");
        assert!(rendered.contains(
            "initial_testnet_peers = [\n    \"dnsseed.testnet.z.cash:18233\",\n    \
             \"seeder.testnet.zec.rocks:18233\",\n    \"testnet.seeder.zfnd.org:18233\",\n    \
             \"my-node.example:18233\",\n]\nlisten_addr = \"[::]:8233\"\n"
        ));
    }

    #[test]
    fn values_are_written_with_their_toml_type() {
        let mut form = FormState::default();
        form.set(("state", "ephemeral"), Some("true".to_owned()));
        form.set(("network", "cache_dir"), Some("false".to_owned()));
        form.set(("mempool", "eviction_memory_time"), Some("30m".to_owned()));
        form.set(("health", "listen_addr"), Some("[::1]:8080".to_owned()));
        let rendered = render_config("", &form).expect("renders");
        assert!(
            rendered.contains("[network]\ncache_dir = false\n"),
            "{rendered}"
        );
        assert!(
            rendered.contains("[state]\nephemeral = true\n"),
            "{rendered}"
        );
        assert!(
            rendered.contains("[mempool]\neviction_memory_time = \"30m\"\n"),
            "{rendered}"
        );
        assert!(
            rendered.contains("[health]\nlisten_addr = \"[::1]:8080\"\n"),
            "{rendered}"
        );
        let parsed = parse_config(&rendered).expect("parses");
        assert_eq!(
            parsed,
            parse_config(&render_config(&rendered, &parsed.form).expect("renders"))
                .expect("parses")
        );

        form.set(
            ("network", "cache_dir"),
            Some("/var/cache/zebra".to_owned()),
        );
        let rendered = render_config(&rendered, &form).expect("renders");
        assert!(
            rendered.contains("cache_dir = \"/var/cache/zebra\"\n"),
            "{rendered}"
        );
    }

    #[test]
    fn unreadable_values_are_left_alone_until_set() {
        let original = "[sync]\ndownload_concurrency_limit = \"fifty\"\n";
        let mut form = parse_config(original).expect("parses").form;
        assert_eq!(render_config(original, &form).expect("renders"), original);
        form.set(
            ("sync", "download_concurrency_limit"),
            Some("60".to_owned()),
        );
        assert_eq!(
            render_config(original, &form).expect("renders"),
            "[sync]\ndownload_concurrency_limit = 60\n"
        );
    }

    #[test]
    fn invalid_values_fail_with_their_path() {
        let form = form_with(("rpc", "indexer_listen_addr"), "0.0.0.0:8230");
        let error = render_config("", &form).expect_err("rejected");
        assert!(
            error.starts_with("rpc.indexer_listen_addr: Must be a loopback address"),
            "{error}"
        );
        let form = form_with(("sync", "download_concurrency_limit"), "0");
        assert_eq!(
            render_config("", &form),
            Err("sync.download_concurrency_limit: Enter a whole number of at least 1.".to_owned())
        );
    }

    #[test]
    fn a_rejected_value_already_in_the_file_does_not_block_the_other_fields() {
        // zebrad accepts 0 here (it raises it to 1); the form's `validate` does not. Carried
        // over from the file it is left alone, so another field can still be written.
        let original = "[sync]\ndownload_concurrency_limit = 0\n";
        let mut form = parse_config(original).expect("parses").form;
        assert_eq!(form.get(("sync", "download_concurrency_limit")), Some("0"));
        assert_eq!(render_config(original, &form).expect("renders"), original);
        form.set(("tracing", "filter"), Some("info".to_owned()));
        assert_eq!(
            render_config(original, &form).expect("renders"),
            "[sync]\ndownload_concurrency_limit = 0\n\n[tracing]\nfilter = \"info\"\n"
        );
        // A new value for that field is checked as before.
        form.set(
            ("sync", "download_concurrency_limit"),
            Some("-3".to_owned()),
        );
        assert_eq!(
            render_config(original, &form),
            Err("sync.download_concurrency_limit: Enter a whole number of at least 1.".to_owned())
        );
        // Spacing around the file's value does not make it a new one.
        let padded = "[sync]\ndownload_concurrency_limit = 0 # none\n";
        let form = parse_config(padded).expect("parses").form;
        assert_eq!(render_config(padded, &form).expect("renders"), padded);
    }

    #[test]
    fn a_section_that_is_not_a_table_fails() {
        let form = form_with(("sync", "parallel_cpu_threads"), "2");
        let error = render_config("[[sync]]\n", &form).expect_err("rejected");
        assert_eq!(error, "`sync` in zebrad.toml is not a table.");
        assert!(render_config("sync = 1\n", &form).is_err());
    }

    // Helpers for the form.

    #[test]
    fn filter_matches_label_key_path_and_doc() {
        let field = get(("rpc", "indexer_listen_addr"));
        assert!(matches_filter(field, "Indexer"));
        assert!(matches_filter(field, "indexer_listen"));
        assert!(matches_filter(field, "rpc.indexer"));
        assert!(matches_filter(field, "LOOPBACK"));
        assert!(matches_filter(field, "  "));
        assert!(!matches_filter(field, "mempool"));
    }

    #[test]
    fn display_default_shows_lists_inline() {
        assert_eq!(
            display_default(get(("network", "initial_testnet_peers"))),
            "dnsseed.testnet.z.cash:18233, seeder.testnet.zec.rocks:18233, testnet.seeder.zfnd.org:18233"
        );
        assert_eq!(display_default(get(("rpc", "listen_addr"))), "not set");
        assert_eq!(display_default(get(("sync", "parallel_cpu_threads"))), "0");
    }

    #[test]
    fn diff_lists_changed_fields() {
        let base = parse_config(FIXTURE).expect("parses").form;
        let mut edited = base.clone();
        assert!(base.diff(&edited).is_empty());
        edited.set(("sync", "parallel_cpu_threads"), Some("2".to_owned()));
        edited.set(("tracing", "filter"), Some("debug".to_owned()));
        assert_eq!(
            base.diff(&edited),
            [("sync", "parallel_cpu_threads"), ("tracing", "filter")]
        );
    }
}
