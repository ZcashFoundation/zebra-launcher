//! The launcher settings form, plus the data directory, diagnostics and "Delete node data".
//! Save sends the whole settings object to `set_settings`. With one node the form also holds
//! that node's start at launch, stop grace period and network, as it did when they were the
//! launcher's, and Save sends those that changed to `update_node`; its data folder changes
//! through `choose_data_dir` right away. With several nodes those fields are each node's and
//! leave this page.

use launcher_types::{
    Network, NodeInfo, NodeUpdate, OpenTarget, STOP_GRACE_SECS, Settings, Theme,
    is_loopback_http_url,
};
use leptos::{prelude::*, task::spawn_local};

use crate::{
    api,
    app::AppState,
    clipboard::copy_to_clipboard,
    components::{
        icons::warning_icon, outcome_note::OutcomeNote, public_services::PublicServices,
        radio_choice::RadioChoice, rows::Rows, transient::Transient, typed_confirm::TypedConfirm,
        update_mode_choice::UpdateModeChoice, updates::UpdateStatus,
    },
    format::{bytes, data_change_allowed, is_low_space, state_dir},
    theme::theme_text,
};

/// The appearance radios, in order.
const THEMES: [Theme; 3] = [Theme::System, Theme::Light, Theme::Dark];

/// The word to type before node data is deleted, here and in the first-run wizard.
pub(crate) const DELETE_WORD: &str = "delete";

/// Form fields that are typed as text and validated on Save.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct TextFields {
    grafana_url: String,
    stop_grace_secs: String,
}

impl TextFields {
    fn from_stored(settings: &Settings, node: &NodeChoices) -> Self {
        Self {
            grafana_url: settings.grafana_url.clone().unwrap_or_default(),
            stop_grace_secs: node.stop_grace_secs.to_string(),
        }
    }
}

/// The node's settings the form edits while the backend runs one node: what its Behaviour and
/// Node sections showed when they were the launcher's.
#[derive(Clone, Debug, PartialEq, Eq)]
struct NodeChoices {
    autostart: bool,
    network: Network,
    stop_grace_secs: u64,
}

impl NodeChoices {
    fn of(info: &NodeInfo) -> Self {
        Self {
            autostart: info.autostart,
            network: info.network,
            stop_grace_secs: info.stop_grace_secs,
        }
    }

    /// What `update_node` changes to make `stored` read as `self`; `None` when nothing does.
    fn update(&self, stored: &NodeChoices) -> Option<NodeUpdate> {
        let update = NodeUpdate {
            autostart: (self.autostart != stored.autostart).then_some(self.autostart),
            network: (self.network != stored.network).then_some(self.network),
            stop_grace_secs: (self.stop_grace_secs != stored.stop_grace_secs)
                .then_some(self.stop_grace_secs),
            name: None,
        };
        (update != NodeUpdate::default()).then_some(update)
    }
}

/// Checks the stop grace field against the range the backend accepts, [`STOP_GRACE_SECS`].
fn parse_stop_grace(text: &str) -> Result<u64, String> {
    match text.trim().parse::<u64>() {
        Ok(secs) if STOP_GRACE_SECS.contains(&secs) => Ok(secs),
        _ => Err(format!(
            "Enter a whole number of seconds from {} to {}.",
            STOP_GRACE_SECS.start(),
            STOP_GRACE_SECS.end()
        )),
    }
}

/// Checks the Grafana URL field. Empty means "no dashboard"; anything else must pass the rule
/// the backend also enforces, [`is_loopback_http_url`], which mirrors the CSP's `frame-src`.
fn parse_grafana_url(input: &str) -> Result<Option<String>, String> {
    let url = input.trim();
    if url.is_empty() {
        Ok(None)
    } else if is_loopback_http_url(url) {
        Ok(Some(url.to_owned()))
    } else {
        Err(
            "Grafana must run on this computer: use http://127.0.0.1 or http://localhost, \
             with an optional port and path."
                .into(),
        )
    }
}

/// True when the form differs from what is stored (`stored` and the node's `stored_node`): a
/// choice in `draft` or `node_draft`, or a text field that reads differently. A text that does
/// not parse differs whatever is stored, even where its parsed value would read as the stored
/// one (a bad URL where none is stored): the user typed something the form will not save, so
/// Revert is offered and a change made elsewhere does not refill the form over it.
fn draft_differs(
    stored: &Settings,
    stored_node: &NodeChoices,
    draft: &Settings,
    node_draft: &NodeChoices,
    fields: &TextFields,
) -> bool {
    let (Ok(grafana_url), Ok(stop_grace_secs)) = (
        parse_grafana_url(&fields.grafana_url),
        parse_stop_grace(&fields.stop_grace_secs),
    ) else {
        return true;
    };
    let edited = Settings {
        grafana_url,
        ..draft.clone()
    };
    let edited_node = NodeChoices {
        stop_grace_secs,
        ..node_draft.clone()
    };
    edited != *stored || edited_node != *stored_node
}

/// "212.4 GB free", or a note that it is unknown: the data folder row here and the wizard's
/// data step.
pub(crate) fn free_space_text(free: Option<u64>) -> String {
    match free {
        Some(free) => format!("{} free", bytes(free)),
        None => "free space unknown".to_owned(),
    }
}

/// The note after the data folder was changed: the free space there, with the warning when
/// Mainnet would not fit.
fn data_dir_saved_text(free: Option<u64>) -> String {
    let mut text = match free {
        Some(free) => format!("Saved. {} free there.", bytes(free)),
        None => "Saved. The free space there could not be read.".to_owned(),
    };
    if is_low_space(free) {
        text.push_str(" Mainnet needs hundreds of GB; this drive may fill up.");
    }
    text
}

/// A change in flight on this page. Only one runs at a time: Save sends the whole settings
/// object, so a Save racing a data folder change could store the folder it read before the
/// change, and neither should run while node data is being deleted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mutation {
    Save,
    DataDir,
    DeleteNodeData,
}

#[component]
pub fn SettingsPage() -> impl IntoView {
    let state = AppState::get();
    let draft = RwSignal::new(None::<Settings>);
    let node_draft = RwSignal::new(None::<NodeChoices>);
    let fields = RwSignal::new(TextFields::default());
    let busy = RwSignal::new(None::<Mutation>);
    let saving = move || busy.get() == Some(Mutation::Save);
    // The outcome of Save, or why it is refused; the next edit clears it.
    let outcome = Transient::<Result<String, String>>::new();
    // The node the form's node fields are about, as stored: the selected one, which with one
    // node is the only one.
    let stored_node = Memo::new(move |_| {
        state
            .nodes_loaded
            .get()
            .then(|| state.node().info.with(NodeChoices::of))
    });

    let reset = move || {
        let loaded = state.settings.get_untracked();
        let node = stored_node.get_untracked();
        fields.set(match (&loaded, &node) {
            (Some(loaded), Some(node)) => TextFields::from_stored(loaded, node),
            _ => TextFields::default(),
        });
        draft.set(loaded);
        node_draft.set(node);
    };
    // Fill the form when the settings and the node arrive, and again when they change
    // elsewhere (the first-run flow, another window) while the form is not edited. The data
    // folder is not a form field, so it does not count as an edit.
    Effect::new(
        move |previous: Option<(Option<Settings>, Option<NodeChoices>)>| {
            let loaded = state.settings.get();
            let node = stored_node.get();
            let untouched = match &previous {
                Some((Some(previous), Some(previous_node))) => draft.with_untracked(|draft| {
                    node_draft.with_untracked(|node_draft| match (draft, node_draft) {
                        (Some(draft), Some(node_draft)) => !fields.with_untracked(|fields| {
                            draft_differs(previous, previous_node, draft, node_draft, fields)
                        }),
                        _ => true,
                    })
                }),
                _ => true,
            };
            if loaded.is_some() && node.is_some() && untouched {
                reset();
            }
            (loaded, node)
        },
    );

    // The Appearance choice shows at once: the shell's theme effect reads the draft's theme
    // while it differs from the stored one. Save stores it; Revert puts the draft back.
    Effect::new(move |_| {
        let chosen = draft.with(|d| d.as_ref().map(|d| d.theme));
        let stored = state.settings.with(|s| s.as_ref().map(|s| s.theme));
        state
            .preview_theme
            .set(chosen.filter(|chosen| Some(*chosen) != stored));
    });

    let grafana_error =
        Memo::new(move |_| fields.with(|f| parse_grafana_url(&f.grafana_url).err()));
    let grace_error =
        Memo::new(move |_| fields.with(|f| parse_stop_grace(&f.stop_grace_secs).err()));
    // Save and Revert are offered while the form differs from what is stored.
    let dirty = move || {
        draft.with(|draft| {
            node_draft.with(|node_draft| {
                let (Some(draft), Some(node_draft)) = (draft, node_draft) else {
                    return false;
                };
                state.settings.with(|stored| {
                    stored_node.with(|stored_node| match (stored, stored_node) {
                        (Some(stored), Some(stored_node)) => fields.with(|fields| {
                            draft_differs(stored, stored_node, draft, node_draft, fields)
                        }),
                        _ => true,
                    })
                })
            })
        })
    };

    let save = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        if busy.get_untracked().is_some() {
            return;
        }
        let (Some(mut settings), Some(node_choices)) =
            (draft.get_untracked(), node_draft.get_untracked())
        else {
            return;
        };
        let f = fields.get_untracked();
        let (grafana_url, stop_grace_secs) = match (
            parse_grafana_url(&f.grafana_url),
            parse_stop_grace(&f.stop_grace_secs),
        ) {
            (Ok(url), Ok(secs)) => (url, secs),
            _ => {
                outcome.report(Err("Fix the highlighted fields first.".into()));
                return;
            }
        };
        settings.grafana_url = grafana_url;
        let edited_node = NodeChoices {
            stop_grace_secs,
            ..node_choices
        };
        let stored = state.settings.get_untracked();
        let node_update = stored_node
            .get_untracked()
            .and_then(|stored_node| edited_node.update(&stored_node));
        let node = state.node_untracked();
        busy.set(Some(Mutation::Save));
        outcome.clear();
        spawn_local(async move {
            let mut saved = Ok(());
            if stored.as_ref() != Some(&settings) {
                match api::set_settings(settings).await {
                    Ok(stored) => state.settings.set(Some(stored)),
                    Err(error) => saved = Err(error),
                }
            }
            if saved.is_ok()
                && let Some(update) = node_update
            {
                match api::update_node(&node.id(), update).await {
                    Ok(info) => {
                        node.info.set(info);
                        state.refresh_nodes();
                    }
                    Err(error) => saved = Err(error),
                }
            }
            match saved {
                Ok(()) => {
                    reset();
                    outcome.report(Ok("Saved.".into()));
                }
                Err(error) => outcome.report(Err(format!("Could not save: {error}"))),
            }
            busy.set(None);
        });
    };

    let update = move |change: fn(&mut Settings, bool), value: bool| {
        draft.update(|draft| {
            if let Some(draft) = draft {
                change(draft, value);
            }
        });
        outcome.clear();
    };
    let read = move |pick: fn(&Settings) -> bool| {
        move || draft.with(|draft| draft.as_ref().is_some_and(pick))
    };
    let update_node = move |change: fn(&mut NodeChoices, bool), value: bool| {
        node_draft.update(|draft| {
            if let Some(draft) = draft {
                change(draft, value);
            }
        });
        outcome.clear();
    };
    // With several nodes, a node's own settings are on its pages, not here.
    let one_node = move || !state.herd();
    view! {
        <Show
            when=move || draft.with(Option::is_some) && node_draft.with(Option::is_some)
            fallback=|| view! { <p class="muted">"Loading settings…"</p> }
        >
            <form class="settings" on:submit=save>
                <h2>"Settings"</h2>

                <fieldset>
                    <legend>"Updates"</legend>
                    <UpdateModeChoice
                        name="update-mode"
                        selected=Signal::derive(move || {
                            draft.with(|d| d.as_ref().map(|d| d.update_mode))
                        })
                        on_select=Callback::new(move |mode| {
                            draft.update(|d| {
                                if let Some(d) = d {
                                    d.update_mode = mode;
                                }
                            });
                            outcome.clear();
                        })
                    />
                    <UpdateStatus />
                </fieldset>

                <fieldset>
                    <legend>"Behaviour"</legend>
                    <label class="check">
                        <input
                            type="checkbox"
                            prop:checked=read(|s| s.close_to_tray)
                            on:change=move |event| {
                                update(|s, v| s.close_to_tray = v, event_target_checked(&event))
                            }
                        />
                        "Keep running in the tray when the window is closed"
                    </label>
                    <Show when=one_node>
                    <label class="check">
                        <input
                            type="checkbox"
                            prop:checked=move || {
                                node_draft.with(|draft| draft.as_ref().is_some_and(|d| d.autostart))
                            }
                            on:change=move |event| {
                                update_node(|d, v| d.autostart = v, event_target_checked(&event))
                            }
                        />
                        "Start the node when the launcher opens"
                    </label>
                    <label class="field">
                        <span>"Stop grace period (seconds)"</span>
                        <input
                            type="number"
                            min=*STOP_GRACE_SECS.start()
                            max=*STOP_GRACE_SECS.end()
                            class:invalid=move || grace_error.get().is_some()
                            prop:value=move || fields.with(|f| f.stop_grace_secs.clone())
                            on:input=move |event| {
                                fields.update(|f| f.stop_grace_secs = event_target_value(&event));
                                outcome.clear();
                            }
                        />
                        <span class="muted">"How long zebrad gets to shut down before it is killed."</span>
                        {move || grace_error.get().map(|e| view! { <span class="error">{e}</span> })}
                    </label>
                    </Show>
                </fieldset>

                <fieldset>
                    <legend>"Appearance"</legend>
                    <RadioChoice
                        name="theme"
                        options=THEMES
                            .into_iter()
                            .map(|theme| {
                                let (label, explanation) = theme_text(theme);
                                (theme, label, explanation)
                            })
                            .collect()
                        selected=Signal::derive(move || draft.with(|d| d.as_ref().map(|d| d.theme)))
                        on_select=Callback::new(move |theme| {
                            draft.update(|d| {
                                if let Some(d) = d {
                                    d.theme = theme;
                                }
                            });
                            outcome.clear();
                        })
                        value=Theme::as_str
                    />
                </fieldset>

                <Show when=one_node>
                <fieldset>
                    <legend>"Node"</legend>
                    <label class="field">
                        <span>"Network"</span>
                        <select
                            prop:value=move || {
                                node_draft.with(|d| match d.as_ref().map(|d| d.network) {
                                    Some(Network::Testnet) => "testnet",
                                    _ => "mainnet",
                                })
                            }
                            on:change=move |event| {
                                let network = match event_target_value(&event).as_str() {
                                    "testnet" => Network::Testnet,
                                    _ => Network::Mainnet,
                                };
                                node_draft.update(|d| {
                                    if let Some(d) = d {
                                        d.network = network;
                                    }
                                });
                                outcome.clear();
                            }
                        >
                            <option value="mainnet">"Mainnet"</option>
                            <option value="testnet">"Testnet"</option>
                        </select>
                    </label>
                    <DataDirRow busy />
                    <p class="muted cfg-settings-pointer">
                        "zebrad's own settings are on the Configure tab."
                    </p>
                </fieldset>
                </Show>

                <Nodes />

                <fieldset>
                    <legend>"Metrics"</legend>
                    <label class="field">
                        <span>"Grafana dashboard URL"</span>
                        <input
                            type="url"
                            placeholder="http://127.0.0.1:3000/d/zebra-overview"
                            class:invalid=move || grafana_error.get().is_some()
                            prop:value=move || fields.with(|f| f.grafana_url.clone())
                            on:input=move |event| {
                                fields.update(|f| f.grafana_url = event_target_value(&event));
                                outcome.clear();
                            }
                        />
                        <span class="muted">
                            "Embedded in the Grafana tab. Only Grafana on this computer (http://127.0.0.1 or http://localhost, any port) can be embedded; leave empty and the tab explains how to set one up."
                        </span>
                        {move || grafana_error.get().map(|e| view! { <span class="error">{e}</span> })}
                    </label>
                </fieldset>

                <div class="buttons">
                    <button
                        type="button"
                        disabled=move || busy.get().is_some() || !dirty()
                        on:click=move |_| {
                            reset();
                            outcome.clear();
                        }
                    >
                        "Revert"
                    </button>
                    <button
                        type="submit"
                        class="primary"
                        disabled=move || {
                            busy.get().is_some() || !dirty() || grafana_error.get().is_some()
                                || grace_error.get().is_some()
                        }
                    >
                        {move || if saving() { "Saving…" } else { "Save" }}
                    </button>
                    <OutcomeNote outcome=outcome.signal() />
                </div>
            </form>
            <PublicServices />
            <section class="settings">
                <Diagnostics />
                <Show when=one_node>
                    <DeleteNodeData busy />
                </Show>
                <About />
            </section>
        </Show>
    }
}

/// True while the backend accepts data changes for the selected node (stopped or failed).
fn data_change_open(state: AppState) -> impl Fn() -> bool + Copy {
    move || {
        state
            .node()
            .status
            .with(|status| data_change_allowed(&status.phase))
    }
}

/// The node's data folder, with "Change…" (folder picker, then `choose_data_dir`). It is saved
/// right away, not by the form's Save.
#[component]
fn DataDirRow(busy: RwSignal<Option<Mutation>>) -> impl IntoView {
    let state = AppState::get();
    let allowed = data_change_open(state);
    let outcome = Transient::<Result<String, String>>::new();
    // The free space at the folder in use, read (read-only) when the row mounts and whenever
    // the folder changes.
    let dir_in_use = Memo::new(move |_| {
        state
            .nodes_loaded
            .get()
            .then(|| state.node().info.with(|info| info.data_dir.clone()))
    });
    // `None` while unread; `Some(None)` when the backend could not tell.
    let free_in_use = RwSignal::new(None::<Option<u64>>);
    Effect::new(move |_| {
        free_in_use.set(None);
        let Some(dir) = dir_in_use.get() else {
            return;
        };
        spawn_local(async move {
            let free = api::free_space(dir.clone()).await.ok().flatten();
            // Unless the folder changed meanwhile.
            if dir_in_use.with_untracked(|current| current.as_ref() == Some(&dir)) {
                free_in_use.set(Some(free));
            }
        });
    });

    let change = move |_| {
        if busy.get_untracked().is_some() {
            return;
        }
        busy.set(Some(Mutation::DataDir));
        outcome.clear();
        let node = state.node_untracked();
        spawn_local(async move {
            // Nothing to say when the picker was cancelled.
            let saved = match api::pick_data_dir().await {
                Ok(None) => None,
                Ok(Some(path)) => Some(match api::choose_data_dir(&node.id(), path).await {
                    Ok(choice) => {
                        node.info.set(choice.node);
                        Ok(data_dir_saved_text(choice.free_bytes))
                    }
                    Err(error) => Err(format!("Could not use that folder: {error}")),
                }),
                Err(error) => Some(Err(format!("Could not open the folder picker: {error}"))),
            };
            if let Some(saved) = saved {
                outcome.report(saved);
            }
            busy.set(None);
        });
    };

    view! {
        <div class="field">
            <span>"Data folder"</span>
            <code class="readonly">
                {move || {
                    state
                        .chosen_data_dir(state.node())
                        .unwrap_or_else(|| "default (the launcher's data folder)".into())
                }}
            </code>
            {move || {
                free_in_use
                    .get()
                    .filter(|_| outcome.get().is_none())
                    .map(free_space_view)
            }}
            <div class="buttons">
                <button
                    type="button"
                    disabled=move || busy.get().is_some() || !allowed()
                    on:click=change
                >
                    "Change…"
                </button>
                <span class="muted">
                    {move || {
                        if allowed() {
                            "The chain state goes into its state folder. Changing it does not move the state already synced; that stays in the old folder until you delete it."
                        } else {
                            "Stop the node to change it."
                        }
                    }}
                </span>
            </div>
            <OutcomeNote outcome=outcome.signal() />
        </div>
    }
}

/// The nodes Zeal runs, with "Add a node…": with one node, the way to a second one, since the
/// sidebar with its "Add node" appears only once there are two.
#[component]
fn Nodes() -> impl IntoView {
    let state = AppState::get();
    let summary = move || {
        let nodes = state.nodes.get();
        match nodes.as_slice() {
            [node] => node.info.with(|info| {
                let other = match info.network {
                    Network::Mainnet => Network::Testnet,
                    Network::Testnet => Network::Mainnet,
                };
                format!(
                    "Zeal runs one node, {}. Add another to run {other} next to {}; the window then lists them in a sidebar.",
                    info.name, info.network
                )
            }),
            nodes => {
                let names: Vec<String> = nodes
                    .iter()
                    .map(|node| node.info.with(|info| info.name.clone()))
                    .collect();
                format!(
                    "Zeal runs {} nodes: {}. Each node's settings are on its pages.",
                    nodes.len(),
                    names.join(", ")
                )
            }
        }
    };
    view! {
        <fieldset>
            <legend>"Nodes"</legend>
            <p class="muted">{summary}</p>
            <div class="buttons">
                <button
                    type="button"
                    class="settings-add-node"
                    disabled=move || !state.nodes_loaded.get() || state.add_refused().is_some()
                    on:click=move |_| state.add_node.run(())
                >
                    "Add a node…"
                </button>
                {move || state.add_refused().map(|why| view! { <span class="muted">{why}</span> })}
            </div>
        </fieldset>
    }
}

/// The free space at the folder in use: `212.4 GB free`, in the warning tone with an icon when
/// Mainnet would not fit, or `free space unknown`: the data folder row here and the add-node
/// dialog's.
pub(crate) fn free_space_view(free: Option<u64>) -> impl IntoView {
    let low = is_low_space(free);
    view! {
        <span class="data-dir-free" class:warn=low class:muted=!low>
            {low.then(warning_icon)}
            {free_space_text(free)}
            {low.then_some(" (Mainnet needs hundreds of GB; this drive may fill up)")}
        </span>
    }
}

/// What is installed and where it keeps its files, read-only.
#[component]
fn About() -> impl IntoView {
    let state = AppState::get();
    let note = Transient::<Result<String, String>>::new();
    let open = move |target: OpenTarget, what: &'static str| {
        note.clear();
        let node = state.node_untracked().id();
        spawn_local(async move {
            if let Err(failure) = api::open_path(target, Some(&node)).await {
                note.report(Err(format!("Could not open {what}: {failure}")));
            }
        });
    };
    let launcher = move || {
        state.update.with(|info| {
            info.as_ref()
                .map_or_else(|| "…".to_owned(), |info| info.current_version.clone())
        })
    };
    let zebrad = move || {
        state.node().stats.with(|stats| {
            stats
                .as_ref()
                .map_or_else(|| "not running".to_owned(), |s| s.zebrad_version.clone())
        })
    };
    let network = move || {
        if state.nodes_loaded.get() {
            state.node().info.with(|info| info.network.to_string())
        } else {
            "…".to_owned()
        }
    };
    view! {
        <fieldset class="about">
            <legend>"About"</legend>
            <Rows rows=vec![
                ("Zeal", launcher.into_any()),
                ("zebrad", zebrad.into_any()),
                ("Network", network.into_any()),
            ] />
            <div class="buttons">
                <button type="button" on:click=move |_| open(OpenTarget::LogDir, "the logs folder")>
                    "Open logs folder"
                </button>
                <button type="button" on:click=move |_| open(OpenTarget::DataDir, "the data folder")>
                    "Open data folder"
                </button>
                <button type="button" on:click=move |_| open(OpenTarget::ConfigDir, "the config folder")>
                    "Open config folder"
                </button>
            </div>
            <OutcomeNote outcome=note.signal() block=true />
        </fieldset>
    }
}

/// "Copy diagnostics": collects the text and copies it, or shows it when the clipboard is
/// denied.
#[component]
fn Diagnostics() -> impl IntoView {
    let busy = RwSignal::new(false);
    let note = Transient::<Result<String, String>>::new();
    let fallback = RwSignal::new(None::<String>);

    let copy = move |_| {
        busy.set(true);
        note.clear();
        fallback.set(None);
        spawn_local(async move {
            match api::copy_diagnostics().await {
                Ok(text) => match copy_to_clipboard(&text).await {
                    Ok(()) => note.report(Ok(
                        "Copied to the clipboard, ready to paste into an issue.".into(),
                    )),
                    Err(error) => {
                        note.report(Err(format!(
                            "Could not copy to the clipboard ({error}). Select the text below and copy it by hand."
                        )));
                        fallback.set(Some(text));
                    }
                },
                Err(error) => note.report(Err(format!("Could not collect diagnostics: {error}"))),
            }
            busy.set(false);
        });
    };

    view! {
        <fieldset>
            <legend>"Diagnostics"</legend>
            <p class="muted">
                "Versions, the node status, the settings, zebrad.toml and the last lines of the logs, for a bug report. The RPC cookie is never included."
            </p>
            <div class="buttons">
                <button type="button" disabled=move || busy.get() on:click=copy>
                    "Copy diagnostics"
                </button>
                <OutcomeNote outcome=note.signal() />
            </div>
            {move || {
                fallback
                    .get()
                    .map(|text| {
                        let lines = text.lines().count();
                        view! {
                            <details class="diagnostics" open>
                                <summary>{format!("Diagnostics ({lines} lines)")}</summary>
                                <pre>{text}</pre>
                            </details>
                        }
                    })
            }}
        </fieldset>
    }
}

/// "Delete node data": deletes the chain state after the user types [`DELETE_WORD`]. Only while
/// zebrad is stopped or failed.
#[component]
fn DeleteNodeData(busy: RwSignal<Option<Mutation>>) -> impl IntoView {
    let state = AppState::get();
    let allowed = data_change_open(state);
    let confirming = RwSignal::new(false);
    let deleting = move || busy.get() == Some(Mutation::DeleteNodeData);
    let outcome = Transient::<Result<String, String>>::new();

    let state_folder = move || {
        state
            .chosen_data_dir(state.node())
            .map(|dir| state_dir(&dir))
            .unwrap_or_else(|| "the state folder in the launcher's data folder".into())
    };
    let delete = move || {
        if busy.get_untracked().is_some() {
            return;
        }
        busy.set(Some(Mutation::DeleteNodeData));
        outcome.clear();
        let node = state.node_untracked().id();
        spawn_local(async move {
            outcome.report(match api::delete_node_data(&node).await {
                Ok(freed) => Ok(format!("Deleted the node data, freeing {}.", bytes(freed))),
                Err(error) => Err(format!("Could not delete the node data: {error}")),
            });
            busy.set(None);
            confirming.set(false);
        });
    };

    view! {
        <fieldset>
            <legend>"Node data"</legend>
            <div class="buttons">
                <button
                    type="button"
                    disabled=move || busy.get().is_some() || confirming.get() || !allowed()
                    on:click=move |_| {
                        outcome.clear();
                        confirming.set(true);
                    }
                >
                    "Delete node data…"
                </button>
                <span class="muted">
                    {move || {
                        if allowed() {
                            "Frees the disk space the chain uses. The next start syncs from scratch."
                        } else {
                            "Stop the node to delete its data."
                        }
                    }}
                </span>
            </div>
            <Show when=move || confirming.get()>
                <TypedConfirm
                    word=DELETE_WORD
                    action="Delete"
                    busy_label="Deleting…"
                    busy=Signal::derive(deleting)
                    // Nothing typed enables Delete while the node runs or another change is in
                    // flight.
                    blocked=Signal::derive(move || {
                        !allowed()
                            || busy
                                .get()
                                .is_some_and(|mutation| mutation != Mutation::DeleteNodeData)
                    })
                    on_confirm=Callback::new(move |()| delete())
                    on_cancel=Callback::new(move |()| confirming.set(false))
                    class="delete-confirm"
                >
                    <p>
                        "This deletes the chain state in " <code>{state_folder}</code>
                        " and nothing else: zebrad.toml, the launcher settings, the logs and the RPC cookie stay. The next start syncs the chain from scratch, which takes a day or more on Mainnet."
                    </p>
                </TypedConfirm>
            </Show>
            <OutcomeNote outcome=outcome.signal() block=true />
        </fieldset>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_grace_must_be_in_range() {
        assert_eq!(parse_stop_grace(" 60 "), Ok(60));
        assert!(parse_stop_grace("4").is_err());
        assert!(parse_stop_grace("3601").is_err());
        assert!(parse_stop_grace("1.5").is_err());
        assert!(parse_stop_grace("").is_err());
    }

    fn node() -> NodeChoices {
        NodeChoices {
            autostart: true,
            network: Network::Mainnet,
            stop_grace_secs: 60,
        }
    }

    #[test]
    fn a_draft_differs_by_a_choice_a_text_field_or_a_text_that_does_not_parse() {
        let stored = Settings::default();
        let stored_node = node();
        let fields = TextFields::from_stored(&stored, &stored_node);
        let differs = |draft: &Settings, node_draft: &NodeChoices, fields: &TextFields| {
            draft_differs(&stored, &stored_node, draft, node_draft, fields)
        };
        assert!(!differs(&stored, &stored_node, &fields));
        // A choice, the launcher's or the node's.
        let draft = Settings {
            close_to_tray: !stored.close_to_tray,
            ..stored.clone()
        };
        assert!(differs(&draft, &stored_node, &fields));
        let testnet = NodeChoices {
            network: Network::Testnet,
            ..node()
        };
        assert!(differs(&stored, &testnet, &fields));
        // A text field is read as it would be saved: padding is not a change, a new URL is.
        let padded = TextFields {
            stop_grace_secs: format!(" {} ", stored_node.stop_grace_secs),
            ..fields.clone()
        };
        assert!(!differs(&stored, &stored_node, &padded));
        let other_url = TextFields {
            grafana_url: "http://127.0.0.1:3000".into(),
            ..fields.clone()
        };
        assert!(differs(&stored, &stored_node, &other_url));
        let other_grace = TextFields {
            stop_grace_secs: "90".into(),
            ..fields.clone()
        };
        assert!(differs(&stored, &stored_node, &other_grace));
        // A text that does not parse differs whatever is stored, even where its parsed value
        // would read as the stored one: no URL is stored, and a bad URL is not "no URL".
        assert_eq!(stored.grafana_url, None);
        let bad_url = TextFields {
            grafana_url: "http://example.com".into(),
            ..fields.clone()
        };
        assert!(differs(&stored, &stored_node, &bad_url));
        let bad_grace = TextFields {
            stop_grace_secs: "0".into(),
            ..fields
        };
        assert!(differs(&stored, &stored_node, &bad_grace));
    }

    #[test]
    fn only_the_node_choices_that_changed_are_sent() {
        assert_eq!(node().update(&node()), None);
        let edited = NodeChoices {
            autostart: false,
            stop_grace_secs: 120,
            ..node()
        };
        assert_eq!(
            edited.update(&node()),
            Some(NodeUpdate {
                autostart: Some(false),
                stop_grace_secs: Some(120),
                ..NodeUpdate::default()
            })
        );
        let testnet = NodeChoices {
            network: Network::Testnet,
            ..node()
        };
        assert_eq!(
            testnet.update(&node()).and_then(|update| update.network),
            Some(Network::Testnet)
        );
    }

    #[test]
    fn free_space_words() {
        assert_eq!(free_space_text(Some(212_400_000_000)), "212.4 GB free");
        assert_eq!(free_space_text(None), "free space unknown");
        assert_eq!(
            data_dir_saved_text(Some(400_000_000_000)),
            "Saved. 400.0 GB free there."
        );
        assert!(data_dir_saved_text(Some(1_000_000_000)).ends_with("this drive may fill up."));
        assert_eq!(
            data_dir_saved_text(None),
            "Saved. The free space there could not be read."
        );
    }

    #[test]
    fn grafana_url_uses_the_shared_rule() {
        assert_eq!(parse_grafana_url(""), Ok(None));
        assert_eq!(parse_grafana_url("  "), Ok(None));
        assert_eq!(
            parse_grafana_url(" http://127.0.0.1:3000/d/zebra-overview "),
            Ok(Some("http://127.0.0.1:3000/d/zebra-overview".into()))
        );
        assert_eq!(
            parse_grafana_url("http://LOCALHOST:3000?orgId=1"),
            Ok(Some("http://LOCALHOST:3000?orgId=1".into()))
        );
        for bad in [
            "https://127.0.0.1:3000",
            "http://[::1]:3000",
            "http://grafana.example.com",
            "http://example.com@127.0.0.1:3000",
            "127.0.0.1:3000",
        ] {
            assert!(parse_grafana_url(bad).is_err(), "{bad} was accepted");
        }
    }
}
