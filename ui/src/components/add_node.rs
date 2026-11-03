//! The add-node dialog (docs/MULTI-NODE.md): pick a kind, fill in its details (network, name,
//! program, the node a service uses), and review what Zeal will set up, with the ports moved
//! out of the way of the nodes that exist. Only the mock can add nodes until the backend runs
//! several.

use launcher_types::{Network, NodeId, NodeInfo, NodeKind, NodePort, NodeRole, assign_ports};
use leptos::{ev, html, prelude::*, task::spawn_local};
use wasm_bindgen::JsCast;

use crate::{api, app::AppState, node_state::NodeState};

/// Where a node's data goes by default: its own folder under the launcher's data folder.
const NODES_DIR: &str = "~/.local/share/org.zfnd.zeal/nodes";

/// What a kind needs that Zeal does not ship, and where it runs, for its card.
fn requirement(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Zebrad => "Bundled with Zeal · Linux, macOS, Windows",
        NodeKind::Zakura => "Your zakurad · release builds for Linux x86-64 and arm64",
        NodeKind::Zcashd => {
            "Your compatibility build · Linux x86-64 · follows a Zebra or Zakura node"
        }
        NodeKind::Zebend => "A checkout of Zebend · macOS on Apple Silicon",
        NodeKind::Zaino => "Your zainod, built with cargo · reads a Zebra node",
        NodeKind::Zallet => "Your zallet · Linux · reads a zebrad built with the indexer feature",
        NodeKind::Generic => "Any program, with its command line",
    }
}

/// The networks a kind runs on: Zebend is Mainnet only.
fn networks(kind: NodeKind) -> &'static [Network] {
    match kind {
        NodeKind::Zebend => &[Network::Mainnet],
        _ => &[Network::Mainnet, Network::Testnet],
    }
}

/// What the program field asks for: nothing for the bundled zebrad.
fn program_prompt(kind: NodeKind) -> Option<(&'static str, &'static str)> {
    match kind {
        NodeKind::Zebrad => None,
        NodeKind::Zakura => Some(("zakurad binary", "/usr/local/bin/zakurad")),
        NodeKind::Zcashd => Some(("zcashd compatibility build", "/usr/local/bin/zcashd")),
        NodeKind::Zebend => Some(("Zebend checkout", "/Users/me/zebend")),
        NodeKind::Zaino => Some(("zainod binary", "/usr/local/bin/zainod")),
        NodeKind::Zallet => Some(("zallet binary", "/usr/local/bin/zallet")),
        NodeKind::Generic => Some(("Command line", "/opt/node/bin/node --config node.toml")),
    }
}

/// What Zeal says before adding a kind with a caveat.
fn caveat(kind: NodeKind) -> Option<&'static str> {
    match kind {
        NodeKind::Zebend => Some(
            "Zebend's project says it is young software that has not been audited: do not use it as the only validator for funds.",
        ),
        NodeKind::Zcashd => Some(
            "Stock zcashd stopped at its end of support in July 2026 and cannot follow NU7. Zeal runs the Zcash Foundation's compatibility build (zebra-compat), connected to the node it follows.",
        ),
        NodeKind::Zakura => Some(
            "Zakura's default x86-64 builds need a CPU with the BMI2 and ADX extensions; it stops at startup without them. Its database cannot be shared with Zebra's.",
        ),
        NodeKind::Zaino => Some(
            "Zaino releases up to 0.10.2 stop syncing at NU7's activation (Mainnet height 3,543,405). Use a release that supports NU7.",
        ),
        NodeKind::Zallet => Some(
            "Zallet is beta: an upgrade may require recreating the wallet. It needs a zebrad built with the indexer feature, which the bundled zebrad is not. Zeal never stores your recovery phrase.",
        ),
        _ => None,
    }
}

/// "P2P 18233 · RPC 18232": a node's ports in a line.
pub fn ports_line(ports: &[NodePort]) -> String {
    if ports.is_empty() {
        return "none".to_owned();
    }
    ports
        .iter()
        .map(|port| format!("{} {}", port.role.label(), port.port))
        .collect::<Vec<_>>()
        .join(" · ")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Kind,
    Details,
    Review,
}

#[component]
pub fn AddNode(
    open: RwSignal<bool>,
    /// What had focus before the dialog opened, taken by the opener before the app behind
    /// the dialog turned inert; focus goes back to it on close.
    return_focus: StoredValue<Option<web_sys::HtmlElement>, LocalStorage>,
) -> impl IntoView {
    let state = AppState::get();
    let step = RwSignal::new(Step::Kind);
    let kind = RwSignal::new(NodeKind::Zebrad);
    let network = RwSignal::new(Network::Testnet);
    let name = RwSignal::new(String::new());
    let name_edited = RwSignal::new(false);
    let program = RwSignal::new(String::new());
    let uses = RwSignal::new(None::<NodeId>);
    let busy = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);

    // A fresh dialog each time it opens, on the network the window shows.
    Effect::new(move |_| {
        if open.get() {
            step.set(Step::Kind);
            kind.set(NodeKind::Zebrad);
            network.set(state.network.get_untracked());
            name_edited.set(false);
            program.set(String::new());
            uses.set(None);
            error.set(None);
        }
    });
    // The name follows the kind and network until the user types one, and is not one that a
    // node already has.
    Effect::new(move |_| {
        let (kind, network) = (kind.get(), network.get());
        let default = state.nodes.with(|nodes| {
            let names: Vec<String> = nodes
                .iter()
                .map(|node| node.info.with(|info| info.name.clone()))
                .collect();
            NodeInfo::unique_default_name(kind, network, names.iter().map(String::as_str))
        });
        if !name_edited.get_untracked() {
            name.set(default);
        }
    });
    // A network the kind does not run on gives way to one it does.
    Effect::new(move |_| {
        let allowed = networks(kind.get());
        if !allowed.contains(&network.get_untracked()) {
            network.set(allowed[0]);
        }
    });

    let existing = move || {
        state
            .nodes
            .with(|nodes| nodes.iter().map(|n| n.info.get()).collect::<Vec<_>>())
    };
    // The nodes a service of this kind can use, on its network.
    let candidates = move || {
        let kind = kind.get();
        let network = network.get();
        existing()
            .into_iter()
            .filter(|info| kind.uses().contains(&info.kind) && info.network == network)
            .collect::<Vec<_>>()
    };
    // Pick the first candidate while none is picked or the picked one no longer fits.
    Effect::new(move |_| {
        let candidates = candidates();
        let fits = uses.with_untracked(|used| {
            used.as_ref()
                .is_some_and(|used| candidates.iter().any(|info| &info.id == used))
        });
        if !fits {
            uses.set(candidates.first().map(|info| info.id.clone()));
        }
    });

    let draft = Memo::new(move |_| {
        let kind = kind.get();
        let network = network.get();
        let nodes = existing();
        let taken_ids: Vec<NodeId> = nodes.iter().map(|info| info.id.clone()).collect();
        let id = NodeId::suggest(kind, network, &taken_ids);
        let taken_ports: Vec<u16> = nodes
            .iter()
            .flat_map(|info| info.ports.iter().map(|port| port.port))
            .collect();
        let wanted = kind.default_ports(network);
        let ports = assign_ports(&wanted, &taken_ports);
        NodeInfo {
            data_dir: format!("{NODES_DIR}/{id}"),
            id,
            name: name.get().trim().to_owned(),
            kind,
            network,
            depends_on: if kind.uses().is_empty() {
                None
            } else {
                uses.get()
            },
            ports,
        }
    });
    // Ports that moved, and who has the one they wanted.
    let moved = move || {
        let info = draft.get();
        let nodes = existing();
        kind.get()
            .default_ports(network.get())
            .into_iter()
            .zip(info.ports)
            .filter(|(wanted, got)| wanted.port != got.port)
            .map(|(wanted, got)| {
                let holder = nodes
                    .iter()
                    .find(|node| node.ports.iter().any(|port| port.port == wanted.port))
                    .map_or_else(|| "another node".to_owned(), |node| node.name.clone());
                format!(
                    "{} {} instead of {}, which {holder} uses",
                    got.role.label(),
                    got.port,
                    wanted.port
                )
            })
            .collect::<Vec<_>>()
    };
    // Why Next or Add is off, if it is.
    let problem = move || -> Option<String> {
        let info = draft.get();
        if info.name.is_empty() {
            return Some("Give the node a name.".into());
        }
        if program_prompt(info.kind).is_some() && program.with(|p| p.trim().is_empty()) {
            let (what, _) = program_prompt(info.kind).unwrap_or_default();
            return Some(format!("Choose the {what}."));
        }
        if !info.kind.uses().is_empty() && info.depends_on.is_none() {
            let wanted: Vec<&str> = info.kind.uses().iter().map(|kind| kind.label()).collect();
            return Some(format!(
                "{} needs a {} node on {} to use: add one first.",
                info.kind.label(),
                wanted.join(" or "),
                info.network
            ));
        }
        None
    };

    // Focus goes back where it was, or to the sidebar's "Add node" where the browser did not
    // focus the clicked button (WebKit does not).
    let restore_focus = move || {
        let target = return_focus
            .get_value()
            .filter(|element| element.is_connected() && element.tag_name() != "BODY")
            .or_else(|| {
                web_sys::window()
                    .and_then(|window| window.document())
                    .and_then(|document| document.query_selector(".side-add").ok().flatten())
                    .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
            });
        if let Some(target) = target {
            request_animation_frame(move || {
                let _ = target.focus();
            });
        }
    };
    let close = move || {
        if !busy.get_untracked() {
            open.set(false);
            restore_focus();
        }
    };
    // Escape closes the dialog wherever focus is, as the tooltip's does.
    let escape = window_event_listener(ev::keydown, move |event| {
        if event.key() == "Escape" && open.get_untracked() {
            close();
        }
    });
    on_cleanup(move || escape.remove());
    // The title takes focus when the dialog opens and at every step, so keys reach the
    // dialog and the new step is read out.
    let heading = NodeRef::<html::H2>::new();
    Effect::new(move |_| {
        let _ = step.get();
        if open.get() {
            request_animation_frame(move || {
                if let Some(heading) = heading.get_untracked() {
                    let _ = heading.focus();
                }
            });
        }
    });
    let add = move |start: bool| {
        let info = draft.get_untracked();
        busy.set(true);
        error.set(None);
        spawn_local(async move {
            match api::add_node(info.clone()).await {
                Ok(()) => {
                    let node = NodeState::new(info, false);
                    state.nodes.update(|nodes| nodes.push(node));
                    crate::app::subscribe_extra(state, node);
                    state.selected.set(node);
                    state.tab.set(crate::app::Tab::Dashboard);
                    busy.set(false);
                    open.set(false);
                    restore_focus();
                    if start && let Err(failure) = node.start().await {
                        state.backend_error.set(Some(failure.to_string()));
                    }
                }
                Err(failure) => {
                    busy.set(false);
                    error.set(Some(failure.to_string()));
                }
            }
        });
    };

    let kind_step = move || {
        view! {
            <p>"What should the new node run?"</p>
            <div class="kind-cards" role="radiogroup" aria-label="Kind">
                {NodeKind::ALL
                    .into_iter()
                    .map(|choice| {
                        view! {
                            <button
                                type="button"
                                role="radio"
                                class="kind-card"
                                class:selected=move || kind.get() == choice
                                aria-checked=move || (kind.get() == choice).to_string()
                                on:click=move |_| kind.set(choice)
                                on:dblclick=move |_| {
                                    kind.set(choice);
                                    step.set(Step::Details);
                                }
                            >
                                <span class="kind-card-name">{choice.label()}</span>
                                <span class="kind-card-role muted">{role_text(choice.role())}</span>
                                <span class="kind-card-summary">{choice.summary()}</span>
                                <span class="kind-card-needs muted">{requirement(choice)}</span>
                            </button>
                        }
                    })
                    .collect_view()}
            </div>
        }
    };

    let details_step = move || {
        let current = kind.get();
        view! {
            <div class="form-grid">
                <span class="form-label" id="add-node-network">"Network"</span>
                <div class="dash-segmented" role="radiogroup" aria-labelledby="add-node-network">
                    {networks(current)
                        .iter()
                        .map(|&choice| {
                            view! {
                                <button
                                    type="button"
                                    role="radio"
                                    class:active=move || network.get() == choice
                                    aria-checked=move || (network.get() == choice).to_string()
                                    on:click=move |_| network.set(choice)
                                >
                                    {choice.as_str()}
                                </button>
                            }
                        })
                        .collect_view()}
                </div>
                <label class="form-label" for="add-node-name">"Name"</label>
                <input
                    id="add-node-name"
                    type="text"
                    prop:value=move || name.get()
                    on:input=move |event| {
                        name_edited.set(true);
                        name.set(event_target_value(&event));
                    }
                />
                {program_prompt(current)
                    .map(|(what, example)| {
                        view! {
                            <label class="form-label" for="add-node-program">
                                "Program"
                            </label>
                            <div class="form-row">
                                <input
                                    id="add-node-program"
                                    type="text"
                                    class="cfg-mono"
                                    spellcheck="false"
                                    placeholder=example
                                    prop:value=move || program.get()
                                    on:input=move |event| program.set(event_target_value(&event))
                                />
                                <button type="button" on:click=move |_| program.set(example.to_owned())>
                                    "Choose…"
                                </button>
                            </div>
                            <span></span>
                            <span class="muted form-note">{format!("Your {what}.")}</span>
                        }
                        .into_any()
                    })
                    .unwrap_or_else(|| {
                        view! {
                            <span class="form-label">"Program"</span>
                            <span>"The bundled zebrad 7.0.0"</span>
                        }
                        .into_any()
                    })}
                {(!current.uses().is_empty())
                    .then(|| {
                        view! {
                            <span class="form-label" id="add-node-uses">"Uses"</span>
                            <div role="radiogroup" aria-labelledby="add-node-uses" class="uses-list">
                                {move || {
                                    let list = candidates();
                                    if list.is_empty() {
                                        return view! {
                                            <span class="warn">
                                                {format!(
                                                    "No node {} can use on {} yet.",
                                                    current.label(),
                                                    network.get(),
                                                )}
                                            </span>
                                        }
                                        .into_any();
                                    }
                                    list.into_iter()
                                        .map(|info| {
                                            let id = info.id.clone();
                                            let picked = {
                                                let id = id.clone();
                                                move || uses.with(|used| used.as_ref() == Some(&id))
                                            };
                                            view! {
                                                <label class="radio-row">
                                                    <input
                                                        type="radio"
                                                        name="add-node-uses"
                                                        prop:checked=picked
                                                        on:change=move |_| uses.set(Some(id.clone()))
                                                    />
                                                    {info.name.clone()}
                                                </label>
                                            }
                                        })
                                        .collect_view()
                                        .into_any()
                                }}
                            </div>
                        }
                    })}
                <span class="form-label">"Data folder"</span>
                <code class="form-value">{move || draft.with(|info| info.data_dir.clone())}</code>
                <span class="form-label">"Ports"</span>
                <div class="form-value">
                    <span>{move || draft.with(|info| ports_line(&info.ports))}</span>
                    {move || {
                        moved()
                            .into_iter()
                            .map(|line| view! { <p class="muted form-note">{line}</p> })
                            .collect_view()
                    }}
                </div>
            </div>
            {caveat(current).map(|text| view! { <div class="banner banner-warn">{text}</div> })}
        }
    };

    let review_step = move || {
        let info = draft.get();
        let uses_name = info.depends_on.as_ref().and_then(|used| {
            existing()
                .into_iter()
                .find(|node| &node.id == used)
                .map(|node| node.name)
        });
        let program_text = if info.kind == NodeKind::Zebrad {
            "the bundled zebrad 7.0.0".to_owned()
        } else {
            program.get().trim().to_owned()
        };
        view! {
            <p>"Zeal will set this node up, stopped. You can start it now or later."</p>
            <dl class="rows">
                <dt>"Name"</dt>
                <dd>{info.name.clone()}</dd>
                <dt>"Kind"</dt>
                <dd>{format!("{} · {}", info.kind.label(), role_text(info.kind.role()))}</dd>
                <dt>"Network"</dt>
                <dd>{info.network.as_str()}</dd>
                <dt>"Program"</dt>
                <dd><code>{program_text}</code></dd>
                {uses_name.map(|used| view! { <dt>"Uses"</dt><dd>{used}</dd> })}
                <dt>"Data folder"</dt>
                <dd><code>{info.data_dir.clone()}</code></dd>
                <dt>"Ports"</dt>
                <dd>{ports_line(&info.ports)}</dd>
                <dt>"Id"</dt>
                <dd><code>{info.id.to_string()}</code></dd>
            </dl>
        }
    };

    let body = move || match step.get() {
        Step::Kind => kind_step().into_any(),
        Step::Details => details_step().into_any(),
        Step::Review => review_step().into_any(),
    };
    let title = move || match step.get() {
        Step::Kind => "Add a node".to_owned(),
        Step::Details => format!("A {} node", kind.get().label()),
        Step::Review => format!("Add {}?", draft.with(|info| info.name.clone())),
    };
    let number = move || match step.get() {
        Step::Kind => 1,
        Step::Details => 2,
        Step::Review => 3,
    };
    let footer = move || {
        let current = step.get();
        view! {
            <div class="buttons wizard-nav">
                <button type="button" on:click=move |_| close()>"Cancel"</button>
                <span class="spacer"></span>
                <button
                    type="button"
                    disabled=move || busy.get() || current == Step::Kind
                    on:click=move |_| {
                        step.set(if current == Step::Review { Step::Details } else { Step::Kind })
                    }
                >
                    "Back"
                </button>
                {match current {
                    Step::Kind => view! {
                        <button type="button" class="primary" on:click=move |_| step.set(Step::Details)>
                            "Next"
                        </button>
                    }
                    .into_any(),
                    Step::Details => view! {
                        <button
                            type="button"
                            class="primary"
                            disabled=move || problem().is_some()
                            title=move || problem().unwrap_or_default()
                            on:click=move |_| step.set(Step::Review)
                        >
                            "Review"
                        </button>
                    }
                    .into_any(),
                    Step::Review => view! {
                        <button type="button" disabled=move || busy.get() on:click=move |_| add(false)>
                            "Add"
                        </button>
                        <button
                            type="button"
                            class="primary"
                            disabled=move || busy.get()
                            on:click=move |_| add(true)
                        >
                            "Add and start"
                        </button>
                    }
                    .into_any(),
                }}
            </div>
        }
    };
    move || {
        open.get().then(|| {
            view! {
                <div class="modal-backdrop">
                    <div class="modal modal-wide" role="dialog" aria-modal="true" aria-labelledby="add-node-title">
                        <p class="muted step-count">{move || format!("Step {} of 3", number())}</p>
                        <h2 id="add-node-title" tabindex="-1" node_ref=heading>{title}</h2>
                        <div>{body}</div>
                        {move || {
                            (step.get() == Step::Details)
                                .then(problem)
                                .flatten()
                                .map(|text| view! { <p class="muted form-note">{text}</p> })
                        }}
                        {move || error.get().map(|text| view! { <p class="error" role="alert">{text}</p> })}
                        {footer}
                    </div>
                </div>
            }
        })
    }
}

fn role_text(role: NodeRole) -> &'static str {
    match role {
        NodeRole::FullNode => "full node",
        NodeRole::Indexer => "indexer",
        NodeRole::Wallet => "wallet",
        NodeRole::Custom => "your program",
    }
}

#[cfg(test)]
mod tests {
    use launcher_types::PortRole;

    use super::*;

    #[test]
    fn ports_read_as_one_line() {
        let ports = [
            NodePort {
                role: PortRole::P2p,
                port: 18233,
            },
            NodePort {
                role: PortRole::Rpc,
                port: 18232,
            },
        ];
        assert_eq!(ports_line(&ports), "P2P 18233 · RPC 18232");
        assert_eq!(ports_line(&[]), "none");
    }

    #[test]
    fn every_kind_has_words_and_a_network() {
        for kind in NodeKind::ALL {
            assert!(!requirement(kind).is_empty());
            assert!(!networks(kind).is_empty());
            assert_eq!(program_prompt(kind).is_none(), kind.bundled(), "{kind:?}");
        }
    }
}
