//! The light client server's backend: when zebrad's endpoint should exist, and whether zebrad
//! is at the tip.

use std::time::{Duration, SystemTime};

use launcher_types::{Network, public::PublicServiceSpec};

use super::Facts;

/// zebrad within this many blocks of its estimate of the network's tip is at the tip.
pub const AT_TIP_BLOCKS: u64 = 2;

/// How long zebrad stays at the tip before the service may open.
pub const TIP_SETTLED: Duration = Duration::from_secs(10 * 60);

/// Whether zebrad is at the tip.
pub fn at_tip(height: Option<u64>, tip: Option<u64>) -> bool {
    match (height, tip) {
        (Some(height), Some(tip)) => tip.saturating_sub(height) <= AT_TIP_BLOCKS,
        _ => false,
    }
}

/// The start of an unbroken stretch of time while a condition holds: `since` while it keeps
/// holding, `now` when it starts to, and `None` when it stops.
pub fn track_since(since: Option<SystemTime>, holds: bool, now: SystemTime) -> Option<SystemTime> {
    if holds {
        Some(since.unwrap_or(now))
    } else {
        None
    }
}

/// Whether zebrad has been at the tip for [`TIP_SETTLED`].
pub fn tip_settled(facts: &Facts) -> bool {
    facts
        .at_tip_since
        .is_some_and(|since| facts.now.duration_since(since).unwrap_or_default() >= TIP_SETTLED)
}

/// Whether zebrad's light wallet endpoint should exist now: the service is switched on, the
/// consent covers it, the launcher runs zebrad on the service's network, and no pause waits
/// for the user. Other automatic pauses keep the endpoint, so a resume needs no restart.
pub fn wants_endpoint(spec: &PublicServiceSpec, facts: &Facts) -> bool {
    spec.switch_on
        && spec.consent_current()
        && spec.network == facts.network
        && facts.held.is_none()
        && facts.supported
        && facts.zebrad_tested()
}

/// The loopback port for the endpoint on `network`: the network's default, or the next one
/// above it that `taken` does not hold.
pub fn pick_endpoint_port(network: Network, taken: impl Fn(u16) -> bool) -> Option<u16> {
    let first = launcher_types::public::endpoint_port(network);
    (first..=first.saturating_add(100)).find(|port| !taken(*port))
}

#[cfg(test)]
mod tests {
    use launcher_types::{
        NodeId,
        public::{CONSENT_VERSION, Consent},
    };

    use super::*;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn at_the_tip_within_two_blocks() {
        assert!(at_tip(Some(100), Some(102)));
        assert!(at_tip(Some(103), Some(102)));
        assert!(!at_tip(Some(100), Some(103)));
        assert!(!at_tip(None, Some(1)));
    }

    #[test]
    fn stretches_restart_after_a_break() {
        let since = track_since(None, true, at(10));
        assert_eq!(since, Some(at(10)));
        assert_eq!(track_since(since, true, at(20)), Some(at(10)));
        assert_eq!(track_since(since, false, at(30)), None);
        assert_eq!(track_since(None, true, at(40)), Some(at(40)));
    }

    #[test]
    fn the_endpoint_exists_only_while_switched_on_with_consent_on_its_network() {
        let mut spec = PublicServiceSpec::new_light_server(
            NodeId::new("zebrad-mainnet").unwrap(),
            Network::Mainnet,
        );
        let facts = Facts::new(at(0), Network::Mainnet);
        assert!(!wants_endpoint(&spec, &facts), "no consent yet");
        spec.consent = Some(Consent {
            version: CONSENT_VERSION,
            at: at(0),
            hostname: String::new(),
        });
        assert!(wants_endpoint(&spec, &facts));
        assert!(!wants_endpoint(&spec, &Facts::new(at(0), Network::Testnet)));
        let held = Facts {
            held: Some(launcher_types::public::PauseReason::NotResumed),
            ..facts.clone()
        };
        assert!(!wants_endpoint(&spec, &held));
        spec.switch_on = false;
        assert!(!wants_endpoint(&spec, &facts));
        spec.switch_on = true;
        let untested = Facts {
            zebrad_version: Some("6.4.0".into()),
            ..facts.clone()
        };
        assert!(!wants_endpoint(&spec, &untested));
    }

    #[test]
    fn endpoint_ports_skip_taken_ones() {
        assert_eq!(pick_endpoint_port(Network::Mainnet, |_| false), Some(9067));
        assert_eq!(
            pick_endpoint_port(Network::Testnet, |port| port < 19069),
            Some(19069)
        );
        assert_eq!(pick_endpoint_port(Network::Mainnet, |_| true), None);
    }
}
