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
    /// The process could not be started at all, for example because the binary is missing.
    SpawnFailed,
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
                | ExitClass::SpawnFailed
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

/// The Zcash network zebrad runs on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Network {
    /// The main network.
    #[default]
    Mainnet,
    /// The public test network.
    Testnet,
}

impl Network {
    /// The name zebrad uses in `network.network`.
    pub fn as_str(self) -> &'static str {
        match self {
            Network::Mainnet => "Mainnet",
            Network::Testnet => "Testnet",
        }
    }
}

impl fmt::Display for Network {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How the launcher handles its own updates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateMode {
    /// Never check for updates.
    #[default]
    Off,
    /// Check and tell the user, but do not install.
    Notify,
    /// Check, download and install.
    Auto,
}

/// The launcher's own settings, stored in `launcher.json`.
///
/// Missing fields take their default, so older files keep loading.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The file format version, for future migrations.
    pub schema_version: u32,
    /// How launcher updates are handled.
    pub update_mode: UpdateMode,
    /// Closing the window hides it to the tray instead of quitting.
    pub close_to_tray: bool,
    /// Start zebrad when the launcher starts.
    pub start_node_on_launch: bool,
    /// The Grafana dashboard URL to embed. Must be loopback `http(s)`, see
    /// [`is_loopback_http_url`].
    pub grafana_url: Option<String>,
    /// The directory chosen for the chain state, if not the default.
    pub data_dir: Option<String>,
    /// The network zebrad runs on.
    pub network: Network,
    /// How long a graceful stop may take before zebrad is killed, in seconds.
    pub stop_grace_secs: u64,
}

impl Settings {
    /// The current [`Settings::schema_version`].
    pub const SCHEMA_VERSION: u32 = 1;
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            update_mode: UpdateMode::Off,
            close_to_tray: true,
            start_node_on_launch: true,
            grafana_url: None,
            data_dir: None,
            network: Network::Mainnet,
            stop_grace_secs: 60,
        }
    }
}

/// Returns true when `url` is an `http://` or `https://` URL whose host is
/// `127.0.0.1`, `localhost` or `[::1]`, with an optional port and path, and no
/// user info.
///
/// This is the rule for [`Settings::grafana_url`]: the app's content security
/// policy only allows loopback frames.
pub fn is_loopback_http_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    let Some(rest) = lower
        .strip_prefix("http://")
        .or_else(|| lower.strip_prefix("https://"))
    else {
        return false;
    };
    // User info (`127.0.0.1@evil.example`), backslashes and whitespace are
    // refused outright: browsers parse them in surprising ways.
    if rest.contains(['\\', ' ', '\t', '\n', '\r']) {
        return false;
    }
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    if authority.contains('@') {
        return false;
    }
    let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
        match v6.split_once(']') {
            Some(parts) => parts,
            None => return false,
        }
    } else {
        match authority.find(':') {
            Some(i) => authority.split_at(i),
            None => (authority, ""),
        }
    };
    let port_ok = match port.strip_prefix(':') {
        Some(digits) => {
            !digits.is_empty()
                && digits.bytes().all(|b| b.is_ascii_digit())
                && digits.parse::<u16>().is_ok()
        }
        None => port.is_empty(),
    };
    port_ok && matches!(host, "127.0.0.1" | "localhost" | "::1")
}

/// Node statistics polled from zebrad's JSON-RPC interface.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NodeStats {
    /// The height of the best verified block.
    pub height: u64,
    /// zebrad's estimate of the network chain tip height.
    pub estimated_height: u64,
    /// Sync progress from 0.0 to 1.0.
    pub verification_progress: f64,
    /// Connected peers.
    pub peers: u32,
    /// Transactions in the mempool.
    pub mempool_transactions: u64,
    /// The chain name zebrad reports, such as `main` or `test`.
    pub chain: String,
    /// The zebrad version, from `getinfo`.
    pub zebrad_version: String,
    /// The last warning or error: the RPC error that prevented the last poll,
    /// or else zebrad's own `getinfo.errors` when it is not empty.
    pub last_error: Option<String>,
    /// When this release stops running, on Mainnet.
    pub eos: Option<EosInfo>,
    /// When these stats were last refreshed.
    pub updated_at: SystemTime,
}

/// When the running zebrad release reaches its end of support and halts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EosInfo {
    /// The block height at which zebrad halts.
    pub block_height: u64,
    /// zebrad's estimate of when that block is reached.
    pub estimated_time: SystemTime,
}

/// The launcher-managed `zebrad.toml`, as shown in the editor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigDocument {
    /// The file content.
    pub text: String,
    /// A compare-and-swap token: pass it back when saving, so an edit made by
    /// someone else in the meantime is detected instead of overwritten.
    pub token: String,
    /// The file's path, for display.
    pub path: String,
}

/// The result of saving an edited `zebrad.toml` (`apply_config`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ApplyOutcome {
    /// zebrad started with the new file, which is now kept.
    Applied,
    /// zebrad refused the new file: the previous file was restored and
    /// zebrad restarted with it.
    RolledBack {
        /// zebrad's description of what is wrong with the new file.
        error: String,
    },
}

/// Why an edited `zebrad.toml` was not saved at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplyErrorKind {
    /// The text is not valid TOML.
    InvalidToml,
    /// A key has a value the launcher does not allow, such as a non-loopback
    /// RPC address.
    Forbidden,
    /// The file changed since it was read; reload it and edit again.
    Conflict,
    /// Another configuration change is still being applied.
    ApplyInProgress,
    /// Reading or writing the file failed.
    Io,
}

/// The rejection returned by `apply_config` when the new file was not saved.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyError {
    /// What kind of problem it is.
    pub kind: ApplyErrorKind,
    /// A message for the user.
    pub message: String,
}

impl fmt::Display for ApplyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ApplyError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_payload_shapes() {
        assert_eq!(
            serde_json::to_value(ApplyOutcome::Applied).unwrap(),
            serde_json::json!({"kind": "applied"})
        );
        assert_eq!(
            serde_json::to_value(ApplyOutcome::RolledBack {
                error: "bad key".into()
            })
            .unwrap(),
            serde_json::json!({"kind": "rolled_back", "error": "bad key"})
        );
        assert_eq!(
            serde_json::to_value(ApplyError {
                kind: ApplyErrorKind::ApplyInProgress,
                message: "busy".into()
            })
            .unwrap(),
            serde_json::json!({"kind": "apply_in_progress", "message": "busy"})
        );
    }

    #[test]
    fn loopback_urls() {
        for ok in [
            "http://127.0.0.1:3000/d/zebra-overview?kiosk",
            "https://localhost",
            "http://localhost:3000",
            "HTTP://LOCALHOST:3000/",
            "http://[::1]:3000/x#y",
            "http://127.0.0.1?x",
        ] {
            assert!(is_loopback_http_url(ok), "{ok}");
        }
        for bad in [
            "",
            "ftp://127.0.0.1",
            "javascript:alert(1)",
            "http://example.com",
            "http://127.0.0.1.example.com",
            "http://127.0.0.1@example.com",
            "http://example.com@127.0.0.1",
            "http://localhost:",
            "http://localhost:99999",
            "http://localhost:+80",
            "http://localhost:3000x",
            "http://[::1",
            "http://[::2]:3000",
            "http://0.0.0.0:3000",
            "http://127.0.0.1\\@example.com",
            "http://127.0.0.1 /",
            "//127.0.0.1",
        ] {
            assert!(!is_loopback_http_url(bad), "{bad}");
        }
    }

    #[test]
    fn settings_defaults_fill_missing_fields() {
        let settings: Settings =
            serde_json::from_str(r#"{"network":"testnet"}"#).expect("partial settings parse");
        assert_eq!(
            settings,
            Settings {
                network: Network::Testnet,
                ..Settings::default()
            }
        );
    }
}
