//! One async function per request: what each page reads, asked for together so the reads go
//! out at once, and built with explorer-model's views.

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use explorer_model::{
    classify::{Classified, classify_query},
    shapes::BlockV1,
    upgrades::Upgrade,
    views, zec_to_zat,
};
use launcher_types::explorer::{
    BlockHash, BlockId, ChainPage, ChainView, ExplorerError, ExplorerErrorKind, ExplorerReply,
    ExplorerRequest, ExplorerResponse, ExplorerRoute, NotFoundHint, Resolution, TxId,
};

use super::actor::Context;

/// The rows of the Chain page.
pub(super) const RECENT_BLOCKS: u32 = 25;
/// The blocks the Chain page's average interval spans.
const INTERVAL_SPAN: u32 = 100;
/// How old a checked tip the Chain page and search accept.
const TIP_AGE: Duration = Duration::from_secs(2);

pub(super) async fn handle(
    request: ExplorerRequest,
    context: Context,
) -> Result<ExplorerReply, ExplorerError> {
    let (chain, response) = match request {
        ExplorerRequest::Chain => {
            let (chain, page) = chain_page(&context).await?;
            (Some(chain), ExplorerResponse::Chain(page))
        }
        ExplorerRequest::Block { id } => (context.chain, block(&context, id).await?),
        ExplorerRequest::BlockTxs {
            hash,
            offset,
            limit,
        } => {
            let block = context.fetch.block(BlockId::Hash(hash)).block().await?;
            (
                context.chain,
                ExplorerResponse::BlockTxs(views::txid_page(&block, offset, limit)),
            )
        }
        ExplorerRequest::Tx { txid } => (context.chain, tx(&context, txid).await?),
        ExplorerRequest::Resolve { query } => {
            let resolution = resolve(&context, &query).await?;
            (context.chain, ExplorerResponse::Resolve(resolution))
        }
        ExplorerRequest::Status | ExplorerRequest::Interest { .. } => {
            return Err(ExplorerError::new(
                ExplorerErrorKind::Internal,
                "the actor answers this request itself",
            ));
        }
    };
    Ok(ExplorerReply { chain, response })
}

/// The Chain page: the tip's 25 blocks with their ledgers, the average interval of the last
/// 100, the supply and what NU7 burned, and the next upgrade.
async fn chain_page(context: &Context) -> Result<(ChainView, ChainPage), ExplorerError> {
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
    let coinbases: Vec<_> = (oldest..=tip)
        .rev()
        .map(|height| blocks[&height].tx.first().map(|txid| fetch.tx(*txid)))
        .collect();
    let mut summaries = Vec::with_capacity(rows as usize);
    for (height, coinbase) in (oldest..=tip).rev().zip(coinbases) {
        let coinbase = match coinbase {
            Some(pending) => pending.tx().await.ok(),
            None => None,
        };
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

/// The Transaction page.
async fn tx(context: &Context, txid: TxId) -> Result<ExplorerResponse, ExplorerError> {
    let tx = match context.fetch.tx(txid).tx().await {
        Ok(tx) => tx,
        Err(err) if err.kind == ExplorerErrorKind::NotFound => {
            return Err(ExplorerError::new(
                ExplorerErrorKind::NotFound,
                "Not in your node's chain or mempool; it only knows transactions it has received",
            ));
        }
        Err(err) => return Err(err),
    };
    // A mined transaction follows its block's rules; a waiting one the next block's.
    let rules_height = match tx.height {
        Some(height) if height >= 0 => u32::try_from(height).unwrap_or(u32::MAX),
        _ => context
            .chain
            .map(|chain| chain.height)
            .or(context.node_height)
            .unwrap_or(0)
            .saturating_add(1),
    };
    Ok(ExplorerResponse::Tx(Box::new(views::tx_page(
        &tx,
        rules_height,
        &context.heights,
    ))))
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
