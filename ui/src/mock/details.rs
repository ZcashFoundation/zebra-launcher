//! The mock's node details: the peer list from the node statistics, and the best block, the
//! supply, the value pools and the upgrades from the chain the mock's Explorer shows, so the
//! dashboard and the Explorer agree.

use std::time::Duration;

use launcher_types::{
    ConsensusBranches, NodeDetails, NodeStats, PeerInfo, UpgradeStatus, ValuePool, explorer::Pool,
};

use super::{Scenario, explorer::ChainFacts};

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

/// The details at `tick`, for `stats`, on `chain`: the facts of the block at the node's
/// height.
pub(super) fn details(
    stats: &NodeStats,
    tick: u64,
    state_bytes: u64,
    scenario: Scenario,
    chain: &ChainFacts,
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
    let zec = |zat: u64| zat as f64 / 100_000_000.0;
    let value_pools: Vec<ValuePool> = Pool::ALL
        .into_iter()
        .map(|pool| ValuePool {
            id: pool.id().into(),
            zec: zec(chain.pools[pool.index()]),
        })
        .collect();
    let supply = zec(chain.pools.iter().sum());
    let upgrades = chain.upgrades.clone();
    NodeDetails {
        peers,
        best_block_hash: Some(chain.best_block_hash.to_hex()),
        headers: Some(stats.height + tick % 2),
        difficulty: Some(chain.difficulty),
        size_on_disk: Some(state_bytes + tick * 2_500_000),
        chain_supply_zec: Some(supply),
        value_pools,
        active_upgrade: upgrades
            .iter()
            .rev()
            .find(|upgrade| upgrade.status == UpgradeStatus::Active)
            .map(|upgrade| upgrade.name.clone()),
        upgrades,
        consensus: Some(ConsensusBranches {
            chain_tip: chain.chain_tip_branch.into(),
            next_block: chain.next_block_branch.into(),
        }),
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

    use explorer_model::synth::SynthChain;
    use launcher_types::Network;

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

    fn chain_at(height: u32) -> ChainFacts {
        let chain = SynthChain::new(Network::Mainnet, height, 1_800_000_000);
        ChainFacts::of(&chain, height)
    }

    #[test]
    fn details_follow_the_stats_and_the_chain() {
        let chain = chain_at(2_950_400);
        let full = details(&stats(10, 2_950_400), 3, 1_000, Scenario::Normal, &chain);
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
        // The best block is the chain's own, as the Explorer shows it.
        assert_eq!(
            full.best_block_hash.as_deref(),
            Some(chain.best_block_hash.to_hex().as_str())
        );
        assert!(full.best_block_hash.unwrap().starts_with("00000000"));
        assert_eq!(full.headers, Some(2_950_401));
        assert_eq!(full.size_on_disk, Some(1_000 + 3 * 2_500_000));
        assert_eq!(full.mempool_bytes, Some(24 * 1_850));
        // Six pools in zebrad's order, adding up to the supply.
        let ids: Vec<&str> = full
            .value_pools
            .iter()
            .map(|pool| pool.id.as_str())
            .collect();
        assert_eq!(
            ids,
            [
                "transparent",
                "sprout",
                "sapling",
                "orchard",
                "lockbox",
                "ironwood"
            ]
        );
        let supply: f64 = full.value_pools.iter().map(|pool| pool.zec).sum();
        assert!((full.chain_supply_zec.unwrap() - supply).abs() < 1e-6);
        assert_eq!(full.value_pools[5].zec, 0.0, "no Ironwood before NU6.3");
        // Below NU6.1 the mock runs NU6, and the next block too.
        assert_eq!(full.active_upgrade.as_deref(), Some("NU6"));
        assert_eq!(full.upgrades.len(), 11);
        assert_eq!(full.consensus.as_ref().unwrap().chain_tip, "c8e71055");
        let at_activation = chain_at(3_146_399);
        assert_eq!(at_activation.chain_tip_branch, "c8e71055");
        assert_eq!(
            at_activation.next_block_branch, "4dec4df0",
            "the next block activates NU6.1"
        );
        // Twelve peers at most, none without peers.
        assert_eq!(
            details(&stats(30, 2_950_400), 3, 0, Scenario::Normal, &chain)
                .peers
                .len(),
            12
        );
        assert!(
            details(&stats(10, 2_950_400), 3, 0, Scenario::NoPeers, &chain)
                .peers
                .is_empty()
        );
    }
}
