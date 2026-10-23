//! Launcher self-updates without Tauri: the update policy, its small on-disk
//! records, the post-update health gate, and the updater actor.
//!
//! The policy is a set of pure functions (when to check, when an automatic
//! install may start, when to warn that checks are stale), so the rules are
//! unit-tested on their own. The actor ([`spawn`]) applies them: it owns the
//! update flow and talks to the network only through an [`UpdateBackend`],
//! which the Tauri app implements with `tauri-plugin-updater`.
//!
//! ```text
//!   UpdaterHandle ──Cmd──▶ actor ──▶ watch<UpdateInfo>
//!   watch<Settings> ──────▶  │  ◀── watch<Option<NodeStats>> (end of support)
//!   watch<Status>  ───────▶  │  ──▶ UpdateBackend: check / download / verify / install
//!                            └───▶ NodeControl: stop zebrad before installing
//! ```
//!
//! In [`UpdateMode::Off`] the actor schedules nothing and never calls the
//! backend unless the user asks for a check.

use std::{
    fs,
    future::Future,
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};

use launcher_types::{
    EosInfo, ExitClass, Phase, RecoveryAction, StaleProcessInfo, Status, UpdateHealthFailure,
    UpdateMode,
};
use serde::{Deserialize, Serialize};
use tokio::{sync::watch, time::Instant};

use crate::{fsutil, paths::Paths, supervisor::SupervisorHandle};

mod actor;

pub use actor::{UpdaterConfig, UpdaterError, UpdaterHandle, spawn};

const DAY: Duration = Duration::from_secs(24 * 60 * 60);

/// The first automatic check runs this long after launch.
pub const FIRST_CHECK_DELAY: Duration = Duration::from_secs(30);
/// Automatic checks repeat about this often, see [`CHECK_JITTER_PERCENT`].
pub const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
/// Each interval is stretched or shrunk by up to this many percent, so
/// launchers started together do not keep hitting the endpoint together.
pub const CHECK_JITTER_PERCENT: u64 = 10;
/// zebrad's end-of-support warning: this close to the estimated halt, Auto
/// mode installs a downloaded update (after a countdown).
pub const EOS_WARNING_WINDOW: Duration = Duration::from_secs(14 * DAY.as_secs());
/// This close to the estimated halt, a stale last check is worth a warning.
pub const EOS_APPROACH_WINDOW: Duration = Duration::from_secs(28 * DAY.as_secs());
/// A last successful check older than this is stale.
pub const STALE_CHECK_AFTER: Duration = Duration::from_secs(14 * DAY.as_secs());
/// How long an automatic install counts down before it starts.
pub const AUTO_INSTALL_COUNTDOWN: Duration = Duration::from_secs(60);
/// After an update, zebrad must reach `Running` with RPC within this long.
pub const HEALTH_GATE_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// The updater's record in the config directory.
pub const RECORD_FILE: &str = "updater.json";
/// The marker written just before an update is installed and checked at the
/// next launch.
pub const PENDING_UPDATE_MARKER: &str = "pending_update_check";

// ---------------------------------------------------------------------------
// Policy
// ---------------------------------------------------------------------------

/// A small seeded pseudo-random source (SplitMix64) for check jitter.
///
/// Not for anything security-related: it only spreads checks out.
#[derive(Clone, Debug)]
pub struct Jitter {
    state: u64,
}

impl Jitter {
    /// A source that yields the same sequence for the same seed.
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// `base` plus or minus up to [`CHECK_JITTER_PERCENT`] percent, to the
    /// millisecond.
    pub fn apply(&mut self, base: Duration) -> Duration {
        let base_ms = u64::try_from(base.as_millis()).unwrap_or(u64::MAX / 2);
        let span = base_ms / 100 * CHECK_JITTER_PERCENT;
        let offset = self.next_u64() % (2 * span + 1);
        Duration::from_millis(base_ms - span + offset)
    }
}

/// When the next automatic check is due: never in Off mode; otherwise
/// [`FIRST_CHECK_DELAY`] after launch for the first one, then a jittered
/// [`CHECK_INTERVAL`] after the previous attempt.
///
/// Manual checks are not governed by this: the user may always ask.
pub fn next_check(
    mode: UpdateMode,
    launched: Instant,
    last_attempt: Option<Instant>,
    jitter: &mut Jitter,
) -> Option<Instant> {
    match mode {
        UpdateMode::Off => None,
        UpdateMode::Notify | UpdateMode::Auto => Some(match last_attempt {
            None => launched + FIRST_CHECK_DELAY,
            Some(at) => at + jitter.apply(CHECK_INTERVAL),
        }),
    }
}

fn eos_within(eos: Option<&EosInfo>, now: SystemTime, window: Duration) -> bool {
    eos.is_some_and(|eos| match now.checked_add(window) {
        Some(limit) => eos.estimated_time <= limit,
        None => true,
    })
}

/// zebrad's end-of-support warning is on: the estimated halt is within
/// [`EOS_WARNING_WINDOW`], or zebrad already refused to run (`halted`).
pub fn eos_warning_active(eos: Option<&EosInfo>, halted: bool, now: SystemTime) -> bool {
    halted || eos_within(eos, now, EOS_WARNING_WINDOW)
}

/// The UI should warn that update checks are stale: the end of support is
/// within [`EOS_APPROACH_WINDOW`] (or zebrad already halted) and no check
/// succeeded in the last [`STALE_CHECK_AFTER`].
pub fn stale_check_warning(
    last_success: Option<SystemTime>,
    eos: Option<&EosInfo>,
    halted: bool,
    now: SystemTime,
) -> bool {
    if !(halted || eos_within(eos, now, EOS_APPROACH_WINDOW)) {
        return false;
    }
    match last_success {
        None => true,
        Some(at) => now
            .duration_since(at)
            .is_ok_and(|age| age > STALE_CHECK_AFTER),
    }
}

/// Auto mode downloads a found update in the background.
pub fn should_auto_download(mode: UpdateMode, can_install: bool) -> bool {
    mode == UpdateMode::Auto && can_install
}

/// A downloaded update starts its install countdown by itself: Auto mode,
/// at the end-of-support warning, unless the user cancelled the countdown
/// for this version. Otherwise only the user starts an install.
pub fn should_auto_install(
    mode: UpdateMode,
    can_install: bool,
    eos_warning: bool,
    dismissed: bool,
) -> bool {
    mode == UpdateMode::Auto && can_install && eos_warning && !dismissed
}

// ---------------------------------------------------------------------------
// Records on disk
// ---------------------------------------------------------------------------

/// What the updater remembers across launches (`updater.json`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdaterRecord {
    /// When a check last reached the endpoint.
    pub last_successful_check: Option<SystemTime>,
}

/// `<config_dir>/updater.json`.
pub fn record_path(paths: &Paths) -> PathBuf {
    paths.config_dir().join(RECORD_FILE)
}

/// Loads the record; a missing or unreadable file gives the default.
pub fn load_record(path: &Path) -> UpdaterRecord {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|err| {
            tracing::warn!(%err, path = %path.display(), "ignoring an unreadable updater record");
            UpdaterRecord::default()
        }),
        Err(err) if err.kind() == io::ErrorKind::NotFound => UpdaterRecord::default(),
        Err(err) => {
            tracing::warn!(%err, path = %path.display(), "could not read the updater record");
            UpdaterRecord::default()
        }
    }
}

/// Saves the record atomically.
pub fn save_record(path: &Path, record: &UpdaterRecord) -> io::Result<()> {
    let json = serde_json::to_vec_pretty(record).map_err(io::Error::other)?;
    fsutil::write_atomic(path, &json)
}

/// The marker written just before an update is installed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingUpdate {
    /// The launcher version that installed the update.
    pub from_version: String,
    /// The version being installed.
    pub to_version: String,
    /// Whether zebrad was running (or starting) before the update, so the
    /// next launch must bring it back.
    pub zebrad_was_running: bool,
    /// When the marker was written.
    pub written_at: SystemTime,
}

/// `<config_dir>/pending_update_check`.
pub fn marker_path(paths: &Paths) -> PathBuf {
    paths.config_dir().join(PENDING_UPDATE_MARKER)
}

/// Writes the marker atomically.
pub fn write_marker(path: &Path, pending: &PendingUpdate) -> io::Result<()> {
    let json = serde_json::to_vec_pretty(pending).map_err(io::Error::other)?;
    fsutil::write_atomic(path, &json)
}

/// Reads the marker. `Ok(None)` when there is none; an unreadable marker is
/// an `InvalidData` error.
pub fn read_marker(path: &Path) -> io::Result<Option<PendingUpdate>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

/// Removes the marker; a missing marker is fine.
pub fn clear_marker(path: &Path) -> io::Result<()> {
    fsutil::remove_if_exists(path)
}

// ---------------------------------------------------------------------------
// Backends
// ---------------------------------------------------------------------------

/// A newer release announced by the update endpoint.
#[derive(Clone, Debug)]
pub struct FoundUpdate<R> {
    /// Its version.
    pub version: String,
    /// Its release notes.
    pub notes: Option<String>,
    /// When it was published.
    pub pub_date: Option<SystemTime>,
    /// The backend's handle for downloading and installing it.
    pub release: R,
}

/// Progress callback for [`UpdateBackend::download`]: bytes in this chunk,
/// and the total size when known.
pub type OnChunk = Box<dyn FnMut(usize, Option<u64>) + Send>;

/// The network and installer side of updates. The Tauri app implements it
/// with `tauri-plugin-updater`; tests use a fake.
///
/// Only [`UpdateBackend::check`] and [`UpdateBackend::download`] make
/// network requests.
pub trait UpdateBackend: Send + Sync + 'static {
    /// A found release, as the backend needs it to download and install.
    type Release: Clone + Send + Sync + 'static;

    /// Whether this installation can replace itself. False for the `.deb`
    /// package, whose updates need root and go through the package manager.
    fn can_install(&self) -> bool;

    /// Asks the endpoint for a release newer than the running one.
    fn check(
        &self,
    ) -> impl Future<Output = Result<Option<FoundUpdate<Self::Release>>, String>> + Send;

    /// Downloads the release's package.
    fn download(
        &self,
        release: &Self::Release,
        on_chunk: OnChunk,
    ) -> impl Future<Output = Result<Vec<u8>, String>> + Send;

    /// Checks the package's signature against the public key built into
    /// the app (and, when configured, the signed version). This hashes the
    /// whole package, so the actor calls it on the blocking pool, once per
    /// install, before zebrad is stopped.
    fn verify(&self, release: &Self::Release, bytes: &[u8]) -> Result<(), String>;

    /// Installs the package. On Windows this ends the process when the
    /// installer starts.
    fn install(
        &self,
        release: &Self::Release,
        bytes: Arc<Vec<u8>>,
    ) -> impl Future<Output = Result<(), String>> + Send;

    /// Relaunches the app after a successful install.
    fn restart(&self);
}

/// What the updater needs from the zebrad supervisor.
pub trait NodeControl: Clone + Send + Sync + 'static {
    /// The supervisor's status.
    fn watch_status(&self) -> watch::Receiver<Status>;
    /// Stops zebrad gracefully (then by force after the grace period) and
    /// waits until it exited.
    fn stop(&self) -> impl Future<Output = Result<(), String>> + Send;
    /// Starts zebrad and waits until it accepted its configuration.
    fn start(&self) -> impl Future<Output = Result<(), String>> + Send;
}

impl NodeControl for SupervisorHandle {
    fn watch_status(&self) -> watch::Receiver<Status> {
        SupervisorHandle::watch_status(self)
    }

    async fn stop(&self) -> Result<(), String> {
        SupervisorHandle::stop(self)
            .await
            .map_err(|err| err.to_string())
    }

    async fn start(&self) -> Result<(), String> {
        SupervisorHandle::start(self)
            .await
            .map_err(|err| err.to_string())
    }
}

/// Whether zebrad counts as running for the update flow: it must be brought
/// back after the install.
pub fn node_is_running(status: &Status) -> bool {
    matches!(
        status.phase,
        Phase::Starting | Phase::Running | Phase::Backoff { .. }
    )
}

// ---------------------------------------------------------------------------
// Health gate
// ---------------------------------------------------------------------------

fn health_failure(
    pending: &PendingUpdate,
    current_version: &str,
    releases_url: &str,
    message: String,
    recovery: Vec<RecoveryAction>,
) -> UpdateHealthFailure {
    UpdateHealthFailure {
        expected_version: pending.to_version.clone(),
        running_version: current_version.to_string(),
        message,
        recovery,
        releases_url: releases_url.to_string(),
    }
}

/// What to offer when zebrad is unhealthy on the new version: its
/// configuration may no longer suit it.
fn unhealthy_recovery() -> Vec<RecoveryAction> {
    vec![
        RecoveryAction::OpenLogs,
        RecoveryAction::ResetConfig,
        RecoveryAction::OpenReleasesPage,
    ]
}

/// Checks that the launcher came back healthy after the update in `pending`:
/// it runs the new version and, if zebrad was running before, zebrad reaches
/// `Running` with an RPC address within `timeout`.
///
/// A zebrad left by an earlier launcher run (`Status::stale`) is not a
/// failure of the update: the gate waits for the user to resolve it, and the
/// time limit restarts once it is gone. If it is still there at the limit,
/// the failure names it and offers only the logs.
///
/// A launcher still on the old version means the install did not complete
/// (the marker is written right before it): that is reported with the logs
/// and the releases page only, since zebrad's configuration is not the cause.
pub async fn health_gate(
    pending: &PendingUpdate,
    current_version: &str,
    mut status: watch::Receiver<Status>,
    timeout: Duration,
    releases_url: &str,
) -> Result<(), UpdateHealthFailure> {
    let fail = |message: String| {
        health_failure(
            pending,
            current_version,
            releases_url,
            message,
            unhealthy_recovery(),
        )
    };
    if !versions_match(&pending.to_version, current_version) {
        return Err(health_failure(
            pending,
            current_version,
            releases_url,
            format!(
                "The update to {} did not complete; Zebra Launcher {current_version} is still running.",
                pending.to_version
            ),
            vec![RecoveryAction::OpenLogs, RecoveryAction::OpenReleasesPage],
        ));
    }
    if !pending.zebrad_was_running {
        return Ok(());
    }
    let mut deadline = Instant::now() + timeout;
    let mut stale: Option<StaleProcessInfo> = None;
    loop {
        {
            let status = status.borrow_and_update();
            match &status.phase {
                Phase::Running if status.rpc_addr.is_some() => return Ok(()),
                // Not a configuration problem: only a newer release helps.
                Phase::Failed {
                    class: ExitClass::EosHalt,
                } => {
                    return Err(health_failure(
                        pending,
                        current_version,
                        releases_url,
                        "zebrad is past its end of support; update required.".to_string(),
                        vec![RecoveryAction::OpenReleasesPage],
                    ));
                }
                Phase::Failed { class } => {
                    return Err(fail(failed_message(*class, status.last_exit.as_ref())));
                }
                _ => {}
            }
            // A zebrad from an earlier launcher run blocks the start. That is
            // not the update's fault: the user was told (`stale_process`) and
            // decides. Once it is resolved, zebrad gets the full time again.
            if stale.is_some() && status.stale.is_none() {
                deadline = Instant::now() + timeout;
            }
            stale = status.stale.clone();
        }
        tokio::select! {
            changed = status.changed() => if changed.is_err() {
                return Err(fail(
                    "The zebrad supervisor stopped before zebrad was running.".to_string(),
                ));
            },
            () = tokio::time::sleep_until(deadline) => {
                return Err(match &stale {
                    Some(stale) => health_failure(
                        pending,
                        current_version,
                        releases_url,
                        format!(
                            "zebrad could not start after the update: a zebrad from an earlier \
                             launcher run is still running (pid {}, {}). Stop it, or start zebrad \
                             and let the launcher stop it.",
                            stale.pid, stale.binary
                        ),
                        vec![RecoveryAction::OpenLogs],
                    ),
                    None => fail(format!(
                        "zebrad did not reach Running with RPC within {} minutes after the update.",
                        timeout.as_secs() / 60
                    )),
                });
            }
        }
    }
}

fn versions_match(a: &str, b: &str) -> bool {
    a.trim_start_matches('v') == b.trim_start_matches('v')
}

fn failed_message(class: ExitClass, exit: Option<&launcher_types::ExitInfo>) -> String {
    let detail = exit
        .and_then(|exit| exit.detail.as_deref())
        .map(|detail| format!(": {detail}"))
        .unwrap_or_default();
    format!("zebrad stopped after the update ({class:?}){detail}.")
}

/// Runs the health gate if the marker at `marker` exists, then removes it.
///
/// When zebrad was running before the update and `start_node` is set (the
/// launcher will not start it by itself), zebrad is started first, also when
/// the update did not complete, since the update stopped it. Returns `None`
/// when there was no marker.
pub async fn run_health_gate<N: NodeControl>(
    marker: &Path,
    current_version: &str,
    node: &N,
    start_node: bool,
    timeout: Duration,
    releases_url: &str,
) -> Option<Result<(), UpdateHealthFailure>> {
    let pending = match read_marker(marker) {
        Ok(Some(pending)) => pending,
        Ok(None) => return None,
        Err(err) => {
            tracing::warn!(%err, "ignoring an unreadable pending-update marker");
            let _ = clear_marker(marker);
            return None;
        }
    };
    tracing::info!(
        from = %pending.from_version,
        to = %pending.to_version,
        "checking the launcher's health after an update"
    );
    if start_node && pending.zebrad_was_running {
        // The gate below reports a failure to reach Running.
        let node = node.clone();
        tokio::spawn(async move {
            if let Err(err) = node.start().await {
                tracing::warn!(%err, "zebrad did not start after the update");
            }
        });
    }
    let result = health_gate(
        &pending,
        current_version,
        node.watch_status(),
        timeout,
        releases_url,
    )
    .await;
    if let Err(err) = clear_marker(marker) {
        tracing::warn!(%err, "could not remove the pending-update marker");
    }
    match &result {
        Ok(()) => tracing::info!(version = %current_version, "the update came back healthy"),
        Err(failure) => {
            tracing::error!(message = %failure.message, "the update did not come back healthy")
        }
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use launcher_types::ExitInfo;

    use super::*;

    const T: Duration = Duration::from_secs(10);

    fn eos_in(days: u64, now: SystemTime) -> EosInfo {
        EosInfo {
            block_height: 3_000_000,
            estimated_time: now + DAY * days as u32,
        }
    }

    #[test]
    fn off_mode_never_schedules_a_check() {
        let launched = Instant::now();
        let mut jitter = Jitter::new(1);
        assert_eq!(
            next_check(UpdateMode::Off, launched, None, &mut jitter),
            None
        );
        assert_eq!(
            next_check(UpdateMode::Off, launched, Some(launched), &mut jitter),
            None
        );
    }

    #[test]
    fn first_check_at_launch_plus_30s_then_every_6h_with_jitter() {
        let launched = Instant::now();
        let mut jitter = Jitter::new(7);
        for mode in [UpdateMode::Notify, UpdateMode::Auto] {
            assert_eq!(
                next_check(mode, launched, None, &mut jitter),
                Some(launched + FIRST_CHECK_DELAY)
            );
        }
        let low = CHECK_INTERVAL.mul_f64(0.9);
        let high = CHECK_INTERVAL.mul_f64(1.1);
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1000 {
            let at = next_check(UpdateMode::Notify, launched, Some(launched), &mut jitter)
                .expect("scheduled");
            let delay = at - launched;
            assert!(delay >= low && delay <= high, "{delay:?}");
            seen.insert(delay);
        }
        assert!(
            seen.len() > 900,
            "jitter should vary: {} values",
            seen.len()
        );
    }

    #[test]
    fn jitter_is_deterministic_per_seed() {
        let a: Vec<_> = (0..5)
            .scan(Jitter::new(42), |j, _| Some(j.apply(CHECK_INTERVAL)))
            .collect();
        let b: Vec<_> = (0..5)
            .scan(Jitter::new(42), |j, _| Some(j.apply(CHECK_INTERVAL)))
            .collect();
        let c: Vec<_> = (0..5)
            .scan(Jitter::new(43), |j, _| Some(j.apply(CHECK_INTERVAL)))
            .collect();
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn eos_warning_window() {
        let now = SystemTime::now();
        assert!(!eos_warning_active(None, false, now));
        assert!(eos_warning_active(None, true, now));
        assert!(!eos_warning_active(Some(&eos_in(15, now)), false, now));
        assert!(eos_warning_active(Some(&eos_in(14, now)), false, now));
        assert!(eos_warning_active(Some(&eos_in(1, now)), false, now));
        let past = EosInfo {
            block_height: 1,
            estimated_time: now - DAY,
        };
        assert!(eos_warning_active(Some(&past), false, now));
    }

    #[test]
    fn stale_check_warning_only_near_eos() {
        let now = SystemTime::now();
        let recent = Some(now - DAY);
        let old = Some(now - DAY * 15);
        let near = eos_in(20, now);
        let far = eos_in(40, now);
        // Far from EOS: never.
        assert!(!stale_check_warning(None, Some(&far), false, now));
        assert!(!stale_check_warning(old, None, false, now));
        // Near EOS: only when the last success is old or missing.
        assert!(stale_check_warning(None, Some(&near), false, now));
        assert!(stale_check_warning(old, Some(&near), false, now));
        assert!(!stale_check_warning(recent, Some(&near), false, now));
        // Halted counts as near.
        assert!(stale_check_warning(old, None, true, now));
    }

    #[test]
    fn auto_install_needs_auto_mode_eos_and_no_dismissal() {
        use UpdateMode::*;
        assert!(should_auto_install(Auto, true, true, false));
        assert!(!should_auto_install(Auto, true, false, false));
        assert!(!should_auto_install(Auto, true, true, true));
        assert!(!should_auto_install(Auto, false, true, false));
        assert!(!should_auto_install(Notify, true, true, false));
        assert!(!should_auto_install(Off, true, true, false));
        assert!(should_auto_download(Auto, true));
        assert!(!should_auto_download(Auto, false));
        assert!(!should_auto_download(Notify, true));
        assert!(!should_auto_download(Off, true));
    }

    #[test]
    fn record_and_marker_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::under(dir.path());
        paths.ensure_dirs().expect("dirs");

        let record_file = record_path(&paths);
        assert_eq!(load_record(&record_file), UpdaterRecord::default());
        let record = UpdaterRecord {
            last_successful_check: Some(
                SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000),
            ),
        };
        save_record(&record_file, &record).expect("save");
        assert_eq!(load_record(&record_file), record);
        fs::write(&record_file, b"{not json").expect("corrupt");
        assert_eq!(load_record(&record_file), UpdaterRecord::default());

        let marker = marker_path(&paths);
        assert!(marker.ends_with(PENDING_UPDATE_MARKER));
        assert_eq!(read_marker(&marker).expect("read"), None);
        let pending = pending("0.2.0", true);
        write_marker(&marker, &pending).expect("write");
        assert_eq!(read_marker(&marker).expect("read"), Some(pending));
        clear_marker(&marker).expect("clear");
        clear_marker(&marker).expect("clear again");
        assert_eq!(read_marker(&marker).expect("read"), None);
    }

    fn pending(to: &str, was_running: bool) -> PendingUpdate {
        PendingUpdate {
            from_version: "0.1.0".into(),
            to_version: to.into(),
            zebrad_was_running: was_running,
            written_at: SystemTime::UNIX_EPOCH,
        }
    }

    fn running() -> Status {
        Status {
            phase: Phase::Running,
            rpc_addr: Some(SocketAddr::from(([127, 0, 0, 1], 8232))),
            ..Status::default()
        }
    }

    #[tokio::test(start_paused = true)]
    async fn gate_passes_when_zebrad_reaches_running_with_rpc() {
        let (tx, rx) = watch::channel(Status {
            phase: Phase::Starting,
            ..Status::default()
        });
        let gate = tokio::spawn(async move {
            health_gate(
                &pending("0.2.0", true),
                "0.2.0",
                rx,
                HEALTH_GATE_TIMEOUT,
                "u",
            )
            .await
        });
        tokio::time::sleep(Duration::from_secs(60)).await;
        // Running without RPC is not enough.
        tx.send_replace(Status {
            phase: Phase::Running,
            ..Status::default()
        });
        tokio::time::sleep(Duration::from_secs(60)).await;
        tx.send_replace(running());
        let result = tokio::time::timeout(T, gate)
            .await
            .expect("in time")
            .expect("join");
        assert_eq!(result, Ok(()));
    }

    #[tokio::test(start_paused = true)]
    async fn gate_fails_after_five_minutes() {
        let (_tx, rx) = watch::channel(Status::default());
        let result = tokio::time::timeout(
            HEALTH_GATE_TIMEOUT + T,
            health_gate(
                &pending("0.2.0", true),
                "0.2.0",
                rx,
                HEALTH_GATE_TIMEOUT,
                "https://r",
            ),
        )
        .await
        .expect("in time");
        let failure = result.expect_err("times out");
        assert_eq!(failure.expected_version, "0.2.0");
        assert_eq!(failure.releases_url, "https://r");
        assert_eq!(
            failure.recovery,
            vec![
                RecoveryAction::OpenLogs,
                RecoveryAction::ResetConfig,
                RecoveryAction::OpenReleasesPage
            ]
        );
        assert!(failure.message.contains("5 minutes"), "{}", failure.message);
    }

    fn stale_info() -> StaleProcessInfo {
        StaleProcessInfo {
            pid: 4242,
            binary: "/old/zebrad".into(),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn gate_waits_for_a_stale_process_to_be_resolved() {
        let (tx, rx) = watch::channel(Status {
            stale: Some(stale_info()),
            ..Status::default()
        });
        let gate = tokio::spawn(async move {
            health_gate(
                &pending("0.2.0", true),
                "0.2.0",
                rx,
                HEALTH_GATE_TIMEOUT,
                "u",
            )
            .await
        });
        // The user resolves it near the end of the time limit...
        tokio::time::sleep(HEALTH_GATE_TIMEOUT - Duration::from_secs(10)).await;
        tx.send_replace(Status {
            phase: Phase::Starting,
            ..Status::default()
        });
        // ...and zebrad still gets the full time to come up.
        tokio::time::sleep(Duration::from_secs(60)).await;
        assert!(!gate.is_finished());
        tx.send_replace(running());
        let result = tokio::time::timeout(T, gate)
            .await
            .expect("in time")
            .expect("join");
        assert_eq!(result, Ok(()));
    }

    #[tokio::test(start_paused = true)]
    async fn gate_names_an_unresolved_stale_process_without_reset_config() {
        let (_tx, rx) = watch::channel(Status {
            stale: Some(stale_info()),
            ..Status::default()
        });
        let failure = tokio::time::timeout(
            HEALTH_GATE_TIMEOUT + T,
            health_gate(
                &pending("0.2.0", true),
                "0.2.0",
                rx,
                HEALTH_GATE_TIMEOUT,
                "u",
            ),
        )
        .await
        .expect("in time")
        .expect_err("still stale");
        assert!(failure.message.contains("4242"), "{}", failure.message);
        assert!(
            failure.message.contains("/old/zebrad"),
            "{}",
            failure.message
        );
        assert_eq!(failure.recovery, vec![RecoveryAction::OpenLogs]);
    }

    #[tokio::test]
    async fn gate_reports_an_eos_halt_without_reset_config() {
        let (_tx, rx) = watch::channel(Status {
            phase: Phase::Failed {
                class: ExitClass::EosHalt,
            },
            ..Status::default()
        });
        let failure = tokio::time::timeout(
            T,
            health_gate(
                &pending("0.2.0", true),
                "0.2.0",
                rx,
                HEALTH_GATE_TIMEOUT,
                "u",
            ),
        )
        .await
        .expect("in time")
        .expect_err("halted");
        assert!(
            failure.message.contains("end of support"),
            "{}",
            failure.message
        );
        assert_eq!(failure.recovery, vec![RecoveryAction::OpenReleasesPage]);
    }

    #[tokio::test]
    async fn gate_fails_fast_on_a_failed_zebrad_or_wrong_version() {
        let (_tx, rx) = watch::channel(Status {
            phase: Phase::Failed {
                class: ExitClass::ConfigError,
            },
            last_exit: Some(ExitInfo {
                code: Some(1),
                class: ExitClass::ConfigError,
                detail: Some("unknown field".into()),
                at: SystemTime::UNIX_EPOCH,
                requested: false,
            }),
            ..Status::default()
        });
        let failure = tokio::time::timeout(
            T,
            health_gate(
                &pending("0.2.0", true),
                "0.2.0",
                rx.clone(),
                HEALTH_GATE_TIMEOUT,
                "u",
            ),
        )
        .await
        .expect("in time")
        .expect_err("failed");
        assert!(
            failure.message.contains("unknown field"),
            "{}",
            failure.message
        );
        // The new version runs but zebrad fails: its configuration may be why.
        assert!(failure.recovery.contains(&RecoveryAction::ResetConfig));

        let failure = tokio::time::timeout(
            T,
            health_gate(
                &pending("0.2.0", false),
                "0.1.0",
                rx.clone(),
                HEALTH_GATE_TIMEOUT,
                "u",
            ),
        )
        .await
        .expect("in time")
        .expect_err("wrong version");
        assert_eq!(failure.running_version, "0.1.0");
        // The install did not complete: zebrad's configuration is not to blame.
        assert!(
            failure.message.contains("did not complete"),
            "{}",
            failure.message
        );
        assert_eq!(
            failure.recovery,
            vec![RecoveryAction::OpenLogs, RecoveryAction::OpenReleasesPage]
        );

        // zebrad was not running before: only the version matters.
        let ok = tokio::time::timeout(
            T,
            health_gate(
                &pending("v0.2.0", false),
                "0.2.0",
                rx,
                HEALTH_GATE_TIMEOUT,
                "u",
            ),
        )
        .await
        .expect("in time");
        assert_eq!(ok, Ok(()));
    }

    /// A node whose `start` makes it Running with RPC.
    #[derive(Clone)]
    struct InstantNode {
        status: Arc<watch::Sender<Status>>,
        starts: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl NodeControl for InstantNode {
        fn watch_status(&self) -> watch::Receiver<Status> {
            self.status.subscribe()
        }
        async fn stop(&self) -> Result<(), String> {
            self.status.send_replace(Status::default());
            Ok(())
        }
        async fn start(&self) -> Result<(), String> {
            self.starts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.status.send_replace(running());
            Ok(())
        }
    }

    #[tokio::test]
    async fn run_health_gate_starts_zebrad_when_asked_and_clears_the_marker() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join(PENDING_UPDATE_MARKER);
        let node = InstantNode {
            status: Arc::new(watch::channel(Status::default()).0),
            starts: Arc::default(),
        };

        assert_eq!(
            tokio::time::timeout(
                T,
                run_health_gate(&marker, "0.2.0", &node, true, HEALTH_GATE_TIMEOUT, "u")
            )
            .await
            .expect("in time"),
            None
        );

        write_marker(&marker, &pending("0.2.0", true)).expect("write");
        let result = tokio::time::timeout(
            T,
            run_health_gate(&marker, "0.2.0", &node, true, HEALTH_GATE_TIMEOUT, "u"),
        )
        .await
        .expect("in time");
        assert_eq!(result, Some(Ok(())));
        assert_eq!(node.starts.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(!marker.exists());

        // The install did not complete (still 0.2.0): zebrad, which the
        // update stopped, is started again anyway.
        write_marker(&marker, &pending("0.3.0", true)).expect("write");
        let result = tokio::time::timeout(
            T,
            run_health_gate(&marker, "0.2.0", &node, true, HEALTH_GATE_TIMEOUT, "u"),
        )
        .await
        .expect("in time");
        assert!(matches!(result, Some(Err(_))), "{result:?}");
        tokio::time::timeout(T, async {
            while node.starts.load(std::sync::atomic::Ordering::SeqCst) < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("zebrad started again");
        assert!(!marker.exists());

        fs::write(&marker, b"garbage").expect("corrupt marker");
        let result = tokio::time::timeout(
            T,
            run_health_gate(&marker, "0.2.0", &node, true, HEALTH_GATE_TIMEOUT, "u"),
        )
        .await
        .expect("in time");
        assert_eq!(result, None);
        assert!(!marker.exists());
    }
}
