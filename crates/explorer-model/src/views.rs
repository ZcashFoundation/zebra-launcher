//! The explorer's pages (`launcher_types::explorer`) built from zebrad's shapes. The backend
//! and the browser mock both build their replies here, so the two cannot disagree.

use launcher_types::explorer::{
    BlockDetails, BlockPage, BlockSummary, FutureBlock, OutPoint, Pool, PoolPart, RewardSplit,
    RewardStream, Shown, TxInput, TxKind, TxOutput, TxPage, TxStatus, TxidPage, UnknownWhy,
    UpgradeCountdown,
};

use crate::{
    ledger::{block_ledger, ledger_check},
    shapes::{BlockV1, Subsidy, Tx},
    upgrades::{Upgrade, UpgradeHeights},
    zec_to_zat,
};

/// The most transaction ids one page holds.
pub const TXIDS_PER_PAGE: u16 = 200;

/// A block in a list, from the block, its parent and its coinbase.
pub fn block_summary(
    block: &BlockV1,
    parent: Option<&BlockV1>,
    coinbase: Option<&Tx>,
    heights: &UpgradeHeights,
) -> BlockSummary {
    BlockSummary {
        height: block.height.unwrap_or(0),
        hash: block.hash,
        time: block
            .time
            .map_or(0, |time| u64::try_from(time).unwrap_or(0)),
        interval: block
            .time
            .zip(parent.and_then(|parent| parent.time))
            .map(|(time, parent)| time - parent),
        size: block.size.and_then(|size| u32::try_from(size).ok()),
        tx_count: u32::try_from(block.tx.len()).unwrap_or(u32::MAX),
        ledger: block_ledger(block, parent, coinbase, heights),
    }
}

/// `limit` of `block`'s transaction ids from `offset`, at most [`TXIDS_PER_PAGE`].
pub fn txid_page(block: &BlockV1, offset: u32, limit: u16) -> TxidPage {
    let total = u32::try_from(block.tx.len()).unwrap_or(u32::MAX);
    let start = (offset as usize).min(block.tx.len());
    let end = (start + usize::from(limit.min(TXIDS_PER_PAGE))).min(block.tx.len());
    TxidPage {
        offset: start as u32,
        total,
        txids: block.tx[start..end].to_vec(),
    }
}

/// Where the block's subsidy went, with the fees its ledger found.
pub fn reward_split(subsidy: &Subsidy, fees: Option<u64>, burned: Option<u64>) -> RewardSplit {
    let zat = |zec: f64| u64::try_from(zec_to_zat(zec)).unwrap_or(0);
    let stream = |stream: &crate::shapes::Stream, lockbox: bool| RewardStream {
        recipient: stream.recipient.clone(),
        zat: u64::try_from(stream.value_zat).unwrap_or(0),
        address: stream.address.clone(),
        lockbox,
    };
    let mut streams: Vec<RewardStream> = subsidy
        .funding_streams
        .iter()
        .map(|item| stream(item, false))
        .collect();
    streams.extend(
        subsidy
            .lockbox_streams
            .iter()
            .map(|item| stream(item, true)),
    );
    if subsidy.founders > 0.0 {
        streams.push(RewardStream {
            recipient: "Founders' reward".into(),
            zat: zat(subsidy.founders),
            address: None,
            lockbox: false,
        });
    }
    RewardSplit {
        subsidy: zat(subsidy.total),
        miner: zat(subsidy.miner),
        streams,
        fees_to_miner: fees.map(|fees| fees - burned.unwrap_or(0)),
        burned,
    }
}

/// The Block page. `subsidy` is `getblocksubsidy` for the block, when zebrad answered.
pub fn block_page(
    block: &BlockV1,
    parent: Option<&BlockV1>,
    coinbase: Option<&Tx>,
    subsidy: Option<&Subsidy>,
    heights: &UpgradeHeights,
) -> BlockPage {
    let summary = block_summary(block, parent, coinbase, heights);
    let height = summary.height;
    let subsidy_zat = subsidy.and_then(|subsidy| u64::try_from(zec_to_zat(subsidy.total)).ok());
    BlockPage {
        previous: block.previous,
        next: block.next,
        era: heights
            .current(height)
            .map(|upgrade| upgrade.name().to_owned()),
        activates: heights
            .activating_at(height)
            .map(|upgrade| upgrade.name().to_owned()),
        reward: subsidy
            .map(|subsidy| reward_split(subsidy, summary.ledger.fees, summary.ledger.burned)),
        check: ledger_check(&summary.ledger, height, subsidy_zat, heights),
        details: BlockDetails {
            version: block.version,
            merkle_root: block.merkle_root.clone(),
            block_commitments: block.block_commitments.clone(),
            final_sapling_root: block.final_sapling_root.clone(),
            final_orchard_root: block.final_orchard_root.clone(),
            bits: block.bits.clone(),
            nonce: block.nonce.clone(),
            difficulty: block.difficulty,
            trees: block.trees.sizes(),
            chain_supply: block
                .chain_supply
                .as_ref()
                .map(|supply| supply.chain_value_zat),
        },
        txids: txid_page(block, 0, TXIDS_PER_PAGE),
        summary,
    }
}

/// Where the transaction is, from zebrad's `height` and `blockhash`.
pub fn tx_status(tx: &Tx) -> TxStatus {
    match (tx.height, tx.block_hash) {
        (Some(height), Some(block)) if height >= 0 => TxStatus::Mined {
            block,
            height: u32::try_from(height).unwrap_or(u32::MAX),
            time: tx
                .block_time
                .or(tx.time)
                .and_then(|time| u64::try_from(time).ok()),
        },
        (Some(_), block) => TxStatus::SideChain { block },
        (None, _) => TxStatus::Mempool,
    }
}

/// What the transaction does with value. `rules_height` is the height whose rules apply: its
/// block's, or for a waiting transaction the next block's.
pub fn tx_kind(tx: &Tx, rules_height: u32, heights: &UpgradeHeights) -> TxKind {
    let net = tx.net_out();
    if tx.is_coinbase() {
        let shielded_to = Pool::SHIELDED
            .into_iter()
            .zip(net)
            .filter(|(_, net)| *net < 0)
            .map(|(pool, _)| pool)
            .collect();
        return TxKind::Coinbase { shielded_to };
    }
    let parts: Vec<(Pool, i64)> = Pool::SHIELDED
        .into_iter()
        .zip(net)
        .zip(tx.touches())
        .filter(|(_, touched)| *touched)
        .map(|(part, _)| part)
        .collect();
    if parts.is_empty() {
        return TxKind::Transparent;
    }
    let into: Vec<Pool> = parts
        .iter()
        .filter(|(_, net)| *net < 0)
        .map(|p| p.0)
        .collect();
    let from: Vec<Pool> = parts
        .iter()
        .filter(|(_, net)| *net > 0)
        .map(|p| p.0)
        .collect();
    let transparent = !tx.vin.is_empty() || !tx.vout.is_empty();
    if !transparent {
        if let [(pool, _)] = parts.as_slice() {
            return TxKind::Shielded {
                pool: *pool,
                same_receiver: *pool == Pool::Orchard
                    && heights.is_active(Upgrade::Nu6_3, rules_height),
            };
        }
        if let ([from], [to], 2) = (from.as_slice(), into.as_slice(), parts.len()) {
            let moved = parts
                .iter()
                .find(|(pool, _)| pool == to)
                .map_or(0, |(_, net)| net.unsigned_abs());
            return TxKind::Migration {
                from: *from,
                to: *to,
                zat: moved,
            };
        }
        return TxKind::Mixed;
    }
    if !into.is_empty() && from.is_empty() && !tx.vin.is_empty() {
        return TxKind::Shielding {
            into,
            transparent_change: !tx.vout.is_empty(),
        };
    }
    if !from.is_empty() && into.is_empty() && tx.vin.is_empty() && !tx.vout.is_empty() {
        return TxKind::Deshielding { from };
    }
    TxKind::Mixed
}

/// The Transaction page, without the values of the outputs its inputs spend. `rules_height`
/// is as for [`tx_kind`].
pub fn tx_page(tx: &Tx, rules_height: u32, heights: &UpgradeHeights) -> TxPage {
    let net = tx.net_out();
    let touches = tx.touches();
    let mut shielded = Vec::new();
    for (index, pool) in Pool::SHIELDED.into_iter().enumerate() {
        if !touches[index] {
            continue;
        }
        let mut part = PoolPart {
            pool,
            spends: 0,
            outputs: 0,
            actions: 0,
            joinsplits: 0,
            net_out: net[index],
        };
        match pool {
            Pool::Sprout => part.joinsplits = u32::try_from(tx.joinsplits.len()).unwrap_or(0),
            Pool::Sapling => {
                part.spends = tx.sapling_spends.0;
                part.outputs = tx.sapling_outputs.0;
            }
            Pool::Orchard => part.actions = tx.orchard.map_or(0, |bundle| bundle.actions.0),
            Pool::Ironwood => part.actions = tx.ironwood.map_or(0, |bundle| bundle.actions.0),
            Pool::Transparent | Pool::Lockbox => {}
        }
        shielded.push(part);
    }
    let coinbase = tx.is_coinbase();
    let fee = if coinbase {
        None
    } else if tx.vin.is_empty() {
        // No transparent inputs: every zatoshi going in is public.
        Some(Shown::Public {
            zat: net.iter().sum::<i64>() - tx.transparent_out(),
        })
    } else {
        Some(Shown::Unknown {
            why: UnknownWhy::InputValues,
        })
    };
    TxPage {
        txid: tx.txid,
        status: tx_status(tx),
        kind: tx_kind(tx, rules_height, heights),
        version: tx.version,
        size: tx.size.and_then(|size| u32::try_from(size).ok()),
        lock_time: tx.lock_time,
        expiry_height: tx.expiry_height.filter(|height| *height != 0),
        inputs: tx
            .vin
            .iter()
            .map(|input| TxInput {
                prevout: input
                    .txid
                    .zip(input.vout)
                    .map(|(txid, vout)| OutPoint { txid, vout }),
            })
            .collect(),
        outputs: tx
            .vout
            .iter()
            .map(|output| TxOutput {
                n: output.n,
                zat: u64::try_from(output.value_zat).unwrap_or(0),
                address: output
                    .script
                    .addresses
                    .as_ref()
                    .and_then(|addresses| addresses.first().cloned()),
                script: output.script.kind.clone(),
            })
            .collect(),
        shielded,
        fee,
    }
}

/// A height above the node's tip `tip`: how far away it is and what happens on the way,
/// counted from `network_tip`. That is the node's tip once it has caught up, and zebrad's
/// estimate of the network's while it syncs, so the blocks the node has yet to download do not
/// count as the future.
pub fn future_block(
    height: u32,
    tip: u32,
    network_tip: u32,
    heights: &UpgradeHeights,
    end_of_support: Option<u64>,
) -> FutureBlock {
    let from = network_tip.max(tip);
    FutureBlock {
        height,
        tip,
        blocks_left: height.saturating_sub(from),
        eta_secs: heights.spacing_between(from, height),
        activates: heights
            .activating_at(height)
            .map(|upgrade| upgrade.name().to_owned()),
        upgrades_before: heights
            .between(from, height.saturating_sub(1))
            .into_iter()
            .map(|(upgrade, start)| (upgrade.name().to_owned(), start))
            .collect(),
        end_of_support,
    }
}

/// The next upgrade above `tip`, with the time its blocks take.
pub fn next_upgrade(tip: u32, heights: &UpgradeHeights) -> Option<UpgradeCountdown> {
    let (upgrade, height) = heights.next_after(tip)?;
    Some(UpgradeCountdown {
        name: upgrade.name().to_owned(),
        height,
        blocks_left: height - tip,
        eta_secs: heights.spacing_between(tip, height),
    })
}

/// The average seconds between blocks from `(height, time)` of an older block to a newer one.
pub fn average_interval(older: (u32, i64), newer: (u32, i64)) -> Option<f64> {
    let blocks = newer.0.checked_sub(older.0).filter(|blocks| *blocks > 0)?;
    Some((newer.1 - older.1) as f64 / f64::from(blocks))
}

/// What NU7 removed from circulation up to the tip: the subsidies of the blocks since NU7
/// minus the growth of the supply over them. Known only while one subsidy covers the whole
/// stretch (no halving since NU7), which `subsidy_at_nu7` and `subsidy_at_tip` show.
pub fn burned_since_nu7(
    nu7: u32,
    tip: u32,
    subsidy_at_nu7: u64,
    subsidy_at_tip: u64,
    supply_before_nu7: u64,
    supply_at_tip: u64,
) -> Option<u64> {
    if tip < nu7 || subsidy_at_nu7 != subsidy_at_tip {
        return None;
    }
    let issued = subsidy_at_tip.checked_mul(u64::from(tip - nu7 + 1))?;
    let grown = supply_at_tip.checked_sub(supply_before_nu7)?;
    issued.checked_sub(grown)
}

#[cfg(test)]
mod tests {
    use launcher_types::{Network, explorer::Hash32};

    use super::*;
    use crate::shapes::{Actions, Count, JoinSplit, ScriptPubKey, TxIn, TxOut};

    fn heights() -> UpgradeHeights {
        UpgradeHeights::defaults(Network::Mainnet)
    }

    fn tx() -> Tx {
        Tx {
            txid: Hash32([1; 32]),
            height: Some(3_550_000),
            confirmations: Some(3),
            vin: Vec::new(),
            vout: Vec::new(),
            sapling_spends: Count(0),
            sapling_outputs: Count(0),
            joinsplits: Vec::new(),
            orchard: None,
            ironwood: None,
            sapling_value_balance: None,
            size: Some(2_000),
            time: None,
            version: 5,
            lock_time: 0,
            expiry_height: Some(0),
            block_hash: Some(Hash32([2; 32])),
            block_time: Some(1_800_000_000),
        }
    }

    fn input() -> TxIn {
        TxIn {
            coinbase: None,
            txid: Some(Hash32([3; 32])),
            vout: Some(0),
        }
    }

    fn output(zat: i64) -> TxOut {
        TxOut {
            value_zat: zat,
            n: 0,
            script: ScriptPubKey {
                kind: "pubkeyhash".into(),
                addresses: Some(vec!["t1x".into()]),
            },
        }
    }

    fn actions(count: u32, value_balance: i64) -> Option<Actions> {
        Some(Actions {
            actions: Count(count),
            value_balance,
        })
    }

    #[test]
    fn kinds_follow_the_value_flow() {
        let heights = heights();
        let rules = 3_550_000;
        assert_eq!(tx_kind(&tx(), rules, &heights), TxKind::Transparent);

        let transparent = Tx {
            vin: vec![input()],
            vout: vec![output(5)],
            ..tx()
        };
        assert_eq!(tx_kind(&transparent, rules, &heights), TxKind::Transparent);

        let orchard = Tx {
            orchard: actions(2, 10_000),
            ..tx()
        };
        assert_eq!(
            tx_kind(&orchard, rules, &heights),
            TxKind::Shielded {
                pool: Pool::Orchard,
                same_receiver: true
            }
        );
        assert_eq!(
            tx_kind(&orchard, 3_000_000, &heights),
            TxKind::Shielded {
                pool: Pool::Orchard,
                same_receiver: false
            },
            "before NU6.3 an Orchard action may pay anyone"
        );

        let migration = Tx {
            orchard: actions(2, 100_010_000),
            ironwood: actions(2, -100_000_000),
            ..tx()
        };
        assert_eq!(
            tx_kind(&migration, rules, &heights),
            TxKind::Migration {
                from: Pool::Orchard,
                to: Pool::Ironwood,
                zat: 100_000_000
            }
        );

        let shielding = Tx {
            vin: vec![input()],
            vout: vec![output(7)],
            ironwood: actions(2, -50),
            ..tx()
        };
        assert_eq!(
            tx_kind(&shielding, rules, &heights),
            TxKind::Shielding {
                into: vec![Pool::Ironwood],
                transparent_change: true
            }
        );

        let deshielding = Tx {
            vout: vec![output(40)],
            sapling_spends: Count(1),
            sapling_value_balance: Some(50),
            ..tx()
        };
        assert_eq!(
            tx_kind(&deshielding, rules, &heights),
            TxKind::Deshielding {
                from: vec![Pool::Sapling]
            }
        );

        let mixed = Tx {
            vin: vec![input()],
            vout: vec![output(1)],
            orchard: actions(2, 10),
            ironwood: actions(2, -5),
            ..tx()
        };
        assert_eq!(tx_kind(&mixed, rules, &heights), TxKind::Mixed);

        let coinbase = Tx {
            vin: vec![TxIn {
                coinbase: Some("03".into()),
                txid: None,
                vout: None,
            }],
            vout: vec![output(100)],
            ironwood: actions(2, -20),
            ..tx()
        };
        assert_eq!(
            tx_kind(&coinbase, rules, &heights),
            TxKind::Coinbase {
                shielded_to: vec![Pool::Ironwood]
            }
        );
    }

    #[test]
    fn a_page_shows_counts_and_public_values_only() {
        let heights = heights();
        let deshielding = Tx {
            vout: vec![output(40)],
            sapling_spends: Count(1),
            sapling_outputs: Count(2),
            sapling_value_balance: Some(50),
            joinsplits: vec![JoinSplit {
                vpub_old: 0,
                vpub_new: 3,
            }],
            ..tx()
        };
        let page = tx_page(&deshielding, 3_550_000, &heights);
        assert_eq!(page.expiry_height, None, "0 is no expiry");
        assert_eq!(page.shielded.len(), 2);
        assert_eq!(page.shielded[0].pool, Pool::Sprout);
        assert_eq!(page.shielded[0].joinsplits, 1);
        assert_eq!(page.shielded[1].outputs, 2);
        assert_eq!(page.shielded[1].net_out, 50);
        // No transparent inputs: the fee is public (50 + 3 out of the pools, 40 paid out).
        assert_eq!(page.fee, Some(Shown::Public { zat: 13 }));
        assert_eq!(page.outputs[0].address.as_deref(), Some("t1x"));
        assert!(matches!(
            page.status,
            TxStatus::Mined {
                height: 3_550_000,
                ..
            }
        ));

        let spending = Tx {
            vin: vec![input()],
            vout: vec![output(1)],
            ..tx()
        };
        let page = tx_page(&spending, 3_550_000, &heights);
        assert_eq!(
            page.fee,
            Some(Shown::Unknown {
                why: UnknownWhy::InputValues
            })
        );
        assert_eq!(page.inputs[0].prevout.map(|out| out.vout), Some(0));
        let waiting = Tx {
            height: None,
            block_hash: None,
            ..tx()
        };
        assert_eq!(tx_status(&waiting), TxStatus::Mempool);
        let side = Tx {
            height: Some(-1),
            ..tx()
        };
        assert!(matches!(tx_status(&side), TxStatus::SideChain { .. }));
    }

    #[test]
    fn future_heights_count_down_across_eras() {
        let heights = heights();
        let future = future_block(3_543_407, 3_543_403, 3_543_403, &heights, Some(3_700_000));
        assert_eq!(future.blocks_left, 4);
        assert_eq!(future.eta_secs, 75 + 25 * 3);
        assert_eq!(future.upgrades_before, vec![("NU7".to_owned(), 3_543_405)]);
        assert_eq!(future.activates, None);
        // A node still syncing at 2,000,000 counts from the network's tip, not its own.
        let syncing = future_block(3_551_010, 2_000_000, 3_551_000, &heights, None);
        assert_eq!(syncing.blocks_left, 10);
        assert_eq!(syncing.eta_secs, 250);
        assert!(syncing.upgrades_before.is_empty(), "already active");
        assert_eq!(
            syncing.tip, 2_000_000,
            "the link goes to a block the node has"
        );
        let at = future_block(3_543_405, 3_543_400, 3_543_400, &heights, None);
        assert_eq!(at.activates.as_deref(), Some("NU7"));
        assert!(at.upgrades_before.is_empty());
        let next = next_upgrade(3_500_000, &heights).expect("NU7 is ahead");
        assert_eq!((next.name.as_str(), next.blocks_left), ("NU7", 43_405));
        assert_eq!(next_upgrade(3_600_000, &heights), None);
    }

    #[test]
    fn averages_and_the_burn() {
        assert_eq!(average_interval((100, 1_000), (200, 8_500)), Some(75.0));
        assert_eq!(average_interval((200, 1_000), (200, 8_500)), None);
        // 100 blocks of 2 ZEC, a supply that grew by 199.9 ZEC: 0.1 ZEC burned.
        assert_eq!(
            burned_since_nu7(
                1_000,
                1_099,
                200_000_000,
                200_000_000,
                5_000,
                5_000 + 19_990_000_000
            ),
            Some(10_000_000)
        );
        assert_eq!(burned_since_nu7(1_000, 999, 1, 1, 0, 0), None, "before NU7");
        assert_eq!(
            burned_since_nu7(1_000, 1_099, 2, 1, 0, 0),
            None,
            "a halving since"
        );
    }

    #[test]
    fn transaction_id_pages() {
        let block = BlockV1 {
            hash: Hash32([0; 32]),
            confirmations: 1,
            size: None,
            height: Some(1),
            version: None,
            merkle_root: None,
            block_commitments: None,
            final_sapling_root: None,
            final_orchard_root: None,
            tx: (0..450u32).map(|i| Hash32([i as u8; 32])).collect(),
            time: None,
            nonce: None,
            bits: None,
            difficulty: None,
            chain_supply: None,
            value_pools: None,
            trees: Default::default(),
            previous: None,
            next: None,
        };
        let first = txid_page(&block, 0, 500);
        assert_eq!(
            (first.offset, first.total, first.txids.len()),
            (0, 450, 200)
        );
        let last = txid_page(&block, 400, 200);
        assert_eq!(last.txids.len(), 50);
        assert_eq!(txid_page(&block, 900, 200).txids.len(), 0);
    }
}
