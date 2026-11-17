//! The mock's block explorer: as in the backend, one for the first node (the mock's own
//! zebrad), under its own id, over a synthetic chain (explorer-model's `synth`) that follows
//! the node's height, and answered with the backend's own builders (explorer-model's
//! `views`), so kinds, fees, ledgers and sentences follow its rules. Nothing is fixture data:
//! every block is computed when asked for. Other nodes, `?herd`'s too, get the backend's
//! answer that the Explorer reads only the node that was first when Zeal started, until P6.
//!
//! It reads as the backend does ([`reads`]): a small block whole at verbosity 3, a big one row
//! by row with the outputs its rows spend after them, a transaction's earlier ones up to 16
//! (all on request), a coinbase's block for its claim. Each read is named as the backend names
//! it for Show RPC and remembered as its cache would keep it, so a second answer comes "from
//! memory", and prefetches fill that memory.
//!
//! `?explorer=slow` makes each answer take 300 to 800 ms instead of 20 to 80, `?explorer=nobi`
//! takes zebrad's value-pool row from every seventh block (so the next one shows the zero-diff
//! case), `?explorer=big` gives every tenth block 5,000 more transactions (about 2 MB, read
//! row by row), and `?reorg` replaces the main node's top two blocks about every 90 s.
//! `?attip`, `?tip=` and `?explorer=empty` choose the main node's height (mock.rs).

mod reads;

use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    time::{Duration, UNIX_EPOCH},
};

use explorer_model::{
    classify::{Classified, classify_query},
    ledger,
    shapes::{BlockV1, Tx},
    synth::SynthChain,
    upgrades::{Upgrade, UpgradeHeights},
    views, zec_to_zat,
};
use launcher_types::{
    Network, NetworkUpgrade, NodeId, NodeStats, Phase,
    explorer::{
        BlockHash, BlockId, Card, CardTarget, ChainPage, ChainView, CoinbaseClaim, ExplorerCall,
        ExplorerError, ExplorerErrorKind, ExplorerLive, ExplorerLiveEvent, ExplorerReply,
        ExplorerRequest, ExplorerResponse, ExplorerRoute, LiveState, NotFoundHint, PrefetchTarget,
        ReorgNotice, Resolution, RowsRead, SpentOutput, TxId, TxPage, TxRowPage,
    },
};
use leptos::prelude::*;

use super::{ExplorerFlag, mock, sleep};
use crate::clock;
use reads::Reads;

/// How often `?reorg` replaces the main node's top blocks.
const REORG_EVERY: Duration = Duration::from_secs(90);
/// The rows of the Chain page, as in the backend.
const RECENT_BLOCKS: u32 = 25;
/// With `?explorer=big`, every this many blocks is a big one.
const BIG_EVERY: u32 = 10;
/// The targets one prefetch reads ahead, as in the backend.
const PREFETCH_TARGETS: usize = 8;

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
    /// The reads made, as the backend's cache would keep them ([`Reads`]): a first Chain page
    /// answers without the fees of rows whose coinbases were not read yet.
    seen: HashSet<String>,
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
    /// The network zebrad runs on: the chain the explorer reads.
    network: Network,
    /// The node's own network, which zebrad takes at its next start.
    ours: Network,
    /// The network zebrad still runs on when the node's own changed meanwhile.
    elsewhere: Option<Network>,
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

/// The network zebrad's `chain` names.
pub(super) fn network_of(chain: &str) -> Option<Network> {
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

/// The first node as its explorer sees it: under its own id, reading the chain zebrad
/// reports.
fn primary_view() -> NodeView {
    let mock = mock();
    let status = mock.status.get_untracked();
    let stats = mock.stats.get_untracked();
    let (id, ours) = mock
        .info
        .with_untracked(|info| (info.id.clone(), info.network));
    let reported = stats.as_ref().and_then(|stats| network_of(&stats.chain));
    let running = status.phase == Phase::Running && status.rpc_addr.is_some();
    NodeView {
        id,
        network: reported.unwrap_or(ours),
        ours,
        elsewhere: reported.filter(|reported| running && *reported != ours),
        running,
        stats,
    }
}

/// The node `node` names, if the explorer serves it: the first node only, as in the backend.
fn view_of(node: &NodeId) -> Result<NodeView, ExplorerError> {
    let primary = primary_view();
    if *node == primary.id {
        Ok(primary)
    } else {
        Err(ExplorerError::new(
            ExplorerErrorKind::UnknownNode,
            "The Explorer reads only the node that was first when Zeal started; a later \
             version gives every node its own.",
        ))
    }
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
        match mock().flags.explorer {
            ExplorerFlag::NoBlockInfo => chain = chain.with_missing_rows(7),
            ExplorerFlag::Big => chain = chain.with_big_blocks(BIG_EVERY),
            _ => {}
        }
        Explorer {
            node: view.id.clone(),
            chain,
            epoch: 0,
            reorg: None,
            known: None,
            sent: None,
            seen: HashSet::new(),
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
        if let Some(network) = view.elsewhere {
            return ExplorerLive {
                state: LiveState::OtherNetwork { network },
                tip: None,
                tip_time: None,
                reorg: None,
            };
        }
        let state = match (view.running, view.heights()) {
            (true, Some(_)) if view.at_tip() => LiveState::AtTip,
            (true, Some((height, estimated))) => LiveState::Syncing { height, estimated },
            _ => LiveState::NotRunning,
        };
        // Another network's chain is not this node's, whether zebrad runs or has stopped.
        if self.chain.network() != view.ours {
            return ExplorerLive {
                state,
                ..ExplorerLive::default()
            };
        }
        ExplorerLive {
            state,
            tip: self.known.map(|known| ChainView {
                stale: !view.running,
                ..known
            }),
            tip_time: self
                .known
                .and_then(|known| u64::try_from(self.chain.time_at(known.height)).ok()),
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
            // zebrad came back on another network: nothing known of the old chain applies, as
            // the backend clears its cache and its index.
            Some(index) if explorers[index].chain.network() != view.network => {
                explorers[index] = Explorer::new(view, height, estimated);
                index
            }
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
    let (id, ours) = mock()
        .info
        .with_untracked(|info| (info.id.clone(), info.network));
    let view = NodeView {
        id,
        network,
        ours,
        elsewhere: None,
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
                sources: None,
            })
        }
        ExplorerRequest::Interest { .. } => Ok(ExplorerReply {
            chain: None,
            response: ExplorerResponse::Done,
            sources: None,
        }),
        request => {
            if !view.running {
                return Err(not_running());
            }
            if let Some(network) = view.elsewhere {
                return Err(ExplorerError::new(
                    ExplorerErrorKind::NodeNotRunning,
                    format!("zebrad runs on {network}; it changes network at its next start"),
                ));
            }
            with_explorer(&view, |explorer| {
                explorer.known = Some(explorer.chain_view(true));
                let chain = explorer.known;
                let mut reads = Reads::new(&explorer.chain, &mut explorer.seen);
                let response = answer(&mut reads, &view, request);
                let sources = call.trace.then(|| reads.sources());
                response.map(|response| ExplorerReply {
                    chain,
                    response,
                    sources,
                })
            })?
        }
    }
}

fn answer(
    reads: &mut Reads,
    view: &NodeView,
    request: ExplorerRequest,
) -> Result<ExplorerResponse, ExplorerError> {
    let heights = reads.chain.heights().clone();
    match request {
        ExplorerRequest::Chain { with_fees } => {
            Ok(ExplorerResponse::Chain(chain_page(reads, with_fees)))
        }
        ExplorerRequest::Block { id } => match reads.block(id) {
            Some(block) => {
                let height = block.height.unwrap_or(0);
                let parent = block
                    .previous
                    .and_then(|hash| reads.block(BlockId::Hash(hash)));
                let coinbase = block.tx.first().and_then(|txid| reads.tx(txid));
                let subsidy = reads.subsidy(height);
                Ok(ExplorerResponse::Block(Box::new(views::block_page(
                    &block,
                    parent.as_ref(),
                    coinbase.as_ref(),
                    Some(&subsidy),
                    &heights,
                ))))
            }
            None => match id {
                BlockId::Height(height) => future(reads.chain, view, height),
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
            with_fees,
        } => block_txs(reads, hash, offset, limit, with_fees).map(ExplorerResponse::BlockTxs),
        ExplorerRequest::Tx { txid, resolve } => {
            let tx = reads.tx(&txid).ok_or_else(|| {
                not_found(
                    "Not in your node's chain or mempool; it only knows transactions it has \
                     received",
                )
            })?;
            Ok(ExplorerResponse::Tx(Box::new(tx_page(reads, &tx, resolve))))
        }
        ExplorerRequest::Resolve { query } => Ok(ExplorerResponse::Resolve(resolve(
            reads,
            view.network,
            &query,
        ))),
        ExplorerRequest::Card { target } => card(reads, target).map(ExplorerResponse::Card),
        ExplorerRequest::Prefetch { targets } => {
            prefetch(reads, targets);
            Ok(ExplorerResponse::Done)
        }
        ExplorerRequest::Status | ExplorerRequest::Interest { .. } => Ok(ExplorerResponse::Done),
    }
}

/// The height whose rules apply to `tx`: its block's, or the next block's.
fn rules_height(chain: &SynthChain, tx: &Tx) -> u32 {
    match tx.height {
        Some(height) if height >= 0 => height as u32,
        _ => chain.tip() + 1,
    }
}

/// A page of a block's rows, as the backend reads it: a small block whole at verbosity 3; a
/// big one row by row, its rows' fees waiting for `with_fees` unless the earlier transactions
/// they spend from were read already.
fn block_txs(
    reads: &mut Reads,
    hash: BlockHash,
    offset: u32,
    limit: u16,
    with_fees: bool,
) -> Result<TxRowPage, ExplorerError> {
    let heights = reads.chain.heights().clone();
    let block = reads
        .block(BlockId::Hash(hash))
        .ok_or_else(|| not_found("Not in your node's best chain"))?;
    let height = block.height.unwrap_or(0);
    let total = block.tx.len();
    let start = (offset as usize).min(total);
    let end = (start + usize::from(limit.min(views::TXIDS_PER_PAGE))).min(total);
    let page = |rows, read, fees_pending| TxRowPage {
        offset: start as u32,
        total: total as u32,
        rows,
        read,
        fees_pending,
    };
    if views::read_whole(&block) {
        let full = reads
            .block_v3(hash)
            .ok_or_else(|| not_found("Not in your node's best chain"))?;
        let rows = full
            .tx
            .iter()
            .enumerate()
            .skip(start)
            .take(end - start)
            .map(|(index, tx)| {
                views::tx_row(
                    tx,
                    index as u32,
                    &views::spent_from_v3(tx),
                    height,
                    &heights,
                )
            })
            .collect();
        return Ok(page(rows, RowsRead::Block, false));
    }
    let txs: Vec<Tx> = block.tx[start..end]
        .iter()
        .filter_map(|txid| reads.tx(txid))
        .collect();
    let mut pending = false;
    let rows = txs
        .iter()
        .enumerate()
        .map(|(index, tx)| {
            let mut spent = vec![None; tx.vin.len()];
            if !tx.is_coinbase() {
                for parent in views::missing_parents(tx, &spent) {
                    if !with_fees && !reads.has_tx(&parent) {
                        reads.fill_tx(&parent);
                        pending = true;
                    }
                }
                if with_fees || !pending {
                    let parents: Vec<(TxId, Option<Tx>)> = views::missing_parents(tx, &spent)
                        .into_iter()
                        .map(|parent| (parent, reads.tx(&parent)))
                        .collect();
                    views::fill_spent(tx, &mut spent, |txid| {
                        parents
                            .iter()
                            .find(|(parent, _)| parent == txid)
                            .and_then(|(_, tx)| tx.clone().map(std::sync::Arc::new))
                    });
                }
            }
            views::tx_row(tx, (start + index) as u32, &spent, height, &heights)
        })
        .collect();
    Ok(page(rows, RowsRead::OneByOne, pending && !with_fees))
}

/// The Transaction page, as the backend reads it: its block at verbosity 3 when that was read
/// already; else up to 16 earlier transactions, or the block whole when it spends from more and
/// the block is small; with `resolve`, all of them. A coinbase gets its block's claim.
fn tx_page(reads: &mut Reads, tx: &Tx, resolve: bool) -> TxPage {
    let heights = reads.chain.heights().clone();
    let rules_height = rules_height(reads.chain, tx);
    let mined_in = tx
        .block_hash
        .filter(|_| tx.height.is_some_and(|height| height >= 0));
    let mut spent: Vec<Option<SpentOutput>> = vec![None; tx.vin.len()];
    let mut resolvable = false;
    let mut claim = None;
    if tx.is_coinbase() {
        claim = Some(coinbase_claim(reads, tx, mined_in));
    } else if !tx.vin.is_empty() {
        if let Some(hash) = mined_in
            && reads.has_v3(hash)
            && let Some(entry) = reads
                .block_v3(hash)
                .and_then(|full| full.tx.into_iter().find(|entry| entry.txid == tx.txid))
        {
            spent = views::spent_from_v3(&entry);
        }
        let parents = views::missing_parents(tx, &spent);
        let small_block = mined_in.filter(|hash| {
            !resolve
                && parents.len() > views::PARENTS_AT_ONCE
                && reads
                    .block(BlockId::Hash(*hash))
                    .is_some_and(|block| views::read_whole(&block))
        });
        if let Some(hash) = small_block {
            if let Some(entry) = reads
                .block_v3(hash)
                .and_then(|full| full.tx.into_iter().find(|entry| entry.txid == tx.txid))
            {
                spent = views::spent_from_v3(&entry);
            }
        } else if !parents.is_empty() {
            let cap = match (resolve, mined_in) {
                (false, _) => views::PARENTS_AT_ONCE,
                (true, Some(_)) => views::RESOLVED_PARENTS,
                (true, None) => views::RESOLVED_WAITING_PARENTS,
            };
            resolvable = !resolve && parents.len() > cap;
            let found: Vec<(TxId, Option<Tx>)> = parents
                .into_iter()
                .take(cap)
                .map(|parent| (parent, reads.tx(&parent)))
                .collect();
            views::fill_spent(tx, &mut spent, |txid| {
                found
                    .iter()
                    .find(|(parent, _)| parent == txid)
                    .and_then(|(_, tx)| tx.clone().map(std::sync::Arc::new))
            });
        }
    }
    let mut page = views::tx_page(tx, &spent, rules_height, &heights);
    page.resolvable = resolvable && page.unresolved > 0;
    page.coinbase = claim;
    page
}

/// What the coinbase `tx` claimed, from its block, the block's parent and its subsidy.
fn coinbase_claim(reads: &mut Reads, tx: &Tx, block: Option<BlockHash>) -> CoinbaseClaim {
    let heights = reads.chain.heights().clone();
    let Some(block) = block.and_then(|hash| reads.block(BlockId::Hash(hash))) else {
        return CoinbaseClaim {
            subsidy: None,
            fees: None,
            burned: None,
            lockbox: None,
        };
    };
    let height = block.height.unwrap_or(0);
    let parent = block
        .previous
        .and_then(|hash| reads.block(BlockId::Hash(hash)));
    let subsidy = u64::try_from(zec_to_zat(reads.subsidy(height).total)).ok();
    let ledger = ledger::block_ledger(&block, parent.as_ref(), Some(tx), &heights);
    views::coinbase_claim(&ledger, subsidy)
}

/// A hover card, from one read.
fn card(reads: &mut Reads, target: CardTarget) -> Result<Card, ExplorerError> {
    match target {
        CardTarget::Block { id } => reads
            .block(id)
            .map(|block| views::block_card(&block))
            .ok_or_else(|| not_found("Your node does not have it")),
        CardTarget::Tx { txid } => {
            let tx = reads
                .tx(&txid)
                .ok_or_else(|| not_found("Your node does not have it"))?;
            let rules_height = rules_height(reads.chain, &tx);
            Ok(views::tx_card(&tx, rules_height, reads.chain.heights()))
        }
    }
}

/// Reads ahead, as the backend fills its cache in the background.
fn prefetch(reads: &mut Reads, targets: Vec<PrefetchTarget>) {
    for target in targets.into_iter().take(PREFETCH_TARGETS) {
        match target {
            PrefetchTarget::Block { id } => reads.fill_block_page(id),
            PrefetchTarget::Tx { txid } => reads.fill_tx(&txid),
            PrefetchTarget::BlockTxs {
                hash,
                offset,
                limit,
            } => {
                if let Some(block) = reads.chain.block(BlockId::Hash(hash))
                    && !views::read_whole(&block)
                {
                    let start = (offset as usize).min(block.tx.len());
                    let end = (start + usize::from(limit)).min(block.tx.len());
                    for txid in &block.tx[start..end] {
                        reads.fill_tx(txid);
                    }
                }
            }
        }
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
    let network_tip = if view.at_tip() { tip } else { estimated };
    Ok(ExplorerResponse::Future(views::future_block(
        height,
        tip,
        network_tip,
        chain.heights(),
        end_of_support,
    )))
}

/// The Chain page, read as the backend reads it: without `with_fees`, the rows whose coinbases
/// were not read yet have no fees, and their coinbases are read for the next ask.
fn chain_page(reads: &mut Reads, with_fees: bool) -> ChainPage {
    let chain = reads.chain;
    let heights = chain.heights().clone();
    let tip = chain.tip();
    reads.best();
    let rows = RECENT_BLOCKS.min(tip + 1);
    let oldest = tip + 1 - rows;
    let blocks: Vec<(BlockV1, Option<BlockV1>)> = (oldest..=tip)
        .rev()
        .filter_map(|height| {
            let block = reads.block(BlockId::Height(height))?;
            let parent = height
                .checked_sub(1)
                .and_then(|parent| chain.block(BlockId::Height(parent)));
            Some((block, parent))
        })
        .collect();
    if let Some(oldest) = oldest.checked_sub(1) {
        reads.block(BlockId::Height(oldest));
    }
    let mut fees_pending = false;
    let summaries = blocks
        .iter()
        .map(|(block, parent)| {
            let coinbase = block.tx.first().and_then(|txid| {
                if with_fees || reads.has_tx(txid) {
                    reads.tx(txid)
                } else {
                    reads.fill_tx(txid);
                    fees_pending = true;
                    None
                }
            });
            views::block_summary(block, parent.as_ref(), coinbase.as_ref(), &heights)
        })
        .collect();
    let tip_block = blocks.first().map(|(block, _)| block);
    let average_interval = tip.checked_sub(100).and_then(|older| {
        let header = reads.header(BlockId::Height(older))?;
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
            let before = reads
                .block(BlockId::Height(nu7 - 1))?
                .chain_supply?
                .chain_value_zat;
            let at_nu7 = u64::try_from(zec_to_zat(reads.subsidy(nu7).total)).ok()?;
            let at_tip = u64::try_from(zec_to_zat(reads.subsidy(tip).total)).ok()?;
            views::burned_since_nu7(nu7, tip, at_nu7, at_tip, before, supply?)
        });
    ChainPage {
        blocks: summaries,
        average_interval,
        target_spacing: heights.target_spacing(tip + 1),
        supply,
        burned_since_nu7,
        next_upgrade: views::next_upgrade(tip, &heights),
        difficulty: tip_block.and_then(|block| block.difficulty),
        fees_pending,
    }
}

/// Where a query leads, as the backend resolves it.
fn resolve(reads: &mut Reads, network: Network, query: &str) -> Resolution {
    let go = |route| Resolution::Go { route };
    let tip = reads.chain.tip();
    let mut lookup = |hash: BlockHash, out: Option<u32>| {
        let block = out.is_none() && reads.block(BlockId::Hash(hash)).is_some();
        let tx = reads.tx(&hash).is_some();
        let block_route = ExplorerRoute::Block {
            id: BlockId::Hash(hash),
        };
        let tx_route = ExplorerRoute::Tx { txid: hash, out };
        match (block, tx) {
            (true, true) => Resolution::Choose {
                routes: vec![block_route, tx_route],
            },
            (true, false) => Resolution::Go { route: block_route },
            (false, true) => Resolution::Go { route: tx_route },
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
            id: BlockId::Height(tip.saturating_sub(back)),
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
    use launcher_types::explorer::Shown;

    use super::*;

    const NOW: i64 = 1_800_000_000;

    #[test]
    fn the_chain_page_reads_newest_first() {
        let chain = SynthChain::new(Network::Mainnet, 3_551_000, NOW);
        let mut read = HashSet::new();
        // As in the backend: the first answer waits for no coinbase, the next has them all.
        let first = chain_page(&mut Reads::new(&chain, &mut read), false);
        assert!(first.fees_pending);
        assert!(first.blocks.iter().all(|block| block.ledger.fees.is_none()));
        let page = chain_page(&mut Reads::new(&chain, &mut read), false);
        assert!(!page.fees_pending);
        assert_eq!(
            page,
            chain_page(&mut Reads::new(&chain, &mut HashSet::new()), true)
        );
        assert_eq!(page.blocks.len(), 25);
        assert_eq!(page.blocks[0].height, 3_551_000);
        assert!(page.blocks.iter().all(|block| block.ledger.fees.is_some()));
        assert!(page.burned_since_nu7.is_some());
        assert_eq!(page.target_spacing, 25);
        // A ten-block chain has ten rows.
        let short_chain = SynthChain::new(Network::Mainnet, 9, NOW);
        let short = chain_page(&mut Reads::new(&short_chain, &mut read), true);
        assert_eq!(short.blocks.len(), 10);
        assert_eq!(short.blocks[9].height, 0);
    }

    #[test]
    fn search_finds_blocks_and_transactions() {
        let chain = SynthChain::new(Network::Testnet, 4_500_000, NOW);
        let block = chain.block(BlockId::Height(4_499_990)).unwrap();
        let mut seen = HashSet::new();
        let mut reads = Reads::new(&chain, &mut seen);
        assert_eq!(
            resolve(&mut reads, Network::Testnet, &block.hash.to_hex()),
            Resolution::Go {
                route: ExplorerRoute::Block {
                    id: BlockId::Hash(block.hash)
                }
            }
        );
        assert_eq!(
            resolve(&mut reads, Network::Testnet, &block.tx[0].to_hex()),
            Resolution::Go {
                route: ExplorerRoute::Tx {
                    txid: block.tx[0],
                    out: None
                }
            }
        );
        assert_eq!(
            resolve(&mut reads, Network::Testnet, "latest"),
            Resolution::Go {
                route: ExplorerRoute::Block {
                    id: BlockId::Height(4_500_000)
                }
            }
        );
    }

    #[test]
    fn a_stopped_node_shows_nothing_of_another_networks_chain() {
        let mut explorer = Explorer {
            node: NodeId::new("zebrad-mainnet").unwrap(),
            chain: SynthChain::new(Network::Mainnet, 3_551_000, NOW),
            epoch: 0,
            reorg: None,
            known: None,
            sent: None,
            seen: HashSet::new(),
        };
        explorer.known = Some(explorer.chain_view(true));
        let stopped = |ours| NodeView {
            id: explorer.node.clone(),
            network: ours,
            ours,
            elsewhere: None,
            running: false,
            stats: None,
        };
        let live = explorer.live(&stopped(Network::Mainnet));
        assert!(
            live.tip
                .is_some_and(|tip| tip.stale && tip.height == 3_551_000),
            "a stopped node keeps its last tip: {live:?}"
        );
        assert!(live.tip_time.is_some());
        // The node moved to Testnet while zebrad was stopped.
        assert_eq!(
            explorer.live(&stopped(Network::Testnet)),
            ExplorerLive::default(),
            "no Mainnet tip under a Testnet node"
        );
    }

    #[test]
    fn rows_read_like_the_backends() {
        let chain = SynthChain::new(Network::Mainnet, 3_551_000, NOW).with_big_blocks(BIG_EVERY);
        let mut seen = HashSet::new();
        // A small block: whole, every fee known, one heavy read.
        let small = chain.block(BlockId::Height(3_550_001)).unwrap();
        let mut reads = Reads::new(&chain, &mut seen);
        let page = block_txs(&mut reads, small.hash, 0, 200, false).unwrap();
        assert_eq!(page.read, RowsRead::Block);
        assert_eq!(page.rows.len(), small.tx.len());
        assert!(
            page.rows[1..]
                .iter()
                .all(|row| matches!(row.fee, Some(Shown::Public { .. })))
        );
        let sources = reads.sources();
        assert_eq!(sources[1].params, format!(r#"["{}",3]"#, small.hash));
        // A big block: row by row, the fees after.
        let big = chain.block(BlockId::Height(3_550_000)).unwrap();
        assert!(big.tx.len() > 5_000);
        let mut reads = Reads::new(&chain, &mut seen);
        let first = block_txs(&mut reads, big.hash, 4_000, 25, false).unwrap();
        assert_eq!(first.read, RowsRead::OneByOne);
        assert!(first.fees_pending);
        let mut reads = Reads::new(&chain, &mut seen);
        let full = block_txs(&mut reads, big.hash, 4_000, 25, true).unwrap();
        assert!(!full.fees_pending);
        for row in &full.rows {
            assert_eq!(
                row.fee,
                chain.fee(&row.txid).map(|zat| Shown::Public { zat })
            );
        }
        // The earlier transactions were read in the background: from memory now.
        assert!(reads.sources().iter().skip(26).all(|call| call.cached));
    }

    #[test]
    fn transaction_pages_read_their_inputs_and_a_coinbase_its_claim() {
        let chain = SynthChain::new(Network::Mainnet, 3_551_000, NOW);
        let mut seen = HashSet::new();
        let block = chain.block(BlockId::Height(3_550_500)).unwrap();
        let spending = block
            .tx
            .iter()
            .skip(1)
            .find(|txid| !chain.tx(txid).unwrap().vin.is_empty())
            .copied()
            .expect("a transaction with inputs");
        let tx = chain.tx(&spending).unwrap();
        let page = tx_page(&mut Reads::new(&chain, &mut seen), &tx, false);
        assert_eq!(page.unresolved, 0);
        assert_eq!(
            page.fee,
            chain.fee(&spending).map(|zat| Shown::Public { zat })
        );
        let coinbase = chain.tx(&block.tx[0]).unwrap();
        let page = tx_page(&mut Reads::new(&chain, &mut seen), &coinbase, false);
        let claim = page.coinbase.expect("a claim");
        assert!(claim.subsidy.is_some() && claim.fees.is_some());
        assert!(matches!(
            explorer_model::flow::tx_flow(&page).balance,
            explorer_model::flow::Balance::Balanced { .. }
        ));
        // A card is one read; a prefetch fills the memory without listing a read.
        let mut reads = Reads::new(&chain, &mut seen);
        assert!(matches!(
            card(
                &mut reads,
                CardTarget::Block {
                    id: BlockId::Height(3_550_400)
                }
            ),
            Ok(Card::Block {
                height: 3_550_400,
                ..
            })
        ));
        prefetch(
            &mut reads,
            vec![PrefetchTarget::Block {
                id: BlockId::Height(3_550_300),
            }],
        );
        assert_eq!(reads.sources().len(), 1);
        let mut reads = Reads::new(&chain, &mut seen);
        let _ = answer(
            &mut reads,
            &NodeView {
                id: NodeId::new("zebrad-mainnet").unwrap(),
                network: Network::Mainnet,
                ours: Network::Mainnet,
                elsewhere: None,
                running: true,
                stats: None,
            },
            ExplorerRequest::Block {
                id: BlockId::Height(3_550_300),
            },
        );
        assert!(reads.sources().iter().all(|call| call.cached));
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
