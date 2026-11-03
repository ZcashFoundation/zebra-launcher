//! The words for the public services (docs/PUBLIC-SERVICES.md): every sentence the light client
//! server's screens show comes from the typed facts in its status, here, as
//! [`crate::update_text`] does for updates. Pure Rust, tested natively.

use std::time::{Duration, SystemTime};

use launcher_types::{
    Network,
    public::{
        AcmeProblem, CheckId, CheckOutcome, DdnsProblem, DegradedReason, EvidenceSource, Finding,
        FirewallTool, LocalTestProblem, NotAvailable, PauseReason, PublicState, PublicStatus,
        Reachability, StepId, StepState, WalletProblem,
    },
};

use crate::format::{bytes, local_date_short, relative, rough_duration, thousands};

/// How a line reads: its colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Ok,
    Neutral,
    Warn,
    Error,
}

impl Tone {
    /// The banner class for this tone.
    pub fn banner_class(self) -> &'static str {
        match self {
            Tone::Ok => "banner banner-ok",
            Tone::Neutral => "banner",
            Tone::Warn => "banner banner-warn",
            Tone::Error => "banner banner-error",
        }
    }
}

/// The service's name, with the word upstream uses for it.
pub const SERVICE_NAME: &str = "Light client server (experimental)";

/// A banner shows this long before a failing certificate expires.
pub const EXPIRY_WARNING: Duration = Duration::from_secs(14 * 24 * 3600);

/// The network's name as the UI writes it.
pub fn network_name(network: Network) -> &'static str {
    match network {
        Network::Mainnet => "Mainnet",
        Network::Testnet => "Testnet",
    }
}

/// Why the front door is closed, in a sentence after "Paused".
pub fn pause_text(why: &PauseReason, status: &PublicStatus) -> String {
    let name = status
        .name
        .as_ref()
        .map_or_else(|| "the name".to_owned(), ToString::to_string);
    match why {
        PauseReason::ByYou => "you switched the light client server off.".into(),
        PauseReason::LanAddressChanged { was } => format!(
            "this computer's address on your network changed from {was}, so the router forward \
             no longer reaches it. Reserve the address again, or update the forward."
        ),
        PauseReason::NameElsewhere { points_to } => {
            let to = points_to
                .first()
                .map_or_else(|| "nowhere".to_owned(), ToString::to_string);
            format!("{name} points at {to}, not this network. Zeal resumes when it matches.")
        }
        PauseReason::VpnRoute { interface } => format!(
            "a VPN ({interface}) carries this computer's traffic, so wallets' answers would leave \
             the wrong way. Turn it off, or set it to leave Zeal out."
        ),
        PauseReason::DataCap => {
            "this month's upload reached its cap. Zeal resumes next month, or raise the cap.".into()
        }
        PauseReason::BackingStopped => {
            "Zebra is not running. Wallets that use your server see an error until it runs.".into()
        }
        PauseReason::NetworkChanged => format!(
            "Zeal runs Zebra on the other network now. The light client server serves {}: \
             switch back, then resume.",
            network_name(status.network)
        ),
        PauseReason::NotResumed => {
            "Zeal started without opening it, as you chose. Switch it on to resume.".into()
        }
    }
}

/// Why wallets are told to try again, in a sentence.
pub fn degraded_text(why: &DegradedReason) -> String {
    match why {
        DegradedReason::BackingRestarting => {
            "Zebra is restarting. Wallets are told to try again.".into()
        }
        DegradedReason::BackingBehind { blocks } => format!(
            "Zebra is catching up, {} blocks behind. Wallets are told to try again.",
            thousands(*blocks)
        ),
        DegradedReason::WalletCheckFailed => {
            "Zebra's light wallet endpoint gave a wrong answer to the wallet check. Wallets are \
             told to try again."
                .into()
        }
        DegradedReason::CertificateExpired => {
            "the certificate expired and could not be renewed. Wallets cannot connect.".into()
        }
    }
}

/// What reachability says, with its evidence and age.
pub fn reach_text(reach: &Reachability, now: SystemTime) -> String {
    let source = |source: EvidenceSource| match source {
        EvidenceSource::TlsAlpn01Validation => "Let's Encrypt reached it through your router",
        EvidenceSource::OutsideTlsConnection => "a connection from outside completed TLS",
    };
    match reach {
        Reachability::NotChecked => "not checked from outside yet".into(),
        Reachability::NotYet => "not yet confirmed from outside".into(),
        Reachability::Proven { latest } => format!(
            "reachable from the internet ({} {})",
            source(latest.source),
            relative(latest.at, now)
        ),
        Reachability::Recent { latest } => format!(
            "last confirmed from outside {} ({})",
            relative(latest.at, now),
            source(latest.source)
        ),
        Reachability::Stale { latest } => format!(
            "not confirmed from outside since {}",
            local_date_short(latest.at)
        ),
        Reachability::Lost { since, .. } => {
            format!("Let's Encrypt could not reach it {}", relative(*since, now))
        }
    }
}

/// The first line of the panel: the state and what it means, with its tone.
pub fn state_line(status: &PublicStatus, now: SystemTime) -> (Tone, String) {
    match &status.state {
        PublicState::Off => (Tone::Neutral, "Off".into()),
        PublicState::NotAvailable { why } => (
            Tone::Neutral,
            match why {
                NotAvailable::NotOnThisSystem => {
                    "Not available · public services run on Linux for now".into()
                }
                NotAvailable::UntestedZebrad { version } => format!(
                    "Not available · Zeal hasn't tested Zebra {version} as a light client server"
                ),
            },
        ),
        PublicState::SettingUp { step } => (
            Tone::Neutral,
            format!("Setting up · {}", step_name(*step).to_lowercase()),
        ),
        PublicState::Serving => {
            let tone = match status.reach {
                Reachability::Proven { .. } => Tone::Ok,
                _ => Tone::Warn,
            };
            let reach = match status.reach {
                Reachability::NotChecked | Reachability::NotYet => {
                    "not yet confirmed from outside".to_owned()
                }
                ref reach => reach_text(reach, now),
            };
            (tone, format!("Serving · {reach}"))
        }
        PublicState::Degraded { why } => (
            Tone::Warn,
            match why {
                DegradedReason::BackingBehind { blocks } => format!(
                    "Catching up · wallets are told to try again ({} blocks behind)",
                    thousands(*blocks)
                ),
                other => format!("Serving with a problem · {}", degraded_text(other)),
            },
        ),
        PublicState::Paused { why, .. } => {
            let tone = if *why == PauseReason::ByYou {
                Tone::Neutral
            } else {
                Tone::Warn
            };
            (tone, format!("Paused · {}", pause_text(why, status)))
        }
    }
}

/// The certificate's line: where it is from and how long it lasts.
pub fn cert_text(status: &PublicStatus, now: SystemTime) -> String {
    let Some(cert) = &status.cert else {
        return "No certificate yet".into();
    };
    match cert.not_after {
        Some(not_after) if not_after > now => {
            let days = not_after.duration_since(now).unwrap_or_default().as_secs() / 86_400;
            let renews = cert
                .renew_at
                .map(|at| format!(", renews about {}", local_date_short(at)))
                .unwrap_or_default();
            format!("Let's Encrypt · {days} days left{renews}")
        }
        Some(_) => "The certificate expired".into(),
        None => "No certificate yet".into(),
    }
}

/// A banner for the header and the panel: automatic pauses, problems wallets see and a
/// certificate that will expire without a renewal. `None` when all is well or the user paused
/// it.
pub fn banner(status: &PublicStatus, now: SystemTime) -> Option<(Tone, String)> {
    let failing_expiry = status.cert.as_ref().and_then(|cert| {
        let not_after = cert.not_after?;
        let left = not_after.duration_since(now).unwrap_or_default();
        (cert.last_problem.is_some() && left < EXPIRY_WARNING && not_after > now).then_some(left)
    });
    match &status.state {
        PublicState::Paused { why, .. } if *why != PauseReason::ByYou => Some((
            Tone::Warn,
            format!("Light client server paused: {}", pause_text(why, status)),
        )),
        PublicState::Degraded {
            why: DegradedReason::CertificateExpired,
        } => Some((
            Tone::Error,
            format!(
                "Light client server: {}",
                degraded_text(&DegradedReason::CertificateExpired)
            ),
        )),
        PublicState::Degraded { why } => Some((
            Tone::Warn,
            format!("Light client server: {}", degraded_text(why)),
        )),
        _ => failing_expiry.map(|left| {
            (
                Tone::Warn,
                format!(
                    "The light client server's certificate expires in {} and could not be \
                     renewed: {}",
                    rough_duration(left),
                    status
                        .cert
                        .as_ref()
                        .and_then(|cert| cert.last_problem.as_ref())
                        .map(acme_text)
                        .unwrap_or_default()
                ),
            )
        }),
    }
}

/// A step's short name.
pub fn step_name(step: StepId) -> &'static str {
    match step {
        StepId::Consent => "What becomes public",
        StepId::Service => "Zebra's light wallet endpoint",
        StepId::Network => "This network",
        StepId::Name => "The name",
        StepId::Port => "The router forward",
        StepId::Firewall => "This computer's firewall",
        StepId::Certificate => "A certificate from Let's Encrypt",
        StepId::LocalTest => "Test on this computer",
        StepId::Open => "Open to the internet",
        StepId::KeepRunning => "Keep it running",
        StepId::Share => "Share",
    }
}

/// A step's line in the checklist, with the real names and addresses once they are known.
pub fn step_title(step: StepId, status: &PublicStatus) -> String {
    let lan = status
        .lan_addr
        .or_else(|| status.net.as_ref().and_then(|net| net.lan_addr))
        .map_or_else(
            || "this computer".to_owned(),
            |lan| format!("this computer ({lan})"),
        );
    match step {
        StepId::Consent if status.consent.is_some() => "You agreed to what becomes public".into(),
        StepId::Service => format!(
            "Zebra · {} serves light wallets on this computer",
            network_name(status.network)
        ),
        StepId::Network => match status.net.as_ref().and_then(|net| net.wan_addr) {
            Some(wan) => format!("Your router has a public address, {wan}"),
            None => "Your router has a public address".into(),
        },
        StepId::Name => match &status.name {
            Some(name) => format!("{name} points here"),
            None => "A name for your server".into(),
        },
        StepId::Port => format!(
            "Port {} on your router → {lan}, port {}",
            status.external_port, status.internal_port
        ),
        other => step_name(other).to_owned(),
    }
}

/// The checklist's mark for a step.
pub fn step_mark(state: StepState) -> &'static str {
    match state {
        StepState::Done => "✓",
        StepState::Running | StepState::NeedsYou => "●",
        StepState::Waiting => "○",
        StepState::Failed => "✕",
    }
}

/// The name of a check.
pub fn check_name(check: CheckId) -> &'static str {
    match check {
        CheckId::Consent => "Your agreement",
        CheckId::Platform => "This system",
        CheckId::ZebradTested => "A tested Zebra",
        CheckId::EndpointOpened => "The light wallet endpoint",
        CheckId::WalletCheck => "The wallet check",
        CheckId::AtTip => "Zebra at the tip",
        CheckId::LanAddress => "This computer's address",
        CheckId::NoVpn => "No VPN",
        CheckId::WanAddress => "The router's public address",
        CheckId::LanReserved => "The address reserved",
        CheckId::NameChosen => "The name",
        CheckId::Token => "The DuckDNS token",
        CheckId::AddressUpdate => "The address update",
        CheckId::NameResolves => "The name points here",
        CheckId::PortFree => "Port free on this computer",
        CheckId::Forward => "The router forward",
        CheckId::Firewall => "The firewall",
        CheckId::StagingCertificate => "A test certificate through your router",
        CheckId::Certificate => "The certificate",
        CheckId::LocalTest => "A wallet's view from this computer",
        CheckId::TipSettled => "Zebra settled at the tip",
        CheckId::Hairpin => "The public address from inside",
        CheckId::KeepAwake => "Keep running",
    }
}

/// What went wrong with an ACME order, with its fix first.
pub fn acme_text(problem: &AcmeProblem) -> String {
    match problem {
        AcmeProblem::Connection => "Let's Encrypt couldn't reach this computer: check the router \
                                    forward and the firewall. Some providers block port 443."
            .into(),
        AcmeProblem::Dns | AcmeProblem::Unauthorized => {
            "Let's Encrypt found the name pointing elsewhere, or not updated yet. Wait a few \
             minutes and try again."
                .into()
        }
        AcmeProblem::Caa => {
            "A CAA record on the name forbids Let's Encrypt. Remove it, or allow letsencrypt.org."
                .into()
        }
        AcmeProblem::RateLimited { retry_at: Some(at) }
        | AcmeProblem::AttemptsSpent { retry_at: at } => format!(
            "Too many tries for now: Zeal tries again on {}.",
            local_date_short(*at)
        ),
        AcmeProblem::RateLimited { retry_at: None } => {
            "Too many tries for now: Zeal tries again later.".into()
        }
        AcmeProblem::Verify => {
            "The certificate Let's Encrypt issued does not verify for the name.".into()
        }
        AcmeProblem::Unreachable => {
            "Let's Encrypt could not be reached from this computer. Check the internet connection."
                .into()
        }
        AcmeProblem::Other { detail } => format!("Let's Encrypt said: {detail}"),
    }
}

fn wallet_text(problem: &WalletProblem) -> String {
    match problem {
        WalletProblem::NoAnswer { detail } => format!("no wallet answer ({detail})"),
        WalletProblem::WrongChain { got, want } => {
            format!("it reports the {got} chain, not {want}")
        }
        WalletProblem::WrongSaplingHeight { got, want } => format!(
            "it reports Sapling at height {}, not {}",
            thousands(*got),
            thousands(*want)
        ),
        WalletProblem::WrongBranch { got, want } => {
            format!("it reports consensus branch {got} where wallets expect {want}")
        }
        WalletProblem::HeightOff { got, want } => format!(
            "it reports height {} where Zebra is at {}",
            thousands(*got),
            thousands(*want)
        ),
    }
}

fn ddns_text(problem: DdnsProblem) -> &'static str {
    match problem {
        DdnsProblem::Refused => {
            "DuckDNS refused the token or the name. Check both and save the token again."
        }
        DdnsProblem::Unreachable => "DuckDNS could not be reached. Zeal tries again.",
        DdnsProblem::BadAnswer => {
            "DuckDNS gave an answer Zeal doesn't understand. Zeal tries again."
        }
        DdnsProblem::TooMany => "DuckDNS asked Zeal to slow down. Zeal tries again later.",
    }
}

/// What a check found, with the fix first.
pub fn finding_text(finding: &Finding) -> String {
    match finding {
        Finding::ConsentNeeded => "Read what becomes public, and agree to go on.".into(),
        Finding::NotOnThisSystem => "Public services run on Linux for now.".into(),
        Finding::UntestedZebrad { version } => {
            format!("Zeal hasn't tested Zebra {version} as a light client server.")
        }
        Finding::ZebradNotRunning => "Start Zebra: the endpoint opens with it.".into(),
        Finding::EndpointNotOpen => "Zebra is opening its light wallet endpoint.".into(),
        Finding::EndpointPortTaken { port } => format!(
            "Another program uses port {port} on this computer, so Zebra could not open its \
             light wallet endpoint. Check again for another port, then start Zebra."
        ),
        Finding::EndpointNotWritten { detail } => {
            format!("Zeal could not restart Zebra with its endpoint: {detail}. Try again.")
        }
        Finding::WalletCheck { problem } => {
            format!(
                "Wallets would refuse this server: {}.",
                wallet_text(problem)
            )
        }
        Finding::Syncing {
            height,
            tip,
            eta_secs,
        } => {
            let eta = eta_secs
                .map(|secs| format!(", in about {}", rough_duration(Duration::from_secs(secs))))
                .unwrap_or_default();
            format!(
                "Zebra is at block {} of {}. Wallets can use your server once it reaches the \
                 tip{eta}; the setup goes on meanwhile.",
                thousands(*height),
                thousands(*tip)
            )
        }
        Finding::NoLanAddress => {
            "This computer has no address on a local network. Connect it to your router.".into()
        }
        Finding::Vpn { interface } => format!(
            "A VPN ({interface}) carries this computer's traffic. Turn it off, or set it to \
             leave Zeal out."
        ),
        Finding::Cgnat { wan } => format!(
            "Your provider shares one address among many homes (carrier-grade NAT, {wan}), so \
             nobody outside can connect in. Many providers give a public IPv4 address on \
             request."
        ),
        Finding::BehindAnotherRouter { wan } => format!(
            "Your router's own address, {wan}, is private: a second router sits in front of it. \
             Forward the port on both, or put one in bridge mode."
        ),
        Finding::WanUnknown => {
            "Your router didn't say its public address. The check from outside decides.".into()
        }
        Finding::LanNotReserved { lan } => format!(
            "Reserve {lan} for this computer in your router's DHCP settings: a forward targets \
             an address."
        ),
        Finding::NameMissing => "Choose a name wallets will use.".into(),
        Finding::BadName { error } => format!("That name doesn't work: {error}."),
        Finding::TokenMissing => {
            "Paste your DuckDNS token. Zeal keeps it in this computer's keychain.".into()
        }
        Finding::KeychainUnavailable => {
            "This computer's keychain could not keep the token. Unlock it, or check that a \
             Secret Service runs."
                .into()
        }
        Finding::AddressUpdateFailed { problem } => ddns_text(*problem).into(),
        Finding::NameNotFound => {
            "The name doesn't resolve yet. Zeal checks again every 30 seconds.".into()
        }
        Finding::NamePointsElsewhere {
            points_to,
            expected,
        } => {
            let to = points_to
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            match expected {
                Some(expected) => format!("The name points at {to}, not {expected}."),
                None => format!("The name points at {to}."),
            }
        }
        Finding::NameHasIpv6 { addrs } => format!(
            "The name also has an IPv6 address ({}), which Zeal didn't check. Let's Encrypt \
             tries IPv6 first: remove it.",
            addrs
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Finding::ProviderSawOther {
            provider_saw,
            router,
        } => format!(
            "DuckDNS saw this computer at {provider_saw}, but the router says {router}: a \
             second router or carrier-grade NAT may be in the way."
        ),
        Finding::PortTaken { port } => format!(
            "Another program uses port {port} on this computer. Close it, then check again."
        ),
        Finding::ForwardNotConfirmed => {
            "Add the forward in your router's settings: it takes about two minutes.".into()
        }
        Finding::Firewall { tool, port } => format!(
            "{} is installed. If it is on, allow port {port}: copy the command below into a \
             terminal. Zeal never runs it.",
            match tool {
                FirewallTool::Ufw => "ufw",
                FirewallTool::Firewalld => "firewalld",
            }
        ),
        Finding::Acme { problem } => acme_text(problem),
        Finding::LocalTestFailed { problem } => match problem {
            LocalTestProblem::Connect { detail } => {
                format!("The front door did not answer on this computer: {detail}.")
            }
            LocalTestProblem::Certificate { detail } => {
                format!("The certificate does not verify: {detail}.")
            }
            LocalTestProblem::Wallet { problem } => {
                format!("A wallet would refuse it: {}.", wallet_text(problem))
            }
        },
        Finding::TipNotSettled { .. } => {
            "Zebra must stay at the tip for 10 minutes before the server opens.".into()
        }
        Finding::NoHairpin => "Your router doesn't loop connections to its public address back \
                               in. That proves nothing about the forward, but a phone at home \
                               then needs mobile data to reach your server."
            .into(),
        Finding::BehindTip { blocks } => {
            format!("Zebra is {} blocks behind the tip.", thousands(*blocks))
        }
    }
}

/// A check's outcome as one line: its name and what it found.
pub fn check_line(check: CheckId, outcome: &CheckOutcome) -> String {
    match outcome {
        CheckOutcome::NotRun => format!("{} · not checked yet", check_name(check)),
        CheckOutcome::Running => format!("{} · checking…", check_name(check)),
        CheckOutcome::Pass => format!("{} · passed", check_name(check)),
        CheckOutcome::Warn { finding } | CheckOutcome::Fail { finding } => {
            format!("{} · {}", check_name(check), finding_text(finding))
        }
    }
}

/// The consent screen's lines: what becomes public, for good, then what the user takes on.
pub fn consent_lines(status: &PublicStatus) -> (Vec<String>, Vec<String>) {
    let (name, sentence) = match &status.name {
        Some(name) => (name.to_string(), format!("The name {name}")),
        None => (
            "the name you choose".to_owned(),
            "The name you choose".to_owned(),
        ),
    };
    let address = status
        .net
        .as_ref()
        .and_then(|net| net.wan_addr)
        .map_or_else(
            || "Your home internet address".to_owned(),
            |wan| format!("Your home internet address, {wan}"),
        );
    let version = status
        .zebrad_version
        .as_deref()
        .map(|version| version.trim_start_matches('v'))
        .map(|version| version.split('+').next().unwrap_or(version).to_owned())
        .unwrap_or_else(|| "its version".into());
    let public = vec![
        format!("{address}, under {name}. DNS history services keep it after you stop."),
        format!(
            "{sentence}, in public certificate logs, minutes after Zeal gets its certificate. \
             Scanners read those logs, so expect connection attempts right away."
        ),
        format!(
            "That this computer runs Zcash software: every wallet that connects is told it is \
             Zebra {version}."
        ),
    ];
    let yours = vec![
        "Zebra's light wallet server is experimental: wallets may not work with it yet, and a \
         Zebra update may change it."
            .to_owned(),
        "Wallets that use your server show it their address, the blocks they download, the \
         transparent addresses they look up and the transactions they send. Zeal keeps no \
         client addresses, but anyone who controls this computer could."
            .to_owned(),
        "While the light client server is switched on, including during setup and automatic \
         pauses, programs on this computer can use Zebra's light wallet port without a \
         password. Web pages in a browser can't."
            .to_owned(),
        "Your internet plan may not allow servers, and the traffic counts toward any data cap. \
         Check its terms, and your local law."
            .to_owned(),
        "Zeal gets the certificate from Let's Encrypt, so you accept its Subscriber Agreement."
            .to_owned(),
        "Zeal talks to your router, your DNS provider and Let's Encrypt. No one else.".to_owned(),
    ];
    (public, yours)
}

/// The use card's lines: connections, handshakes and this month's upload.
pub fn use_lines(status: &PublicStatus) -> Vec<String> {
    let Some(light) = &status.light else {
        return Vec::new();
    };
    let gateway = &light.gateway;
    let cap = status
        .limits
        .monthly_gb
        .map(|gb| format!(" of {gb} GB"))
        .unwrap_or_default();
    vec![
        format!(
            "{} connections, {} networks",
            gateway.open_connections, gateway.client_networks
        ),
        format!(
            "{} handshakes since the front door started",
            thousands(gateway.handshakes)
        ),
        format!("{}{cap} sent this month", bytes(light.month_bytes_up)),
        format!(
            "{} refused by the limits",
            thousands(gateway.refused_limits)
        ),
    ]
}

#[cfg(test)]
mod tests {
    use launcher_herd::public::{Facts, status};
    use launcher_types::{
        NodeId,
        public::{
            CONSENT_VERSION, CertStatus, Consent, Evidence, PublicHostname, PublicServiceSpec,
        },
    };

    use super::*;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    const NOW: u64 = 1_800_000_000;

    fn base() -> PublicStatus {
        let mut spec = PublicServiceSpec::new_light_server(
            NodeId::new("zebrad-mainnet").unwrap(),
            Network::Mainnet,
        );
        spec.name = Some(PublicHostname::parse("zec-alice.duckdns.org").unwrap());
        spec.consent = Some(Consent {
            version: CONSENT_VERSION,
            at: at(NOW),
            hostname: "zec-alice.duckdns.org".into(),
        });
        status(&spec, &Facts::new(at(NOW), Network::Mainnet), at(NOW))
    }

    #[test]
    fn pauses_say_why_and_how_they_end() {
        let status = base();
        let text = pause_text(
            &PauseReason::NameElsewhere {
                points_to: vec!["198.51.100.7".parse().unwrap()],
            },
            &status,
        );
        assert_eq!(
            text,
            "zec-alice.duckdns.org points at 198.51.100.7, not this network. Zeal resumes when it matches."
        );
        assert!(pause_text(&PauseReason::NetworkChanged, &status).contains("serves Mainnet"));
    }

    #[test]
    fn the_first_line_follows_the_state_and_the_evidence() {
        let now = at(NOW);
        let mut status = base();
        status.state = PublicState::Serving;
        status.reach = Reachability::NotYet;
        assert_eq!(
            state_line(&status, now),
            (
                Tone::Warn,
                "Serving · not yet confirmed from outside".into()
            )
        );
        status.reach = Reachability::Proven {
            latest: Evidence {
                source: EvidenceSource::OutsideTlsConnection,
                at: at(NOW - 240),
            },
        };
        let (tone, line) = state_line(&status, now);
        assert_eq!(tone, Tone::Ok);
        assert!(
            line.starts_with(
                "Serving · reachable from the internet (a connection from outside completed TLS"
            ),
            "{line}"
        );
        status.state = PublicState::Degraded {
            why: DegradedReason::BackingBehind { blocks: 1234 },
        };
        assert_eq!(
            state_line(&status, now).1,
            "Catching up · wallets are told to try again (1,234 blocks behind)"
        );
        status.state = PublicState::SettingUp { step: StepId::Port };
        assert_eq!(
            state_line(&status, now).1,
            "Setting up · the router forward"
        );
    }

    #[test]
    fn a_failing_renewal_warns_two_weeks_out() {
        let now = at(NOW);
        let mut status = base();
        status.state = PublicState::Serving;
        status.cert = Some(CertStatus {
            not_after: Some(at(NOW + 10 * 86_400)),
            not_before: Some(at(NOW - 80 * 86_400)),
            renew_at: Some(at(NOW - 20 * 86_400)),
            staging_validated_at: Some(at(NOW - 80 * 86_400)),
            ordering: None,
            next_try: None,
            last_problem: Some(AcmeProblem::Connection),
        });
        let (tone, text) = banner(&status, now).expect("a banner");
        assert_eq!(tone, Tone::Warn);
        assert!(text.contains("expires in 10 days"), "{text}");
        assert!(text.contains("couldn't reach this computer"), "{text}");
        // Without a failure, an expiry is the renewal's business.
        status.cert.as_mut().unwrap().last_problem = None;
        assert_eq!(banner(&status, now), None);
        // The user's own pause needs no banner.
        status.state = PublicState::Paused {
            why: PauseReason::ByYou,
            since: now,
            resumes: launcher_types::public::Resume::NeedsYou,
        };
        assert_eq!(banner(&status, now), None);
    }

    #[test]
    fn steps_name_the_real_addresses() {
        let mut status = base();
        status.lan_addr = Some("192.168.1.20".parse().unwrap());
        assert_eq!(
            step_title(StepId::Port, &status),
            "Port 443 on your router → this computer (192.168.1.20), port 8443"
        );
        assert_eq!(
            step_title(StepId::Name, &status),
            "zec-alice.duckdns.org points here"
        );
    }

    #[test]
    fn every_finding_has_words() {
        let findings = [
            Finding::Cgnat {
                wan: "100.72.4.9".parse().unwrap(),
            },
            Finding::ForwardNotConfirmed,
            Finding::Acme {
                problem: AcmeProblem::Connection,
            },
            Finding::Syncing {
                height: 2_900_000,
                tip: 3_544_812,
                eta_secs: Some(9 * 3600),
            },
        ];
        for finding in &findings {
            let text = finding_text(finding);
            assert!(text.ends_with('.'), "{text}");
        }
        assert!(finding_text(&findings[0]).contains("carrier-grade NAT, 100.72.4.9"));
        assert!(finding_text(&findings[3]).contains("block 2,900,000 of 3,544,812"));
    }

    #[test]
    fn consent_names_the_real_things() {
        let mut status = base();
        status.zebrad_version = Some("v7.0.0+git.e8db87e".into());
        let (public, yours) = consent_lines(&status);
        assert!(
            public[1].starts_with("The name zec-alice.duckdns.org, in")
                && public[0].contains("under zec-alice.duckdns.org"),
            "{public:?}"
        );
        assert!(public[2].ends_with("Zebra 7.0.0."), "{public:?}");
        assert!(yours.iter().any(|line| line.contains("without a password")));
    }
}
