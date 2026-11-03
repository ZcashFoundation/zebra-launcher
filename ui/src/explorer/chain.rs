//! The Chain page, the Explorer's home: the block strip of the newest twelve blocks, tiles for
//! the tip, the mempool, the supply, the next upgrade and the difficulty, and the 25 newest
//! blocks. It follows the tip: a new tip asks again. Ages read the app's clock themselves, so
//! the one-second tick never rebuilds the lists.

use std::time::{Duration, UNIX_EPOCH};

use explorer_model::ledger::nsm_seed;
use launcher_types::{
    Network, NodeId,
    explorer::{
        BlockId, BlockSummary, ChainPage as Page, ExplorerRequest, ExplorerResponse, ExplorerRoute,
        Pool,
    },
};
use leptos::{ev, prelude::*};
use wasm_bindgen::JsCast;

use super::{
    parts::{Failure, Loading, RouteLink},
    state::{Loaded, NodeExplorer, load},
    text,
};
use crate::{
    app::AppState,
    format::{bytes, relative, thousands},
};

/// Blocks on the strip.
const STRIP: usize = 12;
/// An interval bar is as tall as its seconds, up to this many target spacings.
const INTERVAL_CAP: f64 = 4.0;

#[component]
pub fn ChainPage(node: NodeId, nav: NodeExplorer, network: Network) -> impl IntoView {
    let unit = text::unit(network);
    // A new tip, or a new epoch after a reorganisation, asks again.
    let answer = load(node, nav, move || {
        let revision = nav.live.with(|live| {
            live.as_ref()
                .and_then(|live| live.tip)
                .map_or(0, |tip| (tip.epoch << 32) | u64::from(tip.height))
        });
        Some((ExplorerRequest::Chain, revision))
    });
    view! {
        <div class="ex-chain" class:ex-refreshing=move || answer.refreshing.get()>
            {move || match answer.value.get() {
                Loaded::Loading => view! { <Loading /> }.into_any(),
                Loaded::Failed(error) => view! { <Failure error /> }.into_any(),
                Loaded::Ready(reply) => match reply.response {
                    ExplorerResponse::Chain(page) => view! { <Chain page nav unit network /> }.into_any(),
                    _ => view! { <Loading /> }.into_any(),
                },
            }}
        </div>
    }
}

#[component]
fn Chain(page: Page, nav: NodeExplorer, unit: &'static str, network: Network) -> impl IntoView {
    let strip: Vec<BlockSummary> = page.blocks.iter().take(STRIP).rev().cloned().collect();
    let target = page.target_spacing;
    view! {
        <section class="ex-section" aria-labelledby="ex-strip-title">
            <h2 id="ex-strip-title" class="visually-hidden">"The newest blocks"</h2>
            <Strip blocks=strip nav unit target />
        </section>
        <Tiles page=page.clone() unit network />
        <section class="ex-section" aria-labelledby="ex-recent-title">
            <h2 id="ex-recent-title">"Recent blocks"</h2>
            <RecentBlocks blocks=page.blocks nav unit />
        </section>
    }
}

/// The block strip (V1): older blocks to the left, the newest on the right, each a link; under
/// them, a bar per gap as tall as its seconds against the target. ← and → move between tiles.
#[component]
fn Strip(
    blocks: Vec<BlockSummary>,
    nav: NodeExplorer,
    unit: &'static str,
    target: u32,
) -> impl IntoView {
    let app = AppState::get();
    let count = blocks.len();
    let keydown = move |event: ev::KeyboardEvent| {
        let step: i32 = match event.key().as_str() {
            "ArrowRight" => 1,
            "ArrowLeft" => -1,
            "Home" => i32::MIN,
            "End" => i32::MAX,
            _ => return,
        };
        let Some(target) = event
            .target()
            .and_then(|target| target.dyn_into::<web_sys::HtmlElement>().ok())
        else {
            return;
        };
        let Some(list) = target.closest(".ex-strip").ok().flatten() else {
            return;
        };
        let tiles = list.query_selector_all(".ex-tile-link").ok();
        let Some(tiles) = tiles else { return };
        let len = tiles.length() as i32;
        let current = (0..len)
            .find(|index| {
                tiles
                    .item(*index as u32)
                    .is_some_and(|tile| tile.is_same_node(Some(&target)))
            })
            .unwrap_or(0);
        let next = match step {
            i32::MIN => 0,
            i32::MAX => len - 1,
            step => (current + step).clamp(0, len - 1),
        };
        event.prevent_default();
        if let Some(tile) = tiles
            .item(next as u32)
            .and_then(|tile| tile.dyn_into::<web_sys::HtmlElement>().ok())
        {
            let _ = tile.focus();
        }
    };
    let tallest = f64::from(target) * INTERVAL_CAP;
    view! {
        <ol class="ex-strip" aria-label="The newest blocks, oldest first" on:keydown=keydown>
            {blocks
                .into_iter()
                .enumerate()
                .map(|(index, block)| {
                    let height = block.height;
                    let route = ExplorerRoute::Block { id: BlockId::Height(height) };
                    let time = UNIX_EPOCH + Duration::from_secs(block.time);
                    let notes = block.ledger.notes.unwrap_or([0; 3]);
                    let total = notes.iter().sum::<u32>().max(1);
                    let fees = block.ledger.fees.map(|fees| text::amount(fees as i64, unit));
                    let burned = block.ledger.burned.map(|_| "60 % burned".to_owned());
                    let budget = text::budget_text(&block.ledger.budget);
                    let interval = block.interval;
                    let bar = interval.map(|secs| (secs.max(0) as f64).min(tallest) / tallest * 100.0);
                    let newest = index + 1 == count;
                    view! {
                        <li class="ex-tile" class:ex-tile-newest=newest>
                            <button
                                type="button"
                                class="ex-tile-link"
                                tabindex=if newest { "0" } else { "-1" }
                                on:click=move |_| nav.go(route.clone())
                            >
                                <span class="ex-tile-height">{thousands(u64::from(height))}</span>
                                <span class="ex-tile-age muted">{move || relative(time, app.now.get())}</span>
                                <span class="ex-tile-txs">
                                    {format!("{} tx", thousands(u64::from(block.tx_count)))}
                                </span>
                                <span class="ex-notes" role="img" aria-label=format!(
                                    "Notes created: {} Sapling, {} Orchard, {} Ironwood",
                                    notes[0], notes[1], notes[2]
                                )>
                                    {[Pool::Sapling, Pool::Orchard, Pool::Ironwood]
                                        .into_iter()
                                        .zip(notes)
                                        .map(|(pool, count)| view! {
                                            <span
                                                class=format!("ex-notes-part ex-fill-{}", pool.id())
                                                style:width=format!("{:.1}%", f64::from(count) / f64::from(total) * 100.0)
                                            ></span>
                                        })
                                        .collect_view()}
                                </span>
                                <span class="ex-tile-fees">{fees.unwrap_or_else(|| "fees unknown".into())}</span>
                                {burned.map(|burned| view! { <span class="ex-tile-burn muted">{burned}</span> })}
                                <span class="ex-tile-budget muted">{budget}</span>
                            </button>
                            <span
                                class="ex-gap"
                                title=interval.map(text::interval_text)
                                aria-hidden="true"
                            >
                                <span class="ex-gap-target" style:bottom=format!("{:.1}%", 100.0 / INTERVAL_CAP)></span>
                                {bar.map(|bar| view! { <span class="ex-gap-bar" style:height=format!("{bar:.1}%")></span> })}
                            </span>
                        </li>
                    }
                })
                .collect_view()}
        </ol>
        <p class="muted ex-caption">
            {format!(
                "Bars under the blocks: the seconds since the block before, against the {target} s target. \
                 Notes: commitments created in Sapling, Orchard and Ironwood."
            )}
        </p>
    }
}

/// The tiles under the strip: the tip, the mempool, the supply, the next upgrade and the
/// difficulty.
#[component]
fn Tiles(page: Page, unit: &'static str, network: Network) -> impl IntoView {
    let app = AppState::get();
    let tip = page.blocks.first().cloned();
    let interval = page.average_interval.map(|average| {
        format!(
            "{:.0} s between the last 100 blocks · target {} s",
            average, page.target_spacing
        )
    });
    let mempool = move || {
        app.node().stats.with(|stats| {
            stats
                .as_ref()
                .map(|stats| thousands(stats.mempool_transactions))
        })
    };
    let supply = page.supply.map(|supply| text::amount_rounded(supply, unit));
    let burned = page.burned_since_nu7;
    let reserve = burned.map(|burned| nsm_seed(network) + burned);
    let upgrade = page.next_upgrade.clone();
    view! {
        <div class="ex-tiles">
            <div class="tile">
                <span class="tile-label">"Tip"</span>
                <span class="tile-value">
                    {tip.as_ref().map(|tip| thousands(u64::from(tip.height)))}
                </span>
                {tip.map(|tip| {
                    let time = UNIX_EPOCH + Duration::from_secs(tip.time);
                    view! { <span class="tile-detail">{move || format!("mined {}", relative(time, app.now.get()))}</span> }
                })}
                {interval.map(|interval| view! { <span class="tile-detail">{interval}</span> })}
            </div>
            <div class="tile">
                <span class="tile-label">"Mempool"</span>
                <span class="tile-value">{move || mempool().unwrap_or_else(|| "unknown".into())}</span>
                <span class="tile-detail">"transactions waiting in your node"</span>
            </div>
            <div class="tile">
                <span class="tile-label">"Supply"</span>
                <span class="tile-value">{supply.unwrap_or_else(|| "unknown".into())}</span>
                {burned.map(|burned| view! {
                    <span class="tile-detail">{format!("{} burned since NU7", text::amount(burned as i64, unit))}</span>
                })}
                {reserve.map(|reserve| view! {
                    <span class="tile-detail" title="The NU7 seed plus the burned fees; zebrad tracks it but does not report it">
                        {format!("NSM reserve {} (computed by Zeal)", text::amount(reserve as i64, unit))}
                    </span>
                })}
            </div>
            <div class="tile">
                <span class="tile-label">"Next upgrade"</span>
                {match upgrade {
                    Some(upgrade) => view! {
                        <span class="tile-value">{upgrade.name.clone()}</span>
                        <span class="tile-detail">
                            {format!(
                                "at {} · {} blocks · {}",
                                thousands(u64::from(upgrade.height)),
                                thousands(u64::from(upgrade.blocks_left)),
                                text::eta_text(upgrade.eta_secs)
                            )}
                        </span>
                    }
                    .into_any(),
                    None => view! {
                        <span class="tile-value tile-muted">"None scheduled"</span>
                        <span class="tile-detail">"every upgrade your node knows is active"</span>
                    }
                    .into_any(),
                }}
            </div>
            <div class="tile">
                <span class="tile-label">"Difficulty"</span>
                <span class="tile-value">
                    {page.difficulty.map(text::difficulty)}
                </span>
                <span class="tile-detail">"at the tip"</span>
            </div>
        </div>
    }
}

/// The 25 newest blocks as a table.
#[component]
fn RecentBlocks(blocks: Vec<BlockSummary>, nav: NodeExplorer, unit: &'static str) -> impl IntoView {
    let app = AppState::get();
    view! {
        <div class="ex-table-wrap">
            <table class="ex-table">
                <thead>
                    <tr>
                        <th scope="col">"Height"</th>
                        <th scope="col">"Age"</th>
                        <th scope="col" class="ex-num">"Interval"</th>
                        <th scope="col" class="ex-num">"Txs"</th>
                        <th scope="col" class="ex-num">"Size"</th>
                        <th scope="col" class="ex-num" title="Note commitments created: Sapling, Orchard, Ironwood">"Notes S · O · I"</th>
                        <th scope="col" class="ex-num" title="What users moved into shielded pools, net">"Net shielding"</th>
                        <th scope="col" class="ex-num">"Fees"</th>
                        <th scope="col" class="ex-num">"Burned"</th>
                        <th scope="col" class="ex-num">"Budget"</th>
                    </tr>
                </thead>
                <tbody>
                    {blocks
                        .into_iter()
                        .map(|block| {
                            let route = ExplorerRoute::Block { id: BlockId::Height(block.height) };
                            let time = UNIX_EPOCH + Duration::from_secs(block.time);
                            let ledger = &block.ledger;
                            let notes = ledger
                                .notes
                                .map(|n| format!("{} · {} · {}", n[0], n[1], n[2]));
                            let shielding = ledger.users.as_ref().map(|users| {
                                let net = text::net_shielding(users);
                                let sign = if net > 0 { "+" } else { "" };
                                format!("{sign}{}", text::coins(net))
                            });
                            view! {
                                <tr>
                                    <th scope="row">
                                        <RouteLink nav route>{thousands(u64::from(block.height))}</RouteLink>
                                    </th>
                                    <td>{move || relative(time, app.now.get())}</td>
                                    <td class="ex-num">{block.interval.map(text::interval_text)}</td>
                                    <td class="ex-num">{thousands(u64::from(block.tx_count))}</td>
                                    <td class="ex-num">{block.size.map(|size| bytes(u64::from(size)))}</td>
                                    <td class="ex-num">{notes}</td>
                                    <td class="ex-num">{shielding.map(|text| format!("{text} {unit}"))}</td>
                                    <td class="ex-num">{ledger.fees.map(|fees| text::coins(fees as i64))}</td>
                                    <td class="ex-num">{ledger.burned.map(|burned| text::coins(burned as i64))}</td>
                                    <td class="ex-num">{text::budget_text(&ledger.budget)}</td>
                                </tr>
                            }
                        })
                        .collect_view()}
                </tbody>
            </table>
        </div>
    }
}
