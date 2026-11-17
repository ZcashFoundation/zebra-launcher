//! zingod as the light client server's backend (docs/PUBLIC-SERVICES.md, "zingod as the
//! backend"): which pairs Zeal tested, when zingod runs ([`want`]), the restart with each zebrad
//! run, the stall rule, the disk's hysteresis and the recovery from a full disk, the tip
//! cross-check, and what its `/status` means ([`health`]).
//!
//! launcher-public's `ZingodRunner` gathers [`ZingodFacts`]; the service actor adds what it
//! knows ([`IndexerFacts`], in [`Facts::zingod`]), and the light client server's rules read
//! them here: the choice at the consent screen ([`choice`]), the Service step's checks
//! ([`service_checks`]), and what keeps zingod from serving ([`problem`]), which the status
//! panel words. Every type here holds integers only, so the facts keep `Eq`.

use std::{
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use launcher_types::{
    ExitClass, ExitInfo, Network, NetworkUpgrade, Phase, StaleProcessInfo, Status, UpgradeStatus,
    public::{
        CheckId, CheckOutcome, CheckResult, DegradedReason, DownReason, FailDetail, Finding,
        IndexerResources, IndexerStats, IndexerWait, LightBackend, PublicServiceSpec, ZingodChoice,
    },
    zingod::{IndexerState, IndexerStatus},
};

use super::{Checked, Facts, light};

/// The backend a new light client server gets: zebrad's own endpoint, which serves as soon as
/// zebrad is at the tip, needs no extra disk and has PS1's tests behind it. Which one new
/// setups get once Mainnet opens to zingod is the maintainer's call.
pub const DEFAULT_BACKEND: LightBackend = LightBackend::ZebradEndpoint;

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
    /// The index's disk has room for the build the setup is about to start.
    pub space: bool,
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
    /// The zebrad run zingod's last start was for.
    pub running_for: Option<ZebradRun>,
    /// The plan's attempt zingod's last start was for.
    pub started_attempt: Option<u64>,
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
    /// The ports tried under the plan's attempt, after `PortInUse`.
    pub ports_tried: Vec<u16>,
    /// A zingod an earlier Zeal left that Zeal couldn't end: zingod doesn't start again until
    /// zebrad's run or the attempt changes.
    pub stale: Option<StaleProcessInfo>,
    /// The address zingod's `Listening` line named in this run.
    pub listening: Option<SocketAddr>,
}

impl ZingodFacts {
    /// Whether zingod still runs for an earlier attempt than `attempt`, the actor's: a restart
    /// the actor asked for is under way, and zingod serves no more. From the restart's stop on
    /// the phase says so itself, and the runner's next start is for `attempt`, however briefly
    /// the phase between them showed.
    pub fn restarting(&self, attempt: u64) -> bool {
        self.phase.is_active() && self.started_attempt != Some(attempt)
    }
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
    /// The index's disk has too little room for the setup's first build.
    NoSpace,
}

/// Whether zingod should run, and for which zebrad run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Want {
    Run(ZebradRun),
    Stop(Blocked),
}

/// The start conditions that don't depend on zingod itself: when they hold the service actor
/// gives the runner a plan for this zebrad run, and the runner handles zingod's failures and
/// the disk's hysteresis.
pub fn plan_run(start: &StartFacts) -> Result<ZebradRun, Blocked> {
    use Blocked::*;
    if !start.service_on || !start.chosen {
        return Err(Off);
    }
    if !start.consent {
        return Err(NoConsent);
    }
    if !start.tested {
        return Err(Untested);
    }
    if !start.bundled {
        return Err(NotBundled);
    }
    if start.held {
        return Err(Held);
    }
    let Some(zebrad) = &start.zebrad else {
        return Err(NoZebrad);
    };
    if !zebrad.on_network {
        return Err(OtherNetwork);
    }
    if zebrad.rpc_addr.is_none() {
        return Err(NoRpc);
    }
    if zebrad.cookie != Cookie::Present {
        return Err(NoCookie);
    }
    if !zebrad.at_tip_once {
        return Err(NotAtTip);
    }
    if start.chain_mismatch {
        return Err(ChainMismatch);
    }
    if !start.upgrades_known {
        return Err(UpgradeUnknown);
    }
    if !start.space {
        return Err(NoSpace);
    }
    Ok(zebrad.run)
}

/// When zingod runs: every start condition of docs/PUBLIC-SERVICES.md ("When zingod runs"),
/// the first that fails named.
pub fn want(start: &StartFacts, zingod: &ZingodFacts) -> Want {
    use Blocked::*;
    let stop = Want::Stop;
    let run = match plan_run(start) {
        Ok(run) => run,
        Err(blocked) => return stop(blocked),
    };
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
    Want::Run(run)
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
    /// [`MISMATCH_SPAN`] unless a test shortened it.
    span: Option<Duration>,
}

impl CrossCheck {
    /// A cross-check whose mismatches must span `span` instead of [`MISMATCH_SPAN`], for
    /// tests in real time.
    pub fn with_span(span: Duration) -> Self {
        CrossCheck {
            span: Some(span),
            ..CrossCheck::default()
        }
    }

    /// One check; true once [`MISMATCHES`] different hashes in a row span [`MISMATCH_SPAN`].
    /// A match starts over, and a skipped height changes nothing.
    pub fn observe(&mut self, check: TipCheck, now: SystemTime) -> bool {
        let span = self.span.unwrap_or(MISMATCH_SPAN);
        match check {
            TipCheck::Match => {
                *self = CrossCheck {
                    span: self.span,
                    ..CrossCheck::default()
                }
            }
            TipCheck::Skipped => {}
            TipCheck::Mismatch => {
                self.first.get_or_insert(now);
                self.count = self.count.saturating_add(1);
            }
        }
        self.count >= MISMATCHES
            && self
                .first
                .is_some_and(|first| now.duration_since(first).unwrap_or_default() >= span)
    }
}

/// The cross-check to keep after `check` at `now`: a skipped height (zingod a block ahead of
/// zebrad's last reading, common just after a block) keeps the last match or mismatch with the
/// new time, so it doesn't undo a pass until the next check.
pub fn keep_tip(
    last: Option<&Checked<TipCheck>>,
    check: TipCheck,
    now: SystemTime,
) -> Checked<TipCheck> {
    let result = match (check, last) {
        (TipCheck::Skipped, Some(last)) => last.result,
        _ => check,
    };
    Checked { at: now, result }
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
    /// Building its index; the numbers once it has a target, the ETA once it has a rate. The
    /// target is zingod's, bounded by zebrad's height.
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
                target: goal,
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

/// zingod as the service actor sees it: what [`Facts::zingod`] holds while zingod is bundled
/// here or was chosen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexerFacts {
    /// `zeal-zingod` is bundled here.
    pub bundled: bool,
    /// The bundled zingod's version.
    pub version: String,
    /// The index folder: the spec's, or where a first choice would put it.
    pub dir: PathBuf,
    /// That folder's disk, read by the actor or by the runner.
    pub disk: Option<DiskReading>,
    /// That folder is on a network filesystem.
    pub network_fs: bool,
    /// This computer's memory.
    pub total_memory: Option<u64>,
    /// The serving zebrad, as zingod's start reads it.
    pub zebrad: Option<ZebradFacts>,
    /// What the runner knows.
    pub runner: ZingodFacts,
    /// Since when zingod has served more than [`light::AT_TIP_BLOCKS`] behind zebrad without
    /// a break.
    pub behind_since: Option<SystemTime>,
    /// The stall rule after its restart: zingod's height and zebrad's.
    pub stuck: Option<(u64, u64)>,
    /// The tip cross-check found zingod on another chain at this height. It holds until a
    /// rebuild.
    pub mismatch: Option<u64>,
    /// The last tip cross-check.
    pub tip: Option<Checked<TipCheck>>,
    /// zingod still runs for an earlier attempt than the actor's ([`ZingodFacts::restarting`]):
    /// it serves no more.
    pub restarting: bool,
}

/// Bytes in a GB, for the setup's needs.
const GB_NEED: u64 = 1_000_000_000;

/// The free space the Service step wants on the index's disk before a first build, counting
/// what is already built (proposals, set from the Testnet and Mainnet measurements).
pub fn space_needed(network: Network) -> u64 {
    match network {
        Network::Mainnet => 100 * GB_NEED,
        Network::Testnet => 50 * GB_NEED,
    }
}

/// Below this much memory the Service step warns (proposal).
pub const MEMORY_WANTED: u64 = 8 * GB_NEED;

/// zingod's index folder for the node whose data folder is `node_data`, on `network`.
pub fn index_dir(node_data: &Path, network: Network) -> PathBuf {
    node_data
        .join("zingod")
        .join(network.as_str().to_ascii_lowercase())
}

/// The first upgrade zebrad lists as active that zingod `version` doesn't know.
pub fn unknown_upgrade<'a>(
    version: &str,
    upgrades: &'a [NetworkUpgrade],
) -> Option<&'a NetworkUpgrade> {
    upgrades
        .iter()
        .filter(|upgrade| upgrade.status == UpgradeStatus::Active)
        .find(|upgrade| !knows_upgrade(version, &upgrade.name))
}

/// Whether zingod can be chosen for a service on `network`: bundled here, offered on the
/// network, and tested with the serving zebrad.
pub fn choice(facts: &Facts, network: Network) -> ZingodChoice {
    let Some(zingod) = facts.zingod.as_ref().filter(|zingod| zingod.bundled) else {
        return ZingodChoice::NotBundled;
    };
    if !offered_networks(&zingod.version).contains(&network) {
        return ZingodChoice::NotOnNetwork { network };
    }
    let zebrad = facts.zebrad_version.clone().unwrap_or_default();
    if !tested_pair(&zingod.version, &zebrad, network) {
        return ZingodChoice::Untested {
            version: zingod.version.clone(),
            zebrad: release(&zebrad).to_owned(),
            network,
        };
    }
    ZingodChoice::Offered
}

/// Whether the index's disk has room for the setup's build: [`space_needed`], counting what is
/// already built. Only a service still being set up is held to it; an open one keeps the
/// runner's hysteresis alone. An unread disk passes, so the read decides.
fn room(spec: &PublicServiceSpec, zingod: &IndexerFacts) -> bool {
    spec.opened
        || zingod
            .disk
            .is_none_or(|disk| has_room(spec.network, zingod, disk))
}

fn has_room(network: Network, zingod: &IndexerFacts, disk: DiskReading) -> bool {
    let built = zingod.runner.index_bytes.map_or(0, |(bytes, _)| bytes);
    disk.free_bytes.saturating_add(built) >= space_needed(network)
}

/// [`want`]'s facts from the spec and the facts.
pub fn start_facts(spec: &PublicServiceSpec, facts: &Facts) -> StartFacts {
    let zingod = facts.zingod.as_ref();
    let version = zingod.map_or("", |zingod| zingod.version.as_str());
    let zebrad = facts.zebrad_version.as_deref().unwrap_or_default();
    StartFacts {
        service_on: spec.switch_on,
        consent: spec.consent_current(),
        chosen: spec.backend == LightBackend::Zingod,
        tested: tested_pair(version, zebrad, spec.network),
        bundled: zingod.is_some_and(|zingod| zingod.bundled),
        held: facts.held.is_some() || spec.network != facts.network || !facts.supported,
        zebrad: zingod.and_then(|zingod| zingod.zebrad.clone()),
        chain_mismatch: zingod.is_some_and(|zingod| zingod.mismatch.is_some()),
        upgrades_known: unknown_upgrade(version, &facts.upgrades).is_none(),
        space: zingod.is_none_or(|zingod| room(spec, zingod)),
    }
}

/// What a failed zingod's exit names: the ports tried, the folder, the open-file limit or the
/// cookie, from the exit's text (`launcher_core::zingod::logs`'s classes).
pub fn fail_detail(
    class: ExitClass,
    text: Option<&str>,
    ports_tried: &[u16],
    dir: &Path,
) -> FailDetail {
    let text = text.unwrap_or_default().trim();
    match class {
        ExitClass::PortInUse => FailDetail::Ports {
            tried: ports_tried.to_vec(),
        },
        ExitClass::DbLocked | ExitClass::DiskFull => FailDetail::Dir {
            path: dir.to_path_buf(),
        },
        ExitClass::ConfigError => {
            if let Some(rest) = text.strip_prefix("cannot read validator cookie ")
                && let Some((path, error)) = rest.split_once(": ")
            {
                return FailDetail::Cookie {
                    path: PathBuf::from(path),
                    error: error.to_owned(),
                };
            }
            // `grpc.max_connections = 256 needs 1280 open files (…) and the limit is 1024: …`
            let number_after = |word: &str| {
                text.split_once(word).and_then(|(_, rest)| {
                    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
                    digits.parse::<u64>().ok()
                })
            };
            match (number_after(" needs "), number_after("the limit is ")) {
                (Some(needed), Some(limit)) if text.contains("open files") => {
                    FailDetail::OpenFiles { needed, limit }
                }
                _ => FailDetail::None,
            }
        }
        _ => FailDetail::None,
    }
}

/// zingod's served height and zebrad's, and how far behind zingod is.
fn served_behind(status: Option<&IndexerStatus>, zebrad: Option<u64>) -> Option<(u64, u64)> {
    let served = status?.served?;
    Some((served, zebrad?.saturating_sub(served)))
}

/// What keeps zingod from serving, as the status panel's reason; `None` once it serves within
/// [`light::AT_TIP_BLOCKS`] of zebrad, unstuck and on zebrad's chain. zebrad's own problems
/// (stopped, restarting, behind) are [`super::degraded`]'s, before this.
pub fn problem(spec: &PublicServiceSpec, facts: &Facts) -> Option<DegradedReason> {
    let starting = |waiting| Some(DegradedReason::IndexerStarting { waiting });
    let Some(zingod) = facts.zingod.as_ref() else {
        return Some(DegradedReason::IndexerFailed {
            class: ExitClass::SpawnFailed,
            detail: FailDetail::None,
        });
    };
    let runner = &zingod.runner;
    let start = start_facts(spec, facts);
    match want(&start, runner) {
        Want::Stop(blocked) => match blocked {
            Blocked::Untested => Some(DegradedReason::IndexerUntested {
                version: zingod.version.clone(),
                zebrad: release(facts.zebrad_version.as_deref().unwrap_or_default()).to_owned(),
                network: spec.network,
            }),
            Blocked::NotBundled => Some(DegradedReason::IndexerFailed {
                class: ExitClass::SpawnFailed,
                detail: FailDetail::None,
            }),
            // zebrad's restart: the new run's RPC line and cookie come before zingod's start.
            Blocked::NoZebrad | Blocked::OtherNetwork | Blocked::NoRpc | Blocked::NoCookie => {
                Some(DegradedReason::BackingRestarting)
            }
            Blocked::NotAtTip => starting(IndexerWait::ZebradTip),
            Blocked::ChainMismatch => Some(DegradedReason::ChainMismatch {
                at: zingod.mismatch.unwrap_or_default(),
            }),
            Blocked::DiskLow => Some(DegradedReason::DiskLow {
                free_bytes: zingod.disk.map_or(0, |disk| disk.free_bytes),
            }),
            // The one port move after `PortInUse` is the runner's, under way.
            Blocked::Failed(ExitClass::PortInUse) if runner.ports_tried.len() < 2 => {
                starting(IndexerWait::Boot)
            }
            Blocked::Failed(ExitClass::IndexBroken) => Some(DegradedReason::IndexBroken),
            Blocked::Failed(class) => Some(DegradedReason::IndexerFailed {
                class,
                detail: fail_detail(
                    class,
                    runner
                        .last_exit
                        .as_ref()
                        .and_then(|exit| exit.detail.as_deref()),
                    &runner.ports_tried,
                    &zingod.dir,
                ),
            }),
            // A pause, consent or another check says why: the setup's checks, an open
            // service's pause.
            Blocked::Off
            | Blocked::NoConsent
            | Blocked::Held
            | Blocked::UpgradeUnknown
            | Blocked::NoSpace => starting(IndexerWait::Boot),
        },
        Want::Run(run) => {
            if zingod.restarting {
                return starting(IndexerWait::Boot);
            }
            if runner.stale.is_some() {
                return Some(DegradedReason::IndexerFailed {
                    class: ExitClass::DbLocked,
                    detail: FailDetail::Dir {
                        path: zingod.dir.clone(),
                    },
                });
            }
            if runner.phase.is_active() && runner.running_for.is_some_and(|ran| ran != run) {
                return Some(DegradedReason::BackingRestarting);
            }
            match &runner.phase {
                Phase::Backoff { .. } => return starting(IndexerWait::Backoff),
                Phase::Running => {}
                _ => return starting(IndexerWait::Boot),
            }
            let status = runner.status.as_ref();
            let health = health(
                status,
                runner.misses,
                runner.answering_zero_since,
                facts.height,
                facts.now,
            );
            // The stall rule's verdict after its restart, while zingod says it builds, catches
            // up or serves: its height hasn't moved, whatever its state says.
            if let Some((at, zebrad_at)) = zingod.stuck
                && matches!(
                    health,
                    IndexerHealth::Building { .. }
                        | IndexerHealth::CatchingUp { .. }
                        | IndexerHealth::Ready
                )
            {
                return Some(DegradedReason::IndexerStuck { at, zebrad_at });
            }
            match health {
                IndexerHealth::Boot => starting(IndexerWait::Boot),
                IndexerHealth::Building {
                    served,
                    target,
                    eta_secs,
                } => Some(DegradedReason::IndexBuilding {
                    served,
                    target,
                    eta_secs,
                }),
                IndexerHealth::CatchingUp { served } => Some(DegradedReason::IndexBuilding {
                    served: Some(served),
                    target: None,
                    eta_secs: None,
                }),
                IndexerHealth::Down { reason, message } => Some(DegradedReason::IndexerDown {
                    why: DownReason::Zingod { reason, message },
                }),
                IndexerHealth::NoValidator => Some(DegradedReason::IndexerDown {
                    why: DownReason::NoValidator,
                }),
                IndexerHealth::NoAnswer => Some(DegradedReason::IndexerDown {
                    why: DownReason::NoAnswer,
                }),
                IndexerHealth::Ready => {
                    let Some((_, blocks)) = served_behind(status, facts.height) else {
                        return starting(IndexerWait::Boot);
                    };
                    let behind_long = zingod.behind_since.is_some_and(|since| {
                        facts.now.duration_since(since).unwrap_or_default() >= light::BEHIND_GRACE
                    });
                    (blocks > light::AT_TIP_BLOCKS
                        && (blocks >= light::FAR_BEHIND_BLOCKS || behind_long))
                        .then_some(DegradedReason::IndexBehind { blocks })
                }
            }
        }
    }
}

/// Whether zingod is behind zebrad by more than [`light::AT_TIP_BLOCKS`] now, for the actor's
/// `behind_since`.
pub fn behind(status: Option<&IndexerStatus>, zebrad: Option<u64>) -> bool {
    served_behind(status, zebrad).is_some_and(|(_, blocks)| blocks > light::AT_TIP_BLOCKS)
}

/// Whether zingod serves now: the test on this computer, Open and the light monitor wait for it.
pub fn ready(spec: &PublicServiceSpec, facts: &Facts) -> bool {
    spec.backend == LightBackend::Zingod
        && problem(spec, facts).is_none()
        && facts.zingod.as_ref().is_some_and(|zingod| {
            zingod.runner.phase == Phase::Running && zingod.runner.listening.is_some()
        })
}

/// zingod's loopback address: the spec's port, or the one the runner moved to.
pub fn endpoint(spec: &PublicServiceSpec, facts: &Facts) -> Option<SocketAddr> {
    let port = facts
        .zingod
        .as_ref()
        .and_then(|zingod| zingod.runner.port)
        .or(spec.zingod.as_ref().map(|zingod| zingod.port))?;
    Some(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
}

/// A zingod problem as the finding of the setup's check that waits for it: the build's
/// progress as its own finding, the rest with the panel's reason.
fn problem_finding(why: DegradedReason) -> Finding {
    match why {
        DegradedReason::IndexBuilding {
            served,
            target,
            eta_secs,
        } => Finding::IndexBuilding {
            served,
            target,
            eta_secs,
        },
        DegradedReason::ChainMismatch { at } => Finding::ChainMismatch { at },
        why => Finding::IndexerProblem { why },
    }
}

/// Whether a zingod problem clears by itself, so the setup step that shows it runs instead
/// of failing.
pub fn problem_waits(why: &DegradedReason) -> bool {
    matches!(
        why,
        DegradedReason::BackingRestarting
            | DegradedReason::BackingBehind { .. }
            | DegradedReason::IndexerStarting { .. }
            | DegradedReason::IndexBuilding { .. }
            | DegradedReason::IndexBehind { .. }
            | DegradedReason::IndexerDown { .. }
            | DegradedReason::DiskLow { .. }
    )
}

/// The check `id` that waits for zingod to serve, for the test on this computer and Open:
/// `None` once it serves.
pub fn wait_for_index(spec: &PublicServiceSpec, facts: &Facts, id: CheckId) -> Option<CheckResult> {
    if ready(spec, facts) {
        return None;
    }
    let outcome = match problem(spec, facts) {
        Some(why) => CheckOutcome::Fail {
            finding: problem_finding(why),
        },
        None => CheckOutcome::Running,
    };
    Some(CheckResult {
        id,
        outcome,
        at: None,
    })
}

fn check(id: CheckId, outcome: CheckOutcome) -> CheckResult {
    CheckResult {
        id,
        outcome,
        at: None,
    }
}

/// The Service step with zingod: the pair, the upgrades, the room, zebrad at the tip, zingod's
/// port, its index, its tip and the wallet check, each with its finding.
pub fn service_checks(spec: &PublicServiceSpec, facts: &Facts) -> Vec<CheckResult> {
    let fail = |id, finding| check(id, CheckOutcome::Fail { finding });
    let warn = |id, finding| check(id, CheckOutcome::Warn { finding });
    let pass = |id| check(id, CheckOutcome::Pass);
    let running = |id| check(id, CheckOutcome::Running);
    let not_run = |id| check(id, CheckOutcome::NotRun);
    let zingod = facts.zingod.as_ref();
    let version = zingod.map_or("", |zingod| zingod.version.as_str());
    let zebrad_version = facts.zebrad_version.clone().unwrap_or_default();

    let tested = if tested_pair(version, &zebrad_version, spec.network) {
        pass(CheckId::ZingodTested)
    } else {
        fail(
            CheckId::ZingodTested,
            Finding::UntestedZingod {
                version: version.to_owned(),
                zebrad: release(&zebrad_version).to_owned(),
                network: spec.network,
            },
        )
    };
    let upgrades = match unknown_upgrade(version, &facts.upgrades) {
        Some(upgrade) => fail(
            CheckId::UpgradesKnown,
            Finding::UpgradeUnknown {
                upgrade: upgrade.name.clone(),
                height: Some(upgrade.activation_height),
            },
        ),
        None if facts.upgrades.is_empty() => running(CheckId::UpgradesKnown),
        None => pass(CheckId::UpgradesKnown),
    };
    let space = match zingod.and_then(|zingod| zingod.disk.map(|disk| (zingod, disk))) {
        None => running(CheckId::IndexSpace),
        Some((zingod, disk)) if !has_room(spec.network, zingod, disk) => fail(
            CheckId::IndexSpace,
            Finding::IndexSpace {
                path: zingod.dir.clone(),
                free_bytes: disk.free_bytes,
                need_bytes: space_needed(spec.network),
            },
        ),
        Some((zingod, _)) if zingod.network_fs => warn(
            CheckId::IndexSpace,
            Finding::NetworkFilesystem {
                path: zingod.dir.clone(),
            },
        ),
        Some((zingod, _)) => match zingod.total_memory {
            Some(total) if total < MEMORY_WANTED => warn(
                CheckId::IndexSpace,
                Finding::LowMemory {
                    total_bytes: total,
                    want_bytes: MEMORY_WANTED,
                },
            ),
            _ => pass(CheckId::IndexSpace),
        },
    };
    // The setup goes on while zebrad syncs, as with zebrad's endpoint; zingod starts once it
    // is at the tip.
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
    let runner = zingod.map(|zingod| &zingod.runner);
    let blocked = zingod.map(|zingod| want(&start_facts(spec, facts), &zingod.runner));
    let listening = runner.and_then(|runner| runner.listening);
    let opened = match (&blocked, runner) {
        _ if matches!(facts.phase, Phase::Stopped | Phase::Failed { .. }) => {
            fail(CheckId::EndpointOpened, Finding::ZebradNotRunning)
        }
        (_, Some(runner)) if listening.is_some() && runner.phase == Phase::Running => {
            pass(CheckId::EndpointOpened)
        }
        (Some(Want::Stop(Blocked::Failed(ExitClass::PortInUse))), Some(runner))
            if runner.ports_tried.len() >= 2 =>
        {
            fail(
                CheckId::EndpointOpened,
                Finding::IndexerPortTaken {
                    ports: runner.ports_tried.clone(),
                },
            )
        }
        (Some(Want::Run(_)), _) => running(CheckId::EndpointOpened),
        (Some(Want::Stop(Blocked::Failed(_) | Blocked::DiskLow | Blocked::ChainMismatch)), _) => {
            running(CheckId::EndpointOpened)
        }
        _ => not_run(CheckId::EndpointOpened),
    };
    // What the index waits for, once zingod may start; another check says why it may not.
    let gated = matches!(
        blocked,
        None | Some(Want::Stop(
            Blocked::Off
                | Blocked::NoConsent
                | Blocked::Held
                | Blocked::Untested
                | Blocked::NotBundled
                | Blocked::UpgradeUnknown
                | Blocked::NoSpace
        ))
    );
    let index = if gated {
        not_run(CheckId::IndexReady)
    } else if ready(spec, facts) {
        pass(CheckId::IndexReady)
    } else {
        match problem(spec, facts) {
            Some(DegradedReason::IndexBuilding {
                served,
                target,
                eta_secs,
            }) => warn(
                CheckId::IndexReady,
                Finding::IndexBuilding {
                    served,
                    target,
                    eta_secs,
                },
            ),
            Some(why) => fail(CheckId::IndexReady, problem_finding(why)),
            None => running(CheckId::IndexReady),
        }
    };
    let ready = ready(spec, facts);
    let tip_matches = match zingod {
        Some(zingod) if zingod.mismatch.is_some() => fail(
            CheckId::TipMatches,
            Finding::ChainMismatch {
                at: zingod.mismatch.unwrap_or_default(),
            },
        ),
        Some(IndexerFacts {
            tip:
                Some(Checked {
                    at,
                    result: TipCheck::Match,
                }),
            ..
        }) if ready => CheckResult {
            id: CheckId::TipMatches,
            outcome: CheckOutcome::Pass,
            at: Some(*at),
        },
        _ if ready => running(CheckId::TipMatches),
        _ => not_run(CheckId::TipMatches),
    };
    let wallet = match &facts.wallet {
        Some(checked) if ready => CheckResult {
            id: CheckId::WalletCheck,
            outcome: match &checked.result {
                Ok(_) => CheckOutcome::Pass,
                Err(problem) => CheckOutcome::Fail {
                    finding: Finding::WalletCheck {
                        problem: problem.clone(),
                    },
                },
            },
            at: Some(checked.at),
        },
        _ if ready => running(CheckId::WalletCheck),
        _ => not_run(CheckId::WalletCheck),
    };
    vec![
        tested,
        upgrades,
        space,
        tip,
        opened,
        index,
        tip_matches,
        wallet,
    ]
}

/// The Server card's facts, while the spec records zingod's folder.
pub fn stats(spec: &PublicServiceSpec, facts: &Facts) -> Option<IndexerStats> {
    let recorded = spec.zingod.as_ref()?;
    let zingod = facts.zingod.as_ref()?;
    let runner = &zingod.runner;
    Some(IndexerStats {
        version: zingod.version.clone(),
        phase: runner.phase.clone(),
        status: runner.status.clone(),
        dir: recorded.dir.clone(),
        port: runner.port.unwrap_or(recorded.port),
        index_bytes: runner.index_bytes.map(|(bytes, _)| bytes),
        index_partial: runner.index_bytes.is_some_and(|(_, partial)| partial),
        free_bytes: zingod.disk.map(|disk| disk.free_bytes),
        resources: runner.resources.map(|resources| IndexerResources {
            rss_bytes: resources.rss_bytes,
            cpu_millicores: resources.cpu_millicores,
            read_bytes: resources.read_bytes,
            written_bytes: resources.written_bytes,
        }),
        last_exit: runner.last_exit.clone(),
    })
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
                space: true,
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
        let cases: [(&str, Change, Blocked); 18] = [
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
            ("no room", |s, _| s.space = false, Blocked::NoSpace),
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

    /// A stop Zeal asked for is a stop however zingod ended it: zingod 1.1.0 can abort on
    /// `SIGTERM` (docs/PUBLIC-SERVICES.md, "How Zeal runs zingod"), and the supervisor then
    /// reports `Stopped` with the panic kept in the exit. zingod may start again at once.
    #[test]
    fn a_requested_stop_that_aborted_doesnt_keep_zingod_stopped() {
        let (start, mut zingod) = ready();
        zingod.phase = Phase::Stopped;
        zingod.last_exit = Some(ExitInfo {
            code: None,
            class: ExitClass::Crash,
            detail: Some("header sync publishes the verified chain until cancel".into()),
            at: at(7),
            requested: true,
        });
        assert_eq!(want(&start, &zingod), Want::Run(RUN));
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

    /// A restart the actor asks for holds from the new attempt until zingod runs for it, from
    /// the attempt the runner started for rather than a phase it may not see.
    #[test]
    fn a_restart_holds_until_zingod_runs_for_the_new_attempt() {
        let running = |phase: Phase, started_attempt| ZingodFacts {
            phase,
            started_attempt,
            ..ZingodFacts::default()
        };
        assert!(!running(Phase::Running, Some(3)).restarting(3));
        assert!(running(Phase::Running, Some(3)).restarting(4), "asked");
        assert!(running(Phase::Starting, Some(3)).restarting(4));
        assert!(
            !running(Phase::Stopping, Some(3)).restarting(4),
            "stopping: problem() sees zingod as starting from here"
        );
        assert!(!running(Phase::Stopped, Some(3)).restarting(4));
        assert!(
            !running(Phase::Starting, Some(4)).restarting(4),
            "started again"
        );
        assert!(
            !running(Phase::Running, Some(4)).restarting(4),
            "running again, with no stopped phase seen between"
        );
        assert!(
            !running(
                Phase::Failed {
                    class: ExitClass::Crash
                },
                Some(3)
            )
            .restarting(4),
            "Try again after a failure starts zingod, nothing to restart"
        );
        assert!(!running(Phase::Stopped, None).restarting(0));
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

    #[test]
    fn a_skipped_height_keeps_the_last_result() {
        use TipCheck::*;
        let first = keep_tip(None, Skipped, at(0));
        assert_eq!(first.result, Skipped, "nothing to keep");
        let matched = keep_tip(Some(&first), Match, at(60));
        assert_eq!(matched.result, Match);
        let kept = keep_tip(Some(&matched), Skipped, at(120));
        assert_eq!(
            kept,
            Checked {
                at: at(120),
                result: Match
            }
        );
        let mismatch = keep_tip(Some(&kept), Mismatch, at(180));
        assert_eq!(mismatch.result, Mismatch);
        assert_eq!(keep_tip(Some(&mismatch), Skipped, at(240)).result, Mismatch);
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

/// The light client server's rules with zingod as the backend: the Service step, every row of
/// the failure table, and the front door's mode for each.
#[cfg(test)]
mod service_tests {
    use std::path::PathBuf;

    use launcher_types::{
        NodeId,
        public::{
            CONSENT_VERSION, CertStatus, Consent, GatewayMode, LightdInfo, PauseReason,
            PublicState, StepId, StepState, ZingodSpec,
        },
    };

    use super::*;
    use crate::public::{current_step, gateway_mode, open_gates, pause, plan, state};

    const NOW: u64 = 30_000_000;
    const HEIGHT: u64 = 4_612_019;
    const GB: u64 = 1_000_000_000;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    const RUN: ZebradRun = ZebradRun {
        pid: 7,
        started_at: SystemTime::UNIX_EPOCH,
    };

    fn dir() -> PathBuf {
        PathBuf::from("/mnt/zebra/zingod/testnet")
    }

    fn upgrade(name: &str, height: u64, active: bool) -> NetworkUpgrade {
        NetworkUpgrade {
            name: name.into(),
            branch_id: "77190ad9".into(),
            activation_height: height,
            status: if active {
                UpgradeStatus::Active
            } else {
                UpgradeStatus::Pending
            },
        }
    }

    fn serving_status(served: u64) -> IndexerStatus {
        IndexerStatus {
            state: IndexerState::Serving,
            served: Some(served),
            tip: Some(HEIGHT),
            network: "testnet".into(),
            trusted: Some(1),
            answering: Some(1),
            alarms: Vec::new(),
            version: "1.1.0".into(),
        }
    }

    /// An open Testnet service on zingod, serving at zebrad's height with every check clean.
    fn serving() -> (PublicServiceSpec, Facts) {
        let mut spec = PublicServiceSpec::new_light_server(
            NodeId::new("zebrad-testnet").unwrap(),
            Network::Testnet,
        );
        spec.backend = LightBackend::Zingod;
        spec.zingod = Some(ZingodSpec {
            port: 18137,
            dir: dir(),
        });
        spec.consent = Some(Consent {
            version: CONSENT_VERSION,
            at: at(0),
            hostname: String::new(),
            backend: LightBackend::Zingod,
        });
        spec.opened = true;
        let mut facts = Facts::new(at(NOW), Network::Testnet);
        facts.zebrad_version = Some("7.0.0".into());
        facts.phase = Phase::Running;
        facts.height = Some(HEIGHT);
        facts.tip = Some(HEIGHT);
        facts.at_tip_since = Some(at(NOW - 86_400));
        facts.upgrades = vec![upgrade("NU7", 4_465_026, true)];
        facts.cert = Some(CertStatus {
            not_after: Some(at(NOW + 60 * 86_400)),
            not_before: Some(at(NOW - 29 * 86_400)),
            renew_at: None,
            staging_validated_at: Some(at(0)),
            ordering: None,
            next_try: None,
            last_problem: None,
        });
        facts.wallet = Some(Checked {
            at: at(NOW),
            result: Ok(LightdInfo::default()),
        });
        facts.zingod = Some(IndexerFacts {
            bundled: true,
            version: "1.1.0".into(),
            dir: dir(),
            disk: Some(DiskReading {
                free_bytes: 412 * GB,
                free_inodes: Some(1_000_000),
            }),
            network_fs: false,
            total_memory: Some(16 * GB),
            zebrad: Some(ZebradFacts {
                run: RUN,
                on_network: true,
                rpc_addr: Some(SocketAddr::from(([127, 0, 0, 1], 18232))),
                cookie: Cookie::Present,
                at_tip_once: true,
            }),
            runner: ZingodFacts {
                phase: Phase::Running,
                running_for: Some(RUN),
                status: Some(serving_status(HEIGHT)),
                disk_allows: true,
                port: Some(18137),
                ports_tried: vec![18137],
                listening: Some(SocketAddr::from(([127, 0, 0, 1], 18137))),
                ..ZingodFacts::default()
            },
            behind_since: None,
            stuck: None,
            mismatch: None,
            tip: Some(Checked {
                at: at(NOW - 30),
                result: TipCheck::Match,
            }),
            restarting: false,
        });
        (spec, facts)
    }

    fn zingod(facts: &mut Facts) -> &mut IndexerFacts {
        facts.zingod.as_mut().unwrap()
    }

    fn outcome(spec: &PublicServiceSpec, facts: &Facts, id: CheckId) -> CheckOutcome {
        plan(spec, facts)
            .into_iter()
            .flat_map(|step| step.checks)
            .find(|check| check.id == id)
            .unwrap_or_else(|| panic!("no {id:?}"))
            .outcome
    }

    fn degraded_with(spec: &PublicServiceSpec, facts: &Facts) -> PublicState {
        state(spec, facts, &plan(spec, facts), at(NOW))
    }

    #[test]
    fn the_default_backend_stays_zebrads_endpoint() {
        assert_eq!(DEFAULT_BACKEND, LightBackend::ZebradEndpoint);
        assert_eq!(DEFAULT_BACKEND, LightBackend::default());
    }

    #[test]
    fn a_healthy_zingod_serves() {
        let (spec, facts) = serving();
        assert!(ready(&spec, &facts));
        assert_eq!(problem(&spec, &facts), None);
        let steps = plan(&spec, &facts);
        let service = steps
            .iter()
            .find(|step| step.id == StepId::Service)
            .unwrap();
        assert_eq!(service.state, StepState::Done, "{service:#?}");
        let ids: Vec<CheckId> = service.checks.iter().map(|check| check.id).collect();
        assert_eq!(
            ids,
            [
                CheckId::ZingodTested,
                CheckId::UpgradesKnown,
                CheckId::IndexSpace,
                CheckId::AtTip,
                CheckId::EndpointOpened,
                CheckId::IndexReady,
                CheckId::TipMatches,
                CheckId::WalletCheck,
            ]
        );
        assert!(
            service
                .checks
                .iter()
                .all(|check| check.outcome == CheckOutcome::Pass)
        );
        assert_eq!(degraded_with(&spec, &facts), PublicState::Serving);
        assert_eq!(
            gateway_mode(&PublicState::Serving, &facts),
            Some(GatewayMode::Open)
        );
    }

    #[test]
    fn the_service_steps_checks_carry_their_findings() {
        let (mut spec, facts) = serving();
        spec.opened = false;
        // An untested pair: a zebrad pin or channel that moved.
        let mut untested = facts.clone();
        untested.zebrad_version = Some("7.1.0-alpha.0".into());
        assert_eq!(
            outcome(&spec, &untested, CheckId::ZingodTested),
            CheckOutcome::Fail {
                finding: Finding::UntestedZingod {
                    version: "1.1.0".into(),
                    zebrad: "7.1.0-alpha.0".into(),
                    network: Network::Testnet,
                }
            }
        );
        // An active upgrade zingod doesn't know; a scheduled one is Z4's.
        let mut nu8 = facts.clone();
        nu8.upgrades.push(upgrade("NU8", 4_700_000, true));
        assert_eq!(
            outcome(&spec, &nu8, CheckId::UpgradesKnown),
            CheckOutcome::Fail {
                finding: Finding::UpgradeUnknown {
                    upgrade: "NU8".into(),
                    height: Some(4_700_000),
                }
            }
        );
        let mut scheduled = facts.clone();
        scheduled.upgrades.push(upgrade("NU8", 4_900_000, false));
        assert_eq!(
            outcome(&spec, &scheduled, CheckId::UpgradesKnown),
            CheckOutcome::Pass
        );
        // Room: 50 GB on Testnet, what is built counting.
        let mut short = facts.clone();
        zingod(&mut short).disk = Some(DiskReading {
            free_bytes: 30 * GB,
            free_inodes: None,
        });
        zingod(&mut short).runner.index_bytes = None;
        assert_eq!(
            outcome(&spec, &short, CheckId::IndexSpace),
            CheckOutcome::Fail {
                finding: Finding::IndexSpace {
                    path: dir(),
                    free_bytes: 30 * GB,
                    need_bytes: 50 * GB,
                }
            }
        );
        assert_eq!(
            want(
                &start_facts(&spec, &short),
                &zingod(&mut short.clone()).runner
            ),
            Want::Stop(Blocked::NoSpace),
            "no build starts without the room"
        );
        zingod(&mut short).runner.index_bytes = Some((25 * GB, false));
        assert_eq!(
            outcome(&spec, &short, CheckId::IndexSpace),
            CheckOutcome::Pass
        );
        let mut nfs = facts.clone();
        zingod(&mut nfs).network_fs = true;
        assert_eq!(
            outcome(&spec, &nfs, CheckId::IndexSpace),
            CheckOutcome::Warn {
                finding: Finding::NetworkFilesystem { path: dir() }
            }
        );
        let mut small = facts.clone();
        zingod(&mut small).total_memory = Some(4 * GB);
        assert_eq!(
            outcome(&spec, &small, CheckId::IndexSpace),
            CheckOutcome::Warn {
                finding: Finding::LowMemory {
                    total_bytes: 4 * GB,
                    want_bytes: 8 * GB,
                }
            }
        );
        // The build: a warning, so the setup goes on.
        let mut building = facts.clone();
        zingod(&mut building).runner.status = Some(IndexerStatus {
            state: IndexerState::Syncing {
                progress: Some(launcher_types::zingod::SyncProgress {
                    height: 1_204_331,
                    target: Some(HEIGHT),
                    milli_blocks_per_sec: Some(200_000),
                }),
            },
            ..serving_status(1_204_331)
        });
        assert_eq!(
            outcome(&spec, &building, CheckId::IndexReady),
            CheckOutcome::Warn {
                finding: Finding::IndexBuilding {
                    served: Some(1_204_331),
                    target: Some(HEIGHT),
                    eta_secs: Some((HEIGHT - 1_204_331) * 1_000 / 200_000),
                }
            }
        );
        let steps = plan(&spec, &building);
        let state_of = |id| steps.iter().find(|step| step.id == id).unwrap().state;
        assert_eq!(
            state_of(StepId::Service),
            StepState::Done,
            "the setup goes on while the index builds"
        );
        assert_eq!(
            outcome(&spec, &building, CheckId::TipMatches),
            CheckOutcome::NotRun
        );
        // The ports zingod tried.
        let mut ports = facts.clone();
        let runner = &mut zingod(&mut ports).runner;
        runner.phase = Phase::Failed {
            class: ExitClass::PortInUse,
        };
        runner.listening = None;
        runner.ports_tried = vec![18137, 18138];
        assert_eq!(
            outcome(&spec, &ports, CheckId::EndpointOpened),
            CheckOutcome::Fail {
                finding: Finding::IndexerPortTaken {
                    ports: vec![18137, 18138],
                }
            }
        );
        // A chain mismatch.
        let mut mismatch = facts.clone();
        zingod(&mut mismatch).mismatch = Some(4_611_980);
        assert_eq!(
            outcome(&spec, &mismatch, CheckId::TipMatches),
            CheckOutcome::Fail {
                finding: Finding::ChainMismatch { at: 4_611_980 }
            }
        );
        // zebrad still syncing: zingod waits, and its checks with it.
        let mut syncing = facts;
        syncing.height = Some(2_000_000);
        zingod(&mut syncing).zebrad.as_mut().unwrap().at_tip_once = false;
        zingod(&mut syncing).runner = ZingodFacts {
            disk_allows: true,
            ..ZingodFacts::default()
        };
        assert!(matches!(
            outcome(&spec, &syncing, CheckId::AtTip),
            CheckOutcome::Warn {
                finding: Finding::Syncing { .. }
            }
        ));
        assert_eq!(
            outcome(&spec, &syncing, CheckId::EndpointOpened),
            CheckOutcome::NotRun
        );
        assert_eq!(
            outcome(&spec, &syncing, CheckId::IndexReady),
            CheckOutcome::Fail {
                finding: Finding::IndexerProblem {
                    why: DegradedReason::IndexerStarting {
                        waiting: IndexerWait::ZebradTip
                    }
                }
            }
        );
    }

    /// The setup's test on this computer and Open wait for zingod's index.
    #[test]
    fn the_local_test_and_open_wait_for_the_index() {
        let (mut spec, mut facts) = serving();
        spec.opened = false;
        spec.name = Some(launcher_types::public::PublicHostname::parse("zec.example.com").unwrap());
        zingod(&mut facts).runner.status = Some(IndexerStatus {
            state: IndexerState::Syncing { progress: None },
            ..serving_status(0)
        });
        assert!(!ready(&spec, &facts));
        assert!(matches!(
            outcome(&spec, &facts, CheckId::LocalTest),
            CheckOutcome::Fail {
                finding: Finding::IndexBuilding { .. }
            }
        ));
        let steps = plan(&spec, &facts);
        let local = steps
            .iter()
            .find(|step| step.id == StepId::LocalTest)
            .unwrap();
        assert_ne!(local.state, StepState::Failed, "it waits");
        assert!(
            open_gates(&spec, &facts).is_err(),
            "Open waits for the index too"
        );
        let (mut spec, facts) = serving();
        spec.opened = false;
        assert_eq!(
            outcome(&spec, &facts, CheckId::LocalTest),
            CheckOutcome::Running
        );
    }

    /// zingod's first tip cross-check and wallet check, under way once it serves, leave the
    /// setup where it is: the Service step stays done, so the front door keeps the later step's
    /// mode, and Open waits for them. A failed check still counts.
    #[test]
    fn the_first_checks_once_zingod_serves_dont_hold_the_service_step() {
        let (mut spec, mut facts) = serving();
        spec.opened = false;
        zingod(&mut facts).tip = None;
        facts.wallet = None;
        assert!(ready(&spec, &facts));
        assert_eq!(
            outcome(&spec, &facts, CheckId::TipMatches),
            CheckOutcome::Running
        );
        assert_eq!(
            outcome(&spec, &facts, CheckId::WalletCheck),
            CheckOutcome::Running
        );
        let steps = plan(&spec, &facts);
        let service = steps
            .iter()
            .find(|step| step.id == StepId::Service)
            .unwrap();
        assert_eq!(service.state, StepState::Done, "{service:#?}");
        assert_ne!(current_step(&steps), Some(StepId::Service));
        assert_eq!(
            open_gates(&spec, &facts).unwrap_err().check,
            Some(CheckId::TipMatches),
            "Open waits for the cross-check"
        );

        let mut failed = facts.clone();
        failed.wallet = Some(Checked {
            at: at(NOW),
            result: Err(launcher_types::public::WalletProblem::NoAnswer {
                detail: "reset".into(),
            }),
        });
        let steps = plan(&spec, &failed);
        let service = steps
            .iter()
            .find(|step| step.id == StepId::Service)
            .unwrap();
        assert_eq!(service.state, StepState::Failed, "{service:#?}");
    }

    /// Every zingod row of the failure table, on an open service: its reason, and the front
    /// door stays up in `Unavailable`, the untested pair and the catch-up after a zebrad
    /// restart included.
    #[test]
    fn every_zingod_problem_keeps_the_front_door_unavailable() {
        type Change = fn(&mut Facts);
        let cases: Vec<(&str, Change, DegradedReason)> = vec![
            (
                "building",
                |f| {
                    zingod(f).runner.status = Some(IndexerStatus {
                        state: IndexerState::Syncing {
                            progress: Some(launcher_types::zingod::SyncProgress {
                                height: 1_204_331,
                                target: Some(9_999_999),
                                milli_blocks_per_sec: None,
                            }),
                        },
                        ..serving_status(1_204_331)
                    })
                },
                DegradedReason::IndexBuilding {
                    served: Some(1_204_331),
                    target: Some(HEIGHT),
                    eta_secs: None,
                },
            ),
            (
                "building, early",
                |f| {
                    zingod(f).runner.status = Some(IndexerStatus {
                        state: IndexerState::Syncing { progress: None },
                        served: None,
                        ..serving_status(0)
                    })
                },
                DegradedReason::IndexBuilding {
                    served: None,
                    target: None,
                    eta_secs: None,
                },
            ),
            (
                "catching up after a zebrad restart, mempool off",
                |f| {
                    zingod(f).runner.status = Some(IndexerStatus {
                        state: IndexerState::Syncing {
                            progress: Some(launcher_types::zingod::SyncProgress {
                                height: HEIGHT,
                                target: Some(9_999_999),
                                milli_blocks_per_sec: None,
                            }),
                        },
                        ..serving_status(HEIGHT)
                    })
                },
                DegradedReason::IndexBuilding {
                    served: Some(HEIGHT),
                    target: None,
                    eta_secs: None,
                },
            ),
            (
                "restarting with zebrad",
                |f| {
                    zingod(f).zebrad.as_mut().unwrap().run = ZebradRun {
                        pid: 8,
                        started_at: at(5),
                    }
                },
                DegradedReason::BackingRestarting,
            ),
            (
                "zebrad's new run without its RPC line yet",
                |f| zingod(f).zebrad.as_mut().unwrap().rpc_addr = None,
                DegradedReason::BackingRestarting,
            ),
            (
                "waiting for zebrad's tip",
                |f| zingod(f).zebrad.as_mut().unwrap().at_tip_once = false,
                DegradedReason::IndexerStarting {
                    waiting: IndexerWait::ZebradTip,
                },
            ),
            (
                "booting",
                |f| {
                    zingod(f).runner.status = Some(IndexerStatus {
                        state: IndexerState::Provisioning,
                        ..serving_status(0)
                    })
                },
                DegradedReason::IndexerStarting {
                    waiting: IndexerWait::Boot,
                },
            ),
            (
                "no validator for 30 s",
                |f| {
                    let runner = &mut zingod(f).runner;
                    runner.answering_zero_since = Some(at(NOW - 31));
                },
                DegradedReason::IndexerDown {
                    why: DownReason::NoValidator,
                },
            ),
            (
                "down",
                |f| {
                    zingod(f).runner.status = Some(IndexerStatus {
                        state: IndexerState::Down {
                            reason: "tip_unvouched".into(),
                            message: "No validator vouches for the tip".into(),
                        },
                        ..serving_status(HEIGHT)
                    })
                },
                DegradedReason::IndexerDown {
                    why: DownReason::Zingod {
                        reason: "tip_unvouched".into(),
                        message: "No validator vouches for the tip".into(),
                    },
                },
            ),
            (
                "not answering",
                |f| zingod(f).runner.misses = 3,
                DegradedReason::IndexerDown {
                    why: DownReason::NoAnswer,
                },
            ),
            (
                "behind",
                |f| zingod(f).runner.status = Some(serving_status(HEIGHT - 12)),
                DegradedReason::IndexBehind { blocks: 12 },
            ),
            (
                "stuck",
                |f| zingod(f).stuck = Some((4_611_002, 4_611_019)),
                DegradedReason::IndexerStuck {
                    at: 4_611_002,
                    zebrad_at: 4_611_019,
                },
            ),
            (
                "stuck while it says it builds",
                |f| {
                    let zingod = zingod(f);
                    zingod.runner.status = Some(IndexerStatus {
                        state: IndexerState::Syncing {
                            progress: Some(launcher_types::zingod::SyncProgress {
                                height: 1_204_331,
                                target: Some(HEIGHT),
                                milli_blocks_per_sec: None,
                            }),
                        },
                        ..serving_status(1_204_331)
                    });
                    zingod.stuck = Some((1_204_331, HEIGHT));
                },
                DegradedReason::IndexerStuck {
                    at: 1_204_331,
                    zebrad_at: HEIGHT,
                },
            ),
            (
                "stuck while it says it catches up after a zebrad restart",
                |f| {
                    let zingod = zingod(f);
                    zingod.runner.status = Some(IndexerStatus {
                        state: IndexerState::Syncing {
                            progress: Some(launcher_types::zingod::SyncProgress {
                                height: HEIGHT - 40,
                                target: Some(HEIGHT),
                                milli_blocks_per_sec: None,
                            }),
                        },
                        ..serving_status(HEIGHT - 40)
                    });
                    zingod.stuck = Some((HEIGHT - 40, HEIGHT));
                },
                DegradedReason::IndexerStuck {
                    at: HEIGHT - 40,
                    zebrad_at: HEIGHT,
                },
            ),
            (
                "between crashes",
                |f| {
                    zingod(f).runner.phase = Phase::Backoff {
                        until: at(NOW + 2),
                        crashes: 1,
                    }
                },
                DegradedReason::IndexerStarting {
                    waiting: IndexerWait::Backoff,
                },
            ),
            (
                "a crash loop",
                |f| {
                    zingod(f).runner.phase = Phase::Failed {
                        class: ExitClass::Crash,
                    }
                },
                DegradedReason::IndexerFailed {
                    class: ExitClass::Crash,
                    detail: FailDetail::None,
                },
            ),
            (
                "a refused configuration",
                |f| {
                    zingod(f).runner.phase = Phase::Failed {
                        class: ExitClass::ConfigError,
                    }
                },
                DegradedReason::IndexerFailed {
                    class: ExitClass::ConfigError,
                    detail: FailDetail::None,
                },
            ),
            (
                "an unreadable cookie",
                |f| {
                    let runner = &mut zingod(f).runner;
                    runner.phase = Phase::Failed {
                        class: ExitClass::ConfigError,
                    };
                    runner.last_exit = Some(ExitInfo {
                        code: Some(1),
                        class: ExitClass::ConfigError,
                        detail: Some(
                            "cannot read validator cookie /mnt/zebra/testnet/rpc/.cookie: Is a \
                             directory (os error 21)"
                                .into(),
                        ),
                        at: at(NOW),
                        requested: false,
                    });
                },
                DegradedReason::IndexerFailed {
                    class: ExitClass::ConfigError,
                    detail: FailDetail::Cookie {
                        path: PathBuf::from("/mnt/zebra/testnet/rpc/.cookie"),
                        error: "Is a directory (os error 21)".into(),
                    },
                },
            ),
            (
                "the ports taken",
                |f| {
                    let runner = &mut zingod(f).runner;
                    runner.phase = Phase::Failed {
                        class: ExitClass::PortInUse,
                    };
                    runner.ports_tried = vec![18137, 18138];
                },
                DegradedReason::IndexerFailed {
                    class: ExitClass::PortInUse,
                    detail: FailDetail::Ports {
                        tried: vec![18137, 18138],
                    },
                },
            ),
            (
                "a held lock",
                |f| {
                    zingod(f).runner.phase = Phase::Failed {
                        class: ExitClass::DbLocked,
                    }
                },
                DegradedReason::IndexerFailed {
                    class: ExitClass::DbLocked,
                    detail: FailDetail::Dir { path: dir() },
                },
            ),
            (
                "a stale zingod",
                |f| {
                    zingod(f).runner.stale = Some(StaleProcessInfo {
                        pid: 9,
                        binary: "zeal-zingod".into(),
                    })
                },
                DegradedReason::IndexerFailed {
                    class: ExitClass::DbLocked,
                    detail: FailDetail::Dir { path: dir() },
                },
            ),
            (
                "a low disk",
                |f| {
                    let zingod = zingod(f);
                    zingod.runner.disk_allows = false;
                    zingod.disk = Some(DiskReading {
                        free_bytes: 6 * GB,
                        free_inodes: None,
                    });
                },
                DegradedReason::DiskLow { free_bytes: 6 * GB },
            ),
            (
                "a damaged index",
                |f| {
                    zingod(f).runner.phase = Phase::Failed {
                        class: ExitClass::IndexBroken,
                    }
                },
                DegradedReason::IndexBroken,
            ),
            (
                "a chain mismatch",
                |f| zingod(f).mismatch = Some(4_611_980),
                DegradedReason::ChainMismatch { at: 4_611_980 },
            ),
            (
                "a restart Zeal asked for, before zingod stops",
                |f| zingod(f).restarting = true,
                DegradedReason::IndexerStarting {
                    waiting: IndexerWait::Boot,
                },
            ),
            (
                "an untested pair after a zebrad pin or channel change",
                |f| f.zebrad_version = Some("7.1.0-alpha.0".into()),
                DegradedReason::IndexerUntested {
                    version: "1.1.0".into(),
                    zebrad: "7.1.0-alpha.0".into(),
                    network: Network::Testnet,
                },
            ),
        ];
        for (name, change, why) in cases {
            let (spec, mut facts) = serving();
            change(&mut facts);
            let state = degraded_with(&spec, &facts);
            assert_eq!(state, PublicState::Degraded { why: why.clone() }, "{name}");
            assert_eq!(
                gateway_mode(&state, &facts),
                Some(GatewayMode::Unavailable),
                "{name}"
            );
            assert!(!ready(&spec, &facts), "{name}");
        }
    }

    /// Only zebrad stopping and an active upgrade zingod doesn't know close the front door.
    #[test]
    fn a_stopped_zebrad_and_an_unknown_upgrade_close_the_front_door() {
        let (spec, mut facts) = serving();
        facts.phase = Phase::Failed {
            class: ExitClass::EosHalt,
        };
        let state = degraded_with(&spec, &facts);
        assert!(matches!(
            state,
            PublicState::Paused {
                why: PauseReason::BackingStopped,
                ..
            }
        ));
        assert_eq!(gateway_mode(&state, &facts), None);

        let (spec, mut facts) = serving();
        facts.upgrades.push(upgrade("NU8", 4_700_000, true));
        assert_eq!(
            pause(&spec, &facts),
            Some((
                PauseReason::BuildTooOld {
                    upgrade: "NU8".into(),
                    height: 4_700_000,
                },
                launcher_types::public::Resume::Automatic,
            ))
        );
        assert_eq!(gateway_mode(&degraded_with(&spec, &facts), &facts), None);
        assert_eq!(
            want(
                &start_facts(&spec, &facts),
                &facts.zingod.as_ref().unwrap().runner
            ),
            Want::Stop(Blocked::UpgradeUnknown),
            "and zingod doesn't start"
        );
        // zebrad's endpoint doesn't know zingod's upgrades.
        let mut zebrads = PublicServiceSpec {
            backend: LightBackend::ZebradEndpoint,
            backend_port: Some(19067),
            ..spec
        };
        zebrads.consent.as_mut().unwrap().backend = LightBackend::ZebradEndpoint;
        assert_eq!(pause(&zebrads, &facts), None);
    }

    #[test]
    fn the_choice_needs_the_binary_the_network_and_a_tested_pair() {
        let (_, mut facts) = serving();
        assert_eq!(choice(&facts, Network::Testnet), ZingodChoice::Offered);
        assert_eq!(
            choice(&facts, Network::Mainnet),
            ZingodChoice::NotOnNetwork {
                network: Network::Mainnet
            },
            "Mainnet waits for Z4"
        );
        facts.zebrad_version = Some("7.1.0+git.abc".into());
        assert_eq!(
            choice(&facts, Network::Testnet),
            ZingodChoice::Untested {
                version: "1.1.0".into(),
                zebrad: "7.1.0".into(),
                network: Network::Testnet,
            }
        );
        zingod(&mut facts).bundled = false;
        assert_eq!(choice(&facts, Network::Testnet), ZingodChoice::NotBundled);
        facts.zingod = None;
        assert_eq!(choice(&facts, Network::Testnet), ZingodChoice::NotBundled);
    }

    #[test]
    fn a_consent_for_zebrads_endpoint_doesnt_cover_zingod() {
        let (mut spec, facts) = serving();
        spec.consent.as_mut().unwrap().backend = LightBackend::ZebradEndpoint;
        assert!(matches!(
            degraded_with(&spec, &facts),
            PublicState::Paused {
                why: PauseReason::ConsentNeeded,
                ..
            }
        ));
        assert_eq!(
            want(
                &start_facts(&spec, &facts),
                &facts.zingod.as_ref().unwrap().runner
            ),
            Want::Stop(Blocked::NoConsent)
        );
    }

    #[test]
    fn zingod_reads_no_tested_zebrad_list_of_its_own() {
        // zebrad's endpoint wants a zebrad on TESTED_ZEBRAD; zingod wants a tested pair, and
        // an untested zebrad with zingod is a degraded service, not one that isn't available.
        let (spec, mut facts) = serving();
        facts.zebrad_version = Some("7.1.0".into());
        assert!(matches!(
            degraded_with(&spec, &facts),
            PublicState::Degraded {
                why: DegradedReason::IndexerUntested { .. }
            }
        ));
        let mut zebrads = PublicServiceSpec {
            backend: LightBackend::ZebradEndpoint,
            ..spec
        };
        zebrads.consent.as_mut().unwrap().backend = LightBackend::ZebradEndpoint;
        assert!(matches!(
            degraded_with(&zebrads, &facts),
            PublicState::NotAvailable { .. }
        ));
    }

    #[test]
    fn failure_details_come_from_zingods_texts() {
        let path = Path::new("/mnt/zebra/zingod/testnet");
        let captured = include_str!(
            "../../../launcher-core/tests/fixtures/zingod-1.1.0-cookie-dir.stderr.log"
        )
        .trim()
        .trim_start_matches("Error: ");
        assert_eq!(
            fail_detail(ExitClass::ConfigError, Some(captured), &[], path),
            FailDetail::Cookie {
                path: PathBuf::from("<DIR>/cookie-folder/.cookie"),
                error: "Is a directory (os error 21)".into(),
            }
        );
        let limit = "grpc.max_connections = 256 needs 1280 open files (1024 kept for indexes \
                     and the validator) and the limit is 1024: raise the hard limit (systemd \
                     LimitNOFILE=, docker --ulimit nofile=) or lower grpc.max_connections";
        assert_eq!(
            fail_detail(ExitClass::ConfigError, Some(limit), &[], path),
            FailDetail::OpenFiles {
                needed: 1280,
                limit: 1024
            }
        );
        let refused = include_str!(
            "../../../launcher-core/tests/fixtures/zingod-1.1.0-config-error.stderr.log"
        );
        assert_eq!(
            fail_detail(ExitClass::ConfigError, Some(refused), &[], path),
            FailDetail::None
        );
        assert_eq!(
            fail_detail(ExitClass::PortInUse, None, &[18137, 18138], path),
            FailDetail::Ports {
                tried: vec![18137, 18138]
            }
        );
        assert_eq!(
            fail_detail(ExitClass::DbLocked, None, &[], path),
            FailDetail::Dir {
                path: path.to_path_buf()
            }
        );
    }

    #[test]
    fn the_index_folder_is_per_network_beside_the_chain() {
        assert_eq!(
            index_dir(Path::new("/mnt/zebra"), Network::Testnet),
            PathBuf::from("/mnt/zebra/zingod/testnet")
        );
        assert_eq!(space_needed(Network::Mainnet), 100 * GB);
        assert_eq!(space_needed(Network::Testnet), 50 * GB);
    }
}
