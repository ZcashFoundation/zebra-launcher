//! The mock's node details: the peer list, the best block, the chain's supply and value pools,
//! derived from the node statistics so the two agree.

use std::time::Duration;

use launcher_types::{NodeDetails, NodeStats, PeerInfo, ValuePool};

use super::{Scenario, log_lines::mock_hash};

/// The protocol version zebrad 7.0.0 speaks.
const PROTOCOL_VERSION: u32 = 170_120;

/// `(address, inbound, user agent, protocol version, ping in seconds)` of the peers the mock
/// may be connected to, in the order they join; the node statistics' peer count says how many.
const PEERS: [(&str, bool, &str, u32, f64); 12] = [
    ("203.0.113.17:8233", false, "/Zebra:7.0.0/", 170_120, 0.034),
    (
        "198.51.100.42:8233",
        false,
        "/MagicBean:6.2.0/",
        170_120,
        0.121,
    ),
    (
        "[2001:db8::1a]:8233",
        false,
        "/Zebra:7.0.0/",
        170_120,
        0.058,
    ),
    (
        "203.0.113.88:8233",
        true,
        "/MagicBean:6.2.0/",
        170_120,
        0.210,
    ),
    ("192.0.2.9:8233", false, "/Zebra:6.0.0/", 170_100, 0.087),
    (
        "198.51.100.7:8233",
        false,
        "/MagicBean:6.1.0/",
        170_120,
        0.302,
    ),
    ("203.0.113.201:8233", true, "/Zebra:7.0.0/", 170_120, 0.045),
    (
        "[2001:db8:1::77]:8233",
        false,
        "/MagicBean:6.2.0/",
        170_120,
        0.176,
    ),
    ("192.0.2.130:8233", false, "/Zebra:7.0.0/", 170_120, 0.063),
    (
        "198.51.100.250:8233",
        true,
        "/MagicBean:6.2.0/",
        170_120,
        0.389,
    ),
    (
        "203.0.113.5:8233",
        false,
        "/Zebra:7.0.0-rc.0/",
        170_120,
        0.099,
    ),
    ("192.0.2.77:8233", true, "/Zebra:7.0.0/", 170_120, 0.152),
];

/// The value pools in zebrad's order, with their balances in ZEC at the mock's first height.
const POOLS: [(&str, f64); 5] = [
    ("transparent", 13_980_123.45),
    ("sprout", 23_456.78),
    ("sapling", 512_345.67),
    ("orchard", 1_654_321.0),
    ("lockbox", 345_678.9),
];

/// The height the pool balances are quoted at.
const POOLS_HEIGHT: u64 = 2_950_000;

/// How much ZEC a block adds to the supply after the second halving.
const SUBSIDY_PER_BLOCK: f64 = 1.5625;

/// The details at `tick`, for `stats`.
pub(super) fn details(
    stats: &NodeStats,
    tick: u64,
    state_bytes: u64,
    scenario: Scenario,
) -> NodeDetails {
    let peers = if scenario == Scenario::NoPeers {
        Vec::new()
    } else {
        PEERS
            .iter()
            .take(stats.peers as usize)
            .enumerate()
            .map(
                |(index, &(addr, inbound, agent, protocol, ping))| PeerInfo {
                    addr: addr.into(),
                    inbound,
                    user_agent: Some(agent.into()),
                    protocol_version: Some(protocol),
                    state: Some("connected".into()),
                    // Each peer was last heard from a few seconds ago, staggered.
                    last_seen: Some(
                        stats.updated_at - Duration::from_secs((tick + index as u64 * 7) % 45),
                    ),
                    ping_secs: Some(ping + (tick % 5) as f64 * 0.004),
                },
            )
            .collect()
    };
    let minted = stats.height.saturating_sub(POOLS_HEIGHT) as f64 * SUBSIDY_PER_BLOCK;
    let value_pools: Vec<ValuePool> = POOLS
        .iter()
        .map(|&(id, zec)| ValuePool {
            id: id.into(),
            // New coins go to the orchard pool, where mining rewards land.
            zec: if id == "orchard" { zec + minted } else { zec },
        })
        .collect();
    let supply = value_pools.iter().map(|pool| pool.zec).sum();
    // Block hashes start with zeros: the proof of work.
    let hash = format!("00000000{}", &mock_hash(stats.height)[8..]);
    NodeDetails {
        peers,
        best_block_hash: Some(hash),
        headers: Some(stats.height + tick % 2),
        difficulty: Some(118_462_377.5 + (tick % 90) as f64 * 1_024.0),
        size_on_disk: Some(state_bytes + tick * 2_500_000),
        chain_supply_zec: Some(supply),
        value_pools,
        active_upgrade: Some("NU6.1".into()),
        subversion: Some("/Zebra:7.0.0/".into()),
        protocol_version: Some(PROTOCOL_VERSION),
        mempool_bytes: Some(stats.mempool_transactions * 1_850),
        mempool_usage: Some(stats.mempool_transactions * 4_200),
        updated_at: stats.updated_at,
    }
}

#[cfg(test)]
mod tests {
    use std::time::UNIX_EPOCH;

    use super::*;

    fn stats(peers: u32, height: u64) -> NodeStats {
        NodeStats {
            height,
            estimated_height: 3_120_000,
            verification_progress: 0.95,
            peers,
            mempool_transactions: 24,
            chain: "main".into(),
            zebrad_version: "7.0.0".into(),
            last_error: None,
            eos: None,
            updated_at: UNIX_EPOCH + Duration::from_secs(1_000_000),
        }
    }

    #[test]
    fn details_follow_the_stats() {
        let full = details(&stats(10, 2_950_400), 3, 1_000, Scenario::Normal);
        assert_eq!(full.peers.len(), 10);
        assert_eq!(
            full.peers.iter().filter(|peer| peer.inbound).count(),
            3,
            "a mix of directions"
        );
        assert!(
            full.peers
                .iter()
                .any(|peer| peer.user_agent.as_deref() == Some("/MagicBean:6.2.0/"))
        );
        assert!(
            full.peers
                .iter()
                .all(|peer| peer.last_seen.unwrap() <= full.updated_at)
        );
        assert_eq!(full.best_block_hash.as_ref().unwrap().len(), 64);
        assert!(full.best_block_hash.unwrap().starts_with("00000000"));
        assert_eq!(full.headers, Some(2_950_401));
        assert_eq!(full.size_on_disk, Some(1_000 + 3 * 2_500_000));
        assert_eq!(full.mempool_bytes, Some(24 * 1_850));
        assert_eq!(full.value_pools.len(), 5);
        let supply: f64 = full.value_pools.iter().map(|pool| pool.zec).sum();
        assert_eq!(full.chain_supply_zec, Some(supply));
        // 400 blocks of subsidy went to the orchard pool.
        let orchard = full
            .value_pools
            .iter()
            .find(|pool| pool.id == "orchard")
            .unwrap();
        assert_eq!(orchard.zec, 1_654_321.0 + 400.0 * SUBSIDY_PER_BLOCK);
        // Twelve peers at most, none without peers.
        assert_eq!(
            details(&stats(30, 2_950_400), 3, 0, Scenario::Normal)
                .peers
                .len(),
            12
        );
        assert!(
            details(&stats(10, 2_950_400), 3, 0, Scenario::NoPeers)
                .peers
                .is_empty()
        );
    }
}
