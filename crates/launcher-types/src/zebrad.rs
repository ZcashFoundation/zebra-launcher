//! Bundled Zebra channels, their pinned builds, and offline documentation.

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

/// A zebrad build bundled with Zeal. Channels are fixed when a node is created so a newer
/// Zebra database is never reopened by an older channel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ZebradChannel {
    /// The current stable Zebra release.
    #[default]
    Stable,
    /// A prerelease newer than stable.
    Beta,
    /// A snapshot of Zebra development.
    Nightly,
}

impl ZebradChannel {
    /// Every channel, in the order the UI offers them.
    pub const ALL: [ZebradChannel; 3] = [
        ZebradChannel::Stable,
        ZebradChannel::Beta,
        ZebradChannel::Nightly,
    ];

    /// The channel's name on the wire and in resource directories.
    pub fn as_str(self) -> &'static str {
        match self {
            ZebradChannel::Stable => "stable",
            ZebradChannel::Beta => "beta",
            ZebradChannel::Nightly => "nightly",
        }
    }

    /// The channel's name for people.
    pub fn label(self) -> &'static str {
        match self {
            ZebradChannel::Stable => "Stable",
            ZebradChannel::Beta => "Beta",
            ZebradChannel::Nightly => "Nightly",
        }
    }

    /// The pinned zebrad version, without build metadata.
    pub fn version(self) -> &'static str {
        match self {
            ZebradChannel::Stable => include_str!("../../../ZEBRAD_VERSION"),
            ZebradChannel::Beta => include_str!("../../../ZEBRAD_BETA_VERSION"),
            ZebradChannel::Nightly => include_str!("../../../ZEBRAD_NIGHTLY_VERSION"),
        }
        .trim_ascii()
    }

    /// The full Zebra commit hash pinned for this channel.
    pub fn commit(self) -> &'static str {
        match self {
            ZebradChannel::Stable => include_str!("../../../ZEBRAD_COMMIT"),
            ZebradChannel::Beta => include_str!("../../../ZEBRAD_BETA_COMMIT"),
            ZebradChannel::Nightly => include_str!("../../../ZEBRAD_NIGHTLY_COMMIT"),
        }
        .trim_ascii()
    }

    /// The channel build's release or snapshot date, as `YYYY-MM-DD`.
    pub fn date(self) -> &'static str {
        match self {
            ZebradChannel::Stable => include_str!("../../../ZEBRAD_DATE"),
            ZebradChannel::Beta => include_str!("../../../ZEBRAD_BETA_DATE"),
            ZebradChannel::Nightly => include_str!("../../../ZEBRAD_NIGHTLY_DATE"),
        }
        .trim_ascii()
    }

    /// The customary seven-character abbreviation of [`Self::commit`].
    pub fn short_commit(self) -> &'static str {
        &self.commit()[..7]
    }

    /// The executable name used by the packaged sidecar.
    pub fn sidecar_name(self) -> &'static str {
        match self {
            ZebradChannel::Stable => "zeal-zebrad",
            ZebradChannel::Beta => "zeal-zebrad-beta",
            ZebradChannel::Nightly => "zeal-zebrad-nightly",
        }
    }

    /// Whether this channel can be selected for a new node. Beta is offered only while its pin
    /// is SemVer-newer than stable; an invalid pin fails closed.
    pub fn is_available(self) -> bool {
        match self {
            ZebradChannel::Stable | ZebradChannel::Nightly => true,
            ZebradChannel::Beta => {
                semver_newer(self.version(), ZebradChannel::Stable.version()).unwrap_or(false)
            }
        }
    }

    /// Whether `reported` is this channel's pinned version. Zebra's optional leading `v` and
    /// SemVer build metadata without a commit are accepted; the core and prerelease identifiers
    /// must match exactly. A `+git.<hash>` build must report at least seven hexadecimal characters
    /// that prefix this channel's pinned commit.
    pub fn version_matches(self, reported: &str) -> bool {
        match (parse_semver(self.version()), parse_semver(reported.trim())) {
            (Some(expected), Some(reported)) => {
                expected.core == reported.core
                    && expected.prerelease == reported.prerelease
                    && reported
                        .build
                        .is_none_or(|build| commit_matches(self.commit(), build))
            }
            _ => false,
        }
    }
}

/// Offline documentation for a node's Zebra channel. Paths are absolute HTML paths in Tauri
/// and illustrative paths in the browser mock.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeDocs {
    pub channel: ZebradChannel,
    pub book: String,
    pub config: String,
    pub changes: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SemVer<'a> {
    core: [&'a str; 3],
    prerelease: Option<&'a str>,
    build: Option<&'a str>,
}

fn parse_semver(version: &str) -> Option<SemVer<'_>> {
    let version = version.strip_prefix('v').unwrap_or(version);
    let (version, build) = match version.split_once('+') {
        Some((version, build)) => (version, Some(build)),
        None => (version, None),
    };
    if build.is_some_and(|build| !valid_identifiers(build, false)) {
        return None;
    }

    let (core, prerelease) = match version.split_once('-') {
        Some((core, prerelease)) => (core, Some(prerelease)),
        None => (version, None),
    };
    if prerelease.is_some_and(|prerelease| !valid_identifiers(prerelease, true)) {
        return None;
    }

    let mut parts = core.split('.');
    let core = [parts.next()?, parts.next()?, parts.next()?];
    if parts.next().is_some() || !core.iter().all(|part| valid_numeric(part)) {
        return None;
    }
    Some(SemVer {
        core,
        prerelease,
        build,
    })
}

fn commit_matches(pinned: &str, build: &str) -> bool {
    let Some(build) = build.strip_prefix("git.") else {
        return true;
    };
    let reported = build.split('.').next().unwrap_or_default();
    reported.len() >= 7
        && reported.bytes().all(|byte| byte.is_ascii_hexdigit())
        && pinned
            .get(..reported.len())
            .is_some_and(|expected| expected.eq_ignore_ascii_case(reported))
}

fn valid_numeric(identifier: &str) -> bool {
    is_numeric(identifier) && (identifier == "0" || !identifier.starts_with('0'))
}

fn is_numeric(identifier: &str) -> bool {
    !identifier.is_empty() && identifier.bytes().all(|byte| byte.is_ascii_digit())
}

fn valid_identifiers(identifiers: &str, prerelease: bool) -> bool {
    !identifiers.is_empty()
        && identifiers.split('.').all(|identifier| {
            !identifier.is_empty()
                && identifier
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && (!prerelease || !is_numeric(identifier) || valid_numeric(identifier))
        })
}

fn numeric_cmp(left: &str, right: &str) -> Ordering {
    left.len().cmp(&right.len()).then_with(|| left.cmp(right))
}

fn prerelease_cmp(left: &str, right: &str) -> Ordering {
    let mut left = left.split('.');
    let mut right = right.split('.');
    loop {
        match (left.next(), right.next()) {
            (Some(left), Some(right)) => {
                let ordering = match (is_numeric(left), is_numeric(right)) {
                    (true, true) => numeric_cmp(left, right),
                    (true, false) => Ordering::Less,
                    (false, true) => Ordering::Greater,
                    (false, false) => left.cmp(right),
                };
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
        }
    }
}

fn semver_newer(candidate: &str, current: &str) -> Option<bool> {
    let candidate = parse_semver(candidate)?;
    let current = parse_semver(current)?;
    for (candidate, current) in candidate.core.iter().zip(current.core.iter()) {
        let ordering = numeric_cmp(candidate, current);
        if ordering != Ordering::Equal {
            return Some(ordering == Ordering::Greater);
        }
    }
    let ordering = match (candidate.prerelease, current.prerelease) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(candidate), Some(current)) => prerelease_cmp(candidate, current),
    };
    Some(ordering == Ordering::Greater)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{Network, NodeDraft, NodeKind, NodeProgram};

    #[test]
    fn channel_metadata_comes_from_the_pins() {
        let expected = [
            (
                ZebradChannel::Stable,
                "stable",
                "Stable",
                "7.0.0",
                "e8db87eb69254e0d4eedd058eadd48bdfa09065a",
                "2026-10-22",
                "e8db87e",
                "zeal-zebrad",
            ),
            (
                ZebradChannel::Beta,
                "beta",
                "Beta",
                "7.1.0-alpha.0",
                "9889683b9abecd5d28f2de1a49d1a359be11b8eb",
                "2026-10-30",
                "9889683",
                "zeal-zebrad-beta",
            ),
            (
                ZebradChannel::Nightly,
                "nightly",
                "Nightly",
                "7.1.0-alpha.0",
                "7ae1a827471235e8d14204e2aaa77dc9b61ca25c",
                "2026-11-02",
                "7ae1a82",
                "zeal-zebrad-nightly",
            ),
        ];
        assert_eq!(
            ZebradChannel::ALL.map(|channel| channel),
            expected.map(|row| row.0)
        );
        for (channel, name, label, version, commit, date, short_commit, sidecar) in expected {
            assert_eq!(channel.as_str(), name);
            assert_eq!(channel.label(), label);
            assert_eq!(channel.version(), version);
            assert_eq!(channel.commit(), commit);
            assert_eq!(channel.date(), date);
            assert_eq!(channel.short_commit(), short_commit);
            assert_eq!(channel.sidecar_name(), sidecar);
            assert!(channel.is_available(), "{channel:?}");
            assert_eq!(serde_json::to_value(channel).unwrap(), name);
            assert_eq!(
                serde_json::from_value::<ZebradChannel>(json!(name)).unwrap(),
                channel
            );
        }
        assert_eq!(ZebradChannel::default(), ZebradChannel::Stable);
    }

    #[test]
    fn beta_availability_uses_semver_precedence() {
        let precedence = [
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0",
        ];
        for pair in precedence.windows(2) {
            assert_eq!(semver_newer(pair[0], pair[1]), Some(false), "{pair:?}");
            assert_eq!(semver_newer(pair[1], pair[0]), Some(true), "{pair:?}");
        }
        assert_eq!(semver_newer("7.1.0-alpha.0", "7.0.0"), Some(true));
        assert_eq!(semver_newer("7.0.0-alpha.1", "7.0.0"), Some(false));
        assert_eq!(semver_newer("7.0.0+new", "7.0.0+old"), Some(false));
        assert_eq!(
            semver_newer("999999999999999999999.0.0", "2.0.0"),
            Some(true),
            "numeric identifiers are not limited to machine integers"
        );
        for malformed in [
            "",
            "1.0",
            "1.0.0.0",
            "1.01.0",
            "1.0.0-01",
            "1.0.0-",
            "1.0.0+",
            "1.0.0+bad..metadata",
        ] {
            assert_eq!(semver_newer(malformed, "1.0.0"), None, "{malformed:?}");
        }
    }

    #[test]
    fn version_matching_checks_the_base_and_any_reported_commit() {
        for channel in ZebradChannel::ALL {
            let version = channel.version();
            let commit = channel.short_commit();
            let other = ZebradChannel::ALL
                .into_iter()
                .find(|other| *other != channel)
                .expect("another channel");
            assert!(channel.version_matches(version));
            assert!(channel.version_matches(&format!("v{version}")));
            assert!(channel.version_matches(&format!("{version}+build.7")));
            assert!(channel.version_matches(&format!("{version}+git.{commit}")));
            assert!(channel.version_matches(&format!(" v{version}+git.{commit} ")));
            assert!(channel.version_matches(&format!("{version}+git.{}.dirty", channel.commit())));
            assert!(!channel.version_matches(&format!("{version}+git.{}", other.short_commit())));
            assert!(!channel.version_matches(&format!("{version}+git.{}", &commit[..6])));
            assert!(!channel.version_matches(&format!("{version}+git.not-a-hash")));
            assert!(!channel.version_matches(&format!("{version}-modified+git.{commit}")));
            assert!(!channel.version_matches(&format!("{version}+")));
            assert!(!channel.version_matches(&format!("zebrad {version}")));
        }

        let beta = ZebradChannel::Beta;
        let nightly = ZebradChannel::Nightly;
        assert_eq!(beta.version(), nightly.version());
        assert!(!beta.version_matches(&format!(
            "{}+git.{}",
            beta.version(),
            nightly.short_commit()
        )));
        assert!(!nightly.version_matches(&format!(
            "{}+git.{}",
            nightly.version(),
            beta.short_commit()
        )));
        assert!(!ZebradChannel::Stable.version_matches(&format!(
            "7.0.1+git.{}",
            ZebradChannel::Stable.short_commit()
        )));
        assert!(!ZebradChannel::Stable.version_matches(&format!(
            "7.0.0-rc.0+git.{}",
            ZebradChannel::Stable.short_commit()
        )));
        assert!(
            !ZebradChannel::Beta.version_matches(&format!("7.1.0+git.{}", beta.short_commit()))
        );
        assert!(
            !ZebradChannel::Nightly
                .version_matches(&format!("7.1.0-alpha.1+git.{}", nightly.short_commit()))
        );
    }

    #[test]
    fn node_programs_keep_their_wire_shapes() {
        for (program, name, channel) in [
            (NodeProgram::Bundled, "bundled", ZebradChannel::Stable),
            (
                NodeProgram::BundledBeta,
                "bundled_beta",
                ZebradChannel::Beta,
            ),
            (
                NodeProgram::BundledNightly,
                "bundled_nightly",
                ZebradChannel::Nightly,
            ),
        ] {
            assert_eq!(program.channel(), channel);
            assert_eq!(NodeProgram::from_channel(channel), program);
            let wire = json!({"from": name});
            assert_eq!(serde_json::to_value(program).unwrap(), wire);
            assert_eq!(
                serde_json::from_value::<NodeProgram>(wire).unwrap(),
                program
            );
        }
        assert_eq!(NodeProgram::default(), NodeProgram::Bundled);

        let draft: NodeDraft = serde_json::from_value(json!({
            "kind": "zebrad",
            "network": "testnet",
            "name": "Zebra · Testnet",
        }))
        .expect("a draft without a program uses stable");
        assert_eq!(draft.program, NodeProgram::Bundled);
        assert_eq!(draft.network, Network::Testnet);
        assert_eq!(draft.kind, NodeKind::Zebrad);
    }

    #[test]
    fn node_docs_round_trip() {
        let docs = NodeDocs {
            channel: ZebradChannel::Beta,
            book: "/resources/zebra-docs/beta/book/index.html".into(),
            config: "/resources/zebra-docs/beta/config.html".into(),
            changes: "/resources/zebra-docs/beta/changes.html".into(),
        };
        let wire = serde_json::to_value(&docs).unwrap();
        assert_eq!(
            wire,
            json!({
                "channel": "beta",
                "book": "/resources/zebra-docs/beta/book/index.html",
                "config": "/resources/zebra-docs/beta/config.html",
                "changes": "/resources/zebra-docs/beta/changes.html",
            })
        );
        assert_eq!(serde_json::from_value::<NodeDocs>(wire).unwrap(), docs);
    }
}
