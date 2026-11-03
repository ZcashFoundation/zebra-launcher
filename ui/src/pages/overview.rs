//! The overview of a launcher that runs several nodes (docs/MULTI-NODE.md): a card per node with
//! its state and actions, what the nodes use of each other, their ports and their data
//! folders. The sidebar opens it; with a single node the shell shows that node's dashboard
//! instead.

use launcher_types::{NodeInfo, NodeKind, NodeRole, PortRole};
use leptos::{prelude::*, task::spawn_local};

use crate::{
    app::{AppState, Tab},
    format::{controls, thousands},
    node_state::NodeState,
    node_summary::{Tone, summarize},
};

/// "5 nodes · 4 running · 1 stopped": how many there are and how they are doing.
pub fn herd_line(phases: &[launcher_types::Phase]) -> String {
    let running = phases.iter().filter(|phase| phase.is_active()).count();
    let stopped = phases.len() - running;
    let nodes = match phases.len() {
        1 => "1 node".to_owned(),
        n => format!("{n} nodes"),
    };
    let mut parts = vec![nodes];
    if running > 0 {
        parts.push(format!("{running} running"));
    }
    if stopped > 0 {
        parts.push(format!("{stopped} stopped"));
    }
    parts.join(" · ")
}

/// Start every stopped node, the nodes others use first; stop every running one, the users
/// first, so a service never loses its node under it.
fn order_for(nodes: &[NodeState], starting: bool) -> Vec<NodeState> {
    let mut ordered: Vec<NodeState> = nodes.to_vec();
    ordered.sort_by_key(|node| node.info.with_untracked(|info| info.depends_on.is_some()));
    if !starting {
        ordered.reverse();
    }
    ordered
}

#[component]
pub fn Overview(#[prop(into)] on_add: Callback<()>) -> impl IntoView {
    let state = AppState::get();
    let line = move || {
        let phases: Vec<_> = state.nodes.with(|nodes| {
            nodes
                .iter()
                .map(|node| node.status.with(|status| status.phase.clone()))
                .collect()
        });
        herd_line(&phases)
    };
    let run_all = move |starting: bool| {
        let nodes = order_for(&state.nodes.get_untracked(), starting);
        spawn_local(async move {
            for node in nodes {
                let can = node.status.with_untracked(|status| controls(&status.phase));
                let result = if starting && can.start {
                    node.start().await
                } else if !starting && can.stop {
                    node.stop().await
                } else {
                    Ok(())
                };
                if let Err(error) = result {
                    let name = node.info.with_untracked(|info| info.name.clone());
                    state.backend_error.set(Some(format!("{name}: {error}")));
                }
            }
        });
    };
    view! {
        <div class="overview">
            <div class="overview-head">
                <p class="overview-line">{line}</p>
                <div class="buttons">
                    <button type="button" on:click=move |_| run_all(true)>
                        "Start all"
                    </button>
                    <button type="button" on:click=move |_| run_all(false)>
                        "Stop all"
                    </button>
                    <button type="button" class="primary" on:click=move |_| on_add.run(())>
                        "Add node"
                    </button>
                </div>
            </div>
            <div class="node-cards">
                {move || {
                    state
                        .nodes
                        .get()
                        .into_iter()
                        .map(|node| view! { <NodeCard node /> })
                        .collect_view()
                }}
            </div>
            <div class="overview-tables">
                <PortsTable />
                <FoldersTable />
            </div>
        </div>
    }
}

/// One node's card: name, kind and network, its state with a meter for a syncing full node,
/// a line of facts, and Open plus Start or Stop.
#[component]
fn NodeCard(node: NodeState) -> impl IntoView {
    let state = AppState::get();
    let summary = Memo::new(move |_| {
        let now = state.now.get();
        let kind = node.info.with(|info| info.kind);
        node.status.with(|status| {
            node.stats
                .with(|stats| summarize(kind, status, stats.as_ref(), now))
        })
    });
    let facts = move || {
        let info = node.info.get();
        let uses = info.depends_on.as_ref().map(|used| {
            let name = state.nodes.with(|nodes| {
                nodes
                    .iter()
                    .find(|other| other.info.with(|other| &other.id == used))
                    .map(|other| other.info.with(|other| other.name.clone()))
            });
            // The zcashd compatibility build follows its node's chain; services read theirs.
            let verb = if info.kind == NodeKind::Zcashd {
                "follows"
            } else {
                "reads"
            };
            format!("{verb} {}", name.unwrap_or_else(|| used.to_string()))
        });
        let running = node.status.with(|status| status.phase.is_active());
        let numbers = node.stats.with(|stats| {
            stats
                .as_ref()
                .filter(|_| running)
                .map(|stats| match info.kind.role() {
                    NodeRole::FullNode => {
                        format!("{} peers · height {}", stats.peers, thousands(stats.height))
                    }
                    NodeRole::Indexer => format!("indexed to {}", thousands(stats.height)),
                    _ => String::new(),
                })
        });
        [uses, numbers.filter(|text| !text.is_empty())]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ")
    };
    let used_by = move || {
        let id = node.info.with(|info| info.id.clone());
        let users: Vec<String> = state.nodes.with(|nodes| {
            nodes
                .iter()
                .filter(|other| {
                    other
                        .info
                        .with(|other| other.depends_on.as_ref() == Some(&id))
                })
                .map(|other| other.info.with(|other| other.name.clone()))
                .collect()
        });
        (!users.is_empty()).then(|| format!("used by {}", users.join(", ")))
    };
    let open = move |_| {
        state.selected.set(node);
        state.tab.set(Tab::Dashboard);
    };
    let can = move || node.status.with(|status| controls(&status.phase));
    let toggle = move |_| {
        let starting = can().start;
        spawn_local(async move {
            let result = if starting {
                node.start().await
            } else {
                node.stop().await
            };
            if let Err(error) = result {
                state.backend_error.set(Some(error.to_string()));
            }
        });
    };
    let label = move || node.info.with(|info| info.name.clone());
    view! {
        <article
            class="card node-card"
            class:node-card-problem=move || summary.with(|s| s.tone == Tone::Problem)
            aria-label=label
        >
            <header class="node-card-head">
                <span
                    class=move || format!("status-dot {}", summary.with(|s| s.tone.class()))
                    aria-hidden="true"
                ></span>
                <h3>{label}</h3>
                <span class="chip">{move || node.info.with(|info| info.kind.label())}</span>
            </header>
            <p class="node-card-state">{move || summary.with(|s| s.text.clone())}</p>
            {move || {
                summary
                    .with(|s| s.progress)
                    .map(|percent| {
                        view! {
                            <div
                                class="meter node-card-meter"
                                role="meter"
                                aria-label="Sync progress"
                                aria-valuemin="0"
                                aria-valuemax="100"
                                aria-valuenow=format!("{percent:.1}")
                            >
                                <div class="meter-fill" style:width=format!("{percent}%")></div>
                            </div>
                        }
                    })
            }}
            <p class="muted node-card-facts">{facts}</p>
            {move || used_by().map(|text| view! { <p class="muted node-card-facts">{text}</p> })}
            <div class="buttons node-card-actions">
                <button type="button" on:click=open>"Open"</button>
                <button
                    type="button"
                    disabled=move || {
                        let can = can();
                        !(can.start || can.stop)
                    }
                    on:click=toggle
                >
                    {move || if can().start { "Start" } else { "Stop" }}
                </button>
            </div>
        </article>
    }
}

/// Every node's ports in one table, to spot a clash at a glance.
#[component]
fn PortsTable() -> impl IntoView {
    let state = AppState::get();
    let roles = [
        PortRole::P2p,
        PortRole::Rpc,
        PortRole::Metrics,
        PortRole::Grpc,
    ];
    let port_of = |info: &NodeInfo, role: PortRole| {
        info.ports
            .iter()
            .find(|port| port.role == role)
            .map_or_else(|| "—".to_owned(), |port| port.port.to_string())
    };
    view! {
        <section class="card overview-table" aria-labelledby="overview-ports">
            <h3 id="overview-ports">"Ports"</h3>
            <table class="chart-table">
                <thead>
                    <tr>
                        <th scope="col">"Node"</th>
                        {roles.map(|role| view! { <th scope="col">{role.label()}</th> }).collect_view()}
                    </tr>
                </thead>
                <tbody>
                    {move || {
                        state
                            .nodes
                            .get()
                            .into_iter()
                            .map(|node| {
                                let info = node.info.get();
                                view! {
                                    <tr>
                                        <td>{info.name.clone()}</td>
                                        {roles
                                            .map(|role| view! { <td>{port_of(&info, role)}</td> })
                                            .collect_view()}
                                    </tr>
                                }
                            })
                            .collect_view()
                    }}
                </tbody>
            </table>
        </section>
    }
}

/// Where each node keeps its data.
#[component]
fn FoldersTable() -> impl IntoView {
    let state = AppState::get();
    view! {
        <section class="card overview-table" aria-labelledby="overview-folders">
            <h3 id="overview-folders">"Data folders"</h3>
            <table class="chart-table">
                <thead>
                    <tr>
                        <th scope="col">"Node"</th>
                        <th scope="col">"Folder"</th>
                    </tr>
                </thead>
                <tbody>
                    {move || {
                        state
                            .nodes
                            .get()
                            .into_iter()
                            .map(|node| {
                                let info = node.info.get();
                                view! {
                                    <tr>
                                        <td>{info.name.clone()}</td>
                                        <td class="overview-folder">{info.data_dir.clone()}</td>
                                    </tr>
                                }
                            })
                            .collect_view()
                    }}
                </tbody>
            </table>
        </section>
    }
}

#[cfg(test)]
mod tests {
    use launcher_types::Phase;

    use super::*;

    #[test]
    fn the_line_counts_running_and_stopped_nodes() {
        assert_eq!(herd_line(&[Phase::Running]), "1 node · 1 running");
        assert_eq!(
            herd_line(&[Phase::Running, Phase::Starting, Phase::Stopped]),
            "3 nodes · 2 running · 1 stopped"
        );
        assert_eq!(
            herd_line(&[Phase::Stopped, Phase::Stopped]),
            "2 nodes · 2 stopped"
        );
    }
}
