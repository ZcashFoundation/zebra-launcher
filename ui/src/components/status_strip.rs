//! The strip under the tabs: zebrad's phase, chain progress and the Start/Stop/Restart buttons.

use std::time::SystemTime;

use launcher_types::{NodeKind, NodeStats, Phase, Status};
use leptos::{prelude::*, task::spawn_local};

use crate::{
    app::AppState,
    format::{
        self, controls, exit_class_text, is_synced, percent, phase_class, phase_label,
        sync_percent, thousands,
    },
    pages::docs::DocsSection,
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
        .node()
        .stats
        .with(|stats| stats.as_ref().and_then(|stats| stats.last_error.clone()));
    let from_exit = state.node().status.with(|status| match status.phase {
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
                Command::Start => state.node_untracked().start().await,
                Command::Stop => state.node_untracked().stop().await,
                Command::Restart => state.node_untracked().restart().await,
            };
            if let Err(error) = result {
                let verb = command.verb();
                command_error.set(Some(format!("Could not {verb} zebrad: {error}")));
            }
            busy.set(false);
        });
    };
    let can = move || state.node().status.with(|status| controls(&status.phase));

    let stat = move |pick: fn(&NodeStats) -> String| {
        move || {
            state
                .node()
                .stats
                .with(|stats| stats.as_ref().map(pick))
                .unwrap_or_else(|| "—".to_owned())
        }
    };

    view! {
        <div class="status-strip">
            <span class=move || state.node().status.with(|s| phase_class(&s.phase))>
                {move || state.node().status.with(|s| phase_label(&s.phase))}
            </span>
            {move || {
                state
                    .node()
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
                                .node()
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
                    <dt>
                        {move || {
                            state
                                .node()
                                .info
                                .with(|info| info.kind.program().unwrap_or("version"))
                        }}
                    </dt>
                    <dd>
                        {move || {
                            let node = state.node();
                            let zebra = node.info.with(|info| info.kind == NodeKind::Zebrad);
                            let version = node.stats.with(|stats| {
                                stats.as_ref().map(|stats| stats.zebrad_version.clone())
                            });
                            match version {
                                Some(version) if zebra => {
                                    let label = format!("Read zebrad {version} release notes");
                                    view! {
                                        <button
                                            type="button"
                                            class="link docs-version-link"
                                            aria-label=label
                                            title="Read this release's notes"
                                            on:click=move |_| state.open_docs(DocsSection::Changes)
                                        >
                                            {version}
                                        </button>
                                    }
                                    .into_any()
                                }
                                Some(version) => view! { {version} }.into_any(),
                                None => view! { "—" }.into_any(),
                            }
                        }}
                    </dd>
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
            {move || {
                state
                    .node()
                    .stats
                    .with(|stats| stats.as_ref().map(|stats| stats.verification_progress))
                    .map(|progress| {
                        let percent = sync_percent(progress);
                        view! {
                            <div
                                class="sync-bar"
                                role="progressbar"
                                aria-label="Chain sync progress"
                                aria-valuemin="0"
                                aria-valuemax="100"
                                aria-valuenow=format!("{percent:.0}")
                            >
                                <div
                                    class="sync-bar-fill"
                                    class:synced=is_synced(progress)
                                    style:width=format!("{percent:.2}%")
                                ></div>
                            </div>
                        }
                    })
            }}
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
