//! Launcher updates: the header banner, the update buttons, "Check for updates", and the
//! recovery options after an update that did not come back healthy.

use launcher_types::{RecoveryAction, UpdateInfo, UpdateState};
use leptos::{prelude::*, task::spawn_local};

use crate::{
    api,
    app::{AppState, Tab},
    components::reset_config::ResetConfigButton,
    format::{
        NOTES_EXCERPT_CHARS, data_change_allowed, download_progress, notes_excerpt, relative,
        update_state_text,
    },
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

    let headline = move || {
        let now = state.now.get();
        with_info(state, |info| {
            let info = info?;
            let current = &info.current_version;
            Some(match &info.state {
                UpdateState::Available {
                    version, pub_date, ..
                } => view! {
                    <strong>{format!("Zebra Launcher {version} is available")}</strong>
                    {format!(
                        " (this is {current}{}).",
                        pub_date
                            .map(|date| format!(", published {}", relative(date, now)))
                            .unwrap_or_default(),
                    )}
                }
                .into_any(),
                UpdateState::Downloading {
                    version,
                    downloaded,
                    total,
                } => view! {
                    <strong>{format!("Downloading Zebra Launcher {version}")}</strong>
                    {format!(": {}.", download_progress(*downloaded, *total))}
                }
                .into_any(),
                UpdateState::ReadyToInstall {
                    version,
                    countdown_secs: None,
                } => view! {
                    <strong>{format!("Zebra Launcher {version} is ready to install.")}</strong>
                    " It is downloaded and its signature verified. Installing stops the node and restarts the launcher."
                }
                .into_any(),
                UpdateState::ReadyToInstall {
                    version,
                    countdown_secs: Some(secs),
                } => view! {
                    <strong>
                        {format!("Zebra Launcher {version} installs automatically in ")}
                        <span class="countdown" aria-live="polite">
                            {format!("{secs} s")}
                        </span>
                        "."
                    </strong>
                    " This zebrad release is near its end of support. The node will be stopped and the launcher restarts. Cancel to install later."
                }
                .into_any(),
                UpdateState::Installing { version } => view! {
                    <strong>{format!("Installing Zebra Launcher {version}…")}</strong>
                    " The node is being stopped; the launcher restarts next."
                }
                .into_any(),
                UpdateState::Error { message, at } => view! {
                    <strong>"Launcher update failed"</strong>
                    {format!(" ({}): {message}", relative(*at, now))}
                }
                .into_any(),
                UpdateState::Idle | UpdateState::Checking | UpdateState::UpToDate { .. } => {
                    return None;
                }
            })
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
                Action::Check => api::check_for_update().await.map(|found| {
                    set_state(state, found);
                }),
                Action::OpenReleases => api::open_path("releases_page").await,
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

    let buttons = move || {
        with_info(state, |info| {
            let Some(info) = info else {
                return Vec::new();
            };
            match &info.state {
                UpdateState::Available { .. }
                | UpdateState::ReadyToInstall {
                    countdown_secs: None,
                    ..
                } => {
                    if info.can_install {
                        vec![button(
                            "Install",
                            "install the update",
                            Action::Install,
                            true,
                        )]
                    } else {
                        vec![button(
                            "Open releases page",
                            "open the releases page",
                            Action::OpenReleases,
                            true,
                        )]
                    }
                }
                UpdateState::Downloading { .. } => vec![button(
                    "Cancel download",
                    "cancel the download",
                    Action::Cancel,
                    false,
                )],
                UpdateState::ReadyToInstall {
                    countdown_secs: Some(_),
                    ..
                } => vec![
                    button("Install now", "install the update", Action::Install, true),
                    button(
                        "Cancel",
                        "cancel the automatic install",
                        Action::Cancel,
                        false,
                    ),
                ],
                UpdateState::Error { .. } => vec![button(
                    "Check again",
                    "check for updates",
                    Action::Check,
                    false,
                )],
                UpdateState::Idle
                | UpdateState::Checking
                | UpdateState::Installing { .. }
                | UpdateState::UpToDate { .. } => Vec::new(),
            }
        })
    };

    view! {
        {buttons}
        {move || error.get().map(|error| view! { <span class="error">{error}</span> })}
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Install,
    Cancel,
    Check,
    OpenReleases,
}

/// Applies the state a command returned right away; the `update_state` event follows.
fn set_state(state: AppState, update_state: UpdateState) {
    state.update.update(|info| {
        if let Some(info) = info {
            info.state = update_state;
        }
    });
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
    let outcome = RwSignal::new(None::<Result<(), String>>);

    let check = move |_| {
        busy.set(true);
        outcome.set(None);
        spawn_local(async move {
            outcome.set(Some(match api::check_for_update().await {
                Ok(found) => {
                    set_state(state, found);
                    Ok(())
                }
                Err(failure) => Err(format!("Could not check for updates: {failure}")),
            }));
            busy.set(false);
        });
    };
    let blocked = move || {
        busy.get() || with_info(state, |info| info.is_some_and(|i| check_blocked(&i.state)))
    };
    // What the check found, kept current: an update found can start downloading, and so on.
    let result = move || match outcome.get()? {
        Ok(()) if show_result => {
            let now = state.now.get();
            with_info(state, |info| {
                info.map(|info| {
                    view! { <span class="update-result">{update_state_text(&info.state, now)}</span> }
                        .into_any()
                })
            })
        }
        Ok(()) => None,
        Err(error) => Some(view! { <span class="error">{error}</span> }.into_any()),
    };

    view! {
        <button type="button" disabled=blocked on:click=check>
            {move || if busy.get() { "Checking…" } else { label }}
        </button>
        {result}
    }
}

/// The updater in Settings: the running version, the state, "Check now", the update buttons,
/// the stale-check warning and, after a failed update, the recovery options.
#[component]
pub fn UpdateStatus() -> impl IntoView {
    let state = AppState::get();

    let summary = move || {
        let now = state.now.get();
        with_info(state, |info| {
            let Some(info) = info else {
                return view! { <p class="muted">"Reading the updater state…"</p> }.into_any();
            };
            let last_check = info
                .last_successful_check
                .map_or_else(|| "never".to_owned(), |at| relative(at, now));
            view! {
                <p class="muted">
                    {format!(
                        "This is Zebra Launcher {}. Last successful check: {last_check}.",
                        info.current_version,
                    )}
                </p>
                <p class="update-state">{update_state_text(&info.state, now)}</p>
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
    let open = move |target: &'static str, what: &'static str| {
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
                <button type="button" on:click=move |_| open("log_dir", "the log folder")>
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
                                        on:click=move |_| open("config_dir", "the config folder")
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
                    on:click=move |_| open("releases_page", "the releases page")
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
                <dl class="rows">
                    <dt>"Expected version"</dt>
                    <dd>{failure.expected_version}</dd>
                    <dt>"Running version"</dt>
                    <dd>{failure.running_version}</dd>
                </dl>
                <ul class="recovery">
                    {failure.recovery.into_iter().map(recovery).collect_view()}
                </ul>
                {move || error.get().map(|error| view! { <p class="error">{error}</p> })}
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
}
