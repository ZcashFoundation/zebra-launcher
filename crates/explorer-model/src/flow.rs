//! V6, a transaction's flow: where its value comes from and where it goes, as a bowtie.
//! Strands from the left meet in the middle and part to the right, so no input is paired with
//! an output: the chain does not say which pays which. A shielded pool is one node, its
//! per-note amounts hidden; value that stays in it gets no strand, only what it lost (left) or
//! gained (right). Built from the Transaction page alone and laid out here, so both are
//! tested natively; the UI draws the layout as SVG.

use std::collections::HashMap;

use launcher_types::explorer::{Pool, Shown, TxPage};

/// The most nodes a side shows; the smallest of the rest are merged into one.
pub const MAX_STRANDS: usize = 12;

/// What a node of the flow stands for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FlowKey {
    /// `count` transparent inputs spending from one address (`None`: a script without one).
    Inputs { address: Option<String>, count: u32 },
    /// The inputs of the groups past the largest ones.
    MoreInputs { count: u32 },
    /// Inputs whose spent outputs were not read.
    UnknownInputs { count: u32 },
    /// Transparent output `n`.
    Output { n: u32 },
    /// The outputs past the first ones.
    MoreOutputs { count: u32 },
    /// A shielded pool: on the left what it lost, on the right what it gained. With nothing
    /// net it sits on the left, without a strand.
    Pool { pool: Pool },
    /// The transaction's fee.
    Fee,
    /// A coinbase's block subsidy.
    Subsidy,
    /// The fees of a coinbase's block.
    BlockFees,
    /// What the lockbox paid out through the coinbase (NU6.1).
    FromLockbox,
    /// The lockbox's share of the subsidy, which no output pays.
    IntoLockbox,
    /// The burned share of the block's fees, from NU7.
    Burned,
}

/// A node, with its value when public and known.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlowNode {
    pub key: FlowKey,
    pub zat: Option<u64>,
}

impl FlowNode {
    fn new(key: FlowKey, zat: Option<u64>) -> Self {
        Self { key, zat }
    }
}

/// Whether the two sides agree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Balance {
    /// What goes in comes out.
    Balanced { total: u64 },
    /// Some value is not known.
    Unknown,
    /// Both sides are known and differ: before NU6 a miner may leave value unclaimed;
    /// otherwise the numbers are not vouched for.
    Off { left: u64, right: u64 },
}

/// A transaction's flow: the nodes on each side, top to bottom, and whether they balance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Flow {
    pub left: Vec<FlowNode>,
    pub right: Vec<FlowNode>,
    pub balance: Balance,
}

/// The flow of the transaction `page` shows.
pub fn tx_flow(page: &TxPage) -> Flow {
    let mut left = Vec::new();
    let mut right = Vec::new();
    // The lockbox's share, which the coinbase's right side ends with.
    let mut into_lockbox = None;
    match page.coinbase {
        Some(claim) => {
            left.push(FlowNode::new(FlowKey::Subsidy, claim.subsidy));
            left.push(FlowNode::new(FlowKey::BlockFees, claim.fees));
            match claim.lockbox {
                Some(lockbox) if lockbox < 0 => left.push(FlowNode::new(
                    FlowKey::FromLockbox,
                    Some(lockbox.unsigned_abs()),
                )),
                Some(lockbox) if lockbox > 0 => {
                    into_lockbox = Some(FlowNode::new(
                        FlowKey::IntoLockbox,
                        Some(lockbox.unsigned_abs()),
                    ));
                }
                Some(_) => {}
                // Without the pools' rows the lockbox's share is unknown, where there is one.
                None => into_lockbox = Some(FlowNode::new(FlowKey::IntoLockbox, None)),
            }
        }
        None => left.extend(input_nodes(page)),
    }
    for part in &page.shielded {
        let key = FlowKey::Pool { pool: part.pool };
        if part.net_out >= 0 {
            left.push(FlowNode::new(key, Some(part.net_out.unsigned_abs())));
        }
    }
    let outputs = page.outputs.len();
    let shown = if outputs > MAX_STRANDS {
        MAX_STRANDS - 1
    } else {
        outputs
    };
    for output in &page.outputs[..shown] {
        right.push(FlowNode::new(
            FlowKey::Output { n: output.n },
            Some(output.zat),
        ));
    }
    if shown < outputs {
        let rest = &page.outputs[shown..];
        right.push(FlowNode::new(
            FlowKey::MoreOutputs {
                count: rest.len() as u32,
            },
            Some(rest.iter().map(|output| output.zat).sum()),
        ));
    }
    for part in &page.shielded {
        if part.net_out < 0 {
            right.push(FlowNode::new(
                FlowKey::Pool { pool: part.pool },
                Some(part.net_out.unsigned_abs()),
            ));
        }
    }
    right.extend(into_lockbox);
    match (page.coinbase, page.fee) {
        (Some(claim), _) => {
            if let Some(burned) = claim.burned.filter(|burned| *burned > 0) {
                right.push(FlowNode::new(FlowKey::Burned, Some(burned)));
            }
        }
        (None, Some(Shown::Public { zat })) => {
            right.push(FlowNode::new(FlowKey::Fee, u64::try_from(zat).ok()));
        }
        (None, Some(Shown::Unknown { .. } | Shown::Hidden)) => {
            right.push(FlowNode::new(FlowKey::Fee, None));
        }
        (None, None) => {}
    }
    let balance = balance(&left, &right);
    Flow {
        left,
        right,
        balance,
    }
}

/// The inputs as nodes: grouped by the address they spend from, largest first, the smallest
/// groups merged past [`MAX_STRANDS`]; those whose values are unknown last.
fn input_nodes(page: &TxPage) -> Vec<FlowNode> {
    let mut groups: Vec<(Option<String>, u32, u64)> = Vec::new();
    let mut at: HashMap<Option<String>, usize> = HashMap::new();
    let mut unknown = 0u32;
    for input in &page.inputs {
        let Some(spent) = &input.spent else {
            unknown += 1;
            continue;
        };
        let index = *at.entry(spent.address.clone()).or_insert_with(|| {
            groups.push((spent.address.clone(), 0, 0));
            groups.len() - 1
        });
        groups[index].1 += 1;
        groups[index].2 += spent.zat;
    }
    // Largest first; among equals, in the order the inputs come.
    groups.sort_by_key(|group| std::cmp::Reverse(group.2));
    let room = MAX_STRANDS - usize::from(unknown > 0);
    let mut nodes = Vec::new();
    if groups.len() > room {
        let rest = groups.split_off(room - 1);
        nodes.extend(groups.into_iter().map(|(address, count, zat)| {
            FlowNode::new(FlowKey::Inputs { address, count }, Some(zat))
        }));
        nodes.push(FlowNode::new(
            FlowKey::MoreInputs {
                count: rest.iter().map(|group| group.1).sum(),
            },
            Some(rest.iter().map(|group| group.2).sum()),
        ));
    } else {
        nodes.extend(groups.into_iter().map(|(address, count, zat)| {
            FlowNode::new(FlowKey::Inputs { address, count }, Some(zat))
        }));
    }
    if unknown > 0 {
        nodes.push(FlowNode::new(
            FlowKey::UnknownInputs { count: unknown },
            None,
        ));
    }
    nodes
}

fn balance(left: &[FlowNode], right: &[FlowNode]) -> Balance {
    let sum = |nodes: &[FlowNode]| -> Option<u64> {
        nodes
            .iter()
            .try_fold(0u64, |total, node| Some(total + node.zat?))
    };
    match (sum(left), sum(right)) {
        (Some(left), Some(right)) if left == right => Balance::Balanced { total: left },
        (Some(left), Some(right)) => Balance::Off { left, right },
        _ => Balance::Unknown,
    }
}

/// Sizes for [`layout`], in pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sizes {
    /// The strands of the heavier side together.
    pub spread: f64,
    /// The thinnest strand a value that is not zero gets.
    pub strand_min: f64,
    /// The shortest node, so its label fits.
    pub node_min: f64,
    /// Between nodes on a side.
    pub gap: f64,
}

impl Default for Sizes {
    fn default() -> Self {
        Sizes {
            spread: 160.0,
            strand_min: 2.0,
            node_min: 22.0,
            gap: 8.0,
        }
    }
}

/// Where one node and its strand go.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Band {
    /// The node's box.
    pub top: f64,
    pub height: f64,
    /// Its strand's width: linear in zatoshis, at least [`Sizes::strand_min`]; 0 without a
    /// known value.
    pub strand: f64,
    /// Where the strand leaves the node, and where it meets the middle.
    pub strand_top: f64,
    pub middle_top: f64,
}

/// The flow laid out top to bottom: each side's bands in node order, and the middle where the
/// strands meet.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub height: f64,
    pub left: Vec<Band>,
    pub right: Vec<Band>,
    pub middle_top: f64,
    pub middle_height: f64,
}

/// Lays `flow` out at `sizes`. Strands are linear in zatoshis against the heavier side, so a
/// balanced flow's two sides meet the middle at the same width.
pub fn layout(flow: &Flow, sizes: Sizes) -> Layout {
    let total = |nodes: &[FlowNode]| nodes.iter().filter_map(|node| node.zat).sum::<u64>();
    let scale = total(&flow.left).max(total(&flow.right)).max(1) as f64;
    let strand = |node: &FlowNode| match node.zat {
        Some(zat) if zat > 0 => (zat as f64 / scale * sizes.spread).max(sizes.strand_min),
        _ => 0.0,
    };
    let side_height = |nodes: &[FlowNode]| {
        let boxes: f64 = nodes
            .iter()
            .map(|node| strand(node).max(sizes.node_min))
            .sum();
        boxes + sizes.gap * nodes.len().saturating_sub(1) as f64
    };
    let strands = |nodes: &[FlowNode]| nodes.iter().map(strand).sum::<f64>();
    let height = side_height(&flow.left)
        .max(side_height(&flow.right))
        .max(sizes.node_min);
    let middle_height = strands(&flow.left).max(strands(&flow.right));
    let middle_top = (height - middle_height) / 2.0;
    let bands = |nodes: &[FlowNode]| {
        let mut top = (height - side_height(nodes)) / 2.0;
        let mut middle = middle_top + (middle_height - strands(nodes)) / 2.0;
        nodes
            .iter()
            .map(|node| {
                let strand = strand(node);
                let box_height = strand.max(sizes.node_min);
                let band = Band {
                    top,
                    height: box_height,
                    strand,
                    strand_top: top + (box_height - strand) / 2.0,
                    middle_top: middle,
                };
                top += box_height + sizes.gap;
                middle += strand;
                band
            })
            .collect()
    };
    Layout {
        height,
        left: bands(&flow.left),
        right: bands(&flow.right),
        middle_top,
        middle_height,
    }
}

#[cfg(test)]
mod tests {
    use launcher_types::explorer::{
        CoinbaseClaim, Hash32, LogicalActions, OutPoint, PoolPart, SpentOutput, TxInput, TxKind,
        TxOutput, TxStatus, UnknownWhy,
    };

    use super::*;

    fn blank() -> TxPage {
        TxPage {
            txid: Hash32([1; 32]),
            status: TxStatus::Mempool,
            kind: TxKind::Transparent,
            version: 5,
            size: None,
            lock_time: 0,
            expiry_height: None,
            inputs: Vec::new(),
            outputs: Vec::new(),
            shielded: Vec::new(),
            fee: None,
            actions: LogicalActions::default(),
            unresolved: 0,
            resolvable: false,
            burns_fees: false,
            coinbase: None,
        }
    }

    fn input(address: &str, zat: u64) -> TxInput {
        TxInput {
            prevout: Some(OutPoint {
                txid: Hash32([2; 32]),
                vout: 0,
            }),
            spent: Some(SpentOutput {
                zat,
                address: Some(address.into()),
                script: "pubkeyhash".into(),
                height: Some(1),
                coinbase: false,
            }),
        }
    }

    fn output(n: u32, zat: u64) -> TxOutput {
        TxOutput {
            n,
            zat,
            address: Some(format!("t1out{n}")),
            script: "pubkeyhash".into(),
        }
    }

    fn part(pool: Pool, net_out: i64) -> PoolPart {
        PoolPart {
            pool,
            spends: 0,
            outputs: 0,
            actions: 2,
            joinsplits: 0,
            net_out,
        }
    }

    #[test]
    fn shielding_with_change_balances() {
        // Two inputs from one address and one from another shield 1.5 into Orchard, with 0.2
        // of change and a fee of 0.0001.
        let page = TxPage {
            inputs: vec![
                input("t1a", 100_000_000),
                input("t1b", 50_010_000),
                input("t1a", 20_000_000),
            ],
            outputs: vec![output(0, 20_000_000)],
            shielded: vec![part(Pool::Orchard, -150_000_000)],
            fee: Some(Shown::Public { zat: 10_000 }),
            ..blank()
        };
        let flow = tx_flow(&page);
        assert_eq!(
            flow.left,
            vec![
                FlowNode::new(
                    FlowKey::Inputs {
                        address: Some("t1a".into()),
                        count: 2
                    },
                    Some(120_000_000)
                ),
                FlowNode::new(
                    FlowKey::Inputs {
                        address: Some("t1b".into()),
                        count: 1
                    },
                    Some(50_010_000)
                ),
            ]
        );
        assert_eq!(
            flow.right
                .iter()
                .map(|node| node.key.clone())
                .collect::<Vec<_>>(),
            vec![
                FlowKey::Output { n: 0 },
                FlowKey::Pool {
                    pool: Pool::Orchard
                },
                FlowKey::Fee
            ]
        );
        assert_eq!(flow.balance, Balance::Balanced { total: 170_010_000 });
    }

    #[test]
    fn value_that_stays_in_a_pool_has_no_strand() {
        // Fully shielded: Orchard loses only the fee.
        let page = TxPage {
            shielded: vec![part(Pool::Orchard, 10_000)],
            fee: Some(Shown::Public { zat: 10_000 }),
            ..blank()
        };
        let flow = tx_flow(&page);
        assert_eq!(
            flow.left,
            vec![FlowNode::new(
                FlowKey::Pool {
                    pool: Pool::Orchard
                },
                Some(10_000)
            )]
        );
        assert_eq!(flow.right, vec![FlowNode::new(FlowKey::Fee, Some(10_000))]);
        // Orchard to Ironwood: Orchard loses the amount and the fee; Ironwood gains the amount.
        let migration = TxPage {
            shielded: vec![
                part(Pool::Orchard, 100_020_000),
                part(Pool::Ironwood, -100_000_000),
            ],
            fee: Some(Shown::Public { zat: 20_000 }),
            ..blank()
        };
        let flow = tx_flow(&migration);
        assert_eq!(flow.left.len(), 1);
        assert_eq!(
            flow.right[0],
            FlowNode::new(
                FlowKey::Pool {
                    pool: Pool::Ironwood
                },
                Some(100_000_000)
            )
        );
        assert_eq!(flow.balance, Balance::Balanced { total: 100_020_000 });
        // A pool with nothing net stays on the left, with no strand in the layout.
        let even = TxPage {
            shielded: vec![part(Pool::Sapling, 0)],
            inputs: vec![input("t1a", 5)],
            outputs: vec![output(0, 5)],
            fee: Some(Shown::Public { zat: 0 }),
            ..blank()
        };
        let layout = layout(&tx_flow(&even), Sizes::default());
        assert_eq!(layout.left[1].strand, 0.0);
    }

    #[test]
    fn unknown_inputs_leave_the_balance_unknown() {
        let mut missing = input("t1a", 0);
        missing.spent = None;
        let page = TxPage {
            inputs: vec![input("t1a", 7), missing.clone(), missing],
            outputs: vec![output(0, 5)],
            fee: Some(Shown::Unknown {
                why: UnknownWhy::InputValues,
            }),
            ..blank()
        };
        let flow = tx_flow(&page);
        assert_eq!(
            flow.left.last(),
            Some(&FlowNode::new(FlowKey::UnknownInputs { count: 2 }, None))
        );
        assert_eq!(flow.right.last(), Some(&FlowNode::new(FlowKey::Fee, None)));
        assert_eq!(flow.balance, Balance::Unknown);
    }

    #[test]
    fn many_inputs_and_outputs_merge_past_twelve() {
        let page = TxPage {
            inputs: (0..30)
                .map(|index| input(&format!("t1a{index}"), 100 + index))
                .collect(),
            outputs: (0..20).map(|n| output(n, 10)).collect(),
            ..blank()
        };
        let flow = tx_flow(&page);
        assert_eq!(flow.left.len(), MAX_STRANDS);
        // The largest groups first; the 19 smallest merged.
        assert_eq!(
            flow.left[0].key,
            FlowKey::Inputs {
                address: Some("t1a29".into()),
                count: 1
            }
        );
        assert_eq!(
            flow.left[11],
            FlowNode::new(
                FlowKey::MoreInputs { count: 19 },
                Some((100..119).sum::<u64>())
            )
        );
        assert_eq!(flow.right.len(), MAX_STRANDS);
        assert_eq!(
            flow.right[11],
            FlowNode::new(FlowKey::MoreOutputs { count: 9 }, Some(90))
        );
    }

    #[test]
    fn a_coinbase_claims_the_subsidy_and_the_fees() {
        // An NU7 block's coinbase: a subsidy of 300 and fees of 100 go in; 100 to a
        // transparent output, 240 into Ironwood and 60 burned come out.
        let page = TxPage {
            kind: TxKind::Coinbase {
                shielded_to: vec![Pool::Ironwood],
            },
            outputs: vec![output(0, 100)],
            shielded: vec![part(Pool::Ironwood, -240)],
            coinbase: Some(CoinbaseClaim {
                subsidy: Some(300),
                fees: Some(100),
                burned: Some(60),
                lockbox: Some(0),
            }),
            ..blank()
        };
        let flow = tx_flow(&page);
        assert_eq!(
            flow.left
                .iter()
                .map(|node| node.key.clone())
                .collect::<Vec<_>>(),
            vec![FlowKey::Subsidy, FlowKey::BlockFees]
        );
        assert_eq!(
            flow.right
                .iter()
                .map(|node| node.key.clone())
                .collect::<Vec<_>>(),
            vec![
                FlowKey::Output { n: 0 },
                FlowKey::Pool {
                    pool: Pool::Ironwood
                },
                FlowKey::Burned
            ]
        );
        assert_eq!(flow.balance, Balance::Balanced { total: 400 });
        // From NU6 the lockbox's share comes after the outputs and the pools.
        let deferring = TxPage {
            coinbase: Some(CoinbaseClaim {
                lockbox: Some(40),
                subsidy: Some(340),
                ..page.coinbase.unwrap()
            }),
            ..page.clone()
        };
        let flow = tx_flow(&deferring);
        assert_eq!(flow.right[2], FlowNode::new(FlowKey::IntoLockbox, Some(40)));
        assert_eq!(flow.balance, Balance::Balanced { total: 440 });
        // NU6.1's disbursement: the lockbox pays out through the coinbase.
        let disbursing = TxPage {
            coinbase: Some(CoinbaseClaim {
                subsidy: Some(300),
                fees: Some(0),
                burned: None,
                lockbox: Some(-60),
            }),
            outputs: vec![output(0, 360)],
            shielded: Vec::new(),
            ..page.clone()
        };
        let flow = tx_flow(&disbursing);
        assert_eq!(flow.left[2], FlowNode::new(FlowKey::FromLockbox, Some(60)));
        assert_eq!(flow.balance, Balance::Balanced { total: 360 });
        // Without the block's rows, the claim is unknown.
        let unread = TxPage {
            coinbase: Some(CoinbaseClaim {
                subsidy: Some(300),
                fees: None,
                burned: None,
                lockbox: None,
            }),
            ..page
        };
        assert_eq!(tx_flow(&unread).balance, Balance::Unknown);
    }

    #[test]
    fn the_layout_is_linear_and_both_sides_meet_the_middle_alike() {
        let flow = Flow {
            left: vec![
                FlowNode::new(FlowKey::Subsidy, Some(900)),
                FlowNode::new(FlowKey::BlockFees, Some(100)),
            ],
            right: vec![
                FlowNode::new(FlowKey::Output { n: 0 }, Some(1)),
                FlowNode::new(FlowKey::Output { n: 1 }, Some(999)),
            ],
            balance: Balance::Balanced { total: 1_000 },
        };
        let sizes = Sizes::default();
        let layout = layout(&flow, sizes);
        assert_eq!(layout.left[0].strand, 144.0);
        assert_eq!(layout.left[1].strand, 16.0);
        assert_eq!(layout.right[0].strand, 2.0, "the thinnest strand");
        assert!((layout.right[1].strand - 159.84).abs() < 1e-9);
        // Nodes stack with gaps; a thin strand's node is still tall enough for its label.
        assert_eq!(layout.left[1].top, layout.left[0].top + 144.0 + sizes.gap);
        assert_eq!(layout.right[0].height, sizes.node_min);
        let near = |a: f64, b: f64| (a - b).abs() < 1e-9;
        assert!(near(layout.middle_height, 161.84));
        // Strands stack in the middle in node order, each side centred on it.
        assert!(near(layout.left[0].middle_top, layout.middle_top + 0.92));
        assert!(near(
            layout.left[1].middle_top,
            layout.left[0].middle_top + 144.0
        ));
        assert!(near(layout.right[0].middle_top, layout.middle_top));
        // Every box fits the height.
        for band in layout.left.iter().chain(&layout.right) {
            assert!(band.top >= 0.0 && band.top + band.height <= layout.height + 1e-9);
        }
    }
}
