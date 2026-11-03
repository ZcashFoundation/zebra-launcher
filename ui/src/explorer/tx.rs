//! The Transaction page, from the transaction's public parts: where it is and how settled
//! (V7), what it did in one sentence, a card per shielded pool (counts and the net movement;
//! per-note amounts are hidden), the transparent inputs and outputs, the fee when the inputs
//! do not hide it, and the details. Input values come with a later version.

use launcher_types::{
    Network, NodeId,
    explorer::{
        BlockId, ExplorerRequest, ExplorerResponse, ExplorerRoute, Pool, PoolPart, Shown, TxId,
        TxPage as Page, TxStatus,
    },
};
use leptos::prelude::*;

use super::{
    parts::{Failure, HashText, Hidden, KindBadge, Loading, PoolChip, RouteLink},
    state::{Loaded, NodeExplorer, load},
    text,
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
    let unit = text::unit(network);
    let tip = move || {
        nav.live.with(|live| {
            live.as_ref()
                .and_then(|live| live.tip)
                .map_or((0, 0), |tip| (tip.epoch, tip.height))
        })
    };
    // A waiting transaction is asked about again at every new tip, until it is mined; any
    // transaction after a reorganisation, which may have replaced its block.
    let waiting = RwSignal::new(false);
    let answer = load(node, nav, move || {
        let (epoch, height) = tip();
        let height = if waiting.get() { height } else { 0 };
        Some((
            ExplorerRequest::Tx { txid },
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
    view! {
        <div class="ex-tx" class:ex-refreshing=move || answer.refreshing.get()>
            {move || match answer.value.get() {
                Loaded::Loading => view! { <Loading /> }.into_any(),
                Loaded::Failed(error) => view! { <Failure error /> }.into_any(),
                Loaded::Ready(reply) => match reply.response {
                    ExplorerResponse::Tx(page) => view! { <Tx page=*page nav unit out /> }.into_any(),
                    _ => view! { <Loading /> }.into_any(),
                },
            }}
        </div>
    }
}

#[component]
fn Tx(page: Page, nav: NodeExplorer, unit: &'static str, out: Option<u32>) -> impl IntoView {
    let sentence = text::tx_sentence(&page, unit);
    let status = page.status.clone();
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
        {(!page.shielded.is_empty()).then(|| view! {
            <section class="ex-section" aria-labelledby="ex-shielded-title">
                <h2 id="ex-shielded-title">"Shielded parts"</h2>
                <div class="ex-cards">
                    {page
                        .shielded
                        .iter()
                        .cloned()
                        .map(|part| view! { <PoolCard part unit /> })
                        .collect_view()}
                </div>
            </section>
        })}
        <section class="ex-section" aria-labelledby="ex-transparent-title">
            <h2 id="ex-transparent-title">"Transparent inputs and outputs"</h2>
            <Inputs page=page.clone() nav />
            <Outputs page=page.clone() unit out />
        </section>
        <section class="ex-section" aria-labelledby="ex-fee-title">
            <h2 id="ex-fee-title">"Fee"</h2>
            <p>
                {match page.fee {
                    Some(Shown::Public { zat }) => text::amount(zat, unit),
                    Some(Shown::Unknown { .. }) => {
                        "Public, but it needs the values of the outputs this transaction spends, \
                         which this version of the Explorer does not read."
                            .to_owned()
                    }
                    Some(Shown::Hidden) => "hidden".to_owned(),
                    None => "None: a coinbase pays no fee; it claims the block's.".to_owned(),
                }}
            </p>
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

/// One shielded pool's part: counts, the net movement, and the hidden per-note amounts.
#[component]
fn PoolCard(part: PoolPart, unit: &'static str) -> impl IntoView {
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
            <p class="ex-hidden-row">"Amount per note: " <Hidden /></p>
            {rule.map(|rule| view! { <p class="muted ex-caption">{rule}</p> })}
        </div>
    }
}

#[component]
fn Inputs(page: Page, nav: NodeExplorer) -> impl IntoView {
    if page.inputs.is_empty() {
        return view! { <p class="muted">"No transparent inputs."</p> }.into_any();
    }
    view! {
        <h3>{format!("Inputs ({})", page.inputs.len())}</h3>
        <ol class="ex-inputs">
            {page
                .inputs
                .into_iter()
                .map(|input| match input.prevout {
                    Some(prevout) => {
                        let route = ExplorerRoute::Tx {
                            txid: prevout.txid,
                            out: Some(prevout.vout),
                        };
                        view! {
                            <li>
                                <RouteLink nav route>
                                    <code>{format!("{}:{}", short_hash(&prevout.txid.to_hex()), prevout.vout)}</code>
                                </RouteLink>
                            </li>
                        }
                        .into_any()
                    }
                    None => view! { <li>"Coinbase: new coins and the block's fees"</li> }.into_any(),
                })
                .collect_view()}
        </ol>
        <p class="muted ex-caption">"Each input spends an earlier output; its value shows on that transaction's page."</p>
    }
    .into_any()
}

#[component]
fn Outputs(page: Page, unit: &'static str, out: Option<u32>) -> impl IntoView {
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
                        .map(|output| view! {
                            <tr class:ex-row-selected=out == Some(output.n)>
                                <td class="ex-num">{output.n}</td>
                                <td><code class="ex-address">{output.address.unwrap_or_default()}</code></td>
                                <td>{output.script}</td>
                                <td class="ex-num">{text::coins(output.zat as i64)}</td>
                            </tr>
                        })
                        .collect_view()}
                </tbody>
            </table>
        </div>
    }
    .into_any()
}
