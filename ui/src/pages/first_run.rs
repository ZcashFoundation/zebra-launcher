//! The first-run flow: a modal wizard shown while `settings.first_run_done` is false. It cannot
//! be dismissed; Finish records completion (`complete_first_run`) and starts zebrad, which the
//! backend does not start at launch until then.
//!
//! Steps: welcome, data location and network, alpha.4 leftovers (only when some were found:
//! move the old chain state here, delete it, or leave it), update mode, and notes (firewall
//! prompt, logs). [`Wizard`] holds the choices and makes the backend calls; each step's body is
//! a plain function over it, and [`FirstRun`] is the shell: the step count, the title, the
//! error line and the Back, Next and Finish footer, which saves what a step chose.

use launcher_types::{
    FirstRunInfo, ImportOutcome, Network, OpenTarget, Phase, Settings, UpdateMode,
};
use leptos::{prelude::*, task::spawn_local};

use crate::{
    api,
    app::AppState,
    components::{
        radio_choice::RadioChoice, rows::Rows, typed_confirm::TypedConfirm,
        update_mode_choice::UpdateModeChoice,
    },
    format::{bytes, is_low_space, state_dir},
    pages::settings::{DELETE_WORD, free_space_text},
    update_text::update_mode_text,
};

/// One page of the wizard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Welcome,
    Data,
    Legacy,
    Updates,
    Notes,
}

impl Step {
    fn title(self) -> &'static str {
        match self {
            Step::Welcome => "Welcome to Zebra Launcher",
            Step::Data => "Where to keep the chain",
            Step::Legacy => "Files from an earlier launcher",
            Step::Updates => "Launcher updates",
            Step::Notes => "Before the node starts",
        }
    }
}

/// The steps shown, in order. The legacy step only appears when leftovers were found.
fn steps(has_legacy: bool) -> Vec<Step> {
    let mut steps = vec![Step::Welcome, Step::Data];
    if has_legacy {
        steps.push(Step::Legacy);
    }
    steps.extend([Step::Updates, Step::Notes]);
    steps
}

/// True when the backend reports zebrad starting or running.
async fn already_running() -> bool {
    api::get_status()
        .await
        .is_ok_and(|status| matches!(status.phase, Phase::Starting | Phase::Running))
}

/// Saves a change to the current settings and publishes what was stored.
async fn save_settings(state: AppState, change: impl FnOnce(&mut Settings)) -> Result<(), String> {
    let mut settings = state
        .settings
        .get_untracked()
        .ok_or("the settings are not loaded yet")?;
    change(&mut settings);
    let stored = api::set_settings(settings)
        .await
        .map_err(|error| format!("Could not save the settings: {error}"))?;
    state.settings.set(Some(stored));
    Ok(())
}

/// The wizard's choices and the calls that act on them, shared by the shell and the steps.
/// Every field is a signal, so the struct is `Copy`. Whether the wizard is shown is the
/// shell's, as its caller owns that signal.
#[derive(Clone, Copy)]
struct Wizard {
    state: AppState,
    /// What the backend found on this computer, `None` while it looks.
    info: RwSignal<Option<Result<FirstRunInfo, String>>>,
    step: RwSignal<Step>,
    /// A backend call is in flight.
    busy: RwSignal<bool>,
    /// Why the last action failed, shown under the step until the next action.
    error: RwSignal<Option<String>>,
    /// Data step: the chosen directory and its free space (`None`: the default).
    chosen: RwSignal<Option<(String, Option<u64>)>>,
    /// True while the free space of a folder chosen in an earlier run is being read.
    probing: RwSignal<bool>,
    network: RwSignal<Network>,
    /// Legacy step: the user says nothing uses the old state.
    legacy_ack: RwSignal<bool>,
    moved: RwSignal<Option<ImportOutcome>>,
    /// The bytes freed by deleting the alpha.4 state (0 when unknown).
    deleted: RwSignal<Option<u64>>,
    confirm_delete: RwSignal<bool>,
    /// Updates step: Notify is preselected.
    update_mode: RwSignal<UpdateMode>,
    /// Set when the settings were saved but zebrad did not start.
    start_error: RwSignal<Option<String>>,
}

impl Wizard {
    fn new() -> Self {
        Self {
            state: AppState::get(),
            info: RwSignal::new(None),
            step: RwSignal::new(Step::Welcome),
            busy: RwSignal::new(false),
            error: RwSignal::new(None),
            chosen: RwSignal::new(None),
            probing: RwSignal::new(false),
            network: RwSignal::new(Network::Mainnet),
            legacy_ack: RwSignal::new(false),
            moved: RwSignal::new(None),
            deleted: RwSignal::new(None),
            confirm_delete: RwSignal::new(false),
            update_mode: RwSignal::new(UpdateMode::Notify),
            start_error: RwSignal::new(None),
        }
    }

    /// Asks the backend what is on this computer.
    fn load_info(self) {
        self.info.set(None);
        spawn_local(async move {
            self.info.set(Some(
                api::get_first_run_info()
                    .await
                    .map_err(|error| format!("Could not look at this computer: {error}")),
            ));
        });
    }

    /// A folder chosen in an earlier, unfinished run of the wizard: read the free space there
    /// (read-only, creating nothing). Nothing waits for it; until it answers, or if it fails,
    /// the space shows as unknown.
    fn refresh_stored_dir(self, dir: String) {
        self.probing.set(true);
        spawn_local(async move {
            let free = api::free_space(dir.clone()).await.ok().flatten();
            // Unless another folder was chosen meanwhile.
            self.chosen.update(|current| {
                if let Some((current_dir, current_free)) = current
                    && *current_dir == dir
                    && current_free.is_none()
                {
                    *current_free = free;
                }
            });
            self.probing.set(false);
        });
    }

    /// True when alpha.4 leftovers were found, tracked.
    fn has_legacy(self) -> bool {
        self.info.with(|info| {
            matches!(
                info,
                Some(Ok(FirstRunInfo {
                    legacy: Some(_),
                    ..
                }))
            )
        })
    }

    /// Something was done with the alpha.4 state, so moving on no longer means leaving it.
    fn legacy_done(self) -> bool {
        self.moved.with(Option::is_some) || self.deleted.with(Option::is_some)
    }

    /// The data directory in use, its free space and whether it is the default, tracked.
    fn current_dir(self) -> Option<(String, Option<u64>, bool)> {
        if let Some((dir, free)) = self.chosen.get() {
            return Some((dir, free, false));
        }
        self.info.with(|info| match info {
            Some(Ok(info)) => Some((
                info.default_data_dir.clone(),
                info.free_bytes_at_default,
                true,
            )),
            _ => None,
        })
    }

    /// Stores `path` as the data directory, or the folder the picker gives when `None`.
    fn choose(self, path: Option<String>) {
        self.busy.set(true);
        self.error.set(None);
        spawn_local(async move {
            let path = match path {
                Some(path) => Some(path),
                None => match api::pick_data_dir().await {
                    Ok(path) => path,
                    Err(failure) => {
                        self.error
                            .set(Some(format!("Could not open the folder picker: {failure}")));
                        None
                    }
                },
            };
            if let Some(path) = path {
                match api::choose_data_dir(path).await {
                    Ok(choice) => {
                        self.chosen.set(
                            choice
                                .settings
                                .data_dir
                                .clone()
                                .map(|dir| (dir, choice.free_bytes)),
                        );
                        self.state.settings.set(Some(choice.settings));
                    }
                    Err(failure) => {
                        self.error
                            .set(Some(format!("Could not use that folder: {failure}")));
                    }
                }
            }
            self.busy.set(false);
        });
    }

    /// Moves the alpha.4 chain state into the data folder.
    fn import_legacy_state(self) {
        self.busy.set(true);
        self.error.set(None);
        spawn_local(async move {
            match api::import_legacy_state().await {
                Ok(outcome) => self.moved.set(Some(outcome)),
                Err(failure) => self
                    .error
                    .set(Some(format!("Could not move it: {failure}"))),
            }
            self.busy.set(false);
        });
    }

    /// Deletes the alpha.4 chain state, closing the confirmation when it is gone.
    fn delete_legacy_state(self) {
        self.busy.set(true);
        self.error.set(None);
        spawn_local(async move {
            match api::delete_legacy_state().await {
                Ok(freed) => {
                    self.deleted.set(Some(freed));
                    self.confirm_delete.set(false);
                }
                Err(failure) => self
                    .error
                    .set(Some(format!("Could not delete it: {failure}"))),
            }
            self.busy.set(false);
        });
    }

    /// Opens the launcher's log folder.
    fn open_logs(self) {
        spawn_local(async move {
            if let Err(failure) = api::open_path(OpenTarget::LogDir).await {
                self.error
                    .set(Some(format!("Could not open the log folder: {failure}")));
            }
        });
    }
}

/// The first-run wizard. `open` is true while it is shown; the caller makes the rest of the
/// app inert meanwhile.
#[component]
pub fn FirstRun(open: RwSignal<bool>) -> impl IntoView {
    let wizard = Wizard::new();
    let Wizard {
        state,
        info,
        step,
        busy,
        error,
        start_error,
        ..
    } = wizard;

    // Open once the settings say the flow is due, starting from the stored choices.
    Effect::new(move |_| {
        let due = state
            .settings
            .with(|settings| settings.as_ref().filter(|s| !s.first_run_done).cloned());
        if let Some(settings) = due
            && !open.get_untracked()
        {
            wizard.network.set(settings.network);
            wizard
                .chosen
                .set(settings.data_dir.clone().map(|dir| (dir, None)));
            open.set(true);
            wizard.load_info();
            if let Some(dir) = settings.data_dir {
                wizard.refresh_stored_dir(dir);
            }
        }
    });

    let step_list = Memo::new(move |_| steps(wizard.has_legacy()));
    let position = move || {
        let current = step.get();
        step_list.with(|list| list.iter().position(|s| *s == current).unwrap_or(0))
    };
    let go = move |offset: isize| {
        let list = step_list.get_untracked();
        let current = list
            .iter()
            .position(|s| *s == step.get_untracked())
            .unwrap_or(0);
        if let Some(next) = current
            .checked_add_signed(offset)
            .and_then(|index| list.get(index))
        {
            error.set(None);
            step.set(*next);
        }
    };

    // Next saves what the step chose before moving on.
    let next = move |_| {
        let current = step.get_untracked();
        busy.set(true);
        error.set(None);
        spawn_local(async move {
            let saved = match current {
                Step::Data => {
                    let chosen_network = wizard.network.get_untracked();
                    save_settings(state, |s| s.network = chosen_network).await
                }
                Step::Updates => {
                    let mode = wizard.update_mode.get_untracked();
                    save_settings(state, |s| s.update_mode = mode).await
                }
                _ => Ok(()),
            };
            busy.set(false);
            match saved {
                Ok(()) => go(1),
                Err(failure) => error.set(Some(failure)),
            }
        });
    };

    let finish = move |_| {
        busy.set(true);
        error.set(None);
        spawn_local(async move {
            match api::complete_first_run().await {
                Ok(()) => {
                    state.settings.update(|settings| {
                        if let Some(settings) = settings {
                            settings.first_run_done = true;
                        }
                    });
                    match api::start_zebrad().await {
                        Ok(()) => open.set(false),
                        // Nothing starts zebrad before the first run is done, but if it runs
                        // anyway, the start failing is not a problem: the node is up.
                        Err(_) if already_running().await => open.set(false),
                        Err(failure) => start_error.set(Some(failure.to_string())),
                    }
                }
                Err(failure) => error.set(Some(format!("Could not finish: {failure}"))),
            }
            busy.set(false);
        });
    };

    let body = move || match step.get() {
        Step::Welcome => welcome_step().into_any(),
        Step::Data => data_step(wizard).into_any(),
        Step::Legacy => legacy_step(wizard),
        Step::Updates => updates_step(wizard).into_any(),
        Step::Notes => notes_step(wizard).into_any(),
    };

    let legacy_done = move || wizard.legacy_done();
    let footer = move || {
        if let Some(failure) = start_error.get() {
            return view! {
                <div class="banner banner-error" role="alert">
                    {format!(
                        "Setup is saved, but the node did not start: {failure}. You can start it from the dashboard."
                    )}
                </div>
                <div class="buttons wizard-nav">
                    <span class="spacer"></span>
                    <button type="button" class="primary" on:click=move |_| open.set(false)>
                        "Go to the dashboard"
                    </button>
                </div>
            }
            .into_any();
        }
        let current = step.get();
        let is_last = step_list.with(|list| list.last() == Some(&current));
        // Next waits for the first-run info on the data step, since the next steps use it.
        let blocked =
            move || current == Step::Data && !info.with(|info| matches!(info, Some(Ok(_))));
        view! {
            <div class="buttons wizard-nav">
                <button
                    type="button"
                    disabled=move || busy.get() || current == Step::Welcome
                    on:click=move |_| go(-1)
                >
                    "Back"
                </button>
                <span class="spacer"></span>
                {if is_last {
                    view! {
                        <button
                            type="button"
                            class="primary"
                            disabled=move || busy.get()
                            on:click=finish
                        >
                            {move || if busy.get() { "Finishing…" } else { "Finish and start the node" }}
                        </button>
                    }
                        .into_any()
                } else if current == Step::Legacy {
                    // Until the state was moved or deleted, moving on means leaving it where it is.
                    view! {
                        <button
                            type="button"
                            class:primary=legacy_done
                            disabled=move || busy.get()
                            on:click=next
                        >
                            {move || if legacy_done() { "Next" } else { "Leave it" }}
                        </button>
                    }
                        .into_any()
                } else {
                    view! {
                        <button
                            type="button"
                            class="primary"
                            disabled=move || busy.get() || blocked()
                            on:click=next
                        >
                            "Next"
                        </button>
                    }
                        .into_any()
                }}
            </div>
        }
        .into_any()
    };

    move || {
        open.get().then(|| {
            view! {
                <div class="modal-backdrop">
                    <div
                        class="modal"
                        role="dialog"
                        aria-modal="true"
                        aria-labelledby="first-run-title"
                    >
                        <p class="muted step-count">
                            {move || {
                                format!(
                                    "Step {} of {}",
                                    position() + 1,
                                    step_list.with(Vec::len),
                                )
                            }}
                        </p>
                        <h2 id="first-run-title">{move || step.get().title()}</h2>
                        <div>{body}</div>
                        {move || {
                            error
                                .get()
                                .map(|error| {
                                    view! {
                                        <p class="error" role="alert">
                                            {error}
                                        </p>
                                    }
                                })
                        }}
                        {footer}
                    </div>
                </div>
            }
        })
    }
}

/// The welcome step: what the launcher does and what a full node needs.
fn welcome_step() -> impl IntoView {
    view! {
        <p>
            "Zebra Launcher runs zebrad, the Zcash Foundation's Zcash full node, on this computer. It starts and stops the node, restarts it if it crashes, shows its sync progress and logs, and lets you edit its configuration."
        </p>
        <p>
            "A full node downloads and verifies the whole Zcash chain. On Mainnet that is hundreds of GB, and the first sync can take a day or more."
        </p>
        <p class="muted">
            "The next steps choose where the chain is kept, the network, and how the launcher updates itself. Nothing is downloaded until you finish."
        </p>
    }
}

/// The warning shown below 300 GB of free space. The user may continue anyway.
fn low_space_warning(free_bytes: Option<u64>) -> Option<impl IntoView> {
    let free = free_bytes.filter(|_| is_low_space(free_bytes))?;
    Some(view! {
        <div class="banner banner-warn" role="status">
            {format!(
                "Only {} free there. Mainnet needs hundreds of GB, and the chain keeps growing. Choose a folder on a bigger drive, or continue if you use Testnet or will free up space.",
                bytes(free),
            )}
        </div>
    })
}

/// The data step: the folder the chain state goes into, its free space, and the network.
fn data_step(wizard: Wizard) -> impl IntoView {
    let Wizard {
        info,
        busy,
        chosen,
        probing,
        moved,
        network,
        ..
    } = wizard;
    view! {
        <p>
            "The chain state goes into a "<code>"state"</code>
            " folder inside the data folder. Mainnet needs hundreds of GB, so pick a drive with room to spare."
        </p>
        {move || match info.get() {
            None => view! { <p class="muted">"Looking at this computer…"</p> }.into_any(),
            Some(Err(failure)) => {
                view! {
                    <div class="banner banner-error" role="alert">
                        <span>{failure}</span>
                        " "
                        <button type="button" on:click=move |_| wizard.load_info()>
                            "Retry"
                        </button>
                    </div>
                }
                    .into_any()
            }
            Some(Ok(info)) => {
                let default_dir = info.default_data_dir.clone();
                view! {
                    {move || {
                        wizard
                            .current_dir()
                            .map(|(dir, free, is_default)| {
                                let space = if free.is_none() && probing.get() {
                                    "checking the free space…".to_owned()
                                } else {
                                    free_space_text(free)
                                };
                                view! {
                                    <Rows rows=vec![
                                        (
                                            "Data folder",
                                            view! {
                                                <code class="data-dir">{dir.clone()}</code>
                                                {is_default.then_some(" (default)")}
                                            }
                                                .into_any(),
                                        ),
                                        ("Chain state", view! { <code>{state_dir(&dir)}</code> }.into_any()),
                                        ("Space", space.into_any()),
                                    ] />
                                    {low_space_warning(free)}
                                }
                            })
                    }}
                    <div class="buttons">
                        <button
                            type="button"
                            disabled=move || busy.get() || moved.with(Option::is_some)
                            on:click=move |_| wizard.choose(None)
                        >
                            "Choose another folder…"
                        </button>
                        {move || {
                            let default_dir = default_dir.clone();
                            chosen
                                .with(Option::is_some)
                                .then(|| {
                                    view! {
                                        <button
                                            type="button"
                                            disabled=move || {
                                                busy.get() || moved.with(Option::is_some)
                                            }
                                            on:click=move |_| wizard.choose(Some(default_dir.clone()))
                                        >
                                            "Use the default"
                                        </button>
                                    }
                                })
                        }}
                    </div>
                    {move || {
                        moved
                            .with(Option::is_some)
                            .then(|| {
                                view! {
                                    <p class="muted">
                                        "The earlier chain state was already moved into this folder, so it stays fixed here. You can change it later in Settings."
                                    </p>
                                }
                            })
                    }}
                }
                    .into_any()
            }
        }}
        <fieldset class="choices">
            <legend>"Network"</legend>
            <RadioChoice
                name="first-run-network"
                options=vec![
                    (Network::Mainnet, Network::Mainnet.as_str(), "the real Zcash network"),
                    (
                        Network::Testnet,
                        Network::Testnet.as_str(),
                        "the public test network, with worthless coins and a much smaller chain",
                    ),
                ]
                selected=Signal::derive(move || Some(network.get()))
                on_select=Callback::new(move |value| network.set(value))
                value=Network::as_str
            />
        </fieldset>
    }
}

/// The legacy step: the alpha.4 leftovers found, and whether to move, delete or leave the old
/// chain state.
fn legacy_step(wizard: Wizard) -> AnyView {
    let Wizard {
        info,
        busy,
        error,
        moved,
        deleted,
        legacy_ack,
        confirm_delete,
        ..
    } = wizard;
    let Some(Ok(FirstRunInfo {
        legacy: Some(legacy),
        ..
    })) = info.get()
    else {
        return view! { <p class="muted">"Nothing was found."</p> }.into_any();
    };
    let size = if legacy.state_bytes_is_lower_bound {
        format!(
            "at least {} (counting stopped early)",
            bytes(legacy.state_bytes)
        )
    } else {
        bytes(legacy.state_bytes)
    };
    let has_state = legacy.state_dir.is_some();
    let old_state = legacy
        .state_dir
        .as_deref()
        .map(state_dir)
        .unwrap_or_default();
    let delete_size = size.clone();
    let mut found = Vec::new();
    if let Some(path) = legacy.config_file {
        found.push((
            "Old config",
            view! {
                <code>{path}</code>
                <br />
                <span class="muted">
                    "Not used: this launcher writes its own zebrad.toml. It goes away when you delete the old app."
                </span>
            }
            .into_any(),
        ));
    }
    if let Some(dir) = legacy.state_dir {
        found.push((
            "Chain state",
            view! {
                <code>{state_dir(&dir)}</code>
                <br />
                <span>{size}</span>
            }
            .into_any(),
        ));
    }
    view! {
        <p>
            "This computer has files from Zebra Launcher v0.0.0-alpha.4, or from a zebrad you ran yourself:"
        </p>
        <Rows rows=found />
        {if has_state {
            view! {
                <p>
                    "Moving it here saves syncing the chain again. It is moved, not copied, into "
                    <code>{move || wizard.current_dir().map(|(dir, _, _)| state_dir(&dir))}</code>
                    ". If you do not need it, delete it to free the space, or leave it where it is."
                </p>
                <div class="banner banner-warn" role="note">
                    "Quit the old launcher, and any zebrad you started yourself, first. After the move they no longer find this state. The move only works when both folders are on the same drive; otherwise move the folder by hand."
                </div>
                {move || match (moved.get(), deleted.get()) {
                    (Some(outcome), _) => {
                        view! {
                            <div class="banner banner-ok" role="status">
                                {format!("Moved {} to {}.", outcome.from, outcome.to)}
                            </div>
                        }
                            .into_any()
                    }
                    (None, Some(freed)) => {
                        let text = if freed > 0 {
                            format!("Deleted {}, freeing {}.", old_state, bytes(freed))
                        } else {
                            format!("Deleted {}.", old_state)
                        };
                        view! {
                            <div class="banner banner-ok" role="status">
                                {text}
                            </div>
                        }
                            .into_any()
                    }
                    (None, None) => {
                        let old_state = old_state.clone();
                        let delete_size = delete_size.clone();
                        view! {
                            <label class="check">
                                <input
                                    type="checkbox"
                                    prop:checked=move || legacy_ack.get()
                                    on:change=move |event| {
                                        legacy_ack.set(event_target_checked(&event))
                                    }
                                />
                                "The old launcher is not running, and no zebrad uses this folder"
                            </label>
                            <div class="buttons">
                                <button
                                    type="button"
                                    class="primary"
                                    disabled=move || {
                                        busy.get() || !legacy_ack.get() || confirm_delete.get()
                                    }
                                    on:click=move |_| wizard.import_legacy_state()
                                >
                                    {move || {
                                        if busy.get() && !confirm_delete.get() {
                                            "Moving…"
                                        } else {
                                            "Move it here"
                                        }
                                    }}
                                </button>
                                <button
                                    type="button"
                                    disabled=move || {
                                        busy.get() || !legacy_ack.get() || confirm_delete.get()
                                    }
                                    on:click=move |_| {
                                        error.set(None);
                                        confirm_delete.set(true);
                                    }
                                >
                                    "Delete it…"
                                </button>
                            </div>
                            <Show when=move || confirm_delete.get()>
                                {
                                    // The confirmation's text is its own; the step's copies
                                    // stay for the next time it opens.
                                    let old_state = old_state.clone();
                                    let delete_size = delete_size.clone();
                                    view! {
                                        <TypedConfirm
                                            word=DELETE_WORD
                                            action="Delete"
                                            busy_label="Deleting…"
                                            busy=busy
                                            on_confirm=Callback::new(move |()| wizard.delete_legacy_state())
                                            on_cancel=Callback::new(move |()| confirm_delete.set(false))
                                            class="delete-confirm"
                                        >
                                            <p>
                                                "This deletes the old chain state in "
                                                <code>{old_state}</code> " for good, freeing "
                                                {delete_size}
                                                ". Only do this if you do not want to keep it: syncing it again takes a day or more on Mainnet."
                                            </p>
                                        </TypedConfirm>
                                    }
                                }
                            </Show>
                        }
                            .into_any()
                    }
                }}
            }
                .into_any()
        } else {
            view! { <p class="muted">"There is no chain state to move."</p> }.into_any()
        }}
    }
    .into_any()
}

/// The updates step: how the launcher keeps itself up to date.
fn updates_step(wizard: Wizard) -> impl IntoView {
    let update_mode = wizard.update_mode;
    view! {
        <p>"How should the launcher keep itself up to date?"</p>
        <fieldset class="choices">
            <legend>"Updates"</legend>
            <UpdateModeChoice
                name="first-run-update-mode"
                selected=Signal::derive(move || Some(update_mode.get()))
                on_select=Callback::new(move |mode| update_mode.set(mode))
            />
        </fieldset>
        <p class="muted">
            "Each zebrad release stops working some months after it is published (its end of support), so a launcher that is never updated eventually stops syncing. You can change this later in Settings."
        </p>
    }
}

/// The notes step: the firewall prompt, where the logs are, and a summary of the choices.
fn notes_step(wizard: Wizard) -> impl IntoView {
    let network = wizard.network;
    let mode_label = update_mode_text(wizard.update_mode.get_untracked()).0;
    view! {
        <h3>"Firewall"</h3>
        <p>
            "When the node starts, your system may ask whether zebrad may accept incoming connections. zebrad talks to other Zcash nodes on port 8233 (18233 on Testnet). Allowing it helps the network; the node still syncs if you deny it."
        </p>
        <h3>"Logs"</h3>
        <p>
            "The launcher and zebrad logs are in the launcher's log folder: "
            <code>"~/.local/share/org.zfnd.zebra-launcher/logs"</code>" (Linux), "
            <code>"~/Library/Logs/org.zfnd.zebra-launcher"</code>" (macOS), "
            <code>"%LOCALAPPDATA%\\org.zfnd.zebra-launcher\\logs"</code>
            " (Windows). If something goes wrong, Settings → Copy diagnostics collects what a bug report needs."
        </p>
        <div class="buttons">
            <button type="button" on:click=move |_| wizard.open_logs()>
                "Open the log folder"
            </button>
        </div>
        <h3>"Summary"</h3>
        <Rows rows=vec![
            (
                "Data folder",
                view! { <code>{move || wizard.current_dir().map(|(dir, _, _)| dir)}</code> }.into_any(),
            ),
            ("Network", (move || network.get().as_str()).into_any()),
            ("Updates", mode_label.into_any()),
        ] />
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_step_only_when_something_was_found() {
        assert_eq!(
            steps(false),
            [Step::Welcome, Step::Data, Step::Updates, Step::Notes]
        );
        assert_eq!(
            steps(true),
            [
                Step::Welcome,
                Step::Data,
                Step::Legacy,
                Step::Updates,
                Step::Notes
            ]
        );
    }
}
