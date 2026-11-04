//! The selected Zebra node's offline documentation: its book, configuration reference and
//! release notes. Nothing is read until the tab's first visit; after that the frame stays loaded
//! while the page is hidden, like the other tabs' state.

use launcher_types::{NodeDocs, ZebradChannel};
use leptos::{prelude::*, task::spawn_local};
use wasm_bindgen::JsCast;

use crate::{
    api,
    app::{AppState, Tab},
    tauri::is_tauri,
};

/// The three documents shipped for each Zebra channel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DocsSection {
    #[default]
    Book,
    Config,
    Changes,
}

impl DocsSection {
    pub const ALL: [DocsSection; 3] =
        [DocsSection::Book, DocsSection::Config, DocsSection::Changes];

    pub fn label(self) -> &'static str {
        match self {
            DocsSection::Book => "Book",
            DocsSection::Config => "Config",
            DocsSection::Changes => "Changes",
        }
    }

    /// A longer name for the frame and its loading state.
    pub(crate) fn title(self) -> &'static str {
        match self {
            DocsSection::Book => "Zebra book",
            DocsSection::Config => "Zebra configuration reference",
            DocsSection::Changes => "Zebra release notes",
        }
    }

    fn slug(self) -> &'static str {
        match self {
            DocsSection::Book => "book",
            DocsSection::Config => "config",
            DocsSection::Changes => "changes",
        }
    }

    /// This section's local HTML path or converted URL.
    pub fn path(self, docs: &NodeDocs) -> &str {
        match self {
            DocsSection::Book => &docs.book,
            DocsSection::Config => &docs.config,
            DocsSection::Changes => &docs.changes,
        }
    }

    /// The section `by` places to the right, wrapping around the ends.
    fn step(self, by: i32) -> DocsSection {
        let current = DocsSection::ALL
            .iter()
            .position(|section| *section == self)
            .unwrap_or(0) as i32;
        let index = (current + by).rem_euclid(DocsSection::ALL.len() as i32);
        DocsSection::ALL[index as usize]
    }

    fn button_id(self) -> String {
        format!("docs-section-{}", self.slug())
    }

    fn panel_id(self) -> String {
        format!("docs-panel-{}", self.slug())
    }
}

/// The section a key selects in the docs toolbar.
fn section_for_key(current: DocsSection, key: &str) -> Option<DocsSection> {
    Some(match key {
        "ArrowRight" => current.step(1),
        "ArrowLeft" => current.step(-1),
        "Home" => DocsSection::ALL[0],
        "End" => DocsSection::ALL[DocsSection::ALL.len() - 1],
        _ => return None,
    })
}

fn focus_section(section: DocsSection) {
    let button = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.get_element_by_id(&section.button_id()))
        .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok());
    if let Some(button) = button {
        let _ = button.focus();
    }
}

/// Adds the embedded documentation's only launcher-supplied parameter. A fragment stays last,
/// and a converted URL that already has a query keeps it.
fn themed_url(path: &str, dark: bool) -> String {
    let (base, fragment) = path
        .split_once('#')
        .map_or((path, ""), |(base, fragment)| (base, fragment));
    let separator = if base.contains('?') { '&' } else { '?' };
    let theme = if dark { "dark" } else { "light" };
    let fragment = if fragment.is_empty() {
        String::new()
    } else {
        format!("#{fragment}")
    };
    format!("{base}{separator}theme={theme}{fragment}")
}

/// The compact release identity beside the section tabs.
fn channel_summary(channel: ZebradChannel) -> String {
    match channel {
        ZebradChannel::Stable => format!("{} {} · recommended", channel.label(), channel.version()),
        ZebradChannel::Beta | ZebradChannel::Nightly => format!(
            "{} {} · commit {} · {}",
            channel.label(),
            channel.version(),
            channel.short_commit(),
            channel.date()
        ),
    }
}

/// Prerelease channels warn next to their documentation too: opening an older stable binary on a
/// newer database is not safe, so a node's channel is fixed when it is created.
fn channel_warning(channel: ZebradChannel) -> Option<&'static str> {
    match channel {
        ZebradChannel::Stable => None,
        ZebradChannel::Beta => Some(
            "Beta software: expect changes before the stable release. This node's channel cannot be changed because stable Zebra may be unable to reopen its database.",
        ),
        ZebradChannel::Nightly => Some(
            "Nightly software: experimental and not recommended for a primary node. This node's channel cannot be changed because stable Zebra may be unable to reopen its database.",
        ),
    }
}

#[component]
pub fn Docs() -> impl IntoView {
    let state = AppState::get();
    let docs = RwSignal::new(None::<NodeDocs>);
    let loading = RwSignal::new(false);
    let load_error = RwSignal::new(None::<String>);
    let visited = RwSignal::new(false);
    let reloads = RwSignal::new(0u64);
    // Only the newest node request may publish. Picking another node while a read is in flight
    // must not briefly show the previous node's channel or release.
    let request = RwSignal::new(0u64);

    Effect::new(move |_| {
        if state.tab.get() == Tab::Docs {
            visited.set(true);
        }
    });
    Effect::new(move |_| {
        if !visited.get() {
            return;
        }
        reloads.get();
        let id = state.node().info.with(|info| info.id.clone());
        let mine = request.get_untracked().saturating_add(1);
        request.set(mine);
        docs.set(None);
        loading.set(true);
        load_error.set(None);
        spawn_local(async move {
            let result = api::get_node_docs(&id).await;
            if request.get_untracked() != mine {
                return;
            }
            match result {
                Ok(found) => docs.set(Some(found)),
                Err(error) => load_error.set(Some(format!(
                    "Could not read this node's offline documentation: {error}"
                ))),
            }
            loading.set(false);
        });
    });

    let browser = !is_tauri();
    view! {
        <div class="docs">
            <div class="toolbar docs-toolbar">
                <div class="docs-section-tabs" role="tablist" aria-label="Documentation sections">
                    {DocsSection::ALL
                        .into_iter()
                        .map(|section| {
                            let selected = move || state.docs_section.get() == section;
                            let keydown = move |event: web_sys::KeyboardEvent| {
                                let Some(next) = section_for_key(section, &event.key()) else {
                                    return;
                                };
                                event.prevent_default();
                                state.docs_section.set(next);
                                focus_section(next);
                            };
                            view! {
                                <button
                                    type="button"
                                    role="tab"
                                    id=section.button_id()
                                    class="docs-section-tab"
                                    class:active=selected
                                    aria-selected=move || selected().to_string()
                                    aria-controls=section.panel_id()
                                    tabindex=move || if selected() { "0" } else { "-1" }
                                    on:click=move |_| state.docs_section.set(section)
                                    on:keydown=keydown
                                >
                                    {section.label()}
                                </button>
                            }
                        })
                        .collect_view()}
                </div>
                {move || {
                    docs.get().map(|found| {
                        view! {
                            <span class=format!("docs-channel docs-channel-{}", found.channel.as_str())>
                                {channel_summary(found.channel)}
                            </span>
                        }
                    })
                }}
            </div>
            {move || {
                docs.get().and_then(|found| {
                    channel_warning(found.channel).map(|warning| {
                        view! { <p class="banner banner-warn docs-warning">{warning}</p> }
                    })
                })
            }}
            <div
                class="docs-panel"
                role="tabpanel"
                id=move || state.docs_section.get().panel_id()
                aria-labelledby=move || state.docs_section.get().button_id()
                aria-busy=move || loading.get().to_string()
            >
                {move || {
                    if loading.get() {
                        view! {
                            <p class="docs-status muted" role="status">
                                "Loading offline documentation…"
                            </p>
                        }
                        .into_any()
                    } else if let Some(error) = load_error.get() {
                        view! {
                            <div class="docs-status docs-load-error" role="alert">
                                <p>{error}</p>
                                <button type="button" on:click=move |_| reloads.update(|n| *n += 1)>
                                    "Retry"
                                </button>
                            </div>
                        }
                        .into_any()
                    } else if docs.with(Option::is_none) {
                        view! { <p class="docs-status muted">"Documentation is unavailable for this node."</p> }
                            .into_any()
                    } else {
                        ().into_any()
                    }
                }}
                <Show when=move || visited.get()>
                    <iframe
                        class="docs-frame"
                        title=move || {
                            let section = state.docs_section.get();
                            docs.get().map_or_else(
                                || section.title().to_owned(),
                                |found| format!("{} {}", found.channel.label(), section.title()),
                            )
                        }
                        src=move || {
                            (!browser).then(|| {
                                docs.get().map(|found| {
                                    themed_url(
                                        state.docs_section.get().path(&found),
                                        state.resolved_dark.get(),
                                    )
                                })
                            }).flatten()
                        }
                        srcdoc=move || {
                            browser.then(|| {
                                docs.get().map(|found| {
                                    crate::mock::docs::srcdoc(
                                        found.channel,
                                        state.docs_section.get(),
                                        state.resolved_dark.get(),
                                    )
                                })
                            }).flatten()
                        }
                        hidden=move || docs.with(Option::is_none)
                        sandbox="allow-scripts"
                        referrerpolicy="no-referrer"
                    ></iframe>
                </Show>
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn docs() -> NodeDocs {
        NodeDocs {
            channel: ZebradChannel::Beta,
            book: "/converted/beta/book/index.html".into(),
            config: "/converted/beta/config.html".into(),
            changes: "/converted/beta/changes.html".into(),
        }
    }

    #[test]
    fn sections_choose_their_own_document_and_keep_keyboard_order() {
        let docs = docs();
        assert_eq!(DocsSection::Book.path(&docs), docs.book);
        assert_eq!(DocsSection::Config.path(&docs), docs.config);
        assert_eq!(DocsSection::Changes.path(&docs), docs.changes);
        assert_eq!(DocsSection::Book.step(-1), DocsSection::Changes);
        assert_eq!(DocsSection::Changes.step(1), DocsSection::Book);
        assert_eq!(
            section_for_key(DocsSection::Book, "Home"),
            Some(DocsSection::Book)
        );
        assert_eq!(
            section_for_key(DocsSection::Book, "End"),
            Some(DocsSection::Changes)
        );
        assert_eq!(
            section_for_key(DocsSection::Config, "ArrowRight"),
            Some(DocsSection::Changes)
        );
        assert_eq!(section_for_key(DocsSection::Config, "Tab"), None);
        for section in DocsSection::ALL {
            assert_eq!(
                section.button_id(),
                format!("docs-section-{}", section.slug())
            );
            assert_eq!(section.panel_id(), format!("docs-panel-{}", section.slug()));
        }
    }

    #[test]
    fn converted_document_urls_carry_only_the_resolved_theme() {
        assert_eq!(
            themed_url("asset://localhost/zebra/book/index.html", true),
            "asset://localhost/zebra/book/index.html?theme=dark"
        );
        assert_eq!(
            themed_url("http://asset.localhost/zebra/config.html#rpc", false),
            "http://asset.localhost/zebra/config.html?theme=light#rpc"
        );
        assert_eq!(
            themed_url("asset://localhost/zebra/changes.html?download=1#v7", true),
            "asset://localhost/zebra/changes.html?download=1&theme=dark#v7"
        );
    }

    #[test]
    fn prerelease_channel_identities_and_warnings_are_explicit() {
        assert!(channel_summary(ZebradChannel::Stable).contains("recommended"));
        assert!(channel_warning(ZebradChannel::Stable).is_none());
        for channel in [ZebradChannel::Beta, ZebradChannel::Nightly] {
            let summary = channel_summary(channel);
            assert!(summary.contains(channel.version()));
            assert!(summary.contains(channel.short_commit()));
            assert!(summary.contains(channel.date()));
            assert!(
                channel_warning(channel)
                    .unwrap()
                    .contains("cannot be changed")
            );
        }
    }
}
