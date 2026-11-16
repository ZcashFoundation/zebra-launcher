//! The herd's words (docs/HERD.md, UI): a node's state in a few words and in a sentence, and
//! one sentence per network. Plain words first ("different chain", "stuck"), node names, and
//! heights as "block 3,561,498".

use std::time::Duration;

use launcher_types::{
    Network, SYNCED_PROGRESS,
    herd::{BlockRef, Branch, ExcludedWhy, Family, HerdState, NodeInHerd, Relation, Side, Vote},
};

use super::engine::agrees;

/// `3551204` as `3,551,204`.
pub fn thousands(value: u32) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// A duration in round words: "45 s", "6 min", "2 h".
pub fn rough(duration: Duration) -> String {
    let secs = duration.as_secs();
    if secs < 90 {
        format!("{secs} s")
    } else if secs < 90 * 60 {
        format!("{} min", (secs + 30) / 60)
    } else {
        format!("{} h", (secs + 1_800) / 3_600)
    }
}

fn block(height: u32) -> String {
    format!("block {}", thousands(height))
}

fn blocks_behind(blocks: u32) -> String {
    match blocks {
        1 => "1 block behind".to_owned(),
        blocks => format!("{} blocks behind", thousands(blocks)),
    }
}

/// A node's state as a few words for its card, and as a sentence with its `name`.
pub fn describe(name: &str, relation: &Relation) -> (String, String) {
    let both = |word: String, rest: &str| (word, format!("{name} {rest}"));
    match relation {
        Relation::Excluded { why } => {
            let word = match why {
                ExcludedWhy::Stopped => "stopped",
                ExcludedWhy::Starting => "starting",
                ExcludedWhy::Restarting => "restarting",
                ExcludedWhy::Stopping => "stopping",
                ExcludedWhy::EndOfSupport => "halted at its end of support",
                ExcludedWhy::NoStats => "not answering its RPC",
                ExcludedWhy::NotCounted => "not counted",
            };
            let rest = match why {
                ExcludedWhy::EndOfSupport => word.to_owned(),
                _ => format!("is {word}"),
            };
            both(word.to_owned(), &rest)
        }
        Relation::CatchingUp {
            same_chain: Some(false),
            ..
        } => both(
            "on another chain".to_owned(),
            "is on another chain while it catches up: another network or a damaged database",
        ),
        Relation::CatchingUp { progress, .. } if *progress >= SYNCED_PROGRESS => {
            both("being checked".to_owned(), "is being checked")
        }
        Relation::CatchingUp {
            progress,
            same_chain,
        } => {
            // Rounded down to the tenth, like the dashboard, so it never reads 100.0 % early.
            let tenths = (progress.clamp(0.0, 1.0) * 1000.0 + 1e-6).floor() / 10.0;
            let chain = if *same_chain == Some(true) {
                " · same chain"
            } else {
                ""
            };
            (
                format!("catching up {tenths:.1} %{chain}"),
                format!("{name} is catching up ({tenths:.1} %)"),
            )
        }
        Relation::InAgreement { behind: 0 } => both("agrees".to_owned(), "agrees"),
        Relation::InAgreement { behind } | Relation::Lagging { blocks: behind } => {
            let words = blocks_behind(*behind);
            both(words.clone(), &format!("is {words}"))
        }
        Relation::Racing { at } => both(
            "agrees".to_owned(),
            &format!("has a competing block at {} (normal)", block(*at)),
        ),
        Relation::Stalled {
            at,
            replaced_block: false,
        } => {
            let words = format!("stuck at {}", block(*at));
            both(words.clone(), &format!("is {words}"))
        }
        Relation::Stalled {
            at,
            replaced_block: true,
        } => {
            let words = format!("stuck on {}, which your other nodes replaced", block(*at));
            both(words.clone(), &format!("is {words}"))
        }
        Relation::Forked {
            at,
            side: Side::Minority,
            deep: false,
            ..
        } => {
            let words = format!("on a different chain since {}", block(*at));
            both(words.clone(), &format!("is {words}"))
        }
        Relation::Forked {
            at,
            side: Side::Minority,
            deep: true,
            ..
        } => (
            format!("on a different chain since before {}", block(*at)),
            format!(
                "{name} is on a different chain since before {} and can't rejoin it by itself",
                block(*at)
            ),
        ),
        Relation::Forked { at, .. } => {
            let words = format!("disagrees since {}", block(*at));
            both(words.clone(), &words)
        }
        Relation::Unknown => both("not answering".to_owned(), "is not answering"),
    }
}

/// What the herd's sentence is made of.
pub struct Herd<'a> {
    pub network: Network,
    pub state: HerdState,
    pub top: Option<&'a BlockRef>,
    pub nodes: &'a [NodeInHerd],
    pub branches: &'a [Branch],
    /// How long the top has not moved.
    pub quiet_for: Duration,
}

/// "Your 3 nodes", "your other node".
fn nodes_phrase(count: usize, network: Network, other: bool) -> String {
    let net = network.as_str();
    match (count, other) {
        (1, false) => format!("your {net} node"),
        (1, true) => format!("your other {net} node"),
        (count, false) => format!("your {count} {net} nodes"),
        (count, true) => format!("your other {count} {net} nodes"),
    }
}

fn capitalized(text: String) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => text,
    }
}

/// One sentence for the network: "Your 3 Mainnet nodes agree on block 3,551,204", or who is
/// behind, stuck or on another chain, with the fork's height.
pub fn headline(herd: &Herd<'_>) -> String {
    let net = herd.network.as_str();
    let witnesses: Vec<&NodeInHerd> = herd
        .nodes
        .iter()
        .filter(|node| node.vote == Vote::Witness)
        .collect();
    let running = |node: &&&NodeInHerd| match node.relation {
        Relation::Excluded { why } => why == ExcludedWhy::Restarting,
        _ => true,
    };
    let at_tip = |node: &&&NodeInHerd| {
        !matches!(
            node.relation,
            Relation::Excluded { .. } | Relation::CatchingUp { .. } | Relation::Unknown
        )
    };
    // The sentences of the witnesses that do not agree.
    let others = |skip: &dyn Fn(&NodeInHerd) -> bool| -> Vec<String> {
        witnesses
            .iter()
            .filter(|node| !agrees(node) && !skip(node))
            .map(|node| node.sentence.clone())
            .collect()
    };
    let with_others = |main: String, rest: Vec<String>| -> String {
        std::iter::once(main)
            .chain(rest)
            .collect::<Vec<_>>()
            .join("; ")
    };
    let top = herd.top.map_or_else(String::new, |top| block(top.height));
    match herd.state {
        HerdState::Off => String::new(),
        HerdState::Checking => {
            let syncing = witnesses.iter().any(|node| {
                matches!(node.relation, Relation::CatchingUp { progress, .. } if progress < SYNCED_PROGRESS)
            });
            if syncing {
                format!("Your {net} nodes are catching up")
            } else {
                format!("Checking your {net} nodes…")
            }
        }
        HerdState::Single => with_others(
            format!("One {net} node checks the chain, at {top}"),
            others(&|node| at_tip(&&node)),
        ),
        HerdState::Agreed => {
            let count = witnesses.iter().filter(at_tip).count();
            with_others(
                capitalized(format!(
                    "{} agree on {top}",
                    nodes_phrase(count, herd.network, false)
                )),
                others(&|_| false),
            )
        }
        HerdState::Degraded => {
            let running: Vec<&&NodeInHerd> = witnesses.iter().filter(running).collect();
            let agreeing = running.iter().filter(|node| agrees(node)).count();
            with_others(
                format!(
                    "{agreeing} of your {} {net} nodes agree on {top}",
                    running.len()
                ),
                others(&|_| false),
            )
        }
        HerdState::Split => split_headline(herd, &witnesses, &top),
        HerdState::Stalled => format!(
            "No new block on your {net} nodes for {}",
            rough(herd.quiet_for)
        ),
    }
}

fn split_headline(herd: &Herd<'_>, witnesses: &[&NodeInHerd], top: &str) -> String {
    let net = herd.network.as_str();
    let forked: Vec<&NodeInHerd> = witnesses
        .iter()
        .copied()
        .filter(|node| matches!(node.relation, Relation::Forked { .. }))
        .collect();
    let at = forked
        .iter()
        .filter_map(|node| match node.relation {
            Relation::Forked { at, .. } => Some(at),
            _ => None,
        })
        .min()
        .unwrap_or(0);
    let side_of = |node: &NodeInHerd| match node.relation {
        Relation::Forked { side, .. } => Some(side),
        _ => None,
    };
    if forked
        .iter()
        .any(|node| side_of(node) == Some(Side::Implementations))
    {
        let on_main = |node: &&NodeInHerd| {
            herd.branches
                .first()
                .is_some_and(|main| main.nodes.contains(&node.id))
        };
        let (main_side, other_side): (Vec<&NodeInHerd>, Vec<&NodeInHerd>) =
            forked.iter().copied().partition(on_main);
        if main_side.len() == 1 && other_side.len() == 1 {
            // Neither side is the main one: the node list's order.
            let mut pair = [main_side[0], other_side[0]];
            let place = |node: &NodeInHerd| herd.nodes.iter().position(|n| n.id == node.id);
            pair.sort_by_key(|node| place(node));
            return format!(
                "{} and {} disagree since {}: one of them may have lost touch with the network, or they disagree about the rules",
                pair[0].name,
                pair[1].name,
                block(at)
            );
        }
        let channels = {
            let mut zebra: Vec<Family> = witnesses
                .iter()
                .map(|node| node.family)
                .filter(|family| matches!(family, Family::Zebra { .. }))
                .collect();
            zebra.dedup();
            zebra.len() > 1
        };
        let label = |side: &[&NodeInHerd]| {
            let mut labels: Vec<String> = side
                .iter()
                .map(|node| node.family.label(channels))
                .collect();
            labels.dedup();
            labels.join(" and ")
        };
        return format!(
            "{} and {} disagree since {}",
            label(&main_side),
            label(&other_side),
            block(at)
        );
    }
    if forked
        .iter()
        .any(|node| side_of(node) == Some(Side::NoMajority))
    {
        return format!(
            "Your {net} nodes disagree since {}, and Zeal can't tell which is right",
            block(at)
        );
    }
    let agreeing = witnesses.iter().filter(|node| agrees(node)).count();
    let mut clauses: Vec<String> = forked.iter().map(|node| node.sentence.clone()).collect();
    if agreeing > 0 {
        let verb = if agreeing == 1 { "is on" } else { "agree on" };
        clauses.push(format!(
            "{} {verb} {top}",
            nodes_phrase(agreeing, herd.network, true)
        ));
    }
    clauses.join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_and_durations_read_plainly() {
        assert_eq!(thousands(3_551_204), "3,551,204");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(rough(Duration::from_secs(45)), "45 s");
        assert_eq!(rough(Duration::from_secs(6 * 60)), "6 min");
        assert_eq!(rough(Duration::from_secs(3 * 3600)), "3 h");
    }

    #[test]
    fn every_state_has_a_word_and_a_sentence_with_the_name() {
        let cases = [
            (Relation::InAgreement { behind: 0 }, "agrees"),
            (Relation::InAgreement { behind: 1 }, "1 block behind"),
            (Relation::Lagging { blocks: 15 }, "15 blocks behind"),
            (Relation::Racing { at: 3_561_205 }, "agrees"),
            (
                Relation::Stalled {
                    at: 3_561_500,
                    replaced_block: false,
                },
                "stuck at block 3,561,500",
            ),
            (
                Relation::Forked {
                    at: 3_561_498,
                    own: 4,
                    main: 3,
                    side: Side::Minority,
                    deep: false,
                },
                "on a different chain since block 3,561,498",
            ),
            (
                Relation::CatchingUp {
                    progress: 0.3456,
                    same_chain: Some(true),
                },
                "catching up 34.5 % · same chain",
            ),
            (Relation::Unknown, "not answering"),
            (
                Relation::Excluded {
                    why: ExcludedWhy::Stopped,
                },
                "stopped",
            ),
        ];
        for (relation, word) in cases {
            let (got, sentence) = describe("Zebra · Mainnet 2", &relation);
            assert_eq!(got, word, "{relation:?}");
            assert!(sentence.starts_with("Zebra · Mainnet 2 "), "{sentence}");
        }
        let (_, sentence) = describe(
            "Zebra · Mainnet",
            &Relation::Excluded {
                why: ExcludedWhy::EndOfSupport,
            },
        );
        assert_eq!(sentence, "Zebra · Mainnet halted at its end of support");
    }
}
