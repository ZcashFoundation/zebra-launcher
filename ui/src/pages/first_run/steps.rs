//! The bodies of the wizard's steps: a plain function over [`Wizard`] per step, which the shell
//! in `pages/first_run.rs` renders for the current one. A step reads and sets the wizard's
//! signals and calls its actions; moving between steps, and saving what a step chose, is the
//! shell's.

use launcher_types::{FirstRunInfo, Network};
use leptos::prelude::*;

use super::Wizard;
use crate::{
    components::{
        radio_choice::RadioChoice, rows::Rows, typed_confirm::TypedConfirm,
        update_mode_choice::UpdateModeChoice,
    },
    format::{bytes, is_low_space, state_dir},
    pages::settings::{DELETE_WORD, free_space_text},
    update_text::update_mode_text,
};

/// The welcome step: what the launcher does and what a full node needs.
pub(super) fn welcome_step() -> impl IntoView {
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
pub(super) fn data_step(wizard: Wizard) -> impl IntoView {
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
pub(super) fn legacy_step(wizard: Wizard) -> AnyView {
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
pub(super) fn updates_step(wizard: Wizard) -> impl IntoView {
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
pub(super) fn notes_step(wizard: Wizard) -> impl IntoView {
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
