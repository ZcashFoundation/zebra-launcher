//! A fake backend used when the page is not running inside Tauri (`trunk serve` in a browser),
//! so every page can be developed and demonstrated without zebrad.
//!
//! The fake state lives in reference-counted signals; commands change it after short delays to
//! mimic zebrad starting and stopping. Log lines are real zebrad 7.0.0 output from
//! `crates/launcher-core/tests/fixtures`.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
    time::Duration,
};

use launcher_types::{
    ApplyError, ApplyErrorKind, ApplyOutcome, ConfigDocument, DataDirChoice, EosInfo, ExitClass,
    ExitInfo, FirstRunInfo, ImportOutcome, LegacyInstall, LogLevel, LogLine, LogStream, NodeStats,
    Phase, Settings, StaleProcessInfo, Status,
};
use leptos::prelude::*;

use crate::{clock, format};

/// The launcher's own data directory in the mock.
const DEFAULT_DATA_DIR: &str = "/home/user/.local/share/org.zfnd.zebra-launcher";

/// What the mock folder picker returns.
const PICKED_DATA_DIR: &str = "/mnt/storage/zebra";

/// Free space at [`DEFAULT_DATA_DIR`]: below the 300 GB warning, so the warning shows.
const FREE_AT_DEFAULT: u64 = 212_400_000_000;

/// Free space at [`PICKED_DATA_DIR`].
const FREE_AT_PICKED: u64 = 1_834_500_000_000;

/// The size of the chain state outside first-run mode.
const SYNCED_STATE_BYTES: u64 = 248_300_000_000;

/// The alpha.4 leftovers `?firstrun` simulates.
fn sample_legacy() -> LegacyInstall {
    LegacyInstall {
        config_file: Some("/opt/zebra.app/zebrad.toml".into()),
        state_dir: Some("/home/user/.cache/zebra".into()),
        state_bytes: 96_700_000_000,
        state_bytes_is_lower_bound: true,
    }
}

/// True when the page URL's query contains `flag`, such as `?stale` or `?firstrun`.
fn url_flag(flag: &str) -> bool {
    web_sys::window()
        .and_then(|window| window.location().search().ok())
        .is_some_and(|search| search.contains(flag))
}

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

/// A config containing this comment makes the mock fail the next `read_config`.
const READ_FAILURE_MARKER: &str = "# simulate-read-failure";

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
    /// Makes the next `read_config` fail, see [`READ_FAILURE_MARKER`].
    fail_next_read: Arc<AtomicBool>,
    /// The alpha.4 leftovers, until they are moved.
    legacy: ArcRwSignal<Option<LegacyInstall>>,
    /// The size of the chain state, freed by `delete_node_data`.
    state_bytes: Arc<AtomicU64>,
}

thread_local! {
    static MOCK: Mock = Mock::new();
}

fn mock() -> Mock {
    MOCK.with(Mock::clone)
}

impl Mock {
    fn new() -> Self {
        // `?firstrun` in the page URL shows the first-run flow, with alpha.4 leftovers to move.
        // Like the real backend, zebrad is then not started until the flow is finished.
        let first_run = url_flag("firstrun");
        let mock = Self {
            status: ArcRwSignal::new(Status::default()),
            stats: ArcRwSignal::new(None),
            settings: ArcRwSignal::new(Settings {
                first_run_done: !first_run,
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
            fail_next_read: Arc::new(AtomicBool::new(false)),
            legacy: ArcRwSignal::new(first_run.then(sample_legacy)),
            state_bytes: Arc::new(AtomicU64::new(if first_run {
                0
            } else {
                SYNCED_STATE_BYTES
            })),
        };
        // `?stale` in the page URL simulates a zebrad left behind by an earlier launcher run.
        if url_flag("stale") {
            mock.status.update(|status| {
                status.stale = Some(StaleProcessInfo {
                    pid: 1234,
                    binary: "/opt/zebra-launcher/zebrad".into(),
                })
            });
        } else if !first_run {
            mock.start();
        }
        let ticker = mock.clone();
        set_interval(move || ticker.tick_stats(), Duration::from_secs(1));
        mock
    }

    fn phase(&self) -> Phase {
        self.status.with_untracked(|status| status.phase.clone())
    }

    /// Like the backend's guard for data changes, with its message.
    fn require_stopped(&self) -> Result<(), String> {
        if format::data_change_allowed(&self.phase()) {
            Ok(())
        } else {
            Err("stop zebrad first".into())
        }
    }

    /// The current state directory, as the backend derives it.
    fn state_dir(&self) -> String {
        let data_dir = self
            .settings
            .with_untracked(|settings| settings.data_dir.clone())
            .unwrap_or_else(|| DEFAULT_DATA_DIR.into());
        format::state_dir(&data_dir)
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

pub async fn start_zebrad() -> Result<(), String> {
    sleep(100).await;
    let mock = mock();
    if let Some(stale) = mock.status.with_untracked(|status| status.stale.clone()) {
        return Err(format!(
            "a zebrad from an earlier launcher run is still running (pid {})",
            stale.pid
        ));
    }
    if matches!(
        mock.phase(),
        Phase::Stopped | Phase::Failed { .. } | Phase::Backoff { .. }
    ) {
        mock.start();
    }
    Ok(())
}

pub async fn start_zebrad_killing_stale() {
    sleep(500).await;
    let mock = mock();
    mock.status.update(|status| status.stale = None);
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
            let mut batch = Vec::new();
            if batch_number % 60 == 59 {
                // Like the bridge after lagging: the lost lines are replaced by one launcher
                // line that takes the seq of the last of them.
                const DROPPED: u64 = 7;
                let first_dropped = mock.next_seq.fetch_add(DROPPED, Ordering::Relaxed);
                batch.push(LogLine {
                    seq: first_dropped + DROPPED - 1,
                    stream: LogStream::Launcher,
                    timestamp: None,
                    level: Some(LogLevel::Warn),
                    spans: None,
                    target: None,
                    message: format!("{DROPPED} lines dropped"),
                });
            }
            let count = 1 + (js_sys::Math::random() * 3.0) as u64;
            let first = mock.next_seq.fetch_add(count, Ordering::Relaxed);
            batch.extend((first..first + count).map(|seq| mock.line(seq)));
            on_batch(batch);
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

pub async fn read_config() -> Result<ConfigDocument, String> {
    sleep(150).await;
    let mock = mock();
    if mock.fail_next_read.swap(false, Ordering::Relaxed) {
        return Err("(mock) permission denied reading zebrad.toml".into());
    }
    Ok(mock.config.get_untracked())
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
    mock.fail_next_read
        .store(text.contains(READ_FAILURE_MARKER), Ordering::Relaxed);
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
    // Like the real backend: `first_run_done` is only set by `complete_first_run`, while
    // `data_dir` is saved as sent, so the UI must not send a stale one.
    let stored = Settings {
        first_run_done: mock.settings.with_untracked(|s| s.first_run_done),
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

pub async fn get_first_run_info() -> FirstRunInfo {
    // The real backend sizes the leftovers here, which takes a moment.
    sleep(600).await;
    let mock = mock();
    FirstRunInfo {
        is_first_run: !mock.settings.with_untracked(|s| s.first_run_done),
        default_data_dir: DEFAULT_DATA_DIR.into(),
        free_bytes_at_default: Some(FREE_AT_DEFAULT),
        legacy: mock.legacy.get_untracked(),
    }
}

pub async fn pick_data_dir() -> Option<String> {
    sleep(300).await;
    Some(PICKED_DATA_DIR.into())
}

pub async fn choose_data_dir(path: String) -> Result<DataDirChoice, String> {
    sleep(200).await;
    let mock = mock();
    mock.require_stopped()?;
    let chosen = path.trim().trim_end_matches('/').to_string();
    if chosen.is_empty() {
        return Err("cannot use \"\" as the data directory: no such directory".into());
    }
    let is_default = chosen == DEFAULT_DATA_DIR;
    mock.settings
        .update(|settings| settings.data_dir = (!is_default).then_some(chosen));
    Ok(DataDirChoice {
        settings: mock.settings.get_untracked(),
        free_bytes: Some(if is_default {
            FREE_AT_DEFAULT
        } else {
            FREE_AT_PICKED
        }),
    })
}

pub async fn import_legacy_state() -> Result<ImportOutcome, String> {
    sleep(800).await;
    let mock = mock();
    mock.require_stopped()?;
    let Some(LegacyInstall {
        state_dir: Some(state_dir),
        state_bytes,
        ..
    }) = mock.legacy.get_untracked()
    else {
        return Err("no alpha.4 chain state was found".into());
    };
    mock.legacy.set(None);
    mock.state_bytes.fetch_add(state_bytes, Ordering::Relaxed);
    Ok(ImportOutcome {
        from: format!("{state_dir}/state"),
        to: mock.state_dir(),
    })
}

pub async fn delete_node_data() -> Result<u64, String> {
    sleep(700).await;
    let mock = mock();
    mock.require_stopped()?;
    Ok(mock.state_bytes.swap(0, Ordering::Relaxed))
}

pub async fn copy_diagnostics() -> String {
    sleep(200).await;
    let mock = mock();
    let phase = mock.phase();
    let settings = mock.settings.get_untracked();
    let user_agent = web_sys::window()
        .and_then(|window| window.navigator().user_agent().ok())
        .unwrap_or_default();
    let config = mock.config.with_untracked(|config| config.text.clone());
    format!(
        "Zebra Launcher 0.1.0 (zebrad 7.0.0) [mock]\n\
         OS: {user_agent}\n\
         Phase: {phase:?}\n\
         Settings: {settings:?}\n\
         \n\
         --- zebrad.toml ---\n\
         {config}\n\
         --- launcher log (last lines) ---\n\
         (mock) INFO zebra_launcher: started\n\
         --- zebrad log (last lines) ---\n\
         (mock) INFO zebrad::commands::start: Starting zebrad\n"
    )
}

pub async fn complete_first_run() {
    sleep(200).await;
    mock()
        .settings
        .update(|settings| settings.first_run_done = true);
}

pub async fn open_path(target: &str) {
    sleep(50).await;
    leptos::logging::log!("(mock) open_path({target})");
}
