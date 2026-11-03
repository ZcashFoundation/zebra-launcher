//! Evidence that the service is reachable from the internet, and how its age reads.

use std::time::{Duration, SystemTime};

use launcher_types::public::{Evidence, Reachability};

/// Evidence younger than this reads "reachable".
pub const PROVEN_FOR: Duration = Duration::from_secs(24 * 3600);

/// Evidence up to this old reads "last confirmed …"; older is stale.
pub const RECENT_FOR: Duration = Duration::from_secs(7 * 24 * 3600);

/// The newest of `evidence`.
pub fn latest(evidence: &[Evidence]) -> Option<Evidence> {
    evidence.iter().copied().max_by_key(|evidence| evidence.at)
}

/// What `evidence` proves at `now`. `checked` says whether a check from outside has run at
/// all (a staging order, an open service), so "not yet" differs from "not checked".
pub fn reachability(evidence: &[Evidence], checked: bool, now: SystemTime) -> Reachability {
    let Some(latest) = latest(evidence) else {
        return if checked {
            Reachability::NotYet
        } else {
            Reachability::NotChecked
        };
    };
    let age = now.duration_since(latest.at).unwrap_or_default();
    if age < PROVEN_FOR {
        Reachability::Proven { latest }
    } else if age <= RECENT_FOR {
        Reachability::Recent { latest }
    } else {
        Reachability::Stale { latest }
    }
}

/// Adds `new` to `evidence`, keeping the newest of each source only.
pub fn record(evidence: &mut Vec<Evidence>, new: Evidence) {
    match evidence.iter_mut().find(|old| old.source == new.source) {
        Some(old) if old.at < new.at => *old = new,
        Some(_) => {}
        None => evidence.push(new),
    }
}

#[cfg(test)]
mod tests {
    use launcher_types::public::EvidenceSource;

    use super::*;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn evidence_ages_from_proven_to_stale() {
        let day = 24 * 3600;
        let now = at(100 * day);
        assert_eq!(reachability(&[], false, now), Reachability::NotChecked);
        assert_eq!(reachability(&[], true, now), Reachability::NotYet);
        let staging = Evidence {
            source: EvidenceSource::TlsAlpn01Validation,
            at: at(100 * day - 3600),
        };
        assert_eq!(
            reachability(&[staging], true, now),
            Reachability::Proven { latest: staging }
        );
        let older = Evidence {
            at: at(97 * day),
            ..staging
        };
        assert_eq!(
            reachability(&[older], true, now),
            Reachability::Recent { latest: older }
        );
        let stale = Evidence {
            at: at(92 * day),
            ..staging
        };
        assert_eq!(
            reachability(&[stale], true, now),
            Reachability::Stale { latest: stale }
        );
        // The newest wins.
        let outside = Evidence {
            source: EvidenceSource::OutsideTlsConnection,
            at: at(100 * day - 60),
        };
        assert_eq!(
            reachability(&[stale, outside], true, now),
            Reachability::Proven { latest: outside }
        );
    }

    #[test]
    fn recording_keeps_the_newest_per_source() {
        let mut evidence = Vec::new();
        let first = Evidence {
            source: EvidenceSource::TlsAlpn01Validation,
            at: at(10),
        };
        record(&mut evidence, first);
        record(&mut evidence, Evidence { at: at(5), ..first });
        assert_eq!(evidence, [first]);
        let newer = Evidence {
            at: at(20),
            ..first
        };
        record(&mut evidence, newer);
        let outside = Evidence {
            source: EvidenceSource::OutsideTlsConnection,
            at: at(15),
        };
        record(&mut evidence, outside);
        assert_eq!(evidence, [newer, outside]);
    }
}
