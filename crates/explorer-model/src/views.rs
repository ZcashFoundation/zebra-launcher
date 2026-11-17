//! The explorer's pages (`launcher_types::explorer`) built from zebrad's shapes. The backend
//! and the browser mock both build their replies here, so the two cannot disagree.

use launcher_types::explorer::{
    BlockDetails, BlockLedger, BlockPage, BlockSummary, Card, CoinbaseClaim, FutureBlock, OutPoint,
    Pool, PoolPart, RewardSplit, RewardStream, Shown, SpentOutput, TxInput, TxKind, TxOutput,
    TxPage, TxRow, TxStatus, TxidPage, UnknownWhy, UpgradeCountdown,
};

use crate::{
    ledger::{block_ledger, ledger_check},
    shapes::{BlockV1, Prevout, Subsidy, Tx, TxOut},
    upgrades::{Upgrade, UpgradeHeights},
    zec_to_zat, zip317,
};

/// The most transaction ids one page holds.
pub const TXIDS_PER_PAGE: u16 = 200;

/// A block is read whole, at verbosity 3, up to this many transactions...
pub const WHOLE_BLOCK_TXS: usize = 256;
/// ... and this many bytes; a bigger one row by row.
pub const WHOLE_BLOCK_BYTES: i64 = 512 * 1024;
/// The earlier transactions a Transaction page reads for its inputs without being asked to
/// resolve them all.
pub const PARENTS_AT_ONCE: usize = 16;
/// With `resolve`, up to this many for a mined transaction...
pub const RESOLVED_PARENTS: usize = 1_000;
/// ... and this many for a waiting one.
pub const RESOLVED_WAITING_PARENTS: usize = 200;

/// Whether `block` is read whole at verbosity 3 rather than row by row: a small one.
pub fn read_whole(block: &BlockV1) -> bool {
    block.tx.len() <= WHOLE_BLOCK_TXS && block.size.is_none_or(|size| size <= WHOLE_BLOCK_BYTES)
}

/// The earlier transactions `tx` spends from whose outputs are not in `spent` yet, each once,
/// in the order its inputs name them.
pub fn missing_parents(
    tx: &Tx,
    spent: &[Option<SpentOutput>],
) -> Vec<launcher_types::explorer::TxId> {
    let mut seen = std::collections::HashSet::new();
    tx.vin
        .iter()
        .enumerate()
        .filter(|(index, _)| spent.get(*index).is_none_or(Option::is_none))
        .filter_map(|(_, input)| input.txid)
        .filter(|txid| seen.insert(*txid))
        .collect()
}

/// The outputs `tx`'s inputs spend, from the earlier transactions in `parents`, keeping those
/// in `spent` already.
pub fn fill_spent(
    tx: &Tx,
    spent: &mut [Option<SpentOutput>],
    parent: impl Fn(&launcher_types::explorer::TxId) -> Option<std::sync::Arc<Tx>>,
) {
    for (slot, input) in spent.iter_mut().zip(&tx.vin) {
        if slot.is_none()
            && let (Some(txid), Some(vout)) = (input.txid, input.vout)
            && let Some(parent) = parent(&txid)
        {
            *slot = spent_from_parent(&parent, vout);
        }
    }
}

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

/// The shielded pools `tx` touches: counts and the net value that left each.
pub fn shielded_parts(tx: &Tx) -> Vec<PoolPart> {
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
    shielded
}

/// An output as the input that spends it sees it.
fn spent_output(output: &TxOut, height: Option<u32>, coinbase: bool) -> SpentOutput {
    SpentOutput {
        zat: u64::try_from(output.value_zat).unwrap_or(0),
        address: output
            .script
            .addresses
            .as_ref()
            .and_then(|addresses| addresses.first().cloned()),
        script: output.script.kind.clone(),
        height,
        coinbase,
    }
}

/// The output a `prevout` of `getblock` at verbosity 3 describes.
pub fn spent_from_prevout(prevout: &Prevout) -> SpentOutput {
    SpentOutput {
        zat: u64::try_from(prevout.value_zat).unwrap_or(0),
        address: prevout
            .script
            .addresses
            .as_ref()
            .and_then(|addresses| addresses.first().cloned()),
        script: prevout.script.kind.clone(),
        height: Some(prevout.height),
        coinbase: prevout.generated,
    }
}

/// Output `vout` of `parent`, as read with `getrawtransaction`.
pub fn spent_from_parent(parent: &Tx, vout: u32) -> Option<SpentOutput> {
    let output = parent.vout.iter().find(|output| output.n == vout)?;
    let height = parent
        .height
        .filter(|height| *height >= 0)
        .and_then(|height| u32::try_from(height).ok());
    Some(spent_output(output, height, parent.is_coinbase()))
}

/// The outputs `tx`'s inputs spend, as far as `getblock` at verbosity 3 gave them.
pub fn spent_from_v3(tx: &Tx) -> Vec<Option<SpentOutput>> {
    tx.vin
        .iter()
        .map(|input| input.prevout.as_ref().map(spent_from_prevout))
        .collect()
}

/// The fee of `tx`, given the outputs its inputs spend (`spent`, one per input, in order):
/// what goes in less what comes out. Public once every input's value is known; `None` for a
/// coinbase.
pub fn tx_fee(tx: &Tx, spent: &[Option<SpentOutput>]) -> Option<Shown> {
    if tx.is_coinbase() {
        return None;
    }
    let shielded: i64 = tx.net_out().iter().sum();
    let mut inputs = 0i64;
    for index in 0..tx.vin.len() {
        match spent.get(index).and_then(Option::as_ref) {
            Some(output) => inputs += i64::try_from(output.zat).unwrap_or(i64::MAX),
            None => {
                // zebrad's own fee, when verbosity 3 found every spent output.
                return Some(match tx.fee {
                    Some(zat) => Shown::Public { zat },
                    None => Shown::Unknown {
                        why: UnknownWhy::InputValues,
                    },
                });
            }
        }
    }
    Some(Shown::Public {
        zat: inputs + shielded - tx.transparent_out(),
    })
}

/// One row of a block's transaction table: `tx` at `index`, with the outputs its inputs
/// spend as far as they are known. `rules_height` is as for [`tx_kind`].
pub fn tx_row(
    tx: &Tx,
    index: u32,
    spent: &[Option<SpentOutput>],
    rules_height: u32,
    heights: &UpgradeHeights,
) -> TxRow {
    TxRow {
        index,
        txid: tx.txid,
        kind: tx_kind(tx, rules_height, heights),
        version: tx.version,
        size: tx.size.and_then(|size| u32::try_from(size).ok()),
        inputs: u32::try_from(tx.vin.len()).unwrap_or(u32::MAX),
        outputs: u32::try_from(tx.vout.len()).unwrap_or(u32::MAX),
        shielded: shielded_parts(tx),
        actions: zip317::parts(tx),
        fee: tx_fee(tx, spent),
    }
}

/// The Transaction page, with the outputs its inputs spend as far as they are known
/// (`spent`, one per input, in order). `rules_height` is as for [`tx_kind`]. The caller
/// sets [`TxPage::resolvable`] and, for a coinbase, [`TxPage::coinbase`].
pub fn tx_page(
    tx: &Tx,
    spent: &[Option<SpentOutput>],
    rules_height: u32,
    heights: &UpgradeHeights,
) -> TxPage {
    let coinbase = tx.is_coinbase();
    let inputs: Vec<TxInput> = tx
        .vin
        .iter()
        .enumerate()
        .map(|(index, input)| TxInput {
            prevout: input
                .txid
                .zip(input.vout)
                .map(|(txid, vout)| OutPoint { txid, vout }),
            spent: spent.get(index).cloned().flatten(),
        })
        .collect();
    let unresolved = if coinbase {
        0
    } else {
        inputs.iter().filter(|input| input.spent.is_none()).count() as u32
    };
    TxPage {
        txid: tx.txid,
        status: tx_status(tx),
        kind: tx_kind(tx, rules_height, heights),
        version: tx.version,
        size: tx.size.and_then(|size| u32::try_from(size).ok()),
        lock_time: tx.lock_time,
        expiry_height: tx.expiry_height.filter(|height| *height != 0),
        inputs,
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
        shielded: shielded_parts(tx),
        fee: tx_fee(tx, spent),
        actions: zip317::parts(tx),
        unresolved,
        resolvable: false,
        burns_fees: heights.is_active(Upgrade::Nu7, rules_height),
        coinbase: None,
    }
}

/// What a block's coinbase claimed, from the block's ledger and its subsidy (zatoshis).
pub fn coinbase_claim(ledger: &BlockLedger, subsidy: Option<u64>) -> CoinbaseClaim {
    CoinbaseClaim {
        subsidy,
        fees: ledger.fees,
        burned: ledger.burned,
        lockbox: ledger.deltas.map(|deltas| deltas.get(Pool::Lockbox)),
    }
}

/// A block's hover card.
pub fn block_card(block: &BlockV1) -> Card {
    Card::Block {
        height: block.height.unwrap_or(0),
        hash: block.hash,
        time: block
            .time
            .map_or(0, |time| u64::try_from(time).unwrap_or(0)),
        tx_count: u32::try_from(block.tx.len()).unwrap_or(u32::MAX),
        size: block.size.and_then(|size| u32::try_from(size).ok()),
    }
}

/// A transaction's hover card. `rules_height` is as for [`tx_kind`].
pub fn tx_card(tx: &Tx, rules_height: u32, heights: &UpgradeHeights) -> Card {
    Card::Tx {
        txid: tx.txid,
        kind: tx_kind(tx, rules_height, heights),
        status: tx_status(tx),
        inputs: u32::try_from(tx.vin.len()).unwrap_or(u32::MAX),
        outputs: u32::try_from(tx.vout.len()).unwrap_or(u32::MAX),
        pools: shielded_parts(tx)
            .into_iter()
            .map(|part| part.pool)
            .collect(),
        size: tx.size.and_then(|size| u32::try_from(size).ok()),
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
    use crate::shapes::{Actions, Count, HexLen, JoinSplit, ScriptPubKey, ScriptSig, TxIn};

    fn heights() -> UpgradeHeights {
        UpgradeHeights::defaults(Network::Mainnet)
    }

    fn tx() -> Tx {
        Tx {
            txid: Hash32([1; 32]),
            height: Some(3_550_000),
            confirmations: Some(3),
            fee: None,
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
            script_sig: Some(ScriptSig { hex: HexLen(107) }),
            prevout: None,
        }
    }

    fn output(zat: i64) -> TxOut {
        TxOut {
            value_zat: zat,
            n: 0,
            script: ScriptPubKey {
                kind: "pubkeyhash".into(),
                addresses: Some(vec!["t1x".into()]),
                hex: HexLen(25),
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
                script_sig: None,
                prevout: None,
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
        let page = tx_page(&deshielding, &[], 3_550_000, &heights);
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
        let page = tx_page(&spending, &[], 3_550_000, &heights);
        assert_eq!(
            page.fee,
            Some(Shown::Unknown {
                why: UnknownWhy::InputValues
            })
        );
        assert_eq!(page.inputs[0].prevout.map(|out| out.vout), Some(0));
        assert_eq!(page.unresolved, 1);
        assert!(page.burns_fees, "3,550,000 is past NU7");
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

    fn spent(zat: u64) -> SpentOutput {
        SpentOutput {
            zat,
            address: Some("t1y".into()),
            script: "pubkeyhash".into(),
            height: Some(3_549_000),
            coinbase: true,
        }
    }

    #[test]
    fn fees_come_from_the_spent_outputs() {
        let heights = heights();
        // Two inputs of 60 and 50 shield 100 into Ironwood, with 7 of change: a fee of 3.
        let shielding = Tx {
            vin: vec![
                input(),
                TxIn {
                    vout: Some(1),
                    ..input()
                },
            ],
            vout: vec![output(7)],
            ironwood: actions(2, -100),
            ..tx()
        };
        let both = [Some(spent(60)), Some(spent(50))];
        assert_eq!(tx_fee(&shielding, &both), Some(Shown::Public { zat: 3 }));
        let page = tx_page(&shielding, &both, 3_550_000, &heights);
        assert_eq!(page.unresolved, 0);
        assert_eq!(page.inputs[1].spent, Some(spent(50)));
        // One value missing: unknown, unless zebrad's verbosity 3 gave the fee.
        let one = [Some(spent(60)), None];
        assert_eq!(
            tx_fee(&shielding, &one),
            Some(Shown::Unknown {
                why: UnknownWhy::InputValues
            })
        );
        assert_eq!(tx_page(&shielding, &one, 3_550_000, &heights).unresolved, 1);
        let with_fee = Tx {
            fee: Some(3),
            ..shielding.clone()
        };
        assert_eq!(tx_fee(&with_fee, &one), Some(Shown::Public { zat: 3 }));
        // The row counts the same.
        let row = tx_row(&shielding, 4, &both, 3_550_000, &heights);
        assert_eq!(row.index, 4);
        assert_eq!((row.inputs, row.outputs), (2, 1));
        assert_eq!(row.fee, Some(Shown::Public { zat: 3 }));
        assert_eq!(row.shielded.len(), 1);
        assert_eq!(row.actions.ironwood_actions, 2);
        assert_eq!(row.actions.in_bytes, 2 * 148);
        assert_eq!(zip317::logical_actions(&row.actions), 2 + 2);
        // A coinbase pays no fee.
        let coinbase = Tx {
            vin: vec![TxIn {
                coinbase: Some("03aabbcc".into()),
                txid: None,
                vout: None,
                script_sig: None,
                prevout: None,
            }],
            vout: vec![output(100)],
            ..tx()
        };
        assert_eq!(tx_fee(&coinbase, &[]), None);
        assert_eq!(tx_page(&coinbase, &[], 3_550_000, &heights).unresolved, 0);
    }

    #[test]
    fn spent_outputs_read_alike_from_a_parent_and_from_verbosity_3() {
        let parent = Tx {
            vout: vec![output(5), TxOut { n: 1, ..output(9) }],
            ..tx()
        };
        let from_parent = spent_from_parent(&parent, 1).expect("output 1");
        assert_eq!(from_parent.zat, 9);
        assert_eq!(from_parent.height, Some(3_550_000));
        assert!(!from_parent.coinbase);
        assert_eq!(spent_from_parent(&parent, 2), None);
        let prevout = Prevout {
            generated: false,
            height: 3_550_000,
            value_zat: 9,
            script: output(9).script,
        };
        assert_eq!(spent_from_prevout(&prevout), from_parent);
        // A parent still in the mempool has no height.
        let waiting = Tx {
            height: None,
            ..parent
        };
        assert_eq!(spent_from_parent(&waiting, 0).unwrap().height, None);
    }

    #[test]
    fn parents_are_named_once_and_small_blocks_are_read_whole() {
        let other = TxIn {
            txid: Some(Hash32([4; 32])),
            ..input()
        };
        let tx = Tx {
            vin: vec![input(), other.clone(), input(), other],
            ..tx()
        };
        assert_eq!(
            missing_parents(&tx, &[]),
            vec![Hash32([3; 32]), Hash32([4; 32])]
        );
        let known = [Some(spent(1)), None, Some(spent(1)), None];
        assert_eq!(missing_parents(&tx, &known), vec![Hash32([4; 32])]);
        let mut slots = known.clone();
        let parent = Tx {
            vout: vec![output(9)],
            ..tx.clone()
        };
        fill_spent(&tx, &mut slots, |txid| {
            (*txid == Hash32([4; 32])).then(|| std::sync::Arc::new(parent.clone()))
        });
        assert_eq!(slots[1].as_ref().map(|spent| spent.zat), Some(9));
        assert_eq!(slots[0], Some(spent(1)), "kept");
        let block = |txs: u32, size: Option<i64>| BlockV1 {
            hash: Hash32([0; 32]),
            confirmations: 1,
            size,
            height: Some(1),
            version: None,
            merkle_root: None,
            block_commitments: None,
            final_sapling_root: None,
            final_orchard_root: None,
            tx: (0..txs).map(|i| Hash32([i as u8; 32])).collect(),
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
        assert!(read_whole(&block(256, Some(512 * 1024))));
        assert!(read_whole(&block(10, None)), "a size zebrad does not know");
        assert!(!read_whole(&block(257, Some(1_000))));
        assert!(!read_whole(&block(10, Some(600_000))));
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
