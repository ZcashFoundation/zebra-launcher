//! One async function per request: what each page reads, asked for together so the reads go
//! out at once, and built with explorer-model's views.

use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::Duration,
};

use explorer_model::{
    classify::{Classified, classify_query},
    ledger,
    shapes::{BlockV1, Tx},
    upgrades::Upgrade,
    views, zec_to_zat,
};
use launcher_types::explorer::{
    BlockHash, BlockId, Card, CardTarget, ChainPage, ChainView, CoinbaseClaim, ExplorerError,
    ExplorerErrorKind, ExplorerReply, ExplorerRequest, ExplorerResponse, ExplorerRoute,
    NotFoundHint, PrefetchTarget, Resolution, RowsRead, SpentOutput, TxId, TxRowPage,
};

use super::{
    actor::{Context, Fetcher},
    sched::Lane,
};

/// The rows of the Chain page.
pub(super) const RECENT_BLOCKS: u32 = 25;
/// The blocks the Chain page's average interval spans.
const INTERVAL_SPAN: u32 = 100;
/// How old a checked tip the Chain page and search accept.
const TIP_AGE: Duration = Duration::from_secs(2);
/// The earlier transactions one page of a big block's rows reads, on the prefetch lane.
const ROW_PARENTS: usize = 1_000;
/// The targets one prefetch reads ahead.
const PREFETCH_TARGETS: usize = 8;

pub(super) async fn handle(
    request: ExplorerRequest,
    context: Context,
) -> Result<ExplorerReply, ExplorerError> {
    let (chain, response) = match request {
        ExplorerRequest::Chain { with_fees } => {
            let (chain, page) = chain_page(&context, with_fees).await?;
            (Some(chain), ExplorerResponse::Chain(page))
        }
        ExplorerRequest::Block { id } => (context.chain, block(&context, id).await?),
        ExplorerRequest::BlockTxs {
            hash,
            offset,
            limit,
            with_fees,
        } => {
            let page = block_txs(&context, hash, offset, limit, with_fees).await?;
            (context.chain, ExplorerResponse::BlockTxs(page))
        }
        ExplorerRequest::Tx { txid, resolve } => {
            (context.chain, tx(&context, txid, resolve).await?)
        }
        ExplorerRequest::Resolve { query } => {
            let resolution = resolve(&context, &query).await?;
            (context.chain, ExplorerResponse::Resolve(resolution))
        }
        ExplorerRequest::Card { target } => (
            context.chain,
            ExplorerResponse::Card(card(&context, target).await?),
        ),
        ExplorerRequest::Status
        | ExplorerRequest::Interest { .. }
        | ExplorerRequest::Prefetch { .. } => {
            return Err(ExplorerError::new(
                ExplorerErrorKind::Internal,
                "the actor answers this request itself",
            ));
        }
    };
    Ok(ExplorerReply {
        chain,
        response,
        sources: None,
    })
}

/// Reads ahead what a page may soon show, on the prefetch lane, so the page's own reads and
/// the follower go first. Failures are dropped: the page asks again when it is shown.
pub(super) async fn prefetch(targets: Vec<PrefetchTarget>, context: Context) {
    let context = Context {
        fetch: context.fetch.in_lane(Lane::Prefetch).untraced(),
        ..context
    };
    let mut reads = Vec::new();
    for target in targets.into_iter().take(PREFETCH_TARGETS) {
        let context = context.clone();
        reads.push(tokio::spawn(async move {
            match target {
                PrefetchTarget::Block { id } => {
                    let _ = block(&context, id).await;
                }
                PrefetchTarget::Tx { txid } => {
                    let _ = context.fetch.tx(txid).tx().await;
                }
                PrefetchTarget::BlockTxs {
                    hash,
                    offset,
                    limit,
                } => {
                    // Only a big block's rows: a small one is read whole, which is heavy and
                    // waits for its page.
                    if let Some(block) = context.fetch.cached_block(hash).await
                        && !views::read_whole(&block)
                    {
                        let _ = block_txs(&context, hash, offset, limit, false).await;
                    }
                }
            }
        }));
    }
    for read in reads {
        let _ = read.await;
    }
}

/// The Chain page: the tip's 25 blocks with their ledgers, the average interval of the last
/// 100, the supply and what NU7 burned, and the next upgrade. The rows' coinbases are read on
/// the prefetch lane, so a page asked for meanwhile goes first; `with_fees` waits for them,
/// and without it the page takes the cached ones and leaves the others to fill the cache.
async fn chain_page(
    context: &Context,
    with_fees: bool,
) -> Result<(ChainView, ChainPage), ExplorerError> {
    let fetch = &context.fetch;
    let chain = fetch.tip(TIP_AGE).await?;
    let tip = chain.height;
    let rows = RECENT_BLOCKS.min(tip + 1);
    let oldest = tip + 1 - rows;
    // The rows and the parent of the oldest one, asked for together.
    let pending: Vec<(u32, _)> = (oldest.saturating_sub(1)..=tip)
        .rev()
        .map(|height| (height, fetch.block(BlockId::Height(height))))
        .collect();
    let header = (tip >= INTERVAL_SPAN).then(|| fetch.header(BlockId::Height(tip - INTERVAL_SPAN)));
    let nu7 = context
        .heights
        .height(Upgrade::Nu7)
        .filter(|nu7| *nu7 > 0 && tip >= *nu7);
    let burn_reads = nu7.map(|nu7| {
        (
            nu7,
            fetch.block(BlockId::Height(nu7 - 1)),
            fetch.subsidy(nu7),
            fetch.subsidy(tip),
        )
    });
    let mut blocks: BTreeMap<u32, Arc<BlockV1>> = BTreeMap::new();
    for (height, block) in pending {
        blocks.insert(height, block.block().await?);
    }
    // Each row's coinbase, for its fees; a coinbase that cannot be read leaves them unknown.
    let txids: Vec<Option<TxId>> = (oldest..=tip)
        .rev()
        .map(|height| blocks[&height].tx.first().copied())
        .collect();
    let mut coinbases = Vec::with_capacity(txids.len());
    let mut fees_pending = false;
    if with_fees {
        let reads: Vec<_> = txids
            .iter()
            .map(|txid| txid.map(|txid| fetch.tx_later(txid)))
            .collect();
        for read in reads {
            coinbases.push(match read {
                Some(pending) => pending.tx().await.ok(),
                None => None,
            });
        }
    } else {
        for txid in txids {
            let Some(txid) = txid else {
                coinbases.push(None);
                continue;
            };
            let cached = fetch.cached_tx(txid).await;
            if cached.is_none() {
                // Read in the background: the answer fills the cache for the next ask.
                drop(fetch.tx_later(txid));
                fees_pending = true;
            }
            coinbases.push(cached);
        }
    }
    let mut summaries = Vec::with_capacity(rows as usize);
    for (height, coinbase) in (oldest..=tip).rev().zip(coinbases) {
        let block = &blocks[&height];
        let parent = height.checked_sub(1).and_then(|parent| blocks.get(&parent));
        summaries.push(views::block_summary(
            block,
            parent.map(Arc::as_ref),
            coinbase.as_deref(),
            &context.heights,
        ));
    }
    let tip_block = &blocks[&tip];
    let average_interval = match header {
        Some(header) => header.header().await.ok().and_then(|header| {
            views::average_interval(
                (header.height, header.time),
                (tip, tip_block.time.unwrap_or(header.time)),
            )
        }),
        None => None,
    };
    let supply = tip_block
        .chain_supply
        .as_ref()
        .map(|supply| supply.chain_value_zat);
    let burned_since_nu7 = match burn_reads {
        Some((nu7, before, at_nu7, at_tip)) => {
            let before = before.block().await.ok();
            let at_nu7 = at_nu7.subsidy().await.ok();
            let at_tip = at_tip.subsidy().await.ok();
            let zat = |zec: f64| u64::try_from(zec_to_zat(zec)).ok();
            match (
                before.and_then(|block| block.chain_supply.clone()),
                at_nu7.and_then(|subsidy| zat(subsidy.total)),
                at_tip.and_then(|subsidy| zat(subsidy.total)),
                supply,
            ) {
                (Some(before), Some(at_nu7), Some(at_tip), Some(supply)) => {
                    views::burned_since_nu7(
                        nu7,
                        tip,
                        at_nu7,
                        at_tip,
                        before.chain_value_zat,
                        supply,
                    )
                }
                _ => None,
            }
        }
        None => None,
    };
    let page = ChainPage {
        blocks: summaries,
        average_interval,
        target_spacing: context.heights.target_spacing(tip + 1),
        supply,
        burned_since_nu7,
        next_upgrade: views::next_upgrade(tip, &context.heights),
        difficulty: tip_block.difficulty,
        fees_pending,
    };
    Ok((chain, page))
}

/// The Block page, or for a height above the tip, the countdown to it.
async fn block(context: &Context, id: BlockId) -> Result<ExplorerResponse, ExplorerError> {
    let fetch = &context.fetch;
    let block = match fetch.block(id).block().await {
        Ok(block) => block,
        Err(err) if err.kind == ExplorerErrorKind::NotFound => {
            return match id {
                BlockId::Height(height) => future(context, height).await,
                BlockId::Hash(_) => Err(ExplorerError::new(
                    ExplorerErrorKind::NotFound,
                    "Not in your node's best chain. zebrad looks blocks up by hash in its best \
                     chain only.",
                )),
            };
        }
        Err(err) => return Err(err),
    };
    let height = block.height.unwrap_or(0);
    let parent = block
        .previous
        .map(|previous| fetch.block(BlockId::Hash(previous)));
    let coinbase = block.tx.first().map(|txid| fetch.tx(*txid));
    let subsidy = fetch.subsidy(height);
    let parent = match parent {
        Some(pending) => Some(pending.block().await?),
        None => None,
    };
    let coinbase = match coinbase {
        Some(pending) => pending.tx().await.ok(),
        None => None,
    };
    // zebrad refuses some heights (the founders' reward era): the split is then left out.
    let subsidy = subsidy.subsidy().await.ok();
    let page = views::block_page(
        &block,
        parent.as_deref(),
        coinbase.as_deref(),
        subsidy.as_deref(),
        &context.heights,
    );
    Ok(ExplorerResponse::Block(Box::new(page)))
}

/// A height the node has no block at: past its tip while it syncs, else in the future,
/// counted from the network's tip while the node syncs.
async fn future(context: &Context, height: u32) -> Result<ExplorerResponse, ExplorerError> {
    let tip = context.fetch.tip(TIP_AGE).await?.height;
    if height <= tip {
        return Err(ExplorerError::new(
            ExplorerErrorKind::NotFound,
            "Your node does not have this block",
        ));
    }
    let estimated = context.estimated_height.filter(|_| context.syncing);
    if estimated.is_some_and(|estimated| height <= estimated) {
        return Err(ExplorerError::new(
            ExplorerErrorKind::PastTip {
                height,
                node_height: tip,
            },
            format!("Your node has blocks up to {tip}"),
        ));
    }
    Ok(ExplorerResponse::Future(views::future_block(
        height,
        tip,
        estimated.unwrap_or(tip),
        &context.heights,
        context.end_of_support,
    )))
}

/// A page of a block's transaction rows. A small block is read whole, once, at verbosity 3,
/// which brings every spent output and fee. A big one is read row by row: the rows'
/// transactions on the page's lane, then the outputs their inputs spend, on the prefetch lane
/// whether `with_fees` waits for them or the page answers without them. So the reads of rows
/// asked for meanwhile, and the follower's checks of the tip, go before them.
async fn block_txs(
    context: &Context,
    hash: BlockHash,
    offset: u32,
    limit: u16,
    with_fees: bool,
) -> Result<TxRowPage, ExplorerError> {
    let fetch = &context.fetch;
    let block = fetch.block(BlockId::Hash(hash)).block().await?;
    let height = block.height.unwrap_or(0);
    let total = block.tx.len();
    let start = (offset as usize).min(total);
    let end = (start + usize::from(limit.min(views::TXIDS_PER_PAGE))).min(total);
    let row_page = |rows, read, fees_pending| TxRowPage {
        offset: start as u32,
        total: total as u32,
        rows,
        read,
        fees_pending,
    };
    if views::read_whole(&block) {
        let full = fetch.block_v3(hash).block_v3().await?;
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
                    &context.heights,
                )
            })
            .collect();
        return Ok(row_page(rows, RowsRead::Block, false));
    }
    let reads: Vec<_> = block.tx[start..end]
        .iter()
        .map(|txid| fetch.tx(*txid))
        .collect();
    let mut txs = Vec::with_capacity(reads.len());
    for read in reads {
        txs.push(read.tx().await?);
    }
    let (spent, left_out) = spent_by(fetch, &txs, with_fees, ROW_PARENTS).await;
    let rows = txs
        .iter()
        .zip(&spent)
        .enumerate()
        .map(|(index, (tx, spent))| {
            views::tx_row(tx, (start + index) as u32, spent, height, &context.heights)
        })
        .collect();
    Ok(row_page(rows, RowsRead::OneByOne, left_out && !with_fees))
}

/// The outputs the inputs of `txs` spend, one list per transaction, from the earlier
/// transactions in the cache. Up to `cap` of the others are asked for on the prefetch lane,
/// never the fetcher's, so a page waiting for them does not move them ahead of interactive
/// reads: with `read` the answer waits for them, without it they fill the cache for the next
/// ask. Also says whether any was left out.
async fn spent_by(
    fetch: &Fetcher,
    txs: &[Arc<Tx>],
    read: bool,
    cap: usize,
) -> (Vec<Vec<Option<SpentOutput>>>, bool) {
    let mut parents: HashMap<TxId, Option<Arc<Tx>>> = HashMap::new();
    let mut order = Vec::new();
    for tx in txs.iter().filter(|tx| !tx.is_coinbase()) {
        for input in &tx.vin {
            if let Some(txid) = input.txid
                && !parents.contains_key(&txid)
            {
                parents.insert(txid, None);
                order.push(txid);
            }
        }
    }
    let mut missing = Vec::new();
    for txid in order {
        match fetch.cached_tx(txid).await {
            Some(parent) => {
                parents.insert(txid, Some(parent));
            }
            None => missing.push(txid),
        }
    }
    let left_out = !missing.is_empty() && (!read || missing.len() > cap);
    let reads: Vec<_> = missing
        .iter()
        .take(cap)
        .map(|txid| (*txid, fetch.tx_later(*txid)))
        .collect();
    if read {
        for (txid, pending) in reads {
            parents.insert(txid, pending.tx().await.ok());
        }
    }
    // Otherwise the reads go on unheeded, and their answers fill the cache for the next ask.
    let spent = txs
        .iter()
        .map(|tx| {
            tx.vin
                .iter()
                .map(|input| {
                    let parent = parents.get(&input.txid?)?.as_ref()?;
                    views::spent_from_parent(parent, input.vout?)
                })
                .collect()
        })
        .collect();
    (spent, left_out)
}

/// The Transaction page, with the outputs its inputs spend: from its block at verbosity 3
/// when the cache has it; else from up to 16 earlier transactions, or the block read whole
/// when it spends from more and the block is small; else, with `resolve`, from them all (up to
/// 1,000, or 200 for a waiting transaction), the first 16 on the page's lane and the rest on
/// the prefetch lane. A coinbase gets what it claimed, from its block.
async fn tx(
    context: &Context,
    txid: TxId,
    resolve: bool,
) -> Result<ExplorerResponse, ExplorerError> {
    let fetch = &context.fetch;
    let tx = match fetch.tx(txid).tx().await {
        Ok(tx) => tx,
        Err(err) if err.kind == ExplorerErrorKind::NotFound => {
            return Err(ExplorerError::new(
                ExplorerErrorKind::NotFound,
                "Not in your node's chain or mempool; it only knows transactions it has received",
            ));
        }
        Err(err) => return Err(err),
    };
    let rules_height = rules_height(context, &tx);
    let mined_in = tx
        .block_hash
        .filter(|_| tx.height.is_some_and(|height| height >= 0));
    let mut spent: Vec<Option<SpentOutput>> = vec![None; tx.vin.len()];
    let mut resolvable = false;
    let mut claim = None;
    if tx.is_coinbase() {
        claim = Some(coinbase_claim(context, &tx, mined_in).await);
    } else if !tx.vin.is_empty() {
        if let Some(hash) = mined_in
            && let Some(full) = fetch.cached_v3(hash).await
            && let Some(entry) = full.tx.iter().find(|entry| entry.txid == txid)
        {
            spent = views::spent_from_v3(entry);
        }
        let parents = views::missing_parents(&tx, &spent);
        if !parents.is_empty() {
            let small_block = match mined_in {
                Some(hash) if !resolve && parents.len() > views::PARENTS_AT_ONCE => {
                    let block = fetch.block(BlockId::Hash(hash)).block().await.ok();
                    block.filter(|block| views::read_whole(block)).map(|_| hash)
                }
                _ => None,
            };
            if let Some(hash) = small_block {
                if let Some(entry) = fetch
                    .block_v3(hash)
                    .block_v3()
                    .await
                    .ok()
                    .and_then(|full| full.tx.iter().find(|entry| entry.txid == txid).cloned())
                {
                    spent = views::spent_from_v3(&entry);
                }
            } else {
                let cap = match (resolve, mined_in) {
                    (false, _) => views::PARENTS_AT_ONCE,
                    (true, Some(_)) => views::RESOLVED_PARENTS,
                    (true, None) => views::RESOLVED_WAITING_PARENTS,
                };
                resolvable = !resolve && parents.len() > cap;
                let later = fetch.in_lane(Lane::Prefetch);
                let reads: Vec<_> = parents
                    .iter()
                    .take(cap)
                    .enumerate()
                    .map(|(index, txid)| {
                        let fetch = if index < views::PARENTS_AT_ONCE {
                            fetch
                        } else {
                            &later
                        };
                        (*txid, fetch.tx(*txid))
                    })
                    .collect();
                let mut found = HashMap::new();
                for (txid, pending) in reads {
                    if let Ok(parent) = pending.tx().await {
                        found.insert(txid, parent);
                    }
                }
                views::fill_spent(&tx, &mut spent, |txid| found.get(txid).cloned());
            }
        }
    }
    let mut page = views::tx_page(&tx, &spent, rules_height, &context.heights);
    page.resolvable = resolvable && page.unresolved > 0;
    page.coinbase = claim;
    Ok(ExplorerResponse::Tx(Box::new(page)))
}

/// The height whose rules apply to `tx`: its block's, or for a waiting transaction the next
/// block's.
fn rules_height(context: &Context, tx: &Tx) -> u32 {
    match tx.height {
        Some(height) if height >= 0 => u32::try_from(height).unwrap_or(u32::MAX),
        _ => context
            .chain
            .map(|chain| chain.height)
            .or(context.node_height)
            .unwrap_or(0)
            .saturating_add(1),
    }
}

/// What the coinbase `tx` claimed, from its block (mined in `block`), the block's parent and
/// its subsidy: the reads the Block page makes, so after it they come from the cache.
async fn coinbase_claim(context: &Context, tx: &Tx, block: Option<BlockHash>) -> CoinbaseClaim {
    let unknown = CoinbaseClaim {
        subsidy: None,
        fees: None,
        burned: None,
        lockbox: None,
    };
    let fetch = &context.fetch;
    let Some(hash) = block else {
        return unknown;
    };
    let Ok(block) = fetch.block(BlockId::Hash(hash)).block().await else {
        return unknown;
    };
    let height = block.height.unwrap_or(0);
    let parent = block
        .previous
        .map(|previous| fetch.block(BlockId::Hash(previous)));
    let subsidy = fetch.subsidy(height);
    let parent = match parent {
        Some(pending) => pending.block().await.ok(),
        None => None,
    };
    let subsidy = subsidy
        .subsidy()
        .await
        .ok()
        .and_then(|subsidy| u64::try_from(zec_to_zat(subsidy.total)).ok());
    let ledger = ledger::block_ledger(&block, parent.as_deref(), Some(tx), &context.heights);
    views::coinbase_claim(&ledger, subsidy)
}

/// A hover card, from one read on the prefetch lane, which also fills the cache for the page.
async fn card(context: &Context, target: CardTarget) -> Result<Card, ExplorerError> {
    let fetch = context.fetch.in_lane(Lane::Prefetch);
    match target {
        CardTarget::Block { id } => Ok(views::block_card(&*fetch.block(id).block().await?)),
        CardTarget::Tx { txid } => {
            let tx = fetch.tx(txid).tx().await?;
            Ok(views::tx_card(
                &tx,
                rules_height(context, &tx),
                &context.heights,
            ))
        }
    }
}

/// Where a query leads. Keys and recovery phrases are refused without being looked at
/// further.
async fn resolve(context: &Context, query: &str) -> Result<Resolution, ExplorerError> {
    let go = |route| Resolution::Go { route };
    Ok(match classify_query(query, context.network) {
        Classified::Secret(what) => Resolution::Explain {
            hint: NotFoundHint::Refused { what },
        },
        Classified::Height(height) => go(ExplorerRoute::Block {
            id: BlockId::Height(height),
        }),
        Classified::BelowTip(back) => {
            let tip = context.fetch.tip(TIP_AGE).await?.height;
            go(ExplorerRoute::Block {
                id: BlockId::Height(tip.saturating_sub(back)),
            })
        }
        Classified::Hash(hash) => lookup(context, hash, None).await?,
        Classified::Outpoint(outpoint) => {
            lookup(context, outpoint.txid, Some(outpoint.vout)).await?
        }
        Classified::Route(route) => go(route),
        Classified::Later(later) => Resolution::Explain {
            hint: NotFoundHint::NotSearchable {
                what: later.label().to_owned(),
            },
        },
        Classified::Empty | Classified::Unrecognised => Resolution::Explain {
            hint: NotFoundHint::Unrecognised,
        },
    })
}

/// A hash: the block or the transaction the node has, or both. An output index rules the
/// block out.
async fn lookup(
    context: &Context,
    hash: BlockHash,
    out: Option<u32>,
) -> Result<Resolution, ExplorerError> {
    let block = out
        .is_none()
        .then(|| context.fetch.block(BlockId::Hash(hash)));
    let tx = context.fetch.tx(hash);
    let found_block = match block {
        Some(pending) => found(pending.block().await)?,
        None => false,
    };
    let found_tx = found(tx.tx().await)?;
    let block_route = ExplorerRoute::Block {
        id: BlockId::Hash(hash),
    };
    let tx_route = ExplorerRoute::Tx { txid: hash, out };
    Ok(match (found_block, found_tx) {
        (true, true) => Resolution::Choose {
            routes: vec![block_route, tx_route],
        },
        (true, false) => Resolution::Go { route: block_route },
        (false, true) => Resolution::Go { route: tx_route },
        (false, false) => Resolution::Explain {
            hint: NotFoundHint::UnknownHash,
        },
    })
}

/// Whether a read found its subject: `NotFound` is an answer, other failures are errors.
fn found<T>(result: Result<T, ExplorerError>) -> Result<bool, ExplorerError> {
    match result {
        Ok(_) => Ok(true),
        Err(err) if err.kind == ExplorerErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}
