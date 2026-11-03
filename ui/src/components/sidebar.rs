//! The sidebar of a launcher that runs several nodes (docs/MULTI-NODE.md): the overview, the
//! nodes grouped by network with a status dot and one fact each, adding a node, and the pages
//! that are not about one node. With a single node the shell keeps its tab bar instead.

use launcher_types::{Network, NodeInfo};
use leptos::prelude::*;

use crate::{
    app::{AppState, Tab},
    node_state::NodeState,
    node_summary::summarize,
};

/// The networks in the order the sidebar lists them.
const NETWORKS: [Network; 2] = [Network::Mainnet, Network::Testnet];

/// What a node is called in the sidebar, under its network's heading: its kind while it has
/// its default name ("Zebra"), its own name otherwise.
pub fn short_name(info: &NodeInfo) -> String {
    if info.name == NodeInfo::default_name(info.kind, info.network) {
        info.kind.label().to_owned()
    } else {
        info.name.clone()
    }
}

/// A page item's icon: a 16-unit outline in `currentColor`, the only thing the folded rail
/// shows of the item, so it carries the item's name as its title there.
fn page_icon(path: &'static str) -> impl IntoView {
    view! {
        <svg class="side-icon" viewBox="0 0 16 16" aria-hidden="true">
            <path d=path />
        </svg>
    }
}

const OVERVIEW_ICON: &str =
    "M2.5 2.5h4.5v4.5H2.5zM9 2.5h4.5v4.5H9zM2.5 9h4.5v4.5H2.5zM9 9h4.5v4.5H9z";
const GRAFANA_ICON: &str = "M2 13.5h12M3 11l3-4 3 2.5 4-6";
const SETTINGS_ICON: &str = "M8 5.5a2.5 2.5 0 1 1 0 5 2.5 2.5 0 1 1 0-5zM8 1.5v2M8 12.5v2M1.5 8h2M12.5 8h2M3.4 3.4l1.4 1.4M11.2 11.2l1.4 1.4M12.6 3.4l-1.4 1.4M4.8 11.2l-1.4 1.4";

#[component]
pub fn Sidebar(#[prop(into)] on_add: Callback<()>) -> impl IntoView {
    let state = AppState::get();
    let page_item = move |tab: Tab, label: &'static str, icon: &'static str| {
        view! {
            <button
                type="button"
                class="side-item side-page"
                class:active=move || state.tab.get() == tab
                aria-current=move || (state.tab.get() == tab).then_some("page")
                title=label
                on:click=move |_| state.tab.set(tab)
            >
                {page_icon(icon)}
                <span class="side-label">{label}</span>
            </button>
        }
    };
    let running = move || {
        let (running, total) = state.nodes.with(|nodes| {
            let running = nodes
                .iter()
                .filter(|node| node.status.with(|status| status.phase.is_active()))
                .count();
            (running, nodes.len())
        });
        format!("{running}/{total}")
    };
    let groups = move || {
        let nodes = state.nodes.get();
        NETWORKS
            .into_iter()
            .filter_map(|network| {
                let members: Vec<NodeState> = nodes
                    .iter()
                    .copied()
                    .filter(|node| node.info.with(|info| info.network) == network)
                    .collect();
                (!members.is_empty()).then(|| {
                    view! {
                        <div class="side-group" role="group" aria-label=network.as_str()>
                            <div class="side-group-title">{network.as_str()}</div>
                            {members
                                .into_iter()
                                .map(|node| view! { <NodeItem node /> })
                                .collect_view()}
                        </div>
                    }
                })
            })
            .collect_view()
    };
    view! {
        <nav class="sidebar" aria-label="Nodes and pages">
            <div class="brand sidebar-brand">
                <img class="brand-icon" src="/64x64.png" alt="" width="64" height="64" />
                <span class="brand-name">"Zeal"</span>
            </div>
            <button
                type="button"
                class="side-item side-page"
                class:active=move || state.tab.get() == Tab::Overview
                aria-current=move || (state.tab.get() == Tab::Overview).then_some("page")
                title="Overview"
                on:click=move |_| state.tab.set(Tab::Overview)
            >
                {page_icon(OVERVIEW_ICON)}
                <span class="side-label">"Overview"</span>
                <span class="side-fact" title="Nodes running">{running}</span>
            </button>
            {groups}
            <button
                type="button"
                class="side-item side-add"
                title="Add node"
                on:click=move |_| on_add.run(())
            >
                <span class="side-plus" aria-hidden="true">"+"</span>
                <span class="side-label">"Add node"</span>
            </button>
            <div class="side-spacer"></div>
            {page_item(Tab::Grafana, "Grafana", GRAFANA_ICON)}
            {page_item(Tab::Settings, "Settings", SETTINGS_ICON)}
        </nav>
    }
}

/// One node in the sidebar: its status dot, name and one fact. Picking it shows its pages.
#[component]
fn NodeItem(node: NodeState) -> impl IntoView {
    let state = AppState::get();
    let summary = Memo::new(move |_| {
        let now = state.now.get();
        let kind = node.info.with(|info| info.kind);
        node.status.with(|status| {
            node.stats
                .with(|stats| summarize(kind, status, stats.as_ref(), now))
        })
    });
    let active = move || state.node() == node && state.tab.get().is_node_tab();
    let pick = move |_| {
        state.selected.set(node);
        if !state.tab.get_untracked().is_node_tab() {
            state.tab.set(Tab::Dashboard);
        }
    };
    let unseen = move || node.insights.with(|insights| insights.unseen_problems());
    view! {
        <button
            type="button"
            class="side-item side-node"
            class:active=active
            aria-current=move || active().then_some("page")
            title=move || node.info.with(|info| info.name.clone())
            on:click=pick
        >
            <span
                class=move || format!("status-dot {}", summary.with(|s| s.tone.class()))
                aria-hidden="true"
            ></span>
            <span class="side-label">{move || node.info.with(short_name)}</span>
            <span class="side-fact">
                {move || summary.with(|s| s.short.clone())}
            </span>
            {move || {
                let count = unseen();
                (count > 0)
                    .then(|| {
                        view! {
                            <span
                                class="tab-badge"
                                aria-label=format!("{count} new problems in the log")
                            >
                                {count}
                            </span>
                        }
                    })
            }}
        </button>
    }
}

#[cfg(test)]
mod tests {
    use launcher_types::{NodeId, NodeKind};

    use super::*;

    fn info(name: &str) -> NodeInfo {
        NodeInfo {
            id: NodeId::new("zebrad-mainnet").unwrap(),
            name: name.into(),
            kind: NodeKind::Zebrad,
            network: Network::Mainnet,
            depends_on: None,
            data_dir: String::new(),
            ports: Vec::new(),
        }
    }

    #[test]
    fn a_default_name_shortens_to_the_kind() {
        assert_eq!(short_name(&info("Zebra · Mainnet")), "Zebra");
        assert_eq!(short_name(&info("Archive node")), "Archive node");
    }
}
