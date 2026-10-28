//! The zebrad supervisor: one actor task owns the child process and publishes
//! its state.
//!
//! Commands arrive over an `mpsc` channel, state leaves over a `watch` channel,
//! and log lines fan out over a `broadcast` channel with a bounded history for
//! late subscribers. There are no locks: every piece of mutable state lives in
//! the actor task.
//!
//! ```text
//!   SupervisorHandle ──Cmd──▶ actor ──▶ watch<Status>
//!        ▲                     │ ▲ ──▶ broadcast<Arc<LogLine>>
//!        └── status/logs ──────┘ │
//!                 stdout/stderr readers, exit waiter ──ChildEvent──┘
//! ```

use std::{io, path::PathBuf, sync::Arc, time::Duration};

use launcher_types::{ExitInfo, LogLine, Status};
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use crate::{
    logfile::RotatingLogConfig, pidfile::StaleProcess, policy::RestartPolicy,
    process::ZebradCommand,
};

mod actor;

/// Extra time on top of the stop grace period before giving up on a clean
/// shutdown: 70 s in total with the default 60 s grace.
const SHUTDOWN_MARGIN: Duration = Duration::from_secs(10);
/// The shutdown cap is never shorter than this.
const MIN_SHUTDOWN_CAP: Duration = Duration::from_secs(70);

/// How long to wait for zebrad to stop, given the stop grace period, before
/// giving up on a clean shutdown. Callers of a running supervisor use
/// [`SupervisorHandle::shutdown_cap`], which applies its actual grace.
pub fn shutdown_cap(grace: Duration) -> Duration {
    (grace + SHUTDOWN_MARGIN).max(MIN_SHUTDOWN_CAP)
}

/// How the supervisor runs zebrad.
#[derive(Clone, Debug)]
pub struct SupervisorConfig {
    /// The zebrad command line.
    pub command: ZebradCommand,
    /// How long a requested stop may take before zebrad is killed. Fixed for
    /// the supervisor's lifetime; [`SupervisorHandle::stop_grace`] reports it.
    pub stop_grace: Duration,
    /// After zebrad exits, how long to wait for its pipes to close before the
    /// run is classified (a grandchild could keep them open).
    pub exit_drain: Duration,
    /// How many log lines to keep for late subscribers.
    pub history_capacity: usize,
    /// Capacity of the live log broadcast; slow subscribers see a gap.
    pub broadcast_capacity: usize,
    /// When to restart after a crash.
    pub restart: RestartPolicy,
    /// Where to record the running zebrad's pid, if anywhere.
    pub pidfile: Option<PathBuf>,
    /// Where to copy zebrad's output on disk, if anywhere.
    pub log_file: Option<RotatingLogConfig>,
}

impl SupervisorConfig {
    /// Production defaults around `command`: a 60 s stop grace (zebrad's own
    /// task shutdown timeout is 20 s), 10 000 lines of history, the default
    /// restart policy, no pid file and no log file.
    pub fn new(command: ZebradCommand) -> Self {
        Self {
            command,
            stop_grace: Duration::from_secs(60),
            exit_drain: Duration::from_secs(2),
            history_capacity: 10_000,
            broadcast_capacity: 1024,
            restart: RestartPolicy::default(),
            pidfile: None,
            log_file: None,
        }
    }
}

/// Why a supervisor operation failed.
#[derive(Debug, thiserror::Error)]
pub enum SupervisorError {
    /// zebrad could not be spawned.
    #[error("could not start zebrad: {0}")]
    Spawn(#[source] io::Error),
    /// A zebrad from an earlier launcher run is still alive; see
    /// [`StartOptions::kill_stale`].
    #[error("a zebrad from an earlier launcher run is still running (pid {})", .0.pid)]
    StaleProcess(StaleProcess),
    /// zebrad exited before it finished starting.
    #[error("zebrad exited while starting ({:?}){}", .0.class, detail_suffix(.0))]
    ExitedDuringStart(ExitInfo),
    /// The supervisor is shutting down or has shut down.
    #[error("the supervisor has shut down")]
    Closed,
}

fn detail_suffix(info: &ExitInfo) -> String {
    info.detail
        .as_deref()
        .map(|detail| format!(": {detail}"))
        .unwrap_or_default()
}

/// Options for [`SupervisorHandle::start_with`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StartOptions {
    /// Terminate a zebrad left behind by an earlier launcher run before
    /// starting. Without this, such a process makes the start fail with
    /// [`SupervisorError::StaleProcess`] so the user can decide.
    pub kill_stale: bool,
}

/// A log subscription: what was logged so far, then a live feed.
#[derive(Debug)]
pub struct LogSubscription {
    /// The retained history, oldest first.
    pub history: Vec<Arc<LogLine>>,
    /// Lines logged after the snapshot. A `Lagged` error means lines were
    /// missed; fetch them with [`SupervisorHandle::history_since`].
    pub live: broadcast::Receiver<Arc<LogLine>>,
}

enum Cmd {
    Start {
        options: StartOptions,
        reply: oneshot::Sender<Result<(), SupervisorError>>,
    },
    Stop {
        reply: oneshot::Sender<()>,
    },
    Shutdown {
        reply: oneshot::Sender<()>,
    },
    Subscribe {
        reply: oneshot::Sender<LogSubscription>,
    },
    History {
        since: u64,
        reply: oneshot::Sender<Vec<Arc<LogLine>>>,
    },
}

/// A cloneable handle to the supervisor actor.
#[derive(Clone, Debug)]
pub struct SupervisorHandle {
    cmd: mpsc::Sender<Cmd>,
    status: watch::Receiver<Status>,
    stop_grace: Duration,
}

/// Starts the supervisor actor. Must be called from within a Tokio runtime.
///
/// zebrad is not started until [`SupervisorHandle::start`] is called.
pub fn spawn(config: SupervisorConfig) -> io::Result<SupervisorHandle> {
    let (cmd_tx, cmd_rx) = mpsc::channel(64);
    let (status_tx, status_rx) = watch::channel(Status {
        stop_is_graceful: config.command.stop_is_graceful(),
        ..Status::default()
    });
    let stop_grace = config.stop_grace;
    let actor = actor::Actor::new(config, cmd_rx, status_tx)?;
    tokio::spawn(actor.run());
    Ok(SupervisorHandle {
        cmd: cmd_tx,
        status: status_rx,
        stop_grace,
    })
}

impl SupervisorHandle {
    /// The current status.
    pub fn status(&self) -> Status {
        self.status.borrow().clone()
    }

    /// How long a requested stop may take before zebrad is killed
    /// ([`SupervisorConfig::stop_grace`]).
    pub fn stop_grace(&self) -> Duration {
        self.stop_grace
    }

    /// How long a caller should wait for [`Self::stop`] or [`Self::shutdown`]
    /// before giving up: [`shutdown_cap`] of the actual grace, so the cap
    /// always outlasts the escalation to a kill.
    pub fn shutdown_cap(&self) -> Duration {
        shutdown_cap(self.stop_grace)
    }

    /// A receiver that wakes on every status change.
    pub fn watch_status(&self) -> watch::Receiver<Status> {
        self.status.clone()
    }

    /// Waits until `predicate` holds for the status, checking the current
    /// value first. Fails with [`SupervisorError::Closed`] once the actor is gone.
    pub async fn wait_until(
        &self,
        mut predicate: impl FnMut(&Status) -> bool,
    ) -> Result<Status, SupervisorError> {
        let mut rx = self.status.clone();
        loop {
            {
                let status = rx.borrow_and_update();
                if predicate(&status) {
                    return Ok(status.clone());
                }
            }
            rx.changed().await.map_err(|_| SupervisorError::Closed)?;
        }
    }

    /// Starts zebrad and waits until it accepts its configuration (phase
    /// `Running`). Also cancels a pending crash restart and resets the crash
    /// history, since the user asked explicitly.
    pub async fn start(&self) -> Result<(), SupervisorError> {
        self.start_with(StartOptions::default()).await
    }

    /// [`Self::start`] with options.
    pub async fn start_with(&self, options: StartOptions) -> Result<(), SupervisorError> {
        let (reply, response) = oneshot::channel();
        self.send(Cmd::Start { options, reply }).await?;
        response.await.map_err(|_| SupervisorError::Closed)?
    }

    /// Stops zebrad (gracefully, then by force after the grace period) and
    /// waits until it has exited. Cancels a pending restart. Does nothing if
    /// zebrad is not running.
    pub async fn stop(&self) -> Result<(), SupervisorError> {
        let (reply, response) = oneshot::channel();
        self.send(Cmd::Stop { reply }).await?;
        response.await.map_err(|_| SupervisorError::Closed)
    }

    /// Stops zebrad, then starts it again.
    pub async fn restart(&self) -> Result<(), SupervisorError> {
        self.stop().await?;
        self.start().await
    }

    /// Stops zebrad and ends the actor. Further commands fail with
    /// [`SupervisorError::Closed`].
    pub async fn shutdown(&self) -> Result<(), SupervisorError> {
        let (reply, response) = oneshot::channel();
        self.send(Cmd::Shutdown { reply }).await?;
        response.await.map_err(|_| SupervisorError::Closed)
    }

    /// Takes a consistent snapshot of the log history plus a live feed.
    pub async fn subscribe(&self) -> Result<LogSubscription, SupervisorError> {
        let (reply, response) = oneshot::channel();
        self.send(Cmd::Subscribe { reply }).await?;
        response.await.map_err(|_| SupervisorError::Closed)
    }

    /// The retained lines with `seq >= since`, oldest first.
    pub async fn history_since(&self, since: u64) -> Result<Vec<Arc<LogLine>>, SupervisorError> {
        let (reply, response) = oneshot::channel();
        self.send(Cmd::History { since, reply }).await?;
        response.await.map_err(|_| SupervisorError::Closed)
    }

    async fn send(&self, cmd: Cmd) -> Result<(), SupervisorError> {
        self.cmd
            .send(cmd)
            .await
            .map_err(|_| SupervisorError::Closed)
    }
}
