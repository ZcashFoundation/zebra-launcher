//! What a node is to the herd, from its kind and program: its vote and its family
//! (docs/HERD.md, Roles).

use launcher_types::{
    NodeKind, NodeProgram,
    herd::{Family, Vote},
};

/// Whether a node of `kind` counts when Zeal says "k of n agree": zebrad (on any channel) and
/// Zakura have their own peers and validation; the zcashd compatibility build validates but
/// hears only its node; Zebend and a generic node are not relied on alone; services hold no
/// chain.
pub fn vote(kind: NodeKind) -> Vote {
    match kind {
        NodeKind::Zebrad | NodeKind::Zakura => Vote::Witness,
        NodeKind::Zcashd => Vote::Validator,
        NodeKind::Zebend | NodeKind::Generic => Vote::Observer,
        NodeKind::Zaino | NodeKind::Zallet => Vote::None,
    }
}

/// The consensus code a node of `kind` running `program` has: for zebrad, its channel.
pub fn family(kind: NodeKind, program: NodeProgram) -> Family {
    match kind {
        NodeKind::Zebrad => Family::Zebra {
            channel: program.channel(),
        },
        NodeKind::Zakura => Family::Zakura,
        NodeKind::Zcashd => Family::Zcashd,
        NodeKind::Zebend => Family::Zebend,
        NodeKind::Zaino | NodeKind::Zallet | NodeKind::Generic => Family::Other,
    }
}

#[cfg(test)]
mod tests {
    use launcher_types::ZebradChannel;

    use super::*;

    #[test]
    fn zebrad_on_every_channel_and_zakura_are_witnesses() {
        for program in [
            NodeProgram::Bundled,
            NodeProgram::BundledBeta,
            NodeProgram::BundledNightly,
        ] {
            assert_eq!(vote(NodeKind::Zebrad), Vote::Witness);
            assert_eq!(
                family(NodeKind::Zebrad, program),
                Family::Zebra {
                    channel: program.channel()
                }
            );
        }
        assert_ne!(
            family(NodeKind::Zebrad, NodeProgram::Bundled),
            family(NodeKind::Zebrad, NodeProgram::BundledBeta),
            "channels are families of their own"
        );
        assert_eq!(vote(NodeKind::Zakura), Vote::Witness);
        assert_eq!(vote(NodeKind::Zcashd), Vote::Validator);
        assert_eq!(vote(NodeKind::Zebend), Vote::Observer);
        assert_eq!(vote(NodeKind::Zaino), Vote::None);
        assert_eq!(
            family(NodeKind::Zakura, NodeProgram::Bundled),
            Family::Zakura
        );
        let _ = ZebradChannel::Stable;
    }
}
