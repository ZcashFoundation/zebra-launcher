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
//! - emits [`events::UPDATE_INFO`] (an [`UpdateInfo`]) on every change and
//!   [`events::UPDATE_STATE`] (its [`UpdateState`]) when the state changes;
//! - runs the health gate when the previous launch installed an update, and
//!   emits [`events::UPDATE_HEALTH_FAILED`] if the update did not come back
//!   healthy.

use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use launcher_core::{
    registry::NodeHandle,
    updates::{
        self, FoundUpdate, GateNode, NodeControl, OnChunk, PendingUpdate, UpdateBackend,
        UpdaterConfig, UpdaterHandle,
    },
};
use launcher_types::{NodeId, NodeStats, Status, UpdateInfo, UpdateState, events};
use tauri::{
    AppHandle, Emitter, Runtime,
    utils::{config::BundleType, platform::bundle_type},
};
use tauri_plugin_updater::{Update, Updater, UpdaterExt};
use tokio::sync::watch;

use crate::{LaunchPlan, app_state::AppState, commands::RELEASES_URL};

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

/// One node as the post-update health gate sees it: stops go straight to its
/// supervisor, but starts go through [`crate::start_node`], the same path as
/// a start at launch or from the tray: not before the first run is done, not
/// while the node data is being changed, and a zebrad left by an earlier run
/// is reported to the UI (`stale_process`); the health gate waits for the
/// user to resolve it.
struct GatedNode<R: Runtime> {
    app: AppHandle<R>,
    state: AppState,
    node: NodeHandle,
}

// Not derived: that would require `R: Clone`.
impl<R: Runtime> Clone for GatedNode<R> {
    fn clone(&self) -> Self {
        Self {
            app: self.app.clone(),
            state: self.state.clone(),
            node: self.node.clone(),
        }
    }
}

impl<R: Runtime> NodeControl for GatedNode<R> {
    fn watch_status(&self) -> watch::Receiver<Status> {
        self.node.supervisor.watch_status()
    }

    fn watch_stats(&self) -> Option<watch::Receiver<Option<NodeStats>>> {
        Some(self.node.stats.clone())
    }

    async fn stop(&self) -> Result<(), String> {
        NodeControl::stop(&self.node.supervisor).await
    }

    async fn start(&self) -> Result<(), String> {
        crate::start_node(&self.app, &self.state, &self.node).await
    }

    fn shutdown_cap(&self) -> Duration {
        self.node.supervisor.shutdown_cap()
    }
}

/// Every node as the updater's install sees them: a stop stops every node
/// that runs (each killed after its own grace period, side by side) and the
/// registry remembers which ran; a start brings those back through
/// [`crate::start_node`]. The status is the nodes' phases folded into one
/// ([`launcher_core::registry::herd_phase`]).
struct Herd<R: Runtime> {
    app: AppHandle<R>,
    state: AppState,
    status: watch::Receiver<Status>,
}

// Not derived: that would require `R: Clone`.
impl<R: Runtime> Clone for Herd<R> {
    fn clone(&self) -> Self {
        Self {
            app: self.app.clone(),
            state: self.state.clone(),
            status: self.status.clone(),
        }
    }
}

impl<R: Runtime> NodeControl for Herd<R> {
    fn watch_status(&self) -> watch::Receiver<Status> {
        self.status.clone()
    }

    fn shutdown_cap(&self) -> Duration {
        self.state.registry.shutdown_cap()
    }

    async fn stop(&self) -> Result<(), String> {
        self.state
            .registry
            .stop_for_update()
            .await
            .map_err(|err| err.to_string())
    }

    async fn start(&self) -> Result<(), String> {
        let stopped = self.state.registry.take_stopped_for_update().await;
        let mut starts = Vec::new();
        // A node removed since it was stopped stays gone.
        for node in self.state.registry.nodes_of(&stopped) {
            let (app, state) = (self.app.clone(), self.state.clone());
            starts.push(tauri::async_runtime::spawn(async move {
                crate::start_node(&app, &state, &node).await
            }));
        }
        let mut result = Ok(());
        for start in starts {
            if let Err(err) = start.await.map_err(message).and_then(|started| started) {
                result = Err(err);
            }
        }
        result
    }

    fn running_nodes(&self) -> Vec<NodeId> {
        self.state.registry.running_nodes()
    }
}

/// The nodes the health gate checks after an update: those the marker lists
/// as running ([`PendingUpdate::nodes_to_check`]) that are still there, each
/// started by the gate unless the launcher starts it anyway, and named when
/// there are several.
fn gate_nodes<R: Runtime>(
    app: &AppHandle<R>,
    state: &AppState,
    plan: &LaunchPlan,
    pending: &PendingUpdate,
) -> Vec<GateNode<GatedNode<R>>> {
    let first = state.registry.first().map(|node| node.id);
    let ids = pending.nodes_to_check(first.as_ref());
    let several = ids.len() > 1;
    state
        .registry
        .nodes_of(&ids)
        .into_iter()
        .map(|node| {
            let program = node.spec().program;
            GateNode {
                start: plan.gate_may_start(&node.id),
                name: several.then(|| node.info().name),
                program,
                node: GatedNode {
                    app: app.clone(),
                    state: state.clone(),
                    node,
                },
            }
        })
        .collect()
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
/// returned handle. `plan` says which nodes the launcher starts itself, so
/// the gate starts only the others that the update stopped.
pub(crate) fn start<R: Runtime>(
    app: &AppHandle<R>,
    state: &AppState,
    plan: &LaunchPlan,
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
    // The actor and the registry's followers are spawned with `tokio::spawn`,
    // so enter the runtime.
    let handle = tauri::async_runtime::block_on(async {
        let herd = Herd {
            app: app.clone(),
            state: state.clone(),
            status: state.registry.watch_herd(),
        };
        updates::spawn(
            backend,
            herd,
            state.settings.watch(),
            state.registry.eos_stats(),
            config,
        )
    });
    tauri::async_runtime::spawn(forward(app.clone(), handle.watch()));

    let updater = handle.clone();
    let (app, state, plan) = (app.clone(), state.clone(), plan.clone());
    tauri::async_runtime::spawn(async move {
        let gate = updates::run_health_gate(
            &marker_file,
            &current_version,
            |pending| gate_nodes(&app, &state, &plan, pending),
            updates::HEALTH_GATE_TIMEOUT,
            RELEASES_URL,
        )
        .await;
        if let Some(Err(failure)) = gate {
            let _ = updater.report_health_failure(failure.clone()).await;
            if let Err(err) = app.emit(events::UPDATE_HEALTH_FAILED, failure) {
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
            if let Err(err) = app.emit(events::UPDATE_STATE, info.state.clone()) {
                tracing::warn!(%err, "could not emit update_state");
            }
            last_state = Some(info.state.clone());
        }
        if let Err(err) = app.emit(events::UPDATE_INFO, info) {
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
            "/Applications/Zeal.app/Contents/MacOS/zeal-app"
        )));
        assert!(!is_in_app_bundle(Path::new(
            "/Users/me/zeal/target/debug/zeal-app"
        )));
        assert!(!is_in_app_bundle(Path::new(
            "/Applications/Zeal.app/Contents/Resources/zeal-app"
        )));
        assert!(!is_in_app_bundle(Path::new("/Contents/MacOS/zeal-app")));
    }
}
