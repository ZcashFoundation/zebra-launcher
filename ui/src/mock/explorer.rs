//! The mock's block explorers: one per zebrad or Zakura node, over a synthetic chain
//! (explorer-model's `synth`) that follows the node's height, and answered with the backend's
//! own builders (explorer-model's `views`), so kinds, fees, ledgers and sentences follow its
//! rules. Nothing is fixture data: every block is computed when asked for.
//!
//! `?explorer=slow` makes each answer take 300 to 800 ms instead of 20 to 80, `?explorer=nobi`
//! takes zebrad's value-pool row from every seventh block (so the next one shows the zero-diff
//! case), and `?reorg` replaces the main node's top two blocks about every 90 s. `?attip`,
//! `?tip=` and `?explorer=empty` choose the main node's height (mock.rs).

use std::{
    cell::{Cell, RefCell},
    time::{Duration, UNIX_EPOCH},
};

use explorer_model::{
    classify::{Classified, classify_query},
    shapes::BlockV1,
    synth::SynthChain,
    upgrades::{Upgrade, UpgradeHeights},
    views, zec_to_zat,
};
use launcher_types::{
    Network, NetworkUpgrade, NodeId, NodeKind, NodeStats, Phase,
    explorer::{
        BlockHash, BlockId, ChainPage, ChainView, ExplorerCall, ExplorerError, ExplorerErrorKind,
        ExplorerLive, ExplorerLiveEvent, ExplorerReply, ExplorerRequest, ExplorerResponse,
        ExplorerRoute, LiveState, NotFoundHint, ReorgNotice, Resolution,
    },
};
use leptos::prelude::*;

use super::{ExplorerFlag, mock, sleep};
use crate::clock;

/// How often `?reorg` replaces the main node's top blocks.
const REORG_EVERY: Duration = Duration::from_secs(90);
/// The rows of the Chain page, as in the backend.
const RECENT_BLOCKS: u32 = 25;

/// One node's explorer.
struct Explorer {
    node: NodeId,
    chain: SynthChain,
    epoch: u64,
    reorg: Option<ReorgNotice>,
    /// The tip as the backend would know it: read by every request, and followed by itself
    /// only while the node is at the network's tip.
    known: Option<ChainView>,
    /// The last event sent, so only changes go out.
    sent: Option<ExplorerLive>,
}

/// Who hears the `explorer_live` events.
type Listener = Box<dyn Fn(ExplorerLiveEvent)>;

thread_local! {
    static EXPLORERS: RefCell<Vec<Explorer>> = const { RefCell::new(Vec::new()) };
    static LISTENERS: RefCell<Vec<Listener>> = const { RefCell::new(Vec::new()) };
    static TICKING: Cell<bool> = const { Cell::new(false) };
    static LAST_REORG: Cell<u64> = const { Cell::new(0) };
}

/// What the explorer reads of a node.
struct NodeView {
    id: NodeId,
    network: Network,
    running: bool,
    stats: Option<NodeStats>,
}

impl NodeView {
    /// The node's height and zebrad's estimate of the network's.
    fn heights(&self) -> Option<(u32, u32)> {
        let stats = self.stats.as_ref()?;
        let height = u32::try_from(stats.height).ok()?;
        let estimated = u32::try_from(stats.estimated_height).ok()?.max(height);
        Some((height, estimated))
    }

    fn at_tip(&self) -> bool {
        self.stats.as_ref().is_some_and(|stats| {
            launcher_types::at_tip(
                stats.height,
                stats.estimated_height,
                stats.verification_progress,
            )
        })
    }
}

fn network_of(chain: &str) -> Option<Network> {
    match chain {
        "main" => Some(Network::Mainnet),
        "test" => Some(Network::Testnet),
        _ => None,
    }
}

fn now_secs() -> i64 {
    clock::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64)
}

/// The main node as its explorer sees it: under the id of the network zebrad reports.
fn primary_view() -> NodeView {
    let mock = mock();
    let status = mock.status.get_untracked();
    let stats = mock.stats.get_untracked();
    let network = stats
        .as_ref()
        .and_then(|stats| network_of(&stats.chain))
        .unwrap_or_else(|| mock.settings.with_untracked(|settings| settings.network));
    NodeView {
        id: NodeId::suggest(NodeKind::Zebrad, network, &Vec::new()),
        network,
        running: status.phase == Phase::Running && status.rpc_addr.is_some(),
        stats,
    }
}

/// The node `node` names, if it has an explorer.
fn view_of(node: &NodeId) -> Result<NodeView, ExplorerError> {
    let primary = primary_view();
    if *node == primary.id {
        return Ok(primary);
    }
    if let Some((info, running, stats)) = super::herd::explorer_view(node) {
        if !info.kind.has_explorer() {
            return Err(unknown_node());
        }
        return Ok(NodeView {
            id: info.id,
            network: info.network,
            running,
            stats,
        });
    }
    let other = match primary.network {
        Network::Mainnet => Network::Testnet,
        Network::Testnet => Network::Mainnet,
    };
    if *node == NodeId::suggest(NodeKind::Zebrad, other, &Vec::new()) {
        return Err(ExplorerError::new(
            ExplorerErrorKind::NodeNotRunning,
            format!(
                "zebrad runs on {}; it changes network at its next start",
                primary.network
            ),
        ));
    }
    Err(unknown_node())
}

fn unknown_node() -> ExplorerError {
    ExplorerError::new(ExplorerErrorKind::UnknownNode, "this node has no explorer")
}

fn not_running() -> ExplorerError {
    ExplorerError::new(
        ExplorerErrorKind::NodeNotRunning,
        "zebrad is not running, or has not opened its RPC port yet",
    )
}

fn not_found(message: &str) -> ExplorerError {
    ExplorerError::new(ExplorerErrorKind::NotFound, message)
}

impl Explorer {
    fn new(view: &NodeView, height: u32, estimated: u32) -> Self {
        // The node's newest block is as old as the blocks it has yet to catch up on.
        let heights = UpgradeHeights::defaults(view.network);
        let tip_time = now_secs() - heights.spacing_between(height, estimated) as i64;
        let mut chain = SynthChain::new(view.network, height, tip_time);
        if mock().flags.explorer == ExplorerFlag::NoBlockInfo {
            chain = chain.with_missing_rows(7);
        }
        Explorer {
            node: view.id.clone(),
            chain,
            epoch: 0,
            reorg: None,
            known: None,
            sent: None,
        }
    }

    /// Mines what the node gained since the last look.
    fn follow(&mut self, height: u32) {
        if height > self.chain.tip() {
            self.chain.mine(height - self.chain.tip());
        }
    }

    fn chain_view(&self, running: bool) -> ChainView {
        let tip = self.chain.tip();
        ChainView {
            height: tip,
            hash: self.chain.hash(tip).unwrap_or_default(),
            epoch: self.epoch,
            stale: !running,
        }
    }

    fn live(&self, view: &NodeView) -> ExplorerLive {
        let state = match (view.running, view.heights()) {
            (true, Some(_)) if view.at_tip() => LiveState::AtTip,
            (true, Some((height, estimated))) => LiveState::Syncing { height, estimated },
            _ => LiveState::NotRunning,
        };
        ExplorerLive {
            state,
            tip: self.known.map(|known| ChainView {
                stale: !view.running,
                ..known
            }),
            reorg: self.reorg,
        }
    }
}

/// Runs `answer` with the node's explorer, made on first use and brought up to its height.
fn with_explorer<T>(
    view: &NodeView,
    answer: impl FnOnce(&mut Explorer) -> T,
) -> Result<T, ExplorerError> {
    let (height, estimated) = view.heights().ok_or_else(not_running)?;
    EXPLORERS.with_borrow_mut(|explorers| {
        let index = match explorers
            .iter()
            .position(|explorer| explorer.node == view.id)
        {
            Some(index) => index,
            None => {
                explorers.push(Explorer::new(view, height, estimated));
                explorers.len() - 1
            }
        };
        let explorer = &mut explorers[index];
        explorer.follow(height);
        Ok(answer(explorer))
    })
}

/// The best block hash, the pools, the upgrades and the branch ids the main node reports at
/// its height, from the same chain its explorer shows, for the dashboard's details.
pub(super) struct ChainFacts {
    pub(super) best_block_hash: BlockHash,
    pub(super) pools: [u64; 6],
    pub(super) upgrades: Vec<NetworkUpgrade>,
    pub(super) chain_tip_branch: &'static str,
    pub(super) next_block_branch: &'static str,
    pub(super) difficulty: f64,
}

impl ChainFacts {
    pub(super) fn of(chain: &SynthChain, height: u32) -> Self {
        ChainFacts {
            best_block_hash: chain.hash(height).unwrap_or_default(),
            pools: chain.pools(height),
            upgrades: chain.upgrades(height),
            chain_tip_branch: chain.branch_id_at(height),
            next_block_branch: chain.branch_id_at(height + 1),
            difficulty: chain.difficulty(height),
        }
    }
}

/// The facts of the main node with the statistics `stats`, about to be published.
pub(super) fn primary_facts(stats: &NodeStats) -> Option<ChainFacts> {
    let network = network_of(&stats.chain)?;
    let view = NodeView {
        id: NodeId::suggest(NodeKind::Zebrad, network, &Vec::new()),
        network,
        running: true,
        stats: Some(stats.clone()),
    };
    let height = u32::try_from(stats.height).ok()?;
    with_explorer(&view, |explorer| ChainFacts::of(&explorer.chain, height)).ok()
}

/// Every second: the explorers follow their nodes, `?reorg` replaces the main node's top
/// blocks now and then, and changes go out as `explorer_live` events.
fn tick() {
    let primary = primary_view();
    let flags = mock().flags;
    if flags.reorg && primary.running && primary.at_tip() {
        let now = now_secs() as u64;
        let last = LAST_REORG.with(Cell::get);
        if last == 0 {
            LAST_REORG.with(|cell| cell.set(now));
        } else if now >= last + REORG_EVERY.as_secs() {
            LAST_REORG.with(|cell| cell.set(now));
            let _ = with_explorer(&primary, |explorer| {
                let fork = explorer.chain.reorg(2);
                explorer.epoch += 1;
                explorer.reorg = Some(ReorgNotice {
                    fork_height: fork,
                    first_replaced: fork + 1,
                    replaced: 2,
                    epoch: explorer.epoch,
                    at: now,
                });
            });
        }
    }
    // The main node's explorer exists as soon as the node reports a height.
    let _ = with_explorer(&primary, |_| ());
    let nodes: Vec<NodeId> =
        EXPLORERS.with_borrow(|explorers| explorers.iter().map(|e| e.node.clone()).collect());
    for node in nodes {
        let Ok(view) = view_of(&node) else {
            continue;
        };
        let Some(live) = changed_live(&view) else {
            continue;
        };
        let event = ExplorerLiveEvent { node, live };
        LISTENERS.with_borrow(|listeners| {
            for listener in listeners {
                listener(event.clone());
            }
        });
    }
}

/// The node's live state, when it differs from the last one sent. A node that stopped keeps
/// its last tip, marked stale.
fn changed_live(view: &NodeView) -> Option<ExplorerLive> {
    EXPLORERS.with_borrow_mut(|explorers| {
        let explorer = explorers
            .iter_mut()
            .find(|explorer| explorer.node == view.id)?;
        if let Some((height, _)) = view.heights() {
            explorer.follow(height);
            if view.running && view.at_tip() {
                explorer.known = Some(explorer.chain_view(true));
            }
        }
        let live = explorer.live(view);
        let changed = explorer.sent.as_ref() != Some(&live);
        explorer.sent = Some(live.clone());
        changed.then_some(live)
    })
}

fn ensure_ticking() {
    if !TICKING.with(|ticking| ticking.replace(true)) {
        set_interval(tick, Duration::from_secs(1));
    }
}

/// Calls `handler` with every `explorer_live` event from now on.
pub fn on_live(handler: impl Fn(ExplorerLiveEvent) + 'static) {
    LISTENERS.with_borrow_mut(|listeners| listeners.push(Box::new(handler)));
    ensure_ticking();
}

/// How long an answer takes.
fn delay() -> i32 {
    let random = js_sys::Math::random();
    if mock().flags.explorer == ExplorerFlag::Slow {
        300 + (random * 500.0) as i32
    } else {
        20 + (random * 60.0) as i32
    }
}

/// Answers one explorer call, as the backend would.
pub async fn handle(node: &NodeId, call: ExplorerCall) -> Result<ExplorerReply, ExplorerError> {
    sleep(delay()).await;
    ensure_ticking();
    let view = view_of(node)?;
    match call.request {
        ExplorerRequest::Status => {
            let live = with_explorer(&view, |explorer| explorer.live(&view)).unwrap_or_default();
            Ok(ExplorerReply {
                chain: live.tip,
                response: ExplorerResponse::Status(live),
            })
        }
        ExplorerRequest::Interest { .. } => Ok(ExplorerReply {
            chain: None,
            response: ExplorerResponse::Done,
        }),
        request => {
            if !view.running {
                return Err(not_running());
            }
            with_explorer(&view, |explorer| {
                explorer.known = Some(explorer.chain_view(true));
                let chain = explorer.known;
                answer(&explorer.chain, &view, request)
                    .map(|response| ExplorerReply { chain, response })
            })?
        }
    }
}

fn answer(
    chain: &SynthChain,
    view: &NodeView,
    request: ExplorerRequest,
) -> Result<ExplorerResponse, ExplorerError> {
    let heights = chain.heights();
    match request {
        ExplorerRequest::Chain => Ok(ExplorerResponse::Chain(chain_page(chain))),
        ExplorerRequest::Block { id } => match chain.block(id) {
            Some(block) => {
                let height = block.height.unwrap_or(0);
                let parent = block
                    .previous
                    .and_then(|hash| chain.block(BlockId::Hash(hash)));
                let coinbase = block.tx.first().and_then(|txid| chain.tx(txid));
                let subsidy = chain.subsidy(height);
                Ok(ExplorerResponse::Block(Box::new(views::block_page(
                    &block,
                    parent.as_ref(),
                    coinbase.as_ref(),
                    Some(&subsidy),
                    heights,
                ))))
            }
            None => match id {
                BlockId::Height(height) => future(chain, view, height),
                BlockId::Hash(_) => Err(not_found(
                    "Not in your node's best chain. zebrad looks blocks up by hash in its best \
                     chain only.",
                )),
            },
        },
        ExplorerRequest::BlockTxs {
            hash,
            offset,
            limit,
        } => chain
            .block(BlockId::Hash(hash))
            .map(|block| ExplorerResponse::BlockTxs(views::txid_page(&block, offset, limit)))
            .ok_or_else(|| not_found("Not in your node's best chain")),
        ExplorerRequest::Tx { txid } => {
            let tx = chain.tx(&txid).ok_or_else(|| {
                not_found(
                    "Not in your node's chain or mempool; it only knows transactions it has \
                     received",
                )
            })?;
            let rules_height = match tx.height {
                Some(height) if height >= 0 => height as u32,
                _ => chain.tip() + 1,
            };
            Ok(ExplorerResponse::Tx(Box::new(views::tx_page(
                &tx,
                rules_height,
                heights,
            ))))
        }
        ExplorerRequest::Resolve { query } => Ok(ExplorerResponse::Resolve(resolve(
            chain,
            view.network,
            &query,
        ))),
        ExplorerRequest::Status | ExplorerRequest::Interest { .. } => Ok(ExplorerResponse::Done),
    }
}

/// A height the node has no block at: past its tip while it syncs, else in the future.
fn future(
    chain: &SynthChain,
    view: &NodeView,
    height: u32,
) -> Result<ExplorerResponse, ExplorerError> {
    let tip = chain.tip();
    let estimated = view.heights().map_or(tip, |(_, estimated)| estimated);
    if !view.at_tip() && height <= estimated {
        return Err(ExplorerError::new(
            ExplorerErrorKind::PastTip {
                height,
                node_height: tip,
            },
            format!("Your node has blocks up to {tip}"),
        ));
    }
    let end_of_support = view
        .stats
        .as_ref()
        .and_then(|stats| stats.eos.as_ref())
        .map(|eos| eos.block_height);
    Ok(ExplorerResponse::Future(views::future_block(
        height,
        tip,
        chain.heights(),
        end_of_support,
    )))
}

/// The Chain page, read as the backend reads it.
fn chain_page(chain: &SynthChain) -> ChainPage {
    let heights = chain.heights();
    let tip = chain.tip();
    let rows = RECENT_BLOCKS.min(tip + 1);
    let oldest = tip + 1 - rows;
    let block = |height: u32| chain.block(BlockId::Height(height));
    let blocks: Vec<(BlockV1, Option<BlockV1>)> = (oldest..=tip)
        .rev()
        .filter_map(|height| Some((block(height)?, height.checked_sub(1).and_then(block))))
        .collect();
    let summaries = blocks
        .iter()
        .map(|(block, parent)| {
            let coinbase = block.tx.first().and_then(|txid| chain.tx(txid));
            views::block_summary(block, parent.as_ref(), coinbase.as_ref(), heights)
        })
        .collect();
    let tip_block = blocks.first().map(|(block, _)| block);
    let average_interval = tip.checked_sub(100).and_then(|older| {
        let header = chain.header(BlockId::Height(older))?;
        views::average_interval(
            (header.height, header.time),
            (tip, tip_block?.time.unwrap_or(header.time)),
        )
    });
    let supply = tip_block.and_then(|block| block.chain_supply.as_ref().map(|s| s.chain_value_zat));
    let burned_since_nu7 = heights
        .height(Upgrade::Nu7)
        .filter(|nu7| *nu7 > 0 && tip >= *nu7)
        .and_then(|nu7| {
            let before = block(nu7 - 1)?.chain_supply?.chain_value_zat;
            let zat = |height| u64::try_from(zec_to_zat(chain.subsidy(height).total)).ok();
            views::burned_since_nu7(nu7, tip, zat(nu7)?, zat(tip)?, before, supply?)
        });
    ChainPage {
        blocks: summaries,
        average_interval,
        target_spacing: heights.target_spacing(tip + 1),
        supply,
        burned_since_nu7,
        next_upgrade: views::next_upgrade(tip, heights),
        difficulty: tip_block.and_then(|block| block.difficulty),
    }
}

/// Where a query leads, as the backend resolves it.
fn resolve(chain: &SynthChain, network: Network, query: &str) -> Resolution {
    let go = |route| Resolution::Go { route };
    let lookup = |hash: BlockHash, out: Option<u32>| {
        let block = out.is_none() && chain.block(BlockId::Hash(hash)).is_some();
        let tx = chain.tx(&hash).is_some();
        let block_route = ExplorerRoute::Block {
            id: BlockId::Hash(hash),
        };
        let tx_route = ExplorerRoute::Tx { txid: hash, out };
        match (block, tx) {
            (true, true) => Resolution::Choose {
                routes: vec![block_route, tx_route],
            },
            (true, false) => go(block_route),
            (false, true) => go(tx_route),
            (false, false) => Resolution::Explain {
                hint: NotFoundHint::UnknownHash,
            },
        }
    };
    match classify_query(query, network) {
        Classified::Secret(what) => Resolution::Explain {
            hint: NotFoundHint::Refused { what },
        },
        Classified::Height(height) => go(ExplorerRoute::Block {
            id: BlockId::Height(height),
        }),
        Classified::BelowTip(back) => go(ExplorerRoute::Block {
            id: BlockId::Height(chain.tip().saturating_sub(back)),
        }),
        Classified::Hash(hash) => lookup(hash, None),
        Classified::Outpoint(outpoint) => lookup(outpoint.txid, Some(outpoint.vout)),
        Classified::Route(route) => go(route),
        Classified::Later(later) => Resolution::Explain {
            hint: NotFoundHint::NotSearchable {
                what: later.label().to_owned(),
            },
        },
        Classified::Empty | Classified::Unrecognised => Resolution::Explain {
            hint: NotFoundHint::Unrecognised,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;

    #[test]
    fn the_chain_page_reads_newest_first() {
        let chain = SynthChain::new(Network::Mainnet, 3_551_000, NOW);
        let page = chain_page(&chain);
        assert_eq!(page.blocks.len(), 25);
        assert_eq!(page.blocks[0].height, 3_551_000);
        assert!(page.blocks.iter().all(|block| block.ledger.fees.is_some()));
        assert!(page.burned_since_nu7.is_some());
        assert_eq!(page.target_spacing, 25);
        // A ten-block chain has ten rows.
        let short = chain_page(&SynthChain::new(Network::Mainnet, 9, NOW));
        assert_eq!(short.blocks.len(), 10);
        assert_eq!(short.blocks[9].height, 0);
    }

    #[test]
    fn search_finds_blocks_and_transactions() {
        let chain = SynthChain::new(Network::Testnet, 4_500_000, NOW);
        let block = chain.block(BlockId::Height(4_499_990)).unwrap();
        assert_eq!(
            resolve(&chain, Network::Testnet, &block.hash.to_hex()),
            Resolution::Go {
                route: ExplorerRoute::Block {
                    id: BlockId::Hash(block.hash)
                }
            }
        );
        assert_eq!(
            resolve(&chain, Network::Testnet, &block.tx[0].to_hex()),
            Resolution::Go {
                route: ExplorerRoute::Tx {
                    txid: block.tx[0],
                    out: None
                }
            }
        );
        assert_eq!(
            resolve(&chain, Network::Testnet, "latest"),
            Resolution::Go {
                route: ExplorerRoute::Block {
                    id: BlockId::Height(4_500_000)
                }
            }
        );
    }

    #[test]
    fn the_dashboard_and_the_explorer_share_one_chain() {
        let chain = SynthChain::new(Network::Mainnet, 3_551_000, NOW);
        let facts = ChainFacts::of(&chain, 3_551_000);
        assert_eq!(Some(facts.best_block_hash), chain.hash(3_551_000));
        let tip = chain.block(BlockId::Height(3_551_000)).unwrap();
        assert_eq!(
            Some(facts.pools.iter().sum::<u64>()),
            tip.chain_supply.map(|supply| supply.chain_value_zat)
        );
        assert_eq!(facts.chain_tip_branch, "77190ad9");
    }
}
