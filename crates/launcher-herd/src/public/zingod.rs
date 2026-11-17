//! zingod as the light client server's backend (docs/PUBLIC-SERVICES.md, "zingod as the
//! backend"): which pairs Zeal tested, when zingod runs ([`want`]), the restart with each zebrad
//! run, the stall rule, the disk's hysteresis and the recovery from a full disk, the tip
//! cross-check, and what its `/status` means ([`health`]).
//!
//! launcher-public's `ZingodRunner` gathers [`ZingodFacts`]; Z3's service actor adds
//! [`StartFacts`] and acts on the result. Every type here holds integers only, so the facts
//! keep `Eq`.

use std::{
    net::SocketAddr,
    time::{Duration, SystemTime},
};

use launcher_types::{
    ExitClass, ExitInfo, Network, Phase, Status,
    zingod::{IndexerState, IndexerStatus},
};

/// Each zingod version Zeal tested, with the zebrad versions and networks it passed with.
pub const TESTED_ZINGOD: [(&str, &[&str], &[Network]); 1] =
    [("1.1.0", &["7.0.0"], &[Network::Testnet])];

/// The network upgrades each tested zingod knows, by zebrad's names: the schedules its
/// `zcash_protocol` fork compiles in (`packages/zingod-primitives/src/protocol.rs` at 1.1.0).
pub const ZINGOD_KNOWS: [(&str, &[&str]); 1] = [(
    "1.1.0",
    &[
        "Overwinter",
        "Sapling",
        "Blossom",
        "Heartwood",
        "Canopy",
        "NU5",
        "NU6",
        "NU6.1",
        "NU6.2",
        "NU6.3",
        "NU7",
    ],
)];

/// A version without a leading `v` or build metadata: `7.0.0+git.e8db87e` is `7.0.0`.
fn release(version: &str) -> &str {
    let version = version.trim().trim_start_matches('v');
    version.split(['+', ' ']).next().unwrap_or(version)
}

/// The networks zingod `version` is offered on: those it was tested on with some zebrad.
pub fn offered_networks(version: &str) -> &'static [Network] {
    TESTED_ZINGOD
        .iter()
        .find(|(tested, _, _)| *tested == release(version))
        .map_or(&[], |(_, _, networks)| networks)
}

/// Whether zingod `zingod` with zebrad `zebrad` on `network` is a pair Zeal tested.
pub fn tested_pair(zingod: &str, zebrad: &str, network: Network) -> bool {
    TESTED_ZINGOD.iter().any(|(tested, zebrads, networks)| {
        *tested == release(zingod)
            && zebrads.contains(&release(zebrad))
            && networks.contains(&network)
    })
}

/// Whether zingod `version` knows the upgrade zebrad names `upgrade`. An untested version knows
/// none.
pub fn knows_upgrade(version: &str, upgrade: &str) -> bool {
    ZINGOD_KNOWS
        .iter()
        .find(|(known, _)| *known == release(version))
        .is_some_and(|(_, upgrades)| upgrades.contains(&upgrade))
}

/// One zebrad run, by the pid and start time the supervisor recorded in the same status update
/// that cleared the run's addresses: zingod restarts when it changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ZebradRun {
    pub pid: u32,
    pub started_at: SystemTime,
}

impl ZebradRun {
    /// The run `status` describes, if zebrad has one.
    pub fn of(status: &Status) -> Option<Self> {
        Some(ZebradRun {
            pid: status.pid?,
            started_at: status.started_at?,
        })
    }
}

/// zingod runs for `started_for`; zebrad's run is now `current`. A new run means zingod
/// restarts: zebrad writes a new cookie at every start, and zingod reads its cookie once, and
/// again only after a 401, which zebrad 7.0.0 never sends.
pub fn restarts_with(started_for: Option<ZebradRun>, current: Option<ZebradRun>) -> bool {
    matches!((started_for, current), (Some(started_for), Some(current)) if started_for != current)
}

/// zebrad's RPC cookie as Zeal sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cookie {
    Missing,
    Empty,
    Present,
}

/// What [`want`] reads about the zebrad zingod reads, all from one status of its supervisor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZebradFacts {
    pub run: ZebradRun,
    /// zebrad runs on the service's network.
    pub on_network: bool,
    /// The RPC address zebrad logged in this run. The supervisor clears it in the update that
    /// records a new run, and zebrad logs it just after it writes its cookie, so a status that
    /// has it says this run's cookie is on disk: the barrier zingod's start waits for.
    pub rpc_addr: Option<SocketAddr>,
    pub cookie: Cookie,
    /// zebrad has been at the tip once in this run.
    pub at_tip_once: bool,
}

/// What [`want`] reads besides the runner's facts: the service and the user. Z3's service
/// actor fills it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartFacts {
    /// The light client server is switched on.
    pub service_on: bool,
    /// Its consent covers zingod.
    pub consent: bool,
    /// zingod is the chosen backend.
    pub chosen: bool,
    /// The bundled zingod and the serving zebrad are a pair Zeal tested ([`tested_pair`]).
    pub tested: bool,
    /// `zeal-zingod` is bundled here.
    pub bundled: bool,
    /// A pause waits for the user.
    pub held: bool,
    pub zebrad: Option<ZebradFacts>,
    /// The tip cross-check found zingod on another chain.
    pub chain_mismatch: bool,
    /// zingod knows every upgrade zebrad lists as active.
    pub upgrades_known: bool,
}

/// zingod's process tree, in integers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ZingodResources {
    pub rss_bytes: u64,
    pub cpu_millicores: u32,
    pub read_bytes: u64,
    pub written_bytes: u64,
}

/// What the runner knows about zingod.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ZingodFacts {
    /// zingod's supervisor.
    pub phase: Phase,
    pub last_exit: Option<ExitInfo>,
    /// The zebrad run the running zingod was started for.
    pub running_for: Option<ZebradRun>,
    /// The last `/status` answer of this run.
    pub status: Option<IndexerStatus>,
    /// `/status` reads in a row without an answer.
    pub misses: u32,
    /// Since when `/status` has said no validator answers, without a break.
    pub answering_zero_since: Option<SystemTime>,
    /// The index's disk, read every minute.
    pub disk: Option<DiskReading>,
    /// [`disk_allows`] at the last reading.
    pub disk_allows: bool,
    /// What the last `DiskFull` exit showed.
    pub disk_full: Option<DiskFullEvidence>,
    /// The index's size, and whether the walk stopped early (so it is a lower bound).
    pub index_bytes: Option<(u64, bool)>,
    pub resources: Option<ZingodResources>,
    /// The port zingod listens on now.
    pub port: Option<u16>,
    /// The ports tried since the plan was given, after `PortInUse`.
    pub ports_tried: Vec<u16>,
}

/// Why zingod doesn't run now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Blocked {
    /// The service is off, or another backend is chosen.
    Off,
    /// The consent doesn't cover zingod.
    NoConsent,
    Untested,
    NotBundled,
    /// A pause waits for the user.
    Held,
    /// zebrad doesn't run.
    NoZebrad,
    /// zebrad runs another network than the service's.
    OtherNetwork,
    /// zebrad hasn't logged its RPC endpoint in this run.
    NoRpc,
    /// zebrad's cookie is missing or empty.
    NoCookie,
    /// zebrad hasn't been at the tip in this run.
    NotAtTip,
    /// A zingod failure waits for the user.
    Failed(ExitClass),
    ChainMismatch,
    /// The index's disk is below the low mark, or zingod waits for space after `DiskFull`.
    DiskLow,
    /// An active upgrade zingod doesn't know.
    UpgradeUnknown,
}

/// Whether zingod should run, and for which zebrad run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Want {
    Run(ZebradRun),
    Stop(Blocked),
}

/// When zingod runs: every start condition of docs/PUBLIC-SERVICES.md ("When zingod runs"),
/// the first that fails named.
pub fn want(start: &StartFacts, zingod: &ZingodFacts) -> Want {
    use Blocked::*;
    let stop = Want::Stop;
    if !start.service_on || !start.chosen {
        return stop(Off);
    }
    if !start.consent {
        return stop(NoConsent);
    }
    if !start.tested {
        return stop(Untested);
    }
    if !start.bundled {
        return stop(NotBundled);
    }
    if start.held {
        return stop(Held);
    }
    let Some(zebrad) = &start.zebrad else {
        return stop(NoZebrad);
    };
    if !zebrad.on_network {
        return stop(OtherNetwork);
    }
    if zebrad.rpc_addr.is_none() {
        return stop(NoRpc);
    }
    if zebrad.cookie != Cookie::Present {
        return stop(NoCookie);
    }
    if !zebrad.at_tip_once {
        return stop(NotAtTip);
    }
    if start.chain_mismatch {
        return stop(ChainMismatch);
    }
    match &zingod.phase {
        Phase::Failed {
            class: ExitClass::DiskFull,
        } => {
            let recovery = zingod
                .disk_full
                .as_ref()
                .map_or(DiskRecovery::NeedsUser, |evidence| {
                    disk_full_recovery(evidence, zingod.disk)
                });
            match recovery {
                DiskRecovery::NeedsUser => return stop(Failed(ExitClass::DiskFull)),
                DiskRecovery::WaitForSpace => return stop(DiskLow),
                DiskRecovery::Restart => {}
            }
        }
        Phase::Failed { class } => return stop(Failed(*class)),
        _ => {}
    }
    if !zingod.disk_allows {
        return stop(DiskLow);
    }
    if !start.upgrades_known {
        return stop(UpgradeUnknown);
    }
    Want::Run(zebrad.run)
}

/// Bytes in a GB.
const GB: u64 = 1_000_000_000;

/// Below this much free on the index's disk zingod stops, before a commit can fail.
pub const DISK_LOW: u64 = 10 * GB;

/// Above this much free zingod starts again.
pub const DISK_HIGH: u64 = 20 * GB;

/// The index's disk at one moment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiskReading {
    pub free_bytes: u64,
    /// Inodes free to this user (`statvfs` `f_favail`); `None` where the filesystem keeps no
    /// count, as btrfs does, or the platform can't tell.
    pub free_inodes: Option<u64>,
}

/// The disk's hysteresis: no start below [`DISK_LOW`], a start again above [`DISK_HIGH`],
/// and between them what it was. No reading keeps what it was.
pub fn disk_allows(was: bool, free_bytes: Option<u64>) -> bool {
    match free_bytes {
        Some(free) if free < DISK_LOW => false,
        Some(free) if free > DISK_HIGH => true,
        _ => was,
    }
}

/// What zingod's `DiskFull` text names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiskFullKind {
    /// `No space left on device`: no bytes, or no inodes, left.
    NoSpace,
    /// `Disk quota exceeded`: the user's quota, which free space doesn't show.
    Quota,
    /// zingod's own `index commit failed: disk … full`, which is either.
    Unstated,
}

/// What a `DiskFull` exit showed: its text, and the disk read just after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiskFullEvidence {
    pub kind: DiskFullKind,
    pub at_failure: Option<DiskReading>,
}

/// What follows a `DiskFull` exit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiskRecovery {
    /// Bytes ran out; zingod starts again once they are back above [`DISK_HIGH`].
    WaitForSpace,
    /// Bytes ran out and are back, with inodes free: start zingod again.
    Restart,
    /// Nothing Zeal measures covers the failure (a quota, inodes, or bytes that were there all
    /// along): the user's Try again. `DiskFull` is deterministic, so restarting on free bytes
    /// alone would loop outside the crash-loop protection.
    NeedsUser,
}

/// The recovery from a `DiskFull` exit: automatic only on evidence that covers the failure.
/// Zeal reads free bytes and inodes, not quotas, so bytes must have been short when it failed
/// (below [`DISK_LOW`]), and must be back above [`DISK_HIGH`] with inodes free.
pub fn disk_full_recovery(evidence: &DiskFullEvidence, now: Option<DiskReading>) -> DiskRecovery {
    let short_of_bytes = evidence
        .at_failure
        .is_some_and(|at| at.free_bytes < DISK_LOW);
    if evidence.kind == DiskFullKind::Quota || !short_of_bytes {
        return DiskRecovery::NeedsUser;
    }
    match now {
        Some(now) if now.free_bytes > DISK_HIGH && now.free_inodes.is_none_or(|n| n > 0) => {
            DiskRecovery::Restart
        }
        _ => DiskRecovery::WaitForSpace,
    }
}

/// How long `served` may stay still while zebrad is [`STALL_AHEAD`] or more blocks ahead.
pub const STALL: Duration = Duration::from_secs(5 * 60);

/// The same during a build, since the indexes publish in steps.
pub const STALL_BUILDING: Duration = Duration::from_secs(15 * 60);

/// zebrad this many blocks ahead of a still `served` makes zingod stalled.
pub const STALL_AHEAD: u64 = 4;

/// The stall rule's memory between readings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stall {
    served: Option<u64>,
    since: Option<SystemTime>,
    restarted: bool,
}

/// What the stall rule says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StallVerdict {
    Moving,
    /// Still too long: restart zingod, once.
    Restart,
    /// Still too long after the restart: `IndexerStuck` until it moves.
    Stuck {
        at: u64,
        zebrad_at: u64,
    },
}

impl Stall {
    /// One reading: zingod's served height, zebrad's height, whether zingod is building its
    /// index. A reading without a served height (zingod not answering, or booting) changes
    /// nothing; one that moved, or with zebrad near, starts over.
    pub fn observe(
        &mut self,
        served: Option<u64>,
        zebrad_height: Option<u64>,
        building: bool,
        now: SystemTime,
    ) -> StallVerdict {
        let Some(served) = served else {
            return StallVerdict::Moving;
        };
        let behind =
            zebrad_height.is_some_and(|zebrad| zebrad >= served.saturating_add(STALL_AHEAD));
        if self.served != Some(served) || !behind {
            let moved = self.served.is_some_and(|before| before != served);
            *self = Stall {
                served: Some(served),
                since: Some(now),
                restarted: self.restarted && !moved,
            };
            return StallVerdict::Moving;
        }
        let window = if building { STALL_BUILDING } else { STALL };
        let since = self.since.unwrap_or(now);
        if now.duration_since(since).unwrap_or_default() < window {
            return StallVerdict::Moving;
        }
        if !self.restarted {
            self.restarted = true;
            self.since = Some(now);
            return StallVerdict::Restart;
        }
        StallVerdict::Stuck {
            at: served,
            zebrad_at: zebrad_height.unwrap_or(served),
        }
    }
}

/// Different hashes in a row at the served height that make a chain mismatch.
pub const MISMATCHES: u32 = 3;

/// The least time they span, so a reorg clears first.
pub const MISMATCH_SPAN: Duration = Duration::from_secs(3 * 60);

/// One tip cross-check: zingod's `GetLatestBlock` against zebrad's `getblockhash` at that height.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TipCheck {
    Match,
    Mismatch,
    /// zebrad doesn't have the height yet.
    Skipped,
}

/// The cross-check's memory between checks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CrossCheck {
    first: Option<SystemTime>,
    count: u32,
}

impl CrossCheck {
    /// One check; true once [`MISMATCHES`] different hashes in a row span [`MISMATCH_SPAN`].
    /// A match starts over, and a skipped height changes nothing.
    pub fn observe(&mut self, check: TipCheck, now: SystemTime) -> bool {
        match check {
            TipCheck::Match => *self = CrossCheck::default(),
            TipCheck::Skipped => {}
            TipCheck::Mismatch => {
                self.first.get_or_insert(now);
                self.count = self.count.saturating_add(1);
            }
        }
        self.count >= MISMATCHES
            && self
                .first
                .is_some_and(|first| now.duration_since(first).unwrap_or_default() >= MISMATCH_SPAN)
    }
}

/// `/status` reads in a row without an answer that make zingod not answering.
pub const NO_ANSWER_MISSES: u32 = 3;

/// How long no validator answering makes zingod unable to reach zebrad.
pub const NO_VALIDATOR_FOR: Duration = Duration::from_secs(30);

/// A sync with fewer blocks than this left is catching up, with no ETA.
pub const CATCHING_UP_BLOCKS: u64 = 1_000;

/// What zingod's `/status` means for the service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IndexerHealth {
    /// Starting: opening and checking its index, or no answer yet.
    Boot,
    /// Building its index; the numbers once it has a target, the ETA once it has a rate.
    Building {
        served: Option<u64>,
        target: Option<u64>,
        eta_secs: Option<u64>,
    },
    /// Within [`CATCHING_UP_BLOCKS`] of the tip, or at it while zebrad's mempool is off.
    CatchingUp { served: u64 },
    /// `serving` or `degraded`: ready, under the service's other rules.
    Ready,
    /// `down` or `held`, with zingod's reason and message.
    Down { reason: String, message: String },
    /// No validator has answered for [`NO_VALIDATOR_FOR`].
    NoValidator,
    /// [`NO_ANSWER_MISSES`] reads without an answer.
    NoAnswer,
}

/// The start of an unbroken stretch where `/status` says no validator answers.
pub fn answering_zero_since(
    since: Option<SystemTime>,
    status: Option<&IndexerStatus>,
    now: SystemTime,
) -> Option<SystemTime> {
    super::light::track_since(since, status.is_some_and(|s| s.answering == Some(0)), now)
}

/// The health of zingod from its last `/status` (`None` before an answer), the reads missed
/// since, the stretch without a validator, and zebrad's height, which bounds what is left to
/// sync: zingod's target can be its clock's estimate.
pub fn health(
    status: Option<&IndexerStatus>,
    misses: u32,
    answering_zero_since: Option<SystemTime>,
    zebrad_height: Option<u64>,
    now: SystemTime,
) -> IndexerHealth {
    if misses >= NO_ANSWER_MISSES {
        return IndexerHealth::NoAnswer;
    }
    let Some(status) = status else {
        return IndexerHealth::Boot;
    };
    if answering_zero_since
        .is_some_and(|since| now.duration_since(since).unwrap_or_default() >= NO_VALIDATOR_FOR)
    {
        return IndexerHealth::NoValidator;
    }
    match &status.state {
        IndexerState::Provisioning => IndexerHealth::Boot,
        IndexerState::Syncing { progress: None } => IndexerHealth::Building {
            served: None,
            target: None,
            eta_secs: None,
        },
        IndexerState::Syncing {
            progress: Some(progress),
        } => {
            let goal = match (progress.target, zebrad_height) {
                (Some(target), Some(zebrad)) => Some(target.min(zebrad)),
                (target, zebrad) => target.or(zebrad),
            };
            let left = goal.map(|goal| goal.saturating_sub(progress.height));
            if left.is_some_and(|left| left < CATCHING_UP_BLOCKS) {
                return IndexerHealth::CatchingUp {
                    served: progress.height,
                };
            }
            let eta_secs = match (left, progress.milli_blocks_per_sec) {
                (Some(left), Some(rate)) if rate > 0 => Some(left.saturating_mul(1_000) / rate),
                _ => None,
            };
            IndexerHealth::Building {
                served: Some(progress.height),
                target: progress.target,
                eta_secs,
            }
        }
        IndexerState::Serving | IndexerState::Degraded { .. } => IndexerHealth::Ready,
        IndexerState::Down { reason, message } | IndexerState::Held { reason, message } => {
            IndexerHealth::Down {
                reason: reason.clone(),
                message: message.clone(),
            }
        }
        IndexerState::Other { name } => IndexerHealth::Down {
            reason: name.clone(),
            message: String::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use launcher_types::{ZebradChannel, zingod::SyncProgress};

    use super::*;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    const RUN: ZebradRun = ZebradRun {
        pid: 41,
        started_at: SystemTime::UNIX_EPOCH,
    };

    fn ready() -> (StartFacts, ZingodFacts) {
        (
            StartFacts {
                service_on: true,
                consent: true,
                chosen: true,
                tested: true,
                bundled: true,
                held: false,
                zebrad: Some(ZebradFacts {
                    run: RUN,
                    on_network: true,
                    rpc_addr: Some(SocketAddr::from(([127, 0, 0, 1], 18232))),
                    cookie: Cookie::Present,
                    at_tip_once: true,
                }),
                chain_mismatch: false,
                upgrades_known: true,
            },
            ZingodFacts {
                disk_allows: true,
                ..ZingodFacts::default()
            },
        )
    }

    #[test]
    fn zingod_runs_once_every_condition_holds() {
        let (start, zingod) = ready();
        assert_eq!(want(&start, &zingod), Want::Run(RUN));
    }

    /// Each condition turned off alone names itself.
    #[test]
    fn each_condition_alone_stops_zingod() {
        type Change = fn(&mut StartFacts, &mut ZingodFacts);
        let cases: [(&str, Change, Blocked); 17] = [
            ("off", |s, _| s.service_on = false, Blocked::Off),
            ("other backend", |s, _| s.chosen = false, Blocked::Off),
            ("consent", |s, _| s.consent = false, Blocked::NoConsent),
            ("untested", |s, _| s.tested = false, Blocked::Untested),
            ("not bundled", |s, _| s.bundled = false, Blocked::NotBundled),
            ("held", |s, _| s.held = true, Blocked::Held),
            ("no zebrad", |s, _| s.zebrad = None, Blocked::NoZebrad),
            (
                "other network",
                |s, _| s.zebrad.as_mut().unwrap().on_network = false,
                Blocked::OtherNetwork,
            ),
            (
                "no rpc",
                |s, _| s.zebrad.as_mut().unwrap().rpc_addr = None,
                Blocked::NoRpc,
            ),
            (
                "no cookie",
                |s, _| s.zebrad.as_mut().unwrap().cookie = Cookie::Missing,
                Blocked::NoCookie,
            ),
            (
                "empty cookie",
                |s, _| s.zebrad.as_mut().unwrap().cookie = Cookie::Empty,
                Blocked::NoCookie,
            ),
            (
                "not at tip",
                |s, _| s.zebrad.as_mut().unwrap().at_tip_once = false,
                Blocked::NotAtTip,
            ),
            (
                "mismatch",
                |s, _| s.chain_mismatch = true,
                Blocked::ChainMismatch,
            ),
            (
                "refused config, an unreadable cookie included",
                |_, z| {
                    z.phase = Phase::Failed {
                        class: ExitClass::ConfigError,
                    }
                },
                Blocked::Failed(ExitClass::ConfigError),
            ),
            (
                "crash loop",
                |_, z| {
                    z.phase = Phase::Failed {
                        class: ExitClass::Crash,
                    }
                },
                Blocked::Failed(ExitClass::Crash),
            ),
            ("disk", |_, z| z.disk_allows = false, Blocked::DiskLow),
            (
                "upgrade",
                |s, _| s.upgrades_known = false,
                Blocked::UpgradeUnknown,
            ),
        ];
        for (name, change, blocked) in cases {
            let (mut start, mut zingod) = ready();
            change(&mut start, &mut zingod);
            assert_eq!(want(&start, &zingod), Want::Stop(blocked), "{name}");
        }
        for class in [
            ExitClass::DbLocked,
            ExitClass::PortInUse,
            ExitClass::IndexBroken,
            ExitClass::SpawnFailed,
        ] {
            let (start, mut zingod) = ready();
            zingod.phase = Phase::Failed { class };
            assert_eq!(
                want(&start, &zingod),
                Want::Stop(Blocked::Failed(class)),
                "{class:?}"
            );
        }
    }

    /// The barrier is this run's RPC line, never the cookie's time: a cookie written before the
    /// supervisor recorded the run's start still lets zingod start, once the run logged its
    /// endpoint.
    #[test]
    fn the_rpc_line_is_the_barrier_not_the_cookies_time() {
        let (mut start, zingod) = ready();
        let late = ZebradRun {
            pid: 41,
            started_at: at(1_000),
        };
        let zebrad = start.zebrad.as_mut().unwrap();
        zebrad.run = late;
        zebrad.rpc_addr = None;
        assert_eq!(want(&start, &zingod), Want::Stop(Blocked::NoRpc));
        start.zebrad.as_mut().unwrap().rpc_addr = Some(SocketAddr::from(([127, 0, 0, 1], 1)));
        assert_eq!(want(&start, &zingod), Want::Run(late));
    }

    #[test]
    fn a_new_zebrad_run_restarts_zingod() {
        let next = ZebradRun {
            pid: 42,
            started_at: at(5),
        };
        assert!(restarts_with(Some(RUN), Some(next)));
        assert!(!restarts_with(Some(RUN), Some(RUN)));
        assert!(!restarts_with(None, Some(next)), "nothing to restart");
        assert!(
            !restarts_with(Some(RUN), None),
            "zebrad stopped: a stop, not a restart"
        );
        let same_pid_later = ZebradRun {
            pid: 41,
            started_at: at(9),
        };
        assert!(restarts_with(Some(RUN), Some(same_pid_later)));
        let status = Status {
            pid: Some(42),
            started_at: Some(at(5)),
            ..Status::default()
        };
        assert_eq!(ZebradRun::of(&status), Some(next));
        assert_eq!(ZebradRun::of(&Status::default()), None);
    }

    #[test]
    fn the_disk_has_hysteresis() {
        for (was, free, allows) in [
            (true, Some(9 * GB), false),
            (false, Some(15 * GB), false),
            (true, Some(15 * GB), true),
            (false, Some(21 * GB), true),
            (false, Some(DISK_HIGH), false),
            (true, Some(DISK_LOW), true),
            (false, None, false),
            (true, None, true),
        ] {
            assert_eq!(disk_allows(was, free), allows, "{was} {free:?}");
        }
    }

    #[test]
    fn a_full_disk_recovers_only_on_evidence_that_covers_it() {
        let reading = |free_bytes, free_inodes| {
            Some(DiskReading {
                free_bytes,
                free_inodes,
            })
        };
        let evidence = |kind, at_failure| DiskFullEvidence { kind, at_failure };
        let short = reading(2 * GB, Some(1_000));
        let roomy = reading(500 * GB, Some(1_000));
        let cases = [
            (
                "bytes ran out and came back",
                evidence(DiskFullKind::NoSpace, short),
                reading(25 * GB, Some(10)),
                DiskRecovery::Restart,
            ),
            (
                "zingod's own text, bytes short, back",
                evidence(DiskFullKind::Unstated, short),
                reading(25 * GB, None),
                DiskRecovery::Restart,
            ),
            (
                "not back above the high mark yet",
                evidence(DiskFullKind::NoSpace, short),
                reading(15 * GB, Some(10)),
                DiskRecovery::WaitForSpace,
            ),
            (
                "bytes back but no inodes",
                evidence(DiskFullKind::NoSpace, short),
                reading(25 * GB, Some(0)),
                DiskRecovery::WaitForSpace,
            ),
            (
                "no reading now",
                evidence(DiskFullKind::NoSpace, short),
                None,
                DiskRecovery::WaitForSpace,
            ),
            (
                "out of inodes with bytes free",
                evidence(DiskFullKind::NoSpace, roomy),
                reading(500 * GB, Some(1_000)),
                DiskRecovery::NeedsUser,
            ),
            (
                "a quota",
                evidence(DiskFullKind::Quota, short),
                reading(500 * GB, Some(1_000)),
                DiskRecovery::NeedsUser,
            ),
            (
                "zingod's own text with bytes free: a quota or inodes",
                evidence(DiskFullKind::Unstated, roomy),
                reading(500 * GB, Some(1_000)),
                DiskRecovery::NeedsUser,
            ),
            (
                "no reading at the failure",
                evidence(DiskFullKind::NoSpace, None),
                reading(500 * GB, Some(1_000)),
                DiskRecovery::NeedsUser,
            ),
        ];
        for (name, evidence, now, recovery) in cases {
            assert_eq!(disk_full_recovery(&evidence, now), recovery, "{name}");
        }

        let (start, mut zingod) = ready();
        zingod.phase = Phase::Failed {
            class: ExitClass::DiskFull,
        };
        zingod.disk_full = Some(evidence(DiskFullKind::NoSpace, short));
        zingod.disk = reading(5 * GB, Some(10));
        assert_eq!(want(&start, &zingod), Want::Stop(Blocked::DiskLow));
        zingod.disk = reading(25 * GB, Some(10));
        assert_eq!(want(&start, &zingod), Want::Run(RUN));
        zingod.disk_full = Some(evidence(DiskFullKind::Quota, short));
        assert_eq!(
            want(&start, &zingod),
            Want::Stop(Blocked::Failed(ExitClass::DiskFull))
        );
        zingod.disk_full = None;
        assert_eq!(
            want(&start, &zingod),
            Want::Stop(Blocked::Failed(ExitClass::DiskFull))
        );
    }

    #[test]
    fn a_stall_restarts_zingod_once_then_holds() {
        let minute = 60;
        let mut stall = Stall::default();
        assert_eq!(
            stall.observe(Some(100), Some(110), false, at(0)),
            StallVerdict::Moving
        );
        assert_eq!(
            stall.observe(Some(100), Some(110), false, at(4 * minute)),
            StallVerdict::Moving
        );
        assert_eq!(
            stall.observe(Some(100), Some(110), false, at(5 * minute)),
            StallVerdict::Restart
        );
        assert_eq!(
            stall.observe(None, Some(110), false, at(6 * minute)),
            StallVerdict::Moving,
            "booting after the restart changes nothing"
        );
        assert_eq!(
            stall.observe(Some(100), Some(111), false, at(10 * minute)),
            StallVerdict::Stuck {
                at: 100,
                zebrad_at: 111
            }
        );
        assert_eq!(
            stall.observe(Some(101), Some(111), false, at(11 * minute)),
            StallVerdict::Moving,
            "it moved"
        );
        assert_eq!(
            stall.observe(Some(101), Some(111), false, at(16 * minute)),
            StallVerdict::Restart,
            "a later stall restarts it again"
        );
    }

    #[test]
    fn a_build_gets_the_longer_window_and_zebrad_near_is_no_stall() {
        let minute = 60;
        let mut stall = Stall::default();
        stall.observe(Some(100), Some(90_000), true, at(0));
        assert_eq!(
            stall.observe(Some(100), Some(90_000), true, at(14 * minute)),
            StallVerdict::Moving
        );
        assert_eq!(
            stall.observe(Some(100), Some(90_000), true, at(15 * minute)),
            StallVerdict::Restart
        );
        let mut near = Stall::default();
        for minutes in [0, 10, 20, 30] {
            assert_eq!(
                near.observe(Some(100), Some(103), false, at(minutes * minute)),
                StallVerdict::Moving,
                "zebrad 3 ahead"
            );
        }
        assert_eq!(
            near.observe(Some(100), None, false, at(40 * minute)),
            StallVerdict::Moving,
            "zebrad's height unknown"
        );
    }

    #[test]
    fn three_mismatches_over_three_minutes_are_a_mismatch() {
        use TipCheck::*;
        let mut check = CrossCheck::default();
        assert!(!check.observe(Mismatch, at(0)));
        assert!(!check.observe(Mismatch, at(60)));
        assert!(
            !check.observe(Mismatch, at(120)),
            "three, but over 2 minutes"
        );
        assert!(check.observe(Mismatch, at(180)));

        let mut reorg = CrossCheck::default();
        assert!(!reorg.observe(Mismatch, at(0)));
        assert!(!reorg.observe(Mismatch, at(60)));
        assert!(!reorg.observe(Match, at(120)), "a reorg clears");
        assert!(!reorg.observe(Mismatch, at(180)));
        assert!(!reorg.observe(Mismatch, at(240)));

        let mut skipped = CrossCheck::default();
        assert!(!skipped.observe(Mismatch, at(0)));
        assert!(!skipped.observe(Skipped, at(60)), "a height zebrad lacks");
        assert!(!skipped.observe(Mismatch, at(120)));
        assert!(!skipped.observe(Skipped, at(150)));
        assert!(skipped.observe(Mismatch, at(200)));
    }

    fn status(state: IndexerState, answering: Option<u32>) -> IndexerStatus {
        IndexerStatus {
            state,
            served: Some(100),
            tip: Some(100),
            network: "testnet".into(),
            trusted: answering.map(|_| 1),
            answering,
            alarms: Vec::new(),
            version: "1.1.0".into(),
        }
    }

    fn syncing(height: u64, target: Option<u64>, milli: Option<u64>) -> IndexerState {
        IndexerState::Syncing {
            progress: Some(SyncProgress {
                height,
                target,
                milli_blocks_per_sec: milli,
            }),
        }
    }

    #[test]
    fn every_status_state_maps() {
        let down = |reason: &str| IndexerState::Down {
            reason: reason.into(),
            message: "words".into(),
        };
        let cases = [
            (IndexerState::Provisioning, None, IndexerHealth::Boot),
            (
                IndexerState::Syncing { progress: None },
                None,
                IndexerHealth::Building {
                    served: None,
                    target: None,
                    eta_secs: None,
                },
            ),
            (
                syncing(1_000, Some(101_000), Some(50_000)),
                Some(101_000),
                IndexerHealth::Building {
                    served: Some(1_000),
                    target: Some(101_000),
                    eta_secs: Some(2_000),
                },
            ),
            (
                syncing(1_000, Some(101_000), None),
                Some(101_000),
                IndexerHealth::Building {
                    served: Some(1_000),
                    target: Some(101_000),
                    eta_secs: None,
                },
            ),
            (
                syncing(100_500, Some(101_000), Some(50_000)),
                Some(101_000),
                IndexerHealth::CatchingUp { served: 100_500 },
            ),
            (
                // The captured answer at the tip while zebrad's mempool is off: the target is
                // zingod's clock estimate, zebrad's height bounds it.
                syncing(3, Some(6_642_568), None),
                Some(3),
                IndexerHealth::CatchingUp { served: 3 },
            ),
            (IndexerState::Serving, None, IndexerHealth::Ready),
            (
                IndexerState::Degraded {
                    reason: "tip_unvouched".into(),
                    message: "x".into(),
                },
                None,
                IndexerHealth::Ready,
            ),
            (
                down("tip_unvouched"),
                None,
                IndexerHealth::Down {
                    reason: "tip_unvouched".into(),
                    message: "words".into(),
                },
            ),
            (
                IndexerState::Held {
                    reason: "goaway_sent".into(),
                    message: "GOAWAY sent".into(),
                },
                None,
                IndexerHealth::Down {
                    reason: "goaway_sent".into(),
                    message: "GOAWAY sent".into(),
                },
            ),
            (
                IndexerState::Other {
                    name: "resting".into(),
                },
                None,
                IndexerHealth::Down {
                    reason: "resting".into(),
                    message: String::new(),
                },
            ),
        ];
        for (state, zebrad, expected) in cases {
            let status = status(state.clone(), Some(1));
            assert_eq!(
                health(Some(&status), 0, None, zebrad, at(100)),
                expected,
                "{state:?}"
            );
        }
    }

    #[test]
    fn no_answer_and_no_validator_come_from_time() {
        let serving = status(IndexerState::Serving, Some(0));
        assert_eq!(health(None, 0, None, None, at(0)), IndexerHealth::Boot);
        assert_eq!(health(None, 2, None, None, at(0)), IndexerHealth::Boot);
        assert_eq!(
            health(Some(&serving), 3, None, None, at(0)),
            IndexerHealth::NoAnswer
        );
        let since = answering_zero_since(None, Some(&serving), at(100));
        assert_eq!(since, Some(at(100)));
        assert_eq!(
            answering_zero_since(since, Some(&serving), at(129)),
            Some(at(100))
        );
        assert_eq!(
            health(Some(&serving), 0, since, None, at(129)),
            IndexerHealth::Ready
        );
        assert_eq!(
            health(Some(&serving), 0, since, None, at(130)),
            IndexerHealth::NoValidator
        );
        let answering = status(IndexerState::Serving, Some(1));
        assert_eq!(answering_zero_since(since, Some(&answering), at(131)), None);
        let unbooted = status(IndexerState::Syncing { progress: None }, None);
        assert_eq!(answering_zero_since(None, Some(&unbooted), at(131)), None);
        assert_eq!(
            health(Some(&unbooted), 0, None, None, at(200)),
            IndexerHealth::Building {
                served: None,
                target: None,
                eta_secs: None,
            },
            "a null detail is a build with no numbers, never NoAnswer"
        );
    }

    #[test]
    fn the_bundled_zingod_is_tested_with_the_stable_zebrad() {
        let zingod = launcher_types::zingod::version();
        assert!(
            TESTED_ZINGOD.iter().any(|(tested, _, _)| *tested == zingod),
            "ZINGOD_VERSION {zingod} is not on TESTED_ZINGOD"
        );
        let zebrad = ZebradChannel::Stable.version();
        let offered = offered_networks(zingod);
        assert!(!offered.is_empty());
        for network in offered {
            assert!(
                tested_pair(zingod, zebrad, *network),
                "zingod {zingod} with zebrad {zebrad} on {network:?} is untested"
            );
        }
        assert!(
            ZINGOD_KNOWS.iter().any(|(known, _)| *known == zingod),
            "ZINGOD_KNOWS has no entry for {zingod}"
        );
    }

    #[test]
    fn pairs_and_upgrades() {
        assert!(tested_pair("1.1.0", "7.0.0+git.e8db87e", Network::Testnet));
        assert!(tested_pair("v1.1.0", "7.0.0", Network::Testnet));
        assert!(
            !tested_pair("1.1.0", "7.0.0", Network::Mainnet),
            "Mainnet waits for Z4"
        );
        assert!(!tested_pair("1.1.0", "7.1.0-alpha.0", Network::Testnet));
        assert!(!tested_pair("1.2.0", "7.0.0", Network::Testnet));
        assert_eq!(offered_networks("1.2.0"), &[] as &[Network]);
        assert!(knows_upgrade("1.1.0", "NU7"));
        assert!(knows_upgrade("1.1.0", "Overwinter"));
        assert!(!knows_upgrade("1.1.0", "NU8"));
        assert!(!knows_upgrade("1.2.0", "NU7"));
    }
}
