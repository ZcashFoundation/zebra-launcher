//! The first-run flow: a modal wizard shown while `settings.first_run_done` is false. It cannot
//! be dismissed; Finish records completion (`complete_first_run`) and starts zebrad, which the
//! backend does not start at launch until then.
//!
//! Steps: welcome, data location and network, alpha.4 leftovers (only when some were found),
//! update mode, and notes (firewall prompt, logs).

use launcher_types::{FirstRunInfo, ImportOutcome, Network, Phase, Settings, UpdateMode};
use leptos::{prelude::*, task::spawn_local};

use crate::{
    api,
    app::AppState,
    components::update_mode_choice::UpdateModeChoice,
    format::{bytes, is_low_space, state_dir, update_mode_text},
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

/// "212.4 GB free", or a note that it is unknown.
fn free_text(free_bytes: Option<u64>) -> String {
    match free_bytes {
        Some(free) => format!("{} free", bytes(free)),
        None => "free space unknown".to_owned(),
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

/// The first-run wizard. `open` is true while it is shown; the caller makes the rest of the
/// app inert meanwhile.
#[component]
pub fn FirstRun(open: RwSignal<bool>) -> impl IntoView {
    let state = AppState::get();

    let info = RwSignal::new(None::<Result<FirstRunInfo, String>>);
    let step = RwSignal::new(Step::Welcome);
    let busy = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);

    // Data step: the chosen directory and its free space (`None` = the default).
    let chosen = RwSignal::new(None::<(String, Option<u64>)>);
    let network = RwSignal::new(Network::Mainnet);
    // Legacy step.
    let legacy_ack = RwSignal::new(false);
    let moved = RwSignal::new(None::<ImportOutcome>);
    // Updates step: Notify is preselected.
    let update_mode = RwSignal::new(UpdateMode::Notify);
    // Set when the settings were saved but zebrad did not start.
    let start_error = RwSignal::new(None::<String>);

    let load_info = move || {
        info.set(None);
        spawn_local(async move {
            info.set(Some(api::get_first_run_info().await.map_err(|error| {
                format!("Could not look at this computer: {error}")
            })));
        });
    };

    // A folder chosen in an earlier, unfinished run of the wizard: there is no read-only query
    // for the free space of an arbitrary folder, so choose it again (which changes nothing) for
    // the free space there. Until that answers, or if it fails, the space shows as unknown.
    let refresh_stored_dir = move |dir: String| {
        busy.set(true);
        spawn_local(async move {
            if let Ok(choice) = api::choose_data_dir(dir.clone()).await {
                // Unless another folder was chosen meanwhile.
                if chosen.with_untracked(|c| c.as_ref().is_some_and(|(d, _)| *d == dir)) {
                    chosen.set(
                        choice
                            .settings
                            .data_dir
                            .clone()
                            .map(|dir| (dir, choice.free_bytes)),
                    );
                    state.settings.set(Some(choice.settings));
                }
            }
            busy.set(false);
        });
    };

    // Open once the settings say the flow is due, starting from the stored choices.
    Effect::new(move |_| {
        let due = state
            .settings
            .with(|settings| settings.as_ref().filter(|s| !s.first_run_done).cloned());
        if let Some(settings) = due
            && !open.get_untracked()
        {
            network.set(settings.network);
            chosen.set(settings.data_dir.clone().map(|dir| (dir, None)));
            open.set(true);
            load_info();
            if let Some(dir) = settings.data_dir {
                refresh_stored_dir(dir);
            }
        }
    });

    let has_legacy = move || {
        info.with(|info| {
            matches!(
                info,
                Some(Ok(FirstRunInfo {
                    legacy: Some(_),
                    ..
                }))
            )
        })
    };
    let step_list = Memo::new(move |_| steps(has_legacy()));
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

    // The data directory in use and its free space.
    let current_dir = move || -> Option<(String, Option<u64>, bool)> {
        if let Some((dir, free)) = chosen.get() {
            return Some((dir, free, false));
        }
        info.with(|info| match info {
            Some(Ok(info)) => Some((
                info.default_data_dir.clone(),
                info.free_bytes_at_default,
                true,
            )),
            _ => None,
        })
    };

    let choose = move |path: Option<String>| {
        busy.set(true);
        error.set(None);
        spawn_local(async move {
            let path = match path {
                Some(path) => Some(path),
                None => match api::pick_data_dir().await {
                    Ok(path) => path,
                    Err(failure) => {
                        error.set(Some(format!("Could not open the folder picker: {failure}")));
                        None
                    }
                },
            };
            if let Some(path) = path {
                match api::choose_data_dir(path).await {
                    Ok(choice) => {
                        chosen.set(
                            choice
                                .settings
                                .data_dir
                                .clone()
                                .map(|dir| (dir, choice.free_bytes)),
                        );
                        state.settings.set(Some(choice.settings));
                    }
                    Err(failure) => {
                        error.set(Some(format!("Could not use that folder: {failure}")));
                    }
                }
            }
            busy.set(false);
        });
    };

    let welcome_view = || {
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
    };

    let network_radio = move |value: Network, hint: &'static str| {
        view! {
            <label class="radio">
                <input
                    type="radio"
                    name="first-run-network"
                    prop:checked=move || network.get() == value
                    on:change=move |_| network.set(value)
                />
                <span>{value.as_str()}</span>
                <span class="muted">{hint}</span>
            </label>
        }
    };

    let data_view = move || {
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
                            <button type="button" on:click=move |_| load_info()>
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
                            current_dir()
                                .map(|(dir, free, is_default)| {
                                    view! {
                                        <dl class="rows">
                                            <dt>"Data folder"</dt>
                                            <dd>
                                                <code class="data-dir">{dir.clone()}</code>
                                                {is_default.then_some(" (default)")}
                                            </dd>
                                            <dt>"Chain state"</dt>
                                            <dd>
                                                <code>{state_dir(&dir)}</code>
                                            </dd>
                                            <dt>"Space"</dt>
                                            <dd class="free-space">{free_text(free)}</dd>
                                        </dl>
                                        {low_space_warning(free)}
                                    }
                                })
                        }}
                        <div class="buttons">
                            <button
                                type="button"
                                disabled=move || busy.get() || moved.with(Option::is_some)
                                on:click=move |_| choose(None)
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
                                                on:click=move |_| choose(Some(default_dir.clone()))
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
                {network_radio(Network::Mainnet, "the real Zcash network")}
                {network_radio(
                    Network::Testnet,
                    "the public test network, with worthless coins and a much smaller chain",
                )}
            </fieldset>
        }
    };

    let legacy_view = move || {
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
        let move_it = move |_| {
            busy.set(true);
            error.set(None);
            spawn_local(async move {
                match api::import_legacy_state().await {
                    Ok(outcome) => moved.set(Some(outcome)),
                    Err(failure) => error.set(Some(format!("Could not move it: {failure}"))),
                }
                busy.set(false);
            });
        };
        view! {
            <p>
                "This computer has files from Zebra Launcher v0.0.0-alpha.4, or from a zebrad you ran yourself:"
            </p>
            <dl class="rows">
                {legacy
                    .config_file
                    .map(|path| {
                        view! {
                            <dt>"Old config"</dt>
                            <dd>
                                <code>{path}</code>
                                <br />
                                <span class="muted">
                                    "Not used: this launcher writes its own zebrad.toml. It goes away when you delete the old app."
                                </span>
                            </dd>
                        }
                    })}
                {legacy
                    .state_dir
                    .map(|dir| {
                        view! {
                            <dt>"Chain state"</dt>
                            <dd>
                                <code>{state_dir(&dir)}</code>
                                <br />
                                <span class="legacy-size">{size}</span>
                            </dd>
                        }
                    })}
            </dl>
            {if has_state {
                view! {
                    <p>
                        "Moving it here saves syncing the chain again. It is moved, not copied, into "
                        <code>{move || current_dir().map(|(dir, _, _)| state_dir(&dir))}</code> "."
                    </p>
                    <div class="banner banner-warn" role="note">
                        "Quit the old launcher, and any zebrad you started yourself, first. After the move they no longer find this state. The move only works when both folders are on the same drive; otherwise move the folder by hand."
                    </div>
                    {move || match moved.get() {
                        Some(outcome) => {
                            view! {
                                <div class="banner banner-ok" role="status">
                                    {format!("Moved {} to {}.", outcome.from, outcome.to)}
                                </div>
                            }
                                .into_any()
                        }
                        None => {
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
                                        disabled=move || busy.get() || !legacy_ack.get()
                                        on:click=move_it
                                    >
                                        {move || if busy.get() { "Moving…" } else { "Move it here" }}
                                    </button>
                                </div>
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
    };

    let updates_view = move || {
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
    };

    let open_logs = move |_| {
        spawn_local(async move {
            if let Err(failure) = api::open_path("log_dir").await {
                error.set(Some(format!("Could not open the log folder: {failure}")));
            }
        });
    };
    let notes_view = move || {
        let mode_label = update_mode_text(update_mode.get_untracked()).0;
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
                <button type="button" on:click=open_logs>
                    "Open the log folder"
                </button>
            </div>
            <h3>"Summary"</h3>
            <dl class="rows">
                <dt>"Data folder"</dt>
                <dd>
                    <code>{move || current_dir().map(|(dir, _, _)| dir)}</code>
                </dd>
                <dt>"Network"</dt>
                <dd>{move || network.get().as_str()}</dd>
                <dt>"Updates"</dt>
                <dd>{mode_label}</dd>
            </dl>
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
                    let chosen_network = network.get_untracked();
                    save_settings(state, |s| s.network = chosen_network).await
                }
                Step::Updates => {
                    let mode = update_mode.get_untracked();
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
        Step::Welcome => welcome_view().into_any(),
        Step::Data => data_view().into_any(),
        Step::Legacy => legacy_view(),
        Step::Updates => updates_view().into_any(),
        Step::Notes => notes_view().into_any(),
    };

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
                    // Until something was moved, moving on means leaving it where it is.
                    view! {
                        <button
                            type="button"
                            class:primary=move || moved.with(Option::is_some)
                            disabled=move || busy.get()
                            on:click=next
                        >
                            {move || if moved.with(Option::is_some) { "Next" } else { "Leave it" }}
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
                        class="modal first-run"
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
                        <div class="modal-body">{body}</div>
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

    #[test]
    fn free_space_text() {
        assert_eq!(free_text(Some(212_400_000_000)), "212.4 GB free");
        assert_eq!(free_text(None), "free space unknown");
    }
}
