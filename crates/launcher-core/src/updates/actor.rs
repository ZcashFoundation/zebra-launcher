//! The updater actor: one task owns the update flow and publishes
//! [`UpdateInfo`] on a `watch` channel. Network work, downloads and the
//! install run in child tasks that report back over an `mpsc` channel, so the
//! actor stays responsive and can abort them; results from an aborted task
//! are recognised by their job id and dropped.

use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime},
};

use launcher_types::{
    EosInfo, ExitClass, NodeStats, Phase, Settings, Status, UpdateHealthFailure, UpdateInfo,
    UpdateMode, UpdateState,
};
use tokio::{
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
    time::Instant,
};

use super::{
    FoundUpdate, Jitter, NodeControl, PendingUpdate, UpdateBackend, UpdaterRecord, clear_marker,
    eos_warning_active, next_check, save_record, should_auto_download, should_auto_install,
    stale_check_warning, write_marker,
};

/// Download progress is published at most this often.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);

/// How the updater runs.
#[derive(Clone, Debug)]
pub struct UpdaterConfig {
    /// The running launcher version.
    pub current_version: String,
    /// The releases page, for the UI.
    pub releases_url: String,
    /// Where to remember the last successful check, if anywhere.
    pub record_file: Option<PathBuf>,
    /// What was remembered at launch.
    pub record: UpdaterRecord,
    /// Where to write the pending-update marker before installing, if anywhere.
    pub marker_file: Option<PathBuf>,
    /// Seed for the check jitter.
    pub seed: u64,
    /// How long an automatic install counts down.
    pub countdown: Duration,
}

/// Why an updater command failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum UpdaterError {
    /// There is no update to install.
    #[error("no update is available; check for updates first")]
    NoUpdate,
    /// This installation cannot replace itself (the `.deb` package).
    #[error(
        "this installation cannot update itself; download the new version from the releases page"
    )]
    NotInstallable,
    /// The updater task has stopped.
    #[error("the updater has stopped")]
    Closed,
}

enum Cmd {
    Check {
        reply: oneshot::Sender<UpdateState>,
    },
    Install {
        reply: oneshot::Sender<Result<(), UpdaterError>>,
    },
    Cancel {
        reply: oneshot::Sender<()>,
    },
    HealthFailed(UpdateHealthFailure),
}

/// A cloneable handle to the updater actor.
#[derive(Clone, Debug)]
pub struct UpdaterHandle {
    cmd: mpsc::Sender<Cmd>,
    info: watch::Receiver<UpdateInfo>,
}

impl UpdaterHandle {
    /// Everything the UI shows about updates.
    pub fn info(&self) -> UpdateInfo {
        self.info.borrow().clone()
    }

    /// What the updater is doing.
    pub fn state(&self) -> UpdateState {
        self.info.borrow().state.clone()
    }

    /// A receiver that wakes on every change.
    pub fn watch(&self) -> watch::Receiver<UpdateInfo> {
        self.info.clone()
    }

    /// Checks for an update now, in any mode, and returns the resulting
    /// state. While a download or install is under way, returns the current
    /// state without checking.
    pub async fn check(&self) -> Result<UpdateState, UpdaterError> {
        let (reply, response) = oneshot::channel();
        self.send(Cmd::Check { reply }).await?;
        response.await.map_err(|_| UpdaterError::Closed)
    }

    /// Starts installing the found update: downloads it if needed, verifies
    /// it, stops zebrad, installs and restarts the launcher. Returns once the
    /// flow has started; progress shows in the state.
    pub async fn install(&self) -> Result<(), UpdaterError> {
        let (reply, response) = oneshot::channel();
        self.send(Cmd::Install { reply }).await?;
        response.await.map_err(|_| UpdaterError::Closed)?
    }

    /// Cancels a check, a download or an automatic-install countdown. An
    /// install that has started cannot be cancelled.
    pub async fn cancel(&self) -> Result<(), UpdaterError> {
        let (reply, response) = oneshot::channel();
        self.send(Cmd::Cancel { reply }).await?;
        response.await.map_err(|_| UpdaterError::Closed)
    }

    /// Records that this launch followed an update that did not come back
    /// healthy, so [`UpdateInfo::health_failure`] shows it.
    pub async fn report_health_failure(
        &self,
        failure: UpdateHealthFailure,
    ) -> Result<(), UpdaterError> {
        self.send(Cmd::HealthFailed(failure)).await
    }

    async fn send(&self, cmd: Cmd) -> Result<(), UpdaterError> {
        self.cmd.send(cmd).await.map_err(|_| UpdaterError::Closed)
    }
}

/// Starts the updater actor. Must be called inside a Tokio runtime.
///
/// `settings` provides the update mode, `stats` the end-of-support estimate,
/// and `node` the stop cap besides control of zebrad. Nothing touches the
/// network until an automatic check is due (never in Off mode) or
/// [`UpdaterHandle::check`] is called.
pub fn spawn<B: UpdateBackend, N: NodeControl>(
    backend: B,
    node: N,
    settings: watch::Receiver<Settings>,
    stats: watch::Receiver<Option<NodeStats>>,
    config: UpdaterConfig,
) -> UpdaterHandle {
    let (cmd_tx, cmd_rx) = mpsc::channel(16);
    let (events_tx, events_rx) = mpsc::unbounded_channel();
    let can_install = backend.can_install();
    let mode = settings.borrow().update_mode;
    let status = node.watch_status();
    let mut actor = Actor {
        backend: Arc::new(backend),
        node,
        can_install,
        cmd_rx,
        cmd_closed: false,
        events_tx,
        events_rx,
        settings,
        settings_open: true,
        stats,
        stats_open: true,
        status,
        status_open: true,
        info: watch::channel(UpdateInfo {
            state: UpdateState::Idle,
            mode,
            current_version: config.current_version.clone(),
            last_successful_check: config.record.last_successful_check,
            stale_check_warning: false,
            can_install,
            releases_url: config.releases_url.clone(),
            health_failure: None,
        })
        .0,
        mode,
        launched: Instant::now(),
        last_attempt: None,
        next_check: None,
        jitter: Jitter::new(config.seed),
        last_success: config.record.last_successful_check,
        eos: None,
        eos_halted: false,
        state: UpdateState::Idle,
        before_check: None,
        found: None,
        bytes: None,
        install_after_download: false,
        dismissed: None,
        countdown_until: None,
        job: None,
        next_job_id: 0,
        installed: false,
        check_waiters: Vec::new(),
        health_failure: None,
        config,
    };
    actor.reschedule();
    actor.on_stats();
    actor.on_status();
    actor.publish();
    let info = actor.info.subscribe();
    tokio::spawn(actor.run());
    UpdaterHandle { cmd: cmd_tx, info }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JobKind {
    Check,
    Download,
    Install,
}

struct Job {
    id: u64,
    kind: JobKind,
    handle: JoinHandle<()>,
}

enum Event<R> {
    Checked {
        job: u64,
        result: Result<Option<FoundUpdate<R>>, String>,
    },
    Progress {
        job: u64,
        downloaded: u64,
        total: Option<u64>,
    },
    Downloaded {
        job: u64,
        result: Result<Vec<u8>, String>,
    },
    InstallFailed {
        job: u64,
        message: String,
    },
    Installed {
        job: u64,
    },
}

impl<R> Event<R> {
    fn job(&self) -> u64 {
        match self {
            Event::Checked { job, .. }
            | Event::Progress { job, .. }
            | Event::Downloaded { job, .. }
            | Event::InstallFailed { job, .. }
            | Event::Installed { job } => *job,
        }
    }
}

struct Actor<B: UpdateBackend, N: NodeControl> {
    backend: Arc<B>,
    node: N,
    config: UpdaterConfig,
    can_install: bool,

    cmd_rx: mpsc::Receiver<Cmd>,
    cmd_closed: bool,
    events_tx: mpsc::UnboundedSender<Event<B::Release>>,
    events_rx: mpsc::UnboundedReceiver<Event<B::Release>>,
    settings: watch::Receiver<Settings>,
    settings_open: bool,
    stats: watch::Receiver<Option<NodeStats>>,
    stats_open: bool,
    status: watch::Receiver<Status>,
    status_open: bool,
    info: watch::Sender<UpdateInfo>,

    mode: UpdateMode,
    launched: Instant,
    last_attempt: Option<Instant>,
    next_check: Option<Instant>,
    jitter: Jitter,
    last_success: Option<SystemTime>,
    /// The last end-of-support estimate seen; kept while zebrad is stopped.
    eos: Option<EosInfo>,
    /// zebrad refused to run because it is past its end of support.
    eos_halted: bool,

    state: UpdateState,
    /// The state before the running check, restored if it is cancelled.
    before_check: Option<UpdateState>,
    found: Option<FoundUpdate<B::Release>>,
    /// The verified package for `found`.
    bytes: Option<Arc<Vec<u8>>>,
    install_after_download: bool,
    /// The version whose automatic-install countdown the user cancelled.
    dismissed: Option<String>,
    countdown_until: Option<Instant>,
    job: Option<Job>,
    next_job_id: u64,
    /// The install succeeded and the launcher is restarting.
    installed: bool,
    check_waiters: Vec<oneshot::Sender<UpdateState>>,
    health_failure: Option<UpdateHealthFailure>,
}

impl<B: UpdateBackend, N: NodeControl> Actor<B, N> {
    async fn run(mut self) {
        loop {
            let wake = self.next_wakeup();
            tokio::select! {
                Some(event) = self.events_rx.recv() => self.on_event(event),
                cmd = self.cmd_rx.recv(), if !self.cmd_closed => match cmd {
                    Some(cmd) => self.on_cmd(cmd),
                    None => self.cmd_closed = true,
                },
                changed = self.settings.changed(), if self.settings_open => match changed {
                    Ok(()) => self.on_settings(),
                    Err(_) => self.settings_open = false,
                },
                changed = self.stats.changed(), if self.stats_open => match changed {
                    Ok(()) => self.on_stats(),
                    Err(_) => self.stats_open = false,
                },
                changed = self.status.changed(), if self.status_open => match changed {
                    Ok(()) => self.on_status(),
                    Err(_) => self.status_open = false,
                },
                () = sleep_until_or_never(wake) => self.on_timer(),
            }
            if self.cmd_closed && self.job.is_none() {
                // Nobody can ask for anything any more, and nothing is running.
                return;
            }
            self.publish();
        }
    }

    // --- Inputs ---------------------------------------------------------------

    fn on_cmd(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Check { reply } => self.on_check(reply),
            Cmd::Install { reply } => {
                let _ = reply.send(self.on_install());
            }
            Cmd::Cancel { reply } => {
                self.on_cancel();
                let _ = reply.send(());
            }
            Cmd::HealthFailed(failure) => self.health_failure = Some(failure),
        }
    }

    fn on_check(&mut self, reply: oneshot::Sender<UpdateState>) {
        match self.job_kind() {
            Some(JobKind::Check) => self.check_waiters.push(reply),
            Some(JobKind::Download | JobKind::Install) => {
                let _ = reply.send(self.current_state());
            }
            None if self.installed || self.countdown_until.is_some() => {
                let _ = reply.send(self.current_state());
            }
            None => {
                self.check_waiters.push(reply);
                self.start_check();
            }
        }
    }

    fn on_install(&mut self) -> Result<(), UpdaterError> {
        if self.installed || self.job_kind() == Some(JobKind::Install) {
            return Ok(());
        }
        if !self.can_install {
            return Err(UpdaterError::NotInstallable);
        }
        if self.found.is_none() {
            return Err(UpdaterError::NoUpdate);
        }
        match self.job_kind() {
            Some(JobKind::Download) => self.install_after_download = true,
            Some(JobKind::Check) | None => {
                if self.job.is_some() {
                    self.abort_job();
                    self.last_attempt = Some(Instant::now());
                    self.reschedule();
                }
                if self.bytes.is_some() {
                    self.start_install();
                } else {
                    self.install_after_download = true;
                    self.start_download();
                }
            }
            Some(JobKind::Install) => {}
        }
        self.reply_waiters();
        Ok(())
    }

    fn on_cancel(&mut self) {
        match self.job_kind() {
            Some(JobKind::Check) => {
                self.abort_job();
                self.last_attempt = Some(Instant::now());
                self.reschedule();
                // Back to what was shown before the check (up to date, or
                // the last error), not to Idle.
                let before = self.before_check.take();
                self.state = self.resting_state().or(before).unwrap_or_default();
                self.reply_waiters();
            }
            Some(JobKind::Download) => {
                self.abort_job();
                self.install_after_download = false;
                self.bytes = None;
                self.state = self.resting_state().unwrap_or_default();
            }
            Some(JobKind::Install) => {
                tracing::info!("an update install cannot be cancelled once it started");
            }
            None => {
                if self.countdown_until.take().is_some() {
                    self.dismissed = self.found.as_ref().map(|found| found.version.clone());
                    tracing::info!(version = ?self.dismissed, "automatic update install cancelled");
                    self.state = self.resting_state().unwrap_or_default();
                }
            }
        }
    }

    fn on_settings(&mut self) {
        let mode = self.settings.borrow_and_update().update_mode;
        if mode == self.mode {
            return;
        }
        tracing::info!(?mode, "update mode changed");
        self.mode = mode;
        match mode {
            UpdateMode::Off => {
                if self.installed || self.job_kind() == Some(JobKind::Install) {
                    // Too late to stop: zebrad is already being stopped.
                    return;
                }
                self.abort_job();
                self.found = None;
                self.bytes = None;
                self.install_after_download = false;
                self.countdown_until = None;
                self.dismissed = None;
                self.next_check = None;
                self.state = UpdateState::Idle;
                self.reply_waiters();
            }
            UpdateMode::Notify | UpdateMode::Auto => {
                if self.job_kind() != Some(JobKind::Check) {
                    self.reschedule();
                }
                if mode != UpdateMode::Auto && self.countdown_until.take().is_some() {
                    self.state = self.resting_state().unwrap_or_default();
                }
                if should_auto_download(mode, self.can_install)
                    && self.found.is_some()
                    && self.bytes.is_none()
                    && self.job.is_none()
                    && !self.installed
                {
                    self.start_download();
                }
                self.maybe_start_countdown();
            }
        }
    }

    fn on_stats(&mut self) {
        let stats = self.stats.borrow_and_update().clone();
        if let Some(stats) = stats {
            if let Some(eos) = stats.eos {
                self.eos = Some(eos);
            } else if stats.chain != "main" {
                // Testnet has no end of support.
                self.eos = None;
            }
        }
        self.maybe_start_countdown();
    }

    fn on_status(&mut self) {
        let halted = matches!(
            self.status.borrow_and_update().phase,
            Phase::Failed {
                class: ExitClass::EosHalt
            }
        );
        // Sticky for this launch: a halted zebrad never reports its estimate.
        self.eos_halted |= halted;
        self.maybe_start_countdown();
    }

    fn on_timer(&mut self) {
        let now = Instant::now();
        if self.countdown_until.is_some_and(|until| now >= until) {
            self.countdown_until = None;
            tracing::info!("starting the automatic update install");
            self.start_install();
        }
        if self.next_check.is_some_and(|at| now >= at) {
            if self.mode == UpdateMode::Off {
                self.next_check = None;
            } else if self.job.is_some() || self.installed || self.countdown_until.is_some() {
                // Busy: skip this round.
                self.last_attempt = Some(now);
                self.reschedule();
            } else {
                self.start_check();
            }
        }
    }

    fn on_event(&mut self, event: Event<B::Release>) {
        if self.job.as_ref().is_none_or(|job| job.id != event.job()) {
            // From a task that was aborted or replaced.
            return;
        }
        match event {
            Event::Checked { result, .. } => self.on_checked(result),
            Event::Progress {
                downloaded, total, ..
            } => {
                if let UpdateState::Downloading {
                    downloaded: d,
                    total: t,
                    ..
                } = &mut self.state
                {
                    *d = downloaded;
                    *t = total;
                }
            }
            Event::Downloaded { result, .. } => self.on_downloaded(result),
            Event::InstallFailed { message, .. } => {
                self.job = None;
                tracing::error!(%message, "the update was not installed");
                // Do not count down again for this version; the user can retry.
                self.dismissed = self.found.as_ref().map(|found| found.version.clone());
                self.bytes = None;
                self.state = UpdateState::Error {
                    message,
                    at: SystemTime::now(),
                };
            }
            Event::Installed { .. } => {
                self.job = None;
                self.installed = true;
            }
        }
    }

    fn on_checked(&mut self, result: Result<Option<FoundUpdate<B::Release>>, String>) {
        self.job = None;
        self.last_attempt = Some(Instant::now());
        self.reschedule();
        match result {
            Ok(found) => {
                let now = SystemTime::now();
                self.last_success = Some(now);
                self.save_record();
                match found {
                    None => {
                        tracing::info!("the launcher is up to date");
                        self.found = None;
                        self.bytes = None;
                        self.install_after_download = false;
                        self.state = UpdateState::UpToDate { checked_at: now };
                    }
                    Some(found) => {
                        tracing::info!(version = %found.version, "a launcher update is available");
                        let same = self
                            .found
                            .as_ref()
                            .is_some_and(|old| old.version == found.version);
                        if !same {
                            self.bytes = None;
                            self.found = Some(found);
                        }
                        self.state = self.resting_state().unwrap_or_default();
                        if should_auto_download(self.mode, self.can_install) && self.bytes.is_none()
                        {
                            self.start_download();
                        }
                    }
                }
            }
            Err(message) => {
                tracing::warn!(%message, "the update check failed");
                self.state = self.resting_state().unwrap_or_else(|| UpdateState::Error {
                    message,
                    at: SystemTime::now(),
                });
            }
        }
        self.reply_waiters();
        self.maybe_start_countdown();
    }

    fn on_downloaded(&mut self, result: Result<Vec<u8>, String>) {
        self.job = None;
        match result {
            Ok(bytes) => {
                self.bytes = Some(Arc::new(bytes));
                self.state = self.resting_state().unwrap_or_default();
                if std::mem::take(&mut self.install_after_download) {
                    self.start_install();
                } else {
                    self.maybe_start_countdown();
                }
            }
            Err(message) => {
                tracing::warn!(%message, "the update download failed");
                self.install_after_download = false;
                self.state = UpdateState::Error {
                    message,
                    at: SystemTime::now(),
                };
            }
        }
    }

    // --- Jobs -----------------------------------------------------------------

    fn job_kind(&self) -> Option<JobKind> {
        self.job.as_ref().map(|job| job.kind)
    }

    fn abort_job(&mut self) {
        if let Some(job) = self.job.take() {
            job.handle.abort();
        }
    }

    fn new_job_id(&mut self) -> u64 {
        self.next_job_id += 1;
        self.next_job_id
    }

    fn start_check(&mut self) {
        let id = self.new_job_id();
        let backend = self.backend.clone();
        let events = self.events_tx.clone();
        let handle = tokio::spawn(async move {
            let result = backend.check().await;
            let _ = events.send(Event::Checked { job: id, result });
        });
        self.job = Some(Job {
            id,
            kind: JobKind::Check,
            handle,
        });
        self.next_check = None;
        self.before_check = Some(self.current_state());
        self.state = UpdateState::Checking;
    }

    fn start_download(&mut self) {
        let Some(found) = &self.found else {
            return;
        };
        let release = found.release.clone();
        let version = found.version.clone();
        let id = self.new_job_id();
        let backend = self.backend.clone();
        let events = self.events_tx.clone();
        let handle = tokio::spawn(async move {
            let progress = events.clone();
            let mut downloaded = 0u64;
            let mut last_sent: Option<std::time::Instant> = None;
            let on_chunk = Box::new(move |chunk: usize, total: Option<u64>| {
                downloaded += chunk as u64;
                if last_sent.is_none_or(|at| at.elapsed() >= PROGRESS_INTERVAL) {
                    last_sent = Some(std::time::Instant::now());
                    let _ = progress.send(Event::Progress {
                        job: id,
                        downloaded,
                        total,
                    });
                }
            });
            // The install job verifies the package before zebrad is touched.
            let result = backend.download(&release, on_chunk).await;
            let _ = events.send(Event::Downloaded { job: id, result });
        });
        self.job = Some(Job {
            id,
            kind: JobKind::Download,
            handle,
        });
        self.state = UpdateState::Downloading {
            version,
            downloaded: 0,
            total: None,
        };
    }

    fn start_install(&mut self) {
        let (Some(found), Some(bytes)) = (&self.found, &self.bytes) else {
            return;
        };
        let (release, bytes) = (found.release.clone(), bytes.clone());
        let version = found.version.clone();
        self.countdown_until = None;
        let pending = PendingUpdate {
            from_version: self.config.current_version.clone(),
            to_version: version.clone(),
            zebrad_was_running: self.status.borrow().phase.is_active(),
            written_at: SystemTime::now(),
        };
        // The supervisor's grace is fixed at launch; the settings may have
        // changed since, so the cap comes from the supervisor.
        let stop_cap = self.node.shutdown_cap();
        let id = self.new_job_id();
        let handle = tokio::spawn(install(InstallJob {
            id,
            backend: self.backend.clone(),
            node: self.node.clone(),
            release,
            bytes,
            marker: self.config.marker_file.clone(),
            pending,
            stop_cap,
            events: self.events_tx.clone(),
        }));
        self.job = Some(Job {
            id,
            kind: JobKind::Install,
            handle,
        });
        tracing::info!(%version, "installing the launcher update");
        self.state = UpdateState::Installing { version };
    }

    fn maybe_start_countdown(&mut self) {
        if self.countdown_until.is_some() || self.job.is_some() || self.installed {
            return;
        }
        let (Some(found), Some(_)) = (&self.found, &self.bytes) else {
            return;
        };
        let dismissed = self.dismissed.as_deref() == Some(found.version.as_str());
        let eos_warning = eos_warning_active(self.eos.as_ref(), self.eos_halted, SystemTime::now());
        if should_auto_install(self.mode, self.can_install, eos_warning, dismissed) {
            tracing::info!(
                version = %found.version,
                secs = self.config.countdown.as_secs(),
                "end of support is near; installing the update after a countdown"
            );
            self.countdown_until = Some(Instant::now() + self.config.countdown);
            self.state = self.resting_state().unwrap_or_default();
        }
    }

    // --- State ----------------------------------------------------------------

    fn reschedule(&mut self) {
        self.next_check = next_check(
            self.mode,
            self.launched,
            self.last_attempt,
            &mut self.jitter,
        );
    }

    fn next_wakeup(&self) -> Option<Instant> {
        let check = self.next_check.filter(|_| self.mode != UpdateMode::Off);
        // Wake every second during a countdown so the UI sees it tick.
        let countdown = self
            .countdown_until
            .map(|until| until.min(Instant::now() + Duration::from_secs(1)));
        match (check, countdown) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// The state for the found update when nothing is running.
    fn resting_state(&self) -> Option<UpdateState> {
        let found = self.found.as_ref()?;
        Some(if self.bytes.is_some() {
            UpdateState::ReadyToInstall {
                version: found.version.clone(),
                countdown_secs: self.countdown_secs(),
            }
        } else {
            UpdateState::Available {
                version: found.version.clone(),
                notes: found.notes.clone(),
                pub_date: found.pub_date,
            }
        })
    }

    fn countdown_secs(&self) -> Option<u64> {
        self.countdown_until.map(|until| {
            let left = until.saturating_duration_since(Instant::now());
            left.as_secs() + u64::from(left.subsec_nanos() > 0)
        })
    }

    fn current_state(&self) -> UpdateState {
        let mut state = self.state.clone();
        if let UpdateState::ReadyToInstall { countdown_secs, .. } = &mut state {
            *countdown_secs = self.countdown_secs();
        }
        state
    }

    fn reply_waiters(&mut self) {
        if self.job_kind() == Some(JobKind::Check) {
            return;
        }
        let state = self.current_state();
        for waiter in self.check_waiters.drain(..) {
            let _ = waiter.send(state.clone());
        }
    }

    fn save_record(&self) {
        let Some(path) = self.config.record_file.clone() else {
            return;
        };
        let record = UpdaterRecord {
            last_successful_check: self.last_success,
        };
        tokio::task::spawn_blocking(move || {
            if let Err(err) = save_record(&path, &record) {
                tracing::warn!(%err, "could not save the updater record");
            }
        });
    }

    fn publish(&self) {
        let next = UpdateInfo {
            state: self.current_state(),
            mode: self.mode,
            current_version: self.config.current_version.clone(),
            last_successful_check: self.last_success,
            stale_check_warning: stale_check_warning(
                self.last_success,
                self.eos.as_ref(),
                self.eos_halted,
                SystemTime::now(),
            ),
            can_install: self.can_install,
            releases_url: self.config.releases_url.clone(),
            health_failure: self.health_failure.clone(),
        };
        self.info.send_if_modified(|info| {
            if *info == next {
                false
            } else {
                *info = next;
                true
            }
        });
    }
}

async fn sleep_until_or_never(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

struct InstallJob<B: UpdateBackend, N> {
    id: u64,
    backend: Arc<B>,
    node: N,
    release: B::Release,
    bytes: Arc<Vec<u8>>,
    marker: Option<PathBuf>,
    pending: PendingUpdate,
    stop_cap: Duration,
    events: mpsc::UnboundedSender<Event<B::Release>>,
}

/// verify → stop zebrad → marker → install → restart. On failure the marker
/// is removed, zebrad is started again if it was running (once any stop has
/// finished), and the actor is told.
async fn install<B: UpdateBackend, N: NodeControl>(job: InstallJob<B, N>) {
    let InstallJob {
        id,
        backend,
        node,
        release,
        bytes,
        marker,
        pending,
        stop_cap,
        events,
    } = job;
    let was_running = pending.zebrad_was_running;
    let fail = |message: String| {
        let _ = events.send(Event::InstallFailed { job: id, message });
    };
    let start_again = async || {
        if was_running {
            tracing::info!("starting zebrad again after the failed update");
            if let Err(err) = node.start().await {
                tracing::warn!(%err, "zebrad did not start after the failed update");
            }
        }
    };

    // Verify before zebrad is touched. Hashing the whole package is CPU
    // work, so it runs on the blocking pool.
    let verified = {
        let (backend, release, bytes) = (backend.clone(), release.clone(), bytes.clone());
        tokio::task::spawn_blocking(move || backend.verify(&release, &bytes))
            .await
            .unwrap_or_else(|err| Err(format!("the verification task failed: {err}")))
    };
    if let Err(err) = verified {
        fail(format!("the update's signature did not verify: {err}"));
        return;
    }
    tracing::info!("stopping zebrad before installing the update");
    let stop = node.stop();
    tokio::pin!(stop);
    let stopped = match tokio::time::timeout(stop_cap, &mut stop).await {
        Ok(result) => result,
        Err(_) => {
            let secs = stop_cap.as_secs();
            // Never start zebrad on top of a stop that is still under way:
            // wait for it to finish (the supervisor kills zebrad after the
            // grace period) before bringing zebrad back.
            tracing::warn!(
                secs,
                "zebrad did not stop in time; waiting for the stop to finish"
            );
            if tokio::time::timeout(stop_cap, &mut stop).await.is_ok() {
                start_again().await;
            } else {
                tracing::error!("zebrad is still stopping; not starting it again");
            }
            fail(format!(
                "zebrad could not be stopped for the update: it did not stop within {secs} seconds"
            ));
            return;
        }
    };
    if let Err(err) = stopped {
        start_again().await;
        fail(format!("zebrad could not be stopped for the update: {err}"));
        return;
    }

    // Written only now, right before the install, so a crash or failure
    // before this point never makes the next launch run the health gate.
    if let Some(marker) = &marker
        && let Err(err) = write_marker(marker, &pending)
    {
        // The update can still go ahead; only the post-update check is lost.
        tracing::warn!(%err, "could not write the pending-update marker");
    }
    match backend.install(&release, bytes).await {
        Ok(()) => {
            let _ = events.send(Event::Installed { job: id });
            tracing::info!(version = %pending.to_version, "update installed; restarting");
            backend.restart();
        }
        Err(err) => {
            if let Some(marker) = &marker
                && let Err(err) = clear_marker(marker)
            {
                tracing::warn!(%err, "could not remove the pending-update marker");
            }
            start_again().await;
            fail(format!("the update could not be installed: {err}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        net::SocketAddr,
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    use super::*;
    use crate::{
        supervisor,
        updates::{AUTO_INSTALL_COUNTDOWN, CHECK_INTERVAL, FIRST_CHECK_DELAY, read_marker},
    };

    const DAY: Duration = Duration::from_secs(24 * 60 * 60);
    const T: Duration = Duration::from_secs(60);

    /// A fake update endpoint and installer that counts every request.
    #[derive(Clone, Default)]
    struct FakeBackend {
        inner: Arc<FakeInner>,
    }

    #[derive(Default)]
    struct FakeInner {
        latest: watch::Sender<Option<String>>,
        not_installable: AtomicBool,
        bad_signature: AtomicBool,
        install_fails: AtomicBool,
        download_delay_secs: AtomicUsize,
        check_delay_secs: AtomicUsize,
        checks: AtomicUsize,
        downloads: AtomicUsize,
        verifies: AtomicUsize,
        installs: AtomicUsize,
        restarts: AtomicUsize,
    }

    impl FakeBackend {
        fn with_latest(version: Option<&str>) -> Self {
            let backend = Self::default();
            backend
                .inner
                .latest
                .send_replace(version.map(str::to_string));
            backend
        }
        fn requests(&self) -> usize {
            self.inner.checks.load(Ordering::SeqCst) + self.inner.downloads.load(Ordering::SeqCst)
        }
        fn count(&self, field: fn(&FakeInner) -> &AtomicUsize) -> usize {
            field(&self.inner).load(Ordering::SeqCst)
        }
    }

    impl UpdateBackend for FakeBackend {
        type Release = String;

        fn can_install(&self) -> bool {
            !self.inner.not_installable.load(Ordering::SeqCst)
        }

        async fn check(&self) -> Result<Option<FoundUpdate<String>>, String> {
            self.inner.checks.fetch_add(1, Ordering::SeqCst);
            let delay = self.inner.check_delay_secs.load(Ordering::SeqCst) as u64;
            tokio::time::sleep(Duration::from_secs(delay)).await;
            Ok(self
                .inner
                .latest
                .borrow()
                .clone()
                .map(|version| FoundUpdate {
                    version: version.clone(),
                    notes: Some("notes".into()),
                    pub_date: None,
                    release: version,
                }))
        }

        async fn download(
            &self,
            release: &String,
            mut on_chunk: crate::updates::OnChunk,
        ) -> Result<Vec<u8>, String> {
            self.inner.downloads.fetch_add(1, Ordering::SeqCst);
            let delay = self.inner.download_delay_secs.load(Ordering::SeqCst) as u64;
            tokio::time::sleep(Duration::from_secs(delay)).await;
            on_chunk(release.len(), Some(release.len() as u64));
            Ok(release.as_bytes().to_vec())
        }

        fn verify(&self, release: &String, bytes: &[u8]) -> Result<(), String> {
            self.inner.verifies.fetch_add(1, Ordering::SeqCst);
            if self.inner.bad_signature.load(Ordering::SeqCst) || bytes != release.as_bytes() {
                Err("signature mismatch".into())
            } else {
                Ok(())
            }
        }

        async fn install(&self, _release: &String, _bytes: Arc<Vec<u8>>) -> Result<(), String> {
            self.inner.installs.fetch_add(1, Ordering::SeqCst);
            if self.inner.install_fails.load(Ordering::SeqCst) {
                Err("disk full".into())
            } else {
                Ok(())
            }
        }

        fn restart(&self) {
            self.inner.restarts.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[derive(Clone)]
    struct FakeNode {
        status: Arc<watch::Sender<Status>>,
        stops: Arc<AtomicUsize>,
        starts: Arc<AtomicUsize>,
        /// The supervisor's stop grace, in seconds; `shutdown_cap` follows it.
        stop_grace_secs: Arc<AtomicUsize>,
        /// How long `stop` takes, in seconds.
        stop_delay_secs: Arc<AtomicUsize>,
        /// Set while a stop is under way.
        stopping: Arc<AtomicBool>,
        /// A start arrived while a stop was under way.
        started_while_stopping: Arc<AtomicBool>,
    }

    fn running() -> Status {
        Status {
            phase: Phase::Running,
            rpc_addr: Some(SocketAddr::from(([127, 0, 0, 1], 8232))),
            ..Status::default()
        }
    }

    impl FakeNode {
        fn new(status: Status) -> Self {
            Self {
                status: Arc::new(watch::channel(status).0),
                stops: Arc::default(),
                starts: Arc::default(),
                stop_grace_secs: Arc::new(AtomicUsize::new(60)),
                stop_delay_secs: Arc::default(),
                stopping: Arc::default(),
                started_while_stopping: Arc::default(),
            }
        }
    }

    impl NodeControl for FakeNode {
        fn watch_status(&self) -> watch::Receiver<Status> {
            self.status.subscribe()
        }
        fn shutdown_cap(&self) -> Duration {
            let grace = self.stop_grace_secs.load(Ordering::SeqCst) as u64;
            supervisor::shutdown_cap(Duration::from_secs(grace))
        }
        async fn stop(&self) -> Result<(), String> {
            self.stops.fetch_add(1, Ordering::SeqCst);
            self.stopping.store(true, Ordering::SeqCst);
            let delay = self.stop_delay_secs.load(Ordering::SeqCst) as u64;
            tokio::time::sleep(Duration::from_secs(delay)).await;
            self.status.send_replace(Status::default());
            self.stopping.store(false, Ordering::SeqCst);
            Ok(())
        }
        async fn start(&self) -> Result<(), String> {
            if self.stopping.load(Ordering::SeqCst) {
                self.started_while_stopping.store(true, Ordering::SeqCst);
            }
            self.starts.fetch_add(1, Ordering::SeqCst);
            self.status.send_replace(running());
            Ok(())
        }
    }

    struct Harness {
        handle: UpdaterHandle,
        backend: FakeBackend,
        node: FakeNode,
        settings: watch::Sender<Settings>,
        stats: watch::Sender<Option<NodeStats>>,
        _dir: tempfile::TempDir,
        marker: PathBuf,
        record: PathBuf,
    }

    fn harness(mode: UpdateMode, backend: FakeBackend) -> Harness {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("pending_update_check");
        let record = dir.path().join("updater.json");
        let node = FakeNode::new(running());
        let (settings, settings_rx) = watch::channel(Settings {
            update_mode: mode,
            ..Settings::default()
        });
        let (stats, stats_rx) = watch::channel(None);
        let handle = spawn(
            backend.clone(),
            node.clone(),
            settings_rx,
            stats_rx,
            UpdaterConfig {
                current_version: "0.1.0".into(),
                releases_url: "https://example.invalid/releases".into(),
                record_file: Some(record.clone()),
                record: UpdaterRecord::default(),
                marker_file: Some(marker.clone()),
                seed: 1,
                countdown: AUTO_INSTALL_COUNTDOWN,
            },
        );
        Harness {
            handle,
            backend,
            node,
            settings,
            stats,
            _dir: dir,
            marker,
            record,
        }
    }

    fn stats_with_eos(days: u64) -> NodeStats {
        NodeStats {
            height: 1,
            estimated_height: 1,
            verification_progress: 1.0,
            peers: 8,
            mempool_transactions: 0,
            chain: "main".into(),
            zebrad_version: "4.0.0".into(),
            last_error: None,
            eos: Some(EosInfo {
                block_height: 9,
                estimated_time: SystemTime::now() + DAY * days as u32,
            }),
            updated_at: SystemTime::now(),
        }
    }

    async fn wait_for(
        handle: &UpdaterHandle,
        what: &str,
        mut predicate: impl FnMut(&UpdateInfo) -> bool,
    ) -> UpdateInfo {
        let mut rx = handle.watch();
        tokio::time::timeout(T, async {
            loop {
                {
                    let info = rx.borrow_and_update();
                    if predicate(&info) {
                        return info.clone();
                    }
                }
                rx.changed().await.expect("updater alive");
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}: {:?}", handle.info()))
    }

    /// Lets the paused clock run for `d`, letting every task make progress.
    async fn idle(d: Duration) {
        tokio::time::timeout(d + T, tokio::time::sleep(d))
            .await
            .expect("sleep");
    }

    #[tokio::test(start_paused = true)]
    async fn off_mode_makes_zero_requests() {
        let h = harness(UpdateMode::Off, FakeBackend::with_latest(Some("0.2.0")));
        // Everything that could tempt it: EOS imminent, then a halted zebrad.
        h.stats.send_replace(Some(stats_with_eos(1)));
        idle(Duration::from_secs(60)).await;
        h.node.status.send_replace(Status {
            phase: Phase::Failed {
                class: ExitClass::EosHalt,
            },
            ..Status::default()
        });
        idle(DAY * 30).await;
        assert_eq!(
            h.backend.requests(),
            0,
            "Off mode must not touch the network"
        );
        assert_eq!(h.handle.state(), UpdateState::Idle);
        assert!(h.handle.info().stale_check_warning);
        assert_eq!(
            tokio::time::timeout(T, h.handle.install())
                .await
                .expect("in time"),
            Err(UpdaterError::NoUpdate)
        );
        assert_eq!(h.backend.requests(), 0);

        // The user asked: one check, still no download in Off mode.
        let state = tokio::time::timeout(T, h.handle.check())
            .await
            .expect("in time")
            .expect("check");
        assert!(matches!(state, UpdateState::Available { ref version, .. } if version == "0.2.0"));
        idle(DAY).await;
        assert_eq!(h.backend.count(|i| &i.checks), 1);
        assert_eq!(h.backend.count(|i| &i.downloads), 0);
        assert!(!h.handle.info().stale_check_warning);
    }

    #[tokio::test(start_paused = true)]
    async fn notify_checks_30s_after_launch_then_about_every_6h() {
        let h = harness(UpdateMode::Notify, FakeBackend::with_latest(None));
        idle(FIRST_CHECK_DELAY - Duration::from_secs(1)).await;
        assert_eq!(h.backend.count(|i| &i.checks), 0);
        idle(Duration::from_secs(2)).await;
        assert_eq!(h.backend.count(|i| &i.checks), 1);
        wait_for(&h.handle, "up to date", |i| {
            matches!(i.state, UpdateState::UpToDate { .. })
        })
        .await;
        idle(CHECK_INTERVAL.mul_f64(0.9) - Duration::from_secs(5)).await;
        assert_eq!(h.backend.count(|i| &i.checks), 1);
        idle(CHECK_INTERVAL.mul_f64(0.2) + Duration::from_secs(10)).await;
        assert_eq!(h.backend.count(|i| &i.checks), 2);
        // The success was recorded on disk.
        idle(Duration::from_secs(1)).await;
        assert!(
            crate::updates::load_record(&h.record)
                .last_successful_check
                .is_some()
        );

        // Notify finds an update but never downloads it by itself.
        h.backend.inner.latest.send_replace(Some("0.2.0".into()));
        idle(CHECK_INTERVAL * 2).await;
        wait_for(&h.handle, "available", |i| {
            matches!(i.state, UpdateState::Available { .. })
        })
        .await;
        assert_eq!(h.backend.count(|i| &i.downloads), 0);
        assert_eq!(h.node.stops.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn notify_install_on_user_click() {
        let h = harness(UpdateMode::Notify, FakeBackend::with_latest(Some("0.2.0")));
        tokio::time::timeout(T, h.handle.check())
            .await
            .expect("in time")
            .expect("check");
        tokio::time::timeout(T, h.handle.install())
            .await
            .expect("in time")
            .expect("install starts");
        idle(Duration::from_secs(1)).await;
        assert_eq!(h.backend.count(|i| &i.restarts), 1);
        assert_eq!(h.node.stops.load(Ordering::SeqCst), 1);
        assert_eq!(h.backend.count(|i| &i.installs), 1);
        // One verification, before zebrad was stopped.
        assert_eq!(h.backend.count(|i| &i.verifies), 1);
        let pending = read_marker(&h.marker)
            .expect("read")
            .expect("marker written");
        assert_eq!(pending.to_version, "0.2.0");
        assert_eq!(pending.from_version, "0.1.0");
        assert!(pending.zebrad_was_running);
        assert!(matches!(h.handle.state(), UpdateState::Installing { .. }));
    }

    #[tokio::test(start_paused = true)]
    async fn auto_downloads_then_installs_at_eos_after_a_countdown() {
        let h = harness(UpdateMode::Auto, FakeBackend::with_latest(Some("0.2.0")));
        // EOS far away: download, then wait for the user.
        h.stats.send_replace(Some(stats_with_eos(40)));
        idle(FIRST_CHECK_DELAY + Duration::from_secs(1)).await;
        wait_for(&h.handle, "ready", |i| {
            i.state
                == UpdateState::ReadyToInstall {
                    version: "0.2.0".into(),
                    countdown_secs: None,
                }
        })
        .await;
        idle(DAY).await;
        assert_eq!(h.backend.count(|i| &i.installs), 0, "never silently");

        // The EOS warning starts a visible countdown.
        h.stats.send_replace(Some(stats_with_eos(10)));
        let info = wait_for(&h.handle, "countdown", |i| {
            matches!(
                i.state,
                UpdateState::ReadyToInstall {
                    countdown_secs: Some(_),
                    ..
                }
            )
        })
        .await;
        assert!(matches!(
            info.state,
            UpdateState::ReadyToInstall { countdown_secs: Some(s), .. } if s <= 60 && s > 50
        ));
        assert_eq!(h.backend.count(|i| &i.installs), 0);
        idle(AUTO_INSTALL_COUNTDOWN + Duration::from_secs(2)).await;
        assert_eq!(h.backend.count(|i| &i.installs), 1);
        assert_eq!(h.backend.count(|i| &i.restarts), 1);
        assert_eq!(h.node.stops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn cancelling_the_countdown_keeps_the_update_waiting() {
        let h = harness(UpdateMode::Auto, FakeBackend::with_latest(Some("0.2.0")));
        h.stats.send_replace(Some(stats_with_eos(3)));
        idle(FIRST_CHECK_DELAY + Duration::from_secs(1)).await;
        wait_for(&h.handle, "countdown", |i| {
            matches!(
                i.state,
                UpdateState::ReadyToInstall {
                    countdown_secs: Some(_),
                    ..
                }
            )
        })
        .await;
        tokio::time::timeout(T, h.handle.cancel())
            .await
            .expect("in time")
            .expect("cancel");
        assert_eq!(
            h.handle.state(),
            UpdateState::ReadyToInstall {
                version: "0.2.0".into(),
                countdown_secs: None
            }
        );
        idle(DAY).await;
        assert_eq!(h.backend.count(|i| &i.installs), 0);
        // The user can still install by hand.
        tokio::time::timeout(T, h.handle.install())
            .await
            .expect("in time")
            .expect("install");
        idle(Duration::from_secs(1)).await;
        assert_eq!(h.backend.count(|i| &i.installs), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn eos_halt_counts_as_the_warning() {
        let h = harness(UpdateMode::Auto, FakeBackend::with_latest(Some("0.2.0")));
        h.node.status.send_replace(Status {
            phase: Phase::Failed {
                class: ExitClass::EosHalt,
            },
            ..Status::default()
        });
        idle(FIRST_CHECK_DELAY + AUTO_INSTALL_COUNTDOWN + Duration::from_secs(5)).await;
        assert_eq!(h.backend.count(|i| &i.installs), 1);
        // zebrad was not running, so the marker says so and nothing restarts it.
        let pending = read_marker(&h.marker).expect("read").expect("marker");
        assert!(!pending.zebrad_was_running);
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_install_restarts_zebrad_and_reports() {
        let backend = FakeBackend::with_latest(Some("0.2.0"));
        backend.inner.install_fails.store(true, Ordering::SeqCst);
        let h = harness(UpdateMode::Notify, backend);
        tokio::time::timeout(T, h.handle.check())
            .await
            .expect("in time")
            .expect("check");
        tokio::time::timeout(T, h.handle.install())
            .await
            .expect("in time")
            .expect("starts");
        let info = wait_for(&h.handle, "error", |i| {
            matches!(i.state, UpdateState::Error { .. })
        })
        .await;
        let UpdateState::Error { message, .. } = info.state else {
            unreachable!()
        };
        assert!(message.contains("disk full"), "{message}");
        assert_eq!(h.node.stops.load(Ordering::SeqCst), 1);
        assert_eq!(h.node.starts.load(Ordering::SeqCst), 1);
        assert_eq!(h.backend.count(|i| &i.restarts), 0);
        assert_eq!(read_marker(&h.marker).expect("read"), None);
    }

    #[tokio::test(start_paused = true)]
    async fn a_bad_signature_never_stops_zebrad() {
        let backend = FakeBackend::with_latest(Some("0.2.0"));
        backend.inner.bad_signature.store(true, Ordering::SeqCst);
        let h = harness(UpdateMode::Notify, backend);
        tokio::time::timeout(T, h.handle.check())
            .await
            .expect("in time")
            .expect("check");
        tokio::time::timeout(T, h.handle.install())
            .await
            .expect("in time")
            .expect("starts");
        let info = wait_for(&h.handle, "error", |i| {
            matches!(i.state, UpdateState::Error { .. })
        })
        .await;
        assert!(
            matches!(info.state, UpdateState::Error { ref message, .. } if message.contains("signature"))
        );
        assert_eq!(h.node.stops.load(Ordering::SeqCst), 0);
        assert_eq!(h.backend.count(|i| &i.installs), 0);
        assert_eq!(read_marker(&h.marker).expect("read"), None);
    }

    #[tokio::test(start_paused = true)]
    async fn switching_to_off_cancels_a_download() {
        let backend = FakeBackend::with_latest(Some("0.2.0"));
        backend
            .inner
            .download_delay_secs
            .store(3600, Ordering::SeqCst);
        let h = harness(UpdateMode::Auto, backend);
        idle(FIRST_CHECK_DELAY + Duration::from_secs(1)).await;
        wait_for(&h.handle, "downloading", |i| {
            matches!(i.state, UpdateState::Downloading { .. })
        })
        .await;
        h.settings.send_modify(|s| s.update_mode = UpdateMode::Off);
        wait_for(&h.handle, "idle", |i| {
            i.state == UpdateState::Idle && i.mode == UpdateMode::Off
        })
        .await;
        let checks = h.backend.count(|i| &i.checks);
        idle(DAY * 7).await;
        assert_eq!(h.backend.count(|i| &i.checks), checks);
        assert_eq!(h.handle.state(), UpdateState::Idle);
        assert_eq!(h.backend.count(|i| &i.installs), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_a_download() {
        let backend = FakeBackend::with_latest(Some("0.2.0"));
        backend
            .inner
            .download_delay_secs
            .store(3600, Ordering::SeqCst);
        let h = harness(UpdateMode::Notify, backend);
        tokio::time::timeout(T, h.handle.check())
            .await
            .expect("in time")
            .expect("check");
        tokio::time::timeout(T, h.handle.install())
            .await
            .expect("in time")
            .expect("starts");
        assert!(matches!(h.handle.state(), UpdateState::Downloading { .. }));
        tokio::time::timeout(T, h.handle.cancel())
            .await
            .expect("in time")
            .expect("cancel");
        assert!(matches!(h.handle.state(), UpdateState::Available { .. }));
        idle(Duration::from_secs(7200)).await;
        assert_eq!(h.backend.count(|i| &i.installs), 0);
        assert_eq!(h.node.stops.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn deb_installs_only_notify() {
        let backend = FakeBackend::with_latest(Some("0.2.0"));
        backend.inner.not_installable.store(true, Ordering::SeqCst);
        let h = harness(UpdateMode::Auto, backend);
        h.stats.send_replace(Some(stats_with_eos(1)));
        idle(FIRST_CHECK_DELAY + DAY).await;
        let info = h.handle.info();
        assert!(!info.can_install);
        assert!(matches!(info.state, UpdateState::Available { .. }));
        assert_eq!(h.backend.count(|i| &i.downloads), 0);
        assert_eq!(
            tokio::time::timeout(T, h.handle.install())
                .await
                .expect("in time"),
            Err(UpdaterError::NotInstallable)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_pulled_release_is_dropped() {
        let h = harness(UpdateMode::Notify, FakeBackend::with_latest(Some("0.2.0")));
        tokio::time::timeout(T, h.handle.check())
            .await
            .expect("in time")
            .expect("check");
        // The bad-release runbook moves "latest" back: the next check finds nothing.
        h.backend.inner.latest.send_replace(None);
        let state = tokio::time::timeout(T, h.handle.check())
            .await
            .expect("in time")
            .expect("check");
        assert!(matches!(state, UpdateState::UpToDate { .. }));
        assert_eq!(
            tokio::time::timeout(T, h.handle.install())
                .await
                .expect("in time"),
            Err(UpdaterError::NoUpdate)
        );
    }

    async fn check_and_install(h: &Harness) {
        tokio::time::timeout(T, h.handle.check())
            .await
            .expect("in time")
            .expect("check");
        tokio::time::timeout(T, h.handle.install())
            .await
            .expect("in time")
            .expect("install starts");
    }

    #[tokio::test(start_paused = true)]
    async fn the_stop_cap_has_a_floor() {
        let h = harness(UpdateMode::Notify, FakeBackend::with_latest(Some("0.2.0")));
        h.node.stop_grace_secs.store(5, Ordering::SeqCst);
        // The supervisor takes 40 s to stop: more than grace + margin, less
        // than the 70 s floor.
        h.node.stop_delay_secs.store(40, Ordering::SeqCst);
        check_and_install(&h).await;
        idle(Duration::from_secs(41)).await;
        assert_eq!(h.backend.count(|i| &i.installs), 1);
        assert_eq!(h.node.starts.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn the_stop_cap_follows_the_supervisor_not_the_settings() {
        let h = harness(UpdateMode::Notify, FakeBackend::with_latest(Some("0.2.0")));
        // The grace was lowered in the settings after launch; the supervisor
        // still waits its 120 s before killing zebrad, and so must the cap.
        h.settings.send_modify(|s| s.stop_grace_secs = 5);
        h.node.stop_grace_secs.store(120, Ordering::SeqCst);
        h.node.stop_delay_secs.store(100, Ordering::SeqCst);
        check_and_install(&h).await;
        idle(Duration::from_secs(101)).await;
        assert_eq!(
            h.backend.count(|i| &i.installs),
            1,
            "{:?}",
            h.handle.state()
        );
        assert_eq!(h.node.starts.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn a_slow_stop_is_waited_for_before_zebrad_starts_again() {
        let h = harness(UpdateMode::Notify, FakeBackend::with_latest(Some("0.2.0")));
        let cap = supervisor::shutdown_cap(Duration::from_secs(60));
        h.node
            .stop_delay_secs
            .store((cap.as_secs() + 30) as usize, Ordering::SeqCst);
        check_and_install(&h).await;
        idle(cap + Duration::from_secs(10)).await;
        // Past the cap, the stop is still under way: nothing starts yet.
        assert_eq!(h.node.starts.load(Ordering::SeqCst), 0);
        assert!(matches!(h.handle.state(), UpdateState::Installing { .. }));
        let info = wait_for(&h.handle, "error", |i| {
            matches!(i.state, UpdateState::Error { .. })
        })
        .await;
        assert!(
            matches!(info.state, UpdateState::Error { ref message, .. } if message.contains("did not stop")),
            "{:?}",
            info.state
        );
        assert_eq!(h.node.starts.load(Ordering::SeqCst), 1);
        assert!(!h.node.started_while_stopping.load(Ordering::SeqCst));
        assert_eq!(h.backend.count(|i| &i.installs), 0);
        assert_eq!(read_marker(&h.marker).expect("read"), None);
    }

    #[tokio::test(start_paused = true)]
    async fn a_stop_that_never_finishes_does_not_start_zebrad_again() {
        let h = harness(UpdateMode::Notify, FakeBackend::with_latest(Some("0.2.0")));
        h.node.stop_delay_secs.store(100_000, Ordering::SeqCst);
        check_and_install(&h).await;
        // The cap, then as long again for the stop to finish.
        idle(supervisor::shutdown_cap(Duration::from_secs(60)) * 2).await;
        wait_for(&h.handle, "error", |i| {
            matches!(i.state, UpdateState::Error { .. })
        })
        .await;
        assert_eq!(h.node.starts.load(Ordering::SeqCst), 0);
        assert_eq!(h.backend.count(|i| &i.installs), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn cancelling_a_check_restores_the_previous_state() {
        let h = harness(UpdateMode::Notify, FakeBackend::with_latest(None));
        let first = tokio::time::timeout(T, h.handle.check())
            .await
            .expect("in time")
            .expect("check");
        assert!(matches!(first, UpdateState::UpToDate { .. }));

        h.backend
            .inner
            .check_delay_secs
            .store(3600, Ordering::SeqCst);
        let handle = h.handle.clone();
        let check = tokio::spawn(async move { handle.check().await });
        wait_for(&h.handle, "checking", |i| i.state == UpdateState::Checking).await;
        tokio::time::timeout(T, h.handle.cancel())
            .await
            .expect("in time")
            .expect("cancel");
        assert_eq!(h.handle.state(), first);
        let answered = tokio::time::timeout(T, check)
            .await
            .expect("in time")
            .expect("join")
            .expect("check");
        assert_eq!(answered, first);
        assert!(h.handle.info().last_successful_check.is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn the_marker_is_written_only_after_zebrad_stopped() {
        let h = harness(UpdateMode::Notify, FakeBackend::with_latest(Some("0.2.0")));
        h.node.stop_delay_secs.store(30, Ordering::SeqCst);
        check_and_install(&h).await;
        idle(Duration::from_secs(10)).await;
        assert_eq!(h.node.stops.load(Ordering::SeqCst), 1);
        assert_eq!(read_marker(&h.marker).expect("read"), None);
        idle(Duration::from_secs(30)).await;
        assert_eq!(h.backend.count(|i| &i.installs), 1);
        assert!(read_marker(&h.marker).expect("read").is_some());
    }
}
