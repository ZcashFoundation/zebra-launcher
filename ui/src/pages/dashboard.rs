//! The dashboard: node details, the end-of-support banner, and the embedded Grafana dashboard.

use std::time::Duration;

use launcher_types::{ExitClass, Phase};
use leptos::prelude::*;

use crate::{
    app::AppState,
    clock,
    components::{status_strip::phase_detail, updates::CheckForUpdate},
    format::{exit_class_text, kiosk_url, percent, phase_label, relative, thousands},
};

/// Inside this window before the end of support, the EOS banner offers "Check for updates",
/// whatever the update mode.
const EOS_CHECK_WINDOW: Duration = Duration::from_secs(14 * 86_400);

/// The command that starts the Prometheus and Grafana stack, shown when no Grafana URL is set.
const OBSERVABILITY_HINT: &str = "docker compose -f docs/observability/docker-compose.yml up -d";
const GRAFANA_EXAMPLE_URL: &str = "http://127.0.0.1:3000/d/zebra-overview";

#[component]
pub fn Dashboard() -> impl IntoView {
    let state = AppState::get();

    let eos_text = move || {
        let (version, eos) = state.stats.with(|stats| {
            let stats = stats.as_ref()?;
            Some((stats.zebrad_version.clone(), stats.eos.clone()?))
        })?;
        let when = relative(eos.estimated_time, state.now.get());
        Some(format!(
            "zebrad {version} stops working at block {} (estimated {}, {when}). Update the launcher before then.",
            thousands(eos.block_height),
            clock::local_string(eos.estimated_time),
        ))
    };
    let eos_soon = Memo::new(move |_| {
        let now = state.now.get();
        state.stats.with(|stats| {
            stats
                .as_ref()
                .and_then(|stats| stats.eos.as_ref())
                .is_some_and(|eos| {
                    eos.estimated_time
                        .duration_since(now)
                        .map_or(true, |left| left < EOS_CHECK_WINDOW)
                })
        })
    });
    let halted = Memo::new(move |_| {
        state.status.with(|status| {
            status.phase
                == Phase::Failed {
                    class: ExitClass::EosHalt,
                }
        })
    });
    // Separate from the text, which changes every second, so the button keeps its state.
    let eos_banner = view! {
        <Show when=move || eos_text().is_some()>
            <div class="banner banner-warn eos-banner" role="status">
                <span>
                    <strong>"End of support approaching. "</strong>
                    {eos_text}
                </span>
                <Show when=move || eos_soon.get()>
                    <div class="buttons">
                        <CheckForUpdate />
                    </div>
                </Show>
            </div>
        </Show>
        <Show when=move || halted.get()>
            <div class="banner banner-error eos-banner" role="alert">
                <span>
                    <strong>"This zebrad release is past its end of support. "</strong>
                    "It refuses to run, and restarting cannot help. A launcher update brings a newer zebrad."
                </span>
                <div class="buttons">
                    <CheckForUpdate />
                </div>
            </div>
        </Show>
    };

    let node_rows = move || {
        let now = state.now.get();
        state.status.with(|status| {
            let mut rows = vec![(
                "Phase",
                match phase_detail(status, now) {
                    Some(detail) => format!("{} — {detail}", phase_label(&status.phase)),
                    None => phase_label(&status.phase).to_owned(),
                },
            )];
            if let Some(pid) = status.pid {
                rows.push(("PID", pid.to_string()));
            }
            if let Some(started) = status.started_at {
                rows.push(("Started", relative(started, now)));
            }
            if let Some(addr) = status.rpc_addr {
                rows.push(("RPC", addr.to_string()));
            }
            if let Some(addr) = status.metrics_addr {
                rows.push(("Metrics", addr.to_string()));
            }
            if let Some(exit) = &status.last_exit {
                let code = exit.code.map_or_else(
                    || "killed by a signal".to_owned(),
                    |code| format!("code {code}"),
                );
                rows.push((
                    "Last exit",
                    format!(
                        "{} ({code}), {}",
                        exit_class_text(exit.class),
                        relative(exit.at, now)
                    ),
                ));
                if let Some(detail) = &exit.detail {
                    rows.push(("Exit detail", detail.clone()));
                }
            }
            rows
        })
    };

    // One derived signal, so the rows update in place instead of being rebuilt every tick.
    let chain_rows = Signal::derive(move || {
        let now = state.now.get();
        state.stats.with(|stats| {
            stats.as_ref().map_or_else(Vec::new, |s| {
                vec![
                    ("Chain", s.chain.clone()),
                    ("Height", thousands(s.height)),
                    ("Estimated height", thousands(s.estimated_height)),
                    ("Synced", percent(s.verification_progress)),
                    ("Peers", s.peers.to_string()),
                    ("Mempool transactions", thousands(s.mempool_transactions)),
                    ("zebrad", s.zebrad_version.clone()),
                    ("Updated", relative(s.updated_at, now)),
                ]
            })
        })
    });

    // A memo, so saving unrelated settings does not reload the iframe.
    let grafana_url = Memo::new(move |_| {
        state
            .settings
            .with(|settings| settings.as_ref().and_then(|s| s.grafana_url.clone()))
    });
    let grafana = move || {
        match grafana_url.get() {
            Some(url) => view! {
                <iframe
                    class="grafana"
                    title="Grafana dashboard"
                    src=kiosk_url(&url)
                    sandbox="allow-scripts allow-same-origin"
                    referrerpolicy="no-referrer"
                ></iframe>
            }
            .into_any(),
            None => view! {
                <div class="card hint">
                    <h3>"Metrics dashboard"</h3>
                    <p>
                        "zebrad exports Prometheus metrics. To chart them here, start the bundled Prometheus and Grafana stack from a checkout of this repository:"
                    </p>
                    <pre>{OBSERVABILITY_HINT}</pre>
                    <p>
                        "then set the Grafana URL in Settings, for example "
                        <code>{GRAFANA_EXAMPLE_URL}</code> "."
                    </p>
                </div>
            }
            .into_any(),
        }
    };

    view! {
        <div class="dashboard">
            {eos_banner}
            <div class="cards">
                <div class="card">
                    <h3>"Node"</h3>
                    <Rows rows=Signal::derive(node_rows) />
                </div>
                <div class="card">
                    <h3>"Chain"</h3>
                    <Show
                        when=move || state.stats.with(Option::is_some)
                        fallback=|| view! { <p class="muted">"No data from zebrad's RPC yet."</p> }
                    >
                        <Rows rows=chain_rows />
                    </Show>
                </div>
            </div>
            {grafana}
        </div>
    }
}

#[component]
fn Rows(rows: Signal<Vec<(&'static str, String)>>) -> impl IntoView {
    view! {
        <dl class="rows">
            {move || {
                rows.get()
                    .into_iter()
                    .map(|(label, value)| view! {
                        <dt>{label}</dt>
                        <dd>{value}</dd>
                    })
                    .collect_view()
            }}
        </dl>
    }
}
