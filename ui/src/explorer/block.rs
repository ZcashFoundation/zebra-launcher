//! The Block page: height, hash, time, confirmations and finality; the block in a sentence;
//! the turnstile of value between the pools (V2), the reward split (V3), the shielded budget
//! (V4); the transactions as a table on a virtual list, read from the node as their rows come
//! into view (a small block whole, a big one row by row); the header's details. A height above
//! the tip counts down instead, and becomes the block once the node has it. The blocks either
//! side are read ahead, so ◀ and ▶ open at once.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    time::{Duration, UNIX_EPOCH},
};

use launcher_types::{
    Network, NodeId,
    explorer::{
        BlockHash, BlockId, BlockLedger, BlockPage as Page, Budget, CardTarget, ExplorerCall,
        ExplorerError, ExplorerErrorKind, ExplorerReply, ExplorerRequest, ExplorerResponse,
        ExplorerRoute, FutureBlock, LiveState, PerPool, Pool, PrefetchTarget, RewardSplit,
        RowsRead, Shown, SourceCall, TxRow,
    },
};
use leptos::{prelude::*, task::spawn_local};

use super::{
    cards::{CARD_ID, HoverCards, hover_handlers},
    parts::{Failure, HashText, Loading, PoolChip, RouteLink, RpcCalls},
    prefs::Prefs,
    state::{Loaded, NodeExplorer, load},
    text::{self, Unit},
    virtual_list::{VirtualList, Window},
};
use crate::{
    api,
    app::{AppState, Tab},
    format::{bytes, relative, short_hash, thousands},
};

/// The rows asked for at once.
const CHUNK: u32 = 25;
/// The rows in view at once.
const ROWS_SHOWN: u32 = 14;
/// The reads Show RPC keeps for the table: the latest.
const TABLE_SOURCES: usize = 100;

#[component]
pub fn BlockPage(node: NodeId, nav: NodeExplorer, network: Network, id: BlockId) -> impl IntoView {
    let app = AppState::get();
    let prefs = Prefs::get();
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
    // The syncing node's height while it catches up, from the explorer's news.
    let syncing_at = move || {
        nav.live
            .with(|live| match live.as_ref().map(|live| &live.state) {
                Some(LiveState::Syncing { height, .. }) => Some(*height),
                _ => None,
            })
    };
    // A height above the tip asks again at every new tip, so the countdown becomes the block;
    // a height asks again after a reorganisation, which may have put another block there; a
    // height the syncing node had yet to reach asks again once, when the node has reached it.
    let future = RwSignal::new(false);
    let past_tip = RwSignal::new(false);
    let answer = load(node.clone(), nav, move || {
        let revision = match id {
            _ if future.get() => u64::from(tip().unwrap_or(0)),
            BlockId::Height(height) => {
                let reached =
                    past_tip.get() && syncing_at().is_none_or(|at| at >= height) && nav.reads();
                (epoch() << 32) | u64::from(reached)
            }
            BlockId::Hash(_) => 0,
        };
        Some((ExplorerRequest::Block { id }, revision))
    });
    // Once the block is in, the blocks either side are read ahead.
    let ahead = node.clone();
    Effect::new(move |_| {
        if let Loaded::Ready(reply) = answer.value.get()
            && let ExplorerResponse::Block(page) = &reply.response
        {
            let height = page.summary.height;
            let mut targets = Vec::new();
            if let Some(previous) = height.checked_sub(1) {
                targets.push(PrefetchTarget::Block {
                    id: BlockId::Height(previous),
                });
            }
            if page.next.is_some() {
                targets.push(PrefetchTarget::Block {
                    id: BlockId::Height(height + 1),
                });
            }
            let node = ahead.clone();
            spawn_local(async move {
                // Advice: a failure is not worth a banner.
                let _ = api::explorer(&node, ExplorerRequest::Prefetch { targets }).await;
            });
        }
    });
    let sources = Signal::derive(move || match answer.value.get() {
        Loaded::Ready(reply) => reply.sources,
        _ => None,
    });
    Effect::new(move |_| match answer.value.get() {
        Loaded::Ready(reply) => future.set(matches!(reply.response, ExplorerResponse::Future(_))),
        Loaded::Failed(error) => {
            future.set(false);
            if matches!(error.kind, ExplorerErrorKind::PastTip { .. }) {
                past_tip.set(true);
            }
        }
        Loaded::Loading => future.set(false),
    });
    // The table is drawn once per block: an answer for the same block again (Show RPC turned on
    // or off, the node reachable again) keeps its rows, its reads and its place.
    let block_page = move |value: &Loaded<ExplorerReply>| match value {
        Loaded::Ready(reply) => match &reply.response {
            ExplorerResponse::Block(page) => Some(page.clone()),
            _ => None,
        },
        _ => None,
    };
    let shown = Memo::new(move |_| {
        answer
            .value
            .with(|value| block_page(value).map(|page| page.summary.hash))
    });
    let table_node = node.clone();
    let table = move || {
        shown.get()?;
        let page = *answer.value.with_untracked(block_page)?;
        Some(view! { <TxTable page nav node=table_node.clone() network /> })
    };
    view! {
        <div class="ex-block" class:ex-refreshing=move || answer.refreshing.get()>
            {move || match answer.value.get() {
                Loaded::Loading => view! { <Loading /> }.into_any(),
                Loaded::Failed(error) => match error.kind {
                    ExplorerErrorKind::PastTip { height, node_height } => view! {
                        <section class="ex-section">
                            <h2>{format!("Block {}", thousands(u64::from(height)))}</h2>
                            <p>
                                {move || format!(
                                    "Your node has blocks up to {}; it is still catching up.",
                                    thousands(u64::from(syncing_at().unwrap_or(node_height)))
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
                        let page = *page;
                        // A new unit draws the same answer again; nothing is read for it. The
                        // table reads the unit itself.
                        (move || {
                            let unit = prefs.unit(network);
                            view! { <Block page=page.clone() nav unit /> }
                        })
                        .into_any()
                    }
                    ExplorerResponse::Future(future) => view! { <Future future nav /> }.into_any(),
                    _ => view! { <Loading /> }.into_any(),
                },
            }}
            {table}
            {move || answer.value.with(block_page).map(|page| view! {
                <section class="ex-section">
                    <details class="ex-details">
                        <summary>"Header details"</summary>
                        <Details page=*page nav />
                    </details>
                </section>
            })}
            <RpcCalls sources title="What this page asked your node" />
        </div>
    }
}

#[component]
fn Block(page: Page, nav: NodeExplorer, unit: Unit) -> impl IntoView {
    let app = AppState::get();
    let prefs = Prefs::get();
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
                <span>{move || text::time_text(summary.time, prefs.time.get())}</span>
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
    }
}

/// The turnstile (V2): each pool's change, split into what the coinbase paid and what users'
/// transactions moved, then the fees and the burn. Exact zatoshis in a table.
#[component]
fn Turnstile(ledger: BlockLedger, unit: Unit) -> impl IntoView {
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
    let largest = turnstile_scale(&deltas, &coinbase, &users).max(1) as f64;
    let width = |zat: u64| format!("{:.2}%", zat as f64 / largest * 50.0);
    let rows = Pool::ALL.into_iter().map(|pool| {
        let delta = deltas.get(pool);
        let paid = coinbase.get(pool);
        let moved = users.get(pool);
        let lockbox = pool == Pool::Lockbox;
        view! {
            <tr>
                <th scope="row"><PoolChip pool /></th>
                // The cell stays in the table for assistive technology, so every row has the
                // header's five cells; only the bars in it are hidden, and the numbers beside
                // them say the same.
                <td class="ex-bar-cell">
                    <span aria-hidden="true">
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
                    </span>
                </td>
                <td class="ex-num">{unit.number(paid as i64)}</td>
                <td class="ex-num">{(!lockbox).then(|| unit.number(moved))}</td>
                <td class="ex-num" title=format!("{delta} zatoshis")>{unit.number(delta)}</td>
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
                        <th scope="col" class="ex-bar-cell"><span class="visually-hidden">"Bars"</span></th>
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
                            <td class="ex-num">{unit.number(fees as i64)}</td>
                        </tr>
                    })}
                    {burned.map(|burned| view! {
                        <tr>
                            <th scope="row" title="NU7 removes ⌊6 F / 10⌋ of a block's fees from circulation">"Burned"</th>
                            <td></td><td></td><td></td>
                            <td class="ex-num">{unit.number(burned as i64)}</td>
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

/// The turnstile's scale, in zatoshis: its longest bar either side of the axis. Into a pool
/// the coinbase's bar comes first and the users' adds to it; out of a pool the users' bar
/// stands alone. The lockbox row draws its change in place of the users' flow.
fn turnstile_scale(deltas: &PerPool<i64>, coinbase: &PerPool<u64>, users: &PerPool<i64>) -> u64 {
    Pool::ALL
        .into_iter()
        .map(|pool| {
            let moved = if pool == Pool::Lockbox {
                deltas.get(pool)
            } else {
                users.get(pool)
            };
            let into = coinbase
                .get(pool)
                .saturating_add(u64::try_from(moved).unwrap_or(0));
            let out = if moved < 0 { moved.unsigned_abs() } else { 0 };
            into.max(out)
        })
        .max()
        .unwrap_or(0)
}

/// The reward split (V3): one bar of the miner's part, the streams, the fees the miner kept and
/// the burned fees, with the amounts and payout addresses.
#[component]
fn RewardBar(reward: RewardSplit, unit: Unit) -> impl IntoView {
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
            .map(|(name, zat, _, _)| format!("{name} {}", unit.number(*zat as i64)))
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

/// What the table has read: its rows, the chunks whose fees are on their way, how the block
/// is read, Show RPC's reads, and the chunks whose last read failed. `Copy`.
#[derive(Clone, Copy)]
struct Rows {
    rows: RwSignal<HashMap<u32, TxRow>>,
    pending: RwSignal<HashSet<u32>>,
    read: RwSignal<Option<RowsRead>>,
    sources: RwSignal<Option<Vec<SourceCall>>>,
    failed: RwSignal<BTreeMap<u32, ExplorerError>>,
}

/// Reads chunk `chunk` of block `hash`'s rows with `call` (the node's explorer), then again
/// with their fees when the first answer came without them. A chunk that failed is forgotten
/// from `asked`, so scrolling asks again, and its fees are no longer on their way; its failure
/// shows until it is read. An answer that comes once the table has gone (its page was left
/// meanwhile) is dropped, and its fees are not asked for.
async fn read_rows(
    call: impl AsyncFn(ExplorerCall) -> Result<ExplorerReply, ExplorerError>,
    hash: BlockHash,
    chunk: u32,
    trace: bool,
    store: Rows,
    asked: StoredValue<HashSet<u32>>,
) {
    for with_fees in [false, true] {
        let request = ExplorerRequest::BlockTxs {
            hash,
            offset: chunk * CHUNK,
            limit: CHUNK as u16,
            with_fees,
        };
        match call(ExplorerCall { request, trace }).await {
            Ok(reply) => {
                let ExplorerResponse::BlockTxs(page) = reply.response else {
                    return;
                };
                let kept = store.rows.try_update(|rows| {
                    for row in page.rows {
                        rows.insert(row.index, row);
                    }
                });
                if kept.is_none() {
                    return;
                }
                store.read.try_set(Some(page.read));
                if let Some(calls) = reply.sources {
                    store.sources.try_update(|all| {
                        let all = all.get_or_insert_with(Vec::new);
                        all.extend(calls);
                        let over = all.len().saturating_sub(TABLE_SOURCES);
                        all.drain(..over);
                    });
                }
                store.pending.try_update(|pending| {
                    if page.fees_pending {
                        pending.insert(chunk);
                    } else {
                        pending.remove(&chunk);
                    }
                });
                if store
                    .failed
                    .try_with_untracked(|failed| failed.contains_key(&chunk))
                    == Some(true)
                {
                    store.failed.try_update(|failed| {
                        failed.remove(&chunk);
                    });
                }
                if !page.fees_pending {
                    return;
                }
            }
            Err(error) => {
                store.failed.try_update(|failed| {
                    failed.insert(chunk, error);
                });
                store.pending.try_update(|pending| {
                    pending.remove(&chunk);
                });
                asked.try_update_value(|asked| {
                    asked.remove(&chunk);
                });
                return;
            }
        }
    }
}

/// The block's transactions on a virtual list: the rows in view are read as they come into
/// view, a chunk at a time. A small block comes whole in the first answer; a big one row by
/// row, with the fees after the rows and the next rows read ahead. Until a row is read, its id
/// (for the first 200, from the block) holds its place.
#[component]
fn TxTable(page: Page, nav: NodeExplorer, node: NodeId, network: Network) -> impl IntoView {
    let prefs = Prefs::get();
    let hash = page.summary.hash;
    let total = page.summary.tx_count;
    let first = StoredValue::new(page.txids.txids.clone());
    let store = Rows {
        rows: RwSignal::new(HashMap::new()),
        pending: RwSignal::new(HashSet::new()),
        read: RwSignal::new(None),
        sources: RwSignal::new(None),
        failed: RwSignal::new(BTreeMap::new()),
    };
    let asked = StoredValue::new(HashSet::<u32>::new());
    let read_ahead = StoredValue::new(HashSet::<u32>::new());
    let reader = node.clone();
    let on_window = Callback::new(move |window: Window| {
        let (start, end) = window.rows;
        if end <= start {
            return;
        }
        let trace = prefs.rpc.get_untracked();
        let chunks = |(start, end): (u32, u32)| start / CHUNK..=(end.max(start + 1) - 1) / CHUNK;
        let seen = chunks(window.seen);
        // A block read whole answers every chunk from the explorer's cache. A big one's rows
        // in view are asked for, and the overscan and the next chunk read ahead, so the
        // explorer reads the rows in view first.
        let whole = store.read.get_untracked() == Some(RowsRead::Block);
        let (mut asking, mut ahead) = (Vec::new(), Vec::new());
        for chunk in chunks(window.rows) {
            if asked.with_value(|asked| asked.contains(&chunk)) {
                continue;
            }
            if whole || seen.contains(&chunk) {
                asking.push(chunk);
            } else {
                ahead.push(chunk);
            }
        }
        // The explorer reads the newest request first: the chunk with the first row in view
        // is asked for last.
        for chunk in asking.into_iter().rev() {
            asked.update_value(|asked| {
                asked.insert(chunk);
            });
            let node = reader.clone();
            let call = async move |call| api::explorer_call(&node, call).await;
            spawn_local(read_rows(call, hash, chunk, trace, store, asked));
        }
        let next = (end - 1) / CHUNK + 1;
        if next * CHUNK < total && !asked.with_value(|asked| asked.contains(&next)) {
            ahead.push(next);
        }
        ahead.retain(|chunk| read_ahead.try_update_value(|read| read.insert(*chunk)) == Some(true));
        if !whole && !ahead.is_empty() {
            let node = reader.clone();
            spawn_local(async move {
                let targets = ahead
                    .into_iter()
                    .map(|chunk| PrefetchTarget::BlockTxs {
                        hash,
                        offset: chunk * CHUNK,
                        limit: CHUNK as u16,
                    })
                    .collect();
                let _ = api::explorer(&node, ExplorerRequest::Prefetch { targets }).await;
            });
        }
    });
    // The row the grid's keys made active: its hover card under its link, and Enter opens it.
    let txid_at = move |index: u32| {
        store
            .rows
            .with_untracked(|rows| rows.get(&index).map(|row| row.txid))
            .or_else(|| first.with_value(|first| first.get(index as usize).copied()))
    };
    let cards = HoverCards::get();
    let on_active = Callback::new(move |active: Option<(u32, web_sys::Element)>| {
        let Some(cards) = cards else {
            return;
        };
        match active {
            Some((index, row)) => {
                let link = row.query_selector(".ex-link").ok().flatten().unwrap_or(row);
                if let Some(txid) = txid_at(index) {
                    cards.enter(CardTarget::Tx { txid }, link);
                }
            }
            None => cards.leave_soon(),
        }
    });
    let on_open = Callback::new(move |index: u32| {
        if let Some(txid) = txid_at(index) {
            nav.go(ExplorerRoute::Tx { txid, out: None });
        }
    });
    let unit = move || prefs.unit(network);
    let header = move || {
        let unit = unit();
        view! {
            <span role="columnheader" class="ex-num">"#"</span>
            <span role="columnheader">"Transaction"</span>
            <span role="columnheader">"Kind"</span>
            <span role="columnheader" title="Where value came from and went: t transparent, z shielded, cb a coinbase's new coins">"Flow"</span>
            <span role="columnheader">"Pools"</span>
            <span role="columnheader" title="What entered (+) or left (−) each shielded pool">{format!("Net ({unit})")}</span>
            <span role="columnheader" class="ex-num">{format!("Fee ({unit})")}</span>
            <span role="columnheader" class="ex-num" title="Zatoshis per logical action due">"Per action"</span>
            <span role="columnheader" class="ex-num" title="The fee over zebrad's conventional fee">"×"</span>
            <span role="columnheader" class="ex-num" title="ZIP 317's logical actions">"Actions"</span>
            <span role="columnheader" class="ex-num">"Size"</span>
        }
    };
    let row = move |index: Memo<u32>| {
        let row = Memo::new(move |_| store.rows.with(|rows| rows.get(&index.get()).cloned()));
        let txid = move || {
            row.with(|row| row.as_ref().map(|row| row.txid))
                .or_else(|| first.with_value(|first| first.get(index.get() as usize).copied()))
        };
        let pending = move || {
            store
                .pending
                .with(|pending| pending.contains(&(index.get() / CHUNK)))
        };
        let (enter, leave) = hover_handlers(move || txid().map(|txid| CardTarget::Tx { txid }));
        let fee = move || {
            let unit = unit();
            row.with(|row| match row.as_ref().map(|row| row.fee) {
                None => String::new(),
                Some(None) => "—".to_owned(),
                Some(Some(Shown::Public { zat })) => unit.number(zat),
                Some(Some(_)) if pending() => "…".to_owned(),
                Some(Some(_)) => "unknown".to_owned(),
            })
        };
        let figures = move || {
            row.with(|row| {
                row.as_ref()
                    .and_then(|row| text::fee_figures(row.fee, &row.actions))
            })
        };
        let net = move || {
            let unit = unit();
            row.with(|row| {
                row.as_ref()
                    .map(|row| text::net_text(&row.shielded, unit))
                    .unwrap_or_default()
            })
        };
        view! {
            <span role="gridcell" class="ex-num muted">{move || index.get()}</span>
            <span role="gridcell">
                // No Tab stop of its own: the grid is one, and its keys open the active row.
                <button
                    type="button"
                    class="link ex-link"
                    tabindex="-1"
                    aria-describedby=CARD_ID
                    on:click=move |_| {
                        if let Some(txid) = txid() {
                            nav.go(ExplorerRoute::Tx { txid, out: None });
                        }
                    }
                    on:mouseenter=move |event| enter(event.into())
                    on:mouseleave=move |event| leave(event.into())
                >
                    <code>{move || txid().map_or_else(|| "…".to_owned(), |txid| short_hash(&txid.to_hex()))}</code>
                </button>
            </span>
            <span role="gridcell">
                {move || row.with(|row| row.as_ref().map(|row| {
                    let shielded = !matches!(row.kind, launcher_types::explorer::TxKind::Transparent | launcher_types::explorer::TxKind::Coinbase { .. });
                    view! { <span class="ex-kind" class:ex-kind-shielded=shielded>{text::kind_label(&row.kind)}</span> }
                }))}
            </span>
            <span role="gridcell" class="ex-glyph">{move || row.with(|row| row.as_ref().map(text::flow_glyph))}</span>
            <span role="gridcell" class="ex-pools-cell">
                {move || row.with(|row| row.as_ref().map(|row| {
                    row.shielded
                        .iter()
                        .map(|part| view! { <PoolChip pool=part.pool /> })
                        .collect_view()
                }))}
            </span>
            // In full on hover too, should it not fit.
            <span role="gridcell" class="ex-net-cell" title=net>{net}</span>
            <span role="gridcell" class="ex-num">{fee}</span>
            <span role="gridcell" class="ex-num">
                {move || figures().map(|figures| format!("{} zat", thousands(figures.per_action)))}
            </span>
            <span role="gridcell" class="ex-num">{move || figures().map(|figures| text::multiple_text(figures.multiple))}</span>
            <span role="gridcell" class="ex-num">
                {move || row.with(|row| row.as_ref().map(|row| explorer_model::zip317::logical_actions(&row.actions)))}
            </span>
            <span role="gridcell" class="ex-num">
                {move || row.with(|row| row.as_ref().and_then(|row| row.size).map(|size| bytes(u64::from(size))))}
            </span>
        }
    };
    // zebrad does not report the size of a block it has no value-pool row for, and such a
    // block is read row by row in case it is a big one.
    let sized = page.summary.size.is_some();
    let reading = move || {
        let fees = !store.pending.with(HashSet::is_empty);
        match store.read.get() {
            Some(RowsRead::OneByOne) => Some(format!(
                "{}: {} transactions, read from your node as their rows come into view{}.",
                if sized {
                    "A big block"
                } else {
                    "A block of a size zebrad does not report"
                },
                thousands(u64::from(total)),
                if fees {
                    "; reading the fees of the rows in view"
                } else {
                    ""
                }
            )),
            _ => None,
        }
    };
    view! {
        <section class="ex-section" aria-labelledby="ex-txs-title">
            <h2 id="ex-txs-title">
                {format!("Transactions ({})", thousands(u64::from(total)))}
            </h2>
            {move || reading().map(|reading| view! { <p class="muted ex-caption" role="status">{reading}</p> })}
            // The first chunk whose last read failed, until it is read.
            {move || {
                store
                    .failed
                    .with(|failed| failed.values().next().cloned())
                    .map(|error| view! { <Failure error /> })
            }}
            <VirtualList
                total
                rows_shown=ROWS_SHOWN
                id="ex-txs-row"
                label=format!("The block's {} transactions", thousands(u64::from(total)))
                header
                row
                on_window
                on_active
                on_open
            />
            <p class="muted ex-caption">
                "Fees come from the outputs each transaction spends. Per action and × are ZIP 317's, \
                 as zebrad counts them: 1,000 zat per logical action, at least two."
            </p>
            <RpcCalls sources=store.sources title="What the table asked your node" folded=true />
        </section>
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

#[cfg(test)]
mod tests {
    use std::{
        cell::Cell,
        pin::pin,
        task::{Context, Waker},
    };

    use launcher_types::explorer::{Hash32, PerPool, TxRowPage};

    use super::*;

    #[test]
    fn rows_answered_after_the_table_has_gone_are_dropped() {
        for fees_pending in [false, true] {
            let root = Owner::new();
            root.set();
            let table = root.child();
            let (store, asked) = table.with(|| {
                let store = Rows {
                    rows: RwSignal::new(HashMap::new()),
                    pending: RwSignal::new(HashSet::new()),
                    read: RwSignal::new(None),
                    sources: RwSignal::new(None),
                    failed: RwSignal::new(BTreeMap::new()),
                };
                (store, StoredValue::new(HashSet::from([0])))
            });
            let calls = Cell::new(0);
            // The page is left while the node reads; the answer comes all the same.
            let call = async |_: ExplorerCall| {
                calls.set(calls.get() + 1);
                table.cleanup();
                Ok(ExplorerReply {
                    chain: None,
                    response: ExplorerResponse::BlockTxs(TxRowPage {
                        offset: 0,
                        total: 4_600,
                        rows: Vec::new(),
                        read: RowsRead::OneByOne,
                        fees_pending,
                    }),
                    sources: None,
                })
            };
            let mut reading = pin!(read_rows(call, Hash32([7; 32]), 0, false, store, asked));
            let ready = reading
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()));
            assert!(ready.is_ready(), "{fees_pending}");
            assert_eq!(calls.get(), 1, "no fees asked for a table that has gone");
        }
    }

    #[test]
    fn the_turnstile_scale_holds_every_bar() {
        // Orchard: the coinbase pays 10 in and users take 15 out; Sapling: 3 in from the
        // coinbase and 4 more from users; the lockbox gains 2.
        let coinbase = PerPool([0, 0, 3, 10, 0, 0]);
        let users = PerPool([0, 0, 4, -15, 0, 0]);
        let deltas = PerPool([0, 0, 7, -5, 2, 0]);
        assert_eq!(turnstile_scale(&deltas, &coinbase, &users), 15);
        let users = PerPool([0, 0, 4, -1, 0, 0]);
        let deltas = PerPool([0, 0, 7, 9, 2, 0]);
        assert_eq!(turnstile_scale(&deltas, &coinbase, &users), 10);
        let users = PerPool([0, 0, 9, -1, 0, 0]);
        assert_eq!(turnstile_scale(&deltas, &coinbase, &users), 12);
        let quiet = PerPool([0; 6]);
        assert_eq!(turnstile_scale(&quiet, &PerPool([0; 6]), &quiet), 0);
    }
}
