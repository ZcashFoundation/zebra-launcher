//! The public services' rules (docs/PUBLIC-SERVICES.md): from a service's stored spec and the
//! facts the backend gathered ([`Facts`]), the setup's steps ([`plan`]), automatic pauses
//! ([`pause`]), the state, the front door's mode and the status the UI shows ([`status`]).
//!
//! The backend's service actor gathers the facts and acts on the result; the browser mock
//! builds facts for each `?public=` flag and calls the same functions.

pub mod evidence;
pub mod firewall;
pub mod light;
pub mod names;
pub mod pause;
pub mod plan;
pub mod share;
pub mod wallet;

use std::{net::SocketAddr, time::SystemTime};

use launcher_types::{
    Network, Phase,
    public::{
        CertStatus, DnsStatus, Evidence, FirewallInfo, GatewayMode, GatewayStats, LightStats,
        LightdInfo, LocalTestProblem, NetFacts, PublicServiceSpec, PublicState, PublicStatus,
        StepId, WalletProblem,
    },
};

pub use pause::{degraded, pause};
pub use plan::{current_step, open_gates, plan};

/// The zebrad versions Zeal has tested as a light client server.
pub const TESTED_ZEBRAD: [&str; 1] = ["7.0.0"];

/// Whether `version` (as zebrad reports it, maybe with `+git…` build metadata) is on
/// [`TESTED_ZEBRAD`].
pub fn zebrad_tested(version: &str) -> bool {
    let version = version.trim().trim_start_matches('v');
    let release = version.split(['+', ' ']).next().unwrap_or(version);
    TESTED_ZEBRAD.contains(&release)
}

/// A result with when it was produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checked<T> {
    pub at: SystemTime,
    pub result: T,
}

/// What the backend knows about a service's surroundings at one moment: the input of every
/// rule here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Facts {
    pub now: SystemTime,
    /// The platform runs public services: Linux in PS1.
    pub supported: bool,
    /// The zebrad the launcher runs, as it reports its version.
    pub zebrad_version: Option<String>,
    /// The network the launcher runs zebrad on.
    pub network: Network,
    pub phase: Phase,
    /// The light wallet endpoint zebrad logged as opened.
    pub endpoint: Option<SocketAddr>,
    /// zebrad's last start failed because the endpoint's port was taken.
    pub endpoint_port_taken: bool,
    /// Writing the endpoint into `zebrad.toml` and restarting zebrad failed, with why.
    pub endpoint_error: Option<String>,
    /// zebrad's height and its estimate of the network's.
    pub height: Option<u64>,
    pub tip: Option<u64>,
    /// zebrad's estimate of the time to the tip.
    pub eta_secs: Option<u64>,
    /// Since when zebrad has been at the tip without a break ([`light::track_since`]).
    pub at_tip_since: Option<SystemTime>,
    /// The last wallet check of the endpoint on loopback.
    pub wallet: Option<Checked<Result<LightdInfo, WalletProblem>>>,
    pub net: Option<NetFacts>,
    pub dns: Option<DnsStatus>,
    /// The keychain refused to keep a token.
    pub keychain_unavailable: bool,
    /// The front door's port: free or held by the front door (`true`), or taken.
    pub port_free: Option<bool>,
    pub firewall: Option<FirewallInfo>,
    pub cert: Option<CertStatus>,
    /// The test on this computer through the front door.
    pub local_test: Option<Checked<Result<LightdInfo, LocalTestProblem>>>,
    /// The hairpin test: a TLS connection to the public address, after Open.
    pub hairpin: Option<Checked<bool>>,
    pub evidence: Vec<Evidence>,
    /// Since when the name has resolved away from this network without a break.
    pub name_elsewhere_since: Option<SystemTime>,
    /// The upload this month, in bytes.
    pub month_bytes_up: u64,
    pub gateway: GatewayStats,
    /// The keep-awake inhibitor is held.
    pub awake_held: bool,
    /// A pause that only the user's switch ends: Zeal started with "resume when Zeal starts"
    /// off ([`PauseReason::NotResumed`]), or zebrad's network changed
    /// ([`PauseReason::NetworkChanged`]), which switching back does not undo by itself.
    ///
    /// [`PauseReason::NotResumed`]: launcher_types::public::PauseReason::NotResumed
    /// [`PauseReason::NetworkChanged`]: launcher_types::public::PauseReason::NetworkChanged
    pub held: Option<launcher_types::public::PauseReason>,
}

impl Facts {
    /// Facts at `now` for zebrad on `network`, with nothing known yet.
    pub fn new(now: SystemTime, network: Network) -> Self {
        Facts {
            now,
            supported: true,
            zebrad_version: None,
            network,
            phase: Phase::Stopped,
            endpoint: None,
            endpoint_port_taken: false,
            endpoint_error: None,
            height: None,
            tip: None,
            eta_secs: None,
            at_tip_since: None,
            wallet: None,
            net: None,
            dns: None,
            keychain_unavailable: false,
            port_free: None,
            firewall: None,
            cert: None,
            local_test: None,
            hairpin: None,
            evidence: Vec::new(),
            name_elsewhere_since: None,
            month_bytes_up: 0,
            gateway: GatewayStats::default(),
            awake_held: false,
            held: None,
        }
    }

    /// The zebrad version is one Zeal tested; an unknown version counts as tested until zebrad
    /// reports one, since the bundled zebrad is.
    pub fn zebrad_tested(&self) -> bool {
        self.zebrad_version.as_deref().is_none_or(zebrad_tested)
    }
}

/// The service's state from its spec and the facts. `paused_since` is when the current pause
/// began, which the backend keeps.
pub fn state(
    spec: &PublicServiceSpec,
    facts: &Facts,
    steps: &[launcher_types::public::Step],
    paused_since: SystemTime,
) -> PublicState {
    use launcher_types::public::NotAvailable;
    if !facts.supported {
        return PublicState::NotAvailable {
            why: NotAvailable::NotOnThisSystem,
        };
    }
    if !facts.zebrad_tested() {
        return PublicState::NotAvailable {
            why: NotAvailable::UntestedZebrad {
                version: facts.zebrad_version.clone().unwrap_or_default(),
            },
        };
    }
    if let Some((why, resumes)) = pause(spec, facts) {
        return PublicState::Paused {
            why,
            since: paused_since,
            resumes,
        };
    }
    if !spec.opened {
        return PublicState::SettingUp {
            step: current_step(steps).unwrap_or(StepId::Open),
        };
    }
    match degraded(spec, facts) {
        Some(why) => PublicState::Degraded { why },
        None => PublicState::Serving,
    }
}

/// What the front door accepts in `state`, `None` when it is stopped. Nothing reaches zebrad
/// from outside before the service is open: during the setup the front door answers Let's
/// Encrypt's challenge, and once there is a certificate, this computer.
pub fn gateway_mode(state: &PublicState, facts: &Facts) -> Option<GatewayMode> {
    use launcher_types::public::DegradedReason;
    let has_cert = facts
        .cert
        .as_ref()
        .and_then(|cert| cert.not_after)
        .is_some_and(|not_after| not_after > facts.now);
    match state {
        PublicState::Off | PublicState::NotAvailable { .. } | PublicState::Paused { .. } => None,
        PublicState::SettingUp { step } => match step {
            step if *step < StepId::Port => None,
            StepId::Port | StepId::Firewall | StepId::Certificate => {
                Some(GatewayMode::ChallengeOnly)
            }
            _ if has_cert => Some(GatewayMode::LocalOnly),
            _ => Some(GatewayMode::ChallengeOnly),
        },
        PublicState::Serving => Some(GatewayMode::Open),
        PublicState::Degraded {
            why: DegradedReason::CertificateExpired,
        } => Some(GatewayMode::ChallengeOnly),
        PublicState::Degraded { .. } => Some(GatewayMode::Unavailable),
    }
}

/// The status the UI shows for `spec`.
pub fn status(spec: &PublicServiceSpec, facts: &Facts, paused_since: SystemTime) -> PublicStatus {
    let steps = plan(spec, facts);
    let state = state(spec, facts, &steps, paused_since);
    let gateway = gateway_mode(&state, facts);
    let checked = facts
        .cert
        .as_ref()
        .is_some_and(|cert| cert.staging_validated_at.is_some() || cert.not_after.is_some())
        || spec.opened;
    PublicStatus {
        node: spec.node.clone(),
        service: spec.service,
        network: spec.network,
        switch_on: spec.switch_on,
        gateway,
        reach: evidence::reachability(&facts.evidence, checked, facts.now),
        steps,
        name: spec.name.clone(),
        provider: spec.provider,
        external_port: spec.external_port,
        internal_port: spec.internal_port,
        lan_addr: spec.lan_addr,
        consent: spec.consent.clone(),
        opened: spec.opened,
        keep_awake: spec.keep_awake,
        awake_held: facts.awake_held,
        limits: spec.limits,
        cert: facts.cert.clone(),
        net: facts.net.clone(),
        dns: facts.dns.clone(),
        light: Some(LightStats {
            endpoint: facts.endpoint,
            height: facts.height,
            tip: facts.tip,
            info: facts
                .wallet
                .as_ref()
                .and_then(|checked| checked.result.as_ref().ok().cloned()),
            checked_at: facts.wallet.as_ref().map(|checked| checked.at),
            gateway: facts.gateway.clone(),
            month_bytes_up: facts.month_bytes_up,
        }),
        firewall: facts.firewall.clone(),
        share: spec
            .name
            .as_ref()
            .map(|name| share::share(name, spec.external_port)),
        zebrad_version: facts.zebrad_version.clone(),
        state,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use launcher_types::{
        NodeId,
        public::{CONSENT_VERSION, Consent, DegradedReason, NotAvailable, PauseReason},
    };

    use super::*;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn spec() -> PublicServiceSpec {
        let mut spec = PublicServiceSpec::new_light_server(
            NodeId::new("zebrad-mainnet").unwrap(),
            Network::Mainnet,
        );
        spec.consent = Some(Consent {
            version: CONSENT_VERSION,
            at: at(0),
            hostname: String::new(),
        });
        spec
    }

    #[test]
    fn the_state_names_the_first_thing_in_the_way() {
        let facts = Facts::new(at(100), Network::Mainnet);
        let steps = plan(&spec(), &facts);
        assert_eq!(
            state(&spec(), &facts, &steps, at(0)),
            PublicState::SettingUp {
                step: StepId::Service
            }
        );
        let unsupported = Facts {
            supported: false,
            ..facts.clone()
        };
        assert_eq!(
            state(&spec(), &unsupported, &steps, at(0)),
            PublicState::NotAvailable {
                why: NotAvailable::NotOnThisSystem
            }
        );
        let untested = Facts {
            zebrad_version: Some("6.4.0".into()),
            ..facts.clone()
        };
        assert!(matches!(
            state(&spec(), &untested, &steps, at(0)),
            PublicState::NotAvailable {
                why: NotAvailable::UntestedZebrad { .. }
            }
        ));
        let off = PublicServiceSpec {
            switch_on: false,
            ..spec()
        };
        assert!(matches!(
            state(&off, &facts, &steps, at(7)),
            PublicState::Paused { why: PauseReason::ByYou, since, .. } if since == at(7)
        ));
    }

    #[test]
    fn the_front_door_answers_the_outside_only_once_open() {
        let mut facts = Facts::new(at(100), Network::Mainnet);
        let setting_up = |step| PublicState::SettingUp { step };
        assert_eq!(gateway_mode(&setting_up(StepId::Name), &facts), None);
        assert_eq!(
            gateway_mode(&setting_up(StepId::Port), &facts),
            Some(GatewayMode::ChallengeOnly)
        );
        assert_eq!(
            gateway_mode(&setting_up(StepId::LocalTest), &facts),
            Some(GatewayMode::ChallengeOnly),
            "no certificate yet"
        );
        facts.cert = Some(CertStatus {
            not_after: Some(at(1000)),
            not_before: Some(at(0)),
            renew_at: None,
            staging_validated_at: Some(at(0)),
            ordering: None,
            next_try: None,
            last_problem: None,
        });
        assert_eq!(
            gateway_mode(&setting_up(StepId::LocalTest), &facts),
            Some(GatewayMode::LocalOnly)
        );
        assert_eq!(
            gateway_mode(&setting_up(StepId::Open), &facts),
            Some(GatewayMode::LocalOnly)
        );
        assert_eq!(
            gateway_mode(&PublicState::Serving, &facts),
            Some(GatewayMode::Open)
        );
        let degraded = |why| PublicState::Degraded { why };
        assert_eq!(
            gateway_mode(&degraded(DegradedReason::BackingRestarting), &facts),
            Some(GatewayMode::Unavailable)
        );
        assert_eq!(
            gateway_mode(&degraded(DegradedReason::CertificateExpired), &facts),
            Some(GatewayMode::ChallengeOnly)
        );
        let paused = PublicState::Paused {
            why: PauseReason::ByYou,
            since: at(0),
            resumes: launcher_types::public::Resume::NeedsYou,
        };
        assert_eq!(gateway_mode(&paused, &facts), None);
    }

    #[test]
    fn the_status_carries_what_wallets_enter() {
        let mut spec = spec();
        spec.name = Some(launcher_types::public::PublicHostname::parse("zec.example.com").unwrap());
        let status = status(&spec, &Facts::new(at(100), Network::Mainnet), at(0));
        let share = status.share.expect("share");
        assert_eq!(share.zodl, "zec.example.com:443");
        assert_eq!(status.steps.len(), StepId::ALL.len());
        assert_eq!(
            status.reach,
            launcher_types::public::Reachability::NotChecked
        );
    }

    #[test]
    fn tested_versions_ignore_build_metadata() {
        assert!(zebrad_tested("7.0.0"));
        assert!(zebrad_tested("7.0.0+git.e8db87e"));
        assert!(zebrad_tested("v7.0.0"));
        assert!(!zebrad_tested("7.0.1"));
        assert!(!zebrad_tested("6.4.0"));
        assert!(!zebrad_tested("7.0.0-rc.0"));
    }
}
