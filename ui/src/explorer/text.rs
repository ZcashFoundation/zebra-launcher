//! The Explorer's words: the unit amounts read in, search hints and refusals. Pure, tested
//! natively.

use explorer_model::classify::{Classified, Later};
use launcher_types::{
    Network,
    explorer::{NotFoundHint, SecretKind},
};

use crate::format::thousands;

/// Blocks this deep are final in Zebra.
pub const FINAL_DEPTH: u32 = 1_000;

/// The unit amounts read in on `network`: Testnet coins are not ZEC.
pub fn unit(network: Network) -> &'static str {
    match network {
        Network::Mainnet => "ZEC",
        Network::Testnet => "TAZ",
    }
}

fn capitalise(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
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
