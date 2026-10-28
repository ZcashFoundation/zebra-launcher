//! Launcher updates: the header banner, the update buttons, "Check for updates", and the
//! recovery options after an update that did not come back healthy.

use std::time::SystemTime;

use launcher_types::{OpenTarget, RecoveryAction, UpdateInfo, UpdateState};
use leptos::{prelude::*, task::spawn_local};

use crate::{
    api,
    app::{AppState, Tab},
    components::{
        outcome_note::OutcomeNote,
        reset_config::ResetConfigButton,
        rows::{Rows, text_rows},
        transient::Transient,
    },
    format::{data_change_allowed, relative},
    update_text::{NOTES_EXCERPT_CHARS, download_progress, notes_excerpt, update_state_text},
};

/// How the header banner treats an updater state.
#[derive(Clone, Debug, PartialEq, Eq)]
enum BannerKind {
    /// Nothing to show.
    Hidden,
    /// Shown until dismissed; a dismissed key stays hidden for the session.
    Dismissible(String),
    /// Shown and not dismissible: an automatic install counting down, or an install running.
    Pinned,
}

fn banner_kind(state: &UpdateState) -> BannerKind {
    match state {
        UpdateState::Idle | UpdateState::Checking | UpdateState::UpToDate { .. } => {
            BannerKind::Hidden
        }
        // Keyed by version: dismissing an available update also hides its download and the
        // "ready" note, which the user can still find in Settings.
        UpdateState::Available { version, .. }
        | UpdateState::Downloading { version, .. }
        | UpdateState::ReadyToInstall {
            version,
            countdown_secs: None,
        } => BannerKind::Dismissible(format!("version {version}")),
        UpdateState::ReadyToInstall {
            countdown_secs: Some(_),
            ..
        }
        | UpdateState::Installing { .. } => BannerKind::Pinned,
        UpdateState::Error { message, .. } => BannerKind::Dismissible(format!("error {message}")),
    }
}

/// What the banner headline says, without the parts that change while it shows (download
/// progress, countdown seconds, relative times).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Headline {
    Available {
        version: String,
        current: String,
        pub_date: Option<SystemTime>,
    },
    Downloading {
        version: String,
    },
    Ready {
        version: String,
    },
    Countdown {
        version: String,
    },
    Installing {
        version: String,
    },
    Error {
        message: String,
        at: SystemTime,
    },
}

fn headline_for(info: &UpdateInfo) -> Option<Headline> {
    let version = |version: &String| version.clone();
    Some(match &info.state {
        UpdateState::Available {
            version, pub_date, ..
        } => Headline::Available {
            version: version.clone(),
            current: info.current_version.clone(),
            pub_date: *pub_date,
        },
        UpdateState::Downloading { version: v, .. } => Headline::Downloading {
            version: version(v),
        },
        UpdateState::ReadyToInstall {
            version: v,
            countdown_secs: None,
        } => Headline::Ready {
            version: version(v),
        },
        UpdateState::ReadyToInstall {
            version: v,
            countdown_secs: Some(_),
        } => Headline::Countdown {
            version: version(v),
        },
        UpdateState::Installing { version: v } => Headline::Installing {
            version: version(v),
        },
        UpdateState::Error { message, at } => Headline::Error {
            message: message.clone(),
            at: *at,
        },
        UpdateState::Idle | UpdateState::Checking | UpdateState::UpToDate { .. } => return None,
    })
}

/// While one of these runs, "Check for updates" would not change anything.
fn check_blocked(state: &UpdateState) -> bool {
    matches!(
        state,
        UpdateState::Checking
            | UpdateState::Downloading { .. }
            | UpdateState::Installing { .. }
            | UpdateState::ReadyToInstall {
                countdown_secs: Some(_),
                ..
            }
    )
}

/// The updater info with a placeholder until it is loaded, for views that read one field.
fn with_info<T>(state: AppState, read: impl FnOnce(Option<&UpdateInfo>) -> T) -> T {
    state.update.with(|info| read(info.as_ref()))
}

/// The banner under the header while an update is available, downloading, ready, counting down,
/// installing, or failed.
#[component]
pub fn UpdateBanner() -> impl IntoView {
    let state = AppState::get();
    let dismissed = RwSignal::new(Vec::<String>::new());

    // `Some(key)` while shown; the key is `Some` when the banner can be dismissed.
    let shown = Memo::new(move |_| {
        with_info(state, |info| match banner_kind(&info?.state) {
            BannerKind::Hidden => None,
            BannerKind::Pinned => Some(None),
            BannerKind::Dismissible(key) => {
                (!dismissed.with(|d| d.contains(&key))).then_some(Some(key))
            }
        })
    });
    let is_error = move || {
        with_info(state, |info| {
            matches!(info.map(|i| &i.state), Some(UpdateState::Error { .. }))
        })
    };

    // The headline is rebuilt only when its kind or wording changes; the download progress,
    // the countdown and "published 2 days ago" are reactive text inside it, so the aria-live
    // countdown span stays in place and the clock is only read where a text needs it.
    let headline_kind = Memo::new(move |_| with_info(state, |info| headline_for(info?)));
    let headline = move || {
        Some(match headline_kind.get()? {
            Headline::Available {
                version,
                current,
                pub_date,
            } => view! {
                <strong>{format!("Zebra Launcher {version} is available")}</strong>
                {format!(" (this is {current}")}
                {pub_date
                    .map(|date| move || format!(", published {}", relative(date, state.now.get())))}
                ")."
            }
            .into_any(),
            Headline::Downloading { version } => view! {
                <strong>{format!("Downloading Zebra Launcher {version}")}</strong>
                {move || {
                    with_info(state, |info| match info.map(|info| &info.state) {
                        Some(UpdateState::Downloading {
                            downloaded, total, ..
                        }) => format!(": {}.", download_progress(*downloaded, *total)),
                        _ => String::new(),
                    })
                }}
            }
            .into_any(),
            Headline::Ready { version } => view! {
                <strong>{format!("Zebra Launcher {version} is ready to install.")}</strong>
                " It is downloaded and its signature verified. Installing stops the node and restarts the launcher."
            }
            .into_any(),
            Headline::Countdown { version } => view! {
                <strong>
                    {format!("Zebra Launcher {version} installs automatically in ")}
                    <span class="countdown" aria-live="polite">
                        {move || {
                            with_info(state, |info| match info.map(|info| &info.state) {
                                Some(UpdateState::ReadyToInstall {
                                    countdown_secs: Some(secs),
                                    ..
                                }) => format!("{secs} s"),
                                _ => String::new(),
                            })
                        }}
                    </span>
                    "."
                </strong>
                " This zebrad release is near its end of support. The node will be stopped and the launcher restarts. Cancel to install later."
            }
            .into_any(),
            Headline::Installing { version } => view! {
                <strong>{format!("Installing Zebra Launcher {version}…")}</strong>
                " The node is being stopped; the launcher restarts next."
            }
            .into_any(),
            Headline::Error { message, at } => view! {
                <strong>"Launcher update failed"</strong>
                {move || format!(" ({}): {message}", relative(at, state.now.get()))}
            }
            .into_any(),
        })
    };
    let progress = move || {
        with_info(state, |info| match info?.state {
            UpdateState::Downloading {
                downloaded, total, ..
            } => Some(match total {
                Some(total) => view! {
                    <progress class="update-progress" max=total value=downloaded></progress>
                }
                .into_any(),
                // Indeterminate.
                None => view! { <progress class="update-progress"></progress> }.into_any(),
            }),
            _ => None,
        })
    };
    let notes = move || {
        with_info(state, |info| match &info?.state {
            UpdateState::Available {
                notes: Some(notes), ..
            } => notes_excerpt(notes, NOTES_EXCERPT_CHARS)
                .map(|excerpt| view! { <p class="update-notes">{excerpt}</p> }),
            _ => None,
        })
    };
    let cannot_install = move || {
        with_info(state, |info| {
            let info = info?;
            (!info.can_install && matches!(info.state, UpdateState::Available { .. })).then(|| {
                view! {
                    <p class="muted">
                        "This installation (the .deb package, or a development build) cannot update itself: download the new version from the releases page."
                    </p>
                }
            })
        })
    };
    let dismiss = move || {
        shown.get().flatten().map(|key| {
            view! {
                <button
                    type="button"
                    class="link"
                    on:click=move |_| dismissed.update(|d| d.push(key.clone()))
                >
                    "Dismiss"
                </button>
            }
        })
    };

    view! {
        <Show when=move || shown.with(Option::is_some)>
            <div
                class="banner update-banner"
                class:banner-update=move || !is_error()
                class:banner-error=is_error
                role="status"
            >
                <div class="update-body">
                    <p class="update-headline">{headline}</p>
                    {progress}
                    {notes}
                    {cannot_install}
                </div>
                <div class="buttons">
                    <UpdateActions />
                    {dismiss}
                </div>
            </div>
        </Show>
    }
}

/// The buttons for the current updater state: Install (or "Open releases page" where this
/// installation cannot update itself), cancelling a download or countdown, and checking again
/// after an error. Failures show next to them.
#[component]
pub fn UpdateActions() -> impl IntoView {
    let state = AppState::get();
    let busy = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);

    let run = move |what: &'static str, action: Action| {
        busy.set(true);
        error.set(None);
        spawn_local(async move {
            let result = match action {
                Action::Install => api::install_update().await,
                Action::Cancel => api::cancel_update().await,
                // The new state arrives as events, like every other.
                Action::Check => api::check_for_update().await.map(|_| ()),
                Action::OpenReleases => api::open_path(OpenTarget::ReleasesPage).await,
            };
            if let Err(failure) = result {
                error.set(Some(format!("Could not {what}: {failure}")));
            }
            busy.set(false);
        });
    };
    let button = move |label: &'static str, what: &'static str, action: Action, primary: bool| {
        view! {
            <button
                type="button"
                class:primary=primary
                disabled=move || busy.get()
                on:click=move |_| run(what, action)
            >
                {label}
            </button>
        }
        .into_any()
    };

    // Only the button set, so a progress or countdown event does not rebuild the buttons.
    let buttons_kind = Memo::new(move |_| with_info(state, buttons_for));
    let buttons = move || match buttons_kind.get() {
        Buttons::None => Vec::new(),
        Buttons::Install => vec![button(
            "Install",
            "install the update",
            Action::Install,
            true,
        )],
        Buttons::OpenReleases => vec![button(
            "Open releases page",
            "open the releases page",
            Action::OpenReleases,
            true,
        )],
        Buttons::CancelDownload => vec![button(
            "Cancel download",
            "cancel the download",
            Action::Cancel,
            false,
        )],
        Buttons::Countdown => vec![
            button("Install now", "install the update", Action::Install, true),
            button(
                "Cancel",
                "cancel the automatic install",
                Action::Cancel,
                false,
            ),
        ],
        Buttons::CheckAgain => vec![button(
            "Check again",
            "check for updates",
            Action::Check,
            false,
        )],
    };

    view! {
        {buttons}
        <OutcomeNote outcome=Signal::derive(move || error.get().map(Err)) />
    }
}

/// The update buttons a state shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Buttons {
    None,
    /// Install, where this installation can update itself.
    Install,
    /// Where it cannot.
    OpenReleases,
    CancelDownload,
    /// "Install now" and "Cancel" during an automatic-install countdown.
    Countdown,
    CheckAgain,
}

fn buttons_for(info: Option<&UpdateInfo>) -> Buttons {
    let Some(info) = info else {
        return Buttons::None;
    };
    match &info.state {
        UpdateState::Available { .. }
        | UpdateState::ReadyToInstall {
            countdown_secs: None,
            ..
        } => {
            if info.can_install {
                Buttons::Install
            } else {
                Buttons::OpenReleases
            }
        }
        UpdateState::Downloading { .. } => Buttons::CancelDownload,
        UpdateState::ReadyToInstall {
            countdown_secs: Some(_),
            ..
        } => Buttons::Countdown,
        UpdateState::Error { .. } => Buttons::CheckAgain,
        UpdateState::Idle
        | UpdateState::Checking
        | UpdateState::Installing { .. }
        | UpdateState::UpToDate { .. } => Buttons::None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Install,
    Cancel,
    Check,
    OpenReleases,
}

/// "Check for updates": asks the backend to check now, which works in every update mode (Off
/// included), and with `show_result` says what the check found next to the button.
#[component]
pub fn CheckForUpdate(
    #[prop(default = "Check for updates")] label: &'static str,
    #[prop(default = true)] show_result: bool,
) -> impl IntoView {
    let state = AppState::get();
    let busy = RwSignal::new(false);
    // What the check returned. It only feeds the note next to the button: `update_state` and
    // `update_info` events drive the updater state everywhere else, and may already be newer.
    // A state the check found stays for a while; a check that could not run, or found the
    // updater failed, holds until the next check.
    let outcome = Transient::<Result<UpdateState, String>>::new();

    let check = move |_| {
        busy.set(true);
        spawn_local(async move {
            match api::check_for_update().await {
                Ok(found @ UpdateState::Error { .. }) => outcome.hold(Ok(found)),
                Ok(found) => outcome.show(Ok(found)),
                Err(failure) => {
                    outcome.hold(Err(format!("Could not check for updates: {failure}")));
                }
            }
            busy.set(false);
        });
    };
    let blocked = move || {
        busy.get() || with_info(state, |info| info.is_some_and(|i| check_blocked(&i.state)))
    };
    // The note: a check that could not run always; what it found only where asked, a failed
    // updater as a failure.
    let note = Signal::derive(move || {
        outcome.get().and_then(|outcome| match outcome {
            Err(failure) => Some(Err(failure)),
            Ok(_) if !show_result => None,
            Ok(found @ UpdateState::Error { .. }) => {
                Some(Err(update_state_text(&found, state.now.get())))
            }
            Ok(found) => Some(Ok(update_state_text(&found, state.now.get()))),
        })
    });

    view! {
        <button type="button" disabled=blocked on:click=check>
            {move || if busy.get() { "Checking…" } else { label }}
        </button>
        <OutcomeNote outcome=note />
    }
}

/// The updater in Settings: the running version, the state, "Check now", the update buttons,
/// the stale-check warning and, after a failed update, the recovery options.
#[component]
pub fn UpdateStatus() -> impl IntoView {
    let state = AppState::get();

    // Built with the updater info; the two times in it follow the clock on their own.
    let summary = move || {
        with_info(state, |info| {
            let Some(info) = info else {
                return view! { <p class="muted">"Reading the updater state…"</p> }.into_any();
            };
            let last_check = info.last_successful_check;
            let update_state = info.state.clone();
            view! {
                <p class="muted">
                    {format!(
                        "This is Zebra Launcher {}. Last successful check: ",
                        info.current_version,
                    )}
                    {move || {
                        last_check.map_or_else(|| "never".to_owned(), |at| relative(at, state.now.get()))
                    }}
                    "."
                </p>
                <p class="update-state">
                    {move || update_state_text(&update_state, state.now.get())}
                </p>
            }
            .into_any()
        })
    };
    let notes = move || {
        with_info(state, |info| match &info?.state {
            UpdateState::Available {
                notes: Some(notes), ..
            } => notes_excerpt(notes, NOTES_EXCERPT_CHARS)
                .map(|excerpt| view! { <p class="update-notes">{excerpt}</p> }),
            _ => None,
        })
    };
    let hints = move || {
        with_info(state, |info| {
            let info = info?;
            Some(view! {
                {(info.mode == launcher_types::UpdateMode::Off)
                    .then(|| {
                        view! {
                            <p class="muted">
                                "Updates are off: the launcher only contacts the update server when you click Check now."
                            </p>
                        }
                    })}
                {(!info.can_install)
                    .then(|| {
                        view! {
                            <p class="muted">
                                "This installation (the .deb package, or a development build) cannot update itself. When an update is available, download it from the releases page."
                            </p>
                        }
                    })}
                {info
                    .stale_check_warning
                    .then(|| {
                        view! {
                            <div class="banner banner-warn stale-check" role="status">
                                "This zebrad release is near its end of support, and the launcher has not reached the update server for more than 14 days. Check now for a launcher with a newer zebrad."
                            </div>
                        }
                    })}
            })
        })
    };

    view! {
        <div class="update-status">
            {summary}
            {notes}
            <div class="buttons">
                <CheckForUpdate label="Check now" show_result=false />
                <UpdateActions />
            </div>
            {hints}
            <HealthFailurePanel />
        </div>
    }
}

/// The header banner after an update that did not come back healthy; the details and recovery
/// options are in Settings.
#[component]
pub fn HealthFailureBanner() -> impl IntoView {
    let state = AppState::get();
    let dismissed = RwSignal::new(false);
    move || {
        if dismissed.get() {
            return None;
        }
        let failure = state.health_failure()?;
        Some(view! {
            <div class="banner banner-error health-banner" role="alert">
                <span>
                    <strong>
                        {format!(
                            "The update to Zebra Launcher {} did not come back healthy. ",
                            failure.expected_version,
                        )}
                    </strong>
                    {failure.message}
                </span>
                <div class="buttons">
                    <button
                        type="button"
                        class="primary"
                        on:click=move |_| state.tab.set(Tab::Settings)
                    >
                        "Recovery options"
                    </button>
                    <button type="button" class="link" on:click=move |_| dismissed.set(true)>
                        "Dismiss"
                    </button>
                </div>
            </div>
        })
    }
}

/// What went wrong after an update, with a button for each recovery the backend suggests.
#[component]
fn HealthFailurePanel() -> impl IntoView {
    let state = AppState::get();
    let error = RwSignal::new(None::<String>);
    let open = move |target: OpenTarget, what: &'static str| {
        error.set(None);
        spawn_local(async move {
            if let Err(failure) = api::open_path(target).await {
                error.set(Some(format!("Could not open {what}: {failure}")));
            }
        });
    };

    let recovery = move |action: RecoveryAction| {
        match action {
        RecoveryAction::OpenLogs => view! {
            <li>
                <button type="button" on:click=move |_| open(OpenTarget::LogDir, "the log folder")>
                    "Open the log folder"
                </button>
                <span class="muted">"The launcher and zebrad logs show what went wrong."</span>
            </li>
        }
        .into_any(),
        // `reset_config` only runs while zebrad is stopped or failed; while it runs, the
        // manual way is explained instead.
        RecoveryAction::ResetConfig => view! {
            <li>
                <ResetConfigButton />
                {move || {
                    let stopped = state.status.with(|status| data_change_allowed(&status.phase));
                    (!stopped)
                        .then(|| {
                            view! {
                                <span>
                                    "Or reset the zebrad configuration by hand: remove the keys you added on the Configure page, or delete "
                                    <code>"zebrad.toml"</code>
                                    " from the config folder and restart the launcher, which then writes a fresh default file."
                                </span>
                                <div class="buttons">
                                    <button
                                        type="button"
                                        on:click=move |_| state.tab.set(Tab::Configure)
                                    >
                                        "Open Configure"
                                    </button>
                                    <button
                                        type="button"
                                        on:click=move |_| open(OpenTarget::ConfigDir, "the config folder")
                                    >
                                        "Open the config folder"
                                    </button>
                                </div>
                            }
                        })
                }}
            </li>
        }
        .into_any(),
        RecoveryAction::OpenReleasesPage => view! {
            <li>
                <button
                    type="button"
                    on:click=move |_| open(OpenTarget::ReleasesPage, "the releases page")
                >
                    "Open the releases page"
                </button>
                <span class="muted">
                    "To download and reinstall the previous version by hand. Updates never go back to an older version by themselves."
                </span>
            </li>
        }
        .into_any(),
    }
    };

    move || {
        let failure = state.health_failure()?;
        Some(view! {
            <div class="banner banner-error health-panel" role="alert">
                <p>
                    <strong>"The last launcher update did not come back healthy."</strong>
                </p>
                <p>{failure.message}</p>
                <Rows rows=text_rows([
                    ("Expected version", failure.expected_version),
                    ("Running version", failure.running_version),
                ]) />
                <ul class="recovery">
                    {failure.recovery.into_iter().map(recovery).collect_view()}
                </ul>
                <OutcomeNote outcome=Signal::derive(move || error.get().map(Err)) block=true />
            </div>
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use super::*;

    #[test]
    fn banner_shows_actionable_states() {
        let version = || "0.2.0".to_owned();
        for state in [
            UpdateState::Idle,
            UpdateState::Checking,
            UpdateState::UpToDate {
                checked_at: SystemTime::UNIX_EPOCH,
            },
        ] {
            assert_eq!(banner_kind(&state), BannerKind::Hidden, "{state:?}");
        }
        let available = UpdateState::Available {
            version: version(),
            notes: None,
            pub_date: None,
        };
        let downloading = UpdateState::Downloading {
            version: version(),
            downloaded: 1,
            total: None,
        };
        // Dismissing the available update also hides its download.
        assert_eq!(banner_kind(&available), banner_kind(&downloading));
        assert!(matches!(
            banner_kind(&available),
            BannerKind::Dismissible(_)
        ));
        for state in [
            UpdateState::ReadyToInstall {
                version: version(),
                countdown_secs: Some(5),
            },
            UpdateState::Installing { version: version() },
        ] {
            assert_eq!(banner_kind(&state), BannerKind::Pinned, "{state:?}");
        }
        assert!(check_blocked(&downloading));
        assert!(!check_blocked(&available));
    }

    #[test]
    fn buttons_change_with_the_kind_only() {
        let info = |state, can_install| UpdateInfo {
            state,
            mode: launcher_types::UpdateMode::Auto,
            current_version: "0.1.0".into(),
            last_successful_check: None,
            stale_check_warning: false,
            can_install,
            releases_url: String::new(),
            health_failure: None,
        };
        let downloading = |downloaded| UpdateState::Downloading {
            version: "0.2.0".into(),
            downloaded,
            total: Some(100),
        };
        let countdown = |secs| UpdateState::ReadyToInstall {
            version: "0.2.0".into(),
            countdown_secs: Some(secs),
        };
        assert_eq!(buttons_for(None), Buttons::None);
        assert_eq!(
            buttons_for(Some(&info(downloading(1), true))),
            buttons_for(Some(&info(downloading(99), true))),
        );
        assert_eq!(
            buttons_for(Some(&info(countdown(60), true))),
            buttons_for(Some(&info(countdown(3), true))),
        );
        let available = UpdateState::Available {
            version: "0.2.0".into(),
            notes: None,
            pub_date: None,
        };
        assert_eq!(
            buttons_for(Some(&info(available.clone(), true))),
            Buttons::Install
        );
        assert_eq!(
            buttons_for(Some(&info(available, false))),
            Buttons::OpenReleases
        );
    }
}
