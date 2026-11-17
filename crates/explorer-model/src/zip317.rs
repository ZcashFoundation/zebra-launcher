//! ZIP 317 as zebrad 7.0.0 applies it (zebra-chain/src/transaction/unmined/zip317.rs): a
//! transaction's logical actions, the conventional fee, unpaid actions and the weight ratio
//! zebrad's block template picks transactions by. zebrad's rules, not the ZIP text's: a
//! 1,000-zatoshi marginal fee, a weight ratio capped at 10, and no unpaid actions in the
//! mempool (zip317.rs:23, :36, :50, :177-190).

use launcher_types::explorer::LogicalActions;

use crate::shapes::Tx;

/// The fee per logical action, in zatoshis.
pub const MARGINAL_FEE: u64 = 1_000;
/// The logical actions every transaction pays for at least.
pub const GRACE_ACTIONS: u32 = 2;
/// The bytes of a standard pay-to-public-key-hash input, one action's worth of inputs.
pub const STANDARD_INPUT_BYTES: u32 = 150;
/// The bytes of a standard pay-to-public-key-hash output, one action's worth of outputs.
pub const STANDARD_OUTPUT_BYTES: u32 = 34;
/// The cap on the weight ratio.
pub const WEIGHT_RATIO_CAP: f32 = 10.0;

/// What ZIP 317 counts in `tx`.
pub fn parts(tx: &Tx) -> LogicalActions {
    LogicalActions {
        in_bytes: tx.vin.iter().map(|input| input.serialized_size()).sum(),
        out_bytes: tx.vout.iter().map(|output| output.serialized_size()).sum(),
        joinsplits: u32::try_from(tx.joinsplits.len()).unwrap_or(u32::MAX),
        sapling_spends: tx.sapling_spends.0,
        sapling_outputs: tx.sapling_outputs.0,
        orchard_actions: tx.orchard.map_or(0, |bundle| bundle.actions.0),
        ironwood_actions: tx.ironwood.map_or(0, |bundle| bundle.actions.0),
    }
}

/// The transparent part's actions: the larger of ⌈input bytes / 150⌉ and ⌈output bytes /
/// 34⌉.
pub fn transparent_actions(parts: &LogicalActions) -> u32 {
    parts
        .in_bytes
        .div_ceil(STANDARD_INPUT_BYTES)
        .max(parts.out_bytes.div_ceil(STANDARD_OUTPUT_BYTES))
}

/// The logical actions: the transparent part's, two per JoinSplit, the larger of Sapling's
/// spends and outputs, and every Orchard and Ironwood action. This is the sum zebrad's
/// `conventional_actions` takes before its `max(2, ·)`.
pub fn logical_actions(parts: &LogicalActions) -> u32 {
    transparent_actions(parts)
        .saturating_add(parts.joinsplits.saturating_mul(2))
        .saturating_add(parts.sapling_spends.max(parts.sapling_outputs))
        .saturating_add(parts.orchard_actions)
        .saturating_add(parts.ironwood_actions)
}

/// The actions a fee is due for: at least [`GRACE_ACTIONS`].
pub fn paid_actions(logical: u32) -> u32 {
    logical.max(GRACE_ACTIONS)
}

/// The conventional fee for `logical` actions: 1,000 zatoshis for each, at least two.
pub fn conventional_fee(logical: u32) -> u64 {
    MARGINAL_FEE * u64::from(paid_actions(logical))
}

/// The actions a fee of `fee` leaves unpaid; zebrad's mempool takes none with any.
pub fn unpaid_actions(logical: u32, fee: u64) -> u32 {
    let paid = u32::try_from(fee / MARGINAL_FEE).unwrap_or(u32::MAX);
    paid_actions(logical).saturating_sub(paid)
}

/// The weight zebrad's block template gives a transaction: its fee over the conventional
/// fee, with at least one zatoshi counted, capped at 10.
pub fn weight_ratio(fee: u64, conventional: u64) -> f32 {
    (fee.max(1) as f32 / conventional.max(1) as f32).min(WEIGHT_RATIO_CAP)
}

/// The fee over the conventional fee, uncapped: how many times it pays.
pub fn multiple(fee: u64, conventional: u64) -> f64 {
    fee as f64 / conventional.max(1) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts_with(f: impl FnOnce(&mut LogicalActions)) -> LogicalActions {
        let mut parts = LogicalActions::default();
        f(&mut parts);
        parts
    }

    #[test]
    fn logical_actions_follow_zebrads_rule() {
        // One standard input and two standard outputs: two actions either way.
        let transparent = parts_with(|p| {
            p.in_bytes = 148;
            p.out_bytes = 68;
        });
        assert_eq!(transparent_actions(&transparent), 2);
        assert_eq!(logical_actions(&transparent), 2);
        // Three inputs outweigh one output.
        let consolidating = parts_with(|p| {
            p.in_bytes = 3 * 148;
            p.out_bytes = 34;
        });
        assert_eq!(logical_actions(&consolidating), 3);
        // Each shielded part adds its own: JoinSplits twice, Sapling's larger side, Orchard and
        // Ironwood actions alike (Ironwood is counted as Orchard is).
        let everything = parts_with(|p| {
            p.in_bytes = 148;
            p.joinsplits = 1;
            p.sapling_spends = 1;
            p.sapling_outputs = 3;
            p.orchard_actions = 2;
            p.ironwood_actions = 2;
        });
        assert_eq!(logical_actions(&everything), 1 + 2 + 3 + 2 + 2);
        // Nothing at all still pays for two.
        let empty = LogicalActions::default();
        assert_eq!(logical_actions(&empty), 0);
        assert_eq!(conventional_fee(0), 2_000);
    }

    #[test]
    fn fees_against_the_conventional_fee() {
        assert_eq!(conventional_fee(4), 4_000);
        assert_eq!(unpaid_actions(4, 4_000), 0);
        assert_eq!(unpaid_actions(4, 3_999), 1);
        assert_eq!(unpaid_actions(1, 0), 2, "the grace actions are due too");
        assert_eq!(weight_ratio(20_000, 4_000), 5.0);
        assert_eq!(weight_ratio(1_000_000, 4_000), WEIGHT_RATIO_CAP);
        assert_eq!(weight_ratio(0, 2_000), 0.0005, "at least a zatoshi counts");
        assert_eq!(multiple(1_000_000, 4_000), 250.0, "uncapped");
    }
}
