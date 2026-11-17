//! The words for the public services (docs/PUBLIC-SERVICES.md): every sentence the light client
//! server's screens show comes from the typed facts in its status, here, as
//! [`crate::update_text`] does for updates. Pure Rust, tested natively.

use std::time::{Duration, SystemTime};

use launcher_herd::public::zingod::{CATCHING_UP_BLOCKS, DISK_HIGH, space_needed};
use launcher_types::{
    ExitClass, Network, Phase,
    public::{
        AcmeProblem, CheckId, CheckOutcome, CheckResult, DdnsProblem, DegradedReason, DownReason,
        EvidenceSource, FailDetail, Finding, FirewallTool, IndexerStats, IndexerWait, LightBackend,
        LocalTestProblem, NotAvailable, PauseReason, PublicState, PublicStatus, Reachability,
        StepId, StepState, WalletProblem, ZingodChoice,
    },
    zingod::IndexerState,
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

/// The serving node as the UI names it: `Zebra · Testnet`.
pub fn zebra_name(network: Network) -> String {
    format!("Zebra · {}", network_name(network))
}

/// The serving node, by its network when the status is at hand.
fn serving_zebra(status: Option<&PublicStatus>) -> String {
    status.map_or_else(|| "Zebra".to_owned(), |status| zebra_name(status.network))
}

/// The bundled zingod's version, as the status reports it.
fn zingod_version(status: &PublicStatus) -> String {
    status.indexer.as_ref().map_or_else(
        || launcher_types::zingod::version().to_owned(),
        |indexer| indexer.version.clone(),
    )
}

/// `18137 and 18138`, `18137, 18138 and 18139`.
fn list_ports(ports: &[u16]) -> String {
    let words: Vec<String> = ports.iter().map(ToString::to_string).collect();
    match words.as_slice() {
        [] => "its port".to_owned(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// How far zingod's build is: `block 1,204,331 of 4,612,000 (26 %), about 5 h left`; or `an
/// hour or more` before it has numbers.
fn build_progress(served: Option<u64>, target: Option<u64>, eta_secs: Option<u64>) -> String {
    let (Some(served), Some(target)) = (served, target) else {
        return "an hour or more".to_owned();
    };
    let percent = served.saturating_mul(100) / target.max(1);
    let eta = match eta_secs {
        Some(secs) => format!(", about {} left", rough_duration(Duration::from_secs(secs))),
        None => ", an hour or more left".to_owned(),
    };
    format!(
        "block {} of {} ({percent} %){eta}",
        thousands(served),
        thousands(target)
    )
}

/// Whether a zingod sync is only catching up: it has a height, and fewer than
/// [`CATCHING_UP_BLOCKS`] are left, or no target to say how many.
fn catching_up(served: Option<u64>, target: Option<u64>) -> bool {
    match (served, target) {
        (Some(served), Some(target)) => target.saturating_sub(served) < CATCHING_UP_BLOCKS,
        (Some(_), None) => true,
        (None, _) => false,
    }
}

/// The panel's line for a zingod that waits for the user, from what its exit names.
fn failed_text(class: ExitClass, detail: &FailDetail, status: Option<&PublicStatus>) -> String {
    let zebra = serving_zebra(status);
    match (class, detail) {
        (ExitClass::ConfigError, FailDetail::Cookie { path, error }) => format!(
            "zingod can't read {zebra}'s cookie at {}: {error}.",
            path.display()
        ),
        (ExitClass::ConfigError, FailDetail::OpenFiles { needed, limit }) => format!(
            "zingod needs {} open files, and this computer allows Zeal {}. Raise the hard \
             open-file limit (LimitNOFILE in a systemd unit, or nofile in \
             /etc/security/limits.conf), restart Zeal, then try again.",
            thousands(*needed),
            thousands(*limit)
        ),
        (ExitClass::ConfigError, _) => {
            "zingod refused the settings Zeal wrote. This is a bug in Zeal: copy the diagnostics \
             and report it."
                .into()
        }
        (ExitClass::PortInUse, FailDetail::Ports { tried }) => {
            format!("Other programs use ports {}.", list_ports(tried))
        }
        (ExitClass::PortInUse, _) => "Another program uses zingod's port.".into(),
        (ExitClass::DbLocked, FailDetail::Dir { path }) => format!(
            "Another program holds zingod's index in {}.",
            path.display()
        ),
        (ExitClass::DbLocked, _) => "Another program holds zingod's index.".into(),
        (ExitClass::DiskFull, FailDetail::Dir { path }) => format!(
            "zingod ran out of room in {}, though the disk shows free space: a quota or its \
             inodes. Free some, then try again.",
            path.display()
        ),
        (ExitClass::DiskFull, _) => {
            "zingod ran out of room for its index. Free some, then try again.".into()
        }
        (ExitClass::IndexBroken, _) => indexer_text(&DegradedReason::IndexBroken, status),
        (ExitClass::SpawnFailed, FailDetail::Folder { path, error }) => format!(
            "Zeal could not set up zingod's folder {}: {error}. Fix its permissions or its disk, \
             then try again.",
            path.display()
        ),
        (ExitClass::SpawnFailed, _) => "Zeal could not start zingod.".into(),
        (ExitClass::Crash, _) => "zingod stopped unexpectedly 5 times in 10 minutes.".into(),
        (ExitClass::Clean | ExitClass::EosHalt, _) => "zingod stopped and waits for you.".into(),
    }
}

/// The status panel's first line for a zingod problem, without the state's prefix: each row of
/// docs/PUBLIC-SERVICES.md's table. `None` for a reason that isn't zingod's.
pub fn indexer_line(why: &DegradedReason, status: &PublicStatus) -> Option<String> {
    if !is_indexer_reason(why, status.backend) {
        return None;
    }
    let zebra = zebra_name(status.network);
    Some(match why {
        DegradedReason::BackingRestarting if status.backend == LightBackend::Zingod => {
            "Catching up · zingod restarts with Zebra".into()
        }
        DegradedReason::IndexerStarting {
            waiting: IndexerWait::ZebradTip,
        } => format!("Waiting for {zebra} to reach the tip: zingod starts then"),
        DegradedReason::IndexerStarting {
            waiting: IndexerWait::Boot,
        } => "Catching up · zingod is starting".into(),
        DegradedReason::IndexerStarting {
            waiting: IndexerWait::Backoff,
        } => "Catching up · zingod stopped unexpectedly, and Zeal is starting it again".into(),
        DegradedReason::IndexBuilding { served, target, .. } if catching_up(*served, *target) => {
            "Catching up · wallets are told to try again".into()
        }
        DegradedReason::IndexBuilding {
            served,
            target,
            eta_secs,
        } => format!(
            "Building zingod's index: {}",
            build_progress(*served, *target, *eta_secs)
        ),
        DegradedReason::IndexBehind { blocks } => format!(
            "Catching up · zingod is {} blocks behind Zebra",
            thousands(*blocks)
        ),
        DegradedReason::IndexerDown {
            why: DownReason::NoValidator,
        } => format!("Catching up · zingod can't reach {zebra}"),
        other => indexer_text(other, Some(status)),
    })
}

/// Whether `why` is a zingod problem, or zebrad's restart while zingod serves.
fn is_indexer_reason(why: &DegradedReason, backend: LightBackend) -> bool {
    match why {
        DegradedReason::BackingRestarting => backend == LightBackend::Zingod,
        DegradedReason::BackingBehind { .. }
        | DegradedReason::WalletCheckFailed
        | DegradedReason::CertificateExpired => false,
        _ => true,
    }
}

/// A zingod problem as a sentence of its own, for the banner, the panel and the setup. The
/// status, when at hand, names the network and the index's folder.
pub fn indexer_text(why: &DegradedReason, status: Option<&PublicStatus>) -> String {
    let zebra = serving_zebra(status);
    match why {
        DegradedReason::IndexerStarting {
            waiting: IndexerWait::ZebradTip,
        } => format!("zingod starts once {zebra} reaches the tip. Wallets are told to try again."),
        DegradedReason::IndexerStarting {
            waiting: IndexerWait::Boot,
        } => "zingod is starting. Wallets are told to try again.".into(),
        DegradedReason::IndexerStarting {
            waiting: IndexerWait::Backoff,
        } => "zingod stopped unexpectedly, and Zeal is starting it again. Wallets are told to \
              try again."
            .into(),
        DegradedReason::IndexBuilding { served, target, .. } if catching_up(*served, *target) => {
            "zingod is catching up with Zebra. Wallets are told to try again.".into()
        }
        DegradedReason::IndexBuilding {
            served,
            target,
            eta_secs,
        } => format!(
            "Building zingod's index: {}. Wallets can't use your server until this is done. \
             Quitting Zeal pauses it, and it goes on where it stopped.",
            build_progress(*served, *target, *eta_secs)
        ),
        DegradedReason::IndexBehind { blocks } => format!(
            "zingod is {} blocks behind Zebra. Wallets are told to try again.",
            thousands(*blocks)
        ),
        DegradedReason::IndexerDown {
            why: DownReason::Zingod { reason, message },
        } => {
            let said = if message.is_empty() { reason } else { message };
            format!("zingod is down: {said}.")
        }
        DegradedReason::IndexerDown {
            why: DownReason::NoAnswer,
        } => "zingod is down: it isn't answering.".into(),
        DegradedReason::IndexerDown {
            why: DownReason::NoValidator,
        } => format!("zingod can't reach {zebra}. Wallets are told to try again."),
        DegradedReason::IndexerUntested {
            version,
            zebrad,
            network,
        } => format!(
            "zingod {version} isn't tested with Zebra {zebrad} on {} yet. Wallets that use your \
             server are told to try again until Zeal updates zingod, or {zebra} runs a tested \
             version.",
            network_name(*network)
        ),
        DegradedReason::IndexerStuck { at, zebrad_at } => format!(
            "zingod is stuck at block {}; Zebra is at {}.",
            thousands(*at),
            thousands(*zebrad_at)
        ),
        DegradedReason::IndexerFailed { class, detail } => failed_text(*class, detail, status),
        DegradedReason::IndexBroken => "zingod's index is damaged. Rebuilding it takes hours, and \
                                        wallets are told to try again meanwhile."
            .into(),
        DegradedReason::ChainMismatch { at } => format!(
            "zingod's chain doesn't match Zebra's at block {}.",
            thousands(*at)
        ),
        DegradedReason::DiskLow { free_bytes } => {
            let place = status
                .and_then(|status| status.indexer.as_ref())
                .map(|indexer| format!(" for zingod's index in {}", indexer.dir.display()))
                .unwrap_or_default();
            format!(
                "Waiting for disk space: {} free{place}. zingod starts again above {}.",
                bytes(*free_bytes),
                bytes(DISK_HIGH)
            )
        }
        DegradedReason::IndexSpace {
            free_bytes,
            need_bytes,
        } => {
            let place = status
                .and_then(|status| status.indexer.as_ref())
                .map(|indexer| format!(" in {}", indexer.dir.display()))
                .unwrap_or_default();
            format!(
                "Waiting for room: zingod needs {} free{place} to build its index, and {} is \
                 free. It starts once there is room.",
                bytes(*need_bytes),
                bytes(*free_bytes)
            )
        }
        DegradedReason::BackingRestarting => {
            "zingod restarts with Zebra. Wallets are told to try again.".into()
        }
        DegradedReason::BackingBehind { .. }
        | DegradedReason::WalletCheckFailed
        | DegradedReason::CertificateExpired => backing_text(why, status),
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
        PauseReason::ConsentNeeded => {
            "what Zeal tells you before serving changed. Read it again and agree to resume.".into()
        }
        PauseReason::BuildTooOld { upgrade, .. } => format!(
            "zingod {} doesn't know {upgrade}, which is active. Wallets that use your server see \
             an error until Zeal updates zingod.",
            zingod_version(status)
        ),
    }
}

/// Why wallets are told to try again, in a sentence.
pub fn degraded_text(why: &DegradedReason, status: &PublicStatus) -> String {
    if is_indexer_reason(why, status.backend) {
        return indexer_text(why, Some(status));
    }
    backing_text(why, Some(status))
}

/// zebrad's and the front door's reasons, the same for either backend but the wallet check,
/// which names the server that answered it, when the status is at hand.
fn backing_text(why: &DegradedReason, status: Option<&PublicStatus>) -> String {
    match why {
        DegradedReason::BackingRestarting => {
            "Zebra is restarting. Wallets are told to try again.".into()
        }
        DegradedReason::BackingBehind { blocks } => format!(
            "Zebra is catching up, {} blocks behind. Wallets are told to try again.",
            thousands(*blocks)
        ),
        DegradedReason::WalletCheckFailed
            if status.is_some_and(|status| status.backend == LightBackend::Zingod) =>
        {
            "zingod gave a wrong answer to the wallet check. Wallets are told to try again.".into()
        }
        DegradedReason::WalletCheckFailed => {
            "Zebra's light wallet endpoint gave a wrong answer to the wallet check. Wallets are \
             told to try again."
                .into()
        }
        DegradedReason::CertificateExpired => {
            "the certificate expired and could not be renewed. Wallets cannot connect.".into()
        }
        other => indexer_text(other, None),
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
            format!(
                "Setting up · {}",
                backend_step_name(*step, status.backend).to_lowercase()
            ),
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
                other => indexer_line(other, status).unwrap_or_else(|| {
                    format!("Serving with a problem · {}", degraded_text(other, status))
                }),
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

/// Whether the Serving panel offers to try the certificate again: the last order failed, or the
/// certificate expired.
pub fn cert_needs_retry(status: &PublicStatus, now: SystemTime) -> bool {
    status.cert.as_ref().is_some_and(|cert| {
        cert.last_problem.is_some() || cert.not_after.is_some_and(|not_after| not_after <= now)
    })
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
                degraded_text(&DegradedReason::CertificateExpired, status)
            ),
        )),
        PublicState::Degraded { why } => Some((
            Tone::Warn,
            format!("Light client server: {}", degraded_text(why, status)),
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

/// A step's short name for the service's backend: zingod's Service step is zingod's.
pub fn backend_step_name(step: StepId, backend: LightBackend) -> &'static str {
    match (step, backend) {
        (StepId::Service, LightBackend::Zingod) => "zingod",
        _ => step_name(step),
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
        StepId::Service if status.backend == LightBackend::Zingod => format!(
            "zingod serves light wallets on this computer, reading Zebra · {}",
            network_name(status.network)
        ),
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
        CheckId::ZingodTested => "A tested zingod and Zebra",
        CheckId::UpgradesKnown => "The network upgrades",
        CheckId::IndexSpace => "Room for zingod's index",
        CheckId::IndexReady => "zingod's index",
        CheckId::TipMatches => "zingod's tip",
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

/// What a check found, with the fix first. The status, when at hand, names the network and
/// the index's folder in zingod's sentences.
pub fn finding_text(finding: &Finding, status: Option<&PublicStatus>) -> String {
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
        Finding::UntestedZingod {
            version,
            zebrad,
            network,
        } => format!(
            "zingod {version} isn't tested with Zebra {zebrad} on {} yet. Choose Zebra's built-in \
             server, or wait for a Zeal update.",
            network_name(*network)
        ),
        Finding::UpgradeUnknown { upgrade, height } => {
            let at = height
                .map(|height| format!(" since block {}", thousands(height)))
                .unwrap_or_default();
            format!(
                "zingod doesn't know {upgrade}, active{at}. It can't serve until Zeal updates \
                 zingod."
            )
        }
        Finding::IndexSpace {
            path,
            free_bytes,
            need_bytes,
        } => format!(
            "{} free in {}: zingod's index needs {} there. Free some space, or choose Zebra's \
             built-in server.",
            bytes(*free_bytes),
            path.display(),
            bytes(*need_bytes)
        ),
        Finding::NetworkFilesystem { path } => format!(
            "{} is on a network filesystem. zingod reads its index through memory maps and asks \
             for a local disk.",
            path.display()
        ),
        Finding::LowMemory {
            total_bytes,
            want_bytes,
        } => format!(
            "This computer has {} of memory, and zingod's build wants {}: it may be slow.",
            bytes(*total_bytes),
            bytes(*want_bytes)
        ),
        Finding::IndexBuilding { served, target, .. } if catching_up(*served, *target) => {
            "zingod is catching up with Zebra; wallets can use your server again soon.".into()
        }
        Finding::IndexBuilding {
            served,
            target,
            eta_secs,
        } => format!(
            "Building zingod's index: {}. Wallets can't use your server until this is done. \
             Quitting Zeal pauses it, and it goes on where it stopped.",
            build_progress(*served, *target, *eta_secs)
        ),
        Finding::ChainMismatch { at } => format!(
            "zingod's chain doesn't match Zebra's at block {}. Rebuild its index.",
            thousands(*at)
        ),
        Finding::IndexerPortTaken { ports } => format!(
            "Other programs use ports {}. Close one, then try again.",
            list_ports(ports)
        ),
        Finding::IndexerProblem { why } => indexer_text(why, status),
    }
}

/// A check's outcome as one line: its name and what it found.
pub fn check_line(check: CheckId, outcome: &CheckOutcome, status: Option<&PublicStatus>) -> String {
    match outcome {
        CheckOutcome::NotRun => format!("{} · not checked yet", check_name(check)),
        CheckOutcome::Running => format!("{} · checking…", check_name(check)),
        CheckOutcome::Pass => format!("{} · passed", check_name(check)),
        CheckOutcome::Warn { finding } | CheckOutcome::Fail { finding } => {
            format!("{} · {}", check_name(check), finding_text(finding, status))
        }
    }
}

/// The consent screen's lines for `backend`: what becomes public, for good, then what the user
/// takes on. With zingod, its lines take the place of Zebra's software, "experimental" and port
/// lines, and the index's and the public status's join them; nothing names Zebra's version.
pub fn consent_lines(status: &PublicStatus, backend: LightBackend) -> (Vec<String>, Vec<String>) {
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
    let software = match backend {
        LightBackend::ZebradEndpoint => {
            let version = status
                .zebrad_version
                .as_deref()
                .map(|version| version.trim_start_matches('v'))
                .map(|version| version.split('+').next().unwrap_or(version).to_owned())
                .unwrap_or_else(|| "its version".into());
            format!("Zebra {version}")
        }
        LightBackend::Zingod => format!("zingod {}", zingod_version(status)),
    };
    let mut public = vec![
        format!("{address}, under {name}. DNS history services keep it after you stop."),
        format!(
            "{sentence}, in public certificate logs, minutes after Zeal gets its certificate. \
             Scanners read those logs, so expect connection attempts right away."
        ),
        format!(
            "That this computer runs Zcash software: every wallet that connects is told it is \
             {software}."
        ),
    ];
    let experimental = match backend {
        LightBackend::ZebradEndpoint => "Zebra's light wallet server is experimental: wallets may \
                                         not work with it yet, and a Zebra update may change it."
            .to_owned(),
        LightBackend::Zingod => "zingod is experimental: Zingo Labs say it is in development and \
                                 not for production Mainnet use. They publish no binaries, so \
                                 Zeal builds it from their source."
            .to_owned(),
    };
    let port = match backend {
        LightBackend::ZebradEndpoint => "While the light client server is switched on, including \
                                         during setup and automatic pauses, programs on this \
                                         computer can use Zebra's light wallet port without a \
                                         password. Web pages in a browser can't."
            .to_owned(),
        LightBackend::Zingod => "While the light client server is switched on, including during \
                                 setup and automatic pauses, programs on this computer can use \
                                 zingod's port without a password. Web pages in a browser can't \
                                 use its wallet methods, but they can read its public status."
            .to_owned(),
    };
    let mut yours = vec![experimental];
    if backend == LightBackend::Zingod {
        public.push(
            "While it serves, your server's status page is public: its state, its version, its \
             block heights, its alarms (one says when Zebra's release reaches its end of support \
             within a week) and how much it sent in each 10 minutes."
                .to_owned(),
        );
        let place = status.indexer.as_ref().map_or_else(
            || format!("beside {}'s data", zebra_name(status.network)),
            |indexer| format!("in {}", indexer.dir.display()),
        );
        yours.push(format!(
            "zingod keeps its own index of the chain {place}, and needs {} free there to build \
             it. The build can take hours.",
            bytes(space_needed(status.network))
        ));
    }
    yours.extend([
        "Wallets that use your server show it their address, the blocks they download, the \
         transparent addresses they look up and the transactions they send. Zeal keeps no \
         client addresses, but anyone who controls this computer could."
            .to_owned(),
        port,
        "Your internet plan may not allow servers, and the traffic counts toward any data cap. \
         Check its terms, and your local law."
            .to_owned(),
        "Zeal gets the certificate from Let's Encrypt, so you accept its Subscriber Agreement."
            .to_owned(),
        "Zeal talks to your router, your DNS provider and Let's Encrypt. No one else.".to_owned(),
    ]);
    (public, yours)
}

/// The consent screen's choice of server: each backend's label and hint, for `RadioChoice`.
/// No card says "recommended", and zingod's says nothing about Zingo wallets until the wallet
/// runs confirm it.
pub const BACKEND_CHOICES: [(LightBackend, &str, &str); 2] = [
    (
        LightBackend::ZebradEndpoint,
        "Zebra's built-in server · experimental",
        "Serves as soon as Zebra is at the tip, with no extra disk. Sends wallets no \
         transparent data.",
    ),
    (
        LightBackend::Zingod,
        "zingod, by Zingo Labs · experimental · bundled with Zeal",
        "The current light wallet protocol, with transparent data and fees. Builds its own \
         index first, which can take hours.",
    ),
];

/// Why zingod can't be chosen here, `None` when it can.
pub fn zingod_choice_text(choice: &ZingodChoice) -> Option<String> {
    match choice {
        ZingodChoice::Offered => None,
        ZingodChoice::NotBundled => Some("zingod runs on Linux only for now".into()),
        ZingodChoice::NotOnNetwork { .. } => Some("zingod is being tested on Testnet first".into()),
        ZingodChoice::Untested {
            version,
            zebrad,
            network,
        } => Some(format!(
            "zingod {version} isn't tested with Zebra {zebrad} on {} yet",
            network_name(*network)
        )),
    }
}

/// zingod's card's extra line on the consent screen: why it is disabled, or what it needs.
pub fn zingod_choice_note(status: &PublicStatus) -> String {
    zingod_choice_text(&status.zingod_choice).unwrap_or_else(|| {
        format!(
            "Needs {} free beside {}'s data.",
            bytes(space_needed(status.network)),
            zebra_name(status.network)
        )
    })
}

/// The Service step's line for one of zingod's checks: what passed, or what it found.
pub fn service_check_line(check: &CheckResult, status: &PublicStatus) -> String {
    if let CheckOutcome::Warn { finding } | CheckOutcome::Fail { finding } = &check.outcome {
        return finding_text(finding, Some(status));
    }
    let zebra = zebra_name(status.network);
    let version = zingod_version(status);
    match check.id {
        CheckId::ZingodTested => {
            let zebrad = status
                .zebrad_version
                .as_deref()
                .map(|version| version.trim_start_matches('v'))
                .map(|version| version.split('+').next().unwrap_or(version).to_owned())
                .unwrap_or_default();
            format!("{zebra} {zebrad} with zingod {version} is a pair Zeal tested")
        }
        CheckId::UpgradesKnown => format!("zingod knows every upgrade {zebra} lists"),
        CheckId::IndexSpace => match status.indexer.as_ref() {
            Some(IndexerStats {
                free_bytes: Some(free),
                dir,
                built: true,
                ..
            }) => format!(
                "{} free in {}, where zingod's index is built",
                bytes(*free),
                dir.display()
            ),
            Some(IndexerStats {
                free_bytes: Some(free),
                dir,
                ..
            }) => format!(
                "{} free in {}: zingod's index needs {} there",
                bytes(*free),
                dir.display(),
                bytes(space_needed(status.network))
            ),
            _ => "Room for zingod's index".into(),
        },
        CheckId::AtTip => format!("{zebra} is at the tip"),
        CheckId::EndpointOpened => match check.outcome {
            CheckOutcome::Pass => format!("zingod {version} is running, reading {zebra}"),
            CheckOutcome::Running => format!("Starting zingod {version}, reading {zebra}"),
            _ => format!("zingod {version} isn't running"),
        },
        CheckId::IndexReady => "zingod serves at Zebra's tip".into(),
        CheckId::TipMatches => "zingod's tip matches Zebra's".into(),
        CheckId::WalletCheck => "zingod answers the wallet check".into(),
        other => check_name(other).to_owned(),
    }
}

/// A check's mark in a step's list: under way for zebrad syncing and zingod's build, which
/// the setup goes on through, and `!` for another warning.
pub fn check_mark(outcome: &CheckOutcome) -> &'static str {
    match outcome {
        CheckOutcome::Pass => "✓",
        CheckOutcome::Warn {
            finding: Finding::Syncing { .. } | Finding::IndexBuilding { .. },
        }
        | CheckOutcome::Running => "●",
        CheckOutcome::Warn { .. } => "!",
        CheckOutcome::NotRun => "○",
        CheckOutcome::Fail { .. } => "✕",
    }
}

/// The last error zingod's exit gave, under the line of a zingod that stopped, crashed or broke
/// its index, when that line doesn't carry it already: the exit's text, which the reader
/// redacted. A zingod that runs and says it is down has its own message in its line instead.
pub fn last_error_line(why: &DegradedReason, status: &PublicStatus) -> Option<String> {
    let shown = match why {
        DegradedReason::IndexerFailed { detail, .. } => matches!(
            detail,
            FailDetail::None | FailDetail::Ports { .. } | FailDetail::Dir { .. }
        ),
        DegradedReason::IndexerStarting {
            waiting: IndexerWait::Backoff,
        }
        | DegradedReason::IndexBroken => true,
        _ => false,
    };
    let detail = status
        .indexer
        .as_ref()?
        .last_exit
        .as_ref()?
        .detail
        .as_deref()?
        .trim();
    (shown && !detail.is_empty()).then(|| format!("zingod's last error: {detail}"))
}

/// A check's mark in words, for screen readers, which skip the mark: its state before the
/// check's line, as [`check_mark`] shows it.
pub fn check_state_word(outcome: &CheckOutcome) -> &'static str {
    match outcome {
        CheckOutcome::Pass => "Passed: ",
        CheckOutcome::Warn {
            finding: Finding::Syncing { .. } | Finding::IndexBuilding { .. },
        }
        | CheckOutcome::Running => "Under way: ",
        CheckOutcome::Warn { .. } => "Warning: ",
        CheckOutcome::NotRun => "Not checked yet: ",
        CheckOutcome::Fail { .. } => "Failed: ",
    }
}

/// What the panel offers beside a zingod problem's line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexerAction {
    OpenLog,
    TryAgain,
    Rebuild,
    CopyDiagnostics,
}

impl IndexerAction {
    pub fn label(self) -> &'static str {
        match self {
            IndexerAction::OpenLog => "Open the log",
            IndexerAction::TryAgain => "Try again",
            IndexerAction::Rebuild => "Rebuild…",
            IndexerAction::CopyDiagnostics => "Copy diagnostics",
        }
    }
}

/// The buttons a zingod problem gets: the log for what zingod said, Try again for what the user
/// fixes, Rebuild for an index that can't be used, the diagnostics for Zeal's own bug.
pub fn indexer_actions(why: &DegradedReason) -> &'static [IndexerAction] {
    use IndexerAction::*;
    match why {
        DegradedReason::IndexerStuck { .. } | DegradedReason::IndexerDown { .. } => &[OpenLog],
        DegradedReason::IndexBroken | DegradedReason::ChainMismatch { .. } => &[Rebuild],
        DegradedReason::IndexerFailed {
            class: ExitClass::ConfigError,
            detail: FailDetail::None,
        } => &[CopyDiagnostics],
        // zingod never ran: its log has nothing about the folder.
        DegradedReason::IndexerFailed {
            detail: FailDetail::Folder { .. },
            ..
        } => &[TryAgain],
        DegradedReason::IndexerFailed {
            class: ExitClass::Crash | ExitClass::SpawnFailed,
            ..
        } => &[OpenLog, TryAgain],
        DegradedReason::IndexerFailed { .. } => &[TryAgain],
        _ => &[],
    }
}

/// The Server card's lines: zingod's version and state, the heights, the index, memory, CPU and
/// what it read and wrote since it started.
pub fn server_lines(status: &PublicStatus) -> Vec<String> {
    let Some(indexer) = &status.indexer else {
        return Vec::new();
    };
    let state = match (&indexer.phase, indexer.status.as_ref().map(|s| &s.state)) {
        (Phase::Running, Some(IndexerState::Serving)) => "serving",
        (Phase::Running, Some(IndexerState::Syncing { .. })) => "building its index",
        (Phase::Running, Some(IndexerState::Provisioning)) | (Phase::Starting, _) => "starting",
        (Phase::Running, Some(IndexerState::Degraded { .. })) => "serving, degraded",
        (Phase::Running, Some(_)) => "down",
        (Phase::Running, None) => "starting",
        (Phase::Backoff { .. }, _) => "restarting after a crash",
        (Phase::Failed { .. }, _) => "stopped, waiting for you",
        (Phase::Stopping, _) => "stopping",
        (Phase::Stopped, _) => "stopped",
    };
    let mut lines = vec![format!("zingod {} · {state}", indexer.version)];
    let zebra = status.light.as_ref().and_then(|light| light.height);
    let served = indexer.status.as_ref().and_then(|status| status.served);
    match (served, zebra) {
        (Some(served), Some(zebra)) => lines.push(format!(
            "block {} · Zebra {}",
            thousands(served),
            thousands(zebra)
        )),
        (None, Some(zebra)) => lines.push(format!("Zebra at block {}", thousands(zebra))),
        _ => {}
    }
    let index = match indexer.index_bytes {
        Some(size) if indexer.index_partial => format!("index at least {}", bytes(size)),
        Some(size) => format!("index {}", bytes(size)),
        None => "no index yet".into(),
    };
    lines.push(format!("{index} in {}", indexer.dir.display()));
    if let Some(resources) = indexer.resources {
        let disk = if resources.read_bytes > 0 || resources.written_bytes > 0 {
            format!(
                " · {} read, {} written",
                bytes(resources.read_bytes),
                bytes(resources.written_bytes)
            )
        } else {
            String::new()
        };
        lines.push(format!(
            "{} memory · {} % CPU{disk}",
            bytes(resources.rss_bytes),
            resources.cpu_millicores / 10
        ));
    }
    lines
}

/// The line under the panel's cards: Zebra's height, the backend's address and the front door.
pub fn server_footer(status: &PublicStatus) -> Option<String> {
    let indexer = status.indexer.as_ref()?;
    let zebra = status
        .light
        .as_ref()
        .and_then(|light| light.height)
        .map(|height| format!("Zebra at block {}", thousands(height)))
        .unwrap_or_else(|| "Zebra".into());
    let door = if status.gateway.is_some() {
        "front door running"
    } else {
        "front door closed"
    };
    Some(format!(
        "{zebra} · zingod 127.0.0.1:{} · {door}",
        indexer.port
    ))
}

/// The index kept after switching back to Zebra's built-in server, with Delete beside it.
pub fn kept_index_line(status: &PublicStatus) -> Option<String> {
    let indexer = status.indexer.as_ref()?;
    let size = indexer.index_bytes.filter(|bytes| *bytes > 0)?;
    (status.backend == LightBackend::ZebradEndpoint).then(|| {
        format!(
            "zingod's index is still on disk: {} in {}. Switching back to zingod is quick while \
             it is there.",
            bytes(size),
            indexer.dir.display()
        )
    })
}

/// "Turn off and forget"'s box for zingod's index, ticked: the index holds nothing of the
/// user's and rebuilds from Zebra's chain.
pub fn forget_index_label(status: &PublicStatus) -> Option<String> {
    let indexer = status.indexer.as_ref()?;
    let size = indexer
        .index_bytes
        .map(|size| format!("{} in ", bytes(size)))
        .unwrap_or_default();
    Some(format!(
        "Delete zingod's index ({size}{})",
        indexer.dir.display()
    ))
}

/// Above the consent screen that "Server software…" opens.
pub const CHOOSE_AGAIN: &str = "Choose the server that answers wallets. What becomes public \
                                depends on it, so Zeal asks you to agree again.";

/// What "Turn off and forget" does, for the service's backend.
pub fn forget_text(backend: LightBackend) -> &'static str {
    match backend {
        LightBackend::ZebradEndpoint => {
            "Zeal closes the front door, removes Zebra's light wallet endpoint and forgets the \
             setup."
        }
        LightBackend::Zingod => "Zeal closes the front door, stops zingod and forgets the setup.",
    }
}

/// What Rebuild costs, before the button that does it.
pub fn rebuild_text(status: &PublicStatus) -> String {
    let place = status
        .indexer
        .as_ref()
        .map(|indexer| format!(" in {}", indexer.dir.display()))
        .unwrap_or_default();
    format!(
        "Rebuilding deletes zingod's index{place} and builds it again from Zebra's chain. That \
         takes hours, and wallets are told to try again meanwhile."
    )
}

/// The Share section's last words: what the wallet needs, and which server it reaches.
pub fn share_note(backend: LightBackend) -> &'static str {
    match backend {
        LightBackend::ZebradEndpoint => {
            "The wallet must know NU7, and Zodl's Automatic mode also sends transactions to its \
             other servers. This is Zebra's experimental light wallet server."
        }
        LightBackend::Zingod => {
            "The wallet must know NU7, and Zodl's Automatic mode also sends transactions to its \
             other servers. This is zingod, an experimental light wallet server."
        }
    }
}

/// What the serving zebrad's header says while zingod reads it.
pub const ZINGOD_READS: &str = "zingod reads this node";

/// What Stop and Restart say on the zebrad zingod reads.
pub const ZINGOD_RESTARTS: &str = "zingod restarts with Zebra and waits for it to reach the tip: \
                                   wallets are told to try again meanwhile";

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
            backend: Default::default(),
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
        assert!(
            cert_needs_retry(&status, now),
            "the panel offers to try again"
        );
        // Without a failure, an expiry is the renewal's business.
        status.cert.as_mut().unwrap().last_problem = None;
        assert_eq!(banner(&status, now), None);
        assert!(!cert_needs_retry(&status, now));
        // An expired certificate can be tried again too.
        assert!(cert_needs_retry(&status, at(NOW + 11 * 86_400)));
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
            let text = finding_text(finding, None);
            assert!(text.ends_with('.'), "{text}");
        }
        assert!(finding_text(&findings[0], None).contains("carrier-grade NAT, 100.72.4.9"));
        assert!(finding_text(&findings[3], None).contains("block 2,900,000 of 3,544,812"));
    }

    #[test]
    fn every_zingod_problem_has_its_panel_line() {
        use std::path::PathBuf;

        use launcher_types::{
            Phase,
            public::{DownReason, IndexerStats},
        };
        let mut status = base();
        status.network = Network::Testnet;
        status.backend = LightBackend::Zingod;
        status.indexer = Some(IndexerStats {
            version: "1.1.0".into(),
            phase: Phase::Running,
            status: None,
            dir: PathBuf::from("/mnt/zebra/zingod/testnet"),
            built: false,
            port: 18137,
            index_bytes: None,
            index_partial: false,
            free_bytes: None,
            resources: None,
            last_exit: None,
        });
        let cases = [
            (
                DegradedReason::BackingRestarting,
                "Catching up · zingod restarts with Zebra",
            ),
            (
                DegradedReason::IndexerStarting {
                    waiting: IndexerWait::ZebradTip,
                },
                "Waiting for Zebra · Testnet to reach the tip: zingod starts then",
            ),
            (
                DegradedReason::IndexerStarting {
                    waiting: IndexerWait::Boot,
                },
                "Catching up · zingod is starting",
            ),
            (
                DegradedReason::IndexBuilding {
                    served: Some(1_204_331),
                    target: Some(4_612_000),
                    eta_secs: Some(5 * 3600),
                },
                "Building zingod's index: block 1,204,331 of 4,612,000 (26 %), about 5 h left",
            ),
            (
                DegradedReason::IndexBuilding {
                    served: None,
                    target: None,
                    eta_secs: None,
                },
                "Building zingod's index: an hour or more",
            ),
            (
                DegradedReason::IndexBuilding {
                    served: Some(4_611_990),
                    target: Some(4_612_000),
                    eta_secs: None,
                },
                "Catching up · wallets are told to try again",
            ),
            (
                DegradedReason::IndexBehind { blocks: 12 },
                "Catching up · zingod is 12 blocks behind Zebra",
            ),
            (
                DegradedReason::IndexerDown {
                    why: DownReason::NoValidator,
                },
                "Catching up · zingod can't reach Zebra · Testnet",
            ),
            (
                DegradedReason::IndexerStuck {
                    at: 4_611_002,
                    zebrad_at: 4_611_019,
                },
                "zingod is stuck at block 4,611,002; Zebra is at 4,611,019.",
            ),
            (
                DegradedReason::IndexerFailed {
                    class: ExitClass::PortInUse,
                    detail: FailDetail::Ports {
                        tried: vec![18137, 18138],
                    },
                },
                "Other programs use ports 18137 and 18138.",
            ),
            (
                DegradedReason::IndexerFailed {
                    class: ExitClass::ConfigError,
                    detail: FailDetail::Cookie {
                        path: PathBuf::from("/mnt/zebra/testnet/rpc/.cookie"),
                        error: "Is a directory (os error 21)".into(),
                    },
                },
                "zingod can't read Zebra · Testnet's cookie at /mnt/zebra/testnet/rpc/.cookie: \
                 Is a directory (os error 21).",
            ),
            (
                DegradedReason::IndexerFailed {
                    class: ExitClass::SpawnFailed,
                    detail: FailDetail::Folder {
                        path: PathBuf::from("/mnt/zebra/zingod/testnet/home"),
                        error: "Permission denied (os error 13)".into(),
                    },
                },
                "Zeal could not set up zingod's folder /mnt/zebra/zingod/testnet/home: \
                 Permission denied (os error 13). Fix its permissions or its disk, then try again.",
            ),
            (
                DegradedReason::ChainMismatch { at: 4_611_980 },
                "zingod's chain doesn't match Zebra's at block 4,611,980.",
            ),
            (
                DegradedReason::DiskLow {
                    free_bytes: 6_000_000_000,
                },
                "Waiting for disk space: 6.0 GB free for zingod's index in \
                 /mnt/zebra/zingod/testnet. zingod starts again above 20.0 GB.",
            ),
            (
                DegradedReason::IndexSpace {
                    free_bytes: 30_000_000_000,
                    need_bytes: 45_000_000_000,
                },
                "Waiting for room: zingod needs 45.0 GB free in /mnt/zebra/zingod/testnet to \
                 build its index, and 30.0 GB is free. It starts once there is room.",
            ),
        ];
        for (why, line) in cases {
            status.state = PublicState::Degraded { why: why.clone() };
            assert_eq!(state_line(&status, at(NOW)), (Tone::Warn, line.to_owned()));
            let (_, banner) = banner(&status, at(NOW)).expect("a banner");
            assert!(banner.starts_with("Light client server: "), "{banner}");
        }
        status.state = PublicState::Paused {
            why: PauseReason::BuildTooOld {
                upgrade: "NU8".into(),
                height: 4_700_000,
            },
            since: at(NOW),
            resumes: launcher_types::public::Resume::Automatic,
        };
        assert_eq!(
            state_line(&status, at(NOW)).1,
            "Paused · zingod 1.1.0 doesn't know NU8, which is active. Wallets that use your \
             server see an error until Zeal updates zingod."
        );
        // The wallet check names the server that failed it.
        status.state = PublicState::Degraded {
            why: DegradedReason::WalletCheckFailed,
        };
        assert_eq!(
            state_line(&status, at(NOW)).1,
            "Serving with a problem · zingod gave a wrong answer to the wallet check. Wallets \
             are told to try again."
        );
        // zebrad's own reasons read as before with zingod.
        status.state = PublicState::Degraded {
            why: DegradedReason::BackingBehind { blocks: 3 },
        };
        assert!(
            state_line(&status, at(NOW))
                .1
                .starts_with("Catching up · wallets")
        );
        status.backend = LightBackend::ZebradEndpoint;
        status.state = PublicState::Degraded {
            why: DegradedReason::BackingRestarting,
        };
        assert_eq!(
            state_line(&status, at(NOW)).1,
            "Serving with a problem · Zebra is restarting. Wallets are told to try again."
        );
        status.state = PublicState::Degraded {
            why: DegradedReason::WalletCheckFailed,
        };
        assert!(
            state_line(&status, at(NOW))
                .1
                .contains("Zebra's light wallet endpoint gave a wrong answer"),
        );
    }

    #[test]
    fn zingods_findings_have_words() {
        use std::path::PathBuf;
        let findings = [
            Finding::UntestedZingod {
                version: "1.1.0".into(),
                zebrad: "7.1.0".into(),
                network: Network::Testnet,
            },
            Finding::UpgradeUnknown {
                upgrade: "NU8".into(),
                height: Some(4_700_000),
            },
            Finding::IndexSpace {
                path: PathBuf::from("/mnt/zebra/zingod/testnet"),
                free_bytes: 12_000_000_000,
                need_bytes: 50_000_000_000,
            },
            Finding::NetworkFilesystem {
                path: PathBuf::from("/mnt/nas/zingod/testnet"),
            },
            Finding::LowMemory {
                total_bytes: 4_000_000_000,
                want_bytes: 8_000_000_000,
            },
            Finding::IndexBuilding {
                served: Some(100),
                target: Some(200_000),
                eta_secs: None,
            },
            Finding::ChainMismatch { at: 7 },
            Finding::IndexerPortTaken {
                ports: vec![18137, 18138, 18139],
            },
            Finding::IndexerProblem {
                why: DegradedReason::IndexBroken,
            },
        ];
        for finding in &findings {
            let text = finding_text(finding, None);
            assert!(text.ends_with('.'), "{text}");
        }
        assert!(finding_text(&findings[2], None).starts_with(
            "12.0 GB free in /mnt/zebra/zingod/testnet: zingod's index needs 50.0 GB"
        ));
        assert!(finding_text(&findings[7], None).contains("ports 18137, 18138 and 18139"));
    }

    #[test]
    fn consent_names_the_real_things() {
        let mut status = base();
        status.zebrad_version = Some("v7.0.0+git.e8db87e".into());
        let (public, yours) = consent_lines(&status, LightBackend::ZebradEndpoint);
        assert!(
            public[1].starts_with("The name zec-alice.duckdns.org, in")
                && public[0].contains("under zec-alice.duckdns.org"),
            "{public:?}"
        );
        assert!(public[2].ends_with("Zebra 7.0.0."), "{public:?}");
        assert!(yours.iter().any(|line| line.contains("without a password")));
    }

    /// zingod's consent tells what zingod sends and makes public: no Zebra version, no Zebra
    /// port outside Z4's switch, its index and its public status page with the egress.
    #[test]
    fn zingods_consent_names_zingod_and_no_zebra_version_or_port() {
        let mut status = base();
        status.network = Network::Testnet;
        status.zebrad_version = Some("v7.0.0+git.e8db87e".into());
        let (public, yours) = consent_lines(&status, LightBackend::Zingod);
        let all = public
            .iter()
            .chain(&yours)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            public[2].ends_with(&format!(
                "told it is zingod {}.",
                launcher_types::zingod::version()
            )),
            "{public:?}"
        );
        assert!(!all.contains("7.0.0"), "no Zebra version: {all}");
        assert!(!all.contains("Zebra's light wallet port"), "{all}");
        assert!(all.contains("zingod's port without a password"), "{all}");
        assert!(
            all.contains("including during setup and automatic pauses"),
            "{all}"
        );
        assert!(
            all.contains("can't use its wallet methods, but they can read its public status"),
            "{all}"
        );
        assert!(
            public
                .iter()
                .any(|line| line.contains("how much it sent in each 10 minutes")),
            "the egress on the public status page: {public:?}"
        );
        assert!(
            public.iter().any(|line| line.contains("its alarms")
                && line.contains("Zebra's release reaches its end of support")),
            "the alarm that dates Zebra's release: {public:?}"
        );
        assert!(
            yours
                .iter()
                .any(|line| line.starts_with("zingod keeps its own index"))
        );
        assert!(yours[0].starts_with("zingod is experimental"), "{yours:?}");
        assert!(!all.contains("recommended"));
        assert!(
            !all.contains("Zingo wallet"),
            "the Zingo sentence waits for the wallet runs"
        );
        // Zebra's consent is PS1's, word for word.
        let (zebra_public, zebra_yours) = consent_lines(&status, LightBackend::ZebradEndpoint);
        assert_eq!(zebra_public.len(), 3);
        assert_eq!(zebra_yours.len(), 6);
    }

    /// zingod's Service step words each check by its state, for the eye and for a screen
    /// reader: a check that hasn't passed never reads as one that has.
    #[test]
    fn zingods_check_lines_follow_their_state() {
        let mut status = base();
        status.network = Network::Testnet;
        status.backend = LightBackend::Zingod;
        let line = |outcome| {
            let check = CheckResult {
                id: CheckId::EndpointOpened,
                outcome,
                at: None,
            };
            format!(
                "{}{}",
                check_state_word(&check.outcome),
                service_check_line(&check, &status)
            )
        };
        let version = launcher_types::zingod::version();
        assert_eq!(
            line(CheckOutcome::Pass),
            format!("Passed: zingod {version} is running, reading Zebra · Testnet")
        );
        assert_eq!(
            line(CheckOutcome::Running),
            format!("Under way: Starting zingod {version}, reading Zebra · Testnet")
        );
        assert_eq!(
            line(CheckOutcome::NotRun),
            format!("Not checked yet: zingod {version} isn't running")
        );
        assert_eq!(
            check_state_word(&CheckOutcome::Warn {
                finding: Finding::IndexBuilding {
                    served: None,
                    target: None,
                    eta_secs: None,
                }
            }),
            "Under way: "
        );
        assert_eq!(
            check_state_word(&CheckOutcome::Warn {
                finding: Finding::NoHairpin
            }),
            "Warning: "
        );
        assert_eq!(
            check_state_word(&CheckOutcome::Fail {
                finding: Finding::ChainMismatch { at: 7 }
            }),
            "Failed: "
        );
    }

    #[test]
    fn the_choice_says_why_zingod_is_disabled() {
        assert_eq!(zingod_choice_text(&ZingodChoice::Offered), None);
        assert_eq!(
            zingod_choice_text(&ZingodChoice::NotOnNetwork {
                network: Network::Mainnet
            })
            .as_deref(),
            Some("zingod is being tested on Testnet first")
        );
        assert_eq!(
            zingod_choice_text(&ZingodChoice::NotBundled).as_deref(),
            Some("zingod runs on Linux only for now")
        );
        assert!(
            BACKEND_CHOICES
                .iter()
                .all(|(_, label, hint)| !label.contains("recommended")
                    && !hint.contains("recommended")
                    && !hint.contains("Zingo wallet"))
        );
    }

    #[test]
    fn the_server_card_reads_the_indexer() {
        use std::path::PathBuf;

        use launcher_types::public::{IndexerResources, IndexerStats, LightStats};
        let mut status = base();
        status.backend = LightBackend::Zingod;
        status.light = Some(LightStats {
            endpoint: None,
            height: Some(4_612_019),
            tip: Some(4_612_019),
            info: None,
            checked_at: None,
            gateway: Default::default(),
            month_bytes_up: 0,
        });
        status.indexer = Some(IndexerStats {
            version: "1.1.0".into(),
            phase: Phase::Running,
            status: Some(launcher_types::zingod::IndexerStatus {
                state: IndexerState::Serving,
                served: Some(4_612_019),
                tip: Some(4_612_019),
                network: "testnet".into(),
                trusted: Some(1),
                answering: Some(1),
                alarms: Vec::new(),
                version: "1.1.0".into(),
            }),
            dir: PathBuf::from("/mnt/zebra/zingod/testnet"),
            built: true,
            port: 18137,
            index_bytes: Some(18_400_000_000),
            index_partial: false,
            free_bytes: Some(412_000_000_000),
            resources: Some(IndexerResources {
                rss_bytes: 1_400_000_000,
                cpu_millicores: 30,
                read_bytes: 0,
                written_bytes: 0,
            }),
            last_exit: None,
        });
        assert_eq!(
            server_lines(&status),
            [
                "zingod 1.1.0 · serving",
                "block 4,612,019 · Zebra 4,612,019",
                "index 18.4 GB in /mnt/zebra/zingod/testnet",
                "1.4 GB memory · 3 % CPU",
            ]
        );
        if let Some(resources) = status
            .indexer
            .as_mut()
            .and_then(|indexer| indexer.resources.as_mut())
        {
            resources.read_bytes = 2_100_000_000;
            resources.written_bytes = 18_900_000_000;
        }
        assert_eq!(
            server_lines(&status)[3],
            "1.4 GB memory · 3 % CPU · 2.1 GB read, 18.9 GB written"
        );
        // The last exit's text, under the line that doesn't name it.
        if let Some(indexer) = status.indexer.as_mut() {
            indexer.phase = Phase::Failed {
                class: ExitClass::Crash,
            };
            indexer.last_exit = Some(launcher_types::ExitInfo {
                code: Some(101),
                class: ExitClass::Crash,
                detail: Some("task ended before shutdown".into()),
                at: at(NOW),
                requested: false,
            });
        }
        let crash = DegradedReason::IndexerFailed {
            class: ExitClass::Crash,
            detail: FailDetail::None,
        };
        assert_eq!(
            last_error_line(&crash, &status).as_deref(),
            Some("zingod's last error: task ended before shutdown")
        );
        assert!(
            last_error_line(
                &DegradedReason::IndexerStarting {
                    waiting: IndexerWait::Backoff
                },
                &status
            )
            .is_some()
        );
        let cookie = DegradedReason::IndexerFailed {
            class: ExitClass::ConfigError,
            detail: FailDetail::Cookie {
                path: PathBuf::from("/mnt/zebra/testnet/rpc/.cookie"),
                error: "Is a directory (os error 21)".into(),
            },
        };
        assert_eq!(
            last_error_line(&cookie, &status),
            None,
            "its line names the error already"
        );
        assert_eq!(
            last_error_line(
                &DegradedReason::IndexerDown {
                    why: DownReason::NoAnswer
                },
                &status
            ),
            None,
            "a running zingod's down isn't an earlier exit's"
        );
        if let Some(indexer) = status.indexer.as_mut() {
            indexer.phase = Phase::Running;
            indexer.last_exit = None;
        }
        assert_eq!(
            forget_index_label(&status).as_deref(),
            Some("Delete zingod's index (18.4 GB in /mnt/zebra/zingod/testnet)")
        );
        assert_eq!(kept_index_line(&status), None, "zingod still serves");
        status.backend = LightBackend::ZebradEndpoint;
        assert!(
            kept_index_line(&status).is_some_and(|line| line.contains("18.4 GB")),
            "after switching back"
        );
        assert_eq!(
            indexer_actions(&DegradedReason::IndexBroken),
            &[IndexerAction::Rebuild]
        );
        assert_eq!(
            indexer_actions(&DegradedReason::IndexerFailed {
                class: ExitClass::ConfigError,
                detail: FailDetail::None,
            }),
            &[IndexerAction::CopyDiagnostics]
        );
        assert_eq!(
            indexer_actions(&DegradedReason::IndexerFailed {
                class: ExitClass::SpawnFailed,
                detail: FailDetail::Folder {
                    path: PathBuf::from("/mnt/zebra/zingod/testnet/home"),
                    error: "Permission denied (os error 13)".into(),
                },
            }),
            &[IndexerAction::TryAgain],
            "zingod's log has nothing about a folder Zeal couldn't create"
        );
    }
}
