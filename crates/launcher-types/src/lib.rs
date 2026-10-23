//! Types shared by the Zebra Launcher backend and its UI.
//!
//! This crate is compiled both natively and to WebAssembly, so it stays free of
//! async runtimes, Tauri, and platform-specific code.

#![forbid(unsafe_code)]

use std::{fmt, net::SocketAddr, str::FromStr, time::SystemTime};

use serde::{Deserialize, Serialize};

/// Where a log line came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogStream {
    /// zebrad's stdout.
    Stdout,
    /// zebrad's stderr.
    Stderr,
    /// A note added by the launcher itself, such as "zebrad exited".
    Launcher,
}

/// A `tracing` log level as zebrad prints it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    /// The upper-case name zebrad prints.
    pub fn as_str(self) -> &'static str {
        match self {
            LogLevel::Trace => "TRACE",
            LogLevel::Debug => "DEBUG",
            LogLevel::Info => "INFO",
            LogLevel::Warn => "WARN",
            LogLevel::Error => "ERROR",
        }
    }
}

impl fmt::Display for LogLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The error returned when a string is not one of the five `tracing` levels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownLogLevel(pub String);

impl fmt::Display for UnknownLogLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown log level {:?}", self.0)
    }
}

impl std::error::Error for UnknownLogLevel {}

impl FromStr for LogLevel {
    type Err = UnknownLogLevel;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "TRACE" => Ok(LogLevel::Trace),
            "DEBUG" => Ok(LogLevel::Debug),
            "INFO" => Ok(LogLevel::Info),
            "WARN" => Ok(LogLevel::Warn),
            "ERROR" => Ok(LogLevel::Error),
            other => Err(UnknownLogLevel(other.to_string())),
        }
    }
}

/// One line of zebrad output, parsed where possible.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogLine {
    /// Sequence number assigned by the supervisor. A gap means lines were dropped.
    pub seq: u64,
    /// The pipe the line was read from.
    pub stream: LogStream,
    /// The RFC 3339 UTC timestamp as zebrad printed it, if the line had one.
    pub timestamp: Option<String>,
    /// The log level, if the line had one.
    pub level: Option<LogLevel>,
    /// The `tracing` span context, such as `sig{kind=SignalKind(15) name="SIGTERM"}`, if any.
    pub spans: Option<String>,
    /// The `tracing` target, such as `zebrad::signal`, if any.
    pub target: Option<String>,
    /// The message and its fields. For lines without a timestamp and level,
    /// such as continuation lines of a multi-line message, this is the whole line.
    pub message: String,
}

/// A notable event detected in zebrad output.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LogEvent {
    /// zebrad's tracing component is up: the first line of every run.
    TracingStarted,
    /// The Prometheus metrics listener is bound.
    MetricsOpened { addr: SocketAddr },
    /// The Zcash P2P listener is bound.
    P2pOpened { addr: SocketAddr },
    /// The RPC auth cookie file has been written.
    RpcCookieWritten,
    /// The JSON-RPC listener is bound.
    RpcOpened { addr: SocketAddr },
    /// `Starting zebrad`: the configuration loaded and the components initialised.
    StartingZebrad,
    /// A shutdown was requested; `reason` is `SIGTERM`, `SIGINT`, `Ctrl-C`, or similar.
    ShutdownRequested { reason: String },
    /// Tasks are being shut down.
    ShuttingDown,
    /// `stopping zebrad`: the last line of a graceful shutdown.
    Stopped,
    /// End-of-support warning: this release halts soon.
    EosWarn { message: String },
    /// End-of-support halt: this release is too old and refuses to run.
    EosHalt { message: String },
    /// The configuration file or environment could not be loaded.
    ConfigError,
    /// `zebrad fatal error: ...`, with the detail that follows it.
    FatalError { message: String },
    /// Another process holds the state database lock.
    DbLocked { message: String },
    /// A listener address is already in use.
    PortInUse { message: String },
    /// Any other panic.
    Panic { message: String },
}

/// Why a zebrad run ended, derived from its exit status and the detected events.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitClass {
    /// Exit code 0 after a requested shutdown.
    Clean,
    /// The configuration could not be loaded.
    ConfigError,
    /// The release is past its end of support.
    EosHalt,
    /// Another process holds the state database lock.
    DbLocked,
    /// A listener address is already in use.
    PortInUse,
    /// An unexpected exit: a non-zero code, a signal, or a panic with no known cause.
    Crash,
}

impl ExitClass {
    /// Returns true when restarting without a change cannot help.
    pub fn is_deterministic(self) -> bool {
        matches!(
            self,
            ExitClass::ConfigError
                | ExitClass::EosHalt
                | ExitClass::DbLocked
                | ExitClass::PortInUse
        )
    }
}

/// What the supervisor is doing with zebrad.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum Phase {
    /// No zebrad is running and none is scheduled.
    Stopped,
    /// zebrad was spawned and is loading its configuration.
    Starting,
    /// zebrad accepted its configuration and is running.
    Running,
    /// A stop was requested; waiting for zebrad to exit.
    Stopping,
    /// zebrad crashed; a restart is scheduled.
    Backoff {
        /// When the restart happens.
        until: SystemTime,
        /// How many crashes happened in the current window.
        crashes: u32,
    },
    /// zebrad will not be restarted until the user intervenes.
    Failed {
        /// Why.
        class: ExitClass,
    },
}

/// How the last zebrad run ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitInfo {
    /// The exit code, or `None` when zebrad was killed by a signal.
    pub code: Option<i32>,
    /// The classification derived from the exit code and the logged events.
    pub class: ExitClass,
    /// zebrad's own description of a fatal error or panic, if it logged one.
    pub detail: Option<String>,
    /// When the exit was observed.
    pub at: SystemTime,
    /// Whether the launcher asked zebrad to stop.
    pub requested: bool,
}

/// A zebrad process left behind by an earlier launcher run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StaleProcessInfo {
    /// Its process id.
    pub pid: u32,
    /// The zebrad binary it was started from, for display.
    pub binary: String,
}

/// The supervisor's view of zebrad, published on a `watch` channel.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    /// The current phase.
    pub phase: Phase,
    /// The pid of the running zebrad, while there is one.
    pub pid: Option<u32>,
    /// When the current run was spawned.
    pub started_at: Option<SystemTime>,
    /// The RPC address zebrad reported as opened in this run.
    pub rpc_addr: Option<SocketAddr>,
    /// The metrics address zebrad reported as opened in this run.
    pub metrics_addr: Option<SocketAddr>,
    /// How the previous run ended.
    pub last_exit: Option<ExitInfo>,
    /// A zebrad from an earlier launcher run that is still alive, if detected.
    pub stale: Option<StaleProcessInfo>,
}

impl Default for Status {
    fn default() -> Self {
        Self {
            phase: Phase::Stopped,
            pid: None,
            started_at: None,
            rpc_addr: None,
            metrics_addr: None,
            last_exit: None,
            stale: None,
        }
    }
}
