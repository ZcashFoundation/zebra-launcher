//! The header row of a launcher that runs several nodes, in place of the tab bar: on a node's
//! pages, the node (its status, name, kind, channel and network) and its tabs; on the overview
//! and the launcher's own pages, the page's name.

use launcher_types::NodeKind;
use leptos::prelude::*;

use crate::{
    app::{AppState, Tab},
    components::{
        nav::{focus_tab, network_class, tab_for_key_in},
        zebrad_channel::ZebradChannelChip,
    },
    format::version_tag,
    node_summary::summarize,
};

/// The tabs a node offers: Configure and Docs belong to a zebrad node's own release, so other
/// kinds have neither; the Explorer is for the kinds that keep the chain and answer zebrad's RPC
/// ([`NodeKind::has_explorer`]).
fn node_tabs(kind: NodeKind) -> Vec<Tab> {
    Tab::NODE
        .into_iter()
        .filter(|tab| kind == NodeKind::Zebrad || !matches!(*tab, Tab::Configure | Tab::Docs))
        .filter(|tab| kind.has_explorer() || *tab != Tab::Explorer)
        .collect()
}

#[component]
pub fn HerdHeader() -> impl IntoView {
    let state = AppState::get();
    let node = move || state.node();
    // A node without a Configure, Docs or Explorer tab shows its dashboard when picked from it.
    Effect::new(move |_| {
        let kind = state.node().info.with(|info| info.kind);
        if !node_tabs(kind).contains(&state.tab.get()) && state.tab.get().is_node_tab() {
            state.tab.set(Tab::Dashboard);
        }
    });
    let summary = Memo::new(move |_| {
        let now = state.now.get();
        let node = state.node();
        let kind = node.info.with(|info| info.kind);
        node.status.with(|status| {
            node.stats
                .with(|stats| summarize(kind, status, stats.as_ref(), now))
        })
    });
    let version = move || {
        state
            .update
            .with(|info| info.as_ref().map(|info| version_tag(&info.current_version)))
    };
    let title = move || {
        let tab = state.tab.get();
        if tab.is_node_tab() {
            let node = node();
            view! {
                <div class="herd-title">
                    <span
                        class=move || format!("status-dot {}", summary.with(|s| s.tone.class()))
                        aria-hidden="true"
                    ></span>
                    <h1>{move || node.info.with(|info| info.name.clone())}</h1>
                    <span class="chip">{move || node.info.with(|info| info.kind.label())}</span>
                    {move || {
                        node.info.with(|info| {
                            (info.kind == NodeKind::Zebrad).then(|| {
                                let channel = info.program.channel();
                                view! { <ZebradChannelChip channel /> }
                            })
                        })
                    }}
                    <span class=move || node.info.with(|info| network_class(info.network))>
                        {move || node.info.with(|info| info.network.as_str())}
                    </span>
                </div>
                <nav class="tabs" role="tablist" aria-label="Node pages">
                    {node_tabs(node.info.with_untracked(|info| info.kind))
                        .into_iter()
                        .map(|tab| {
                            let selected = move || state.tab.get() == tab;
                            let keydown = move |event: web_sys::KeyboardEvent| {
                                let tabs = node_tabs(node.info.with_untracked(|info| info.kind));
                                let Some(next) = tab_for_key_in(&tabs, tab, &event.key()) else {
                                    return;
                                };
                                event.prevent_default();
                                state.tab.set(next);
                                focus_tab(next);
                            };
                            view! {
                                <button
                                    type="button"
                                    role="tab"
                                    id=tab.button_id()
                                    class="tab"
                                    class:active=selected
                                    aria-selected=move || selected().to_string()
                                    aria-controls=tab.page_id()
                                    tabindex=move || if selected() { "0" } else { "-1" }
                                    on:click=move |_| state.tab.set(tab)
                                    on:keydown=keydown
                                >
                                    {tab.label()}
                                </button>
                            }
                        })
                        .collect_view()}
                </nav>
            }
            .into_any()
        } else {
            // The overview is about one network; Grafana and Settings are the launcher's own.
            let network = (tab == Tab::Overview).then(|| state.network.get());
            view! {
                <div class="herd-title">
                    <h1>{tab.label()}</h1>
                    {network.map(|network| view! { <span class=network_class(network)>{network.as_str()}</span> })}
                </div>
                <div class="tabs"></div>
            }
            .into_any()
        }
    };
    view! {
        <div class="nav-bar herd-header">
            {title}
            <div class="nav-meta">
                <span class="launcher-version muted" title="Zeal version">
                    {version}
                </span>
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_zebra_nodes_have_config_and_offline_docs_tabs() {
        assert_eq!(
            node_tabs(NodeKind::Zebrad),
            [
                Tab::Dashboard,
                Tab::Logs,
                Tab::Explorer,
                Tab::Configure,
                Tab::Docs,
                Tab::NodeSettings
            ]
        );
        assert_eq!(
            node_tabs(NodeKind::Zakura),
            [Tab::Dashboard, Tab::Logs, Tab::Explorer, Tab::NodeSettings]
        );
        assert_eq!(
            node_tabs(NodeKind::Zaino),
            [Tab::Dashboard, Tab::Logs, Tab::NodeSettings]
        );
    }

    #[test]
    fn node_tabs_keep_the_arrow_key_order() {
        let tabs = node_tabs(NodeKind::Zebrad);
        assert_eq!(
            tab_for_key_in(&tabs, Tab::Configure, "ArrowRight"),
            Some(Tab::Docs)
        );
        assert_eq!(
            tab_for_key_in(&tabs, Tab::NodeSettings, "ArrowRight"),
            Some(Tab::Dashboard)
        );
        assert_eq!(
            tab_for_key_in(&tabs, Tab::Docs, "End"),
            Some(Tab::NodeSettings)
        );
    }
}
