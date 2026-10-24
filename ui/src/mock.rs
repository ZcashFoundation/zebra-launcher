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
    ExitInfo, FirstRunInfo, ImportOutcome, LegacyInstall, LogLevel, LogLine, LogStream, Network,
    NodeStats, Phase, RecoveryAction, Settings, StaleProcessInfo, Status, UpdateHealthFailure,
    UpdateInfo, UpdateMode, UpdateState,
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

/// The data directory `?firstrun&storeddir` starts with, as if chosen in an earlier, unfinished
/// first run.
const STORED_DATA_DIR: &str = "/media/small-disk/zebra";

/// Free space at any other folder, such as [`STORED_DATA_DIR`]: below the warning.
const FREE_ELSEWHERE: u64 = 120_000_000_000;

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

/// The page URL's query parameters, as `(key, value)` pairs.
fn url_params() -> Vec<(String, String)> {
    let search = web_sys::window()
        .and_then(|window| window.location().search().ok())
        .unwrap_or_default();
    search
        .trim_start_matches('?')
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (key.to_owned(), value.to_owned())
        })
        .collect()
}

/// True when the page URL's query has the parameter `flag`, such as `?stale` or `?firstrun`.
fn url_flag(flag: &str) -> bool {
    url_params().iter().any(|(key, _)| key == flag)
}

/// The value of `key` in the page URL's query, such as `available` for `?update=available`.
fn url_value(key: &str) -> Option<String> {
    url_params()
        .into_iter()
        .find_map(|(k, value)| (k == key).then_some(value))
}

/// The launcher version the mock runs.
const MOCK_VERSION: &str = "0.1.0";

/// The version the mock's update server offers.
const MOCK_NEW_VERSION: &str = "0.2.0";

/// The size of the mock update download.
const MOCK_DOWNLOAD_BYTES: u64 = 84_000_000;

const MOCK_RELEASES_URL: &str = "https://github.com/ZcashFoundation/zebra-launcher/releases";

const MOCK_NOTES: &str = "Bundles zebrad 7.1.0, whose end of support is about four months after \
its release, and makes the launcher check for updates once more before it halts. Also fixes the \
log view jumping when lines are dropped, and shows the free space of a data folder chosen in an \
earlier, unfinished first run.\n\n## Changes\n\n- zebrad 7.1.0\n- Fewer redraws in the log view";

/// What `?update=…` simulates. A check finds [`MOCK_NEW_VERSION`] with any of them except
/// `error` (the check fails) and `health` (nothing newer).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UpdateFlag {
    /// `available`: an update was found (Notify mode).
    Available,
    /// `downloading`: Auto mode is downloading it.
    Downloading,
    /// `ready`: downloaded and verified, waiting for Install.
    Ready,
    /// `countdown`: Auto mode installs it after a 60 s countdown.
    Countdown,
    /// `error`: the last check failed.
    Error,
    /// `health`: the last update did not come back healthy.
    Health,
}

impl UpdateFlag {
    fn from_url() -> Option<Self> {
        Some(match url_value("update")?.as_str() {
            "available" => UpdateFlag::Available,
            "downloading" => UpdateFlag::Downloading,
            "ready" => UpdateFlag::Ready,
            "countdown" => UpdateFlag::Countdown,
            "error" => UpdateFlag::Error,
            "health" => UpdateFlag::Health,
            _ => return None,
        })
    }

    fn mode(self) -> UpdateMode {
        match self {
            UpdateFlag::Downloading | UpdateFlag::Countdown => UpdateMode::Auto,
            _ => UpdateMode::Notify,
        }
    }

    fn initial_state(self) -> UpdateState {
        let version = MOCK_NEW_VERSION.to_owned();
        match self {
            UpdateFlag::Available => available(),
            UpdateFlag::Downloading => UpdateState::Downloading {
                version,
                downloaded: 0,
                total: Some(MOCK_DOWNLOAD_BYTES),
            },
            UpdateFlag::Ready => UpdateState::ReadyToInstall {
                version,
                countdown_secs: None,
            },
            UpdateFlag::Countdown => UpdateState::ReadyToInstall {
                version,
                countdown_secs: Some(60),
            },
            UpdateFlag::Error => check_error(),
            UpdateFlag::Health => UpdateState::Idle,
        }
    }
}

fn available() -> UpdateState {
    UpdateState::Available {
        version: MOCK_NEW_VERSION.into(),
        notes: Some(MOCK_NOTES.into()),
        pub_date: Some(clock::now() - Duration::from_secs(2 * 86_400)),
    }
}

fn check_error() -> UpdateState {
    UpdateState::Error {
        message: "(mock) could not fetch latest.json: error sending request for url \
                  (https://github.com/ZcashFoundation/zebra-launcher/releases/latest/download/latest.json)"
            .into(),
        at: clock::now(),
    }
}

fn health_failure() -> UpdateHealthFailure {
    UpdateHealthFailure {
        expected_version: MOCK_NEW_VERSION.into(),
        running_version: MOCK_VERSION.into(),
        message: "(mock) zebrad did not reach Running with RPC within 5 minutes after the update"
            .into(),
        recovery: vec![
            RecoveryAction::OpenLogs,
            RecoveryAction::ResetConfig,
            RecoveryAction::OpenReleasesPage,
        ],
        releases_url: MOCK_RELEASES_URL.into(),
    }
}

/// What `?eos=…` simulates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EosFlag {
    /// No flag: the end of support is 41 days away.
    Later,
    /// `soon`: 10 days away, inside the 14-day warning.
    Soon,
    /// `halt`: zebrad refuses to run.
    Halt,
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

/// What `reset_config` writes: the launcher's minimal file for the network and the state
/// directory, like `config::render_minimal`.
fn minimal_config(network: Network, state_dir: &str) -> String {
    let rpc_port = match network {
        Network::Mainnet => 8232,
        Network::Testnet => 18232,
    };
    format!(
        r#"# zebrad configuration managed by Zebra Launcher.
#
# The launcher keeps network.network, rpc.listen_addr, rpc.enable_cookie_auth,
# rpc.cookie_dir, metrics.endpoint_addr and state.cache_dir up to date.
# Add your own settings below; the reference shows every option with its
# default. Keys you do not set use zebrad's defaults.

[network]
network = "{network}"

[rpc]
listen_addr = "127.0.0.1:{rpc_port}"
enable_cookie_auth = true
cookie_dir = "{DEFAULT_DATA_DIR}/rpc"

[metrics]
endpoint_addr = "127.0.0.1:9999"

[state]
cache_dir = "{state_dir}"
"#,
        network = network.as_str(),
    )
}

/// The text that makes the mock reject a config, and the error zebrad 7.0.0 prints for it.
const BAD_KEY: &str = "bogus";
const CONFIG_ERROR: &str = "error: Zebra could not load the provided configuration file and/or environment variables.\nerror: zebrad fatal error: I/O operation failed: Configuration error: unknown field `bogus`, expected one of `listen_addr`, `indexer_listen_addr`, `lightwalletd_listen_addr`, `parallel_cpu_threads`, `debug_force_finished_sync`, `cookie_dir`, `enable_cookie_auth`, `max_response_body_size` for key `rpc`";

/// A config containing this comment makes the mock report that the file changed on disk.
const CONFLICT_MARKER: &str = "# simulate-conflict";

/// A config containing this comment makes the mock fail the next `read_config`.
const READ_FAILURE_MARKER: &str = "# simulate-read-failure";

/// `(stream, level, spans, target, message)` in the shapes zebrad 7.0.0 prints.
type SampleLine = (
    LogStream,
    Option<LogLevel>,
    Option<&'static str>,
    Option<&'static str>,
    &'static str,
);

/// A sample line with the message built for its `seq`.
type GeneratedLine = (
    LogStream,
    Option<LogLevel>,
    Option<&'static str>,
    Option<&'static str>,
    String,
);

/// The start of a run: the first lines of every mock zebrad, by `seq`.
const STARTUP_LINES: &[SampleLine] = &[
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
    (LogStream::Stdout, None, None, None, "version: 7.0.0"),
    (
        LogStream::Stdout,
        None,
        None,
        None,
        "Zcash network: Mainnet",
    ),
    (
        LogStream::Stdout,
        None,
        None,
        None,
        "running state version: 29.0.0",
    ),
    (
        LogStream::Stdout,
        Some(LogLevel::Info),
        Some(r#"{zebrad="ad6528b" net="Main"}"#),
        Some("zebrad::components::metrics"),
        "Opened metrics endpoint at 127.0.0.1:9999",
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
        Some("zebrad::commands::start"),
        "initializing node state",
    ),
    (
        LogStream::Stdout,
        Some(LogLevel::Info),
        None,
        Some("zebrad::commands::start"),
        "opening database, this may take a few minutes",
    ),
    (
        LogStream::Stdout,
        Some(LogLevel::Info),
        None,
        Some("zebra_state::service::finalized_state::disk_db"),
        "Opened Zebra state cache at /home/user/.local/share/org.zfnd.zebra-launcher/state/state/v29/mainnet",
    ),
    (
        LogStream::Stdout,
        Some(LogLevel::Info),
        None,
        Some("zebra_state::service"),
        "loaded Zebra state cache chain_tip=Some(Height(2949999))",
    ),
    (
        LogStream::Stdout,
        Some(LogLevel::Info),
        None,
        Some("zebra_state::service::finalized_state::disk_db"),
        "Total Database Disk Size: 248.3 GB",
    ),
    (
        LogStream::Stdout,
        Some(LogLevel::Info),
        None,
        Some("zebra_state::service::finalized_state::disk_db"),
        "Total Live Data Disk Size: 247.9 GB",
    ),
    (
        LogStream::Stdout,
        Some(LogLevel::Info),
        None,
        Some("zebrad::commands::start"),
        "initializing network",
    ),
    (
        LogStream::Stdout,
        Some(LogLevel::Info),
        Some("open_listener{addr=[::]:8233}"),
        Some("zebra_network::peer_set::initialize"),
        "Opened Zcash protocol endpoint at [::]:8233",
    ),
    (
        LogStream::Stdout,
        Some(LogLevel::Info),
        Some("add_initial_peers"),
        Some("zebra_network::peer_set::initialize"),
        "connecting to initial peer set ipv4_peer_count=14 ipv6_peer_count=1",
    ),
    (
        LogStream::Stdout,
        Some(LogLevel::Info),
        Some("add_initial_peers"),
        Some("zebra_network::peer_set::initialize"),
        "an initial peer connection failed successes=11 errors=1 addr=203.0.113.77:8233 e=connection timed out",
    ),
    (
        LogStream::Stdout,
        Some(LogLevel::Info),
        Some("add_initial_peers"),
        Some("zebra_network::peer_set::initialize"),
        "finished connecting to initial seed and disk cache peers handshake_success_total=12 handshake_error_total=3 outbound_connections=12",
    ),
    (
        LogStream::Stdout,
        Some(LogLevel::Info),
        None,
        Some("zebrad::commands::start"),
        "initializing verifiers",
    ),
    (
        LogStream::Stdout,
        Some(LogLevel::Info),
        Some("checkpoint"),
        Some("zebra_consensus::checkpoint"),
        "initialising CheckpointVerifier max_height=Height(2900000)",
    ),
    (
        LogStream::Stdout,
        Some(LogLevel::Info),
        None,
        Some("zebrad::commands::start"),
        "initializing syncer",
    ),
    (
        LogStream::Stdout,
        Some(LogLevel::Info),
        None,
        Some("zebrad::commands::start"),
        "initializing mempool",
    ),
    (
        LogStream::Stdout,
        Some(LogLevel::Info),
        None,
        Some("zebra_rpc::server::cookie"),
        "RPC auth cookie written to disk",
    ),
    (
        LogStream::Stdout,
        Some(LogLevel::Info),
        None,
        Some("zebra_rpc::server"),
        "Opened RPC endpoint at 127.0.0.1:8232",
    ),
    (
        LogStream::Stdout,
        Some(LogLevel::Info),
        Some("checkpoint"),
        Some("zebra_consensus::checkpoint"),
        "verified checkpoint range block_count=400 current_range=(Excluded(Height(2899599)), Included(Height(2899999)))",
    ),
    (
        LogStream::Stdout,
        Some(LogLevel::Info),
        Some("checkpoint"),
        Some("zebra_consensus::checkpoint"),
        "verified final checkpoint: starting full validation final_checkpoint_height=Height(2900000)",
    ),
    (
        LogStream::Stdout,
        Some(LogLevel::Info),
        None,
        Some("zebrad::commands::start"),
        "spawned initial Zebra tasks",
    ),
    (
        LogStream::Stdout,
        Some(LogLevel::Info),
        Some("sync:try_to_sync"),
        Some("zebrad::components::sync"),
        "starting sync, obtaining new tips state_tip=Some(Height(2949999))",
    ),
    (
        LogStream::Stdout,
        Some(LogLevel::Info),
        None,
        Some("zebrad::components::mempool"),
        "activating mempool: Zebra is close to the tip tip_height=Height(2949999)",
    ),
];

/// The chain tip the mock's progress lines count towards.
const MOCK_TIP_HEIGHT: u64 = 3_050_000;

/// The height the mock has reached at `seq`.
fn mock_height(seq: u64) -> u64 {
    2_950_000 + seq * 2
}

/// A hex digest that is a function of `seq`, for block hashes.
fn mock_hash(seq: u64) -> String {
    (0..4u64)
        .map(|lane| {
            let mut x = seq
                .wrapping_add(lane.wrapping_mul(0x9e37_79b9_7f4a_7c15))
                .wrapping_mul(0xbf58_476d_1ce4_e5b9);
            x ^= x >> 31;
            format!("{x:016x}")
        })
        .collect()
}

/// The mock's log line at `seq`: a startup sequence, then a steady state where every 20th line
/// reports progress, every 37th warns, every 101st comes from stderr and the rest is INFO
/// gossip. Deterministic in `seq`, so any range can be regenerated.
fn sample_line(seq: u64) -> GeneratedLine {
    if let Some(&(stream, level, spans, target, message)) =
        STARTUP_LINES.get(usize::try_from(seq).unwrap_or(usize::MAX))
    {
        return (stream, level, spans, target, message.to_owned());
    }
    let height = mock_height(seq);
    let info = Some(LogLevel::Info);
    if seq.is_multiple_of(101) {
        return (
            LogStream::Stderr,
            None,
            None,
            None,
            "thread 'tokio-rt-worker' (210439) panicked at zebra-network/src/peer/connection.rs:1313:23: (mock) a stderr line".to_owned(),
        );
    }
    if seq.is_multiple_of(37) {
        let addr = format!("203.0.113.{}:8233", (seq / 37) % 250);
        return match (seq / 37) % 3 {
            0 => (
                LogStream::Stdout,
                Some(LogLevel::Warn),
                None,
                Some("zebra_network::peer_set::set"),
                format!("dropping stalled peer: exceeded FindBlocks/FindHeaders stall threshold addr={addr}"),
            ),
            1 => (
                LogStream::Stdout,
                Some(LogLevel::Warn),
                Some("crawl_and_dial{new_peer_interval=61s}"),
                Some("zebra_network::peer_set::initialize"),
                format!("failed to make outbound connection to peer error=connection timed out candidate.addr={addr}"),
            ),
            _ => (
                LogStream::Stdout,
                Some(LogLevel::Warn),
                None,
                Some("zebrad::commands::start"),
                "TCP slow-start-after-idle is enabled, which resets TCP's congestion window between block requests and significantly reduces single-peer throughput for block propagation. Hint: set `net.ipv4.tcp_slow_start_after_idle=0` via sysctl.".to_owned(),
            ),
        };
    }
    if seq.is_multiple_of(20) {
        let remaining = MOCK_TIP_HEIGHT.saturating_sub(height);
        let percent = height as f64 / MOCK_TIP_HEIGHT as f64 * 100.0;
        return (
            LogStream::Stdout,
            info,
            None,
            Some("zebrad::components::sync::progress"),
            format!(
                "estimated progress to chain tip sync_percent={percent:.3}% current_height=Height({height}) network_upgrade=Nu6_1 remaining_sync_blocks={remaining} time_since_last_state_block={}s",
                (seq % 5) * 15 + 5
            ),
        );
    }
    match seq % 6 {
        0 => (
            LogStream::Stdout,
            info,
            Some("sync:try_to_sync"),
            Some("zebrad::components::sync"),
            format!("starting sync, obtaining new tips state_tip=Some(Height({height}))"),
        ),
        1 => (
            LogStream::Stdout,
            info,
            None,
            Some("zebrad::components::sync::gossip"),
            format!(
                r#"height=Height({height}) hash=block::Hash("{}") is_block_submission=false log_msg="sending committed block broadcast""#,
                mock_hash(seq)
            ),
        ),
        2 => (
            LogStream::Stdout,
            info,
            Some("sync:extend_tips"),
            Some("zebrad::components::sync"),
            format!(
                "extending tips tips=1 in_flight={} lookahead_limit=2000",
                40 + seq % 11
            ),
        ),
        3 => (
            LogStream::Stdout,
            info,
            None,
            Some("zebrad::components::mempool::crawler"),
            format!(
                "fetched mempool transaction ids from peers tx_count={}",
                8 + seq % 23
            ),
        ),
        4 => (
            LogStream::Stdout,
            info,
            Some("crawl_and_dial{new_peer_interval=61s}"),
            Some("zebra_network::peer_set::initialize"),
            format!(
                "crawled for new peers outbound_connections={} address_book_len={}",
                12 + seq % 3,
                1_200 + seq % 97
            ),
        ),
        _ => (
            LogStream::Stdout,
            info,
            None,
            Some("zebra_state::service::write"),
            format!(
                "committed block to the finalized state height=Height({})",
                height - 1
            ),
        ),
    }
}

#[cfg(test)]
mod sample_line_tests {
    use super::*;

    #[test]
    fn mock_lines_follow_the_schedule() {
        assert_eq!(
            sample_line(0).3,
            Some("zebrad::components::tracing::component")
        );
        assert_eq!(sample_line(2).4, "version: 7.0.0");
        let startup = STARTUP_LINES.len() as u64;
        assert!(sample_line(startup - 1).4.starts_with("activating mempool"));
        // Steady state: 40 is the first progress line after the startup sequence.
        let progress = sample_line(40);
        assert_eq!(progress.3, Some("zebrad::components::sync::progress"));
        assert!(
            progress.4.contains("current_height=Height(2950080)"),
            "{}",
            progress.4
        );
        assert_eq!(sample_line(37).1, Some(LogLevel::Warn));
        assert_eq!(sample_line(74).1, Some(LogLevel::Warn));
        assert_eq!(sample_line(101).0, LogStream::Stderr);
        assert_eq!(sample_line(202).0, LogStream::Stderr);
        assert_eq!(sample_line(41).1, Some(LogLevel::Info));
        assert_eq!(sample_line(41), sample_line(41), "deterministic");
        assert_eq!(mock_hash(7).len(), 64);
        assert_ne!(mock_hash(7), mock_hash(8));
    }
}

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
    /// `?running`: zebrad runs from the start, even with `?firstrun`, and `start_zebrad` fails
    /// while it runs.
    refuse_start_while_running: bool,
    eos: EosFlag,
    /// The updater, as `get_update_info` returns it. `mode` follows the settings.
    update: ArcRwSignal<UpdateInfo>,
    /// Bumped by every updater command, so a delayed step of an older one is skipped.
    update_generation: Arc<AtomicU64>,
    /// The `update_health_failed` event, sent a moment after load with `?update=health`.
    health_event: ArcRwSignal<Option<UpdateHealthFailure>>,
    update_flag: Option<UpdateFlag>,
    /// The user clicked Install, so the download ends in an install (Auto mode's background
    /// download waits instead).
    install_requested: Arc<AtomicBool>,
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
        let update_flag = UpdateFlag::from_url();
        let update_mode = update_flag.map_or(UpdateMode::Off, UpdateFlag::mode);
        let mock = Self {
            status: ArcRwSignal::new(Status::default()),
            stats: ArcRwSignal::new(None),
            settings: ArcRwSignal::new(Settings {
                first_run_done: !first_run,
                data_dir: url_flag("storeddir").then(|| STORED_DATA_DIR.into()),
                update_mode,
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
            // `?configfail`: the first `read_config` fails, as for an unreadable file.
            fail_next_read: Arc::new(AtomicBool::new(url_flag("configfail"))),
            legacy: ArcRwSignal::new(first_run.then(sample_legacy)),
            state_bytes: Arc::new(AtomicU64::new(if first_run {
                0
            } else {
                SYNCED_STATE_BYTES
            })),
            refuse_start_while_running: url_flag("running"),
            eos: match url_value("eos").as_deref() {
                Some("soon") => EosFlag::Soon,
                Some("halt") => EosFlag::Halt,
                _ => EosFlag::Later,
            },
            update: ArcRwSignal::new(UpdateInfo {
                state: update_flag.map_or(UpdateState::Idle, UpdateFlag::initial_state),
                mode: update_mode,
                current_version: MOCK_VERSION.into(),
                last_successful_check: if url_flag("stalecheck") {
                    Some(clock::now() - Duration::from_secs(20 * 86_400))
                } else {
                    update_flag.map(|_| clock::now() - Duration::from_secs(5 * 3600))
                },
                stale_check_warning: url_flag("stalecheck"),
                can_install: !url_flag("deb"),
                releases_url: MOCK_RELEASES_URL.into(),
                health_failure: (update_flag == Some(UpdateFlag::Health)).then(health_failure),
            }),
            update_generation: Arc::new(AtomicU64::new(0)),
            health_event: ArcRwSignal::new(None),
            update_flag,
            install_requested: Arc::new(AtomicBool::new(false)),
        };
        // `?stale` in the page URL simulates a zebrad left behind by an earlier launcher run.
        if url_flag("stale") {
            mock.status.update(|status| {
                status.stale = Some(StaleProcessInfo {
                    pid: 1234,
                    binary: "/opt/zebra-launcher/zebrad".into(),
                })
            });
        } else if !first_run || mock.refuse_start_while_running {
            mock.start();
        }
        let ticker = mock.clone();
        set_interval(move || ticker.tick_stats(), Duration::from_secs(1));
        match update_flag {
            Some(UpdateFlag::Downloading) => mock.download(),
            Some(UpdateFlag::Countdown) => mock.count_down(),
            Some(UpdateFlag::Health) => {
                let event = mock.health_event.clone();
                set_timeout(
                    move || event.set(Some(health_failure())),
                    Duration::from_millis(1500),
                );
            }
            _ => {}
        }
        mock
    }

    fn update_state(&self) -> UpdateState {
        self.update.with_untracked(|info| info.state.clone())
    }

    fn set_update_state(&self, state: UpdateState) {
        self.update.update(|info| info.state = state);
    }

    /// Runs `step` after `delay` unless another updater command was issued in between.
    fn after_update(&self, delay: Duration, step: impl FnOnce(&Mock) + 'static) {
        let generation = self.update_generation.load(Ordering::Relaxed);
        let mock = self.clone();
        set_timeout(
            move || {
                if mock.update_generation.load(Ordering::Relaxed) == generation {
                    step(&mock);
                }
            },
            delay,
        );
    }

    /// Downloads in steps, then installs (a user's Install) or waits (Auto mode).
    fn download(&self) {
        self.after_update(Duration::from_millis(300), |mock| {
            let UpdateState::Downloading {
                version,
                downloaded,
                total,
            } = mock.update_state()
            else {
                return;
            };
            let downloaded = (downloaded + MOCK_DOWNLOAD_BYTES / 25).min(MOCK_DOWNLOAD_BYTES);
            if downloaded < MOCK_DOWNLOAD_BYTES {
                mock.set_update_state(UpdateState::Downloading {
                    version,
                    downloaded,
                    total,
                });
                mock.download();
            } else if mock.install_requested.load(Ordering::Relaxed) {
                mock.install(version);
            } else {
                mock.set_update_state(UpdateState::ReadyToInstall {
                    version,
                    countdown_secs: None,
                });
            }
        });
    }

    /// Counts the automatic install down, one second at a time.
    fn count_down(&self) {
        self.after_update(Duration::from_secs(1), |mock| {
            let UpdateState::ReadyToInstall {
                version,
                countdown_secs: Some(secs),
            } = mock.update_state()
            else {
                return;
            };
            if secs <= 1 {
                mock.install(version);
            } else {
                mock.set_update_state(UpdateState::ReadyToInstall {
                    version,
                    countdown_secs: Some(secs - 1),
                });
                mock.count_down();
            }
        });
    }

    /// Installs, and "restarts" as the new version a moment later.
    fn install(&self, version: String) {
        self.install_requested.store(false, Ordering::Relaxed);
        self.set_update_state(UpdateState::Installing {
            version: version.clone(),
        });
        self.after_update(Duration::from_secs(3), move |mock| {
            mock.update.update(|info| {
                info.current_version = version;
                info.state = UpdateState::Idle;
            });
        });
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
        if self.eos == EosFlag::Halt {
            // zebrad 7.0.0 refuses to run past its end of support and exits with code 1.
            self.after(Duration::from_millis(1500), |mock| {
                mock.status.update(|status| {
                    status.phase = Phase::Failed {
                        class: ExitClass::EosHalt,
                    };
                    status.pid = None;
                    status.started_at = None;
                    status.last_exit = Some(ExitInfo {
                        code: Some(1),
                        class: ExitClass::EosHalt,
                        detail: Some(
                            "(mock) Zebra refuses to run if the release date is older than \
                             end of support"
                                .into(),
                        ),
                        at: clock::now(),
                        requested: false,
                    });
                });
            });
            return;
        }
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
                estimated_time: clock::now()
                    + Duration::from_secs(
                        if self.eos == EosFlag::Soon { 10 } else { 41 } * 24 * 3600,
                    ),
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
        let (stream, level, spans, target, message) = sample_line(seq);
        let has_prefix = level.is_some();
        LogLine {
            seq,
            stream,
            timestamp: has_prefix.then(clock::now_iso),
            level,
            spans: spans.map(Into::into),
            target: target.map(Into::into),
            message,
            dropped: None,
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
    if mock.refuse_start_while_running && matches!(mock.phase(), Phase::Starting | Phase::Running) {
        return Err("(mock) another start is in progress".into());
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
                // Like the bridge after lagging: the lost lines are replaced by one gap marker
                // that takes the seq of the last of them.
                const DROPPED: u64 = 7;
                let first_dropped = mock.next_seq.fetch_add(DROPPED, Ordering::Relaxed);
                batch.push(LogLine::gap_marker(DROPPED, first_dropped + DROPPED - 1));
            }
            let count = 1 + (js_sys::Math::random() * 3.0) as u64;
            let first = mock.next_seq.fetch_add(count, Ordering::Relaxed);
            batch.extend((first..first + count).map(|seq| mock.line(seq)));
            on_batch(batch);
        },
        Duration::from_millis(400),
    );
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

pub async fn reset_config() -> Result<ConfigDocument, String> {
    sleep(300).await;
    let mock = mock();
    mock.require_stopped()?;
    let network = mock.settings.with_untracked(|settings| settings.network);
    let text = minimal_config(network, &mock.state_dir());
    mock.config.update(|config| {
        config.text = text;
        config.token = format!("mock-{}", js_sys::Date::now());
    });
    Ok(mock.config.get_untracked())
}

pub async fn get_settings() -> Settings {
    sleep(50).await;
    mock().settings.get_untracked()
}

pub async fn set_settings(settings: Settings) -> Settings {
    sleep(200).await;
    let mock = mock();
    // Like the real backend: `first_run_done` is only set by `complete_first_run` and
    // `data_dir` only by `choose_data_dir`.
    let stored = mock.settings.with_untracked(|current| Settings {
        first_run_done: current.first_run_done,
        schema_version: current.schema_version,
        data_dir: current.data_dir.clone(),
        ..settings
    });
    mock.settings.set(stored.clone());
    // Like the updater actor: turning updates off stops everything but an install.
    let mode = stored.update_mode;
    if mode == UpdateMode::Off && !matches!(mock.update_state(), UpdateState::Installing { .. }) {
        mock.update_generation.fetch_add(1, Ordering::Relaxed);
        mock.update.update(|info| {
            info.mode = mode;
            info.state = UpdateState::Idle;
        });
    } else {
        mock.update.update(|info| info.mode = mode);
    }
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
        .update(|settings| settings.data_dir = (!is_default).then(|| chosen.clone()));
    Ok(DataDirChoice {
        settings: mock.settings.get_untracked(),
        free_bytes: Some(match chosen.as_str() {
            DEFAULT_DATA_DIR => FREE_AT_DEFAULT,
            PICKED_DATA_DIR => FREE_AT_PICKED,
            _ => FREE_ELSEWHERE,
        }),
    })
}

pub async fn free_space(path: String) -> Option<u64> {
    // Slower than `get_first_run_info`, so the wizard's data step shows "checking" while Next
    // is already enabled.
    sleep(1500).await;
    let path = path.trim().trim_end_matches('/');
    match path {
        DEFAULT_DATA_DIR => Some(FREE_AT_DEFAULT),
        PICKED_DATA_DIR => Some(FREE_AT_PICKED),
        _ if path.starts_with('/') => Some(FREE_ELSEWHERE),
        _ => None,
    }
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

pub async fn delete_legacy_state() -> Result<u64, String> {
    sleep(700).await;
    let mock = mock();
    mock.require_stopped()?;
    let Some(LegacyInstall {
        state_dir: Some(_),
        state_bytes,
        ..
    }) = mock.legacy.get_untracked()
    else {
        return Err("no alpha.4 chain state was found".into());
    };
    // The old config next to the old app stays.
    mock.legacy.update(|legacy| {
        if let Some(legacy) = legacy {
            legacy.state_dir = None;
            legacy.state_bytes = 0;
        }
    });
    Ok(state_bytes)
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

/// Calls `handler` with every updater state change, starting with the current state.
pub fn on_update_state(handler: impl Fn(UpdateState) + 'static) {
    let update = mock().update;
    let state = Memo::new(move |_| update.with(|info| info.state.clone()));
    Effect::new(move |_| handler(state.get()));
}

/// Calls `handler` with every change of the update info, starting with the current one.
pub fn on_update_info(handler: impl Fn(UpdateInfo) + 'static) {
    let update = mock().update;
    Effect::new(move |_| handler(update.get()));
}

/// Calls `handler` once the mock's health failure event fires (`?update=health`).
pub fn on_update_health_failed(handler: impl Fn(UpdateHealthFailure) + 'static) {
    let event = mock().health_event;
    Effect::new(move |_| {
        if let Some(failure) = event.get() {
            handler(failure);
        }
    });
}

pub async fn get_update_info() -> UpdateInfo {
    sleep(80).await;
    mock().update.get_untracked()
}

pub async fn check_for_update() -> UpdateState {
    let mock = mock();
    // A download, a countdown or an install keeps going; a check just reports it.
    if matches!(
        mock.update_state(),
        UpdateState::Downloading { .. }
            | UpdateState::ReadyToInstall { .. }
            | UpdateState::Installing { .. }
    ) {
        sleep(50).await;
        return mock.update_state();
    }
    let generation = mock.update_generation.fetch_add(1, Ordering::Relaxed) + 1;
    mock.set_update_state(UpdateState::Checking);
    sleep(1200).await;
    if mock.update_generation.load(Ordering::Relaxed) != generation {
        // Cancelled, or the mode changed meanwhile; whatever superseded it set the state.
        return mock.update_state();
    }
    let found = match mock.update_flag {
        Some(UpdateFlag::Error) => check_error(),
        None | Some(UpdateFlag::Health) => UpdateState::UpToDate {
            checked_at: clock::now(),
        },
        Some(_)
            if mock
                .update
                .with_untracked(|info| info.current_version != MOCK_VERSION) =>
        {
            UpdateState::UpToDate {
                checked_at: clock::now(),
            }
        }
        Some(_) => available(),
    };
    mock.update.update(|info| {
        if !matches!(found, UpdateState::Error { .. }) {
            info.last_successful_check = Some(clock::now());
            info.stale_check_warning = false;
        }
        info.state = found.clone();
    });
    found
}

pub async fn install_update() -> Result<(), String> {
    sleep(100).await;
    let mock = mock();
    if !mock.update.with_untracked(|info| info.can_install) {
        return Err("this installation cannot update itself; download the new version from the releases page".into());
    }
    // Validate first: bumping the generation cancels a running check, which must not happen
    // for an install that is refused.
    match mock.update_state() {
        UpdateState::Available { version, .. } => {
            mock.update_generation.fetch_add(1, Ordering::Relaxed);
            mock.install_requested.store(true, Ordering::Relaxed);
            mock.set_update_state(UpdateState::Downloading {
                version,
                downloaded: 0,
                total: Some(MOCK_DOWNLOAD_BYTES),
            });
            mock.download();
            Ok(())
        }
        UpdateState::ReadyToInstall { version, .. } => {
            mock.update_generation.fetch_add(1, Ordering::Relaxed);
            mock.install(version);
            Ok(())
        }
        _ => Err("no update is available; check for updates first".into()),
    }
}

pub async fn cancel_update() {
    sleep(50).await;
    let mock = mock();
    let cancelled = match mock.update_state() {
        UpdateState::Checking => UpdateState::Idle,
        UpdateState::Downloading { .. } => available(),
        UpdateState::ReadyToInstall {
            version,
            countdown_secs: Some(_),
        } => UpdateState::ReadyToInstall {
            version,
            countdown_secs: None,
        },
        _ => return,
    };
    mock.update_generation.fetch_add(1, Ordering::Relaxed);
    mock.install_requested.store(false, Ordering::Relaxed);
    mock.set_update_state(cancelled);
}

pub async fn open_path(target: &str) {
    sleep(50).await;
    leptos::logging::log!("(mock) open_path({target})");
}
