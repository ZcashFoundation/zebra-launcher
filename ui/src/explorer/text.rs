//! The Explorer's words and numbers: amounts in ZEC (TAZ on Testnet), budgets, intervals,
//! search hints and refusals. Pure, tested natively.

use explorer_model::classify::{Classified, Later};
use launcher_types::{
    Network,
    explorer::{Budget, NotFoundHint, PerPool, Pool, SecretKind},
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

/// What users moved into the shielded pools in a block, net of what they took out.
pub fn net_shielding(users: &PerPool<i64>) -> i64 {
    Pool::SHIELDED.iter().map(|pool| users.get(*pool)).sum()
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

fn capitalise(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
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
