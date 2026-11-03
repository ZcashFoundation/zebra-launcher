//! The Explorer's words and numbers: amounts in ZEC (TAZ on Testnet), pool movements, block
//! and transaction sentences, budgets, confirmations, search hints and refusals. Pure, tested
//! natively. A hidden amount has no number here: callers show "hidden" instead.

use explorer_model::classify::{Classified, Later};
use launcher_types::{
    Network, Phase,
    explorer::{
        BlockSummary, Budget, LedgerCheck, NotFoundHint, PerPool, Pool, ReorgNotice, SecretKind,
        Shown, TxKind, TxPage, UnknownWhy,
    },
};

use crate::format::{bytes, thousands};

/// Blocks this deep are final in Zebra.
pub const FINAL_DEPTH: u32 = 1_000;

/// The unit amounts read in on `network`: Testnet coins are not ZEC.
pub fn unit(network: Network) -> &'static str {
    match network {
        Network::Mainnet => "ZEC",
        Network::Testnet => "TAZ",
    }
}

/// An exact amount in whole coins, without trailing zeros: `1.5625`, `0.0010007`, `2`,
/// `-0.5`, `14,654,995.76514816`.
pub fn coins(zat: i64) -> String {
    let sign = if zat < 0 { "-" } else { "" };
    let magnitude = zat.unsigned_abs();
    let whole = thousands(magnitude / 100_000_000);
    let fraction = format!("{:08}", magnitude % 100_000_000);
    let fraction = fraction.trim_end_matches('0');
    if fraction.is_empty() {
        format!("{sign}{whole}")
    } else {
        format!("{sign}{whole}.{fraction}")
    }
}

/// [`coins`] with the unit: `1.5625 ZEC`.
pub fn amount(zat: i64, unit: &str) -> String {
    format!("{} {unit}", coins(zat))
}

/// A large amount rounded to two decimals from 1,000 coins up, exact below: pool balances and
/// the supply.
pub fn amount_rounded(zat: u64, unit: &str) -> String {
    if zat < 100_000_000_000 {
        return amount(i64::try_from(zat).unwrap_or(i64::MAX), unit);
    }
    let hundredths = (zat + 500_000) / 1_000_000;
    format!(
        "{}.{:02} {unit}",
        thousands(hundredths / 100),
        hundredths % 100
    )
}

/// What a transaction or a block did to one pool, as a movement: `1.2 ZEC entered the
/// Ironwood pool`, `0.5 ZEC left the Orchard pool`.
pub fn movement(pool: Pool, into: i64, unit: &str) -> String {
    match into {
        0 => format!("No change in the {} pool", pool.label()),
        into if into > 0 => format!("{} entered the {} pool", amount(into, unit), pool.label()),
        out => format!("{} left the {} pool", amount(-out, unit), pool.label()),
    }
}

/// What users moved into the shielded pools in a block, net of what they took out.
pub fn net_shielding(users: &PerPool<i64>) -> i64 {
    Pool::SHIELDED.iter().map(|pool| users.get(*pool)).sum()
}

/// The net shielding as words: `13.4 ZEC into shielded pools`.
pub fn shielding_text(net: i64, unit: &str) -> String {
    match net {
        0 => "nothing in or out of shielded pools".to_owned(),
        net if net > 0 => format!("{} into shielded pools", amount(net, unit)),
        net => format!("{} out of shielded pools", amount(-net, unit)),
    }
}

/// The notes a block created, `212 note commitments`, or `None` when unknown.
pub fn notes_text(notes: Option<[u32; 3]>) -> Option<String> {
    let total: u32 = notes?.iter().sum();
    Some(match total {
        1 => "1 note commitment".to_owned(),
        n => format!("{} note commitments", thousands(u64::from(n))),
    })
}

/// A block in one sentence: `37 transactions · 212 note commitments · users moved 13.4 ZEC
/// into shielded pools · fees 0.0021 ZEC (0.00126 burned)`.
pub fn block_sentence(summary: &BlockSummary, unit: &str) -> String {
    let ledger = &summary.ledger;
    let mut parts = vec![match summary.tx_count {
        1 => "1 transaction".to_owned(),
        n => format!("{} transactions", thousands(u64::from(n))),
    }];
    parts.extend(notes_text(ledger.notes));
    if let Some(users) = &ledger.users {
        parts.push(format!(
            "users moved {}",
            shielding_text(net_shielding(users), unit)
        ));
    }
    match (ledger.fees, ledger.burned) {
        (Some(fees), Some(burned)) => parts.push(format!(
            "fees {} ({} burned)",
            amount(fees as i64, unit),
            coins(burned as i64)
        )),
        (Some(fees), None) => parts.push(format!("fees {}", amount(fees as i64, unit))),
        (None, _) => parts.push("fees unknown".to_owned()),
    }
    parts.join(" · ")
}

/// How full a block is: `≥ 39 of 330`, `70.6 kB of 2.0 MB`.
pub fn budget_text(budget: &Budget) -> String {
    match *budget {
        Budget::Units {
            used,
            limit,
            complete,
        } => {
            let at_least = if complete { "" } else { "≥ " };
            format!("{at_least}{used} of {limit}")
        }
        Budget::Bytes { used, limit } => {
            format!("{} of {}", bytes(u64::from(used)), bytes(u64::from(limit)))
        }
        Budget::Unknown => "unknown".to_owned(),
    }
}

/// A budget as a fraction of its limit, for a meter.
pub fn budget_fraction(budget: &Budget) -> Option<f64> {
    match *budget {
        Budget::Units { used, limit, .. } | Budget::Bytes { used, limit } if limit > 0 => {
            Some(f64::from(used) / f64::from(limit))
        }
        _ => None,
    }
}

/// The era's check as a caption.
pub fn check_text(check: &LedgerCheck, unit: &str) -> String {
    match check {
        LedgerCheck::Balanced => "Pool changes, the subsidy and the fees add up ✓".to_owned(),
        LedgerCheck::Unclaimed { zat } => format!(
            "The miner left {} of the subsidy and fees unclaimed",
            amount(*zat as i64, unit)
        ),
        LedgerCheck::Unverified => {
            "Unverified: the pool changes, the subsidy and the fees do not add up".to_owned()
        }
        LedgerCheck::NotChecked => {
            "Not checked: the subsidy or the pool changes are unknown".to_owned()
        }
    }
}

fn pools_list(pools: &[Pool]) -> String {
    let names: Vec<&str> = pools.iter().map(|pool| pool.label()).collect();
    match names.as_slice() {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// A transaction kind's badge: `Shielding`, `Orchard → Ironwood`.
pub fn kind_label(kind: &TxKind) -> String {
    match kind {
        TxKind::Coinbase { .. } => "Coinbase".to_owned(),
        TxKind::Transparent => "Transparent".to_owned(),
        TxKind::Shielded { pool, .. } => format!("Fully shielded · {}", pool.label()),
        TxKind::Migration { from, to, .. } => format!("{} → {}", from.label(), to.label()),
        TxKind::Shielding { .. } => "Shielding".to_owned(),
        TxKind::Deshielding { .. } => "Deshielding".to_owned(),
        TxKind::Mixed => "Mixed".to_owned(),
    }
}

/// A transaction in a sentence, from its public parts only.
pub fn tx_sentence(page: &TxPage, unit: &str) -> String {
    let counts: Vec<String> = page
        .shielded
        .iter()
        .map(|part| match part.pool {
            Pool::Sapling => format!(
                "{} Sapling spend{} and {} output{}",
                part.spends,
                plural(part.spends),
                part.outputs,
                plural(part.outputs)
            ),
            Pool::Sprout => format!("{} JoinSplit{}", part.joinsplits, plural(part.joinsplits)),
            pool => format!(
                "{} {} action{}",
                part.actions,
                pool.label(),
                plural(part.actions)
            ),
        })
        .collect();
    let what = match &page.kind {
        TxKind::Coinbase { shielded_to } if shielded_to.is_empty() => {
            "The block's coinbase: the subsidy and the fees, paid to transparent outputs."
                .to_owned()
        }
        TxKind::Coinbase { shielded_to } => format!(
            "The block's coinbase, paying into {}; shielded coinbase amounts are public by \
             consensus.",
            pools_list(shielded_to)
        ),
        TxKind::Transparent => "Moved value between transparent addresses.".to_owned(),
        TxKind::Shielded { pool, .. } => {
            format!("Moved value within the {} pool.", pool.label())
        }
        TxKind::Migration { from, to, zat } => format!(
            "Moved {} from {} to {}.",
            amount(*zat as i64, unit),
            from.label(),
            to.label()
        ),
        TxKind::Shielding {
            into,
            transparent_change,
        } => {
            let change = if *transparent_change {
                ", with transparent change"
            } else {
                ""
            };
            format!("Moved transparent value into {}{change}.", pools_list(into))
        }
        TxKind::Deshielding { from } => {
            format!(
                "Moved value out of {} to transparent outputs.",
                pools_list(from)
            )
        }
        TxKind::Mixed => "Moved value between transparent and shielded pools.".to_owned(),
    };
    let mut sentence = what;
    if !counts.is_empty() {
        sentence.push_str(&format!(
            " {}; amounts per note hidden.",
            capitalise(&counts.join(" and "))
        ));
    }
    match page.fee {
        Some(Shown::Public { zat }) => sentence.push_str(&format!(" Fee {}.", amount(zat, unit))),
        Some(Shown::Unknown {
            why: UnknownWhy::InputValues,
        }) => sentence.push_str(" The fee needs the values of the spent outputs."),
        Some(Shown::Unknown { .. } | Shown::Hidden) | None => {}
    }
    sentence
}

fn plural(count: u32) -> &'static str {
    if count == 1 { "" } else { "s" }
}

fn capitalise(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Confirmations of a block at `height` with the node's tip at `tip`: the block itself counts.
pub fn confirmations(height: u32, tip: u32) -> u32 {
    tip.saturating_sub(height)
        .saturating_add(1)
        .min(if height > tip { 0 } else { u32::MAX })
}

/// Zebra's finality: a block 1,000 deep is final; above that it is provisional, `deep` blocks
/// down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Finality {
    Final,
    Provisional { deep: u32 },
}

pub fn finality(height: u32, tip: u32) -> Finality {
    let deep = tip.saturating_sub(height);
    if deep >= FINAL_DEPTH {
        Finality::Final
    } else {
        Finality::Provisional { deep }
    }
}

pub fn finality_text(finality: Finality) -> String {
    match finality {
        Finality::Final => "Final in Zebra".to_owned(),
        Finality::Provisional { deep } => format!("Provisional · {deep} of 1,000 deep"),
    }
}

/// Where a confirmation count sits on the meter's log scale from 1 to 1,001, from 0 to 1.
pub fn confirmations_fraction(confirmations: u32) -> f64 {
    if confirmations == 0 {
        return 0.0;
    }
    (f64::from(confirmations).log10() / 1_001f64.log10()).min(1.0)
}

/// `about 2 h 5 min`, `about 4 d 3 h`, `under a minute`.
pub fn eta_text(secs: u64) -> String {
    if secs < 60 {
        return "under a minute".to_owned();
    }
    format!(
        "about {}",
        crate::charts::uptime_text(std::time::Duration::from_secs(secs))
    )
}

/// The network difficulty: whole with separators from a thousand up, two decimals below.
pub fn difficulty(difficulty: f64) -> String {
    if !difficulty.is_finite() || difficulty < 0.0 {
        "unknown".to_owned()
    } else if difficulty >= 1000.0 {
        thousands(difficulty.round() as u64)
    } else {
        format!("{difficulty:.2}")
    }
}

/// The seconds between two blocks: `1 min 15 s`, `-5 s` (miners set the times).
pub fn interval_text(secs: i64) -> String {
    let sign = if secs < 0 { "-" } else { "" };
    let magnitude = secs.unsigned_abs();
    match magnitude {
        0..60 => format!("{sign}{magnitude} s"),
        _ => format!("{sign}{} min {} s", magnitude / 60, magnitude % 60),
    }
}

/// The banner's words for a reorganisation the explorer noticed at `at`: how many blocks were
/// replaced, or at least how many when its checks could not tell where the chains part, and
/// above which height.
pub fn reorg_text(reorg: &ReorgNotice, at: &str) -> String {
    let blocks = match reorg.replaced {
        1 => "1 block was".to_owned(),
        n => format!("{} blocks were", thousands(u64::from(n))),
    };
    let shared = thousands(u64::from(reorg.fork_height));
    if reorg.first_replaced <= reorg.fork_height.saturating_add(1) {
        format!(
            "{} replaced at {at}: the node switched to another branch above {shared}.",
            capitalise(&blocks)
        )
    } else {
        format!(
            "At least {blocks} replaced at {at}: the node switched to another branch above a \
             height between {shared} and {}.",
            thousands(u64::from(reorg.first_replaced - 1))
        )
    }
}

/// What the banner says while the node is not running, in its phase's words; `None` while it
/// runs.
pub fn not_running_text(phase: &Phase) -> Option<&'static str> {
    Some(match phase {
        Phase::Running => return None,
        Phase::Stopped => "The node is not running, so the Explorer cannot read its chain.",
        Phase::Starting => "The node is starting; the Explorer reads its chain once it runs.",
        Phase::Stopping => "The node is stopping, so the Explorer cannot read its chain.",
        Phase::Backoff { .. } => {
            "The node stopped and restarts shortly; the Explorer reads its chain once it runs."
        }
        Phase::Failed { .. } => "The node failed, so the Explorer cannot read its chain.",
    })
}

/// The hint under the search box, as the query is typed.
pub fn hint(classified: &Classified, tip: Option<u32>, network: Network) -> String {
    match classified {
        Classified::Empty => "A height, a block hash or a transaction id".to_owned(),
        Classified::Secret(_) => "A key or a recovery phrase · Zeal never takes keys".to_owned(),
        Classified::Height(height) => match tip {
            Some(tip) if *height > tip => format!(
                "Height · past your node's tip at {}",
                thousands(u64::from(tip))
            ),
            Some(tip) => format!("Height · your node is at {}", thousands(u64::from(tip))),
            None => "Height".to_owned(),
        },
        Classified::BelowTip(0) => "The latest block".to_owned(),
        Classified::BelowTip(back) => format!("{back} blocks below the tip"),
        Classified::Hash(_) => "A block hash or a transaction id".to_owned(),
        Classified::Outpoint(_) => "A transaction output".to_owned(),
        Classified::Route(_) => "A page of the Explorer".to_owned(),
        Classified::Later(later) if later.other_network() => format!(
            "{} of the other network; this node is on {network}",
            capitalise(later.label())
        ),
        Classified::Later(Later::HashPrefix) => {
            "The start of a hash · type all 64 digits to look it up".to_owned()
        }
        Classified::Later(later) => format!(
            "{} · the Explorer looks these up in a later version",
            capitalise(later.label())
        ),
        Classified::Unrecognised => "Not a height, a block hash or a transaction id".to_owned(),
    }
}

/// What the search box says after refusing a secret. The field is cleared and nothing is kept.
pub fn refusal(kind: SecretKind) -> &'static str {
    match kind {
        SecretKind::ViewingKey => {
            "This looks like a viewing key. Zeal never needs it, and it was not saved."
        }
        SecretKind::SpendingKey => {
            "This looks like a spending key. Zeal never needs it, and it was not saved. Anyone \
             who has a spending key can spend your funds."
        }
        SecretKind::RecoveryPhrase => {
            "This looks like a recovery phrase. Zeal never needs it, and it was not saved. \
             Anyone who has a recovery phrase can spend your funds."
        }
    }
}

/// Why a search found nothing.
pub fn not_found_text(hint: &NotFoundHint) -> String {
    match hint {
        NotFoundHint::PastNodeTip { node_height } => format!(
            "Your node has blocks up to {}.",
            thousands(u64::from(*node_height))
        ),
        NotFoundHint::UnknownHash => "Not a block in your node's best chain, nor a transaction \
                                      in its chain or mempool. Your node only knows the \
                                      transactions it has received."
            .to_owned(),
        NotFoundHint::NotSearchable { what } => {
            format!("This looks like {what}. The Explorer looks these up in a later version.")
        }
        NotFoundHint::Refused { what } => refusal(*what).to_owned(),
        NotFoundHint::Unrecognised => "Not a height, a block hash or a transaction id.".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use launcher_types::explorer::{BlockLedger, Hash32, PoolPart, TxStatus};

    use super::*;

    #[test]
    fn amounts_are_exact_and_say_their_unit() {
        assert_eq!(coins(156_250_000), "1.5625");
        assert_eq!(coins(100_070), "0.0010007");
        assert_eq!(coins(200_000_000), "2");
        assert_eq!(coins(-50_000_000), "-0.5");
        assert_eq!(coins(0), "0");
        assert_eq!(coins(1_465_499_576_514_816), "14,654,995.76514816");
        assert_eq!(amount(1, "TAZ"), "0.00000001 TAZ");
        assert_eq!(unit(Network::Testnet), "TAZ");
        assert_eq!(unit(Network::Mainnet), "ZEC");
        assert_eq!(
            amount_rounded(1_465_499_576_514_816, "ZEC"),
            "14,654,995.77 ZEC"
        );
        assert_eq!(amount_rounded(12_345_678, "ZEC"), "0.12345678 ZEC");
    }

    #[test]
    fn movements_read_as_value_entering_or_leaving() {
        assert_eq!(
            movement(Pool::Ironwood, 120_000_000, "ZEC"),
            "1.2 ZEC entered the Ironwood pool"
        );
        assert_eq!(
            movement(Pool::Orchard, -50_000_000, "ZEC"),
            "0.5 ZEC left the Orchard pool"
        );
        assert_eq!(
            movement(Pool::Sapling, 0, "ZEC"),
            "No change in the Sapling pool"
        );
    }

    fn summary(users: Option<[i64; 6]>, fees: Option<u64>, burned: Option<u64>) -> BlockSummary {
        BlockSummary {
            height: 3_550_000,
            hash: Hash32([0; 32]),
            time: 0,
            interval: Some(30),
            size: Some(70_000),
            tx_count: 37,
            ledger: BlockLedger {
                pools: None,
                deltas: None,
                coinbase: None,
                users: users.map(PerPool),
                fees,
                burned,
                notes: Some([10, 2, 200]),
                budget: Budget::Units {
                    used: 212,
                    limit: 330,
                    complete: false,
                },
            },
        }
    }

    #[test]
    fn a_block_in_a_sentence() {
        let block = summary(
            Some([-1_350_000_000, 0, 340_000_000, 0, 0, 1_000_000_000]),
            Some(210_000),
            Some(126_000),
        );
        assert_eq!(
            block_sentence(&block, "ZEC"),
            "37 transactions · 212 note commitments · users moved 13.4 ZEC into shielded pools · \
             fees 0.0021 ZEC (0.00126 burned)"
        );
        let unknown = summary(None, None, None);
        assert_eq!(
            block_sentence(&unknown, "TAZ"),
            "37 transactions · 212 note commitments · fees unknown"
        );
        assert_eq!(budget_text(&block.ledger.budget), "≥ 212 of 330");
        assert_eq!(
            budget_text(&Budget::Bytes {
                used: 70_553,
                limit: 2_000_000
            }),
            "70.6 kB of 2.0 MB"
        );
        assert_eq!(budget_fraction(&Budget::Unknown), None);
    }

    #[test]
    fn transactions_in_a_sentence_never_show_hidden_amounts() {
        let page = TxPage {
            txid: Hash32([1; 32]),
            status: TxStatus::Mempool,
            kind: TxKind::Migration {
                from: Pool::Orchard,
                to: Pool::Ironwood,
                zat: 10_000_000_000,
            },
            version: 6,
            size: Some(9_000),
            lock_time: 0,
            expiry_height: None,
            inputs: Vec::new(),
            outputs: Vec::new(),
            shielded: vec![
                PoolPart {
                    pool: Pool::Orchard,
                    spends: 0,
                    outputs: 0,
                    actions: 2,
                    joinsplits: 0,
                    net_out: 10_000_004_000,
                },
                PoolPart {
                    pool: Pool::Ironwood,
                    spends: 0,
                    outputs: 0,
                    actions: 2,
                    joinsplits: 0,
                    net_out: -10_000_000_000,
                },
            ],
            fee: Some(Shown::Public { zat: 4_000 }),
        };
        assert_eq!(
            tx_sentence(&page, "ZEC"),
            "Moved 100 ZEC from Orchard to Ironwood. 2 Orchard actions and 2 Ironwood actions; \
             amounts per note hidden. Fee 0.00004 ZEC."
        );
        assert_eq!(kind_label(&page.kind), "Orchard → Ironwood");
        let unknown_fee = TxPage {
            fee: Some(Shown::Unknown {
                why: UnknownWhy::InputValues,
            }),
            kind: TxKind::Shielding {
                into: vec![Pool::Sapling, Pool::Orchard],
                transparent_change: true,
            },
            ..page
        };
        assert!(tx_sentence(&unknown_fee, "ZEC").starts_with(
            "Moved transparent value into Sapling and Orchard, with transparent change."
        ));
    }

    #[test]
    fn confirmations_and_finality() {
        assert_eq!(confirmations(100, 100), 1);
        assert_eq!(confirmations(90, 100), 11);
        assert_eq!(confirmations(101, 100), 0);
        assert_eq!(finality(100, 1_099), Finality::Provisional { deep: 999 });
        assert_eq!(finality(100, 1_100), Finality::Final);
        assert_eq!(
            finality_text(Finality::Provisional { deep: 3 }),
            "Provisional · 3 of 1,000 deep"
        );
        assert_eq!(confirmations_fraction(0), 0.0);
        assert_eq!(confirmations_fraction(1), 0.0);
        assert_eq!(confirmations_fraction(1_001), 1.0);
        assert!(confirmations_fraction(10) > 0.3 && confirmations_fraction(10) < 0.35);
    }

    #[test]
    fn difficulty_reads_whole_or_with_two_decimals() {
        assert_eq!(difficulty(118_462_377.5), "118,462,378");
        assert_eq!(difficulty(42.123), "42.12");
        assert_eq!(difficulty(f64::NAN), "unknown");
    }

    #[test]
    fn times_and_intervals() {
        assert_eq!(eta_text(30), "under a minute");
        assert_eq!(eta_text(7_500), "about 2 h 5 min");
        assert_eq!(interval_text(75), "1 min 15 s");
        assert_eq!(interval_text(-5), "-5 s");
    }

    #[test]
    fn reorganisations_say_only_what_the_checks_showed() {
        let exact = ReorgNotice {
            fork_height: 3_550_998,
            first_replaced: 3_550_999,
            replaced: 2,
            epoch: 1,
            at: 0,
        };
        assert_eq!(
            reorg_text(&exact, "14:02"),
            "2 blocks were replaced at 14:02: the node switched to another branch above 3,550,998."
        );
        let bounded = ReorgNotice {
            fork_height: 3_550_000,
            first_replaced: 3_551_000,
            replaced: 1,
            ..exact
        };
        assert_eq!(
            reorg_text(&bounded, "14:02"),
            "At least 1 block was replaced at 14:02: the node switched to another branch above a \
             height between 3,550,000 and 3,550,999."
        );
    }

    #[test]
    fn the_banner_follows_the_phase() {
        assert_eq!(not_running_text(&Phase::Running), None);
        assert!(
            not_running_text(&Phase::Starting)
                .unwrap()
                .contains("is starting")
        );
        assert!(
            not_running_text(&Phase::Stopped)
                .unwrap()
                .contains("not running")
        );
    }

    #[test]
    fn hints_and_refusals() {
        let net = Network::Mainnet;
        assert_eq!(
            hint(&Classified::Height(3_601_234), Some(3_601_234), net),
            "Height · your node is at 3,601,234"
        );
        assert_eq!(
            hint(&Classified::Height(4_000_000), Some(3_601_234), net),
            "Height · past your node's tip at 3,601,234"
        );
        assert_eq!(
            hint(
                &Classified::Later(Later::Transparent {
                    other_network: true
                }),
                None,
                net
            ),
            "A transparent address of the other network; this node is on Mainnet"
        );
        assert!(hint(&Classified::Secret(SecretKind::ViewingKey), None, net).contains("never"));
        assert!(refusal(SecretKind::RecoveryPhrase).contains("not saved"));
        assert_eq!(
            not_found_text(&NotFoundHint::PastNodeTip {
                node_height: 2_951_020
            }),
            "Your node has blocks up to 2,951,020."
        );
    }
}
