//! The light client server's screens (docs/PUBLIC-SERVICES.md): Settings → Public services
//! with the switch, the guided setup and its consent screen, and the status panel; the card on
//! the node's dashboard; and the header banner for automatic pauses and certificate problems.
//! Every sentence comes from [`crate::public_text`].

use launcher_herd::public::{current_step, names::DUCKDNS_ZONE};
use launcher_types::{
    Network,
    public::{
        CONSENT_VERSION, CheckOutcome, ConnectionReport, DnsProvider, PublicStatus, StepAnswer,
        StepId, StepState, UploadPreset,
    },
};
use leptos::{prelude::*, task::spawn_local};

use crate::{
    api,
    app::{AppState, Tab},
    clipboard::copy_to_clipboard,
    public_state::PublicContext,
    public_text::{
        self, SERVICE_NAME, Tone, cert_text, check_line, consent_lines, finding_text, network_name,
        reach_text, state_line, step_mark, step_title, use_lines,
    },
};

/// The light client server's status, if one is set up, tracked.
fn light_status() -> Memo<Option<PublicStatus>> {
    let public = PublicContext::get();
    Memo::new(move |_| public.statuses.with(|statuses| statuses.first().cloned()))
}

fn tone_class(tone: Tone) -> &'static str {
    match tone {
        Tone::Ok => "public-line tone-ok",
        Tone::Neutral => "public-line",
        Tone::Warn => "public-line tone-warn",
        Tone::Error => "public-line tone-error",
    }
}

/// The network's pill; Testnet in the warning tone, as everywhere.
fn network_pill(network: Network) -> impl IntoView {
    let class = match network {
        Network::Mainnet => "network-pill",
        Network::Testnet => "network-pill network-testnet",
    };
    view! { <span class=class>{network_name(network)}</span> }
}

/// Runs a command, showing its error in `note` and refreshing the status after.
fn act<F>(busy: RwSignal<bool>, note: RwSignal<Option<String>>, command: F)
where
    F: Future<Output = Result<(), String>> + 'static,
{
    busy.set(true);
    note.set(None);
    spawn_local(async move {
        if let Err(error) = command.await {
            note.set(Some(error));
        }
        busy.set(false);
    });
}

/// The switch: a `<button role="switch">` with a visible label.
#[component]
fn Switch(
    #[prop(into)] on: Signal<bool>,
    #[prop(into)] disabled: Signal<bool>,
    on_toggle: Callback<bool>,
) -> impl IntoView {
    view! {
        <button
            type="button"
            role="switch"
            class="public-switch"
            class:on=move || on.get()
            aria-checked=move || if on.get() { "true" } else { "false" }
            aria-label="Light client server"
            disabled=move || disabled.get()
            on:click=move |_| on_toggle.run(!on.get_untracked())
        >
            <span class="public-switch-knob" aria-hidden="true"></span>
            <span>{move || if on.get() { "On" } else { "Off" }}</span>
        </button>
    }
}

/// Settings → Public services.
#[component]
pub fn PublicServices() -> impl IntoView {
    let status = light_status();
    let has_service = Memo::new(move |_| status.with(Option::is_some));
    view! {
        <section class="settings public" aria-labelledby="public-heading">
            <h2 id="public-heading">"Public services"</h2>
            <p class="muted">
                "Serve light wallets from this computer: phones reach Zebra through Zeal's front \
                 door, with a certificate from Let's Encrypt. Off until you set it up, and \
                 nothing becomes public before the last step."
            </p>
            {move || {
                if has_service.get() {
                    view! { <LightServer status /> }.into_any()
                } else {
                    view! { <NotSetUp /> }.into_any()
                }
            }}
        </section>
    }
}

/// Before a setup: the switch, and "Check my connection".
#[component]
fn NotSetUp() -> impl IntoView {
    let state = AppState::get();
    let public = PublicContext::get();
    let busy = RwSignal::new(false);
    let note = RwSignal::new(None::<String>);
    let report = RwSignal::new(None::<ConnectionReport>);
    let begin = Callback::new(move |on: bool| {
        if !on {
            return;
        }
        let network = state.network.get_untracked();
        act(busy, note, async move {
            api::public::begin_public_setup(network)
                .await
                .map(|_| public.refresh())
                .map_err(|error| error.to_string())
        });
    });
    let check = move |_| {
        busy.set(true);
        note.set(None);
        spawn_local(async move {
            match api::public::check_connection().await {
                Ok(found) => report.set(Some(found)),
                Err(error) => note.set(Some(error.to_string())),
            }
            busy.set(false);
        });
    };
    view! {
        <div class="card public-card">
            <div class="public-head">
                <h3>{SERVICE_NAME}</h3>
                {move || network_pill(state.network.get())}
                <Switch on=Signal::stored(false) disabled=busy on_toggle=begin />
            </div>
            <p class="muted">
                "Zebra's own light wallet server, behind Zeal's TLS front door. It needs a name \
                 (a free DuckDNS name works), a forward of port 443 in your router and a public \
                 IPv4 address. Switching it on starts a guided setup that checks every step."
            </p>
            <div class="buttons">
                <button type="button" disabled=busy on:click=check>
                    "Check my connection"
                </button>
            </div>
            {move || note.get().map(|note| view! { <p class="error">{note}</p> })}
            {move || report.get().map(|report| view! { <ConnectionResult report /> })}
        </div>
    }
}

/// What "Check my connection" found.
#[component]
fn ConnectionResult(report: ConnectionReport) -> impl IntoView {
    let lan = report
        .net
        .lan_addr
        .map_or_else(|| "unknown".to_owned(), |lan| lan.to_string());
    let wan = report
        .net
        .wan_addr
        .map_or_else(|| "not reported".to_owned(), |wan| wan.to_string());
    view! {
        <dl class="rows">
            <dt>"This computer"</dt>
            <dd>{lan}</dd>
            <dt>"Router's public address"</dt>
            <dd>{wan}</dd>
        </dl>
        <ul class="public-checks">
            {report
                .checks
                .into_iter()
                .map(|check| {
                    let class = outcome_class(&check.outcome);
                    view! { <li class=class>{check_line(check.id, &check.outcome)}</li> }
                })
                .collect_view()}
        </ul>
    }
}

fn outcome_class(outcome: &CheckOutcome) -> &'static str {
    match outcome {
        CheckOutcome::Pass => "check-pass",
        CheckOutcome::Warn { .. } => "check-warn",
        CheckOutcome::Fail { .. } => "check-fail",
        CheckOutcome::NotRun | CheckOutcome::Running => "check-waiting",
    }
}

/// A set-up light client server: the header, the state line, then the setup or the panel.
#[component]
fn LightServer(status: Memo<Option<PublicStatus>>) -> impl IntoView {
    let state = AppState::get();
    let busy = RwSignal::new(false);
    let note = RwSignal::new(None::<String>);
    let read =
        move |f: fn(&PublicStatus) -> bool| move || status.with(|s| s.as_ref().is_some_and(f));
    let opened = Memo::new(move |_| status.with(|s| s.as_ref().is_some_and(|s| s.opened)));
    let network =
        Memo::new(move |_| status.with(|s| s.as_ref().map_or(Network::Mainnet, |s| s.network)));
    let in_scope = move || state.network.get() == network.get();
    let toggle = Callback::new(move |on: bool| {
        act(busy, note, async move {
            api::public::set_public_switch(on)
                .await
                .map_err(|error| error.to_string())
        });
    });
    view! {
        <div class="card public-card">
            <div class="public-head">
                <h3>{SERVICE_NAME}</h3>
                {move || network_pill(network.get())}
                <Switch on=Signal::derive(read(|s| s.switch_on)) disabled=busy on_toggle=toggle />
            </div>
            {move || {
                status
                    .get()
                    .map(|status| {
                        let (tone, line) = state_line(&status, AppState::get().now.get());
                        view! { <p class=tone_class(tone) role="status">{line}</p> }
                    })
            }}
            {move || note.get().map(|note| view! { <p class="error">{note}</p> })}
            <Show
                when=in_scope
                fallback=move || {
                    view! {
                        <p class="muted">
                            {move || format!(
                                "This light client server serves {}. Switch to that network to \
                                 see it.",
                                network_name(network.get()),
                            )}
                        </p>
                    }
                }
            >
                {move || {
                    if opened.get() {
                        view! { <Serving status /> }.into_any()
                    } else {
                        view! { <Checklist status /> }.into_any()
                    }
                }}
                <Forget />
            </Show>
        </div>
    }
}

/// The guided setup: one line per step, the current one with what it needs.
#[component]
fn Checklist(status: Memo<Option<PublicStatus>>) -> impl IntoView {
    let current =
        Memo::new(move |_| status.with(|s| s.as_ref().and_then(|s| current_step(&s.steps))));
    let steps = move || status.with(|s| s.as_ref().map(|s| s.steps.clone()).unwrap_or_default());
    view! {
        <ol class="public-steps">
            {move || {
                steps()
                    .into_iter()
                    .map(|step| {
                        let id = step.id;
                        let mark = step_mark(step.state);
                        let class = match step.state {
                            StepState::Done => "public-step done",
                            StepState::Failed => "public-step failed",
                            StepState::Waiting => "public-step waiting",
                            StepState::Running | StepState::NeedsYou => "public-step current",
                        };
                        let title = status.with(|s| {
                            s.as_ref().map(|s| step_title(id, s)).unwrap_or_default()
                        });
                        let problems: Vec<String> = step
                            .checks
                            .iter()
                            .filter_map(|check| match &check.outcome {
                                CheckOutcome::Fail { finding } | CheckOutcome::Warn { finding } => {
                                    Some(finding_text(finding))
                                }
                                _ => None,
                            })
                            .collect();
                        let is_current = current.get_untracked() == Some(id);
                        view! {
                            <li class=class aria-current=is_current.then_some("step")>
                                <span class="public-mark" aria-hidden="true">{mark}</span>
                                <span class="public-step-title">{title}</span>
                                // Waiting steps keep their findings for later; a done step
                                // can still carry a warning, such as Zebra still syncing.
                                {(step.state != StepState::Waiting && !problems.is_empty())
                                    .then(|| {
                                        view! {
                                            <ul class="public-findings">
                                                {problems
                                                    .into_iter()
                                                    .map(|text| view! { <li>{text}</li> })
                                                    .collect_view()}
                                            </ul>
                                        }
                                    })}
                            </li>
                        }
                    })
                    .collect_view()
            }}
        </ol>
        {move || current.get().map(|step| view! { <StepBody step status /> })}
    }
}

/// What the current step needs from the user.
#[component]
fn StepBody(step: StepId, status: Memo<Option<PublicStatus>>) -> impl IntoView {
    let busy = RwSignal::new(false);
    let note = RwSignal::new(None::<String>);
    let answer = move |answer: StepAnswer| {
        act(busy, note, async move {
            api::public::answer_public_step(answer)
                .await
                .map_err(|error| error.to_string())
        });
    };
    let recheck = move |_| {
        act(busy, note, async move {
            api::public::run_public_checks(step)
                .await
                .map_err(|error| error.to_string())
        });
    };
    let body = match step {
        StepId::Consent => view! { <ConsentScreen status busy on_agree=Callback::new(move |()| answer(StepAnswer::Consent { version: CONSENT_VERSION })) /> }.into_any(),
        StepId::Network => view! {
            <div class="buttons">
                <button type="button" class="primary" disabled=busy on:click=move |_| answer(StepAnswer::LanReserved)>
                    "I've reserved the address"
                </button>
                <button type="button" disabled=busy on:click=recheck>"Check again"</button>
            </div>
        }
        .into_any(),
        StepId::Name => view! { <NameForm status busy note /> }.into_any(),
        StepId::Port => {
            let (lan, gateway) = status.with_untracked(|s| {
                let net = s.as_ref().and_then(|s| s.net.clone());
                (
                    s.as_ref()
                        .and_then(|s| s.lan_addr)
                        .or_else(|| net.as_ref().and_then(|net| net.lan_addr))
                        .map_or_else(|| "this computer".to_owned(), |lan| lan.to_string()),
                    net.and_then(|net| net.gateway)
                        .map_or_else(|| "your router".to_owned(), |gateway| gateway.to_string()),
                )
            });
            let (external, internal) = status.with_untracked(|s| {
                s.as_ref().map_or((443, 8443), |s| (s.external_port, s.internal_port))
            });
            view! {
                <ol class="public-howto">
                    <li>{format!("Open your router's settings, often at http://{gateway}.")}</li>
                    <li>"Find Port forwarding (also called Virtual servers or NAT forwarding)."</li>
                    <li>{format!("Add: TCP, external port {external}, to {lan}, port {internal}.")}</li>
                    <li>"Save. Leave UPnP as it is: Zeal doesn't need it."</li>
                </ol>
                <div class="buttons">
                    <button type="button" class="primary" disabled=busy on:click=move |_| answer(StepAnswer::ForwardedByHand)>
                        "I've done it: check"
                    </button>
                </div>
            }
            .into_any()
        }
        StepId::Firewall => {
            let commands = status.with_untracked(|s| {
                s.as_ref()
                    .and_then(|s| s.firewall.clone())
                    .map(|firewall| firewall.allow)
                    .unwrap_or_default()
            });
            view! {
                <div class="public-commands">
                    {commands
                        .into_iter()
                        .map(|command| view! { <CopyLine text=command /> })
                        .collect_view()}
                </div>
                <div class="buttons">
                    <button type="button" class="primary" disabled=busy on:click=move |_| answer(StepAnswer::FirewallDone)>
                        "Done"
                    </button>
                </div>
            }
            .into_any()
        }
        StepId::Open => view! { <OpenButton /> }.into_any(),
        StepId::Service | StepId::Certificate | StepId::LocalTest | StepId::KeepRunning
        | StepId::Share => view! {
            <div class="buttons">
                <button type="button" disabled=busy on:click=recheck>"Check again"</button>
            </div>
        }
        .into_any(),
    };
    view! {
        <div class="public-step-body">
            {body}
            {move || note.get().map(|note| view! { <p class="error">{note}</p> })}
        </div>
    }
}

/// What becomes public, for good, and the tick that agrees to it.
#[component]
fn ConsentScreen(
    status: Memo<Option<PublicStatus>>,
    busy: RwSignal<bool>,
    on_agree: Callback<()>,
) -> impl IntoView {
    let ticked = RwSignal::new(false);
    let lines = move || status.with(|s| s.as_ref().map(consent_lines).unwrap_or_default());
    view! {
        <div class="public-consent">
            <p><strong>"Serving wallets from this computer makes these public, for good:"</strong></p>
            <ul>{move || lines().0.into_iter().map(|line| view! { <li>{line}</li> }).collect_view()}</ul>
            <p><strong>"What you take on:"</strong></p>
            <ul>{move || lines().1.into_iter().map(|line| view! { <li>{line}</li> }).collect_view()}</ul>
            <label class="check">
                <input
                    type="checkbox"
                    prop:checked=move || ticked.get()
                    on:change=move |event| ticked.set(event_target_checked(&event))
                />
                "I understand what becomes public"
            </label>
            <div class="buttons">
                <button
                    type="button"
                    class="primary"
                    disabled=move || busy.get() || !ticked.get()
                    on:click=move |_| on_agree.run(())
                >
                    "Agree and continue"
                </button>
            </div>
        </div>
    }
}

/// The name step: who keeps the name, the name, and DuckDNS's token.
#[component]
fn NameForm(
    status: Memo<Option<PublicStatus>>,
    busy: RwSignal<bool>,
    note: RwSignal<Option<String>>,
) -> impl IntoView {
    let initial = status.with_untracked(|s| {
        s.as_ref()
            .map_or((DnsProvider::DuckDns, String::new()), |s| {
                (
                    s.provider,
                    s.name.as_ref().map(ToString::to_string).unwrap_or_default(),
                )
            })
    });
    let provider = RwSignal::new(initial.0);
    let name = RwSignal::new(initial.1);
    let token = RwSignal::new(String::new());
    let saved = Memo::new(move |_| status.with(|s| s.as_ref().and_then(|s| s.name.clone())));
    let has_token = Memo::new(move |_| {
        status.with(|s| {
            s.as_ref()
                .and_then(|s| s.dns.as_ref())
                .is_some_and(|dns| dns.has_token)
        })
    });
    let save_name = move |_| {
        let (name, provider) = (name.get_untracked(), provider.get_untracked());
        act(busy, note, async move {
            api::public::answer_public_step(StepAnswer::Name { name, provider })
                .await
                .map_err(|error| error.to_string())
        });
    };
    let save_token = move |_| {
        let value = token.get_untracked();
        act(busy, note, async move {
            let result = api::public::answer_public_step(StepAnswer::Token { token: value })
                .await
                .map_err(|error| error.to_string());
            if result.is_ok() {
                token.set(String::new());
            }
            result
        });
    };
    let sign_up = move |_| {
        spawn_local(async move {
            let _ = api::public::open_signup(DnsProvider::DuckDns).await;
        });
    };
    view! {
        <div class="public-name">
            <fieldset>
                <legend>"Who keeps the name"</legend>
                <label class="check">
                    <input
                        type="radio"
                        name="public-provider"
                        prop:checked=move || provider.get() == DnsProvider::DuckDns
                        on:change=move |_| provider.set(DnsProvider::DuckDns)
                    />
                    {format!("A free name under {DUCKDNS_ZONE}: Zeal keeps its address current")}
                </label>
                <label class="check">
                    <input
                        type="radio"
                        name="public-provider"
                        prop:checked=move || provider.get() == DnsProvider::Manual
                        on:change=move |_| provider.set(DnsProvider::Manual)
                    />
                    "A name I keep pointing here myself"
                </label>
            </fieldset>
            <label class="field">
                <span>"Name"</span>
                <input
                    type="text"
                    autocomplete="off"
                    spellcheck="false"
                    placeholder=move || {
                        if provider.get() == DnsProvider::DuckDns { "zec-alice" } else { "zec.example.com" }
                    }
                    prop:value=move || name.get()
                    on:input=move |event| name.set(event_target_value(&event))
                />
                <span class="muted">"Pick a neutral name: it shows in public certificate logs."</span>
            </label>
            <div class="buttons">
                <button type="button" class="primary" disabled=busy on:click=save_name>"Save name"</button>
                <Show when=move || provider.get() == DnsProvider::DuckDns>
                    <button type="button" on:click=sign_up>"Get a DuckDNS name and token"</button>
                </Show>
            </div>
            <Show when=move || {
                saved.get().is_some() && provider.get() == DnsProvider::DuckDns && !has_token.get()
            }>
                <label class="field">
                    <span>"DuckDNS token"</span>
                    <input
                        type="password"
                        autocomplete="off"
                        prop:value=move || token.get()
                        on:input=move |event| token.set(event_target_value(&event))
                    />
                    <span class="muted">"Kept in this computer's keychain, never in Zeal's files."</span>
                </label>
                <div class="buttons">
                    <button type="button" class="primary" disabled=busy on:click=save_token>"Save token"</button>
                </div>
            </Show>
        </div>
    }
}

/// "Open to the internet", the only button that makes the service public.
#[component]
fn OpenButton() -> impl IntoView {
    let busy = RwSignal::new(false);
    let note = RwSignal::new(None::<String>);
    let open = move |_| {
        busy.set(true);
        note.set(None);
        spawn_local(async move {
            if let Err(error) = api::public::open_public_service().await {
                let text = match error.check {
                    Some(check) => format!(
                        "Not yet: {}.",
                        public_text::check_name(check).to_lowercase()
                    ),
                    None => error.message,
                };
                note.set(Some(text));
            }
            busy.set(false);
        });
    };
    view! {
        <div class="buttons">
            <button type="button" class="primary" disabled=busy on:click=open>
                "Open to the internet"
            </button>
        </div>
        {move || note.get().map(|note| view! { <p class="error">{note}</p> })}
    }
}

/// A command or address to copy.
#[component]
fn CopyLine(text: String) -> impl IntoView {
    let copied = RwSignal::new(false);
    let value = text.clone();
    let copy = move |_| {
        let value = value.clone();
        spawn_local(async move {
            copied.set(copy_to_clipboard(&value).await.is_ok());
        });
    };
    view! {
        <div class="public-copy">
            <code>{text}</code>
            <button type="button" on:click=copy>{move || if copied.get() { "Copied" } else { "Copy" }}</button>
        </div>
    }
}

/// The status panel of an open service.
#[component]
fn Serving(status: Memo<Option<PublicStatus>>) -> impl IntoView {
    let state = AppState::get();
    let busy = RwSignal::new(false);
    let note = RwSignal::new(None::<String>);
    let with = move |f: fn(&PublicStatus, std::time::SystemTime) -> String| {
        move || {
            status.with(|s| {
                s.as_ref()
                    .map(|s| f(s, state.now.get()))
                    .unwrap_or_default()
            })
        }
    };
    let answer = move |answer: StepAnswer| {
        act(busy, note, async move {
            api::public::answer_public_step(answer)
                .await
                .map_err(|error| error.to_string())
        });
    };
    let share = move || status.with(|s| s.as_ref().and_then(|s| s.share.clone()));
    let checks = move || {
        status.with(|s| {
            let Some(s) = s else { return String::new() };
            let all: Vec<_> = s.steps.iter().flat_map(|step| &step.checks).collect();
            let passed = all
                .iter()
                .filter(|check| check.outcome == CheckOutcome::Pass)
                .count();
            let warned = all
                .iter()
                .filter(|check| matches!(check.outcome, CheckOutcome::Warn { .. }))
                .count();
            format!("Checks: {passed} passed, {warned} with a warning")
        })
    };
    let preset = Memo::new(move |_| {
        status.with(|s| {
            s.as_ref()
                .and_then(|s| UploadPreset::of(s.limits.upload_kbit))
        })
    });
    view! {
        <div class="cards public-cards">
            <div class="card">
                <h4>"Reachability"</h4>
                <p>{with(|s, _| s.share.as_ref().map(|share| share.zodl.clone()).unwrap_or_default())}</p>
                <p class="muted">{with(|s, now| reach_text(&s.reach, now))}</p>
                <p class="muted">
                    {with(|s, _| format!(
                        "{} on your router → {}:{}",
                        s.external_port,
                        s.lan_addr.map_or_else(|| "this computer".to_owned(), |lan| lan.to_string()),
                        s.internal_port,
                    ))}
                </p>
            </div>
            <div class="card">
                <h4>"Certificate"</h4>
                <p>{with(cert_text)}</p>
            </div>
            <div class="card">
                <h4>"Use"</h4>
                <ul class="public-use">
                    {move || status.with(|s| s.as_ref().map(use_lines).unwrap_or_default())
                        .into_iter()
                        .map(|line| view! { <li>{line}</li> })
                        .collect_view()}
                </ul>
            </div>
        </div>
        {move || share().map(|share| view! {
            <div class="public-share">
                <h4>"Share"</h4>
                <p class="muted">"Zodl: Settings → Advanced → Choose a server → Custom, in Manual mode."</p>
                <CopyLine text=share.zodl />
                <p class="muted">"Zingo and Zkool:"</p>
                <CopyLine text=share.zingo />
                <p class="muted">
                    "The wallet must know NU7, and Zodl's Automatic mode also sends transactions to \
                     its other servers. This is Zebra's experimental light wallet server."
                </p>
            </div>
        })}
        <fieldset class="public-keep">
            <legend>"Keep it running"</legend>
            <label class="check">
                <input
                    type="checkbox"
                    prop:checked=move || status.with(|s| s.as_ref().is_some_and(|s| s.keep_awake))
                    on:change=move |event| answer(StepAnswer::KeepAwake { on: event_target_checked(&event) })
                />
                "Keep this computer awake while serving (it cannot stop a closed lid)"
            </label>
            <label class="field">
                <span>"Upload limit"</span>
                <select
                    prop:value=move || match preset.get() {
                        Some(UploadPreset::Light) => "light",
                        Some(UploadPreset::Unlimited) => "unlimited",
                        _ => "standard",
                    }
                    on:change=move |event| {
                        let preset = match event_target_value(&event).as_str() {
                            "light" => UploadPreset::Light,
                            "unlimited" => UploadPreset::Unlimited,
                            _ => UploadPreset::Standard,
                        };
                        answer(StepAnswer::Upload { preset });
                    }
                >
                    <option value="light">"Light: 5 Mbit/s"</option>
                    <option value="standard">"Standard: 10 Mbit/s"</option>
                    <option value="unlimited">"No limit"</option>
                </select>
            </label>
        </fieldset>
        <p class="muted">{checks}</p>
        {move || note.get().map(|note| view! { <p class="error">{note}</p> })}
    }
}

/// "Turn off and forget", with what stays public afterwards.
#[component]
fn Forget() -> impl IntoView {
    let public = PublicContext::get();
    let asking = RwSignal::new(false);
    let remove_token = RwSignal::new(true);
    let busy = RwSignal::new(false);
    let result = RwSignal::new(None::<Vec<String>>);
    let forget = move |_| {
        busy.set(true);
        spawn_local(async move {
            match api::public::forget_public_service(remove_token.get_untracked()).await {
                Ok(still) => {
                    let mut lines = Vec::new();
                    if let Some((external, lan, internal)) = still.forward {
                        let lan =
                            lan.map_or_else(|| "this computer".to_owned(), |lan| lan.to_string());
                        lines.push(format!(
                            "Remove the router forward you added: TCP {external} → {lan}:{internal}."
                        ));
                    }
                    lines.extend(
                        still
                            .firewall_remove
                            .iter()
                            .map(|command| format!("Remove the firewall rule: {command}")),
                    );
                    if let Some(name) = still.name {
                        lines.push(format!(
                            "{name} stays in DNS history services, and in public certificate logs."
                        ));
                    }
                    result.set(Some(lines));
                    public.refresh();
                }
                Err(error) => result.set(Some(vec![error.to_string()])),
            }
            busy.set(false);
            asking.set(false);
        });
    };
    view! {
        <div class="public-forget">
            <Show
                when=move || asking.get()
                fallback=move || view! {
                    <button type="button" class="danger" on:click=move |_| asking.set(true)>
                        "Turn off and forget…"
                    </button>
                }
            >
                <p>"Zeal closes the front door, removes Zebra's light wallet endpoint and forgets the setup."</p>
                <label class="check">
                    <input
                        type="checkbox"
                        prop:checked=move || remove_token.get()
                        on:change=move |event| remove_token.set(event_target_checked(&event))
                    />
                    "Also delete the DuckDNS token from the keychain"
                </label>
                <div class="buttons">
                    <button type="button" class="danger" disabled=busy on:click=forget>"Turn off and forget"</button>
                    <button type="button" on:click=move |_| asking.set(false)>"Cancel"</button>
                </div>
            </Show>
            {move || result.get().map(|lines| view! {
                <ul class="public-still">{lines.into_iter().map(|line| view! { <li>{line}</li> }).collect_view()}</ul>
            })}
        </div>
    }
}

/// The light client server on the node's dashboard: its state, and the way to its settings.
#[component]
pub fn PublicCard() -> impl IntoView {
    let state = AppState::get();
    let status = light_status();
    move || {
        let status = status.get()?;
        (state.node().info.with(|info| info.network) == status.network).then(|| {
            let (tone, line) = state_line(&status, state.now.get());
            view! {
                <section class="card public-dash" aria-label="Light client server">
                    <h3>{SERVICE_NAME}</h3>
                    <p class=tone_class(tone)>{line}</p>
                    <button type="button" class="link" on:click=move |_| state.tab.set(Tab::Settings)>
                        "Settings → Public services"
                    </button>
                </section>
            }
        })
    }
}

/// The header banner on the node's pages: automatic pauses, problems wallets see and a
/// certificate that will expire without a renewal.
#[component]
pub fn PublicBanner() -> impl IntoView {
    let state = AppState::get();
    let status = light_status();
    move || {
        let status = status.get()?;
        if !state.tab.get().is_node_tab() || state.network.get() != status.network {
            return None;
        }
        let (tone, text) = public_text::banner(&status, state.now.get())?;
        Some(view! {
            <div class=tone.banner_class() role="status">
                <span>{text}</span>
                <button type="button" class="link" on:click=move |_| state.tab.set(Tab::Settings)>
                    "Open public services"
                </button>
            </div>
        })
    }
}
