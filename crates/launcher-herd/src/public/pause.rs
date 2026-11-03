//! Automatic pauses and degraded service.
//!
//! Problems with the exposure itself (the switch, the network, the LAN address, the name, a
//! VPN, the data cap, a stopped zebrad) close the front door: a pause. Problems that clear
//! themselves (zebrad restarting or behind, a failed wallet check, an expired certificate)
//! keep it up and tell wallets to try again, so Let's Encrypt's renewals still get through.

use std::time::Duration;

use launcher_types::{
    Phase,
    public::{DegradedReason, PauseReason, PublicServiceSpec, Resume},
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
    if spec.network != facts.network {
        return Some((PauseReason::NetworkChanged, Resume::Automatic));
    }
    if !spec.opened {
        return None;
    }
    if facts.held_at_launch {
        return Some((PauseReason::NotResumed, Resume::NeedsYou));
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
    if !light::at_tip(facts.height, facts.tip) {
        let blocks = match (facts.height, facts.tip) {
            (Some(height), Some(tip)) => tip.saturating_sub(height),
            _ => 0,
        };
        return Some(DegradedReason::BackingBehind { blocks });
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
    use std::{net::IpAddr, time::SystemTime};

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
            Some((PauseReason::NetworkChanged, Resume::Automatic))
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
        let held = Facts {
            held_at_launch: true,
            ..base
        };
        assert_eq!(
            pause(&spec, &held),
            Some((PauseReason::NotResumed, Resume::NeedsYou))
        );
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
