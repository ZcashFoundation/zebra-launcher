//! The tray icon: Show, Start and Stop, Quit, and a tooltip with what the
//! nodes are doing. With one node they read as they always have ("Start
//! zebrad", "Zeal: zebrad running"); with several, Start all and Stop all act
//! on every node and the tooltip counts them ("Zeal: 2 running, 1 stopped").
//! Submenus per node come later.
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

use launcher_core::{firstrun::start_allowed, registry::NodeSummary};
use launcher_types::Phase;
use tauri::{
    AppHandle, Wry,
    menu::{Menu, MenuEvent, MenuItem},
    tray::{TrayIcon, TrayIconBuilder},
};
use tokio::sync::watch;

use crate::app_state::AppState;

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
            tauri::async_runtime::spawn(follow_nodes(
                tray,
                items,
                state.registry.watch_summaries(),
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

/// The menu items whose state follows the nodes' phases.
struct Items {
    start: MenuItem<Wry>,
    stop: MenuItem<Wry>,
}

fn build(app: &AppHandle) -> tauri::Result<(TrayIcon, Items)> {
    let show = MenuItem::with_id(app, SHOW, "Show Zeal", true, None::<&str>)?;
    let start = MenuItem::with_id(app, START, "Start zebrad", true, None::<&str>)?;
    let stop = MenuItem::with_id(app, STOP, "Stop zebrad", false, None::<&str>)?;
    let quit = MenuItem::with_id(app, QUIT, "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &start, &stop, &quit])?;
    let mut builder = TrayIconBuilder::with_id("main")
        .tooltip(tooltip(&[Phase::Stopped]))
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
        // Every node that is stopped or failed starts; the others are left.
        START => {
            if let Some(state) = crate::try_state(app) {
                for node in state.registry.nodes().iter() {
                    if node.supervisor.status().phase.is_settled() {
                        tauri::async_runtime::spawn(crate::start_in_background(
                            app.clone(),
                            state.clone(),
                            node.clone(),
                        ));
                    }
                }
            }
        }
        STOP => {
            if let Some(state) = crate::try_state(app) {
                tauri::async_runtime::spawn(async move {
                    if let Err(err) = state.registry.stop_all().await {
                        tracing::warn!(%err, "could not stop the nodes from the tray");
                    }
                });
            }
        }
        // Runs the exit flow in `exit.rs`: stop zebrad, then exit.
        QUIT => app.exit(0),
        _ => {}
    }
}

/// Updates the tooltip and the Start/Stop items on every change of a node's
/// phase, of the node list or of the settings, until the registry or the
/// settings store ends. Start stays disabled until the first run is done.
async fn follow_nodes(
    tray: TrayIcon,
    items: Items,
    mut summaries: watch::Receiver<Vec<NodeSummary>>,
    mut settings: watch::Receiver<launcher_types::Settings>,
) {
    loop {
        let phases: Vec<Phase> = summaries
            .borrow_and_update()
            .iter()
            .map(|summary| summary.phase.clone())
            .collect();
        let may_start = start_allowed(&settings.borrow_and_update()).is_ok();
        if let Err(err) = tray.set_tooltip(Some(tooltip(&phases))) {
            tracing::debug!(%err, "could not update the tray tooltip");
        }
        let (start, stop) = menu_state(&phases, may_start);
        let (start_text, stop_text) = menu_text(phases.len());
        let _ = items.start.set_enabled(start);
        let _ = items.stop.set_enabled(stop);
        let _ = items.start.set_text(start_text);
        let _ = items.stop.set_text(stop_text);
        let changed = tokio::select! {
            changed = summaries.changed() => changed,
            changed = settings.changed() => changed,
        };
        if changed.is_err() {
            return;
        }
    }
}

/// The tooltip for the nodes' `phases`: one node's phase in words, as it
/// always read, or how many nodes run (or will, [`Phase::is_active`]) and
/// how many do not.
pub fn tooltip(phases: &[Phase]) -> String {
    let [phase] = phases else {
        let running = phases.iter().filter(|phase| phase.is_active()).count();
        let stopped = phases.len() - running;
        return format!("Zeal: {running} running, {stopped} stopped");
    };
    let what = match phase {
        Phase::Stopped => "zebrad stopped",
        Phase::Starting => "zebrad starting",
        Phase::Running => "zebrad running",
        Phase::Stopping => "zebrad stopping",
        Phase::Backoff { .. } => "zebrad crashed, restarting soon",
        Phase::Failed { .. } => "zebrad failed, needs attention",
    };
    format!("Zeal: {what}")
}

/// Whether Start and Stop are enabled for the nodes' `phases`: Start while a
/// node is settled, and only when `may_start` ([`start_allowed`]); Stop
/// while one is active. With one node, neither while it is stopping.
pub fn menu_state(phases: &[Phase], may_start: bool) -> (bool, bool) {
    (
        may_start && phases.iter().any(Phase::is_settled),
        phases.iter().any(Phase::is_active),
    )
}

/// The Start and Stop items' words for `count` nodes.
pub fn menu_text(count: usize) -> (&'static str, &'static str) {
    if count == 1 {
        ("Start zebrad", "Stop zebrad")
    } else {
        ("Start all", "Stop all")
    }
}

#[cfg(test)]
mod tests {
    use launcher_types::ExitClass;

    use super::*;

    #[test]
    fn one_node_reads_as_it_always_has() {
        let failed = Phase::Failed {
            class: ExitClass::ConfigError,
        };
        assert_eq!(menu_state(&[Phase::Stopped], true), (true, false));
        assert_eq!(menu_state(&[Phase::Stopped], false), (false, false));
        assert_eq!(
            menu_state(std::slice::from_ref(&failed), true),
            (true, false)
        );
        assert_eq!(menu_state(&[Phase::Running], true), (false, true));
        assert_eq!(menu_state(&[Phase::Starting], true), (false, true));
        assert_eq!(menu_state(&[Phase::Stopping], true), (false, false));
        assert_eq!(tooltip(&[Phase::Running]), "Zeal: zebrad running");
        assert_eq!(menu_text(1), ("Start zebrad", "Stop zebrad"));
    }

    #[test]
    fn several_nodes_are_counted_and_started_or_stopped_together() {
        let backoff = Phase::Backoff {
            until: std::time::SystemTime::UNIX_EPOCH,
            crashes: 1,
        };
        let phases = [Phase::Running, backoff, Phase::Stopped];
        assert_eq!(tooltip(&phases), "Zeal: 2 running, 1 stopped");
        assert_eq!(
            tooltip(&[Phase::Stopped, Phase::Stopped]),
            "Zeal: 0 running, 2 stopped"
        );
        // Start all while one is stopped, Stop all while one runs.
        assert_eq!(menu_state(&phases, true), (true, true));
        assert_eq!(menu_state(&phases, false), (false, true));
        assert_eq!(
            menu_state(&[Phase::Running, Phase::Running], true),
            (false, true)
        );
        assert_eq!(menu_text(3), ("Start all", "Stop all"));
    }
}
