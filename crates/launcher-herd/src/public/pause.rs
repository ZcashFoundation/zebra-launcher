//! Automatic pauses and degraded service.
//!
//! Problems with the exposure itself (the switch, the network, the LAN address, the name, a
//! VPN, the data cap, a stopped zebrad) close the front door: a pause. Problems that clear
//! themselves (zebrad restarting or behind, a failed wallet check, an expired certificate)
//! keep it up and tell wallets to try again, so Let's Encrypt's renewals still get through.

use std::time::{Duration, SystemTime};

use launcher_types::{
    Phase,
    public::{DegradedReason, PauseReason, PublicServiceSpec, PublicState, Resume},
};

use super::{Facts, light};

/// A name that points elsewhere this long pauses the service.
pub const NAME_ELSEWHERE_PAUSE: Duration = Duration::from_secs(15 * 60);

/// Bytes in a GB, for the monthly cap.
const GB: u64 = 1_000_000_000;

/// Why the front door must be closed now, and how it comes back; `None` when it may be up.
///
/// The switch and the network apply from the start. The others watch an open service: during
/// the setup the same facts are failing checks of their steps.
pub fn pause(spec: &PublicServiceSpec, facts: &Facts) -> Option<(PauseReason, Resume)> {
    if !spec.switch_on {
        return Some((PauseReason::ByYou, Resume::NeedsYou));
    }
    // During the setup, consent is the first step's.
    if spec.opened && !spec.consent_current() {
        return Some((PauseReason::ConsentNeeded, Resume::NeedsYou));
    }
    if spec.network != facts.network {
        return Some((PauseReason::NetworkChanged, Resume::NeedsYou));
    }
    if let Some(why) = &facts.held {
        return Some((why.clone(), Resume::NeedsYou));
    }
    if !spec.opened {
        return None;
    }
    if let Some(interface) = facts.net.as_ref().and_then(|net| net.vpn.clone()) {
        return Some((PauseReason::VpnRoute { interface }, Resume::Automatic));
    }
    if let (Some(was), Some(now)) = (
        spec.lan_addr,
        facts.net.as_ref().and_then(|net| net.lan_addr),
    ) && was != now
    {
        return Some((PauseReason::LanAddressChanged { was }, Resume::Automatic));
    }
    let elsewhere_long = facts.name_elsewhere_since.is_some_and(|since| {
        facts.now.duration_since(since).unwrap_or_default() >= NAME_ELSEWHERE_PAUSE
    });
    if elsewhere_long {
        let points_to = facts
            .dns
            .as_ref()
            .map(|dns| dns.resolved.clone())
            .unwrap_or_default();
        return Some((PauseReason::NameElsewhere { points_to }, Resume::Automatic));
    }
    if let Some(cap) = spec.limits.monthly_gb
        && facts.month_bytes_up >= u64::from(cap) * GB
    {
        return Some((PauseReason::DataCap, Resume::NextMonth));
    }
    if matches!(facts.phase, Phase::Stopped | Phase::Failed { .. }) {
        return Some((PauseReason::BackingStopped, Resume::Automatic));
    }
    None
}

/// Whether the name may be pointed at the network `net` describes: the check found the default
/// route's gateway and this computer's LAN address, as the network step needs, no VPN carries
/// that route, and for an open service this computer still has the LAN address the forward
/// targets. A DNS update from a foreign network would publish where the user is, and a route
/// that could not be read says nothing about a VPN.
pub fn may_point_name_here(
    spec: &PublicServiceSpec,
    net: &launcher_types::public::NetFacts,
) -> bool {
    let route_known = net.gateway.is_some() && net.lan_addr.is_some();
    let lan_kept = !spec.opened || spec.lan_addr.is_none() || net.lan_addr == spec.lan_addr;
    route_known && net.vpn.is_none() && lan_kept
}

/// Since when an open service's name has resolved away from this network without a break,
/// from the last value in `facts.name_elsewhere_since` and the facts at `facts.now`. Without a
/// resolution to compare, the last answer holds: a pause for the name must not end because
/// nothing looked.
pub fn name_elsewhere_since(spec: &PublicServiceSpec, facts: &Facts) -> Option<SystemTime> {
    if !spec.opened {
        return None;
    }
    // Without NAT-PMP's answer, the address DuckDNS saw, as the name step checks.
    let expected = super::expected_address(facts);
    match (&facts.dns, expected) {
        (Some(dns), Some(expected)) if dns.checked_at.is_some() => light::track_since(
            facts.name_elsewhere_since,
            !dns.resolved.contains(&expected),
            facts.now,
        ),
        _ => facts.name_elsewhere_since,
    }
}

/// Whether the network checks and the name's resolution run in `state`: while the service
/// runs, and through a pause that ends by itself, so they see its cause clear. A pause only
/// the user ends stops them.
pub fn watches_surroundings(state: &PublicState) -> bool {
    match state {
        PublicState::Paused { resumes, .. } => {
            matches!(resumes, Resume::Automatic | Resume::NextMonth)
        }
        PublicState::NotAvailable { .. } | PublicState::Off => false,
        PublicState::SettingUp { .. } | PublicState::Serving | PublicState::Degraded { .. } => true,
    }
}

/// Why an open service tells wallets to try again, if it must.
pub fn degraded(spec: &PublicServiceSpec, facts: &Facts) -> Option<DegradedReason> {
    let expired = facts
        .cert
        .as_ref()
        .and_then(|cert| cert.not_after)
        .is_none_or(|not_after| not_after <= facts.now);
    if expired {
        return Some(DegradedReason::CertificateExpired);
    }
    let endpoint_open = spec.backend_port.is_some_and(|port| {
        facts
            .endpoint
            .is_some_and(|addr| addr.port() == port && addr.ip().is_loopback())
    });
    if facts.phase != Phase::Running || !endpoint_open {
        return Some(DegradedReason::BackingRestarting);
    }
    // A few blocks behind for a moment is an ordinary gap between blocks; an unknown height
    // is behind.
    let blocks = match (facts.height, facts.tip) {
        (Some(height), Some(tip)) => Some(tip.saturating_sub(height)),
        _ => None,
    };
    let behind_long = facts.behind_since.is_some_and(|since| {
        facts.now.duration_since(since).unwrap_or_default() >= light::BEHIND_GRACE
    });
    match blocks {
        None => return Some(DegradedReason::BackingBehind { blocks: 0 }),
        Some(blocks)
            if blocks > light::AT_TIP_BLOCKS
                && (blocks >= light::FAR_BEHIND_BLOCKS || behind_long) =>
        {
            return Some(DegradedReason::BackingBehind { blocks });
        }
        Some(_) => {}
    }
    if facts
        .wallet
        .as_ref()
        .is_some_and(|checked| checked.result.is_err())
    {
        return Some(DegradedReason::WalletCheckFailed);
    }
    None
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    use launcher_types::{
        ExitClass, Network, NodeId,
        public::{
            CONSENT_VERSION, CertStatus, Consent, DnsProvider, DnsStatus, LightdInfo, NetFacts,
            PublicHostname, WalletProblem, WanKind,
        },
    };

    use super::*;
    use crate::public::Checked;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    const NOW: u64 = 10_000_000;

    fn lan() -> IpAddr {
        "192.168.1.20".parse().unwrap()
    }

    fn open_spec() -> PublicServiceSpec {
        let mut spec = PublicServiceSpec::new_light_server(
            NodeId::new("zebrad-mainnet").unwrap(),
            Network::Mainnet,
        );
        spec.consent = Some(Consent {
            version: CONSENT_VERSION,
            at: at(0),
            hostname: "zec.example.com".into(),
        });
        spec.name = Some(PublicHostname::parse("zec.example.com").unwrap());
        spec.backend_port = Some(9067);
        spec.lan_addr = Some(lan());
        spec.opened = true;
        spec
    }

    fn serving_facts() -> Facts {
        let mut facts = Facts::new(at(NOW), Network::Mainnet);
        facts.phase = Phase::Running;
        facts.endpoint = Some("127.0.0.1:9067".parse().unwrap());
        facts.height = Some(3_544_812);
        facts.tip = Some(3_544_813);
        facts.net = Some(NetFacts {
            lan_addr: Some(lan()),
            gateway: Some("192.168.1.1".parse().unwrap()),
            interface: Some("eth0".into()),
            vpn: None,
            wan: WanKind::Public,
            wan_addr: Some("203.0.113.10".parse().unwrap()),
            checked_at: at(NOW),
        });
        facts.cert = Some(CertStatus {
            not_after: Some(at(NOW + 60 * 86_400)),
            not_before: Some(at(NOW - 29 * 86_400)),
            renew_at: Some(at(NOW + 30 * 86_400)),
            staging_validated_at: Some(at(NOW - 29 * 86_400)),
            ordering: None,
            next_try: None,
            last_problem: None,
        });
        facts.wallet = Some(Checked {
            at: at(NOW),
            result: Ok(LightdInfo::default()),
        });
        facts
    }

    #[test]
    fn a_healthy_open_service_is_neither_paused_nor_degraded() {
        let (spec, facts) = (open_spec(), serving_facts());
        assert_eq!(pause(&spec, &facts), None);
        assert_eq!(degraded(&spec, &facts), None);
    }

    #[test]
    fn exposure_problems_pause() {
        let spec = open_spec();
        let base = serving_facts();

        let off = PublicServiceSpec {
            switch_on: false,
            ..spec.clone()
        };
        assert_eq!(
            pause(&off, &base),
            Some((PauseReason::ByYou, Resume::NeedsYou))
        );
        let testnet = Facts {
            network: Network::Testnet,
            ..base.clone()
        };
        assert_eq!(
            pause(&spec, &testnet),
            Some((PauseReason::NetworkChanged, Resume::NeedsYou))
        );
        // Switching back keeps it paused until the user resumes.
        let back = Facts {
            held: Some(PauseReason::NetworkChanged),
            ..base.clone()
        };
        assert_eq!(
            pause(&spec, &back),
            Some((PauseReason::NetworkChanged, Resume::NeedsYou))
        );

        let mut vpn = base.clone();
        vpn.net.as_mut().unwrap().vpn = Some("wg0".into());
        assert_eq!(
            pause(&spec, &vpn),
            Some((
                PauseReason::VpnRoute {
                    interface: "wg0".into()
                },
                Resume::Automatic
            ))
        );

        let mut moved = base.clone();
        moved.net.as_mut().unwrap().lan_addr = Some("192.168.1.33".parse().unwrap());
        assert_eq!(
            pause(&spec, &moved),
            Some((
                PauseReason::LanAddressChanged { was: lan() },
                Resume::Automatic
            ))
        );

        let elsewhere = "198.51.100.7".parse().unwrap();
        let mut name = base.clone();
        name.dns = Some(DnsStatus {
            name: PublicHostname::parse("zec.example.com").unwrap(),
            provider: DnsProvider::Manual,
            has_token: false,
            resolved: vec![elsewhere],
            provider_saw: None,
            updated_at: None,
            problem: None,
            checked_at: Some(at(NOW)),
        });
        name.name_elsewhere_since = Some(at(NOW - 14 * 60));
        assert_eq!(pause(&spec, &name), None, "not yet 15 minutes");
        name.name_elsewhere_since = Some(at(NOW - 15 * 60));
        assert_eq!(
            pause(&spec, &name),
            Some((
                PauseReason::NameElsewhere {
                    points_to: vec![elsewhere]
                },
                Resume::Automatic
            ))
        );

        let capped = Facts {
            month_bytes_up: 300 * GB,
            ..base.clone()
        };
        assert_eq!(
            pause(&spec, &capped),
            Some((PauseReason::DataCap, Resume::NextMonth))
        );
        let uncapped = PublicServiceSpec {
            limits: launcher_types::public::PublicLimits {
                monthly_gb: None,
                ..spec.limits
            },
            ..spec.clone()
        };
        assert_eq!(pause(&uncapped, &capped), None);

        let failed = Facts {
            phase: Phase::Failed {
                class: ExitClass::EosHalt,
            },
            ..base.clone()
        };
        assert_eq!(
            pause(&spec, &failed),
            Some((PauseReason::BackingStopped, Resume::Automatic))
        );
        let lapsed = PublicServiceSpec {
            consent: Some(Consent {
                version: CONSENT_VERSION - 1,
                ..spec.consent.clone().unwrap()
            }),
            ..spec.clone()
        };
        assert_eq!(
            pause(&lapsed, &base),
            Some((PauseReason::ConsentNeeded, Resume::NeedsYou)),
            "a release that changed the consent asks again"
        );
        let held = Facts {
            held: Some(PauseReason::NotResumed),
            ..base
        };
        assert_eq!(
            pause(&spec, &held),
            Some((PauseReason::NotResumed, Resume::NeedsYou))
        );
    }

    #[test]
    fn the_name_points_here_only_from_the_home_network_without_a_vpn() {
        let spec = open_spec();
        let home = serving_facts().net.unwrap();
        assert!(may_point_name_here(&spec, &home));
        let vpn = NetFacts {
            vpn: Some("wg0".into()),
            ..home.clone()
        };
        assert!(!may_point_name_here(&spec, &vpn));
        let cafe = NetFacts {
            lan_addr: Some("10.0.4.17".parse().unwrap()),
            ..home.clone()
        };
        assert!(!may_point_name_here(&spec, &cafe));
        let unknown = NetFacts {
            lan_addr: None,
            ..home.clone()
        };
        assert!(!may_point_name_here(&spec, &unknown));
        // During the setup the LAN address is the network step's check.
        let setting_up = PublicServiceSpec {
            opened: false,
            ..spec
        };
        assert!(may_point_name_here(&setting_up, &cafe));
        assert!(!may_point_name_here(&setting_up, &vpn));
        // No default route with a gateway, or none that could be read.
        let no_route = NetFacts {
            lan_addr: None,
            gateway: None,
            interface: None,
            vpn: None,
            ..home.clone()
        };
        assert!(!may_point_name_here(&setting_up, &no_route));
        let no_gateway = NetFacts {
            gateway: None,
            ..home
        };
        assert!(!may_point_name_here(&setting_up, &no_gateway));
    }

    #[test]
    fn a_tunnels_default_ahead_of_the_routers_points_no_name_here() {
        // The network check's facts for a default without a gateway on tun0 at a lower metric
        // than the router's on wlan0, which asks no NAT-PMP question.
        let net = NetFacts {
            lan_addr: Some(lan()),
            gateway: Some("192.168.1.1".parse().unwrap()),
            interface: Some("wlan0".into()),
            vpn: Some("tun0".into()),
            wan: WanKind::Unknown,
            wan_addr: None,
            checked_at: at(NOW),
        };
        let spec = open_spec();
        assert!(!may_point_name_here(&spec, &net));
        let setting_up = PublicServiceSpec {
            opened: false,
            ..spec.clone()
        };
        assert!(!may_point_name_here(&setting_up, &net));
        let mut facts = serving_facts();
        facts.net = Some(net);
        assert_eq!(
            pause(&spec, &facts),
            Some((
                PauseReason::VpnRoute {
                    interface: "tun0".into()
                },
                Resume::Automatic
            ))
        );
    }

    #[test]
    fn a_name_pointing_elsewhere_holds_until_a_resolution_says_otherwise() {
        let spec = open_spec();
        let mut facts = serving_facts();
        let dns = |resolved: &str, checked: bool| DnsStatus {
            name: PublicHostname::parse("zec.example.com").unwrap(),
            provider: DnsProvider::Manual,
            has_token: false,
            resolved: vec![resolved.parse().unwrap()],
            provider_saw: None,
            updated_at: None,
            problem: None,
            checked_at: checked.then(|| at(NOW)),
        };
        facts.dns = Some(dns("198.51.100.7", true));
        assert_eq!(name_elsewhere_since(&spec, &facts), Some(at(NOW)));
        facts.name_elsewhere_since = Some(at(NOW - 20 * 60));
        // Paused: no resolution, or none yet from a new resolver. It still points away.
        facts.dns = None;
        assert_eq!(name_elsewhere_since(&spec, &facts), Some(at(NOW - 20 * 60)));
        facts.dns = Some(dns("198.51.100.7", false));
        assert_eq!(name_elsewhere_since(&spec, &facts), Some(at(NOW - 20 * 60)));
        facts.dns = Some(dns("203.0.113.10", true));
        assert_eq!(
            name_elsewhere_since(&spec, &facts),
            None,
            "points here again"
        );
        let setting_up = PublicServiceSpec {
            opened: false,
            ..spec
        };
        facts.name_elsewhere_since = Some(at(NOW - 20 * 60));
        assert_eq!(name_elsewhere_since(&setting_up, &facts), None);
    }

    #[test]
    fn pauses_that_end_by_themselves_keep_watching() {
        let paused = |why, resumes| PublicState::Paused {
            why,
            since: at(0),
            resumes,
        };
        let vpn = PauseReason::VpnRoute {
            interface: "wg0".into(),
        };
        assert!(watches_surroundings(&paused(vpn, Resume::Automatic)));
        assert!(watches_surroundings(&paused(
            PauseReason::DataCap,
            Resume::NextMonth
        )));
        assert!(!watches_surroundings(&paused(
            PauseReason::ByYou,
            Resume::NeedsYou
        )));
        assert!(!watches_surroundings(&paused(
            PauseReason::NotResumed,
            Resume::NeedsYou
        )));
        assert!(watches_surroundings(&PublicState::Serving));
        assert!(!watches_surroundings(&PublicState::Off));
    }

    #[test]
    fn during_the_setup_only_the_switch_and_network_pause() {
        let spec = PublicServiceSpec {
            opened: false,
            ..open_spec()
        };
        let mut facts = serving_facts();
        facts.net.as_mut().unwrap().vpn = Some("tun0".into());
        facts.phase = Phase::Stopped;
        assert_eq!(pause(&spec, &facts), None);
    }

    #[test]
    fn passing_problems_degrade() {
        let spec = open_spec();
        let base = serving_facts();
        let restarting = Facts {
            phase: Phase::Starting,
            endpoint: None,
            ..base.clone()
        };
        assert_eq!(
            degraded(&spec, &restarting),
            Some(DegradedReason::BackingRestarting)
        );
        let behind = Facts {
            height: Some(3_544_700),
            ..base.clone()
        };
        assert_eq!(
            degraded(&spec, &behind),
            Some(DegradedReason::BackingBehind { blocks: 113 })
        );
        // zebrad's estimate runs ahead during an ordinary gap between blocks.
        let mut gap = Facts {
            height: Some(3_544_810),
            behind_since: Some(at(NOW - 9 * 60)),
            ..base.clone()
        };
        assert_eq!(degraded(&spec, &gap), None, "3 behind for 9 minutes");
        gap.behind_since = Some(at(NOW - 10 * 60));
        assert_eq!(
            degraded(&spec, &gap),
            Some(DegradedReason::BackingBehind { blocks: 3 })
        );
        let far = Facts {
            height: Some(3_544_803),
            behind_since: Some(at(NOW)),
            ..base.clone()
        };
        assert_eq!(
            degraded(&spec, &far),
            Some(DegradedReason::BackingBehind { blocks: 10 }),
            "10 behind at once"
        );
        let unknown = Facts {
            height: None,
            ..base.clone()
        };
        assert_eq!(
            degraded(&spec, &unknown),
            Some(DegradedReason::BackingBehind { blocks: 0 })
        );
        let wrong = Facts {
            wallet: Some(Checked {
                at: at(NOW),
                result: Err(WalletProblem::NoAnswer {
                    detail: "reset".into(),
                }),
            }),
            ..base.clone()
        };
        assert_eq!(
            degraded(&spec, &wrong),
            Some(DegradedReason::WalletCheckFailed)
        );
        let mut expired = base;
        expired.cert.as_mut().unwrap().not_after = Some(at(NOW));
        assert_eq!(
            degraded(&spec, &expired),
            Some(DegradedReason::CertificateExpired)
        );
    }
}
