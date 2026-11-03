//! A node's own settings, a tab of the node header when Zeal runs several nodes: its name,
//! whether it starts when Zeal opens, and removing it, keeping or deleting its data. (With one
//! node, Settings holds what it has; a single node cannot be removed.) Its folder and ports
//! are shown, not edited, for now.

use launcher_types::{NodeInfo, NodeKind, NodeUpdate};
use leptos::{prelude::*, task::spawn_local};

use crate::{
    api,
    app::{AppState, Tab},
    components::{
        add_node::ports_line, outcome_note::OutcomeNote, rows::Rows, transient::Transient,
        typed_confirm::TypedConfirm,
    },
    format::state_dir,
    pages::settings::DELETE_WORD,
};

/// What the form edits.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Fields {
    name: String,
    autostart: bool,
}

impl Fields {
    fn of(info: &NodeInfo) -> Self {
        Self {
            name: info.name.clone(),
            autostart: info.autostart,
        }
    }

    /// What `update_node` changes to make `stored` read as `self`; `None` when nothing does.
    /// The name is compared as it would be saved, trimmed.
    fn update(&self, stored: &NodeInfo) -> Option<NodeUpdate> {
        let name = self.name.trim();
        let update = NodeUpdate {
            name: (name != stored.name).then(|| name.to_owned()),
            autostart: (self.autostart != stored.autostart).then_some(self.autostart),
            ..NodeUpdate::default()
        };
        (update != NodeUpdate::default()).then_some(update)
    }
}

/// Why the name cannot be saved, if it cannot: empty, too long, or another node's.
fn name_problem(name: &str, others: &[String]) -> Option<String> {
    let name = name.trim();
    if name.is_empty() {
        return Some("Give the node a name.".into());
    }
    if name.chars().count() > NodeInfo::MAX_NAME_LEN {
        return Some(format!(
            "Keep the name to {} characters.",
            NodeInfo::MAX_NAME_LEN
        ));
    }
    others
        .iter()
        .any(|other| other.trim().to_lowercase() == name.to_lowercase())
        .then(|| format!("Another node is called {name}."))
}

/// What a removal does with the node's data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Keep {
    Data,
    Nothing,
}

#[component]
pub fn NodeSettings() -> impl IntoView {
    let state = AppState::get();
    let node = move || state.node();
    let fields = RwSignal::new(None::<Fields>);
    let saving = RwSignal::new(false);
    let outcome = Transient::<Result<String, String>>::new();
    // The form follows the node: another node picked, or a change saved elsewhere while the
    // form is not edited.
    Effect::new(
        move |previous: Option<(crate::node_state::NodeState, Fields)>| {
            let node = node();
            let stored = node.info.with(Fields::of);
            let untouched = match &previous {
                Some((before, before_stored)) => {
                    *before != node
                        || fields.with_untracked(|fields| fields.as_ref() == Some(before_stored))
                }
                None => true,
            };
            if untouched {
                fields.set(Some(stored.clone()));
                outcome.clear();
            }
            (node, stored)
        },
    );
    let others = move || {
        let current = node();
        state.nodes.with(|nodes| {
            nodes
                .iter()
                .filter(|other| **other != current)
                .map(|other| other.info.with(|info| info.name.clone()))
                .collect::<Vec<_>>()
        })
    };
    let problem = move || {
        fields.with(|fields| {
            fields
                .as_ref()
                .and_then(|fields| name_problem(&fields.name, &others()))
        })
    };
    let update = move || {
        let stored = node().info.get();
        fields.with(|fields| fields.as_ref().and_then(|fields| fields.update(&stored)))
    };
    let save = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        if saving.get_untracked() || untrack(problem).is_some() {
            return;
        }
        let Some(change) = untrack(update) else {
            return;
        };
        let target = state.node_untracked();
        saving.set(true);
        outcome.clear();
        spawn_local(async move {
            match api::update_node(&target.id(), change).await {
                Ok(info) => {
                    fields.set(Some(Fields::of(&info)));
                    target.info.set(info);
                    state.refresh_nodes();
                    outcome.report(Ok("Saved.".into()));
                }
                Err(error) => outcome.report(Err(format!("Could not save: {error}"))),
            }
            saving.set(false);
        });
    };

    view! {
        <Show
            when=move || fields.with(Option::is_some)
            fallback=|| view! { <p class="muted">"Loading the node…"</p> }
        >
            <form class="settings node-settings" on:submit=save>
                <h2>"Node settings"</h2>
                <fieldset>
                    <legend>"Node"</legend>
                    <label class="field">
                        <span>"Name"</span>
                        <input
                            type="text"
                            maxlength=NodeInfo::MAX_NAME_LEN
                            class:invalid=move || problem().is_some()
                            prop:value=move || {
                                fields.with(|f| f.as_ref().map(|f| f.name.clone()).unwrap_or_default())
                            }
                            on:input=move |event| {
                                fields.update(|f| {
                                    if let Some(f) = f {
                                        f.name = event_target_value(&event);
                                    }
                                });
                                outcome.clear();
                            }
                        />
                        <span class="muted">"How the sidebar, the overview and the tray name it."</span>
                        {move || problem().map(|text| view! { <span class="error">{text}</span> })}
                    </label>
                    <label class="check">
                        <input
                            type="checkbox"
                            prop:checked=move || fields.with(|f| f.as_ref().is_some_and(|f| f.autostart))
                            on:change=move |event| {
                                fields.update(|f| {
                                    if let Some(f) = f {
                                        f.autostart = event_target_checked(&event);
                                    }
                                });
                                outcome.clear();
                            }
                        />
                        "Start this node when Zeal opens"
                    </label>
                    <div class="buttons">
                        <button
                            type="button"
                            disabled=move || saving.get() || update().is_none()
                            on:click=move |_| {
                                fields.set(Some(node().info.with_untracked(Fields::of)));
                                outcome.clear();
                            }
                        >
                            "Revert"
                        </button>
                        <button
                            type="submit"
                            class="primary"
                            disabled=move || saving.get() || update().is_none() || problem().is_some()
                        >
                            {move || if saving.get() { "Saving…" } else { "Save" }}
                        </button>
                        <OutcomeNote outcome=outcome.signal() />
                    </div>
                </fieldset>
                <fieldset>
                    <legend>"Files and ports"</legend>
                    <Rows rows=vec![
                        ("Id", (move || node().info.with(|info| info.id.to_string())).into_any()),
                        (
                            "Data folder",
                            (move || view! { <code>{node().info.with(|info| info.data_dir.clone())}</code> })
                                .into_any(),
                        ),
                        ("Ports", (move || node().info.with(|info| ports_line(&info.ports))).into_any()),
                    ] />
                    {move || {
                        (node().info.with(|info| info.kind) == NodeKind::Zebrad)
                            .then(|| view! { <p class="muted">"zebrad's own settings are on the Configure tab."</p> })
                    }}
                </fieldset>
            </form>
            <section class="settings node-settings">
                <RemoveNode />
            </section>
        </Show>
    }
}

/// "Remove node…": stops the node and takes it off the list, keeping its data folder or
/// deleting its chain state (typing [`DELETE_WORD`] to confirm). Zeal keeps at least one node.
#[component]
fn RemoveNode() -> impl IntoView {
    let state = AppState::get();
    let node = move || state.node();
    let keep = RwSignal::new(Keep::Data);
    let confirming = RwSignal::new(false);
    let removing = RwSignal::new(false);
    let outcome = Transient::<Result<String, String>>::new();
    // Another node picked: start over.
    Effect::new(move |_| {
        let _ = node();
        confirming.set(false);
        keep.set(Keep::Data);
        outcome.clear();
    });
    let last = move || state.nodes.with(Vec::len) < 2;
    let name = move || node().info.with(|info| info.name.clone());
    let remove = move || {
        if removing.get_untracked() {
            return;
        }
        let target = state.node_untracked();
        let delete = keep.get_untracked() == Keep::Nothing;
        let name = target.info.with_untracked(|info| info.name.clone());
        removing.set(true);
        outcome.clear();
        spawn_local(async move {
            let result = api::remove_node(&target.id(), delete).await;
            // A removal that could not delete all of the node's files has still taken the node
            // off the list: the backend saves the list first. Its error says so.
            let gone = match &result {
                Ok(_) => true,
                Err(_) => api::list_nodes()
                    .await
                    .is_ok_and(|list| list.iter().all(|info| info.id != target.id())),
            };
            if gone {
                // The page goes with the node; the overview shows the nodes left, and what
                // could not be deleted goes to the error banner.
                state.refresh_nodes();
                state.tab.set(Tab::Overview);
                if let Err(error) = result {
                    state.backend_error.set(Some(error.to_string()));
                }
            } else if let Err(error) = result {
                outcome.report(Err(format!("Could not remove {name}: {error}")));
            }
            removing.set(false);
            confirming.set(false);
        });
    };
    let choice = move |value: Keep, label: &'static str, explanation: &'static str| {
        view! {
            <label class="radio-row">
                <input
                    type="radio"
                    name="remove-node-data"
                    prop:checked=move || keep.get() == value
                    disabled=move || removing.get()
                    on:change=move |_| keep.set(value)
                />
                <span>{label}</span>
                <span class="muted">{explanation}</span>
            </label>
        }
    };
    view! {
        <fieldset>
            <legend>"Remove"</legend>
            <p class="muted">
                {move || {
                    if last() {
                        "Zeal needs a node: add another before removing this one.".to_owned()
                    } else {
                        format!("Stops {} and takes it off the list.", name())
                    }
                }}
            </p>
            {choice(Keep::Data, "Keep its data", "The folder stays, with the chain it synced.")}
            {choice(
                Keep::Nothing,
                "Delete its chain state",
                "Frees the disk space the chain uses.",
            )}
            <div class="buttons">
                <button
                    type="button"
                    class="danger remove-node"
                    disabled=move || last() || removing.get() || confirming.get()
                    on:click=move |_| {
                        outcome.clear();
                        confirming.set(true);
                    }
                >
                    "Remove node…"
                </button>
            </div>
            <Show when=move || confirming.get() && keep.get() == Keep::Data>
                <div class="banner banner-warn" role="alert">
                    <p>
                        {move || format!("Remove {}? It is stopped first. Its data folder stays: ", name())}
                        <code>{move || node().info.with(|info| info.data_dir.clone())}</code>
                    </p>
                    <div class="buttons">
                        <button
                            type="button"
                            class="danger"
                            disabled=move || removing.get()
                            on:click=move |_| remove()
                        >
                            {move || if removing.get() { "Removing…" } else { "Remove" }}
                        </button>
                        <button
                            type="button"
                            disabled=move || removing.get()
                            on:click=move |_| confirming.set(false)
                        >
                            "Cancel"
                        </button>
                    </div>
                </div>
            </Show>
            <Show when=move || confirming.get() && keep.get() == Keep::Nothing>
                <TypedConfirm
                    word=DELETE_WORD
                    action="Remove and delete"
                    busy_label="Removing…"
                    busy=Signal::derive(move || removing.get())
                    on_confirm=Callback::new(move |()| remove())
                    on_cancel=Callback::new(move |()| confirming.set(false))
                    class="delete-confirm"
                >
                    <p>
                        {move || format!("This stops {} and deletes its chain state in ", name())}
                        <code>{move || node().info.with(|info| state_dir(&info.data_dir))}</code>
                        {move || {
                            // A node added later has a folder of its own, which goes too; the
                            // first node keeps its files in the launcher's own folders, which
                            // stay.
                            match node().info.with(|info| info.own_folder.clone()) {
                                Some(folder) => view! {
                                    ", then its folder "
                                    <code>{folder}</code>
                                    ". The node leaves the list."
                                }
                                .into_any(),
                                None => "; its data folder and the launcher's own files stay. The node leaves the list."
                                    .into_any(),
                            }
                        }}
                    </p>
                </TypedConfirm>
            </Show>
            <OutcomeNote outcome=outcome.signal() block=true />
        </fieldset>
    }
}

#[cfg(test)]
mod tests {
    use launcher_types::{Network, NodeId, NodeKind, NodeProgram};

    use super::*;

    fn info() -> NodeInfo {
        NodeInfo {
            id: NodeId::new("zebrad-testnet").unwrap(),
            name: "Zebra · Testnet".into(),
            kind: NodeKind::Zebrad,
            network: Network::Testnet,
            depends_on: None,
            data_dir: "/data/nodes/zebrad-testnet".into(),
            own_folder: Some("/data/nodes/zebrad-testnet".into()),
            ports: NodeKind::Zebrad.default_ports(Network::Testnet),
            program: NodeProgram::Bundled,
            autostart: true,
            stop_grace_secs: 60,
        }
    }

    #[test]
    fn only_what_changed_is_sent() {
        let stored = info();
        assert_eq!(Fields::of(&stored).update(&stored), None);
        let padded = Fields {
            name: " Zebra · Testnet ".into(),
            ..Fields::of(&stored)
        };
        assert_eq!(
            padded.update(&stored),
            None,
            "a padded name is the same name"
        );
        let edited = Fields {
            name: "Test bench".into(),
            autostart: false,
        };
        assert_eq!(
            edited.update(&stored),
            Some(NodeUpdate {
                name: Some("Test bench".into()),
                autostart: Some(false),
                ..NodeUpdate::default()
            })
        );
    }

    #[test]
    fn a_name_is_not_empty_not_too_long_and_not_another_nodes() {
        let others = vec!["Zebra · Mainnet".to_owned()];
        assert_eq!(name_problem("Test bench", &others), None);
        assert!(name_problem("  ", &others).is_some());
        assert!(name_problem(&"x".repeat(NodeInfo::MAX_NAME_LEN + 1), &others).is_some());
        assert!(name_problem("zebra · mainnet", &others).is_some());
    }
}
