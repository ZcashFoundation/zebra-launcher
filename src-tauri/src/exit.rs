//! Quitting stops zebrad first.
//!
//! On [`RunEvent::ExitRequested`] the exit is prevented, the supervisor is
//! shut down (graceful stop, then kill after the grace period) with an overall
//! cap, and the app exits again with the requested code. The second request
//! passes through because the guard records that the shutdown finished.
//! [`RunEvent::Exit`] is the last resort when the event loop ends without a
//! request we could prevent: it blocks until the shutdown finishes or the cap
//! expires.
//!
//! Closing the main window hides it instead when `close_to_tray` is on and
//! the tray icon exists, so zebrad keeps running; without a tray (Linux
//! without libappindicator) closing the window quits as above. The tray's
//! Show item, a second launch and the macOS dock icon show it again.

use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use launcher_core::supervisor;
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
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                shutdown(&state).await;
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
                tracing::info!("window hidden to the tray; zebrad keeps running");
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
            tracing::warn!("exiting without a prior exit request; stopping zebrad");
            // If a shutdown is already running, this waits for the actor to end.
            tauri::async_runtime::block_on(shutdown(state.inner()));
            state.exit.finished.store(true, Ordering::SeqCst);
        }
        _ => {}
    }
}

/// Stops zebrad and the supervisor, giving up after the cap.
async fn shutdown(state: &AppState) {
    let grace = Duration::from_secs(state.settings.current().stop_grace_secs);
    let cap = supervisor::shutdown_cap(grace);
    tracing::info!(cap_secs = cap.as_secs(), "stopping zebrad before exit");
    match tokio::time::timeout(cap, state.supervisor.shutdown()).await {
        // `Closed` means an earlier shutdown already ended the actor.
        Ok(_) => tracing::info!("zebrad stopped; exiting"),
        Err(_) => tracing::error!(
            cap_secs = cap.as_secs(),
            "zebrad did not stop in time; exiting anyway"
        ),
    }
}
