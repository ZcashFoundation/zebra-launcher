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
use leptos::{ev, html, prelude::*};
use wasm_bindgen::JsCast;

use super::{
    parts::{Failure, Loading, RouteLink},
    state::{Loaded, NodeExplorer, load},
    text,
};
use crate::{
    app::{AppState, Tab},
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
/// Where the page is too narrow for all twelve, the strip opens scrolled to the newest.
#[component]
fn Strip(
    blocks: Vec<BlockSummary>,
    nav: NodeExplorer,
    unit: &'static str,
    target: u32,
) -> impl IntoView {
    let app = AppState::get();
    let count = blocks.len();
    // Once drawn, and whenever the tab is shown again (a hidden strip has no width to scroll),
    // the strip shows its newest end; older tiles are a scroll to the left.
    let list = NodeRef::<html::Ol>::new();
    Effect::new(move |_| {
        if app.tab.get() != Tab::Explorer {
            return;
        }
        request_animation_frame(move || {
            if let Some(list) = list.get_untracked() {
                list.set_scroll_left(list.scroll_width());
            }
        });
    });
    // A roving tabindex: Tab reaches one tile, the newest until another takes focus; ←, →,
    // Home and End move focus, and the tabindex with it.
    let active = RwSignal::new(count.saturating_sub(1));
    let focus_tile = move |index: usize| {
        let tile = list
            .get_untracked()
            .and_then(|list| list.query_selector_all(".ex-tile-link").ok())
            .and_then(|tiles| tiles.item(u32::try_from(index).ok()?))
            .and_then(|tile| tile.dyn_into::<web_sys::HtmlElement>().ok());
        if let Some(tile) = tile {
            let _ = tile.focus();
        }
    };
    let tallest = f64::from(target) * INTERVAL_CAP;
    view! {
        <ol class="ex-strip" aria-label="The newest blocks, oldest first" node_ref=list>
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
                    let keydown = move |event: ev::KeyboardEvent| {
                        if let Some(next) = strip_step(index, count, &event.key()) {
                            event.prevent_default();
                            active.set(next);
                            focus_tile(next);
                        }
                    };
                    view! {
                        <li class="ex-tile" class:ex-tile-newest=newest>
                            <button
                                type="button"
                                class="ex-tile-link"
                                tabindex=move || if active.get() == index { "0" } else { "-1" }
                                on:focus=move |_| active.set(index)
                                on:keydown=keydown
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

/// Where a key moves focus in a strip of `count` tiles from tile `at`: ← and → one tile, Home
/// and End to either end; `None` for any other key, or a strip without tiles.
fn strip_step(at: usize, count: usize, key: &str) -> Option<usize> {
    let last = count.checked_sub(1)?;
    match key {
        "ArrowRight" => Some((at + 1).min(last)),
        "ArrowLeft" => Some(at.saturating_sub(1).min(last)),
        "Home" => Some(0),
        "End" => Some(last),
        _ => None,
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

#[cfg(test)]
mod tests {
    use super::strip_step;

    #[test]
    fn the_strip_keys_move_within_its_tiles() {
        assert_eq!(strip_step(11, 12, "ArrowLeft"), Some(10));
        assert_eq!(
            strip_step(11, 12, "ArrowRight"),
            Some(11),
            "the newest is the last"
        );
        assert_eq!(strip_step(0, 12, "ArrowLeft"), Some(0));
        assert_eq!(strip_step(4, 12, "Home"), Some(0));
        assert_eq!(strip_step(4, 12, "End"), Some(11));
        assert_eq!(strip_step(4, 12, "Enter"), None, "Enter follows the link");
        assert_eq!(strip_step(0, 0, "End"), None);
    }
}
