//! The guided setup: each step's checks from the spec and the facts, the step they leave the
//! setup on, and the gates "Open to the internet" re-runs.
//!
//! The plan only reads; the backend runs the checks it lists when their step is reached.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use launcher_types::{
    Phase,
    public::{
        CheckId, CheckOutcome, CheckResult, DnsProvider, Finding, OpenError, PublicServiceSpec,
        Step, StepId, StepState, WanKind,
    },
};

use super::{Facts, light};

fn check(id: CheckId, outcome: CheckOutcome, at: Option<std::time::SystemTime>) -> CheckResult {
    CheckResult { id, outcome, at }
}

fn pass(id: CheckId) -> CheckResult {
    check(id, CheckOutcome::Pass, None)
}

fn fail(id: CheckId, finding: Finding) -> CheckResult {
    check(id, CheckOutcome::Fail { finding }, None)
}

fn warn(id: CheckId, finding: Finding) -> CheckResult {
    check(id, CheckOutcome::Warn { finding }, None)
}

fn running(id: CheckId) -> CheckResult {
    check(id, CheckOutcome::Running, None)
}

fn not_run(id: CheckId) -> CheckResult {
    check(id, CheckOutcome::NotRun, None)
}

/// A finding the user resolves with an answer or an action of theirs, rather than a problem
/// to fix: its step needs them instead of having failed.
fn needs_user(finding: &Finding) -> bool {
    matches!(
        finding,
        Finding::ConsentNeeded
            | Finding::LanNotReserved { .. }
            | Finding::NameMissing
            | Finding::TokenMissing
            | Finding::ForwardNotConfirmed
            | Finding::Firewall { .. }
    )
}

/// A finding that only time resolves.
fn waits(finding: &Finding) -> bool {
    matches!(finding, Finding::TipNotSettled { .. })
}

/// The state its checks give a step.
fn step_state(checks: &[CheckResult]) -> StepState {
    let fails = || {
        checks.iter().filter_map(|check| match &check.outcome {
            CheckOutcome::Fail { finding } => Some(finding),
            _ => None,
        })
    };
    if fails().any(|finding| !needs_user(finding) && !waits(finding)) {
        StepState::Failed
    } else if fails().any(needs_user) {
        StepState::NeedsYou
    } else if fails().any(waits)
        || checks
            .iter()
            .any(|check| matches!(check.outcome, CheckOutcome::Running | CheckOutcome::NotRun))
    {
        StepState::Running
    } else {
        StepState::Done
    }
}

/// The step the setup is on: the first one not done.
pub fn current_step(steps: &[Step]) -> Option<StepId> {
    steps
        .iter()
        .find(|step| step.state != StepState::Done)
        .map(|step| step.id)
}

/// Every step with its checks. The setup is linear: every step after the first one that is not
/// done waits for it, keeping its checks for the details.
pub fn plan(spec: &PublicServiceSpec, facts: &Facts) -> Vec<Step> {
    let mut steps: Vec<Step> = StepId::ALL
        .into_iter()
        .map(|id| {
            let checks = checks(id, spec, facts);
            let state = step_state(&checks);
            Step { id, state, checks }
        })
        .collect();
    // "Open to the internet" is the user's click once its gate passes.
    if let Some(open) = steps.iter_mut().find(|step| step.id == StepId::Open)
        && !spec.opened
        && open.state == StepState::Done
    {
        open.state = StepState::NeedsYou;
    }
    let mut reached = false;
    for step in &mut steps {
        let done = step.state == StepState::Done;
        if reached {
            step.state = StepState::Waiting;
        }
        reached |= !done;
    }
    steps
}

fn checks(id: StepId, spec: &PublicServiceSpec, facts: &Facts) -> Vec<CheckResult> {
    match id {
        StepId::Consent => consent(spec, facts),
        StepId::Service => service(spec, facts),
        StepId::Network => network(spec, facts),
        StepId::Name => name(spec, facts),
        StepId::Port => port(spec, facts),
        StepId::Firewall => firewall(spec, facts),
        StepId::Certificate => certificate(facts),
        StepId::LocalTest => local_test(facts),
        StepId::Open => open(spec, facts),
        StepId::KeepRunning | StepId::Share => {
            if spec.opened {
                vec![pass(CheckId::KeepAwake)]
            } else {
                vec![not_run(CheckId::KeepAwake)]
            }
        }
    }
}

fn consent(spec: &PublicServiceSpec, facts: &Facts) -> Vec<CheckResult> {
    let platform = if facts.supported {
        pass(CheckId::Platform)
    } else {
        fail(CheckId::Platform, Finding::NotOnThisSystem)
    };
    let consent = if spec.consent_current() {
        check(
            CheckId::Consent,
            CheckOutcome::Pass,
            spec.consent.as_ref().map(|consent| consent.at),
        )
    } else {
        fail(CheckId::Consent, Finding::ConsentNeeded)
    };
    vec![platform, consent]
}

fn service(spec: &PublicServiceSpec, facts: &Facts) -> Vec<CheckResult> {
    let tested = if facts.zebrad_tested() {
        pass(CheckId::ZebradTested)
    } else {
        fail(
            CheckId::ZebradTested,
            Finding::UntestedZebrad {
                version: facts.zebrad_version.clone().unwrap_or_default(),
            },
        )
    };
    let wanted = spec
        .backend_port
        .map(|port| SocketAddr::from((Ipv4Addr::LOCALHOST, port)));
    let opened = facts.endpoint.is_some() && facts.endpoint == wanted;
    let endpoint = match wanted {
        _ if facts.endpoint_port_taken => fail(
            CheckId::EndpointOpened,
            Finding::EndpointPortTaken {
                port: spec.backend_port.unwrap_or_default(),
            },
        ),
        None => match &facts.endpoint_error {
            Some(detail) => fail(
                CheckId::EndpointOpened,
                Finding::EndpointNotWritten {
                    detail: detail.clone(),
                },
            ),
            None => not_run(CheckId::EndpointOpened),
        },
        Some(_) if opened => pass(CheckId::EndpointOpened),
        Some(_) if matches!(facts.phase, Phase::Stopped | Phase::Failed { .. }) => {
            fail(CheckId::EndpointOpened, Finding::ZebradNotRunning)
        }
        Some(_) => running(CheckId::EndpointOpened),
    };
    let wallet = match &facts.wallet {
        Some(checked) if opened => match &checked.result {
            Ok(_) => check(CheckId::WalletCheck, CheckOutcome::Pass, Some(checked.at)),
            Err(problem) => check(
                CheckId::WalletCheck,
                CheckOutcome::Fail {
                    finding: Finding::WalletCheck {
                        problem: problem.clone(),
                    },
                },
                Some(checked.at),
            ),
        },
        _ if opened => running(CheckId::WalletCheck),
        _ => not_run(CheckId::WalletCheck),
    };
    // The setup goes on while zebrad syncs; only Open waits for the tip.
    let tip = match (facts.height, facts.tip) {
        _ if light::at_tip(facts.height, facts.tip) => pass(CheckId::AtTip),
        (Some(height), Some(tip)) => warn(
            CheckId::AtTip,
            Finding::Syncing {
                height,
                tip,
                eta_secs: facts.eta_secs,
            },
        ),
        _ => not_run(CheckId::AtTip),
    };
    vec![tested, endpoint, wallet, tip]
}

fn network(spec: &PublicServiceSpec, facts: &Facts) -> Vec<CheckResult> {
    let ids = [
        CheckId::LanAddress,
        CheckId::NoVpn,
        CheckId::WanAddress,
        CheckId::LanReserved,
    ];
    let Some(net) = &facts.net else {
        return ids.into_iter().map(running).collect();
    };
    let at = Some(net.checked_at);
    let with_at = |mut result: CheckResult| {
        result.at = at;
        result
    };
    let lan = match (net.lan_addr, net.gateway) {
        (Some(_), Some(_)) => pass(CheckId::LanAddress),
        _ => fail(CheckId::LanAddress, Finding::NoLanAddress),
    };
    let vpn = match &net.vpn {
        None => pass(CheckId::NoVpn),
        Some(interface) => fail(
            CheckId::NoVpn,
            Finding::Vpn {
                interface: interface.clone(),
            },
        ),
    };
    let wan = match (net.wan, net.wan_addr) {
        (WanKind::Public, _) => pass(CheckId::WanAddress),
        (WanKind::Cgnat, Some(wan)) => fail(CheckId::WanAddress, Finding::Cgnat { wan }),
        (WanKind::BehindAnotherRouter, Some(wan)) => {
            fail(CheckId::WanAddress, Finding::BehindAnotherRouter { wan })
        }
        _ => warn(CheckId::WanAddress, Finding::WanUnknown),
    };
    let reserved = match net.lan_addr {
        Some(lan) if spec.lan_addr == Some(lan) => pass(CheckId::LanReserved),
        Some(lan) => fail(CheckId::LanReserved, Finding::LanNotReserved { lan }),
        None => not_run(CheckId::LanReserved),
    };
    [lan, vpn, wan, reserved].into_iter().map(with_at).collect()
}

/// The address the name must resolve to: the router's public address, or else the one the
/// provider saw.
fn expected_address(facts: &Facts) -> Option<IpAddr> {
    let router = facts
        .net
        .as_ref()
        .filter(|net| net.wan == WanKind::Public)
        .and_then(|net| net.wan_addr);
    router.or_else(|| facts.dns.as_ref().and_then(|dns| dns.provider_saw))
}

fn name(spec: &PublicServiceSpec, facts: &Facts) -> Vec<CheckResult> {
    let mut checks = Vec::new();
    let Some(name) = &spec.name else {
        checks.push(fail(CheckId::NameChosen, Finding::NameMissing));
        checks.push(not_run(CheckId::NameResolves));
        return checks;
    };
    checks.push(pass(CheckId::NameChosen));
    let dns = facts.dns.as_ref().filter(|dns| dns.name == *name);
    if spec.provider == DnsProvider::DuckDns {
        let has_token = dns.is_some_and(|dns| dns.has_token);
        checks.push(match () {
            _ if has_token => pass(CheckId::Token),
            _ if facts.keychain_unavailable => fail(CheckId::Token, Finding::KeychainUnavailable),
            _ => fail(CheckId::Token, Finding::TokenMissing),
        });
        let router = facts
            .net
            .as_ref()
            .filter(|net| net.wan == WanKind::Public)
            .and_then(|net| net.wan_addr);
        checks.push(match dns {
            Some(dns) if dns.problem.is_some() => fail(
                CheckId::AddressUpdate,
                Finding::AddressUpdateFailed {
                    problem: dns
                        .problem
                        .unwrap_or(launcher_types::public::DdnsProblem::BadAnswer),
                },
            ),
            Some(dns) if dns.updated_at.is_some() => {
                let outcome = match (dns.provider_saw, router) {
                    (Some(provider_saw), Some(router)) if provider_saw != router => {
                        CheckOutcome::Warn {
                            finding: Finding::ProviderSawOther {
                                provider_saw,
                                router,
                            },
                        }
                    }
                    _ => CheckOutcome::Pass,
                };
                check(CheckId::AddressUpdate, outcome, dns.updated_at)
            }
            _ if has_token => running(CheckId::AddressUpdate),
            _ => not_run(CheckId::AddressUpdate),
        });
    }
    let resolves = match dns.filter(|dns| dns.checked_at.is_some()) {
        None => running(CheckId::NameResolves),
        Some(dns) => {
            let v4: Vec<IpAddr> = dns
                .resolved
                .iter()
                .copied()
                .filter(IpAddr::is_ipv4)
                .collect();
            let v6: Vec<IpAddr> = dns
                .resolved
                .iter()
                .copied()
                .filter(IpAddr::is_ipv6)
                .collect();
            let outcome = match expected_address(facts) {
                _ if v4.is_empty() => CheckOutcome::Fail {
                    finding: Finding::NameNotFound,
                },
                Some(expected) if !v4.contains(&expected) => CheckOutcome::Fail {
                    finding: Finding::NamePointsElsewhere {
                        points_to: v4,
                        expected: Some(expected),
                    },
                },
                _ if !v6.is_empty() => CheckOutcome::Warn {
                    finding: Finding::NameHasIpv6 { addrs: v6 },
                },
                _ => CheckOutcome::Pass,
            };
            check(CheckId::NameResolves, outcome, dns.checked_at)
        }
    };
    checks.push(resolves);
    checks
}

fn port(spec: &PublicServiceSpec, facts: &Facts) -> Vec<CheckResult> {
    let free = match facts.port_free {
        Some(true) => pass(CheckId::PortFree),
        Some(false) => fail(
            CheckId::PortFree,
            Finding::PortTaken {
                port: spec.internal_port,
            },
        ),
        None => running(CheckId::PortFree),
    };
    let forward = if spec.forwarded {
        pass(CheckId::Forward)
    } else {
        fail(CheckId::Forward, Finding::ForwardNotConfirmed)
    };
    vec![free, forward]
}

fn firewall(spec: &PublicServiceSpec, facts: &Facts) -> Vec<CheckResult> {
    let tool = facts.firewall.as_ref().and_then(|info| info.tool);
    let result = match tool {
        Some(tool) if !spec.firewall_done => fail(
            CheckId::Firewall,
            Finding::Firewall {
                tool,
                port: spec.internal_port,
            },
        ),
        Some(_) => pass(CheckId::Firewall),
        None if facts.firewall.is_some() => pass(CheckId::Firewall),
        None => running(CheckId::Firewall),
    };
    vec![result]
}

fn certificate(facts: &Facts) -> Vec<CheckResult> {
    use launcher_types::public::CertIssuer;
    let Some(cert) = &facts.cert else {
        return vec![
            running(CheckId::StagingCertificate),
            not_run(CheckId::Certificate),
        ];
    };
    let valid = cert
        .not_after
        .is_some_and(|not_after| not_after > facts.now);
    let problem = cert.last_problem.clone();
    let staging = match () {
        _ if cert.staging_validated_at.is_some() || valid => check(
            CheckId::StagingCertificate,
            CheckOutcome::Pass,
            cert.staging_validated_at,
        ),
        _ if cert.ordering == Some(CertIssuer::Staging) => running(CheckId::StagingCertificate),
        _ => match &problem {
            Some(problem) => fail(
                CheckId::StagingCertificate,
                Finding::Acme {
                    problem: problem.clone(),
                },
            ),
            None => running(CheckId::StagingCertificate),
        },
    };
    let production = match () {
        _ if valid => check(CheckId::Certificate, CheckOutcome::Pass, cert.not_before),
        _ if cert.staging_validated_at.is_none() => not_run(CheckId::Certificate),
        _ if cert.ordering == Some(CertIssuer::Production) => running(CheckId::Certificate),
        _ => match problem {
            Some(problem) => fail(CheckId::Certificate, Finding::Acme { problem }),
            None => running(CheckId::Certificate),
        },
    };
    vec![staging, production]
}

fn local_test(facts: &Facts) -> Vec<CheckResult> {
    let result = match &facts.local_test {
        None => running(CheckId::LocalTest),
        Some(checked) => match &checked.result {
            Ok(_) => check(CheckId::LocalTest, CheckOutcome::Pass, Some(checked.at)),
            Err(problem) => check(
                CheckId::LocalTest,
                CheckOutcome::Fail {
                    finding: Finding::LocalTestFailed {
                        problem: problem.clone(),
                    },
                },
                Some(checked.at),
            ),
        },
    };
    vec![result]
}

fn open(spec: &PublicServiceSpec, facts: &Facts) -> Vec<CheckResult> {
    let settled = if light::tip_settled(facts) || spec.opened {
        pass(CheckId::TipSettled)
    } else {
        fail(
            CheckId::TipSettled,
            Finding::TipNotSettled {
                since: facts.at_tip_since,
            },
        )
    };
    // Only an open front door can be reached through the public address, so the hairpin
    // test joins the step once the service is open.
    if !spec.opened {
        return vec![settled];
    }
    let hairpin = match &facts.hairpin {
        Some(checked) if checked.result => {
            check(CheckId::Hairpin, CheckOutcome::Pass, Some(checked.at))
        }
        Some(checked) => check(
            CheckId::Hairpin,
            CheckOutcome::Warn {
                finding: Finding::NoHairpin,
            },
            Some(checked.at),
        ),
        None => running(CheckId::Hairpin),
    };
    vec![settled, hairpin]
}

/// The gates "Open to the internet" re-runs at the moment of the click: every step before it
/// done, zebrad settled at the tip, and nothing that would pause the service.
pub fn open_gates(spec: &PublicServiceSpec, facts: &Facts) -> Result<(), OpenError> {
    let refuse = |check: Option<CheckId>, message: &str| OpenError {
        check,
        message: message.to_string(),
    };
    if let Some((why, _)) = super::pause(spec, facts) {
        return Err(refuse(None, &format!("the service is paused: {why:?}")));
    }
    for step in plan(spec, facts) {
        if step.id >= StepId::Open {
            break;
        }
        if let Some(failed) = step.checks.iter().find(|check| !check.outcome.passed()) {
            return Err(refuse(
                Some(failed.id),
                &format!("{:?} has not passed yet", failed.id),
            ));
        }
    }
    if !light::tip_settled(facts) {
        return Err(refuse(
            Some(CheckId::TipSettled),
            "zebrad must stay at the tip for 10 minutes before the service opens",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use launcher_types::{
        Network, NodeId,
        public::{
            AcmeProblem, CONSENT_VERSION, CertIssuer, CertStatus, Consent, DnsStatus, FirewallTool,
            LightdInfo, NetFacts, PublicHostname, WalletProblem,
        },
    };

    use super::*;
    use crate::public::{Checked, firewall::firewall_info};

    const NOW: u64 = 20_000_000;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn lan() -> IpAddr {
        "192.168.1.20".parse().unwrap()
    }

    fn wan() -> IpAddr {
        "203.0.113.10".parse().unwrap()
    }

    fn name() -> PublicHostname {
        PublicHostname::parse("zec-alice.duckdns.org").unwrap()
    }

    fn fresh() -> PublicServiceSpec {
        PublicServiceSpec::new_light_server(
            NodeId::new("zebrad-mainnet").unwrap(),
            Network::Mainnet,
        )
    }

    fn net() -> NetFacts {
        NetFacts {
            lan_addr: Some(lan()),
            gateway: Some("192.168.1.1".parse().unwrap()),
            interface: Some("eth0".into()),
            vpn: None,
            wan: WanKind::Public,
            wan_addr: Some(wan()),
            checked_at: at(NOW),
        }
    }

    fn dns(resolved: Vec<IpAddr>) -> DnsStatus {
        DnsStatus {
            name: name(),
            provider: DnsProvider::DuckDns,
            has_token: true,
            resolved,
            provider_saw: Some(wan()),
            updated_at: Some(at(NOW - 60)),
            problem: None,
            checked_at: Some(at(NOW)),
        }
    }

    /// A spec and facts with every step up to the test on this computer passed.
    fn ready() -> (PublicServiceSpec, Facts) {
        let mut spec = fresh();
        spec.consent = Some(Consent {
            version: CONSENT_VERSION,
            at: at(NOW - 3600),
            hostname: name().to_string(),
        });
        spec.backend_port = Some(9067);
        spec.lan_addr = Some(lan());
        spec.name = Some(name());
        spec.provider = DnsProvider::DuckDns;
        spec.forwarded = true;
        spec.firewall_done = true;
        let mut facts = Facts::new(at(NOW), Network::Mainnet);
        facts.zebrad_version = Some("7.0.0".into());
        facts.phase = Phase::Running;
        facts.endpoint = Some("127.0.0.1:9067".parse().unwrap());
        facts.height = Some(3_544_812);
        facts.tip = Some(3_544_812);
        facts.at_tip_since = Some(at(NOW - 3600));
        facts.wallet = Some(Checked {
            at: at(NOW),
            result: Ok(LightdInfo::default()),
        });
        facts.net = Some(net());
        facts.dns = Some(dns(vec![wan()]));
        facts.port_free = Some(true);
        facts.firewall = Some(firewall_info(Some(FirewallTool::Ufw), 8443));
        facts.cert = Some(CertStatus {
            not_after: Some(at(NOW + 89 * 86_400)),
            not_before: Some(at(NOW - 86_400)),
            renew_at: Some(at(NOW + 59 * 86_400)),
            staging_validated_at: Some(at(NOW - 86_400)),
            ordering: None,
            next_try: None,
            last_problem: None,
        });
        facts.local_test = Some(Checked {
            at: at(NOW),
            result: Ok(LightdInfo::default()),
        });
        (spec, facts)
    }

    fn state_of(steps: &[Step], id: StepId) -> StepState {
        steps.iter().find(|step| step.id == id).unwrap().state
    }

    fn outcome_of(steps: &[Step], id: CheckId) -> CheckOutcome {
        steps
            .iter()
            .flat_map(|step| &step.checks)
            .find(|check| check.id == id)
            .unwrap()
            .outcome
            .clone()
    }

    #[test]
    fn a_new_setup_starts_at_the_consent() {
        let facts = Facts::new(at(NOW), Network::Mainnet);
        let steps = plan(&fresh(), &facts);
        assert_eq!(steps.len(), StepId::ALL.len());
        assert_eq!(state_of(&steps, StepId::Consent), StepState::NeedsYou);
        assert_eq!(current_step(&steps), Some(StepId::Consent));
        for step in &steps[1..] {
            assert_eq!(step.state, StepState::Waiting, "{:?}", step.id);
        }
    }

    #[test]
    fn the_setup_goes_on_while_zebrad_syncs() {
        let (spec, mut facts) = ready();
        facts.height = Some(2_900_000);
        facts.tip = Some(3_544_812);
        facts.at_tip_since = None;
        let steps = plan(&spec, &facts);
        assert!(matches!(
            outcome_of(&steps, CheckId::AtTip),
            CheckOutcome::Warn {
                finding: Finding::Syncing { .. }
            }
        ));
        assert_eq!(state_of(&steps, StepId::Service), StepState::Done);
        assert_eq!(state_of(&steps, StepId::LocalTest), StepState::Done);
        // Open waits for the tip, without failing.
        assert_eq!(state_of(&steps, StepId::Open), StepState::Running);
        assert_eq!(current_step(&steps), Some(StepId::Open));
        assert_eq!(
            open_gates(&spec, &facts).unwrap_err().check,
            Some(CheckId::TipSettled)
        );
    }

    #[test]
    fn a_ready_setup_offers_open_and_open_succeeds() {
        let (spec, facts) = ready();
        let steps = plan(&spec, &facts);
        assert_eq!(current_step(&steps), Some(StepId::Open));
        assert_eq!(state_of(&steps, StepId::Open), StepState::NeedsYou);
        assert_eq!(open_gates(&spec, &facts), Ok(()));
        // Once opened, every step is done.
        let opened = PublicServiceSpec {
            opened: true,
            ..spec
        };
        let facts = Facts {
            hairpin: Some(Checked {
                at: at(NOW),
                result: true,
            }),
            ..facts
        };
        let steps = plan(&opened, &facts);
        assert_eq!(current_step(&steps), None, "{steps:#?}");
    }

    #[test]
    fn carrier_grade_nat_fails_the_network_step() {
        let (spec, mut facts) = ready();
        let cgnat: IpAddr = "100.72.4.9".parse().unwrap();
        facts.net = Some(NetFacts {
            wan: WanKind::Cgnat,
            wan_addr: Some(cgnat),
            ..net()
        });
        let steps = plan(&spec, &facts);
        assert_eq!(state_of(&steps, StepId::Network), StepState::Failed);
        assert_eq!(
            outcome_of(&steps, CheckId::WanAddress),
            CheckOutcome::Fail {
                finding: Finding::Cgnat { wan: cgnat }
            }
        );
        assert_eq!(current_step(&steps), Some(StepId::Network));
        assert_eq!(state_of(&steps, StepId::Name), StepState::Waiting);
        assert_eq!(
            open_gates(&spec, &facts).unwrap_err().check,
            Some(CheckId::WanAddress)
        );
    }

    #[test]
    fn user_answers_need_the_user() {
        let (mut spec, facts) = ready();
        spec.lan_addr = None;
        let steps = plan(&spec, &facts);
        assert_eq!(state_of(&steps, StepId::Network), StepState::NeedsYou);
        let (mut spec, facts) = ready();
        spec.forwarded = false;
        assert_eq!(
            state_of(&plan(&spec, &facts), StepId::Port),
            StepState::NeedsYou
        );
        let (mut spec, facts) = ready();
        spec.firewall_done = false;
        assert_eq!(
            state_of(&plan(&spec, &facts), StepId::Firewall),
            StepState::NeedsYou
        );
        // Without a firewall tool on PATH there is nothing to do.
        let facts = Facts {
            firewall: Some(firewall_info(None, 8443)),
            ..facts
        };
        assert_eq!(
            state_of(&plan(&spec, &facts), StepId::Firewall),
            StepState::Done
        );
    }

    #[test]
    fn the_name_must_point_at_the_router() {
        let (spec, mut facts) = ready();
        let elsewhere: IpAddr = "198.51.100.7".parse().unwrap();
        facts.dns = Some(dns(vec![elsewhere]));
        let steps = plan(&spec, &facts);
        assert_eq!(
            outcome_of(&steps, CheckId::NameResolves),
            CheckOutcome::Fail {
                finding: Finding::NamePointsElsewhere {
                    points_to: vec![elsewhere],
                    expected: Some(wan())
                }
            }
        );
        facts.dns = Some(dns(Vec::new()));
        assert_eq!(
            outcome_of(&plan(&spec, &facts), CheckId::NameResolves),
            CheckOutcome::Fail {
                finding: Finding::NameNotFound
            }
        );
        let v6: IpAddr = "2001:db8::10".parse().unwrap();
        facts.dns = Some(dns(vec![wan(), v6]));
        assert_eq!(
            outcome_of(&plan(&spec, &facts), CheckId::NameResolves),
            CheckOutcome::Warn {
                finding: Finding::NameHasIpv6 { addrs: vec![v6] }
            }
        );
        // With CGNAT, what DuckDNS saw differs from the router's own address.
        let (spec, mut facts) = ready();
        facts.dns.as_mut().unwrap().provider_saw = Some("198.51.100.7".parse().unwrap());
        assert!(matches!(
            outcome_of(&plan(&spec, &facts), CheckId::AddressUpdate),
            CheckOutcome::Warn {
                finding: Finding::ProviderSawOther { .. }
            }
        ));
    }

    #[test]
    fn a_duckdns_name_needs_its_token() {
        let (spec, mut facts) = ready();
        facts.dns.as_mut().unwrap().has_token = false;
        facts.dns.as_mut().unwrap().updated_at = None;
        let steps = plan(&spec, &facts);
        assert_eq!(
            outcome_of(&steps, CheckId::Token),
            CheckOutcome::Fail {
                finding: Finding::TokenMissing
            }
        );
        assert_eq!(state_of(&steps, StepId::Name), StepState::NeedsYou);
        facts.keychain_unavailable = true;
        assert_eq!(
            state_of(&plan(&spec, &facts), StepId::Name),
            StepState::Failed
        );
        // A name kept by hand needs no token.
        let manual = PublicServiceSpec {
            provider: DnsProvider::Manual,
            ..spec
        };
        let steps = plan(&manual, &facts);
        assert!(
            steps
                .iter()
                .flat_map(|step| &step.checks)
                .all(|check| check.id != CheckId::Token)
        );
    }

    #[test]
    fn certificate_failures_name_their_fix() {
        let (spec, mut facts) = ready();
        facts.cert = Some(CertStatus {
            not_after: None,
            not_before: None,
            renew_at: None,
            staging_validated_at: None,
            ordering: None,
            next_try: Some(at(NOW + 3600)),
            last_problem: Some(AcmeProblem::Connection),
        });
        let steps = plan(&spec, &facts);
        assert_eq!(
            outcome_of(&steps, CheckId::StagingCertificate),
            CheckOutcome::Fail {
                finding: Finding::Acme {
                    problem: AcmeProblem::Connection
                }
            }
        );
        assert_eq!(state_of(&steps, StepId::Certificate), StepState::Failed);
        // Staging passed, production in flight.
        facts.cert.as_mut().unwrap().staging_validated_at = Some(at(NOW - 60));
        facts.cert.as_mut().unwrap().last_problem = None;
        facts.cert.as_mut().unwrap().ordering = Some(CertIssuer::Production);
        let steps = plan(&spec, &facts);
        assert_eq!(
            outcome_of(&steps, CheckId::Certificate),
            CheckOutcome::Running
        );
        assert_eq!(state_of(&steps, StepId::Certificate), StepState::Running);
    }

    #[test]
    fn the_endpoint_and_wallet_checks_follow_zebrad() {
        let (spec, mut facts) = ready();
        facts.endpoint = None;
        facts.phase = Phase::Starting;
        let steps = plan(&spec, &facts);
        assert_eq!(
            outcome_of(&steps, CheckId::EndpointOpened),
            CheckOutcome::Running
        );
        assert_eq!(
            outcome_of(&steps, CheckId::WalletCheck),
            CheckOutcome::NotRun
        );
        facts.endpoint_port_taken = true;
        assert_eq!(
            outcome_of(&plan(&spec, &facts), CheckId::EndpointOpened),
            CheckOutcome::Fail {
                finding: Finding::EndpointPortTaken { port: 9067 }
            }
        );
        let (spec, mut facts) = ready();
        let problem = WalletProblem::WrongBranch {
            got: "37a5165b".into(),
            want: "77190ad9".into(),
        };
        facts.wallet = Some(Checked {
            at: at(NOW),
            result: Err(problem.clone()),
        });
        let steps = plan(&spec, &facts);
        assert_eq!(state_of(&steps, StepId::Service), StepState::Failed);
        assert!(matches!(
            outcome_of(&steps, CheckId::WalletCheck),
            CheckOutcome::Fail {
                finding: Finding::WalletCheck { problem: got }
            } if got == problem
        ));
    }

    #[test]
    fn open_refuses_while_paused() {
        let (spec, facts) = ready();
        let off = PublicServiceSpec {
            switch_on: false,
            ..spec
        };
        assert_eq!(open_gates(&off, &facts).unwrap_err().check, None);
    }
}
