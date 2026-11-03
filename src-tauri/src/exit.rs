//! Quitting stops every node first.
//!
//! On [`RunEvent::ExitRequested`] the exit is prevented, every node's
//! supervisor is shut down side by side (graceful stop, then kill after that
//! node's grace period) with an overall cap, and the app exits again with
//! the requested code. The second request
//! passes through because the guard records that the shutdown finished.
//! [`RunEvent::Exit`] is the last resort when the event loop ends without a
//! request we could prevent: it blocks until the shutdown finishes or the cap
//! expires.
//!
//! Closing the main window hides it instead when `close_to_tray` is on and
//! the tray icon exists, so the nodes keep running; without a tray (Linux
//! without libappindicator) closing the window quits as above. The tray's
//! Show item, a second launch and the macOS dock icon show it again.

use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, Manager, RunEvent, Runtime, WindowEvent};

use crate::app_state::AppState;

/// Records the progress of the exit flow. Atomics only: the flow runs on the
/// event loop thread and in one task.
#[derive(Debug, Default)]
pub struct ExitGuard {
    started: AtomicBool,
    finished: AtomicBool,
}

/// The `App::run` callback.
pub fn on_run_event<R: Runtime>(app: &AppHandle<R>, event: RunEvent) {
    match event {
        RunEvent::ExitRequested { api, code, .. } => {
            // Without state, startup failed and there is nothing to stop.
            let Some(state) = app.try_state::<AppState>() else {
                return;
            };
            if state.exit.finished.load(Ordering::SeqCst) {
                return;
            }
            api.prevent_exit();
            if state.exit.started.swap(true, Ordering::SeqCst) {
                return;
            }
            let state = state.inner().clone();
            let public = public_services(app);
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                shutdown(&state, public).await;
                state.exit.finished.store(true, Ordering::SeqCst);
                if code == Some(tauri::RESTART_EXIT_CODE) {
                    app.request_restart();
                } else {
                    app.exit(code.unwrap_or(0));
                }
            });
        }
        RunEvent::WindowEvent {
            label,
            event: WindowEvent::CloseRequested { api, .. },
            ..
        } => {
            let Some(state) = app.try_state::<AppState>() else {
                return;
            };
            let hide = !state.exit.started.load(Ordering::SeqCst)
                && state.tray_ready.load(Ordering::SeqCst)
                && state.settings.current().close_to_tray;
            if hide && let Some(window) = app.get_webview_window(&label) {
                api.prevent_close();
                if let Err(err) = window.hide() {
                    tracing::warn!(%err, "could not hide the window");
                }
                tracing::info!("window hidden to the tray; the nodes keep running");
            }
        }
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { .. } => crate::show_main_window(app),
        RunEvent::Exit => {
            let Some(state) = app.try_state::<AppState>() else {
                return;
            };
            if state.exit.finished.load(Ordering::SeqCst) {
                return;
            }
            state.exit.started.store(true, Ordering::SeqCst);
            tracing::warn!("exiting without a prior exit request; stopping the nodes");
            // If a shutdown is already running, this waits for the actor to end.
            tauri::async_runtime::block_on(shutdown(state.inner(), public_services(app)));
            state.exit.finished.store(true, Ordering::SeqCst);
        }
        _ => {}
    }
}

/// The public services, once startup managed them.
fn public_services<R: Runtime>(app: &AppHandle<R>) -> Option<launcher_public::PublicServices> {
    app.try_state::<launcher_public::PublicServices>()
        .map(|public| public.inner().clone())
}

/// Closes the public services' front door, then stops every node and its supervisor,
/// giving up after the cap.
///
/// The cap is the longest of the supervisors' (they stop side by side):
/// each grace is fixed at launch, while the settings may have changed since.
async fn shutdown(state: &AppState, public: Option<launcher_public::PublicServices>) {
    if let Some(public) = public {
        // Nothing public outlives Zeal: the front door closes before the nodes stop.
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), public.shutdown()).await;
    }
    let cap = state.registry.shutdown_cap();
    tracing::info!(cap_secs = cap.as_secs(), "stopping the nodes before exit");
    match tokio::time::timeout(cap, state.registry.shutdown()).await {
        Ok(()) => tracing::info!("the nodes stopped; exiting"),
        Err(_) => tracing::error!(
            cap_secs = cap.as_secs(),
            "the nodes did not stop in time; exiting anyway"
        ),
    }
}
