//! What a block did to the value pools, from three cheap reads: the block and its parent at
//! verbosity 1, and the block's coinbase transaction.
//!
//! A block's pool changes are the negated sum of its transactions' value balances, with the
//! lockbox and the burned fees set apart (zebra-chain/src/block.rs:303-372), and a coinbase's
//! value balance is minus its outputs. So the fees are exact in every era:
//!
//! > fees = the coinbase's outputs − the change of the five pools other than the lockbox
//!
//! and the subsidy closes the books: from NU6 the pools grow by the subsidy, from NU7 by the
//! subsidy less the burned share of the fees, and before NU6 by at most the subsidy (a miner
//! may leave part of it unclaimed).

use launcher_types::{
    Network,
    explorer::{BlockLedger, Budget, LedgerCheck, PerPool, Pool},
};

use crate::{
    shapes::{BlockV1, Tx},
    upgrades::{Upgrade, UpgradeHeights},
};

/// ZIP 218's limit on a block's shielded spends, outputs and actions, from NU7.
pub const BUDGET_LIMIT: u32 = 330;
/// The largest block, in bytes (zebra-chain/src/block/serialize.rs).
pub const MAX_BLOCK_BYTES: u32 = 2_000_000;

/// The share of a block's fees that NU7 removes from circulation: ⌊6 F / 10⌋, computed without
/// overflow as zebrad does (zebra-chain/src/parameters/network/subsidy.rs).
pub fn burned_share(fees: u64) -> u64 {
    fees / 10 * 6 + fees % 10 * 6 / 10
}

/// The NSM reserve at NU7's activation, in zatoshis (subsidy.rs:723-748): 368.58445520 ZEC
/// on Mainnet, 557.68414957 on Testnet. Burned fees add to it.
pub fn nsm_seed(network: Network) -> u64 {
    match network {
        Network::Mainnet => 36_858_445_520,
        Network::Testnet => 55_768_414_957,
    }
}

/// What the coinbase paid into each pool: its transparent outputs, and its shielded outputs
/// (the negated value balances). Nothing goes to the lockbox through outputs.
pub fn coinbase_into_pools(coinbase: &Tx) -> PerPool<u64> {
    let mut pools = PerPool([0u64; 6]);
    pools.set(
        Pool::Transparent,
        u64::try_from(coinbase.transparent_out()).unwrap_or(0),
    );
    for (pool, net_out) in Pool::SHIELDED.into_iter().zip(coinbase.net_out()) {
        pools.set(pool, u64::try_from(-net_out).unwrap_or(0));
    }
    pools
}

/// The ledger of `block`, given its `parent` (absent for the genesis block) and its coinbase
/// transaction.
pub fn block_ledger(
    block: &BlockV1,
    parent: Option<&BlockV1>,
    coinbase: Option<&Tx>,
    heights: &UpgradeHeights,
) -> BlockLedger {
    let height = block.height.unwrap_or(0);
    let pools = block.pools();
    // Genesis changes the pools from nothing; any other block needs its parent's row too,
    // since zebrad's own change diffs against zero when only the parent lacks one.
    let before = if height == 0 {
        Some([0u64; 6])
    } else {
        parent.and_then(BlockV1::pools)
    };
    let deltas = match (pools, before) {
        (Some(after), Some(before)) => Some(PerPool(std::array::from_fn(|index| {
            after[index] as i64 - before[index] as i64
        }))),
        _ => None,
    };
    let coinbase_in = coinbase.map(coinbase_into_pools);
    let (users, fees) = match (deltas, coinbase_in) {
        (Some(deltas), Some(paid)) => {
            let mut users = PerPool([0i64; 6]);
            let mut claimed = 0i64;
            let mut grown = 0i64;
            for pool in Pool::ALL {
                if pool == Pool::Lockbox {
                    continue;
                }
                let paid = paid.get(pool) as i64;
                users.set(pool, deltas.get(pool) - paid);
                claimed += paid;
                grown += deltas.get(pool);
            }
            (Some(users), u64::try_from(claimed - grown).ok())
        }
        _ => (None, None),
    };
    let burned = fees
        .filter(|_| heights.is_active(Upgrade::Nu7, height))
        .map(burned_share);
    let notes = if height == 0 {
        Some(
            block
                .trees
                .sizes()
                .map(|size| u32::try_from(size).unwrap_or(u32::MAX)),
        )
    } else {
        parent.map(|parent| {
            let (after, before) = (block.trees.sizes(), parent.trees.sizes());
            std::array::from_fn(|index| {
                u32::try_from(after[index].saturating_sub(before[index])).unwrap_or(u32::MAX)
            })
        })
    };
    let budget = if heights.is_active(Upgrade::Nu7, height) {
        match notes {
            // The trees count Sapling outputs and Orchard and Ironwood actions; Sapling spends
            // need the transactions, so this is a lower bound.
            Some(notes) => Budget::Units {
                used: notes.iter().sum(),
                limit: BUDGET_LIMIT,
                complete: false,
            },
            None => Budget::Unknown,
        }
    } else {
        match block.size.and_then(|size| u32::try_from(size).ok()) {
            Some(used) => Budget::Bytes {
                used,
                limit: MAX_BLOCK_BYTES,
            },
            None => Budget::Unknown,
        }
    };
    BlockLedger {
        pools: pools.map(PerPool),
        deltas,
        coinbase: coinbase_in,
        users,
        fees,
        burned,
        notes,
        budget,
    }
}

/// Whether `ledger`, the ledger of the block at `height`, adds up with the block's subsidy
/// (`getblocksubsidy`'s total, in zatoshis).
pub fn ledger_check(
    ledger: &BlockLedger,
    height: u32,
    subsidy: Option<u64>,
    heights: &UpgradeHeights,
) -> LedgerCheck {
    let (Some(deltas), Some(subsidy)) = (ledger.deltas, subsidy) else {
        return LedgerCheck::NotChecked;
    };
    let grown: i64 = deltas.0.iter().sum();
    let subsidy = subsidy as i64;
    if heights.is_active(Upgrade::Nu7, height) {
        match ledger.burned {
            Some(burned) if subsidy - grown == burned as i64 => LedgerCheck::Balanced,
            Some(_) => LedgerCheck::Unverified,
            None => LedgerCheck::NotChecked,
        }
    } else if heights.is_active(Upgrade::Nu6, height) {
        if grown == subsidy {
            LedgerCheck::Balanced
        } else {
            LedgerCheck::Unverified
        }
    } else {
        match subsidy - grown {
            0 => LedgerCheck::Balanced,
            gap if gap > 0 => LedgerCheck::Unclaimed { zat: gap as u64 },
            _ => LedgerCheck::Unverified,
        }
    }
}

#[cfg(test)]
mod tests {
    use launcher_types::explorer::Hash32;

    use super::*;
    use crate::shapes::{Actions, Count, PoolValue, ScriptPubKey, TreeSize, Trees, TxIn, TxOut};

    fn heights() -> UpgradeHeights {
        UpgradeHeights::defaults(Network::Mainnet)
    }

    fn block(height: u32, pools: Option<[u64; 6]>, trees: [u64; 3]) -> BlockV1 {
        BlockV1 {
            hash: Hash32([height as u8; 32]),
            confirmations: 1,
            size: pools.map(|_| 2_000),
            height: Some(height),
            version: Some(4),
            merkle_root: None,
            block_commitments: None,
            final_sapling_root: None,
            final_orchard_root: None,
            tx: Vec::new(),
            time: Some(0),
            nonce: None,
            bits: None,
            difficulty: None,
            chain_supply: None,
            value_pools: pools.map(|pools| {
                Pool::ALL
                    .into_iter()
                    .map(|pool| PoolValue {
                        id: pool.id().into(),
                        chain_value_zat: pools[pool.index()],
                        value_delta_zat: None,
                    })
                    .collect()
            }),
            trees: Trees {
                sapling: TreeSize { size: trees[0] },
                orchard: TreeSize { size: trees[1] },
                ironwood: TreeSize { size: trees[2] },
            },
            previous: None,
            next: None,
        }
    }

    fn coinbase(transparent: i64, ironwood: i64) -> Tx {
        Tx {
            txid: Hash32([9; 32]),
            height: None,
            confirmations: None,
            vin: vec![TxIn {
                coinbase: Some("03".into()),
                txid: None,
                vout: None,
            }],
            vout: vec![TxOut {
                value_zat: transparent,
                n: 0,
                script: ScriptPubKey {
                    kind: "pubkeyhash".into(),
                    addresses: None,
                },
            }],
            sapling_spends: Count(0),
            sapling_outputs: Count(0),
            joinsplits: Vec::new(),
            orchard: None,
            ironwood: (ironwood > 0).then_some(Actions {
                actions: Count(2),
                value_balance: -ironwood,
            }),
            sapling_value_balance: None,
            size: None,
            time: None,
            version: 6,
            lock_time: 0,
            expiry_height: None,
            block_hash: None,
            block_time: None,
        }
    }

    #[test]
    fn the_burned_share_rounds_down() {
        assert_eq!(burned_share(0), 0);
        assert_eq!(burned_share(9), 5);
        assert_eq!(burned_share(10), 6);
        assert_eq!(burned_share(15), 9);
        assert_eq!(burned_share(u64::MAX), u64::MAX / 10 * 6 + 3);
    }

    /// An NU7 block: a subsidy of S, a lockbox deferral L, fees F of which ⌊6F/10⌋ burn.
    #[test]
    fn fees_and_burn_from_the_coinbase_and_the_pools() {
        let heights = heights();
        let height = 3_550_000;
        let (subsidy, deferral, fees) = (52_083_333u64, 6_250_000u64, 210_000u64);
        let burned = burned_share(fees);
        // The coinbase claims the subsidy less the deferral, plus the fees less the burn:
        // 30,000,000 transparent and the rest into Ironwood.
        let claimed = subsidy - deferral + fees - burned;
        let cb = coinbase(30_000_000, (claimed - 30_000_000) as i64);
        // Users moved 5 ZEC from Orchard to Ironwood and paid the fees from Orchard.
        let before = [
            1_000_000_000_000,
            0,
            50_000_000_000,
            90_000_000_000,
            7_000_000_000,
            10_000_000_000,
        ];
        let mut after = before;
        after[Pool::Transparent.index()] += 30_000_000;
        after[Pool::Orchard.index()] -= 500_000_000 + fees;
        after[Pool::Ironwood.index()] += claimed - 30_000_000 + 500_000_000;
        after[Pool::Lockbox.index()] += deferral;
        let parent = block(height - 1, Some(before), [10, 20, 30]);
        let child = block(height, Some(after), [12, 26, 40]);
        let ledger = block_ledger(&child, Some(&parent), Some(&cb), &heights);
        assert_eq!(ledger.fees, Some(fees));
        assert_eq!(ledger.burned, Some(burned));
        assert_eq!(ledger.notes, Some([2, 6, 10]));
        let users = ledger.users.expect("users");
        assert_eq!(users.get(Pool::Orchard), -(500_000_000 + fees as i64));
        assert_eq!(users.get(Pool::Ironwood), 500_000_000);
        assert_eq!(users.get(Pool::Transparent), 0);
        assert_eq!(users.get(Pool::Lockbox), 0);
        assert_eq!(
            ledger.budget,
            Budget::Units {
                used: 18,
                limit: 330,
                complete: false
            }
        );
        assert_eq!(
            ledger_check(&ledger, height, Some(subsidy), &heights),
            LedgerCheck::Balanced
        );
        assert_eq!(
            ledger_check(&ledger, height, Some(subsidy + 1), &heights),
            LedgerCheck::Unverified
        );
        assert_eq!(
            ledger_check(&ledger, height, None, &heights),
            LedgerCheck::NotChecked
        );
    }

    #[test]
    fn a_parent_without_a_row_leaves_the_flows_unknown() {
        let heights = heights();
        let pools = Some([10, 0, 0, 0, 0, 0]);
        let parent = block(3_000_000, None, [1, 1, 0]);
        let child = block(3_000_001, pools, [2, 3, 0]);
        let ledger = block_ledger(&child, Some(&parent), Some(&coinbase(5, 0)), &heights);
        assert_eq!(ledger.pools.map(|pools| pools.0), pools);
        assert_eq!(ledger.deltas, None);
        assert_eq!(ledger.fees, None);
        assert_eq!(ledger.users, None);
        assert_eq!(ledger.notes, Some([1, 2, 0]), "the trees still count notes");
        assert_eq!(
            ledger.budget,
            Budget::Bytes {
                used: 2_000,
                limit: 2_000_000
            }
        );
        // Without its own row the size is unknown too.
        let ledger = block_ledger(&parent, None, None, &heights);
        assert_eq!(ledger.budget, Budget::Unknown);
        assert_eq!(ledger.notes, None);
    }

    #[test]
    fn eras_close_the_books_differently() {
        let heights = heights();
        let ledger = |deltas: [i64; 6], burned: Option<u64>| BlockLedger {
            pools: None,
            deltas: Some(PerPool(deltas)),
            coinbase: None,
            users: None,
            fees: None,
            burned,
            notes: None,
            budget: Budget::Unknown,
        };
        // Before NU6 a miner may claim less.
        assert_eq!(
            ledger_check(
                &ledger([90, 0, 0, 0, 0, 0], None),
                2_000_000,
                Some(100),
                &heights
            ),
            LedgerCheck::Unclaimed { zat: 10 }
        );
        assert_eq!(
            ledger_check(
                &ledger([100, 0, 0, 0, 0, 0], None),
                2_000_000,
                Some(100),
                &heights
            ),
            LedgerCheck::Balanced
        );
        // From NU6 the pools grow by exactly the subsidy.
        assert_eq!(
            ledger_check(
                &ledger([88, 0, 0, 0, 12, 0], None),
                3_000_000,
                Some(100),
                &heights
            ),
            LedgerCheck::Balanced
        );
        assert_eq!(
            ledger_check(
                &ledger([80, 0, 0, 0, 12, 0], None),
                3_000_000,
                Some(100),
                &heights
            ),
            LedgerCheck::Unverified
        );
    }

    #[test]
    fn the_coinbase_pays_its_outputs_into_pools() {
        let paid = coinbase_into_pools(&coinbase(7, 3));
        assert_eq!(paid.get(Pool::Transparent), 7);
        assert_eq!(paid.get(Pool::Ironwood), 3);
        assert_eq!(paid.get(Pool::Lockbox), 0);
        assert_eq!(nsm_seed(Network::Mainnet), 36_858_445_520);
    }
}
