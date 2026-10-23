//! The tray icon: Show, Start zebrad, Stop zebrad, Quit, and a tooltip with
//! the current phase.
//!
//! Platform notes: on Linux the icon goes through libayatana-appindicator,
//! which delivers no click events (only the menu works) and ignores the
//! tooltip; GNOME shows nothing without an AppIndicator extension. When the
//! library is missing the tray cannot be created at all, which
//! [`AppState::tray_ready`] records so closing the window quits instead of
//! hiding it.

use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::atomic::Ordering,
};

use launcher_core::firstrun::start_allowed;
use launcher_types::Phase;
use tauri::{
    AppHandle, Wry,
    menu::{Menu, MenuEvent, MenuItem},
    tray::{TrayIcon, TrayIconBuilder},
};
use tokio::sync::watch;

use crate::app_state::{AppState, is_stopped};

const SHOW: &str = "show";
const START: &str = "start";
const STOP: &str = "stop";
const QUIT: &str = "quit";

/// Creates the tray icon and keeps its tooltip and menu in step with the
/// supervisor. Failure is logged, not fatal.
pub fn install(app: &AppHandle, state: &AppState) {
    // libappindicator panics instead of failing when its library is missing.
    match catch_unwind(AssertUnwindSafe(|| build(app))) {
        Ok(Ok((tray, items))) => {
            state.tray_ready.store(true, Ordering::SeqCst);
            tauri::async_runtime::spawn(follow_status(
                tray,
                items,
                state.supervisor.watch_status(),
                state.settings.watch(),
            ));
        }
        Ok(Err(err)) => {
            tracing::warn!(%err, "could not create the tray icon; closing the window quits")
        }
        Err(_) => tracing::warn!(
            "could not create the tray icon (is libayatana-appindicator3 installed?); closing the window quits"
        ),
    }
}

/// The menu items whose state follows the phase.
struct Items {
    start: MenuItem<Wry>,
    stop: MenuItem<Wry>,
}

fn build(app: &AppHandle) -> tauri::Result<(TrayIcon, Items)> {
    let show = MenuItem::with_id(app, SHOW, "Show Zebra Launcher", true, None::<&str>)?;
    let start = MenuItem::with_id(app, START, "Start zebrad", true, None::<&str>)?;
    let stop = MenuItem::with_id(app, STOP, "Stop zebrad", false, None::<&str>)?;
    let quit = MenuItem::with_id(app, QUIT, "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &start, &stop, &quit])?;
    let mut builder = TrayIconBuilder::with_id("main")
        .tooltip(tooltip(&Phase::Stopped))
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(on_menu_event);
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    let tray = builder.build(app)?;
    Ok((tray, Items { start, stop }))
}

fn on_menu_event(app: &AppHandle, event: MenuEvent) {
    match event.id.as_ref() {
        SHOW => crate::show_main_window(app),
        START => {
            if let Some(state) = crate::try_state(app) {
                tauri::async_runtime::spawn(crate::start_in_background(app.clone(), state));
            }
        }
        STOP => {
            if let Some(state) = crate::try_state(app) {
                tauri::async_runtime::spawn(async move {
                    if let Err(err) = state.supervisor.stop().await {
                        tracing::warn!(%err, "could not stop zebrad from the tray");
                    }
                });
            }
        }
        // Runs the exit flow in `exit.rs`: stop zebrad, then exit.
        QUIT => app.exit(0),
        _ => {}
    }
}

/// Updates the tooltip and the Start/Stop items on every status or settings
/// change, until the supervisor or the settings store ends. Start stays
/// disabled until the first run is done.
async fn follow_status(
    tray: TrayIcon,
    items: Items,
    mut status: watch::Receiver<launcher_types::Status>,
    mut settings: watch::Receiver<launcher_types::Settings>,
) {
    loop {
        let phase = status.borrow_and_update().phase.clone();
        let may_start = start_allowed(&settings.borrow_and_update()).is_ok();
        if let Err(err) = tray.set_tooltip(Some(tooltip(&phase))) {
            tracing::debug!(%err, "could not update the tray tooltip");
        }
        let (start, stop) = menu_state(&phase, may_start);
        let _ = items.start.set_enabled(start);
        let _ = items.stop.set_enabled(stop);
        let changed = tokio::select! {
            changed = status.changed() => changed,
            changed = settings.changed() => changed,
        };
        if changed.is_err() {
            return;
        }
    }
}

/// The tooltip for `phase`.
pub fn tooltip(phase: &Phase) -> String {
    let what = match phase {
        Phase::Stopped => "zebrad stopped",
        Phase::Starting => "zebrad starting",
        Phase::Running => "zebrad running",
        Phase::Stopping => "zebrad stopping",
        Phase::Backoff { .. } => "zebrad crashed, restarting soon",
        Phase::Failed { .. } => "zebrad failed, needs attention",
    };
    format!("Zebra Launcher: {what}")
}

/// Whether Start and Stop are enabled in `phase`; Start only when
/// `may_start` ([`start_allowed`]).
pub fn menu_state(phase: &Phase, may_start: bool) -> (bool, bool) {
    let stopped = is_stopped(phase);
    (stopped && may_start, !stopped && *phase != Phase::Stopping)
}

#[cfg(test)]
mod tests {
    use launcher_types::ExitClass;

    use super::*;

    #[test]
    fn menu_follows_the_phase() {
        assert_eq!(menu_state(&Phase::Stopped, true), (true, false));
        assert_eq!(menu_state(&Phase::Stopped, false), (false, false));
        assert_eq!(
            menu_state(
                &Phase::Failed {
                    class: ExitClass::ConfigError
                },
                true
            ),
            (true, false)
        );
        assert_eq!(menu_state(&Phase::Running, true), (false, true));
        assert_eq!(menu_state(&Phase::Starting, true), (false, true));
        assert_eq!(menu_state(&Phase::Stopping, true), (false, false));
        assert_eq!(tooltip(&Phase::Running), "Zebra Launcher: zebrad running");
    }
}
