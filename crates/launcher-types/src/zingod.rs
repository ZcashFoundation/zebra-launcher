//! The zingod build Zeal bundles on Linux, read from the root `ZINGOD_*` pins when this crate
//! is built, and what zingod's `GET /status` says ([`IndexerStatus`]).

use serde::{Deserialize, Serialize};

/// The pinned zingod version, without a leading `v`.
pub fn version() -> &'static str {
    include_str!("../../../ZINGOD_VERSION").trim_ascii()
}

/// The full zingod commit that the version's tag names.
pub fn commit() -> &'static str {
    include_str!("../../../ZINGOD_COMMIT").trim_ascii()
}

/// The pinned commit's date in UTC, as `YYYY-MM-DD`.
pub fn date() -> &'static str {
    include_str!("../../../ZINGOD_DATE").trim_ascii()
}

/// An indexer's `/status`, parsed: zingod's [`ZingodStatus`] through `From`. Integers only,
/// so the types that hold it keep `Eq`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexerStatus {
    pub state: IndexerState,
    /// The height its methods answer at; `None` before its boot.
    pub served: Option<u64>,
    /// The verified best block: the tip its validators vouch for, so always the validator's.
    pub tip: Option<u64>,
    /// The network as the indexer names it: `mainnet`, `testnet` or `regtest`.
    pub network: String,
    /// Its trusted validators, and how many answer; `None` before its boot.
    pub trusted: Option<u32>,
    pub answering: Option<u32>,
    /// Its own alarms, as it words them.
    pub alarms: Vec<String>,
    /// Its version, as `/status` reports it.
    pub version: String,
}

/// What the indexer says it is doing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum IndexerState {
    /// Opening and checking its index before it serves.
    Provisioning,
    /// Building or catching up its index. `None` while it has no sync target yet.
    Syncing { progress: Option<SyncProgress> },
    /// Serving at the verified tip.
    Serving,
    /// Serving a fresh tip that no trusted validator vouches for.
    Degraded { reason: String, message: String },
    /// Not serving, with its reason.
    Down { reason: String, message: String },
    /// Held by its operator (`zingod hold`), which Zeal never does.
    Held { reason: String, message: String },
    /// A state this version of Zeal doesn't know.
    Other { name: String },
}

/// How far a syncing indexer is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncProgress {
    /// The height it serves, or else the one it reached.
    pub height: u64,
    /// The height it syncs to; `None` when the answer had none.
    pub target: Option<u64>,
    /// Its rate over the last minute, in thousandths of a block a second; `None` before it has
    /// one.
    pub milli_blocks_per_sec: Option<u64>,
}

/// zingod's `/status` as it sends it (`packages/zingod/src/health.rs` at 1.1.0), unknown fields
/// ignored. Turn it into an [`IndexerStatus`] with `From`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ZingodStatus {
    state: String,
    #[serde(default)]
    detail: Option<Detail>,
    chain: Chain,
    build: Build,
}

/// Every shape of `detail` at once: syncing's height, target and rate, and the reason and
/// message of `down`, `degraded` and `held`. Provisioning's and serving's details are left out.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
struct Detail {
    height: Option<u64>,
    target: Option<u64>,
    blocks_per_sec: Option<f64>,
    reason: Option<String>,
    message: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
struct Chain {
    network: String,
    #[serde(default)]
    tip: Option<Block>,
    #[serde(default)]
    served: Option<u64>,
    #[serde(default)]
    validators: Option<Validators>,
    #[serde(default)]
    alarms: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
struct Block {
    height: u64,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
struct Validators {
    trusted: u32,
    answering: u32,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
struct Build {
    version: String,
}

impl From<ZingodStatus> for IndexerStatus {
    fn from(status: ZingodStatus) -> Self {
        let detail = status.detail.unwrap_or_default();
        let why = |detail: Detail| {
            (
                detail.reason.unwrap_or_default(),
                detail.message.unwrap_or_default(),
            )
        };
        let state = match status.state.as_str() {
            "provisioning" => IndexerState::Provisioning,
            "syncing" => IndexerState::Syncing {
                progress: detail.height.map(|height| SyncProgress {
                    height,
                    target: detail.target,
                    milli_blocks_per_sec: detail.blocks_per_sec.and_then(milli),
                }),
            },
            "serving" => IndexerState::Serving,
            "degraded" => {
                let (reason, message) = why(detail);
                IndexerState::Degraded { reason, message }
            }
            "down" => {
                let (reason, message) = why(detail);
                IndexerState::Down { reason, message }
            }
            "held" => {
                let (reason, message) = why(detail);
                IndexerState::Held { reason, message }
            }
            _ => IndexerState::Other { name: status.state },
        };
        IndexerStatus {
            state,
            served: status.chain.served,
            tip: status.chain.tip.map(|tip| tip.height),
            network: status.chain.network,
            trusted: status.chain.validators.as_ref().map(|v| v.trusted),
            answering: status.chain.validators.as_ref().map(|v| v.answering),
            alarms: status.chain.alarms,
            version: status.build.version,
        }
    }
}

/// `rate` blocks a second in thousandths, rounded; `None` for a rate that isn't one.
fn milli(rate: f64) -> Option<u64> {
    let milli = (rate * 1000.0).round();
    // Below 2^53 every integer is exact, which is far past any real rate.
    (milli.is_finite() && (0.0..9.0e15).contains(&milli)).then_some(milli as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_build_comes_from_the_pins() {
        assert_eq!(super::version(), "1.1.0");
        assert_eq!(super::commit(), "bfce8b2b433578054dec82928d2707e523ec75b7");
        assert_eq!(super::date(), "2026-11-10");
    }

    fn parse(json: &str) -> IndexerStatus {
        serde_json::from_str::<ZingodStatus>(json)
            .expect("zingod's /status")
            .into()
    }

    macro_rules! fixture {
        ($name:literal) => {
            include_str!(concat!(
                "../../launcher-core/tests/fixtures/zingod-1.1.0-status-",
                $name,
                ".json"
            ))
        };
    }

    #[test]
    fn captured_answers_parse() {
        let syncing = parse(fixture!("syncing-null"));
        assert_eq!(syncing.state, IndexerState::Syncing { progress: None });
        assert_eq!(
            (syncing.served, syncing.tip, syncing.network.as_str()),
            (None, None, "testnet")
        );
        assert_eq!((syncing.trusted, syncing.answering), (Some(1), Some(0)));
        assert_eq!(syncing.version, "1.1.0");

        // At zebrad's tip of 3 blocks, with zingod's clock estimate as the target: Regtest's
        // genesis is old.
        let at_tip = parse(fixture!("syncing-tip"));
        let IndexerState::Syncing {
            progress: Some(progress),
        } = &at_tip.state
        else {
            panic!("{at_tip:?}");
        };
        assert_eq!(progress.height, 3);
        assert!(progress.target.is_some_and(|target| target > 1_000_000));
        assert_eq!(progress.milli_blocks_per_sec, None);
        assert_eq!((at_tip.served, at_tip.tip), (Some(3), Some(3)));

        let serving = parse(fixture!("serving"));
        assert_eq!(serving.state, IndexerState::Serving);
        assert_eq!(serving.alarms, ["1 validator on a stale tip"]);
        assert_eq!((serving.trusted, serving.answering), (Some(1), Some(1)));

        let waiting = parse(fixture!("no-validator"));
        assert_eq!(
            waiting.state,
            IndexerState::Down {
                reason: "no_validator_reachable".into(),
                message: "No trusted validator reachable".into(),
            }
        );
        assert_eq!((waiting.trusted, waiting.answering), (None, None));
    }

    #[test]
    fn the_rate_is_kept_in_thousandths() {
        let json = r#"{"state":"syncing","detail":{"height":10,"target":20,"blocks_per_sec":6.2},
            "chain":{"network":"testnet","tip":null,"served":null,"validators":null,"alarms":[]},
            "build":{"version":"1.1.0"}}"#;
        assert_eq!(
            parse(json).state,
            IndexerState::Syncing {
                progress: Some(SyncProgress {
                    height: 10,
                    target: Some(20),
                    milli_blocks_per_sec: Some(6_200),
                })
            }
        );
        assert_eq!(milli(-1.0), None);
        assert_eq!(milli(f64::NAN), None);
        assert_eq!(milli(0.0004), Some(0));
    }

    #[test]
    fn unknown_states_and_fields_still_parse() {
        let json = r#"{"state":"resting","detail":{"phase":"x"},"new":1,
            "chain":{"network":"mainnet","alarms":[]},"build":{"version":"9.0.0","commit":"x"}}"#;
        assert_eq!(
            parse(json).state,
            IndexerState::Other {
                name: "resting".into()
            }
        );
        let provisioning = r#"{"state":"provisioning","detail":{"phase":"downloading","bytes":4,"total_bytes":9,"bytes_per_sec":null},
            "chain":{"network":"mainnet","tip":null,"served":null,"validators":null,"alarms":[]},"build":{"version":"1.1.0"}}"#;
        assert_eq!(parse(provisioning).state, IndexerState::Provisioning);
    }

    #[test]
    fn the_status_round_trips_on_the_wire() {
        let status = parse(fixture!("syncing-tip"));
        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(
            serde_json::from_str::<IndexerStatus>(&json).unwrap(),
            status
        );
        assert_eq!(
            serde_json::to_value(IndexerState::Serving).unwrap(),
            serde_json::json!({"state": "serving"})
        );
    }
}
