//! The Transaction page, from the transaction's public parts: where it is and how settled
//! (V7), what it did in one sentence, where its value went (V6), a card per shielded pool
//! (counts and the net movement; per-note amounts are hidden), the transparent inputs with
//! the outputs they spend and the outputs, the fee as ZIP 317 counts it, and the details. A
//! transaction spending from many earlier ones has the rest read on request.

use explorer_model::flow::tx_flow;
use launcher_types::{
    Network, NodeId,
    explorer::{
        BlockId, ExplorerRequest, ExplorerResponse, ExplorerRoute, Pool, PoolPart, Shown, TxId,
        TxKind, TxPage as Page, TxStatus,
    },
};
use leptos::prelude::*;

use super::{
    flow_diagram::{FlowDiagram, Lit, input_node, output_node},
    parts::{Failure, HashText, Hidden, KindBadge, Loading, PoolChip, RouteLink, RpcCalls},
    prefs::Prefs,
    state::{Loaded, NodeExplorer, load},
    text::{self, FeeFigures, Unit},
};
use crate::format::{bytes, short_hash, thousands};

#[component]
pub fn TxPage(
    node: NodeId,
    nav: NodeExplorer,
    network: Network,
    txid: TxId,
    out: Option<u32>,
) -> impl IntoView {
    let prefs = Prefs::get();
    let tip = move || {
        nav.live.with(|live| {
            live.as_ref()
                .and_then(|live| live.tip)
                .map_or((0, 0), |tip| (tip.epoch, tip.height))
        })
    };
    // A waiting transaction is asked about again at every new tip, until it is mined; any
    // transaction after a reorganisation, which may have replaced its block. "Read them all"
    // asks again with every earlier transaction its inputs spend from.
    let waiting = RwSignal::new(false);
    let resolve = RwSignal::new(false);
    let answer = load(node, nav, move || {
        let (epoch, height) = tip();
        let height = if waiting.get() { height } else { 0 };
        Some((
            ExplorerRequest::Tx {
                txid,
                resolve: resolve.get(),
            },
            (epoch << 32) | u64::from(height),
        ))
    });
    Effect::new(move |_| {
        let is_waiting = matches!(
            answer.value.get(),
            Loaded::Ready(ref reply) if matches!(&reply.response, ExplorerResponse::Tx(page) if page.status == TxStatus::Mempool)
        );
        waiting.set(is_waiting);
    });
    let sources = Signal::derive(move || match answer.value.get() {
        Loaded::Ready(reply) => reply.sources,
        _ => None,
    });
    view! {
        <div class="ex-tx" class:ex-refreshing=move || answer.refreshing.get()>
            {move || match answer.value.get() {
                Loaded::Loading => view! { <Loading /> }.into_any(),
                Loaded::Failed(error) => view! { <Failure error /> }.into_any(),
                Loaded::Ready(reply) => match reply.response {
                    ExplorerResponse::Tx(page) => {
                        // A new unit draws the same answer again; nothing is read for it.
                        let unit = prefs.unit(network);
                        view! { <Tx page=*page nav unit network out resolve /> }.into_any()
                    }
                    _ => view! { <Loading /> }.into_any(),
                },
            }}
            <RpcCalls sources title="What this page asked your node" />
        </div>
    }
}

#[component]
fn Tx(
    page: Page,
    nav: NodeExplorer,
    unit: Unit,
    network: Network,
    out: Option<u32>,
    resolve: RwSignal<bool>,
) -> impl IntoView {
    let sentence = text::tx_sentence(&page, unit);
    let coinbase = matches!(page.kind, TxKind::Coinbase { .. });
    let status = page.status.clone();
    let flow = tx_flow(&page);
    let lit = RwSignal::new(None::<Lit>);
    // The output an input link led here for comes into view.
    if out.is_some() {
        request_animation_frame(|| {
            if let Some(row) = web_sys::window()
                .and_then(|window| window.document())
                .and_then(|document| document.query_selector(".ex-tx .ex-row-selected").ok())
                .flatten()
            {
                row.scroll_into_view_with_bool(false);
            }
        });
    }
    view! {
        <section class="ex-section" aria-labelledby="ex-tx-title">
            <div class="ex-title-row">
                <h2 id="ex-tx-title">"Transaction"</h2>
                <KindBadge kind=page.kind.clone() />
            </div>
            <HashText hash=page.txid full=true />
            <Status status nav />
            <p class="ex-sentence">{sentence}</p>
        </section>
        <section class="ex-section" aria-labelledby="ex-flow-title">
            <h2 id="ex-flow-title">"Where the value went"</h2>
            <FlowDiagram page=page.clone() flow=flow.clone() unit nav lit />
            <p class="muted ex-caption">
                {if coinbase {
                    "Strands meet in the middle. A coinbase's shielded outputs are public by \
                     consensus, though the Explorer does not decrypt them."
                } else {
                    "Strands meet in the middle: the chain does not say which input paid which \
                     output, and inside a shielded pool the amount of each note stays hidden."
                }}
            </p>
        </section>
        {(!page.shielded.is_empty()).then(|| view! {
            <section class="ex-section" aria-labelledby="ex-shielded-title">
                <h2 id="ex-shielded-title">"Shielded parts"</h2>
                <div class="ex-cards">
                    {page
                        .shielded
                        .iter()
                        .cloned()
                        .map(|part| view! { <PoolCard part unit coinbase /> })
                        .collect_view()}
                </div>
            </section>
        })}
        <section class="ex-section" aria-labelledby="ex-transparent-title">
            <h2 id="ex-transparent-title">"Transparent inputs and outputs"</h2>
            <Inputs page=page.clone() nav unit flow=flow.clone() lit />
            <Outputs page=page.clone() unit out flow lit />
        </section>
        <section class="ex-section" aria-labelledby="ex-fee-title">
            <h2 id="ex-fee-title">"Fee"</h2>
            <FeeCard page=page.clone() unit network resolve />
        </section>
        <section class="ex-section">
            <details class="ex-details">
                <summary>"Details"</summary>
                <dl class="rows ex-detail-rows">
                    <dt>"Version"</dt>
                    <dd>{page.version}</dd>
                    {page.size.map(|size| view! { <dt>"Size"</dt><dd>{bytes(u64::from(size))}</dd> })}
                    <dt>"Lock time"</dt>
                    <dd>{page.lock_time}</dd>
                    <dt>"Expiry height"</dt>
                    <dd>
                        {page
                            .expiry_height
                            .map_or_else(|| "none".to_owned(), |height| thousands(u64::from(height)))}
                    </dd>
                </dl>
            </details>
        </section>
    }
}

/// Inputs whose spent outputs were not read, and the button that reads them all.
#[component]
fn Unresolved(unresolved: u32, resolvable: bool, resolve: RwSignal<bool>) -> impl IntoView {
    (unresolved > 0).then(|| {
        view! {
            <p class="ex-unresolved">
                {format!(
                    "{} input{} spent output{} not read yet, so the fee and the balance are unknown.",
                    thousands(u64::from(unresolved)),
                    if unresolved == 1 { "'s" } else { "s'" },
                    if unresolved == 1 { " is" } else { "s are" }
                )}
                {resolvable.then(|| view! {
                    " "
                    <button type="button" disabled=move || resolve.get() on:click=move |_| resolve.set(true)>
                        "Read them all"
                    </button>
                })}
            </p>
        }
    })
}

/// The fee card: the fee, ZIP 317's logical actions part by part, the conventional fee, per
/// action, the multiple and zebrad's weight ratio, with what they mean for a waiting or a mined
/// transaction.
#[component]
fn FeeCard(page: Page, unit: Unit, network: Network, resolve: RwSignal<bool>) -> impl IntoView {
    let waiting = page.status == TxStatus::Mempool;
    let burns = page.burns_fees.then_some(
        "From NU7, 60 % of a block's fees are burned and 40 % go to the miner; the split applies \
         to the block's total, not to each transaction.",
    );
    match page.fee {
        None => view! {
            <p>"None: a coinbase pays no fee; it claims the block's, above."</p>
        }
        .into_any(),
        Some(Shown::Public { zat }) => {
            let figures = FeeFigures::new(u64::try_from(zat).unwrap_or(0), &page.actions);
            let verdict = text::fee_verdict(&figures, &page.status);
            view! {
                <dl class="rows ex-fee-rows">
                    <dt>"Fee"</dt>
                    <dd>{text::amount(zat, unit)}</dd>
                    <dt title="ZIP 317, as zebrad counts them">"Logical actions"</dt>
                    <dd>{text::actions_breakdown(&page.actions)}</dd>
                    <dt>"Conventional fee"</dt>
                    <dd>
                        {format!(
                            "{} (1,000 zat × {} actions{})",
                            text::amount(figures.conventional as i64, unit),
                            figures.paid,
                            if figures.logical < figures.paid { ", at least two" } else { "" }
                        )}
                    </dd>
                    <dt>"Per action"</dt>
                    <dd>{format!("{} zat", thousands(figures.per_action))}</dd>
                    <dt>"Multiple"</dt>
                    <dd>{text::multiple_text(figures.multiple)}</dd>
                    <dt title="The fee over the conventional fee, capped at 10">"Weight ratio"</dt>
                    <dd>{format!("{:.2} of at most 10", figures.ratio)}</dd>
                </dl>
                <p>{verdict}</p>
                {waiting.then(|| view! {
                    <p class="muted ex-caption">
                        "zebrad picks transactions for its block template at random, weighted by \
                         this ratio."
                    </p>
                })}
                {(waiting && network == Network::Mainnet).then(|| view! {
                    <p class="muted ex-caption">
                        "Wallets pay 5,000 zat per action until Mainnet block 3,590,000, then 1,000."
                    </p>
                })}
                {burns.map(|burns| view! { <p class="muted ex-caption">{burns}</p> })}
            }
            .into_any()
        }
        Some(Shown::Unknown { .. } | Shown::Hidden) => view! {
            <p>
                "Public, but some of the outputs this transaction spends were not read, so the \
                 fee is unknown here."
            </p>
            <Unresolved unresolved=page.unresolved resolvable=page.resolvable resolve />
            <p class="muted ex-caption">
                {format!(
                    "It is counted as {} logical actions: a conventional fee of {}.",
                    explorer_model::zip317::logical_actions(&page.actions),
                    text::amount(
                        explorer_model::zip317::conventional_fee(
                            explorer_model::zip317::logical_actions(&page.actions)
                        ) as i64,
                        unit
                    )
                )}
            </p>
        }
        .into_any(),
    }
}

/// Where the transaction is, and the confirmations meter (V7) once it is mined.
#[component]
fn Status(status: TxStatus, nav: NodeExplorer) -> impl IntoView {
    match status {
        TxStatus::Mined { block, height, .. } => {
            let route = ExplorerRoute::Block {
                id: BlockId::Hash(block),
            };
            let progress = move || {
                nav.live.with(|live| {
                    live.as_ref().and_then(|live| live.tip).map(|tip| {
                        let count = text::confirmations(height, tip.height);
                        (count, text::finality(height, tip.height))
                    })
                })
            };
            view! {
                <p class="ex-meta">
                    <span>"Mined in block "</span>
                    <RouteLink nav route>{thousands(u64::from(height))}</RouteLink>
                </p>
                {move || progress().map(|(count, finality)| {
                    let fraction = text::confirmations_fraction(count);
                    view! {
                        <div class="ex-confirmations">
                            <div
                                class="ex-meter"
                                role="meter"
                                aria-label="Confirmations, on a log scale to 1,001"
                                aria-valuemin="0"
                                aria-valuemax="1001"
                                aria-valuenow=count.min(1_001).to_string()
                            >
                                <span class="ex-meter-fill" style:width=format!("{:.2}%", fraction * 100.0)></span>
                                {[10u32, 100, 1_000].into_iter().map(|mark| view! {
                                    <span class="ex-meter-tick" style:left=format!("{:.2}%", text::confirmations_fraction(mark) * 100.0)></span>
                                }).collect_view()}
                            </div>
                            <p class="ex-meter-label">
                                {format!("{} confirmation{} · {}", thousands(u64::from(count)), if count == 1 { "" } else { "s" }, text::finality_text(finality))}
                            </p>
                            <p class="muted ex-caption">
                                "Zebra treats blocks 1,000 deep as final; this is Zebra's policy, not a consensus rule."
                            </p>
                        </div>
                    }
                })}
            }
            .into_any()
        }
        TxStatus::Mempool => view! {
            <p class="ex-meta">"Waiting in your node's mempool; the page follows it into a block."</p>
        }
        .into_any(),
        TxStatus::SideChain { block } => view! {
            <p class="ex-meta warn">
                "In a block outside your node's best chain"
                {block.map(|block| format!(" ({})", short_hash(&block.to_hex())))}
                "."
            </p>
        }
        .into_any(),
    }
}

/// One shielded pool's part: counts, the net movement, and the per-note amounts: hidden, or in
/// a coinbase public by consensus but not decrypted here.
#[component]
fn PoolCard(part: PoolPart, unit: Unit, coinbase: bool) -> impl IntoView {
    let counts = match part.pool {
        Pool::Sapling => format!(
            "{} spend{} · {} output{}",
            part.spends,
            if part.spends == 1 { "" } else { "s" },
            part.outputs,
            if part.outputs == 1 { "" } else { "s" }
        ),
        Pool::Sprout => format!(
            "{} JoinSplit{}",
            part.joinsplits,
            if part.joinsplits == 1 { "" } else { "s" }
        ),
        _ => format!(
            "{} action{}",
            part.actions,
            if part.actions == 1 { "" } else { "s" }
        ),
    };
    let rule = match part.pool {
        Pool::Orchard => Some(
            "From NU6.3, every Orchard action pays the receiver of the note it spends: value can \
             only leave Orchard.",
        ),
        Pool::Sprout => Some("Closed to new value since Canopy; no JoinSplits from NU7."),
        _ => None,
    };
    view! {
        <div class=format!("card ex-card ex-card-{}", part.pool.id())>
            <h3><PoolChip pool=part.pool /></h3>
            <p>{counts}</p>
            <p>{text::movement(part.pool, -part.net_out, unit)}</p>
            {if coinbase {
                view! {
                    <p title="A coinbase's shielded outputs use the all-zero outgoing viewing key, so anyone can decrypt them">
                        "Amount per note: public by consensus, not decrypted here"
                    </p>
                }
                .into_any()
            } else {
                view! { <p class="ex-hidden-row">"Amount per note: " <Hidden /></p> }.into_any()
            }}
            {rule.map(|rule| view! { <p class="muted ex-caption">{rule}</p> })}
        </div>
    }
}

#[component]
fn Inputs(
    page: Page,
    nav: NodeExplorer,
    unit: Unit,
    flow: explorer_model::flow::Flow,
    lit: RwSignal<Option<Lit>>,
) -> impl IntoView {
    if page.inputs.is_empty() {
        return view! { <p class="muted">"No transparent inputs."</p> }.into_any();
    }
    if matches!(page.kind, TxKind::Coinbase { .. }) {
        return view! {
            <h3>"Inputs (1)"</h3>
            <p>"Coinbase: new coins and the block's fees"</p>
        }
        .into_any();
    }
    let status = page.status.clone();
    view! {
        <h3>{format!("Inputs ({})", page.inputs.len())}</h3>
        <div class="ex-table-wrap">
            <table class="ex-table">
                <thead>
                    <tr>
                        <th scope="col" class="ex-num">"#"</th>
                        <th scope="col">"Spends"</th>
                        <th scope="col">"Address"</th>
                        <th scope="col" class="ex-num">{format!("Value ({unit})")}</th>
                        <th scope="col" class="ex-num" title="Blocks between the output and this transaction">"Age"</th>
                        <th scope="col"><span class="visually-hidden">"Made by"</span></th>
                    </tr>
                </thead>
                <tbody>
                    {page
                        .inputs
                        .iter()
                        .enumerate()
                        .map(|(index, input)| {
                            let node = input_node(&flow, &page, index).map(Lit::Left);
                            let spends = input.prevout.map(|prevout| {
                                let route = ExplorerRoute::Tx {
                                    txid: prevout.txid,
                                    out: Some(prevout.vout),
                                };
                                view! {
                                    <RouteLink nav route>
                                        <code>{format!("{}:{}", short_hash(&prevout.txid.to_hex()), prevout.vout)}</code>
                                    </RouteLink>
                                }
                            });
                            let spent = input.spent.clone();
                            let age = text::coin_age(spent.as_ref().and_then(|spent| spent.height), &status);
                            view! {
                                <tr
                                    class:ex-row-lit=move || node.is_some() && lit.get() == node
                                    on:mouseenter=move |_| lit.set(node)
                                    on:mouseleave=move |_| lit.set(None)
                                >
                                    <td class="ex-num">{index}</td>
                                    <td>{spends}</td>
                                    {match spent {
                                        Some(spent) => view! {
                                            <td><code class="ex-address">{spent.address.unwrap_or_else(|| spent.script.clone())}</code></td>
                                            <td class="ex-num">{unit.number(spent.zat as i64)}</td>
                                            <td class="ex-num">{age}</td>
                                            <td>{spent.coinbase.then(|| view! { <span class="chip">"coinbase"</span> })}</td>
                                        }
                                        .into_any(),
                                        None => view! {
                                            <td class="muted">"not read"</td>
                                            <td class="ex-num muted">"unknown"</td>
                                            <td></td>
                                            <td></td>
                                        }
                                        .into_any(),
                                    }}
                                </tr>
                            }
                        })
                        .collect_view()}
                </tbody>
            </table>
        </div>
    }
    .into_any()
}

#[component]
fn Outputs(
    page: Page,
    unit: Unit,
    out: Option<u32>,
    flow: explorer_model::flow::Flow,
    lit: RwSignal<Option<Lit>>,
) -> impl IntoView {
    if page.outputs.is_empty() {
        return view! { <p class="muted">"No transparent outputs."</p> }.into_any();
    }
    view! {
        <h3>{format!("Outputs ({})", page.outputs.len())}</h3>
        <div class="ex-table-wrap">
            <table class="ex-table">
                <thead>
                    <tr>
                        <th scope="col" class="ex-num">"#"</th>
                        <th scope="col">"Address"</th>
                        <th scope="col">"Script"</th>
                        <th scope="col" class="ex-num">{format!("Value ({unit})")}</th>
                    </tr>
                </thead>
                <tbody>
                    {page
                        .outputs
                        .into_iter()
                        .map(|output| {
                            // The output an input link led here for: marked in words and by a
                            // bar, not by the row's tint alone.
                            let followed = out == Some(output.n);
                            let node = output_node(&flow, output.n).map(Lit::Right);
                            view! {
                                <tr
                                    class:ex-row-selected=followed
                                    class:ex-row-lit=move || node.is_some() && lit.get() == node
                                    aria-current=followed.then_some("true")
                                    on:mouseenter=move |_| lit.set(node)
                                    on:mouseleave=move |_| lit.set(None)
                                >
                                    <td class="ex-num">
                                        {followed.then(|| view! {
                                            <span class="chip ex-followed" title="The output the input you followed spends">
                                                "followed"
                                            </span>
                                        })}
                                        {output.n}
                                    </td>
                                    <td><code class="ex-address">{output.address.unwrap_or_default()}</code></td>
                                    <td>{output.script}</td>
                                    <td class="ex-num">{unit.number(output.zat as i64)}</td>
                                </tr>
                            }
                        })
                        .collect_view()}
                </tbody>
            </table>
        </div>
    }
    .into_any()
}
