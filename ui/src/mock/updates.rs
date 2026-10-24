//! The mock's launcher updater: the `?update=…` states, a check that finds 0.2.0, a download
//! in steps, the automatic-install countdown and the install that "restarts" as the new version.

use std::{sync::atomic::Ordering, time::Duration};

use launcher_types::{RecoveryAction, UpdateHealthFailure, UpdateInfo, UpdateMode, UpdateState};
use leptos::prelude::*;

use crate::clock;

use super::{Mock, mock, sleep, url_value};

/// The launcher version the mock runs.
pub(super) const MOCK_VERSION: &str = "0.1.0";

/// The version the mock's update server offers.
const MOCK_NEW_VERSION: &str = "0.2.0";

/// The size of the mock update download.
const MOCK_DOWNLOAD_BYTES: u64 = 84_000_000;

pub(super) const MOCK_RELEASES_URL: &str =
    "https://github.com/ZcashFoundation/zebra-launcher/releases";

const MOCK_NOTES: &str = "Bundles zebrad 7.1.0, whose end of support is about four months after \
its release, and makes the launcher check for updates once more before it halts. Also fixes the \
log view jumping when lines are dropped, and shows the free space of a data folder chosen in an \
earlier, unfinished first run.\n\n## Changes\n\n- zebrad 7.1.0\n- Fewer redraws in the log view";

/// What `?update=…` simulates. A check finds [`MOCK_NEW_VERSION`] with any of them except
/// `error` (the check fails) and `health` (nothing newer).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum UpdateFlag {
    /// `available`: an update was found (Notify mode).
    Available,
    /// `downloading`: Auto mode is downloading it.
    Downloading,
    /// `ready`: downloaded and verified, waiting for Install.
    Ready,
    /// `countdown`: Auto mode installs it after a 60 s countdown.
    Countdown,
    /// `error`: the last check failed.
    Error,
    /// `health`: the last update did not come back healthy.
    Health,
}

impl UpdateFlag {
    pub(super) fn from_url() -> Option<Self> {
        Some(match url_value("update")?.as_str() {
            "available" => UpdateFlag::Available,
            "downloading" => UpdateFlag::Downloading,
            "ready" => UpdateFlag::Ready,
            "countdown" => UpdateFlag::Countdown,
            "error" => UpdateFlag::Error,
            "health" => UpdateFlag::Health,
            _ => return None,
        })
    }

    pub(super) fn mode(self) -> UpdateMode {
        match self {
            UpdateFlag::Downloading | UpdateFlag::Countdown => UpdateMode::Auto,
            _ => UpdateMode::Notify,
        }
    }

    pub(super) fn initial_state(self) -> UpdateState {
        let version = MOCK_NEW_VERSION.to_owned();
        match self {
            UpdateFlag::Available => available(),
            UpdateFlag::Downloading => UpdateState::Downloading {
                version,
                downloaded: 0,
                total: Some(MOCK_DOWNLOAD_BYTES),
            },
            UpdateFlag::Ready => UpdateState::ReadyToInstall {
                version,
                countdown_secs: None,
            },
            UpdateFlag::Countdown => UpdateState::ReadyToInstall {
                version,
                countdown_secs: Some(60),
            },
            UpdateFlag::Error => check_error(),
            UpdateFlag::Health => UpdateState::Idle,
        }
    }
}

fn available() -> UpdateState {
    UpdateState::Available {
        version: MOCK_NEW_VERSION.into(),
        notes: Some(MOCK_NOTES.into()),
        pub_date: Some(clock::now() - Duration::from_secs(2 * 86_400)),
    }
}

fn check_error() -> UpdateState {
    UpdateState::Error {
        message: "(mock) could not fetch latest.json: error sending request for url \
                  (https://github.com/ZcashFoundation/zebra-launcher/releases/latest/download/latest.json)"
            .into(),
        at: clock::now(),
    }
}

pub(super) fn health_failure() -> UpdateHealthFailure {
    UpdateHealthFailure {
        expected_version: MOCK_NEW_VERSION.into(),
        running_version: MOCK_VERSION.into(),
        message: "(mock) zebrad did not reach Running with RPC within 5 minutes after the update"
            .into(),
        recovery: vec![
            RecoveryAction::OpenLogs,
            RecoveryAction::ResetConfig,
            RecoveryAction::OpenReleasesPage,
        ],
        releases_url: MOCK_RELEASES_URL.into(),
    }
}

impl Mock {
    pub(super) fn update_state(&self) -> UpdateState {
        self.update.with_untracked(|info| info.state.clone())
    }

    fn set_update_state(&self, state: UpdateState) {
        self.update.update(|info| info.state = state);
    }

    /// Runs `step` after `delay` unless another updater command was issued in between.
    fn after_update(&self, delay: Duration, step: impl FnOnce(&Mock) + 'static) {
        let generation = self.update_generation.load(Ordering::Relaxed);
        let mock = self.clone();
        set_timeout(
            move || {
                if mock.update_generation.load(Ordering::Relaxed) == generation {
                    step(&mock);
                }
            },
            delay,
        );
    }

    /// Downloads in steps, then installs (a user's Install) or waits (Auto mode).
    pub(super) fn download(&self) {
        self.after_update(Duration::from_millis(300), |mock| {
            let UpdateState::Downloading {
                version,
                downloaded,
                total,
            } = mock.update_state()
            else {
                return;
            };
            let downloaded = (downloaded + MOCK_DOWNLOAD_BYTES / 25).min(MOCK_DOWNLOAD_BYTES);
            if downloaded < MOCK_DOWNLOAD_BYTES {
                mock.set_update_state(UpdateState::Downloading {
                    version,
                    downloaded,
                    total,
                });
                mock.download();
            } else if mock.install_requested.load(Ordering::Relaxed) {
                mock.install(version);
            } else {
                mock.set_update_state(UpdateState::ReadyToInstall {
                    version,
                    countdown_secs: None,
                });
            }
        });
    }

    /// Counts the automatic install down, one second at a time.
    pub(super) fn count_down(&self) {
        self.after_update(Duration::from_secs(1), |mock| {
            let UpdateState::ReadyToInstall {
                version,
                countdown_secs: Some(secs),
            } = mock.update_state()
            else {
                return;
            };
            if secs <= 1 {
                mock.install(version);
            } else {
                mock.set_update_state(UpdateState::ReadyToInstall {
                    version,
                    countdown_secs: Some(secs - 1),
                });
                mock.count_down();
            }
        });
    }

    /// Installs, and "restarts" as the new version a moment later.
    fn install(&self, version: String) {
        self.install_requested.store(false, Ordering::Relaxed);
        self.set_update_state(UpdateState::Installing {
            version: version.clone(),
        });
        self.after_update(Duration::from_secs(3), move |mock| {
            mock.update.update(|info| {
                info.current_version = version;
                info.state = UpdateState::Idle;
            });
        });
    }
}

/// Calls `handler` with every updater state change, starting with the current state.
pub fn on_update_state(handler: impl Fn(UpdateState) + 'static) {
    let update = mock().update;
    let state = Memo::new(move |_| update.with(|info| info.state.clone()));
    Effect::new(move |_| handler(state.get()));
}

/// Calls `handler` with every change of the update info, starting with the current one.
pub fn on_update_info(handler: impl Fn(UpdateInfo) + 'static) {
    let update = mock().update;
    Effect::new(move |_| handler(update.get()));
}

/// Calls `handler` once the mock's health failure event fires (`?update=health`).
pub fn on_update_health_failed(handler: impl Fn(UpdateHealthFailure) + 'static) {
    let event = mock().health_event;
    Effect::new(move |_| {
        if let Some(failure) = event.get() {
            handler(failure);
        }
    });
}

pub async fn get_update_info() -> UpdateInfo {
    sleep(80).await;
    mock().update.get_untracked()
}

pub async fn check_for_update() -> UpdateState {
    let mock = mock();
    // A download, a countdown or an install keeps going; a check just reports it.
    if matches!(
        mock.update_state(),
        UpdateState::Downloading { .. }
            | UpdateState::ReadyToInstall { .. }
            | UpdateState::Installing { .. }
    ) {
        sleep(50).await;
        return mock.update_state();
    }
    let generation = mock.update_generation.fetch_add(1, Ordering::Relaxed) + 1;
    mock.set_update_state(UpdateState::Checking);
    sleep(1200).await;
    if mock.update_generation.load(Ordering::Relaxed) != generation {
        // Cancelled, or the mode changed meanwhile; whatever superseded it set the state.
        return mock.update_state();
    }
    let found = match mock.update_flag {
        Some(UpdateFlag::Error) => check_error(),
        None | Some(UpdateFlag::Health) => UpdateState::UpToDate {
            checked_at: clock::now(),
        },
        Some(_)
            if mock
                .update
                .with_untracked(|info| info.current_version != MOCK_VERSION) =>
        {
            UpdateState::UpToDate {
                checked_at: clock::now(),
            }
        }
        Some(_) => available(),
    };
    mock.update.update(|info| {
        if !matches!(found, UpdateState::Error { .. }) {
            info.last_successful_check = Some(clock::now());
            info.stale_check_warning = false;
        }
        info.state = found.clone();
    });
    found
}

pub async fn install_update() -> Result<(), String> {
    sleep(100).await;
    let mock = mock();
    if !mock.update.with_untracked(|info| info.can_install) {
        return Err("this installation cannot update itself; download the new version from the releases page".into());
    }
    // Validate first: bumping the generation cancels a running check, which must not happen
    // for an install that is refused.
    match mock.update_state() {
        UpdateState::Available { version, .. } => {
            mock.update_generation.fetch_add(1, Ordering::Relaxed);
            mock.install_requested.store(true, Ordering::Relaxed);
            mock.set_update_state(UpdateState::Downloading {
                version,
                downloaded: 0,
                total: Some(MOCK_DOWNLOAD_BYTES),
            });
            mock.download();
            Ok(())
        }
        UpdateState::ReadyToInstall { version, .. } => {
            mock.update_generation.fetch_add(1, Ordering::Relaxed);
            mock.install(version);
            Ok(())
        }
        _ => Err("no update is available; check for updates first".into()),
    }
}

pub async fn cancel_update() {
    sleep(50).await;
    let mock = mock();
    let cancelled = match mock.update_state() {
        UpdateState::Checking => UpdateState::Idle,
        UpdateState::Downloading { .. } => available(),
        UpdateState::ReadyToInstall {
            version,
            countdown_secs: Some(_),
        } => UpdateState::ReadyToInstall {
            version,
            countdown_secs: None,
        },
        _ => return,
    };
    mock.update_generation.fetch_add(1, Ordering::Relaxed);
    mock.install_requested.store(false, Ordering::Relaxed);
    mock.set_update_state(cancelled);
}
