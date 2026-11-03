//! The Block page: height, hash, time, confirmations and finality; the block in a sentence;
//! the turnstile of value between the pools (V2), the reward split (V3), the shielded budget
//! (V4); the transaction ids, 200 a page; the header's details. A height above the tip counts
//! down instead, and becomes the block once the node has it.

use std::time::{Duration, UNIX_EPOCH};

use launcher_types::{
    Network, NodeId,
    explorer::{
        BlockId, BlockLedger, BlockPage as Page, Budget, ExplorerErrorKind, ExplorerRequest,
        ExplorerResponse, ExplorerRoute, FutureBlock, Pool, RewardSplit, TxidPage,
    },
};
use leptos::prelude::*;

use super::{
    parts::{Failure, HashText, Loading, PoolChip, RouteLink},
    state::{Loaded, NodeExplorer, load},
    text,
};
use crate::{
    app::{AppState, Tab},
    format::{bytes, local_datetime_short, relative, short_hash, thousands},
};

#[component]
pub fn BlockPage(node: NodeId, nav: NodeExplorer, network: Network, id: BlockId) -> impl IntoView {
    let app = AppState::get();
    let unit = text::unit(network);
    let tip = move || {
        nav.live.with(|live| {
            live.as_ref()
                .and_then(|live| live.tip.map(|tip| tip.height))
        })
    };
    let epoch = move || {
        nav.live.with(|live| {
            live.as_ref()
                .and_then(|live| live.tip)
                .map_or(0, |tip| tip.epoch)
        })
    };
    // A height above the tip asks again at every new tip, so the countdown becomes the block;
    // a height asks again after a reorganisation, which may have put another block there.
    let future = RwSignal::new(false);
    let answer = load(node.clone(), nav, move || {
        let revision = match id {
            _ if future.get() => u64::from(tip().unwrap_or(0)),
            BlockId::Height(_) => epoch() << 32,
            BlockId::Hash(_) => 0,
        };
        Some((ExplorerRequest::Block { id }, revision))
    });
    Effect::new(move |_| {
        let is_future = matches!(
            answer.value.get(),
            Loaded::Ready(ref reply) if matches!(reply.response, ExplorerResponse::Future(_))
        );
        future.set(is_future);
    });
    view! {
        <div class="ex-block" class:ex-refreshing=move || answer.refreshing.get()>
            {move || match answer.value.get() {
                Loaded::Loading => view! { <Loading /> }.into_any(),
                Loaded::Failed(error) => match error.kind {
                    ExplorerErrorKind::PastTip { height, node_height } => view! {
                        <section class="ex-section">
                            <h2>{format!("Block {}", thousands(u64::from(height)))}</h2>
                            <p>
                                {format!(
                                    "Your node has blocks up to {}; it is still catching up.",
                                    thousands(u64::from(node_height))
                                )}
                            </p>
                            <button type="button" on:click=move |_| app.tab.set(Tab::Dashboard)>
                                "See the sync on the Dashboard"
                            </button>
                        </section>
                    }
                    .into_any(),
                    _ => view! { <Failure error /> }.into_any(),
                },
                Loaded::Ready(reply) => match reply.response {
                    ExplorerResponse::Block(page) => {
                        view! { <Block page=*page nav unit node=node.clone() /> }.into_any()
                    }
                    ExplorerResponse::Future(future) => view! { <Future future nav /> }.into_any(),
                    _ => view! { <Loading /> }.into_any(),
                },
            }}
        </div>
    }
}

#[component]
fn Block(page: Page, nav: NodeExplorer, unit: &'static str, node: NodeId) -> impl IntoView {
    let app = AppState::get();
    let summary = page.summary.clone();
    let height = summary.height;
    let time = UNIX_EPOCH + Duration::from_secs(summary.time);
    let confirmations = move || {
        nav.live.with(|live| {
            live.as_ref().and_then(|live| live.tip).map(|tip| {
                (
                    text::confirmations(height, tip.height),
                    text::finality(height, tip.height),
                )
            })
        })
    };
    let previous = height.checked_sub(1).map(|previous| ExplorerRoute::Block {
        id: BlockId::Height(previous),
    });
    let next = ExplorerRoute::Block {
        id: BlockId::Height(height + 1),
    };
    let sentence = text::block_sentence(&summary, unit);
    let check = text::check_text(&page.check, unit);
    view! {
        <section class="ex-section ex-block-head" aria-labelledby="ex-block-title">
            <div class="ex-title-row">
                {previous.map(|route| view! {
                    <RouteLink nav route>
                        <span aria-hidden="true">"◀"</span>
                        <span class="visually-hidden">"Previous block"</span>
                    </RouteLink>
                })}
                <h2 id="ex-block-title">{format!("Block {}", thousands(u64::from(height)))}</h2>
                <RouteLink nav route=next>
                    <span aria-hidden="true">"▶"</span>
                    <span class="visually-hidden">"Next block"</span>
                </RouteLink>
                {page.era.clone().map(|era| view! { <span class="chip">{era}</span> })}
                {page.activates.clone().map(|upgrade| view! {
                    <span class="chip ex-chip-accent">{format!("activates {upgrade}")}</span>
                })}
            </div>
            <HashText hash=summary.hash full=true />
            <p class="ex-meta">
                <span>{local_datetime_short(time)}</span>
                <span class="muted">{move || relative(time, app.now.get())}</span>
                {move || confirmations().map(|(count, finality)| view! {
                    <span>{format!("{} confirmation{}", thousands(u64::from(count)), if count == 1 { "" } else { "s" })}</span>
                    <span class="chip" class:ex-chip-final=matches!(finality, text::Finality::Final)
                        title="Zebra treats blocks 1,000 deep as final; this is Zebra's policy, not a consensus rule">
                        {text::finality_text(finality)}
                    </span>
                })}
            </p>
            <p class="ex-sentence">{sentence}</p>
        </section>
        <section class="ex-section" aria-labelledby="ex-flows-title">
            <h2 id="ex-flows-title">"Value between the pools"</h2>
            <Turnstile ledger=summary.ledger.clone() unit />
            <p class="muted ex-caption">{check}</p>
            <p class="muted ex-caption">
                "Aggregate pool flows do not identify users or prove that coins were sold."
            </p>
        </section>
        {page.reward.clone().map(|reward| view! {
            <section class="ex-section" aria-labelledby="ex-reward-title">
                <h2 id="ex-reward-title">"Where the reward went"</h2>
                <RewardBar reward unit />
            </section>
        })}
        <section class="ex-section" aria-labelledby="ex-budget-title">
            <h2 id="ex-budget-title">"How full"</h2>
            <BudgetMeter ledger=summary.ledger.clone() />
        </section>
        <section class="ex-section" aria-labelledby="ex-txs-title">
            <h2 id="ex-txs-title">
                {format!("Transactions ({})", thousands(u64::from(summary.tx_count)))}
            </h2>
            <Txids first=page.txids.clone() hash=summary.hash nav node />
        </section>
        <section class="ex-section">
            <details class="ex-details">
                <summary>"Header details"</summary>
                <Details page nav />
            </details>
        </section>
    }
}

/// The turnstile (V2): each pool's change, split into what the coinbase paid and what users'
/// transactions moved, then the fees and the burn. Exact zatoshis in a table.
#[component]
fn Turnstile(ledger: BlockLedger, unit: &'static str) -> impl IntoView {
    let (Some(deltas), Some(coinbase), Some(users)) =
        (ledger.deltas, ledger.coinbase, ledger.users)
    else {
        return view! {
            <p>
                "zebrad has no value-pool row for this block or its parent, so its pool changes \
                 and fees are unknown here."
            </p>
        }
        .into_any();
    };
    let largest = Pool::ALL
        .iter()
        .map(|pool| deltas.get(*pool).unsigned_abs().max(coinbase.get(*pool)))
        .max()
        .unwrap_or(1)
        .max(1) as f64;
    let width = |zat: u64| format!("{:.2}%", zat as f64 / largest * 50.0);
    let rows = Pool::ALL.into_iter().map(|pool| {
        let delta = deltas.get(pool);
        let paid = coinbase.get(pool);
        let moved = users.get(pool);
        let lockbox = pool == Pool::Lockbox;
        view! {
            <tr>
                <th scope="row"><PoolChip pool /></th>
                <td class="ex-bar-cell" aria-hidden="true">
                    <span class="ex-axis"></span>
                    {(paid > 0).then(|| view! {
                        <span class=format!("ex-flow ex-flow-coinbase ex-fill-{}", pool.id()) style:width=width(paid)></span>
                    })}
                    {(moved != 0 || lockbox && delta != 0).then(|| {
                        let amount = if lockbox { delta } else { moved };
                        view! {
                            <span
                                class=format!("ex-flow ex-fill-{}", pool.id())
                                class:ex-flow-out=amount < 0
                                style:width=width(amount.unsigned_abs())
                            ></span>
                        }
                    })}
                </td>
                <td class="ex-num">{text::coins(paid as i64)}</td>
                <td class="ex-num">{(!lockbox).then(|| text::coins(moved))}</td>
                <td class="ex-num" title=format!("{delta} zatoshis")>{text::coins(delta)}</td>
            </tr>
        }
    });
    let fees = ledger.fees;
    let burned = ledger.burned;
    view! {
        <div class="ex-table-wrap">
            <table class="ex-table ex-turnstile">
                <thead>
                    <tr>
                        <th scope="col">"Pool"</th>
                        <th scope="col" class="ex-bar-cell"><span class="visually-hidden">"Change"</span></th>
                        <th scope="col" class="ex-num" title="Coinbase outputs: the subsidy and the claimed fees">"Coinbase"</th>
                        <th scope="col" class="ex-num" title="The block's other transactions, fees included">"Users"</th>
                        <th scope="col" class="ex-num">{format!("Change ({unit})")}</th>
                    </tr>
                </thead>
                <tbody>{rows.collect_view()}</tbody>
                <tfoot>
                    {fees.map(|fees| view! {
                        <tr>
                            <th scope="row">"Fees"</th>
                            <td></td><td></td><td></td>
                            <td class="ex-num">{text::coins(fees as i64)}</td>
                        </tr>
                    })}
                    {burned.map(|burned| view! {
                        <tr>
                            <th scope="row" title="NU7 removes ⌊6 F / 10⌋ of a block's fees from circulation">"Burned"</th>
                            <td></td><td></td><td></td>
                            <td class="ex-num">{text::coins(burned as i64)}</td>
                        </tr>
                    })}
                </tfoot>
            </table>
        </div>
        <p class="muted ex-caption">
            "Lighter bars: what the coinbase paid in (the subsidy and the claimed fees). Darker: \
             the block's other transactions, net. The lockbox row is the deferred development fund."
        </p>
    }
    .into_any()
}

/// The reward split (V3): one bar of the miner's part, the streams, the fees the miner kept and
/// the burned fees, with the amounts and payout addresses.
#[component]
fn RewardBar(reward: RewardSplit, unit: &'static str) -> impl IntoView {
    let mut parts: Vec<(String, u64, &'static str, Option<String>)> =
        vec![("Miner".to_owned(), reward.miner, "ex-fill-miner", None)];
    for stream in &reward.streams {
        let class = if stream.lockbox {
            "ex-fill-lockbox"
        } else {
            "ex-fill-stream"
        };
        parts.push((
            stream.recipient.clone(),
            stream.zat,
            class,
            stream.address.clone(),
        ));
    }
    if let Some(fees) = reward.fees_to_miner {
        parts.push(("Fees to the miner".to_owned(), fees, "ex-fill-fees", None));
    }
    if let Some(burned) = reward.burned {
        parts.push(("Burned fees".to_owned(), burned, "ex-fill-burned", None));
    }
    let total: u64 = parts.iter().map(|part| part.1).sum::<u64>().max(1);
    let summary = format!(
        "Subsidy {}: {}",
        text::amount(reward.subsidy as i64, unit),
        parts
            .iter()
            .map(|(name, zat, _, _)| format!("{name} {}", text::coins(*zat as i64)))
            .collect::<Vec<_>>()
            .join(", ")
    );
    view! {
        <div class="ex-stack" role="img" aria-label=summary>
            {parts
                .iter()
                .map(|(name, zat, class, _)| view! {
                    <span
                        class=format!("ex-stack-part {class}")
                        style:width=format!("{:.2}%", *zat as f64 / total as f64 * 100.0)
                        title=format!("{name}: {}", text::amount(*zat as i64, unit))
                    ></span>
                })
                .collect_view()}
        </div>
        <dl class="rows ex-reward-rows">
            <dt>"Subsidy"</dt>
            <dd>{text::amount(reward.subsidy as i64, unit)}</dd>
            {parts
                .into_iter()
                .map(|(name, zat, class, address)| view! {
                    <dt>
                        <span class=format!("ex-swatch {class}") aria-hidden="true"></span>
                        {name}
                    </dt>
                    <dd>
                        {text::amount(zat as i64, unit)}
                        {address.map(|address| view! { <code class="ex-address">{address}</code> })}
                    </dd>
                })
                .collect_view()}
        </dl>
    }
}

/// The budget meter (V4): from NU7, shielded spends, outputs and actions against ZIP 218's
/// 330, ticks at 300 (Sapling's own limit) and 330; before NU7, the size against 2 MB.
#[component]
fn BudgetMeter(ledger: BlockLedger) -> impl IntoView {
    let budget = ledger.budget;
    let fraction = text::budget_fraction(&budget)
        .unwrap_or(0.0)
        .clamp(0.0, 1.0);
    let label = text::budget_text(&budget);
    let caption = match budget {
        Budget::Units {
            complete: false, ..
        } => {
            "Sapling outputs and Orchard and Ironwood actions, from the note trees. Sapling spends \
             are not counted here, so the block holds at least this much. ZIP 218 allows 330 from \
             NU7, at most 300 of them Sapling."
        }
        Budget::Units { .. } => "Shielded spends, outputs and actions against ZIP 218's 330.",
        Budget::Bytes { .. } => "Before NU7 a block is limited by its size: 2,000,000 bytes.",
        Budget::Unknown => "zebrad has no row for this block, so its size is unknown.",
    };
    let units = matches!(budget, Budget::Units { .. });
    view! {
        <div class="ex-meter" role="img" aria-label=format!("{label}: {caption}")>
            <span class="ex-meter-fill" style:width=format!("{:.2}%", fraction * 100.0)></span>
            {units.then(|| view! {
                <span class="ex-meter-tick" style:left=format!("{:.2}%", 300.0 / 330.0 * 100.0)></span>
            })}
        </div>
        <p class="ex-meter-label">{label}</p>
        <p class="muted ex-caption">{caption}</p>
    }
}

/// The block's transaction ids, 200 a page.
#[component]
fn Txids(
    first: TxidPage,
    hash: launcher_types::explorer::BlockHash,
    nav: NodeExplorer,
    node: NodeId,
) -> impl IntoView {
    let offset = RwSignal::new(0u32);
    let total = first.total;
    let first_page = first.clone();
    let answer = load(node, nav, move || {
        let offset = offset.get();
        (offset > 0).then_some((
            ExplorerRequest::BlockTxs {
                hash,
                offset,
                limit: 200,
            },
            0,
        ))
    });
    let page = move || {
        if offset.get() == 0 {
            return Some(first_page.clone());
        }
        match answer.value.get() {
            Loaded::Ready(reply) => match reply.response {
                ExplorerResponse::BlockTxs(page) => Some(page),
                _ => None,
            },
            _ => None,
        }
    };
    view! {
        {move || match page() {
            Some(page) => {
                let start = page.offset;
                view! {
                    <ol class="ex-txids" start=start + 1>
                        {page
                            .txids
                            .into_iter()
                            .enumerate()
                            .map(|(index, txid)| {
                                let route = ExplorerRoute::Tx { txid, out: None };
                                let coinbase = start == 0 && index == 0;
                                view! {
                                    <li>
                                        <RouteLink nav route>
                                            <code>{short_hash(&txid.to_hex())}</code>
                                        </RouteLink>
                                        {coinbase.then(|| view! { <span class="ex-kind">"Coinbase"</span> })}
                                    </li>
                                }
                            })
                            .collect_view()}
                    </ol>
                }
                .into_any()
            }
            None => view! { <Loading /> }.into_any(),
        }}
        {(total > 200).then(|| view! {
            <div class="buttons ex-pager">
                <button
                    type="button"
                    disabled=move || offset.get() == 0
                    on:click=move |_| offset.update(|offset| *offset = offset.saturating_sub(200))
                >
                    "Previous 200"
                </button>
                <span class="muted">
                    {move || format!(
                        "{}–{} of {}",
                        thousands(u64::from(offset.get() + 1)),
                        thousands(u64::from((offset.get() + 200).min(total))),
                        thousands(u64::from(total))
                    )}
                </span>
                <button
                    type="button"
                    disabled=move || offset.get() + 200 >= total
                    on:click=move |_| offset.update(|offset| *offset += 200)
                >
                    "Next 200"
                </button>
            </div>
        })}
    }
}

/// The header fields, folded away.
#[component]
fn Details(page: Page, nav: NodeExplorer) -> impl IntoView {
    let details = page.details;
    let row = |label: &'static str, value: Option<String>| {
        value.map(|value| view! { <dt>{label}</dt><dd><code>{value}</code></dd> })
    };
    let trees = details.trees;
    view! {
        <dl class="rows ex-detail-rows">
            {row("Version", details.version.map(|version| version.to_string()))}
            {row("Merkle root", details.merkle_root.clone())}
            {row("Block commitments", details.block_commitments.clone())}
            {row("Final Sapling root", details.final_sapling_root.clone())}
            {row("Final Orchard root", details.final_orchard_root.clone())}
            {row("Bits", details.bits.clone())}
            {row("Nonce", details.nonce.clone())}
            {row("Difficulty", details.difficulty.map(text::difficulty))}
            {row(
                "Note trees",
                Some(format!(
                    "Sapling {} · Orchard {} · Ironwood {}",
                    thousands(trees[0]),
                    thousands(trees[1]),
                    thousands(trees[2])
                )),
            )}
            {row("Supply after it", details.chain_supply.map(|zat| format!("{} zatoshis", thousands(zat))))}
            {page.summary.size.map(|size| view! { <dt>"Size"</dt><dd>{bytes(u64::from(size))}</dd> })}
            {page.previous.map(|hash| view! {
                <dt>"Previous"</dt>
                <dd>
                    <RouteLink nav route=ExplorerRoute::Block { id: BlockId::Hash(hash) }>
                        <code>{short_hash(&hash.to_hex())}</code>
                    </RouteLink>
                </dd>
            })}
            {page.next.map(|hash| view! {
                <dt>"Next"</dt>
                <dd>
                    <RouteLink nav route=ExplorerRoute::Block { id: BlockId::Hash(hash) }>
                        <code>{short_hash(&hash.to_hex())}</code>
                    </RouteLink>
                </dd>
            })}
        </dl>
    }
}

/// A height above the tip: how far, what happens on the way, and the node's end of support.
#[component]
fn Future(future: FutureBlock, nav: NodeExplorer) -> impl IntoView {
    let height = future.height;
    let previous = ExplorerRoute::Block {
        id: BlockId::Height(future.tip),
    };
    let stops = future
        .end_of_support
        .filter(|end| u64::from(height) > *end)
        .map(|end| {
            format!(
                "Your zebrad stops at block {}: a newer release follows the chain past it.",
                thousands(end)
            )
        });
    view! {
        <section class="ex-section" aria-labelledby="ex-future-title">
            <h2 id="ex-future-title">{format!("Block {}", thousands(u64::from(height)))}</h2>
            <p class="ex-sentence">
                {format!(
                    "Not mined yet: {} block{} to go, {} at the target spacing.",
                    thousands(u64::from(future.blocks_left)),
                    if future.blocks_left == 1 { "" } else { "s" },
                    text::eta_text(future.eta_secs)
                )}
            </p>
            {future.activates.map(|upgrade| view! {
                <p><span class="chip ex-chip-accent">{format!("{upgrade} activates at this height")}</span></p>
            })}
            {(!future.upgrades_before.is_empty()).then(|| view! {
                <p>
                    {format!(
                        "On the way: {}.",
                        future
                            .upgrades_before
                            .iter()
                            .map(|(name, at)| format!("{name} at {}", thousands(u64::from(*at))))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )}
                </p>
            })}
            {stops.map(|stops| view! { <p class="warn">{stops}</p> })}
            <p class="muted">"The page becomes the block once your node has it."</p>
            <RouteLink nav route=previous>"Go to the tip"</RouteLink>
        </section>
    }
}
