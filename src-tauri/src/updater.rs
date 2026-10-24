//! Launcher self-updates: `tauri-plugin-updater` as the backend of the
//! launcher-core updater actor, the bridge to the UI, and the post-update
//! health gate.
//!
//! The update policy (modes, schedule, end-of-support countdown) lives in
//! [`launcher_core::updates`]. This module only:
//!
//! - builds a plugin `Updater` per check, from the endpoints in
//!   `tauri.conf.json` (`plugins.updater.endpoints`). The actor never asks for
//!   a check in Off mode unless the user does, so no updater is built and no
//!   request is made. Release builds honour no runtime endpoint override; the
//!   QA override is build-time (`TAURI_CONFIG` / `--config`, see
//!   docs/RELEASING.md). Debug builds also read `LAUNCHER_UPDATE_ENDPOINT`.
//! - emits `update_info` (an [`UpdateInfo`]) on every change and
//!   `update_state` (its [`UpdateState`]) when the state changes;
//! - runs the health gate when the previous launch installed an update, and
//!   emits `update_health_failed` if the update did not come back healthy.

use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use launcher_core::updates::{
    self, FoundUpdate, NodeControl, OnChunk, UpdateBackend, UpdaterConfig, UpdaterHandle,
};
use launcher_types::{Status, UpdateInfo, UpdateState};
use tauri::{
    AppHandle, Emitter, Runtime,
    utils::{config::BundleType, platform::bundle_type},
};
use tauri_plugin_updater::{Update, Updater, UpdaterExt};
use tokio::sync::watch;

use crate::{app_state::AppState, commands::RELEASES_URL};

/// The event carrying every [`UpdateState`] change.
pub const UPDATE_STATE_EVENT: &str = "update_state";
/// The event carrying every [`UpdateInfo`] change.
pub const UPDATE_INFO_EVENT: &str = "update_info";
/// The event emitted when an update did not come back healthy, with an
/// [`launcher_types::UpdateHealthFailure`].
pub const UPDATE_HEALTH_FAILED_EVENT: &str = "update_health_failed";

/// How long a check may take.
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a download may take in total. The plugin copies the builder's
/// timeout into the `Update` and applies it to the download request as an
/// overall limit, so the check's 30 s would end any large download; the
/// download gets this instead. The user can cancel it sooner.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// Development only: replaces the configured endpoints. Not read by release
/// builds.
#[cfg(debug_assertions)]
const ENDPOINT_ENV: &str = "LAUNCHER_UPDATE_ENDPOINT";

/// The updater backend over `tauri-plugin-updater`.
pub struct PluginBackend<R: Runtime> {
    app: AppHandle<R>,
}

fn message(err: impl std::fmt::Display) -> String {
    err.to_string()
}

impl<R: Runtime> PluginBackend<R> {
    /// Builds a plugin updater. Only called for a check.
    fn updater(&self) -> Result<Updater, String> {
        let mut builder = self.app.updater_builder().timeout(CHECK_TIMEOUT);
        if let Some(endpoints) = dev_endpoints()? {
            builder = builder.endpoints(endpoints).map_err(message)?;
        }
        builder.build().map_err(message)
    }
}

#[cfg(debug_assertions)]
fn dev_endpoints() -> Result<Option<Vec<tauri::Url>>, String> {
    match std::env::var(ENDPOINT_ENV) {
        Ok(url) => {
            let url = url
                .parse()
                .map_err(|err| format!("{ENDPOINT_ENV} is not a URL: {err}"))?;
            tracing::warn!(%url, "using the development update endpoint");
            Ok(Some(vec![url]))
        }
        Err(_) => Ok(None),
    }
}

#[cfg(not(debug_assertions))]
fn dev_endpoints() -> Result<Option<Vec<tauri::Url>>, String> {
    Ok(None)
}

/// Whether this installation can replace itself: the AppImage, the NSIS
/// installer and the macOS app can. The `.deb` cannot (its updates need root
/// and the package manager), and neither can a development build.
///
/// On macOS `bundle_type()` reports `App` for any build, bundled or not, so
/// the running executable must also be inside a `.app` bundle, and a debug
/// build never counts.
fn can_self_update() -> bool {
    match bundle_type() {
        Some(BundleType::AppImage | BundleType::Nsis) => true,
        Some(BundleType::App) => {
            !cfg!(debug_assertions)
                && std::env::current_exe().is_ok_and(|exe| is_in_app_bundle(&exe))
        }
        _ => false,
    }
}

/// `exe` is the main executable of a macOS `.app` bundle
/// (`<name>.app/Contents/MacOS/<exe>`).
fn is_in_app_bundle(exe: &std::path::Path) -> bool {
    let mut dirs = exe.ancestors().skip(1).map(std::path::Path::file_name);
    matches!(
        (dirs.next(), dirs.next(), dirs.next()),
        (Some(Some(macos)), Some(Some(contents)), Some(Some(app)))
            if macos == "MacOS"
                && contents == "Contents"
                && std::path::Path::new(app).extension().is_some_and(|ext| ext == "app")
    )
}

impl<R: Runtime> UpdateBackend for PluginBackend<R> {
    type Release = Update;

    fn can_install(&self) -> bool {
        can_self_update()
    }

    async fn check(&self) -> Result<Option<FoundUpdate<Update>>, String> {
        let updater = self.updater()?;
        let update = updater.check().await.map_err(message)?;
        Ok(update.map(|update| FoundUpdate {
            version: update.version.clone(),
            notes: update.body.clone(),
            // `time::OffsetDateTime`, which the plugin does not re-export.
            pub_date: update
                .date
                .and_then(|date| u64::try_from(date.unix_timestamp()).ok())
                .and_then(|secs| UNIX_EPOCH.checked_add(Duration::from_secs(secs))),
            release: update,
        }))
    }

    async fn download(&self, release: &Update, on_chunk: OnChunk) -> Result<Vec<u8>, String> {
        // The check's timeout came along in `release.timeout`; replace it.
        let mut release = release.clone();
        release.timeout = Some(DOWNLOAD_TIMEOUT);
        release.download(on_chunk, || {}).await.map_err(message)
    }

    fn verify(&self, release: &Update, bytes: &[u8]) -> Result<(), String> {
        release.verify(bytes).map_err(message)
    }

    async fn install(&self, release: &Update, bytes: Arc<Vec<u8>>) -> Result<(), String> {
        let release = release.clone();
        // Unpacks and replaces files; on Windows it starts the installer and
        // exits the process.
        tauri::async_runtime::spawn_blocking(move || release.install(bytes.as_slice()))
            .await
            .map_err(message)?
            .map_err(message)
    }

    fn restart(&self) {
        // Goes through the exit flow (zebrad is already stopped), then relaunches.
        self.app.request_restart();
    }
}

/// The zebrad supervisor as the updater sees it: stops go straight to the
/// supervisor, but starts (the health gate's, and the restart after a failed
/// install) go through [`crate::start_node`], the same path as a start at
/// launch or from the tray: not before the first run is done, not while the
/// node data is being changed, and a zebrad left by an earlier run is
/// reported to the UI (`stale_process`); the health gate waits for the user
/// to resolve it.
struct GatedNode<R: Runtime> {
    app: AppHandle<R>,
    state: AppState,
}

// Not derived: that would require `R: Clone`.
impl<R: Runtime> Clone for GatedNode<R> {
    fn clone(&self) -> Self {
        Self {
            app: self.app.clone(),
            state: self.state.clone(),
        }
    }
}

impl<R: Runtime> NodeControl for GatedNode<R> {
    fn watch_status(&self) -> watch::Receiver<Status> {
        self.state.supervisor.watch_status()
    }

    async fn stop(&self) -> Result<(), String> {
        NodeControl::stop(&self.state.supervisor).await
    }

    async fn start(&self) -> Result<(), String> {
        crate::start_node(&self.app, &self.state).await
    }
}

/// A seed for the check jitter that differs between launches and machines.
fn seed() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or_default();
    nanos ^ (u64::from(std::process::id()) << 32)
}

/// Starts the updater actor, its event bridge and, if the previous launch
/// installed an update, the health gate. Call once from setup; manage the
/// returned handle. `gate_may_start` is [`crate::LaunchPlan::gate_may_start`]:
/// whether the gate starts zebrad when the update stopped a running one.
pub fn start<R: Runtime>(
    app: &AppHandle<R>,
    state: &AppState,
    gate_may_start: bool,
) -> UpdaterHandle {
    let current_version = app.package_info().version.to_string();
    let record_file = updates::record_path(&state.paths);
    let marker_file = updates::marker_path(&state.paths);
    let config = UpdaterConfig {
        current_version: current_version.clone(),
        releases_url: RELEASES_URL.to_string(),
        record: updates::load_record(&record_file),
        record_file: Some(record_file),
        marker_file: Some(marker_file.clone()),
        seed: seed(),
        countdown: updates::AUTO_INSTALL_COUNTDOWN,
    };
    let backend = PluginBackend { app: app.clone() };
    // The actor is spawned with `tokio::spawn`, so enter the runtime.
    let node = GatedNode {
        app: app.clone(),
        state: state.clone(),
    };
    let handle = tauri::async_runtime::block_on(async {
        updates::spawn(
            backend,
            node.clone(),
            state.settings.watch(),
            state.node_stats.clone(),
            config,
        )
    });
    tauri::async_runtime::spawn(forward(app.clone(), handle.watch()));

    let updater = handle.clone();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let gate = updates::run_health_gate(
            &marker_file,
            &current_version,
            &node,
            gate_may_start,
            updates::HEALTH_GATE_TIMEOUT,
            RELEASES_URL,
        )
        .await;
        if let Some(Err(failure)) = gate {
            let _ = updater.report_health_failure(failure.clone()).await;
            if let Err(err) = app.emit(UPDATE_HEALTH_FAILED_EVENT, failure) {
                tracing::warn!(%err, "could not emit update_health_failed");
            }
        }
    });
    handle
}

/// Emits `update_info` on every change and `update_state` when the state
/// changed, until the updater ends.
async fn forward<R: Runtime>(app: AppHandle<R>, mut rx: watch::Receiver<UpdateInfo>) {
    let mut last_state: Option<UpdateState> = None;
    loop {
        let info = rx.borrow_and_update().clone();
        if last_state.as_ref() != Some(&info.state) {
            if let Err(err) = app.emit(UPDATE_STATE_EVENT, info.state.clone()) {
                tracing::warn!(%err, "could not emit update_state");
            }
            last_state = Some(info.state.clone());
        }
        if let Err(err) = app.emit(UPDATE_INFO_EVENT, info) {
            tracing::warn!(%err, "could not emit update_info");
        }
        if rx.changed().await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::is_in_app_bundle;

    #[test]
    fn app_bundle_detection() {
        assert!(is_in_app_bundle(Path::new(
            "/Applications/Zebra Launcher.app/Contents/MacOS/zebra-launcher"
        )));
        assert!(!is_in_app_bundle(Path::new(
            "/Users/me/zebra-launcher/target/debug/zebra-launcher"
        )));
        assert!(!is_in_app_bundle(Path::new(
            "/Applications/Zebra Launcher.app/Contents/Resources/zebra-launcher"
        )));
        assert!(!is_in_app_bundle(Path::new(
            "/Contents/MacOS/zebra-launcher"
        )));
    }
}
