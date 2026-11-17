//! V6, a transaction's flow, drawn as SVG from explorer-model's `flow` layout: strands from
//! the inputs and the pools that lost value meet in the middle and part to the outputs, the
//! pools that gained and the fee. A pool node is an outlined box whose per-note amounts stay
//! hidden; a value that is not known has a dashed node and no strand. Hovering or focusing a
//! node or a table row lights both; an input from one earlier output opens that transaction.

use explorer_model::flow::{Flow, FlowKey, FlowNode, Layout, Sizes, layout};
use launcher_types::explorer::{ExplorerRoute, TxPage};
use leptos::prelude::*;

use super::{
    state::NodeExplorer,
    text::{self, Unit},
};

/// The drawing's width in its own units; it scales to the page.
const WIDTH: f64 = 840.0;
/// Where the left nodes, the middle and the right nodes stand.
const LEFT_X: f64 = 256.0;
const MIDDLE_X: f64 = 412.0;
const RIGHT_X: f64 = 564.0;
const NODE_WIDTH: f64 = 12.0;
const MIDDLE_WIDTH: f64 = 8.0;
/// Above and below the nodes.
const PAD: f64 = 6.0;

/// Which node is lit: a side and its place on that side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lit {
    Left(usize),
    Right(usize),
}

/// The flow's sizes: nodes tall enough for two lines of label.
pub fn sizes() -> Sizes {
    Sizes {
        spread: 180.0,
        strand_min: 2.0,
        node_min: 30.0,
        gap: 8.0,
    }
}

/// The left node an input stands in, by its place among the page's inputs.
pub fn input_node(flow: &Flow, page: &TxPage, input: usize) -> Option<usize> {
    let spent = page.inputs.get(input)?.spent.as_ref();
    flow.left
        .iter()
        .position(|node| match (&node.key, spent) {
            (FlowKey::Inputs { address, .. }, Some(spent)) => *address == spent.address,
            (FlowKey::UnknownInputs { .. }, None) => true,
            _ => false,
        })
        .or_else(|| {
            // Merged past the largest groups.
            spent.and_then(|_| {
                flow.left
                    .iter()
                    .position(|node| matches!(node.key, FlowKey::MoreInputs { .. }))
            })
        })
}

/// The right node output `n` stands in.
pub fn output_node(flow: &Flow, n: u32) -> Option<usize> {
    flow.right
        .iter()
        .position(|node| node.key == FlowKey::Output { n })
        .or_else(|| {
            flow.right
                .iter()
                .position(|node| matches!(node.key, FlowKey::MoreOutputs { .. }))
        })
}

/// The colour class of a node and its strand.
fn colour(key: &FlowKey) -> &'static str {
    match key {
        FlowKey::Inputs { .. }
        | FlowKey::MoreInputs { .. }
        | FlowKey::Output { .. }
        | FlowKey::MoreOutputs { .. } => "ex-c-transparent",
        FlowKey::Pool { pool } => match pool.id() {
            "sprout" => "ex-c-sprout",
            "sapling" => "ex-c-sapling",
            "orchard" => "ex-c-orchard",
            "ironwood" => "ex-c-ironwood",
            _ => "ex-c-transparent",
        },
        FlowKey::Fee | FlowKey::BlockFees => "ex-c-fees",
        FlowKey::Subsidy => "ex-c-subsidy",
        FlowKey::FromLockbox | FlowKey::IntoLockbox => "ex-c-lockbox",
        FlowKey::UnknownInputs { .. } | FlowKey::Burned => "ex-c-none",
    }
}

/// A node's amount, in words: a pool's is its net change, its notes' amounts hidden (in a
/// coinbase public by consensus, though not decrypted here), and never a zero for value that
/// stayed in it.
fn amount_text(node: &FlowNode, unit: Unit, coinbase: bool) -> String {
    let notes = if coinbase {
        "public by consensus"
    } else {
        "per note hidden"
    };
    match (&node.key, node.zat) {
        (FlowKey::Pool { .. }, Some(0)) => format!("nothing net · {notes}"),
        (FlowKey::Pool { .. }, Some(zat)) => {
            format!("{} net · {notes}", text::amount(zat as i64, unit))
        }
        (_, Some(zat)) => text::amount(zat as i64, unit),
        (_, None) => "value unknown".to_owned(),
    }
}

/// A strand from `(x0, y0)` to `(x1, y1)`, `width` tall at both ends.
fn strand_path(x0: f64, y0: f64, x1: f64, y1: f64, width: f64) -> String {
    let middle = (x0 + x1) / 2.0;
    format!(
        "M{x0:.1},{y0:.1} C{middle:.1},{y0:.1} {middle:.1},{y1:.1} {x1:.1},{y1:.1} \
         L{x1:.1},{y2:.1} C{middle:.1},{y2:.1} {middle:.1},{y3:.1} {x0:.1},{y3:.1} Z",
        y2 = y1 + width,
        y3 = y0 + width,
    )
}

#[component]
pub fn FlowDiagram(
    page: TxPage,
    flow: Flow,
    unit: Unit,
    nav: NodeExplorer,
    lit: RwSignal<Option<Lit>>,
) -> impl IntoView {
    let laid: Layout = layout(&flow, sizes());
    let height = laid.height + 2.0 * PAD;
    let summary = text::conservation_text(&flow, unit);
    let dim = move |me: Lit| lit.get().is_some_and(|lit| lit != me);
    let side = |left: bool| {
        let nodes = if left { &flow.left } else { &flow.right };
        let bands = if left { &laid.left } else { &laid.right };
        nodes
            .iter()
            .zip(bands)
            .enumerate()
            .map(|(index, (node, band))| {
                let me = if left {
                    Lit::Left(index)
                } else {
                    Lit::Right(index)
                };
                let class = colour(&node.key);
                let label = text::flow_label(&node.key, &page);
                let amount = amount_text(node, unit, page.coinbase.is_some());
                let pool = matches!(node.key, FlowKey::Pool { .. });
                let unknown = node.zat.is_none() || matches!(node.key, FlowKey::Burned);
                let node_x = if left { LEFT_X } else { RIGHT_X };
                let top = band.top + PAD;
                let strand = (band.strand > 0.0).then(|| {
                    let path = if left {
                        strand_path(
                            LEFT_X + NODE_WIDTH,
                            band.strand_top + PAD,
                            MIDDLE_X,
                            band.middle_top + PAD,
                            band.strand,
                        )
                    } else {
                        strand_path(
                            MIDDLE_X + MIDDLE_WIDTH,
                            band.middle_top + PAD,
                            RIGHT_X,
                            band.strand_top + PAD,
                            band.strand,
                        )
                    };
                    view! {
                        <path
                            class=format!("ex-strand {class}")
                            class:ex-lit=move || lit.get() == Some(me)
                            class:ex-dim=move || dim(me)
                            d=path
                        />
                    }
                });
                // An input from one earlier output opens that transaction at the output.
                let opens = match &node.key {
                    FlowKey::Inputs { count: 1, address } => page
                        .inputs
                        .iter()
                        .find(|input| {
                            input.spent.as_ref().map(|spent| &spent.address) == Some(address)
                        })
                        .and_then(|input| input.prevout)
                        .map(|prevout| ExplorerRoute::Tx {
                            txid: prevout.txid,
                            out: Some(prevout.vout),
                        }),
                    _ => None,
                };
                let clickable = opens.is_some();
                let opens = StoredValue::new(opens);
                let (text_x, anchor) = if left {
                    (LEFT_X - 8.0, "end")
                } else {
                    (RIGHT_X + NODE_WIDTH + 8.0, "start")
                };
                let middle = top + band.height / 2.0;
                view! {
                    {strand}
                    <g
                        class="ex-flow-node"
                        class:ex-lit=move || lit.get() == Some(me)
                        class:ex-dim=move || dim(me)
                        class:ex-flow-link=clickable
                        tabindex="0"
                        role=clickable.then_some("link")
                        aria-label=format!("{label}: {amount}")
                        on:mouseenter=move |_| lit.set(Some(me))
                        on:mouseleave=move |_| lit.set(None)
                        on:focus=move |_| lit.set(Some(me))
                        on:blur=move |_| lit.set(None)
                        on:click=move |_| {
                            if let Some(route) = opens.get_value() {
                                nav.go(route);
                            }
                        }
                        on:keydown=move |event: web_sys::KeyboardEvent| {
                            if event.key() == "Enter"
                                && let Some(route) = opens.get_value()
                            {
                                event.prevent_default();
                                nav.go(route);
                            }
                        }
                    >
                        <rect
                            class=format!("ex-flow-box {class}")
                            class:ex-flow-pool=pool
                            class:ex-flow-unknown=unknown
                            x=format!("{node_x:.1}")
                            y=format!("{top:.1}")
                            width=format!("{NODE_WIDTH:.1}")
                            height=format!("{:.1}", band.height)
                            rx="2"
                        />
                        <text class="ex-flow-label" x=format!("{text_x:.1}") y=format!("{:.1}", middle - 3.0) text-anchor=anchor>
                            {label.clone()}
                        </text>
                        <text class="ex-flow-amount" x=format!("{text_x:.1}") y=format!("{:.1}", middle + 11.0) text-anchor=anchor>
                            {amount.clone()}
                        </text>
                    </g>
                }
            })
            .collect_view()
    };
    let left = side(true);
    let right = side(false);
    view! {
        <div class="ex-flow-wrap">
            <svg
                class="ex-flow-svg"
                viewBox=format!("0 0 {WIDTH} {height:.1}")
                role="group"
                aria-label=format!("Where the value went. {summary}")
            >
                <rect
                    class="ex-flow-middle"
                    x=format!("{MIDDLE_X:.1}")
                    y=format!("{:.1}", laid.middle_top + PAD)
                    width=format!("{MIDDLE_WIDTH:.1}")
                    height=format!("{:.1}", laid.middle_height.max(2.0))
                />
                {left}
                {right}
            </svg>
        </div>
        <p class="ex-conservation">{summary}</p>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_strand_is_a_closed_band() {
        let path = strand_path(0.0, 10.0, 100.0, 50.0, 4.0);
        assert_eq!(
            path,
            "M0.0,10.0 C50.0,10.0 50.0,50.0 100.0,50.0 L100.0,54.0 C50.0,54.0 50.0,14.0 0.0,14.0 Z"
        );
    }
}
