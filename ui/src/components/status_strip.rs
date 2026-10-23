//! The strip under the tabs: zebrad's phase, chain progress and the Start/Stop/Restart buttons.

use std::time::SystemTime;

use launcher_types::{Phase, Status};
use leptos::{prelude::*, task::spawn_local};

use crate::{
    api,
    app::AppState,
    format::{self, controls, exit_class_text, percent, phase_class, phase_label, thousands},
};

/// Extra words for the badge: the restart countdown or the failure reason.
pub fn phase_detail(status: &Status, now: SystemTime) -> Option<String> {
    match &status.phase {
        Phase::Backoff { until, crashes } => Some(format!(
            "{} (crash {crashes})",
            match until.duration_since(now) {
                Ok(left) => format!("in {}", format::rough_duration(left)),
                Err(_) => "now".to_owned(),
            }
        )),
        Phase::Failed { class } => Some(exit_class_text(*class).to_owned()),
        _ => None,
    }
}

/// The error to show in the strip: zebrad's last logged error, or why it failed.
pub fn last_error(state: &AppState) -> Option<String> {
    let from_stats = state
        .stats
        .with(|stats| stats.as_ref().and_then(|stats| stats.last_error.clone()));
    let from_exit = state.status.with(|status| match status.phase {
        Phase::Failed { .. } | Phase::Backoff { .. } => status
            .last_exit
            .as_ref()
            .and_then(|exit| exit.detail.clone()),
        _ => None,
    });
    from_exit.or(from_stats)
}

#[derive(Clone, Copy)]
enum Command {
    Start,
    Stop,
    Restart,
}

impl Command {
    fn verb(self) -> &'static str {
        match self {
            Command::Start => "start",
            Command::Stop => "stop",
            Command::Restart => "restart",
        }
    }
}

#[component]
pub fn StatusStrip() -> impl IntoView {
    let state = AppState::get();
    let busy = RwSignal::new(false);
    let command_error = RwSignal::new(None::<String>);

    let run = move |command: Command| {
        busy.set(true);
        command_error.set(None);
        spawn_local(async move {
            let result = match command {
                Command::Start => api::start_zebrad().await,
                Command::Stop => api::stop_zebrad().await,
                Command::Restart => api::restart_zebrad().await,
            };
            if let Err(error) = result {
                let verb = command.verb();
                command_error.set(Some(format!("Could not {verb} zebrad: {error}")));
            }
            busy.set(false);
        });
    };
    let can = move || state.status.with(|status| controls(&status.phase));

    let stat = move |pick: fn(&crate::types::NodeStats) -> String| {
        move || {
            state
                .stats
                .with(|stats| stats.as_ref().map(pick))
                .unwrap_or_else(|| "—".to_owned())
        }
    };

    view! {
        <div class="status-strip">
            <span class=move || state.status.with(|s| phase_class(&s.phase))>
                {move || state.status.with(|s| phase_label(&s.phase))}
            </span>
            {move || {
                state
                    .status
                    .with(|status| phase_detail(status, state.now.get()))
                    .map(|detail| view! { <span class="phase-detail">{detail}</span> })
            }}
            <dl class="stats">
                <div>
                    <dt>"PID"</dt>
                    <dd>
                        {move || {
                            state
                                .status
                                .with(|s| s.pid.map(|pid| pid.to_string()))
                                .unwrap_or_else(|| "—".into())
                        }}
                    </dd>
                </div>
                <div>
                    <dt>"Height"</dt>
                    <dd>
                        {stat(|s| {
                            format!("{} / {}", thousands(s.height), thousands(s.estimated_height))
                        })}
                    </dd>
                </div>
                <div>
                    <dt>"Synced"</dt>
                    <dd>{stat(|s| percent(s.verification_progress))}</dd>
                </div>
                <div>
                    <dt>"Peers"</dt>
                    <dd>{stat(|s| s.peers.to_string())}</dd>
                </div>
                <div>
                    <dt>"Mempool"</dt>
                    <dd>{stat(|s| thousands(s.mempool_transactions))}</dd>
                </div>
                <div>
                    <dt>"zebrad"</dt>
                    <dd>{stat(|s| s.zebrad_version.clone())}</dd>
                </div>
            </dl>
            <div class="controls">
                <button
                    disabled=move || busy.get() || !can().start
                    on:click=move |_| run(Command::Start)
                >
                    "Start"
                </button>
                <button disabled=move || busy.get() || !can().stop on:click=move |_| run(Command::Stop)>
                    "Stop"
                </button>
                <button
                    disabled=move || busy.get() || !can().restart
                    on:click=move |_| run(Command::Restart)
                >
                    "Restart"
                </button>
            </div>
        </div>
        {move || {
            command_error
                .get()
                .or_else(|| last_error(&state))
                .map(|error| {
                    let title = error.clone();
                    view! {
                        <div class="last-error" title=title>
                            <span class="label">"Last error"</span>
                            {error}
                        </div>
                    }
                })
        }}
    }
}
