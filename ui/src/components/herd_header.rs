//! The header row of a launcher that runs several nodes, in place of the tab bar: on a node's
//! pages, the node (its status, name, kind and network) and its tabs; on the overview and the
//! launcher's own pages, the page's name.

use launcher_types::NodeKind;
use leptos::prelude::*;

use crate::{
    app::{AppState, Tab},
    components::nav::network_class,
    format::version_tag,
    node_summary::summarize,
};

/// The tabs a node offers. Configure edits the backend node's `zebrad.toml` only until the
/// backend runs several nodes, so the other nodes have no Configure yet; the Explorer is for
/// the kinds that keep the chain and answer zebrad's RPC ([`NodeKind::has_explorer`]).
fn node_tabs(primary: bool, kind: NodeKind) -> Vec<Tab> {
    Tab::NODE
        .into_iter()
        .filter(|tab| primary || *tab != Tab::Configure)
        .filter(|tab| kind.has_explorer() || *tab != Tab::Explorer)
        .collect()
}

#[component]
pub fn HerdHeader() -> impl IntoView {
    let state = AppState::get();
    let node = move || state.node();
    // A node without a Configure or an Explorer tab shows its dashboard when picked from it.
    Effect::new(move |_| {
        let node = state.node();
        let kind = node.info.with(|info| info.kind);
        if !node_tabs(node.primary, kind).contains(&state.tab.get())
            && state.tab.get().is_node_tab()
        {
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
                    <span class=move || node.info.with(|info| network_class(info.network))>
                        {move || node.info.with(|info| info.network.as_str())}
                    </span>
                </div>
                <nav class="tabs" role="tablist" aria-label="Node pages">
                    {node_tabs(node.primary, node.info.with(|info| info.kind))
                        .into_iter()
                        .map(|tab| {
                            view! {
                                <button
                                    role="tab"
                                    id=tab.button_id()
                                    class="tab"
                                    class:active=move || state.tab.get() == tab
                                    aria-selected=move || (state.tab.get() == tab).to_string()
                                    aria-controls=tab.page_id()
                                    on:click=move |_| state.tab.set(tab)
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
    fn the_explorer_is_for_nodes_that_keep_the_chain() {
        assert_eq!(
            node_tabs(true, NodeKind::Zebrad),
            [Tab::Dashboard, Tab::Logs, Tab::Explorer, Tab::Configure]
        );
        assert_eq!(
            node_tabs(false, NodeKind::Zakura),
            [Tab::Dashboard, Tab::Logs, Tab::Explorer]
        );
        assert_eq!(
            node_tabs(false, NodeKind::Zaino),
            [Tab::Dashboard, Tab::Logs]
        );
    }
}
