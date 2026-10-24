//! The mock's log corpus: real zebrad 7.0.0 lines from `crates/launcher-core/tests/fixtures`,
//! as a startup sequence and a steady state generated from the line's sequence number.

use launcher_types::{LogLevel, LogStream};

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
pub(super) fn sample_line(seq: u64) -> GeneratedLine {
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
