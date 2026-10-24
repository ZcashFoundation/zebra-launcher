//! The supervisor actor task. See the module docs of [`super`].

use std::{
    collections::VecDeque,
    io,
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

use launcher_types::{
    ExitClass, ExitInfo, LogEvent, LogLine, LogStream, Phase, StaleProcessInfo, Status,
};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, BufReader},
    process::Child,
    runtime::Handle,
    sync::{broadcast, mpsc, oneshot, watch},
    task::JoinHandle,
};

use super::{Cmd, LogSubscription, StartOptions, SupervisorConfig, SupervisorError};
use crate::{
    logfile::LogFileWriter,
    logs::{Detector, classify_event, classify_exit, parse_line},
    pidfile,
    policy::{Decision, RestartTracker},
    process::{ChildHandle, SpawnedChild, Spawner, StopMethod},
};

/// Longer lines are truncated; zebrad's are a few hundred bytes.
const MAX_LINE_BYTES: usize = 64 * 1024;
/// How long to wait for a stale zebrad to exit after terminating it.
const STALE_KILL_WAIT: Duration = Duration::from_secs(5);

enum ChildEvent {
    Line {
        run: u64,
        stream: LogStream,
        text: String,
    },
    Eof {
        run: u64,
        stream: LogStream,
    },
    Exited {
        run: u64,
        status: io::Result<std::process::ExitStatus>,
    },
}

struct ExitOutcome {
    code: Option<i32>,
    signal: Option<i32>,
}

struct StopState {
    deadline: Instant,
    escalated: bool,
    forced: bool,
    replies: Vec<oneshot::Sender<()>>,
}

/// The child has exited; the run ends once its pipes close or the drain
/// deadline passes.
struct Exited {
    outcome: ExitOutcome,
    /// The stop that was under way when the child exited, if any.
    stop: Option<StopState>,
    /// Stops requested after the exit. There is nothing left to signal, so
    /// they only mark the run as stopped (no restart) and are answered when
    /// it is finalized.
    late_stop: Option<Vec<oneshot::Sender<()>>>,
    drain_deadline: Instant,
}

impl Exited {
    /// Whether a stop was requested, before or after the exit.
    fn stopped(&self) -> bool {
        self.stop.is_some() || self.late_stop.is_some()
    }
}

/// Where a run is in its life. Every handler matches on this, so a command
/// that arrives after the child exited cannot act on a reaped pid.
enum RunState {
    /// Spawned; waiting for `Starting zebrad`.
    Starting,
    /// zebrad accepted its configuration.
    Running,
    /// A stop was requested; waiting for the child to exit.
    Stopping(StopState),
    /// The child exited; waiting for its pipes to close.
    Exited(Exited),
}

impl RunState {
    /// Records the child's exit, keeping the stop that preceded it.
    fn on_exit(&mut self, outcome: ExitOutcome, drain_deadline: Instant) {
        let stop = match std::mem::replace(self, RunState::Starting) {
            RunState::Stopping(stop) => Some(stop),
            RunState::Exited(exited) => {
                // One exit per run; keep what was recorded.
                *self = RunState::Exited(exited);
                return;
            }
            RunState::Starting | RunState::Running => None,
        };
        *self = RunState::Exited(Exited {
            outcome,
            stop,
            late_stop: None,
            drain_deadline,
        });
    }
}

/// The callers still waiting on a run that has ended.
struct RunReplies {
    start: Vec<oneshot::Sender<Result<(), SupervisorError>>>,
    stop: Vec<oneshot::Sender<()>>,
    info: ExitInfo,
}

impl RunReplies {
    /// Start requests learn that zebrad exited; stop requests that it stopped.
    fn send(self) {
        for reply in self.start {
            let _ = reply.send(Err(SupervisorError::ExitedDuringStart(self.info.clone())));
        }
        for reply in self.stop {
            let _ = reply.send(());
        }
    }
}

struct Run {
    id: u64,
    handle: ChildHandle,
    detector: Detector,
    /// The first deterministic cause of failure logged in this run, which
    /// decides the exit class over the exit code.
    deterministic_cause: Option<ExitClass>,
    detail: Option<String>,
    stdout_eof: bool,
    stderr_eof: bool,
    state: RunState,
    /// Callers waiting for this run to reach `Running`.
    start_replies: Vec<oneshot::Sender<Result<(), SupervisorError>>>,
    started_at: Instant,
    tasks: Vec<JoinHandle<()>>,
}

pub(super) struct Actor {
    config: SupervisorConfig,
    cmd_rx: mpsc::Receiver<Cmd>,
    cmd_closed: bool,
    status: watch::Sender<Status>,
    logs: broadcast::Sender<Arc<LogLine>>,
    history: VecDeque<Arc<LogLine>>,
    next_seq: u64,
    spawner: Spawner,
    log_file: Option<LogFileWriter>,
    restarts: RestartTracker,
    child_tx: mpsc::Sender<ChildEvent>,
    child_rx: mpsc::Receiver<ChildEvent>,
    run: Option<Run>,
    next_run_id: u64,
    backoff_until: Option<Instant>,
    /// Starts requested while a stop is in progress; served once zebrad has exited.
    pending_start: Vec<oneshot::Sender<Result<(), SupervisorError>>>,
    /// `Some` once a shutdown was requested; the actor exits when zebrad is gone.
    shutdown_replies: Option<Vec<oneshot::Sender<()>>>,
}

impl Actor {
    pub(super) fn new(
        config: SupervisorConfig,
        cmd_rx: mpsc::Receiver<Cmd>,
        status: watch::Sender<Status>,
    ) -> io::Result<Self> {
        let spawner = Spawner::new(Handle::current())?;
        let log_file = config
            .log_file
            .clone()
            .map(LogFileWriter::spawn)
            .transpose()?;
        let (logs, _) = broadcast::channel(config.broadcast_capacity.max(1));
        let (child_tx, child_rx) = mpsc::channel(1024);
        Ok(Self {
            restarts: RestartTracker::new(config.restart.clone()),
            history: VecDeque::with_capacity(config.history_capacity.min(10_000)),
            config,
            cmd_rx,
            cmd_closed: false,
            status,
            logs,
            next_seq: 0,
            spawner,
            log_file,
            child_tx,
            child_rx,
            run: None,
            next_run_id: 1,
            backoff_until: None,
            pending_start: Vec::new(),
            shutdown_replies: None,
        })
    }

    pub(super) async fn run(mut self) {
        loop {
            if self.shutdown_replies.is_some() && self.run.is_none() {
                break;
            }
            let deadline = self.next_deadline();
            tokio::select! {
                biased;
                event = self.child_rx.recv() => {
                    if let Some(event) = event {
                        self.on_child_event(event).await;
                    }
                }
                cmd = self.cmd_rx.recv(), if !self.cmd_closed => match cmd {
                    Some(cmd) => self.on_cmd(cmd).await,
                    None => {
                        self.cmd_closed = true;
                        self.begin_shutdown(None);
                    }
                },
                _ = sleep_until_or_never(deadline) => self.on_timer().await,
            }
        }
        for reply in self.shutdown_replies.take().into_iter().flatten() {
            let _ = reply.send(());
        }
    }

    fn next_deadline(&self) -> Option<Instant> {
        let mut deadline = self.backoff_until;
        if let Some(run) = &self.run {
            let run_deadline = match &run.state {
                RunState::Stopping(stop) if !stop.escalated => Some(stop.deadline),
                RunState::Exited(exited) => Some(exited.drain_deadline),
                _ => None,
            };
            if let Some(run_deadline) = run_deadline {
                deadline = Some(deadline.map_or(run_deadline, |d| d.min(run_deadline)));
            }
        }
        deadline
    }

    async fn on_cmd(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Start { options, reply } => self.on_start(options, reply).await,
            Cmd::Stop { reply } => self.on_stop(vec![reply]),
            Cmd::Shutdown { reply } => self.begin_shutdown(Some(reply)),
            Cmd::Subscribe { reply } => {
                let _ = reply.send(LogSubscription {
                    history: self.history.iter().cloned().collect(),
                    live: self.logs.subscribe(),
                });
            }
            Cmd::History { since, reply } => {
                let _ = reply.send(
                    self.history
                        .iter()
                        .filter(|line| line.seq >= since)
                        .cloned()
                        .collect(),
                );
            }
        }
    }

    async fn on_start(
        &mut self,
        options: StartOptions,
        reply: oneshot::Sender<Result<(), SupervisorError>>,
    ) {
        if self.shutdown_replies.is_some() {
            let _ = reply.send(Err(SupervisorError::Closed));
            return;
        }
        if let Some(run) = self.run.as_mut() {
            match &run.state {
                RunState::Starting => run.start_replies.push(reply),
                RunState::Running => {
                    let _ = reply.send(Ok(()));
                }
                // Served once the stopped run is finalized.
                RunState::Stopping(_) => self.pending_start.push(reply),
                RunState::Exited(exited) if exited.stopped() => self.pending_start.push(reply),
                // The child is gone but the exit is not classified yet;
                // `finalize` answers with `ExitedDuringStart`.
                RunState::Exited(_) => run.start_replies.push(reply),
            }
            return;
        }
        // An explicit start cancels a scheduled restart and forgives past crashes.
        self.backoff_until = None;
        self.restarts.reset();
        self.spawn_run(options, vec![reply]).await;
    }

    async fn spawn_run(
        &mut self,
        options: StartOptions,
        start_replies: Vec<oneshot::Sender<Result<(), SupervisorError>>>,
    ) {
        if let Some(path) = self.config.pidfile.clone() {
            match pidfile::check(&path) {
                Ok(Some(stale)) => {
                    let mut alive = true;
                    if options.kill_stale {
                        if let Err(err) = pidfile::terminate(stale.pid) {
                            tracing::warn!(
                                ?err,
                                pid = stale.pid,
                                "could not terminate stale zebrad"
                            );
                        }
                        let deadline = Instant::now() + STALE_KILL_WAIT;
                        while pidfile::is_alive(stale.pid) && Instant::now() < deadline {
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                        alive = pidfile::is_alive(stale.pid);
                        if !alive {
                            let _ = pidfile::remove(&path);
                            self.note(format!(
                                "terminated a zebrad left behind by an earlier run (pid {})",
                                stale.pid
                            ));
                        }
                    }
                    if alive {
                        let info = StaleProcessInfo {
                            pid: stale.pid,
                            binary: stale.binary.display().to_string(),
                        };
                        self.status.send_modify(|s| s.stale = Some(info));
                        for reply in start_replies {
                            let _ = reply.send(Err(SupervisorError::StaleProcess(stale.clone())));
                        }
                        return;
                    }
                }
                Ok(None) => {}
                Err(err) => tracing::warn!(?err, "could not read the zebrad pid file"),
            }
        }
        self.status.send_modify(|s| s.stale = None);

        match self.spawner.spawn(self.config.command.clone()).await {
            Ok(spawned) => self.install_run(spawned, start_replies),
            Err(err) => {
                self.note(format!("could not start zebrad: {err}"));
                let info = ExitInfo {
                    code: None,
                    class: ExitClass::SpawnFailed,
                    detail: Some(err.to_string()),
                    at: SystemTime::now(),
                    requested: false,
                };
                self.status.send_modify(|s| {
                    s.phase = Phase::Failed {
                        class: ExitClass::SpawnFailed,
                    };
                    s.last_exit = Some(info);
                });
                for reply in start_replies {
                    let _ = reply.send(Err(SupervisorError::Spawn(io::Error::new(
                        err.kind(),
                        err.to_string(),
                    ))));
                }
            }
        }
    }

    fn install_run(
        &mut self,
        spawned: SpawnedChild,
        start_replies: Vec<oneshot::Sender<Result<(), SupervisorError>>>,
    ) {
        let SpawnedChild {
            handle,
            child,
            stdout,
            stderr,
        } = spawned;
        let id = self.next_run_id;
        self.next_run_id += 1;
        let pid = handle.pid();

        let tasks = vec![
            tokio::spawn(pump(stdout, id, LogStream::Stdout, self.child_tx.clone())),
            tokio::spawn(pump(stderr, id, LogStream::Stderr, self.child_tx.clone())),
            tokio::spawn(wait_for_exit(child, id, self.child_tx.clone())),
        ];

        if let Some(path) = &self.config.pidfile
            && let Err(err) = pidfile::write(path, pid, &self.config.command.binary)
        {
            tracing::warn!(?err, "could not write the zebrad pid file");
        }

        self.run = Some(Run {
            id,
            handle,
            detector: Detector::new(),
            deterministic_cause: None,
            detail: None,
            stdout_eof: false,
            stderr_eof: false,
            state: RunState::Starting,
            start_replies,
            started_at: Instant::now(),
            tasks,
        });
        self.status.send_modify(|s| {
            s.phase = Phase::Starting;
            s.pid = Some(pid);
            s.started_at = Some(SystemTime::now());
            s.rpc_addr = None;
            s.metrics_addr = None;
        });
        self.note(format!("zebrad started (pid {pid})"));
    }

    fn on_stop(&mut self, replies: Vec<oneshot::Sender<()>>) {
        if let Some(run) = self.run.as_mut() {
            match &mut run.state {
                RunState::Starting | RunState::Running => {
                    let mut forced = false;
                    let note = match run.handle.request_stop() {
                        Ok(StopMethod::StdinClosed) => "stop requested (stdin closed)".to_string(),
                        Ok(StopMethod::Signal) => "stop requested (SIGTERM)".to_string(),
                        Ok(StopMethod::Terminated) => {
                            forced = true;
                            "stop requested (terminated: this zebrad cannot stop gracefully on this platform)".to_string()
                        }
                        Err(err) => {
                            forced = true;
                            let _ = run.handle.kill();
                            format!("could not ask zebrad to stop ({err}); killed it")
                        }
                    };
                    run.state = RunState::Stopping(StopState {
                        deadline: Instant::now() + self.config.stop_grace,
                        escalated: false,
                        forced,
                        replies,
                    });
                    self.status.send_modify(|s| s.phase = Phase::Stopping);
                    self.note(note);
                }
                RunState::Stopping(stop)
                | RunState::Exited(Exited {
                    stop: Some(stop), ..
                }) => stop.replies.extend(replies),
                // The child is already gone: there is no pid to signal and
                // the phase stays until the exit is classified. The stop
                // still counts, so the run is not restarted.
                RunState::Exited(exited) => {
                    let first = exited.late_stop.is_none();
                    exited
                        .late_stop
                        .get_or_insert_with(Vec::new)
                        .extend(replies);
                    if first {
                        self.note(
                            "stop requested after zebrad exited; it will not be restarted"
                                .to_string(),
                        );
                    }
                }
            }
            return;
        }
        if self.backoff_until.take().is_some() {
            self.status.send_modify(|s| s.phase = Phase::Stopped);
            self.note("scheduled restart cancelled".to_string());
        }
        for reply in replies {
            let _ = reply.send(());
        }
    }

    fn begin_shutdown(&mut self, reply: Option<oneshot::Sender<()>>) {
        self.shutdown_replies
            .get_or_insert_with(Vec::new)
            .extend(reply);
        for reply in self.pending_start.drain(..) {
            let _ = reply.send(Err(SupervisorError::Closed));
        }
        self.on_stop(Vec::new());
    }

    async fn on_timer(&mut self) {
        let now = Instant::now();
        if let Some(run) = self.run.as_mut() {
            match &mut run.state {
                RunState::Stopping(stop) if !stop.escalated && now >= stop.deadline => {
                    stop.escalated = true;
                    stop.forced = true;
                    let killed = run.handle.kill();
                    let grace = self.config.stop_grace;
                    self.note(match killed {
                        Ok(()) => format!("zebrad did not stop within {grace:?}; killed it"),
                        Err(err) => format!("could not kill zebrad: {err}"),
                    });
                }
                RunState::Exited(exited) if now >= exited.drain_deadline => {
                    self.finalize().await;
                }
                _ => {}
            }
        }
        if self.backoff_until.is_some_and(|until| now >= until) {
            self.backoff_until = None;
            self.spawn_run(StartOptions::default(), Vec::new()).await;
        }
    }

    async fn on_child_event(&mut self, event: ChildEvent) {
        let current = self.run.as_ref().map(|run| run.id);
        match event {
            ChildEvent::Line { run, stream, text } if Some(run) == current => {
                self.push_line(stream, text);
            }
            ChildEvent::Eof { run, stream } if Some(run) == current => {
                if let Some(run) = self.run.as_mut() {
                    match stream {
                        LogStream::Stdout => run.stdout_eof = true,
                        LogStream::Stderr => run.stderr_eof = true,
                        LogStream::Launcher => {}
                    }
                }
                self.maybe_finalize().await;
            }
            ChildEvent::Exited { run, status } if Some(run) == current => {
                let outcome = match status {
                    Ok(status) => ExitOutcome {
                        code: status.code(),
                        signal: unix_signal(&status),
                    },
                    Err(err) => {
                        tracing::warn!(?err, "waiting for zebrad failed");
                        ExitOutcome {
                            code: None,
                            signal: None,
                        }
                    }
                };
                if let Some(run) = self.run.as_mut() {
                    run.state
                        .on_exit(outcome, Instant::now() + self.config.exit_drain);
                }
                self.maybe_finalize().await;
            }
            // Events from a previous run that are still draining.
            _ => {}
        }
    }

    async fn maybe_finalize(&mut self) {
        let ready = self.run.as_ref().is_some_and(|run| {
            matches!(run.state, RunState::Exited(_)) && run.stdout_eof && run.stderr_eof
        });
        if ready {
            self.finalize().await;
        }
    }

    async fn finalize(&mut self) {
        let Some(run) = self.run.take() else {
            return;
        };
        for task in &run.tasks {
            task.abort();
        }
        let no_outcome = || ExitOutcome {
            code: None,
            signal: None,
        };
        let (exit, stop, late_stop) = match run.state {
            RunState::Exited(exited) => (exited.outcome, exited.stop, exited.late_stop),
            // Only exited runs are finalized; these arms are defensive and
            // still answer the callers of a stop under way.
            RunState::Stopping(stop) => (no_outcome(), Some(stop), None),
            RunState::Starting | RunState::Running => (no_outcome(), None, None),
        };
        // A stop issued before the exit; one issued after it did not end the run.
        let requested = stop.is_some();
        let forced = stop.as_ref().is_some_and(|stop| stop.forced);
        let stopped = requested || late_stop.is_some() || self.shutdown_replies.is_some();
        let mut class = classify_exit(exit.code, run.deterministic_cause);
        if requested && !class.is_deterministic() {
            // A stop the user asked for ended the run; the exit code says how.
            class = ExitClass::Clean;
        }
        let detail = run.detail.or_else(|| {
            if forced {
                Some("killed by the launcher".to_string())
            } else {
                exit.signal
                    .map(|signal| format!("killed by signal {signal}"))
            }
        });
        let info = ExitInfo {
            code: exit.code,
            class,
            detail,
            at: SystemTime::now(),
            requested,
        };

        if let Some(path) = &self.config.pidfile
            && let Err(err) = pidfile::remove(path)
        {
            tracing::warn!(?err, "could not remove the zebrad pid file");
        }
        let code = exit
            .code
            .map_or_else(|| "no exit code".to_string(), |c| format!("exit code {c}"));
        self.note(format!(
            "zebrad exited ({code}, {class:?}{})",
            if requested { ", stop requested" } else { "" }
        ));

        // The callers waiting on this run are answered only once the status
        // shows how it ended, so a caller that reads the status after its
        // reply sees the final phase.
        let mut stop_replies = stop.map(|stop| stop.replies).unwrap_or_default();
        stop_replies.extend(late_stop.unwrap_or_default());
        let replies = RunReplies {
            start: run.start_replies,
            stop: stop_replies,
            info: info.clone(),
        };

        self.status.send_modify(|s| {
            s.pid = None;
            s.started_at = None;
            s.rpc_addr = None;
            s.metrics_addr = None;
            s.last_exit = Some(info);
        });

        if stopped {
            self.status.send_modify(|s| s.phase = Phase::Stopped);
            replies.send();
            if self.shutdown_replies.is_none() && !self.pending_start.is_empty() {
                let replies = std::mem::take(&mut self.pending_start);
                self.spawn_run(StartOptions::default(), replies).await;
            }
            return;
        }

        let now = Instant::now();
        if run.started_at.elapsed() >= self.config.restart.window {
            self.restarts.reset();
        }
        match self.restarts.on_exit(class, now) {
            Decision::GiveUp => {
                self.status
                    .send_modify(|s| s.phase = Phase::Failed { class });
                self.note(format!("not restarting zebrad ({class:?})"));
            }
            Decision::RestartAfter(delay) => {
                let crashes = self.restarts.recent_crashes(now);
                self.backoff_until = Some(now + delay);
                self.status.send_modify(|s| {
                    s.phase = Phase::Backoff {
                        until: SystemTime::now() + delay,
                        crashes,
                    }
                });
                self.note(format!("restarting zebrad in {delay:?}"));
            }
        }
        replies.send();
    }

    /// Records a line: on disk, in the history, to live subscribers, and
    /// through the event detector of the current run.
    fn push_line(&mut self, stream: LogStream, text: String) {
        if let Some(log_file) = &self.log_file {
            log_file.write_line(stream, text.trim_end_matches(['\r', '\n']));
        }
        let seq = self.next_seq;
        self.next_seq += 1;
        let line = Arc::new(parse_line(seq, stream, &text));
        if self.history.len() >= self.config.history_capacity.max(1) {
            self.history.pop_front();
        }
        self.history.push_back(line.clone());
        let _ = self.logs.send(line.clone());

        if let Some(run) = self.run.as_mut()
            && let Some(event) = run.detector.detect(&line)
        {
            if run.deterministic_cause.is_none() {
                run.deterministic_cause = classify_event(&event);
            }
            self.apply_event(event);
        }
    }

    fn apply_event(&mut self, event: LogEvent) {
        let Some(run) = self.run.as_mut() else {
            return;
        };
        match event {
            LogEvent::StartingZebrad => match run.state {
                RunState::Starting => {
                    run.state = RunState::Running;
                    self.status.send_modify(|s| s.phase = Phase::Running);
                    for reply in run.start_replies.drain(..) {
                        let _ = reply.send(Ok(()));
                    }
                }
                // A stop is under way: the callers learn zebrad came up, the
                // phase stays Stopping.
                RunState::Stopping(_) => {
                    for reply in run.start_replies.drain(..) {
                        let _ = reply.send(Ok(()));
                    }
                }
                // Already running, or the child exited before this line was
                // processed: an exited run never becomes Running.
                RunState::Running | RunState::Exited(_) => {}
            },
            LogEvent::RpcOpened { addr } => self.status.send_modify(|s| s.rpc_addr = Some(addr)),
            LogEvent::MetricsOpened { addr } => {
                self.status.send_modify(|s| s.metrics_addr = Some(addr));
            }
            LogEvent::FatalError { message }
            | LogEvent::Panic { message }
            | LogEvent::DbLocked { message }
            | LogEvent::PortInUse { message }
            | LogEvent::EosHalt { message } => {
                run.detail.get_or_insert(message);
            }
            _ => {}
        }
    }

    /// Adds a launcher note to the log stream.
    fn note(&mut self, text: String) {
        self.push_line(LogStream::Launcher, text);
    }
}

async fn sleep_until_or_never(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await,
        None => std::future::pending().await,
    }
}

#[cfg(unix)]
fn unix_signal(status: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal()
}

#[cfg(not(unix))]
fn unix_signal(_status: &std::process::ExitStatus) -> Option<i32> {
    None
}

async fn wait_for_exit(mut child: Child, run: u64, tx: mpsc::Sender<ChildEvent>) {
    let status = child.wait().await;
    let _ = tx.send(ChildEvent::Exited { run, status }).await;
}

/// Forwards lines from one pipe until EOF, then reports EOF.
async fn pump<R: AsyncRead + Unpin>(
    reader: R,
    run: u64,
    stream: LogStream,
    tx: mpsc::Sender<ChildEvent>,
) {
    let mut reader = BufReader::with_capacity(64 * 1024, reader);
    let mut buf = Vec::with_capacity(1024);
    while let Ok(true) = read_line_bounded(&mut reader, &mut buf, MAX_LINE_BYTES).await {
        let text = String::from_utf8_lossy(&buf).into_owned();
        if tx
            .send(ChildEvent::Line { run, stream, text })
            .await
            .is_err()
        {
            return;
        }
    }
    let _ = tx.send(ChildEvent::Eof { run, stream }).await;
}

/// Reads one line (without its newline) into `buf`, keeping at most `max`
/// bytes of it. Returns `Ok(false)` at EOF with nothing read.
async fn read_line_bounded<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    buf: &mut Vec<u8>,
    max: usize,
) -> io::Result<bool> {
    buf.clear();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(!buf.is_empty());
        }
        let newline = available.iter().position(|&b| b == b'\n');
        let chunk_len = newline.unwrap_or(available.len());
        let room = max.saturating_sub(buf.len());
        buf.extend_from_slice(&available[..chunk_len.min(room)]);
        let consumed = chunk_len + usize::from(newline.is_some());
        reader.consume(consumed);
        if newline.is_some() {
            return Ok(true);
        }
    }
}
