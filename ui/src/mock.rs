//! A fake backend used when the page is not running inside Tauri (`trunk serve` in a browser),
//! so every page can be developed and demonstrated without zebrad.
//!
//! The fake state lives in reference-counted signals; commands change it after short delays to
//! mimic zebrad starting and stopping. Log lines are real zebrad 7.0.0 output from
//! `crates/launcher-core/tests/fixtures`.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU32, AtomicU64, Ordering},
    },
    time::Duration,
};

use launcher_types::{
    ApplyError, ApplyErrorKind, ApplyOutcome, ConfigDocument, EosInfo, ExitClass, ExitInfo,
    LogLevel, LogLine, LogStream, NodeStats, Phase, Settings, Status,
};
use leptos::prelude::*;

use crate::clock;

/// The config document the mock starts with: what the launcher's minimal managed file looks like.
const SAMPLE_CONFIG: &str = r#"# Managed by Zebra Launcher. Keys you add are kept; the RPC, metrics and
# network keys below are enforced by the launcher.

[network]
network = "Mainnet"
listen_addr = "[::]:8233"

[rpc]
listen_addr = "127.0.0.1:8232"
enable_cookie_auth = true

[metrics]
endpoint_addr = "127.0.0.1:9999"

[state]
cache_dir = "/home/user/.local/share/org.zfnd.zebra-launcher/zebra"

[tracing]
use_color = false
"#;

/// The text that makes the mock reject a config, and the error zebrad 7.0.0 prints for it.
const BAD_KEY: &str = "bogus";
const CONFIG_ERROR: &str = "error: Zebra could not load the provided configuration file and/or environment variables.\nerror: zebrad fatal error: I/O operation failed: Configuration error: unknown field `bogus`, expected one of `listen_addr`, `indexer_listen_addr`, `lightwalletd_listen_addr`, `parallel_cpu_threads`, `debug_force_finished_sync`, `cookie_dir`, `enable_cookie_auth`, `max_response_body_size` for key `rpc`";

/// A config containing this comment makes the mock report that the file changed on disk.
const CONFLICT_MARKER: &str = "# simulate-conflict";

/// `(stream, level, spans, target, message)` taken from real zebrad output.
type SampleLine = (
    LogStream,
    Option<LogLevel>,
    Option<&'static str>,
    Option<&'static str>,
    &'static str,
);

const SAMPLE_LINES: &[SampleLine] = &[
    (
        LogStream::Stdout,
        Some(LogLevel::Info),
        None,
        Some("zebrad::components::tracing::component"),
        r#"started tracing component filter="info" TRACING_STATIC_MAX_LEVEL=LevelFilter::INFO LOG_STATIC_MAX_LEVEL=Info"#,
    ),
    (
        LogStream::Stdout,
        Some(LogLevel::Info),
        None,
        Some("zebrad::application"),
        "Diagnostic metadata:",
    ),
    (
        LogStream::Stdout,
        None,
        None,
        None,
        "version: 7.0.0-rc.0+192.gad6528b.modified",
    ),
    (
        LogStream::Stdout,
        None,
        None,
        None,
        "Zcash network: Regtest",
    ),
    (
        LogStream::Stdout,
        Some(LogLevel::Info),
        Some(r#"{zebrad="ad6528b" net="Regtest"}"#),
        Some("zebrad::components::metrics"),
        "Opened metrics endpoint at 127.0.0.1:19999",
    ),
    (
        LogStream::Stdout,
        Some(LogLevel::Info),
        None,
        Some("zebrad::commands::start"),
        "Starting zebrad",
    ),
    (
        LogStream::Stdout,
        Some(LogLevel::Warn),
        None,
        Some("zebrad::commands::start"),
        "TCP slow-start-after-idle is enabled, which resets TCP's congestion window between block requests and significantly reduces single-peer throughput for block propagation. Hint: set `net.ipv4.tcp_slow_start_after_idle=0` via sysctl.",
    ),
    (
        LogStream::Stdout,
        Some(LogLevel::Info),
        None,
        Some("zebra_state::service::finalized_state::disk_db"),
        "Opened Zebra state cache at <DATA_DIR>/state/state/v29/regtest",
    ),
    (
        LogStream::Stdout,
        Some(LogLevel::Info),
        Some("open_listener{addr=127.0.0.1:0}"),
        Some("zebra_network::peer_set::initialize"),
        "Opened Zcash protocol endpoint at 127.0.0.1:46417",
    ),
    (
        LogStream::Stdout,
        Some(LogLevel::Warn),
        Some("add_initial_peers"),
        Some("zebra_network::config"),
        "no initial peers in the network config. Hint: you must configure at least one peer IP or DNS seeder to run Zebra",
    ),
    (
        LogStream::Stdout,
        Some(LogLevel::Info),
        Some("add_initial_peers"),
        Some("zebra_network::peer_set::initialize"),
        "connecting to initial peer set ipv4_peer_count=0 ipv6_peer_count=0",
    ),
    (
        LogStream::Stdout,
        Some(LogLevel::Warn),
        None,
        Some("zebrad::components::sync::progress"),
        "initial sync can't download and verify the genesis block. Hint: check your network connection, and your computer clock and time zone sync_percent=0.000 % current_height=None",
    ),
    (
        LogStream::Stdout,
        Some(LogLevel::Info),
        None,
        Some("zebra_state::service::finalized_state::disk_format::upgrade"),
        "checking database format produced by new blocks in this instance is valid running_version=29.0.0",
    ),
    (
        LogStream::Stderr,
        None,
        None,
        None,
        "thread 'main' (210466) panicked at zebrad/src/components/metrics.rs:38:27:",
    ),
    (
        LogStream::Stdout,
        Some(LogLevel::Error),
        None,
        Some("zebrad::components::sync"),
        "(mock) an ERROR line, to show the colour",
    ),
    (
        LogStream::Stdout,
        Some(LogLevel::Debug),
        None,
        Some("zebra_network::peer_set"),
        "(mock) a DEBUG line, to show the colour",
    ),
    (
        LogStream::Stdout,
        Some(LogLevel::Trace),
        None,
        Some("zebra_network::peer_set"),
        "(mock) a TRACE line, to show the colour",
    ),
    (
        LogStream::Launcher,
        None,
        None,
        None,
        "(mock) a note from the launcher, such as \"zebrad exited with code 0\"",
    ),
    (
        LogStream::Stdout,
        Some(LogLevel::Info),
        None,
        Some("zebra_state::service"),
        "waiting for the block write task to finish",
    ),
];

#[derive(Clone)]
struct Mock {
    status: ArcRwSignal<Status>,
    stats: ArcRwSignal<Option<NodeStats>>,
    settings: ArcRwSignal<Settings>,
    config: ArcRwSignal<ConfigDocument>,
    /// The next log sequence number.
    next_seq: Arc<AtomicU64>,
    /// Bumped by every command, so a delayed transition from an older command is skipped.
    generation: Arc<AtomicU64>,
    next_pid: Arc<AtomicU32>,
}

thread_local! {
    static MOCK: Mock = Mock::new();
}

fn mock() -> Mock {
    MOCK.with(Mock::clone)
}

impl Mock {
    fn new() -> Self {
        let mock = Self {
            status: ArcRwSignal::new(Status::default()),
            stats: ArcRwSignal::new(None),
            settings: ArcRwSignal::new(Settings {
                data_dir: Some("/home/user/.local/share/org.zfnd.zebra-launcher".into()),
                ..Settings::default()
            }),
            config: ArcRwSignal::new(ConfigDocument {
                text: SAMPLE_CONFIG.into(),
                token: "mock-1".into(),
                path: "/home/user/.local/share/org.zfnd.zebra-launcher/zebrad.toml".into(),
            }),
            next_seq: Arc::new(AtomicU64::new(0)),
            generation: Arc::new(AtomicU64::new(0)),
            next_pid: Arc::new(AtomicU32::new(4242)),
        };
        mock.start();
        let ticker = mock.clone();
        set_interval(move || ticker.tick_stats(), Duration::from_secs(1));
        mock
    }

    fn phase(&self) -> Phase {
        self.status.with_untracked(|status| status.phase.clone())
    }

    /// Runs `step` after `delay` unless another command was issued in between.
    fn after(&self, delay: Duration, step: impl FnOnce(&Mock) + 'static) {
        let generation = self.generation.load(Ordering::Relaxed);
        let mock = self.clone();
        set_timeout(
            move || {
                if mock.generation.load(Ordering::Relaxed) == generation {
                    step(&mock);
                }
            },
            delay,
        );
    }

    fn start(&self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
        let pid = self.next_pid.fetch_add(1, Ordering::Relaxed);
        self.status.update(|status| {
            status.phase = Phase::Starting;
            status.pid = Some(pid);
            status.started_at = Some(clock::now());
            status.rpc_addr = None;
            status.metrics_addr = None;
        });
        self.after(Duration::from_millis(2500), |mock| {
            mock.status.update(|status| {
                status.phase = Phase::Running;
                status.rpc_addr = "127.0.0.1:8232".parse().ok();
                status.metrics_addr = "127.0.0.1:9999".parse().ok();
            });
        });
    }

    fn stop(&self, then_start: bool) {
        self.generation.fetch_add(1, Ordering::Relaxed);
        self.status.update(|status| status.phase = Phase::Stopping);
        self.after(Duration::from_millis(1500), move |mock| {
            mock.status.update(|status| {
                status.phase = Phase::Stopped;
                status.pid = None;
                status.started_at = None;
                status.rpc_addr = None;
                status.metrics_addr = None;
                status.last_exit = Some(ExitInfo {
                    code: Some(0),
                    class: ExitClass::Clean,
                    detail: None,
                    at: clock::now(),
                    requested: true,
                });
            });
            mock.stats.set(None);
            if then_start {
                mock.start();
            }
        });
    }

    fn tick_stats(&self) {
        if self.phase() != Phase::Running {
            return;
        }
        let jitter = (js_sys::Math::random() * 3.0) as u64;
        let mut stats = self.stats.get_untracked().unwrap_or_else(|| NodeStats {
            height: 2_950_000,
            estimated_height: 3_120_000,
            verification_progress: 0.0,
            peers: 8,
            mempool_transactions: 0,
            chain: "main".into(),
            zebrad_version: "7.0.0".into(),
            last_error: Some("(mock) no initial peers in the network config".into()),
            eos: Some(EosInfo {
                block_height: 3_160_000,
                estimated_time: clock::now() + Duration::from_secs(41 * 24 * 3600),
            }),
            updated_at: clock::now(),
        });
        stats.height = (stats.height + 40 + jitter * 20).min(stats.estimated_height);
        stats.estimated_height += jitter / 2;
        stats.verification_progress = stats.height as f64 / stats.estimated_height as f64;
        stats.peers = 6 + jitter as u32 * 3;
        stats.mempool_transactions = 10 + jitter * 7;
        stats.updated_at = clock::now();
        self.stats.set(Some(stats));
    }

    fn line(&self, seq: u64) -> LogLine {
        let (stream, level, spans, target, message) =
            SAMPLE_LINES[(seq as usize) % SAMPLE_LINES.len()];
        let has_prefix = level.is_some();
        LogLine {
            seq,
            stream,
            timestamp: has_prefix.then(clock::now_iso),
            level,
            spans: spans.map(Into::into),
            target: target.map(Into::into),
            message: message.into(),
        }
    }
}

/// Waits for `millis` milliseconds.
async fn sleep(millis: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        if let Some(window) = web_sys::window() {
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, millis);
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

/// Calls `handler` with every status change, starting with the current one.
pub fn on_status(handler: impl Fn(Status) + 'static) {
    let status = mock().status;
    Effect::new(move |_| handler(status.get()));
}

/// Calls `handler` with every node stats change, starting with the current value.
pub fn on_node_stats(handler: impl Fn(Option<NodeStats>) + 'static) {
    let stats = mock().stats;
    Effect::new(move |_| handler(stats.get()));
}

pub async fn get_status() -> Status {
    sleep(50).await;
    mock().status.get_untracked()
}

pub async fn start_zebrad() {
    sleep(100).await;
    let mock = mock();
    if matches!(
        mock.phase(),
        Phase::Stopped | Phase::Failed { .. } | Phase::Backoff { .. }
    ) {
        mock.start();
    }
}

pub async fn stop_zebrad() {
    sleep(100).await;
    mock().stop(false);
}

pub async fn restart_zebrad() {
    sleep(100).await;
    mock().stop(true);
}

/// Streams batches of sample lines while zebrad is "running", skipping a few sequence numbers
/// now and then to show the dropped-lines marker.
pub fn subscribe_logs(on_batch: impl Fn(Vec<LogLine>) + 'static) {
    let mock = mock();
    let batches = AtomicU64::new(0);
    set_interval(
        move || {
            if matches!(mock.phase(), Phase::Stopped | Phase::Failed { .. }) {
                return;
            }
            let batch_number = batches.fetch_add(1, Ordering::Relaxed);
            if batch_number % 60 == 59 {
                mock.next_seq.fetch_add(7, Ordering::Relaxed);
            }
            let count = 1 + (js_sys::Math::random() * 3.0) as u64;
            let first = mock.next_seq.fetch_add(count, Ordering::Relaxed);
            on_batch((first..first + count).map(|seq| mock.line(seq)).collect());
        },
        Duration::from_millis(400),
    );
}

pub async fn log_history(since: u64) -> Vec<LogLine> {
    sleep(50).await;
    let mock = mock();
    let end = mock.next_seq.load(Ordering::Relaxed);
    (since.max(end.saturating_sub(200))..end)
        .map(|seq| mock.line(seq))
        .collect()
}

pub async fn read_config() -> ConfigDocument {
    sleep(150).await;
    mock().config.get_untracked()
}

pub async fn apply_config(text: String, token: String) -> Result<ApplyOutcome, ApplyError> {
    sleep(300).await;
    let mock = mock();
    let current = mock.config.get_untracked();
    if token != current.token || text.contains(CONFLICT_MARKER) {
        return Err(ApplyError {
            kind: ApplyErrorKind::Conflict,
            message: format!("{} changed on disk since it was read", current.path),
        });
    }
    mock.stop(true);
    sleep(2000).await;
    if text.contains(BAD_KEY) {
        return Ok(ApplyOutcome::RolledBack {
            error: CONFIG_ERROR.into(),
        });
    }
    mock.config.update(|config| {
        config.text = text;
        config.token = format!("mock-{}", js_sys::Date::now());
    });
    Ok(ApplyOutcome::Applied)
}

pub async fn get_settings() -> Settings {
    sleep(50).await;
    mock().settings.get_untracked()
}

pub async fn set_settings(settings: Settings) -> Settings {
    sleep(200).await;
    let mock = mock();
    // The real backend owns these; the UI only displays them.
    let stored = Settings {
        data_dir: mock.settings.with_untracked(|s| s.data_dir.clone()),
        schema_version: mock.settings.with_untracked(|s| s.schema_version),
        ..settings
    };
    mock.settings.set(stored.clone());
    stored
}

pub async fn get_node_stats() -> Option<NodeStats> {
    sleep(50).await;
    mock().stats.get_untracked()
}
