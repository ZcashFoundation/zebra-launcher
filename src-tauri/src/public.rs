//! The public services in the app: the [`EndpointHost`] that writes zebrad's light wallet
//! endpoint and restarts zebrad behind the app's permits, and the `public_status` event.

use std::{future::Future, pin::Pin, sync::Arc};

use launcher_core::config::{self, LightwalletdKey};
use launcher_public::{
    EndpointHost, Options, PublicServices, Wiring, ddns::HttpsFetch, secrets::Keychain,
};
use launcher_types::{events, public::PublicStatus};
use tauri::{AppHandle, Emitter, Runtime};

use crate::{app_state::AppState, try_state};

/// The bundled zebrad's version, until zebrad reports its own.
const ZEBRAD_VERSION: &str = include_str!("../../ZEBRAD_VERSION");

/// How long applying the endpoint waits for zebrad's restart: its stop grace and a start.
const RESTART_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

/// Applies the managed keys the way `apply_config` does around a restart: the config permit,
/// the start permit while zebrad runs, `config::ensure`, and a restart only if zebrad was
/// running. A busy permit fails fast, so the service actor clears the port again.
struct Host<R: Runtime> {
    app: AppHandle<R>,
}

impl<R: Runtime> EndpointHost for Host<R> {
    fn apply(&self, restart: bool) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> {
        let state = try_state(&self.app);
        Box::pin(async move {
            let state = state.ok_or("Zeal is still starting")?;
            // PS1 serves from the first node (docs/PUBLIC-SERVICES.md).
            let node = state.first_node()?;
            let _config = node.gates.try_config_permit()?;
            let running = node.supervisor.status().phase.is_active();
            let _start = if running && restart {
                Some(node.gates.start_permit()?)
            } else {
                None
            };
            let paths = node.paths.clone();
            let mut keys = node.managed_keys();
            keys.lightwalletd =
                LightwalletdKey::for_public(&state.settings.current().public, node.spec().network);
            tauri::async_runtime::spawn_blocking(move || config::ensure(&paths, &keys))
                .await
                .map_err(|err| err.to_string())?
                .map_err(|err| format!("zebrad.toml could not be updated: {err}"))?;
            if running && restart {
                tracing::info!("restarting zebrad for the light wallet endpoint");
                // A start resolves once zebrad accepted its configuration, as in apply_config.
                tokio::time::timeout(RESTART_TIMEOUT, node.supervisor.restart())
                    .await
                    .map_err(|_| "zebrad did not restart in time".to_string())?
                    .map_err(|err| err.to_string())?;
            }
            Ok(())
        })
    }
}

/// Starts the public services and forwards their status to the UI. `held` comes from
/// [`launcher_public::prepare_launch`].
pub fn start<R: Runtime>(app: &AppHandle<R>, state: &AppState, held: bool) -> PublicServices {
    // The registry always holds a node, and PS1 serves from the first.
    let node = state
        .registry
        .first()
        .expect("the registry always holds a node");
    let options = Options::new(state.paths.data_dir(), ZEBRAD_VERSION.trim());
    let fetch: Arc<dyn launcher_public::ddns::Fetch> = match HttpsFetch::new() {
        Ok(fetch) => Arc::new(fetch),
        Err(err) => {
            tracing::warn!(%err, "no HTTPS client for dynamic DNS");
            Arc::new(Unreachable)
        }
    };
    let wiring = Wiring {
        settings: state.settings.clone(),
        node: node.watch_spec(),
        status: node.supervisor.watch_status(),
        stats: node.stats.clone(),
        host: Arc::new(Host { app: app.clone() }),
        secrets: Arc::new(Keychain),
        fetch,
    };
    // The actor and its forwarders are Tokio tasks, and the setup hook runs outside the
    // runtime: spawn them inside it, as the explorer does.
    let services =
        tauri::async_runtime::block_on(async move { PublicServices::spawn(options, wiring, held) });
    let app = app.clone();
    let mut rx = services.watch();
    tauri::async_runtime::spawn(async move {
        let mut sent: Vec<PublicStatus> = Vec::new();
        loop {
            let statuses = rx.borrow_and_update().clone();
            for status in &statuses {
                if !sent.contains(status)
                    && let Err(err) = app.emit(events::PUBLIC_STATUS, status)
                {
                    tracing::warn!(%err, "could not emit public_status");
                }
            }
            sent = statuses;
            if rx.changed().await.is_err() {
                return;
            }
        }
    });
    services
}

/// A dynamic DNS client that never reaches anything, when no HTTPS client could be built.
struct Unreachable;

impl launcher_public::ddns::Fetch for Unreachable {
    fn get(&self, _url: String) -> launcher_public::ddns::Fetched {
        Box::pin(async { Err(()) })
    }
}
