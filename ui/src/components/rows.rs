//! A label/value list, `<dl class="rows">`: the dashboard's Node and Chain cards and the sync
//! stage card now; Settings' About, the health-failure panel and the wizard's summaries in
//! the second phase, in place of their hand-written `<dl class="rows">`.
//!
//! A cell is a view, so a cell that shows a time reads `state.now` on its own
//! (`(move || relative(at, state.now.get())).into_any()`) and the list is not rebuilt every
//! second; plain text goes through [`text_rows`]. The list is built once per call: a caller
//! whose set of rows changes wraps `<Rows>` in a closure over what decides them.

use leptos::prelude::*;

/// A label and its cell.
pub type Row = (&'static str, AnyView);

/// Rows of plain text (or any other static cell).
pub fn text_rows<I, V>(rows: I) -> Vec<Row>
where
    I: IntoIterator<Item = (&'static str, V)>,
    V: IntoAny,
{
    rows.into_iter()
        .map(|(label, value)| (label, value.into_any()))
        .collect()
}

/// `rows` as a definition list; `class` is added to `rows`.
#[component]
pub fn Rows(rows: Vec<Row>, #[prop(optional)] class: &'static str) -> impl IntoView {
    let class = if class.is_empty() {
        "rows".to_owned()
    } else {
        format!("rows {class}")
    };
    view! {
        <dl class=class>
            {rows
                .into_iter()
                .map(|(label, value)| view! {
                    <dt>{label}</dt>
                    <dd>{value}</dd>
                })
                .collect_view()}
        </dl>
    }
}
