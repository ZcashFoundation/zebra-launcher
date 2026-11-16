//! The herd as the window shows it (docs/HERD.md, UI): the overview's sentence for the
//! network shown, each node's state on its card, the glyph on the network's tab and the words
//! a sidebar item's title gains. Pure Rust, tested natively. The views come from the
//! backend's `herd` event, or from the mock's engine; a network without a herd (one node, or
//! none with a vote) shows nothing, so one node looks as it always did.

use launcher_types::{
    Level, NodeId,
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
    /// The same mark in the problem tone: a node on another chain, or the herd stalled long.
    Problem,
}

impl Glyph {
    pub fn symbol(self) -> &'static str {
        match self {
            Glyph::Check => "✓",
            Glyph::Warning | Glyph::Problem => "!",
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
/// for attention, none without a herd or while the herd is checking or catching up.
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

/// What a network's tab adds to its name and tooltip: the herd's sentence.
pub fn tab_part(view: Option<&HerdView>) -> Option<String> {
    sentence(view).map(|(text, _)| text)
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
        assert_eq!(Glyph::Problem.symbol(), Glyph::Warning.symbol());
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
