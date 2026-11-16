//! The herd as the window shows it (docs/HERD.md, UI): the overview's sentence for the
//! network shown, each node's state on its card, the glyph on the network's tab and the words
//! a sidebar item's title gains. Pure Rust, tested natively. The views come from the
//! backend's `herd` event, or from the mock's engine; a network without a herd (one node, or
//! none with a vote) shows nothing, so one node looks as it always did.

use launcher_types::{
    Level, Network, NodeId,
    herd::{HerdState, HerdView, Vote},
};

/// How the herd's words read, as a colour beside the word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HerdTone {
    Ok,
    Neutral,
    Warning,
    Problem,
}

impl HerdTone {
    pub fn of(level: Level) -> Self {
        match level {
            Level::Healthy => HerdTone::Ok,
            Level::Neutral => HerdTone::Neutral,
            Level::Attention | Level::Urgent => HerdTone::Warning,
            Level::Problem => HerdTone::Problem,
        }
    }

    /// The CSS class.
    pub fn class(self) -> &'static str {
        match self {
            HerdTone::Ok => "herd-tone-ok",
            HerdTone::Neutral => "herd-tone-neutral",
            HerdTone::Warning => "herd-tone-warning",
            HerdTone::Problem => "herd-tone-problem",
        }
    }
}

/// The mark on a network's tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glyph {
    /// The nodes agree.
    Check,
    /// Something wants a look: a node behind or stuck.
    Warning,
    /// A problem: a node on another chain, or the herd stalled long. A cross, as the status
    /// card's problem icon, so the mark does not rely on its colour.
    Problem,
}

impl Glyph {
    pub fn symbol(self) -> &'static str {
        match self {
            Glyph::Check => "✓",
            Glyph::Warning => "!",
            Glyph::Problem => "✗",
        }
    }

    pub fn class(self) -> &'static str {
        match self {
            Glyph::Check => "herd-glyph herd-tone-ok",
            Glyph::Warning => "herd-glyph herd-tone-warning",
            Glyph::Problem => "herd-glyph herd-tone-problem",
        }
    }
}

/// The view's network has a herd to show.
fn shown(view: Option<&HerdView>) -> Option<&HerdView> {
    view.filter(|view| view.state != HerdState::Off)
}

/// The glyph on a network's tab: a check while its nodes agree, a warning once its level asks
/// for attention, a cross for a problem, none without a herd or while the herd is checking or
/// catching up.
pub fn glyph(view: Option<&HerdView>) -> Option<Glyph> {
    let view = shown(view)?;
    if view.level >= Level::Problem {
        Some(Glyph::Problem)
    } else if view.level >= Level::Attention {
        Some(Glyph::Warning)
    } else if view.state == HerdState::Agreed {
        Some(Glyph::Check)
    } else {
        None
    }
}

/// The overview's sentence for the network: its headline with a tone, `None` without a herd.
pub fn sentence(view: Option<&HerdView>) -> Option<(String, HerdTone)> {
    let view = shown(view)?;
    Some((view.headline.clone(), HerdTone::of(view.level)))
}

/// A node's state in its herd, for its card: its word, its sentence and a tone; `None` when
/// its network has no herd or the node does not count in it (a service, or the zcashd
/// compatibility build, which follows another node).
pub fn node_state(view: Option<&HerdView>, node: &NodeId) -> Option<(String, String, HerdTone)> {
    let node = shown(view)?
        .node(node)
        .filter(|node| node.vote == Vote::Witness)?;
    Some((
        node.word.clone(),
        node.sentence.clone(),
        HerdTone::of(node.level),
    ))
}

/// What a node's sidebar item adds to its title: "in the herd: 6 blocks behind".
pub fn title_part(view: Option<&HerdView>, node: &NodeId) -> Option<String> {
    let (word, _, _) = node_state(view, node)?;
    Some(format!("in the herd: {word}"))
}

/// What a network's tab adds to its tooltip: the herd's sentence.
pub fn tab_part(view: Option<&HerdView>) -> Option<String> {
    sentence(view).map(|(text, _)| text)
}

/// What a network's tab adds to its accessible name: the herd's state in a few words, after
/// its level when that asks for attention, since the glyph is not read. No heights or times,
/// so the name of the focused tab does not change with every block; the tooltip has the
/// sentence ([`tab_part`]).
pub fn tab_state(view: Option<&HerdView>) -> Option<String> {
    let view = shown(view)?;
    let words = match view.state {
        HerdState::Off => return None,
        HerdState::Checking => "not checked yet",
        HerdState::Single => "one node checks the chain",
        HerdState::Agreed => "the nodes agree",
        HerdState::Degraded => "not every node agrees",
        HerdState::Split => "the nodes disagree",
        HerdState::Stalled => "no new block",
    };
    Some(match view.level.word() {
        Some(level) if view.level >= Level::Attention => format!("{level}: {words}"),
        _ => words.to_owned(),
    })
}

/// What the overview's live region says: the herd's sentence as it read when the network, the
/// herd's state or its level last changed. The sentence names the top block and how long the
/// herd has been quiet, which change with every block or minute; a screen reader hears it
/// again only when one of those three changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Announcement {
    network: Network,
    state: HerdState,
    level: Level,
    pub text: String,
}

/// The announcement for `view`, after `previous`: `previous` itself while the network, the
/// state and the level are the same, else the sentence now. `None` without a herd.
pub fn announcement(
    previous: Option<&Announcement>,
    view: Option<&HerdView>,
) -> Option<Announcement> {
    let view = shown(view)?;
    match previous {
        Some(previous)
            if (previous.network, previous.state, previous.level)
                == (view.network, view.state, view.level) =>
        {
            Some(previous.clone())
        }
        _ => Some(Announcement {
            network: view.network,
            state: view.state,
            level: view.level,
            text: view.headline.clone(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use launcher_types::{
        Network,
        herd::{Family, NodeInHerd, Relation, Vote},
    };

    use super::*;

    fn view(state: HerdState, level: Level) -> HerdView {
        HerdView {
            state,
            level,
            headline: "Your 3 Mainnet nodes agree on block 3,551,204".into(),
            nodes: vec![NodeInHerd {
                id: NodeId::new("zebrad-mainnet-2").unwrap(),
                name: "Zebra · Mainnet 2".into(),
                vote: Vote::Witness,
                family: Family::Zakura,
                tip: None,
                relation: Relation::Lagging { blocks: 6 },
                level: Level::Attention,
                word: "6 blocks behind".into(),
                sentence: "Zebra · Mainnet 2 is 6 blocks behind".into(),
            }],
            ..HerdView::off(Network::Mainnet, SystemTime::UNIX_EPOCH)
        }
    }

    #[test]
    fn a_network_without_a_herd_shows_nothing() {
        let off = HerdView::off(Network::Mainnet, SystemTime::UNIX_EPOCH);
        let id = NodeId::new("zebrad-mainnet").unwrap();
        for view in [None, Some(&off)] {
            assert_eq!(glyph(view), None);
            assert_eq!(sentence(view), None);
            assert_eq!(node_state(view, &id), None);
            assert_eq!(title_part(view, &id), None);
            assert_eq!(tab_part(view), None);
            assert_eq!(tab_state(view), None);
            assert_eq!(announcement(None, view), None);
        }
    }

    #[test]
    fn the_tab_shows_a_check_while_the_nodes_agree_and_a_warning_when_one_wants_a_look() {
        assert_eq!(
            glyph(Some(&view(HerdState::Agreed, Level::Healthy))),
            Some(Glyph::Check)
        );
        assert_eq!(
            glyph(Some(&view(HerdState::Degraded, Level::Attention))),
            Some(Glyph::Warning)
        );
        assert_eq!(
            glyph(Some(&view(HerdState::Split, Level::Problem))),
            Some(Glyph::Problem)
        );
        // The mark, not only its colour, tells a problem from a warning.
        assert_ne!(Glyph::Problem.symbol(), Glyph::Warning.symbol());
        // Catching up, or one node at the tip: neither mark.
        assert_eq!(
            glyph(Some(&view(HerdState::Degraded, Level::Neutral))),
            None
        );
        assert_eq!(
            glyph(Some(&view(HerdState::Checking, Level::Neutral))),
            None
        );
        assert_eq!(glyph(Some(&view(HerdState::Single, Level::Neutral))), None);
    }

    #[test]
    fn the_tabs_name_says_the_state_without_a_height() {
        assert_eq!(
            tab_state(Some(&view(HerdState::Agreed, Level::Healthy))).as_deref(),
            Some("the nodes agree")
        );
        assert_eq!(
            tab_state(Some(&view(HerdState::Degraded, Level::Attention))).as_deref(),
            Some("Attention: not every node agrees")
        );
        assert_eq!(
            tab_state(Some(&view(HerdState::Split, Level::Problem))).as_deref(),
            Some("Problem: the nodes disagree")
        );
        assert_eq!(
            tab_state(Some(&view(HerdState::Checking, Level::Neutral))).as_deref(),
            Some("not checked yet")
        );
    }

    #[test]
    fn the_live_sentence_changes_only_with_the_state_or_the_level() {
        let first = view(HerdState::Agreed, Level::Healthy);
        let said = announcement(None, Some(&first)).expect("a herd");
        assert_eq!(said.text, first.headline);
        // A new block: the same state, a new sentence. Nothing new is said.
        let next = HerdView {
            headline: "Your 3 Mainnet nodes agree on block 3,551,205".into(),
            ..first.clone()
        };
        assert_eq!(announcement(Some(&said), Some(&next)), Some(said.clone()));
        // A new level: the sentence as it reads now.
        let worse = HerdView {
            headline: "2 of your 3 Mainnet nodes agree on block 3,551,205; Zebra · Mainnet 2 is 6 blocks behind".into(),
            ..view(HerdState::Degraded, Level::Attention)
        };
        let said = announcement(Some(&said), Some(&worse)).expect("a herd");
        assert_eq!(said.text, worse.headline);
        // The other network's herd is said too.
        let testnet = HerdView {
            network: Network::Testnet,
            headline: "Your 2 Testnet nodes agree on block 4,471,230".into(),
            ..worse.clone()
        };
        assert_eq!(
            announcement(Some(&said), Some(&testnet)).map(|said| said.text),
            Some(testnet.headline.clone())
        );
    }

    #[test]
    fn the_sentence_and_the_nodes_words_carry_their_tones() {
        let view = view(HerdState::Degraded, Level::Attention);
        assert_eq!(
            sentence(Some(&view)),
            Some((
                "Your 3 Mainnet nodes agree on block 3,551,204".to_owned(),
                HerdTone::Warning
            ))
        );
        let id = NodeId::new("zebrad-mainnet-2").unwrap();
        let (word, sentence, tone) = node_state(Some(&view), &id).expect("in the herd");
        assert_eq!(word, "6 blocks behind");
        assert!(sentence.starts_with("Zebra · Mainnet 2"));
        assert_eq!(tone, HerdTone::Warning);
        assert_eq!(
            title_part(Some(&view), &id).as_deref(),
            Some("in the herd: 6 blocks behind")
        );
        assert_eq!(
            node_state(Some(&view), &NodeId::new("zaino-mainnet").unwrap()),
            None
        );
        // A node that does not count shows no herd words.
        let mut validator = view.clone();
        validator.nodes[0].vote = Vote::Validator;
        assert_eq!(node_state(Some(&validator), &id), None);
        assert_eq!(HerdTone::of(Level::Problem).class(), "herd-tone-problem");
    }
}
