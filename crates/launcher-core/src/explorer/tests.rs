//! The explorer actor over a synthetic chain: what each page costs the node, merging and the
//! cache, the node and network rules, a reorganisation followed at the tip, the time limits at
//! their own values on Tokio's paused clock, and logs that never carry what was asked for.

use std::{
    collections::HashMap,
    net::{Ipv4Addr, SocketAddr},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, SystemTime},
};

use explorer_model::{
    shapes::{Count, HexLen, Prevout, ScriptPubKey, ScriptSig, Tx, TxIn, TxOut},
    synth::SynthChain,
    zec_to_zat,
};
use launcher_types::{
    EosInfo, Network, NodeDetails, NodeId, NodeStats, Phase, Status,
    explorer::{
        BlockHash, BlockId, Card, CardTarget, ChainPage, ExplorerCall, ExplorerErrorKind,
        ExplorerLive, ExplorerReply, ExplorerRequest, ExplorerResponse, ExplorerRoute, Hash32,
        LedgerCheck, LiveState, NotFoundHint, PageKind, PrefetchTarget, Resolution, RowsRead,
        SecretKind, Shown, TxId, TxStatus, UnknownWhy,
    },
};
use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::watch;

use crate::nodes::NodeSpec;

use super::{
    ExplorerConfig, ExplorerHandle, ExplorerInputs,
    source::{Answer, ChainSource, ReadCall, ReadResult, SourceError},
    spawn_with,
};

const NOW: i64 = 1_800_000_000;
const TIP: u32 = 3_551_000;
const LIMIT: Duration = Duration::from_secs(10);

/// Calls per method, in [`slot`]'s order.
type Counts = Arc<[AtomicU64; 6]>;

/// The slot of `call` in [`Counts`]; a block at verbosity 3 is counted on its own
/// ([`Synth::v3`]).
fn slot(call: ReadCall) -> Option<usize> {
    match call {
        ReadCall::Best => Some(0),
        ReadCall::Block(_) => Some(1),
        ReadCall::BlockHash(_) => Some(2),
        ReadCall::Header(_) => Some(3),
        ReadCall::Tx(_) => Some(4),
        ReadCall::Subsidy(_) => Some(5),
        ReadCall::BlockV3(_) => None,
    }
}

/// A source over a synthetic chain the test can mine and reorganise, that counts its calls and
/// holds them while its gate is closed. Every answer goes through zebrad's JSON and back, as
/// an answer from zebrad's RPC is decoded.
#[derive(Clone)]
struct Synth {
    chain: watch::Receiver<SynthChain>,
    gate: watch::Receiver<bool>,
    /// Holds transaction reads only.
    tx_gate: watch::Receiver<bool>,
    /// Holds subsidy reads only.
    subsidy_gate: watch::Receiver<bool>,
    counts: Counts,
    /// Blocks read at verbosity 3.
    v3: Arc<AtomicU64>,
    /// Each read as it starts, in order.
    started: std::sync::mpsc::Sender<ReadCall>,
    /// Transactions the chain does not have, read before its own and added to their blocks
    /// at verbosity 3.
    extra: watch::Receiver<HashMap<TxId, Tx>>,
    /// Blocks at verbosity 3 are not answered.
    fail_v3: Arc<AtomicBool>,
}

/// The transactions of `extra` mined in block `hash`.
fn in_block(extra: &HashMap<TxId, Tx>, hash: BlockHash) -> Vec<&Tx> {
    let mut found: Vec<&Tx> = extra
        .values()
        .filter(|tx| tx.block_hash == Some(hash))
        .collect();
    found.sort_by_key(|tx| tx.txid.to_hex());
    found
}

/// The output `input` spends, as verbosity 3 describes it, from `extra` or the chain.
fn spent(extra: &HashMap<TxId, Tx>, chain: &SynthChain, input: &TxIn) -> Option<Prevout> {
    let txid = input.txid?;
    let parent = extra.get(&txid).cloned().or_else(|| chain.tx(&txid))?;
    let output = parent
        .vout
        .iter()
        .find(|output| Some(output.n) == input.vout)?;
    Some(Prevout {
        generated: parent.is_coinbase(),
        height: parent
            .height
            .and_then(|height| u32::try_from(height).ok())?,
        value_zat: output.value_zat,
        script: output.script.clone(),
    })
}

/// `value` as zebrad writes it and the explorer reads it back.
fn through_json<T: Serialize + DeserializeOwned>(value: T) -> T {
    serde_json::from_value(serde_json::to_value(value).expect("serializes")).expect("decodes")
}

/// The size of `result` as zebrad's JSON.
fn json_size(result: &ReadResult) -> Option<u32> {
    let bytes = match result {
        ReadResult::Best(best) => serde_json::to_vec(best),
        ReadResult::Block(block) => serde_json::to_vec(&**block),
        ReadResult::BlockV3(block) => serde_json::to_vec(&**block),
        ReadResult::BlockHash(hash) => serde_json::to_vec(hash),
        ReadResult::Header(header) => serde_json::to_vec(header),
        ReadResult::Tx(tx) => serde_json::to_vec(&**tx),
        ReadResult::Subsidy(subsidy) => serde_json::to_vec(&**subsidy),
    };
    bytes.ok().and_then(|bytes| u32::try_from(bytes.len()).ok())
}

impl ChainSource for Synth {
    async fn read(&self, call: ReadCall) -> Result<Answer, SourceError> {
        match slot(call) {
            Some(slot) => self.counts[slot].fetch_add(1, Ordering::SeqCst),
            None => self.v3.fetch_add(1, Ordering::SeqCst),
        };
        let _ = self.started.send(call);
        let mut gate = self.gate.clone();
        let _ = gate.wait_for(|open| *open).await;
        let held = match call {
            ReadCall::Tx(_) => Some(&self.tx_gate),
            ReadCall::Subsidy(_) => Some(&self.subsidy_gate),
            _ => None,
        };
        if let Some(gate) = held {
            let _ = gate.clone().wait_for(|open| *open).await;
        }
        let chain = self.chain.borrow().clone();
        let extra = self.extra.borrow().clone();
        if let ReadCall::Tx(txid) = call
            && let Some(tx) = extra.get(&txid)
        {
            let result = ReadResult::Tx(Arc::new(through_json(tx.clone())));
            return Ok(Answer {
                bytes: json_size(&result),
                result,
            });
        }
        if matches!(call, ReadCall::BlockV3(_)) && self.fail_v3.load(Ordering::SeqCst) {
            return Err(SourceError::Timeout);
        }
        let found = match call {
            ReadCall::Best => Some(ReadResult::Best(through_json(chain.best()))),
            ReadCall::Block(id) => chain
                .block(id)
                .map(|block| ReadResult::Block(Arc::new(through_json(block)))),
            ReadCall::BlockHash(height) => chain.hash(height).map(ReadResult::BlockHash),
            ReadCall::Header(id) => chain
                .header(id)
                .map(|header| ReadResult::Header(through_json(header))),
            ReadCall::Tx(txid) => chain
                .tx(&txid)
                .map(|tx| ReadResult::Tx(Arc::new(through_json(tx)))),
            ReadCall::Subsidy(height) => Some(ReadResult::Subsidy(Arc::new(through_json(
                chain.subsidy(height),
            )))),
            ReadCall::BlockV3(hash) => chain.block_v3(BlockId::Hash(hash)).map(|mut block| {
                block
                    .tx
                    .extend(in_block(&extra, hash).into_iter().cloned().map(|mut tx| {
                        let mut paid = tx.transparent_out();
                        for input in &mut tx.vin {
                            input.prevout = spent(&extra, &chain, input);
                            paid -= input
                                .prevout
                                .as_ref()
                                .map_or(0, |prevout| prevout.value_zat);
                        }
                        tx.fee = Some(-paid);
                        tx
                    }));
                ReadResult::BlockV3(Arc::new(through_json(block)))
            }),
        };
        let result = found.ok_or(SourceError::NotFound)?;
        Ok(Answer {
            bytes: json_size(&result),
            result,
        })
    }
}

struct Harness {
    explorer: ExplorerHandle,
    chain: watch::Sender<SynthChain>,
    gate: watch::Sender<bool>,
    tx_gate: watch::Sender<bool>,
    subsidy_gate: watch::Sender<bool>,
    counts: Counts,
    v3: Arc<AtomicU64>,
    started: std::sync::mpsc::Receiver<ReadCall>,
    extra: watch::Sender<HashMap<TxId, Tx>>,
    fail_v3: Arc<AtomicBool>,
    status: watch::Sender<Status>,
    stats: watch::Sender<Option<NodeStats>>,
    details: watch::Sender<Option<NodeDetails>>,
    spec: watch::Sender<NodeSpec>,
}

fn stats_for(chain: &SynthChain) -> NodeStats {
    NodeStats {
        best: None,
        height: u64::from(chain.tip()),
        estimated_height: u64::from(chain.tip()),
        verification_progress: 1.0,
        peers: 8,
        mempool_transactions: 0,
        chain: "main".into(),
        zebrad_version: "7.0.0".into(),
        last_error: None,
        eos: Some(EosInfo {
            block_height: 3_700_000,
            estimated_time: SystemTime::UNIX_EPOCH,
        }),
        updated_at: SystemTime::UNIX_EPOCH,
    }
}

fn running() -> Status {
    Status {
        phase: Phase::Running,
        rpc_addr: Some(SocketAddr::from((Ipv4Addr::LOCALHOST, 8232))),
        started_at: Some(SystemTime::UNIX_EPOCH),
        ..Status::default()
    }
}

fn harness_with(chain: SynthChain, config: impl FnOnce(&mut ExplorerConfig)) -> Harness {
    let stats = stats_for(&chain);
    let details = NodeDetails {
        peers: Vec::new(),
        best_block_hash: None,
        headers: None,
        difficulty: None,
        size_on_disk: None,
        chain_supply_zec: None,
        value_pools: Vec::new(),
        active_upgrade: None,
        upgrades: chain.upgrades(chain.tip()),
        consensus: None,
        subversion: None,
        protocol_version: None,
        mempool_bytes: None,
        mempool_usage: None,
        updated_at: SystemTime::UNIX_EPOCH,
    };
    let (chain_tx, chain_rx) = watch::channel(chain);
    let (gate_tx, gate_rx) = watch::channel(true);
    let (tx_gate_tx, tx_gate_rx) = watch::channel(true);
    let (subsidy_gate_tx, subsidy_gate_rx) = watch::channel(true);
    let counts: Counts = Arc::new(std::array::from_fn(|_| AtomicU64::new(0)));
    let v3 = Arc::new(AtomicU64::new(0));
    let (started_tx, started) = std::sync::mpsc::channel();
    let (extra_tx, extra) = watch::channel(HashMap::new());
    let fail_v3 = Arc::new(AtomicBool::new(false));
    let (status_tx, status) = watch::channel(running());
    let (stats_tx, stats) = watch::channel(Some(stats));
    let (details_tx, details) = watch::channel(Some(details));
    let (spec_tx, spec) = watch::channel(NodeSpec::first(Network::Mainnet));
    let source = Synth {
        chain: chain_rx,
        gate: gate_rx,
        tx_gate: tx_gate_rx,
        subsidy_gate: subsidy_gate_rx,
        counts: counts.clone(),
        v3: v3.clone(),
        started: started_tx,
        extra,
        fail_v3: fail_v3.clone(),
    };
    let mut explorer_config = ExplorerConfig::new(PathBuf::from("/nonexistent/.cookie"));
    config(&mut explorer_config);
    let explorer = spawn_with(
        ExplorerInputs {
            status,
            stats,
            details,
            spec,
        },
        explorer_config,
        move |_, _| Ok(source.clone()),
    );
    Harness {
        explorer,
        chain: chain_tx,
        gate: gate_tx,
        tx_gate: tx_gate_tx,
        subsidy_gate: subsidy_gate_tx,
        counts,
        v3,
        started,
        extra: extra_tx,
        fail_v3,
        status: status_tx,
        stats: stats_tx,
        details: details_tx,
        spec: spec_tx,
    }
}

fn harness() -> Harness {
    harness_with(SynthChain::new(Network::Mainnet, TIP, NOW), |_| {})
}

fn mainnet() -> NodeId {
    NodeId::new("zebrad-mainnet").unwrap()
}

impl Harness {
    async fn ask(
        &self,
        request: ExplorerRequest,
    ) -> Result<ExplorerReply, launcher_types::explorer::ExplorerError> {
        tokio::time::timeout(
            LIMIT,
            self.explorer.call(&mainnet(), ExplorerCall::new(request)),
        )
        .await
        .expect("the explorer answers in time")
    }

    async fn reply(&self, request: ExplorerRequest) -> ExplorerResponse {
        self.ask(request).await.expect("an answer").response
    }

    /// Calls so far: best, block, blockhash, header, tx, subsidy.
    fn counts(&self) -> [u64; 6] {
        std::array::from_fn(|index| self.counts[index].load(Ordering::SeqCst))
    }

    /// Blocks read at verbosity 3 so far.
    fn v3_reads(&self) -> u64 {
        self.v3.load(Ordering::SeqCst)
    }

    /// The reads started since the last look, in the order they started.
    fn started(&self) -> Vec<ReadCall> {
        self.started.try_iter().collect()
    }
}

fn delta(after: [u64; 6], before: [u64; 6]) -> [u64; 6] {
    std::array::from_fn(|index| after[index] - before[index])
}

/// A transaction of the block at `height`, not its coinbase, with transparent inputs or
/// without.
fn txid_where(chain: &SynthChain, height: u32, inputs: bool) -> TxId {
    let block = chain.block(BlockId::Height(height)).expect("the block");
    block
        .tx
        .iter()
        .skip(1)
        .copied()
        .find(|txid| chain.tx(txid).expect("its transaction").vin.is_empty() != inputs)
        .expect("such a transaction")
}

/// The Chain page asked for as the UI asks: first without waiting for the coinbases, then
/// with them.
async fn chain_pages(harness: &Harness) -> (ChainPage, ChainPage) {
    let ExplorerResponse::Chain(first) = harness
        .reply(ExplorerRequest::Chain { with_fees: false })
        .await
    else {
        panic!("a chain page");
    };
    let ExplorerResponse::Chain(full) = harness
        .reply(ExplorerRequest::Chain { with_fees: true })
        .await
    else {
        panic!("a chain page");
    };
    (first, full)
}

#[tokio::test]
async fn the_chain_page_stays_within_its_budget_and_a_revisit_is_free() {
    let harness = harness();
    // The first answer has the blocks; their coinbases are on their way.
    let (first, page) = chain_pages(&harness).await;
    assert!(first.fees_pending);
    assert!(first.blocks.iter().all(|block| block.ledger.fees.is_none()));
    assert_eq!(
        first
            .blocks
            .iter()
            .map(|block| block.height)
            .collect::<Vec<_>>(),
        page.blocks
            .iter()
            .map(|block| block.height)
            .collect::<Vec<_>>()
    );
    assert!(!page.fees_pending);
    assert_eq!(page.blocks.len(), 25);
    assert_eq!(page.blocks[0].height, TIP, "newest first");
    assert_eq!(page.blocks[24].height, TIP - 24);
    assert!(page.blocks.iter().all(|block| block.ledger.fees.is_some()));
    assert!(page.average_interval.is_some());
    assert!(page.burned_since_nu7.is_some(), "the tip is past NU7");
    assert_eq!(page.target_spacing, 25);
    let [best, blocks, hashes, headers, txs, subsidies] = harness.counts();
    assert!(
        best <= 2,
        "a tip check, and one more if the first is 2 s old: {best}"
    );
    assert!(blocks <= 27, "26 blocks and the one before NU7: {blocks}");
    assert_eq!(hashes, 0);
    assert_eq!(headers, 1);
    assert_eq!(txs, 25, "one coinbase per row, read once");
    assert!(subsidies <= 2);
    // Again, a moment later: everything from the cache, the fees too, at once.
    let before = harness.counts();
    let ExplorerResponse::Chain(again) = harness
        .reply(ExplorerRequest::Chain { with_fees: false })
        .await
    else {
        panic!("a chain page");
    };
    assert_eq!(again, page);
    assert_eq!(delta(harness.counts(), before), [0; 6]);
}

#[tokio::test(start_paused = true)]
async fn a_page_goes_before_the_chain_pages_coinbases() {
    let harness = harness();
    // A transaction without transparent inputs: its page is one read.
    let txid = txid_where(&harness.chain.borrow(), TIP - 3_000, false);
    // The Chain page answers; its 25 coinbases wait, two of them read, the rest queued.
    harness.tx_gate.send_replace(false);
    let ExplorerResponse::Chain(first) = harness
        .reply(ExplorerRequest::Chain { with_fees: false })
        .await
    else {
        panic!("a chain page");
    };
    assert!(first.fees_pending);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(harness.counts()[4], 2, "two prefetches at a time");
    // A Transaction page's read starts at once, ahead of the 23 queued coinbases.
    let explorer = harness.explorer.clone();
    let page = tokio::spawn(async move {
        explorer
            .call(
                &mainnet(),
                ExplorerCall::new(ExplorerRequest::Tx {
                    txid,
                    resolve: false,
                }),
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(harness.counts()[4], 3, "the page's read is under way");
    // Once the Chain page's check of the tip is too old for a page, the pages that need the
    // tip check it again ahead of the coinbases too.
    tokio::time::sleep(Duration::from_secs(3)).await;
    let before = harness.counts();
    let ExplorerResponse::Future(future) = harness
        .reply(ExplorerRequest::Block {
            id: BlockId::Height(TIP + 100),
        })
        .await
    else {
        panic!("a countdown");
    };
    assert_eq!(future.blocks_left, 100);
    assert_eq!(delta(harness.counts(), before)[0], 1, "the tip was checked");
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(
        harness
            .reply(ExplorerRequest::Resolve {
                query: "tip-5".into()
            })
            .await,
        ExplorerResponse::Resolve(Resolution::Go {
            route: ExplorerRoute::Block {
                id: BlockId::Height(TIP - 5)
            }
        })
    );
    // And so does the follower, once the Chain page is shown.
    harness.change_chain(|chain| chain.mine(1));
    harness.show(PageKind::Chain).await;
    let mut live = harness.explorer.live();
    tokio::time::timeout(
        LIMIT,
        live.wait_for(|event| event.live.tip.is_some_and(|tip| tip.height == TIP + 1)),
    )
    .await
    .expect("the follower moved on")
    .expect("the explorer runs");
    assert_eq!(harness.counts()[4], 3, "the coinbases are still held");
    harness.tx_gate.send_replace(true);
    assert!(matches!(
        page.await.expect("no panic").expect("a page").response,
        ExplorerResponse::Tx(_)
    ));
    let ExplorerResponse::Chain(full) = harness
        .reply(ExplorerRequest::Chain { with_fees: true })
        .await
    else {
        panic!("a chain page");
    };
    assert_eq!(full.blocks[0].height, TIP + 1);
    assert!(full.blocks.iter().all(|block| block.ledger.fees.is_some()));
    assert_eq!(
        harness.counts()[4],
        27,
        "the first page's 25 coinbases, the new tip's and the page's transaction"
    );
}

#[tokio::test]
async fn the_tip_goes_out_with_its_time() {
    let harness = harness();
    let ExplorerResponse::Chain(page) = harness
        .reply(ExplorerRequest::Chain { with_fees: true })
        .await
    else {
        panic!("a chain page");
    };
    let mut live = harness.explorer.live();
    let event = tokio::time::timeout(LIMIT, live.wait_for(|event| event.live.tip_time.is_some()))
        .await
        .expect("the tip's time goes out")
        .expect("the explorer runs")
        .clone();
    assert_eq!(event.live.tip.map(|tip| tip.height), Some(TIP));
    assert_eq!(event.live.tip_time, Some(page.blocks[0].time));
}

#[tokio::test]
async fn a_block_page_reads_two_blocks_its_coinbase_and_its_subsidy() {
    let harness = harness();
    let height = TIP - 2_000;
    let ExplorerResponse::Block(page) = harness
        .reply(ExplorerRequest::Block {
            id: BlockId::Height(height),
        })
        .await
    else {
        panic!("a block page");
    };
    assert_eq!(page.summary.height, height);
    assert_eq!(page.check, LedgerCheck::Balanced);
    assert_eq!(page.era.as_deref(), Some("NU7"));
    assert!(page.reward.is_some());
    assert!(!page.txids.txids.is_empty());
    assert_eq!(harness.counts(), [0, 2, 0, 0, 1, 1]);
    // The same block by hash, and its transaction ids: no more calls.
    let before = harness.counts();
    let ExplorerResponse::Block(by_hash) = harness
        .reply(ExplorerRequest::Block {
            id: BlockId::Hash(page.summary.hash),
        })
        .await
    else {
        panic!("a block page");
    };
    assert_eq!(by_hash, page);
    assert_eq!(delta(harness.counts(), before), [0; 6]);
}

#[tokio::test]
async fn the_genesis_block_has_no_parent_to_read() {
    // zebrad writes the genesis block's parent as 64 zeros, which no block has.
    let harness = harness_with(SynthChain::new(Network::Mainnet, 9, NOW), |_| {});
    let ExplorerResponse::Block(page) = harness
        .reply(ExplorerRequest::Block {
            id: BlockId::Height(0),
        })
        .await
    else {
        panic!("a block page");
    };
    assert_eq!(page.summary.height, 0);
    assert_eq!(page.previous, None);
    assert_eq!(harness.counts(), [0, 1, 0, 0, 1, 1], "one block, no parent");
}

#[tokio::test]
async fn reads_of_one_block_merge() {
    let harness = harness();
    harness.gate.send_replace(false);
    let id = BlockId::Height(TIP - 5_000);
    let first = harness.ask(ExplorerRequest::Block { id });
    let second = harness.ask(ExplorerRequest::Block { id });
    let release = async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        harness.gate.send_replace(true);
    };
    let (first, second, ()) = tokio::join!(first, second, release);
    assert_eq!(first.unwrap().response, second.unwrap().response);
    assert_eq!(
        harness.counts(),
        [0, 2, 0, 0, 1, 1],
        "two pages, one set of reads"
    );
}

#[tokio::test]
async fn a_transaction_without_transparent_inputs_is_one_read() {
    let harness = harness();
    let txid = txid_where(&harness.chain.borrow(), TIP - 10, false);
    let ExplorerResponse::Tx(page) = harness
        .reply(ExplorerRequest::Tx {
            txid,
            resolve: false,
        })
        .await
    else {
        panic!("a transaction page");
    };
    assert_eq!(page.txid, txid);
    assert!(matches!(page.status, TxStatus::Mined { height, .. } if height == TIP - 10));
    assert_eq!(harness.counts(), [0, 0, 0, 0, 1, 0]);
    let unknown = harness
        .ask(ExplorerRequest::Tx {
            txid: Hash32([7; 32]),
            resolve: false,
        })
        .await
        .expect_err("unknown");
    assert_eq!(unknown.kind, ExplorerErrorKind::NotFound);
}

#[tokio::test]
async fn a_height_above_the_tip_counts_down() {
    let harness = harness();
    let ExplorerResponse::Future(future) = harness
        .reply(ExplorerRequest::Block {
            id: BlockId::Height(TIP + 10),
        })
        .await
    else {
        panic!("a countdown");
    };
    assert_eq!(future.blocks_left, 10);
    assert_eq!(future.eta_secs, 250);
    assert_eq!(future.end_of_support, Some(3_700_000));
    // While the node syncs, a height it has not reached yet is past its tip, not the future.
    harness.stats.send_modify(|stats| {
        if let Some(stats) = stats {
            stats.estimated_height = u64::from(TIP) + 100;
            stats.verification_progress = 0.99;
        }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let err = harness
        .ask(ExplorerRequest::Block {
            id: BlockId::Height(TIP + 10),
        })
        .await
        .expect_err("past the tip");
    assert_eq!(
        err.kind,
        ExplorerErrorKind::PastTip {
            height: TIP + 10,
            node_height: TIP
        }
    );
    // Past the network's tip, the count starts there, not at the node's.
    let ExplorerResponse::Future(future) = harness
        .reply(ExplorerRequest::Block {
            id: BlockId::Height(TIP + 110),
        })
        .await
    else {
        panic!("a countdown");
    };
    assert_eq!((future.blocks_left, future.tip), (10, TIP));
}

#[tokio::test]
async fn search_resolves_heights_hashes_and_refuses_keys() {
    let harness = harness();
    let (block_hash, txid) = {
        let chain = harness.chain.borrow();
        let block = chain.block(BlockId::Height(TIP - 3)).unwrap();
        (block.hash, block.tx[0])
    };
    let resolve = |query: String| harness.reply(ExplorerRequest::Resolve { query });
    let route = |response| match response {
        ExplorerResponse::Resolve(resolution) => resolution,
        other => panic!("a resolution: {other:?}"),
    };
    assert_eq!(
        route(resolve("3,120,000".into()).await),
        Resolution::Go {
            route: ExplorerRoute::Block {
                id: BlockId::Height(3_120_000)
            }
        }
    );
    assert_eq!(
        route(resolve(block_hash.to_hex()).await),
        Resolution::Go {
            route: ExplorerRoute::Block {
                id: BlockId::Hash(block_hash)
            }
        }
    );
    assert_eq!(
        route(resolve(txid.to_hex()).await),
        Resolution::Go {
            route: ExplorerRoute::Tx { txid, out: None }
        }
    );
    assert_eq!(
        route(resolve(format!("{}:0", txid.to_hex())).await),
        Resolution::Go {
            route: ExplorerRoute::Tx { txid, out: Some(0) }
        }
    );
    assert_eq!(
        route(resolve("ab".repeat(32)).await),
        Resolution::Explain {
            hint: NotFoundHint::UnknownHash
        }
    );
    assert_eq!(
        route(resolve("tip-3".into()).await),
        Resolution::Go {
            route: ExplorerRoute::Block {
                id: BlockId::Height(TIP - 3)
            }
        }
    );
    assert_eq!(
        route(resolve("zxviews1qqqq".into()).await),
        Resolution::Explain {
            hint: NotFoundHint::Refused {
                what: SecretKind::ViewingKey
            }
        }
    );
    assert!(matches!(
        route(resolve(format!("t1{}", "a".repeat(33))).await),
        Resolution::Explain {
            hint: NotFoundHint::NotSearchable { .. }
        }
    ));
}

#[tokio::test]
async fn the_first_node_answers_by_its_own_id_on_its_own_network() {
    let harness = harness();
    let call = |node: &'static str| {
        let explorer = harness.explorer.clone();
        async move {
            explorer
                .call(
                    &NodeId::new(node).unwrap(),
                    ExplorerCall::new(ExplorerRequest::Chain { with_fees: true }),
                )
                .await
        }
    };
    // Other nodes get their explorers in a later phase.
    for other in ["zebrad-testnet", "zebrad-mainnet-2", "zakura-mainnet"] {
        let err = call(other).await.expect_err("another node");
        assert_eq!(err.kind, ExplorerErrorKind::UnknownNode, "{other}");
        assert!(
            err.message.contains("first when Zeal started"),
            "{}",
            err.message
        );
    }

    harness.status.send_replace(Status::default());
    tokio::time::sleep(Duration::from_millis(20)).await;
    let stopped = harness
        .ask(ExplorerRequest::Chain { with_fees: true })
        .await
        .expect_err("stopped");
    assert_eq!(stopped.kind, ExplorerErrorKind::NodeNotRunning);
    let ExplorerResponse::Status(live) = harness.reply(ExplorerRequest::Status).await else {
        panic!("a status");
    };
    assert_eq!(live.state, LiveState::NotRunning);
    assert_eq!(harness.counts(), [0; 6], "nothing was read");
    harness.status.send_replace(running());
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(
        harness
            .ask(ExplorerRequest::Chain { with_fees: true })
            .await
            .is_ok()
    );

    // After its first run a node keeps its id when its network changes, and zebrad keeps
    // running on the old network until its next start: the node's page reads nothing of it.
    harness
        .spec
        .send_modify(|spec| spec.network = Network::Testnet);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let before = harness.counts();
    let waiting = harness
        .ask(ExplorerRequest::Chain { with_fees: true })
        .await
        .expect_err("zebrad still on Mainnet");
    assert_eq!(waiting.kind, ExplorerErrorKind::NodeNotRunning);
    assert!(
        waiting.message.contains("next start"),
        "{}",
        waiting.message
    );
    let ExplorerResponse::Status(live) = harness.reply(ExplorerRequest::Status).await else {
        panic!("a status");
    };
    assert_eq!(
        live,
        ExplorerLive {
            state: LiveState::OtherNetwork {
                network: Network::Mainnet
            },
            tip: None,
            tip_time: None,
            reorg: None,
        },
        "no Mainnet tip under a Testnet node"
    );
    assert_eq!(delta(harness.counts(), before), [0; 6]);

    // zebrad starts again on Testnet: the same id answers, from the Testnet chain.
    harness
        .chain
        .send_replace(SynthChain::new(Network::Testnet, 4_500_000, NOW));
    harness.stats.send_modify(|stats| {
        if let Some(stats) = stats {
            stats.chain = "test".into();
            stats.height = 4_500_000;
            stats.estimated_height = 4_500_000;
        }
    });
    harness.status.send_replace(running_since(100));
    tokio::time::sleep(Duration::from_millis(20)).await;
    let ExplorerResponse::Chain(page) = harness
        .reply(ExplorerRequest::Chain { with_fees: true })
        .await
    else {
        panic!("a chain page");
    };
    assert_eq!(page.blocks[0].height, 4_500_000);
    let event = harness.explorer.live().borrow().clone();
    assert_eq!(event.node, mainnet(), "the node's own id");
    assert_eq!(event.live.state, LiveState::AtTip);

    // A first run that renames the node moves the explorer to the new id.
    harness
        .spec
        .send_modify(|spec| spec.id = NodeId::new("zebrad-testnet").unwrap());
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(call("zebrad-testnet").await.is_ok());
    assert_eq!(
        harness.explorer.live().borrow().node.as_str(),
        "zebrad-testnet"
    );
}

impl Harness {
    /// zebrad stops, and the monitor clears what it read.
    async fn stop(&self) {
        self.status.send_replace(Status::default());
        self.stats.send_replace(None);
        self.details.send_replace(None);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    async fn live(&self) -> ExplorerLive {
        let ExplorerResponse::Status(live) = self.reply(ExplorerRequest::Status).await else {
            panic!("a status");
        };
        live
    }
}

#[tokio::test]
async fn a_network_changed_while_zebrad_is_stopped_leaves_nothing_of_the_old_chain() {
    let harness = harness();
    chain_pages(&harness).await;
    harness.stop().await;
    let live = harness.live().await;
    assert!(
        live.tip.is_some_and(|tip| tip.stale),
        "a stopped node keeps its last tip: {live:?}"
    );

    // The node moves to Testnet while zebrad is stopped.
    harness
        .spec
        .send_modify(|spec| spec.network = Network::Testnet);
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(
        harness.live().await,
        ExplorerLive::default(),
        "no Mainnet tip under a Testnet node"
    );

    // zebrad starts on Testnet, and the monitor has not read it yet: the pages read Testnet,
    // with Testnet's upgrade heights.
    harness
        .chain
        .send_replace(SynthChain::new(Network::Testnet, 3_400_000, NOW));
    harness.status.send_replace(running_since(100));
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(harness.live().await, ExplorerLive::default());
    let (_, page) = chain_pages(&harness).await;
    assert_eq!(page.blocks[0].height, 3_400_000);
    assert_eq!(
        page.next_upgrade.map(|next| next.height),
        Some(3_536_500),
        "Testnet's next upgrade, not Mainnet's at 3,428,143"
    );
    let live = harness.explorer.live().borrow().live.clone();
    assert_eq!(
        live.tip.map(|tip| (tip.height, tip.epoch)),
        Some((3_400_000, 0)),
        "a new index, not Mainnet's reorganised"
    );
    assert_eq!(live.reorg, None);
}

#[tokio::test]
async fn zebrad_stopped_on_the_old_network_leaves_nothing_of_it() {
    let harness = harness();
    chain_pages(&harness).await;
    harness
        .spec
        .send_modify(|spec| spec.network = Network::Testnet);
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(
        harness.live().await.state,
        LiveState::OtherNetwork {
            network: Network::Mainnet
        }
    );
    // zebrad stops; for a moment the monitor's last statistics still say Mainnet.
    harness.status.send_replace(Status::default());
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(harness.live().await, ExplorerLive::default());
    harness.stop().await;
    assert_eq!(harness.live().await, ExplorerLive::default());
    // zebrad starts again, still before the monitor reads it.
    harness.status.send_replace(running_since(100));
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(harness.live().await, ExplorerLive::default());
}

#[tokio::test]
async fn an_answer_from_the_old_network_is_dropped() {
    let height = 600_000;
    let testnet = SynthChain::new(Network::Testnet, 3_400_000, NOW);
    let testnet_subsidy = zec_to_zat(testnet.subsidy(height).total);
    let harness = harness();
    assert_ne!(
        zec_to_zat(harness.chain.borrow().subsidy(height).total),
        testnet_subsidy,
        "the networks' subsidies differ at {height}"
    );
    // A Mainnet Block page whose subsidy read zebrad holds.
    harness.subsidy_gate.send_replace(false);
    let explorer = harness.explorer.clone();
    let old_page = tokio::spawn(async move {
        explorer
            .call(
                &mainnet(),
                ExplorerCall::new(ExplorerRequest::Block {
                    id: BlockId::Height(height),
                }),
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(harness.counts()[5], 1, "the subsidy read is under way");
    // The node moves to Testnet while zebrad runs on Mainnet; zebrad stops, and the monitor
    // clears its statistics.
    harness
        .spec
        .send_modify(|spec| spec.network = Network::Testnet);
    tokio::time::sleep(Duration::from_millis(20)).await;
    harness.stop().await;
    // zebrad answers the old read late, with Mainnet's subsidy.
    harness.subsidy_gate.send_replace(true);
    let old_page = tokio::time::timeout(LIMIT, old_page)
        .await
        .expect("the old page is answered")
        .expect("no panic");
    // zebrad starts on Testnet, and the monitor reads it.
    harness.chain.send_replace(testnet.clone());
    harness.stats.send_replace(Some(NodeStats {
        chain: "test".into(),
        ..stats_for(&testnet)
    }));
    harness.status.send_replace(running_since(100));
    tokio::time::sleep(Duration::from_millis(20)).await;
    let before = harness.counts();
    let ExplorerResponse::Block(page) = harness
        .reply(ExplorerRequest::Block {
            id: BlockId::Height(height),
        })
        .await
    else {
        panic!("a block page");
    };
    assert_eq!(
        delta(harness.counts(), before)[5],
        1,
        "Testnet's subsidy is read from zebrad"
    );
    assert_eq!(
        page.reward
            .and_then(|reward| i64::try_from(reward.subsidy).ok()),
        Some(testnet_subsidy),
        "Testnet's subsidy, not Mainnet's"
    );
    // The old page went without the subsidy: its read failed when the network changed.
    assert!(
        old_page.is_ok_and(|reply| matches!(
            reply.response,
            ExplorerResponse::Block(page) if page.reward.is_none()
        )),
        "the old page went without its subsidy"
    );
}

#[tokio::test]
async fn a_reorganisation_at_the_tip_is_followed_once() {
    let harness = harness_with(SynthChain::new(Network::Mainnet, TIP, NOW), |config| {
        config.tip_poll = Duration::from_millis(30);
    });
    let ExplorerResponse::Chain(_) = harness
        .reply(ExplorerRequest::Chain { with_fees: true })
        .await
    else {
        panic!("a chain page");
    };
    harness
        .reply(ExplorerRequest::Interest {
            visible: true,
            page: PageKind::Chain,
        })
        .await;
    let mut live = harness.explorer.live();
    let before = harness.counts();
    // The node replaces its top two blocks and mines one more on the new branch.
    harness.chain.send_modify(|chain| {
        chain.reorg(2);
        chain.mine(1);
    });
    harness.stats.send_modify(|stats| {
        if let Some(stats) = stats {
            stats.height += 1;
            stats.estimated_height += 1;
        }
    });
    let event = tokio::time::timeout(LIMIT, live.wait_for(|event| event.live.reorg.is_some()))
        .await
        .expect("the reorganisation is noticed")
        .expect("the explorer runs")
        .clone();
    let reorg = event.live.reorg.unwrap();
    assert_eq!(reorg.fork_height, TIP - 2);
    assert_eq!(reorg.first_replaced, TIP - 1, "where exactly");
    assert_eq!(reorg.replaced, 2);
    assert_eq!(reorg.epoch, 1);
    let tip = event.live.tip.unwrap();
    assert_eq!((tip.height, tip.epoch), (TIP + 1, 1));
    let spent = delta(harness.counts(), before);
    assert!(spent[2] <= 10, "at most ten getblockhash: {spent:?}");
    // The page now shows the new branch.
    let ExplorerResponse::Chain(page) = harness
        .reply(ExplorerRequest::Chain { with_fees: true })
        .await
    else {
        panic!("a chain page");
    };
    let expected = harness.chain.borrow().hash(TIP).unwrap();
    assert_eq!(page.blocks[1].hash, expected);
    // Hidden, the explorer does not poll.
    harness
        .reply(ExplorerRequest::Interest {
            visible: false,
            page: PageKind::Chain,
        })
        .await;
    tokio::time::sleep(Duration::from_millis(40)).await;
    let quiet = harness.counts();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(delta(harness.counts(), quiet), [0; 6]);
    assert_eq!(harness.explorer.live().borrow().live.tip.unwrap().epoch, 1);
}

impl Harness {
    /// Changes the node's chain and reports the new tip in the statistics, as the monitor
    /// would. Returns what `change` returns.
    fn change_chain<R>(&self, change: impl FnOnce(&mut SynthChain) -> R) -> R {
        let mut out = None;
        self.chain.send_modify(|chain| out = Some(change(chain)));
        let stats = stats_for(&self.chain.borrow());
        self.stats.send_replace(Some(stats));
        out.expect("changed")
    }

    async fn show(&self, page: PageKind) {
        self.reply(ExplorerRequest::Interest {
            visible: true,
            page,
        })
        .await;
    }

    async fn next_reorg(&self) -> launcher_types::explorer::ReorgNotice {
        let mut live = self.explorer.live();
        tokio::time::timeout(LIMIT, live.wait_for(|event| event.live.reorg.is_some()))
            .await
            .expect("the reorganisation is noticed")
            .expect("the explorer runs")
            .live
            .reorg
            .expect("a notice")
    }
}

/// The statistics of a node still catching up with a network 100 blocks ahead of `chain`.
fn syncing(chain: &SynthChain) -> NodeStats {
    NodeStats {
        estimated_height: u64::from(chain.tip()) + 100,
        verification_progress: 0.99,
        ..stats_for(chain)
    }
}

#[tokio::test]
async fn blocks_read_while_the_tip_is_unchecked_do_not_mislead_the_fork_search() {
    let fresh = Duration::from_millis(200);
    let harness = harness_with(SynthChain::new(Network::Mainnet, TIP, NOW), |config| {
        config.fresh = fresh;
    });
    // The node syncs, so nothing follows the tip between pages.
    harness
        .stats
        .send_replace(Some(syncing(&harness.chain.borrow())));
    tokio::time::sleep(Duration::from_millis(20)).await;
    let ExplorerResponse::Chain(_) = harness
        .reply(ExplorerRequest::Chain { with_fees: true })
        .await
    else {
        panic!("a chain page");
    };
    // Long enough that the check no longer vouches for the heights near the tip, and that the
    // Chain page checks the tip again.
    tokio::time::sleep(Duration::from_millis(2_100)).await;
    // The node replaces its top ten blocks and mines five more.
    let mut fork = 0;
    harness.chain.send_modify(|chain| {
        fork = chain.reorg(10);
        chain.mine(5);
    });
    harness
        .stats
        .send_replace(Some(syncing(&harness.chain.borrow())));
    tokio::time::sleep(Duration::from_millis(20)).await;
    // A block asked for by height comes from the node's new branch.
    let ExplorerResponse::Block(_) = harness
        .reply(ExplorerRequest::Block {
            id: BlockId::Height(TIP - 4),
        })
        .await
    else {
        panic!("a block page");
    };
    let ExplorerResponse::Chain(_) = harness
        .reply(ExplorerRequest::Chain { with_fees: true })
        .await
    else {
        panic!("a chain page");
    };
    let reorg = harness.next_reorg().await;
    assert_eq!(
        (reorg.fork_height, reorg.first_replaced, reorg.replaced),
        (fork, fork + 1, 10),
        "{reorg:?}"
    );
    // A replaced height asked for by height is the node's block, not the cached one.
    let ExplorerResponse::Block(page) = harness
        .reply(ExplorerRequest::Block {
            id: BlockId::Height(TIP - 8),
        })
        .await
    else {
        panic!("a block page");
    };
    assert_eq!(
        Some(page.summary.hash),
        harness.chain.borrow().hash(TIP - 8)
    );
}

#[tokio::test]
async fn a_block_found_on_a_new_branch_checks_the_tip_again() {
    // Hidden: the follower does not poll.
    let harness = harness();
    let ExplorerResponse::Chain(_) = harness
        .reply(ExplorerRequest::Chain { with_fees: true })
        .await
    else {
        panic!("a chain page");
    };
    let fork = harness.change_chain(|chain| {
        let fork = chain.reorg(3);
        chain.mine(1);
        fork
    });
    // A search finds a block of the new branch before anything checked the tip.
    let found = harness.chain.borrow().hash(TIP - 1).unwrap();
    let ExplorerResponse::Resolve(Resolution::Go { .. }) = harness
        .reply(ExplorerRequest::Resolve {
            query: found.to_hex(),
        })
        .await
    else {
        panic!("the block");
    };
    let reorg = harness.next_reorg().await;
    assert_eq!(
        (reorg.fork_height, reorg.first_replaced, reorg.replaced),
        (fork, fork + 1, 3),
        "{reorg:?}"
    );
}

#[tokio::test]
async fn without_a_known_height_below_it_a_reorganisation_is_a_lower_bound() {
    let harness = harness_with(SynthChain::new(Network::Mainnet, TIP, NOW), |config| {
        config.tip_poll = Duration::from_millis(30);
    });
    let txid = harness
        .chain
        .borrow()
        .block(BlockId::Height(TIP - 2))
        .unwrap()
        .tx[1];
    let ExplorerResponse::Tx(before) = harness
        .reply(ExplorerRequest::Tx {
            txid,
            resolve: false,
        })
        .await
    else {
        panic!("a transaction page");
    };
    assert!(matches!(before.status, TxStatus::Mined { .. }));
    // The follower knows only the tip.
    harness.show(PageKind::Tx).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let fork = harness.change_chain(|chain| {
        let fork = chain.reorg(3);
        chain.mine(1);
        fork
    });
    let reorg = harness.next_reorg().await;
    assert!(
        reorg.fork_height <= fork && fork < reorg.first_replaced,
        "{reorg:?}, fork {fork}"
    );
    assert!((1..=3).contains(&reorg.replaced), "{reorg:?}");
    // The transaction's block was replaced: the page says so instead of the cached status.
    let ExplorerResponse::Tx(after) = harness
        .reply(ExplorerRequest::Tx {
            txid,
            resolve: false,
        })
        .await
    else {
        panic!("a transaction page");
    };
    assert!(
        matches!(after.status, TxStatus::SideChain { .. }),
        "{:?}",
        after.status
    );
}

#[tokio::test]
async fn far_apart_known_heights_bound_a_reorganisation() {
    let harness = harness_with(SynthChain::new(Network::Mainnet, TIP, NOW), |config| {
        config.tip_poll = Duration::from_millis(30);
    });
    let ExplorerResponse::Chain(_) = harness
        .reply(ExplorerRequest::Chain { with_fees: true })
        .await
    else {
        panic!("a chain page");
    };
    // 144 blocks arrive while the Explorer is hidden; shown again, the follower moves on.
    harness.change_chain(|chain| chain.mine(144));
    harness.show(PageKind::Tx).await;
    let mut live = harness.explorer.live();
    tokio::time::timeout(
        LIMIT,
        live.wait_for(|event| event.live.tip.is_some_and(|tip| tip.height == TIP + 144)),
    )
    .await
    .expect("moved on")
    .expect("the explorer runs");
    // One block is replaced: not the 144 the follower did not see one by one.
    let fork = harness.change_chain(|chain| {
        let fork = chain.reorg(1);
        chain.mine(1);
        fork
    });
    let reorg = harness.next_reorg().await;
    assert!(
        reorg.fork_height <= fork && fork < reorg.first_replaced,
        "{reorg:?}, fork {fork}"
    );
    assert_eq!(reorg.replaced, 1, "{reorg:?}");
}

/// zebrad running since `secs` after the Unix epoch: a run of its own.
fn running_since(secs: u64) -> Status {
    Status {
        started_at: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs)),
        ..running()
    }
}

impl Harness {
    /// zebrad stops, and starts again on `chain`.
    async fn restart_on(&self, chain: SynthChain) {
        self.status.send_replace(Status::default());
        self.stats.send_replace(None);
        tokio::time::sleep(Duration::from_millis(50)).await;
        self.stats.send_replace(Some(stats_for(&chain)));
        self.chain.send_replace(chain);
        self.status.send_replace(running_since(100));
    }
}

#[tokio::test]
async fn a_restart_a_block_lower_replaces_nothing() {
    let harness = harness_with(SynthChain::new(Network::Mainnet, TIP, NOW), |config| {
        config.tip_poll = Duration::from_millis(30);
    });
    let ExplorerResponse::Chain(_) = harness
        .reply(ExplorerRequest::Chain { with_fees: true })
        .await
    else {
        panic!("a chain page");
    };
    harness.show(PageKind::Chain).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    // zebrad comes back without its newest block, which had not reached the disk.
    harness
        .restart_on(SynthChain::new(Network::Mainnet, TIP - 1, NOW - 25))
        .await;
    let mut live = harness.explorer.live();
    let event = tokio::time::timeout(
        LIMIT,
        live.wait_for(|event| event.live.tip.is_some_and(|tip| tip.height == TIP - 1)),
    )
    .await
    .expect("the tip went back")
    .expect("the explorer runs")
    .clone();
    assert_eq!(event.live.reorg, None, "no block was replaced");
    assert_eq!(event.live.tip.unwrap().epoch, 1, "pages read again");
}

#[tokio::test]
async fn a_restart_lower_on_another_branch_replaced_blocks() {
    let harness = harness_with(SynthChain::new(Network::Mainnet, TIP, NOW), |config| {
        config.tip_poll = Duration::from_millis(30);
    });
    let ExplorerResponse::Chain(_) = harness
        .reply(ExplorerRequest::Chain { with_fees: true })
        .await
    else {
        panic!("a chain page");
    };
    harness.show(PageKind::Chain).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    // zebrad comes back one block lower, on a branch that parts above TIP - 3.
    let mut lower = SynthChain::new(Network::Mainnet, TIP - 1, NOW - 25);
    let fork = lower.reorg(2);
    harness.restart_on(lower).await;
    let reorg = harness.next_reorg().await;
    assert_eq!(
        (reorg.fork_height, reorg.first_replaced, reorg.replaced),
        (fork, fork + 1, 3),
        "{reorg:?}"
    );
}

#[tokio::test]
async fn a_new_run_is_checked_before_heights_near_its_tip_are_trusted() {
    let harness = harness();
    // The Chain page checks the tip, and the blocks near it then come from the cache.
    let ExplorerResponse::Chain(_) = harness
        .reply(ExplorerRequest::Chain { with_fees: true })
        .await
    else {
        panic!("a chain page");
    };
    let id = BlockId::Height(TIP - 5);
    harness.reply(ExplorerRequest::Block { id }).await;
    let before = harness.counts();
    harness.reply(ExplorerRequest::Block { id }).await;
    assert_eq!(delta(harness.counts(), before), [0; 6], "from the cache");
    harness
        .restart_on(SynthChain::new(Network::Mainnet, TIP, NOW))
        .await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    let before = harness.counts();
    harness.reply(ExplorerRequest::Block { id }).await;
    assert_eq!(
        delta(harness.counts(), before)[1],
        1,
        "the block is read again; its parent comes from the cache"
    );
}

#[tokio::test]
async fn a_syncing_node_is_not_followed_and_gets_fewer_reads_at_once() {
    let poll = Duration::from_millis(30);
    let harness = harness_with(SynthChain::new(Network::Mainnet, TIP, NOW), |config| {
        config.tip_poll = poll;
    });
    harness
        .stats
        .send_replace(Some(syncing(&harness.chain.borrow())));
    tokio::time::sleep(Duration::from_millis(20)).await;
    harness.show(PageKind::Chain).await;
    tokio::time::sleep(poll * 8).await;
    assert_eq!(harness.counts()[0], 0, "no tip checks while the node syncs");
    // Eight Block pages at once: four reads run while the node syncs, six at the tip.
    for (stats, limit) in [
        (syncing(&harness.chain.borrow()), 4),
        (stats_for(&harness.chain.borrow()), 6),
    ] {
        harness.stats.send_replace(Some(stats));
        tokio::time::sleep(Duration::from_millis(20)).await;
        harness.gate.send_replace(false);
        let before = harness.counts()[1];
        let pages: Vec<_> = (0..8)
            .map(|back| {
                let explorer = harness.explorer.clone();
                let request = ExplorerRequest::Block {
                    id: BlockId::Height(TIP - 2_000 - 10 * back - limit),
                };
                tokio::spawn(
                    async move { explorer.call(&mainnet(), ExplorerCall::new(request)).await },
                )
            })
            .collect();
        // Wait until the reads that may run have started (a slow runner takes a while), then
        // check that no more did.
        let started = tokio::time::Instant::now();
        while harness.counts()[1] - before < u64::from(limit)
            && started.elapsed() < Duration::from_secs(5)
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        let running = harness.counts()[1] - before;
        if limit == 4 {
            assert_eq!(running, 4, "while the node syncs");
        } else {
            // At the tip the follower checks the tip too, on the same permits, and the closed
            // gate holds that check: one page read may wait for it.
            assert!((5..=6).contains(&running), "at the tip: {running}");
        }
        harness.gate.send_replace(true);
        for page in pages {
            page.await.expect("no panic").expect("a block page");
        }
    }
}

#[tokio::test]
async fn a_new_run_of_zebrad_keeps_the_cache() {
    let harness = harness();
    let ExplorerResponse::Chain(first) = harness
        .reply(ExplorerRequest::Chain { with_fees: true })
        .await
    else {
        panic!("a chain page");
    };
    // zebrad stops, and starts again on another RPC port.
    harness.status.send_replace(Status::default());
    tokio::time::sleep(Duration::from_millis(20)).await;
    harness.status.send_replace(Status {
        rpc_addr: Some(SocketAddr::from((Ipv4Addr::LOCALHOST, 18232))),
        ..running_since(100)
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let before = harness.counts();
    let ExplorerResponse::Chain(again) = harness
        .reply(ExplorerRequest::Chain { with_fees: true })
        .await
    else {
        panic!("a chain page");
    };
    assert_eq!(again, first);
    assert_eq!(
        delta(harness.counts(), before),
        [1, 0, 0, 0, 0, 0],
        "one tip check; the rest from the cache"
    );
}

#[tokio::test]
async fn a_small_blocks_rows_are_one_heavy_read_with_every_fee() {
    let harness = harness();
    let height = TIP - 2_000;
    let ExplorerResponse::Block(page) = harness
        .reply(ExplorerRequest::Block {
            id: BlockId::Height(height),
        })
        .await
    else {
        panic!("a block page");
    };
    let hash = page.summary.hash;
    let before = harness.counts();
    let ExplorerResponse::BlockTxs(rows) = harness
        .reply(ExplorerRequest::BlockTxs {
            hash,
            offset: 0,
            limit: 200,
            with_fees: false,
        })
        .await
    else {
        panic!("a page of rows");
    };
    assert_eq!(rows.read, RowsRead::Block);
    assert!(!rows.fees_pending);
    assert_eq!(rows.total, page.summary.tx_count);
    assert_eq!(rows.rows.len() as u32, page.summary.tx_count);
    assert_eq!(harness.v3_reads(), 1, "the block, whole, once");
    assert_eq!(delta(harness.counts(), before), [0; 6], "nothing else");
    // Every fee is known, and they add up to the block's.
    let chain = harness.chain.borrow().clone();
    let mut fees = 0;
    for row in &rows.rows[1..] {
        let Some(Shown::Public { zat }) = row.fee else {
            panic!("a fee: {row:?}");
        };
        assert_eq!(Some(zat), chain.fee(&row.txid));
        fees += zat;
    }
    assert_eq!(Some(fees as u64), page.summary.ledger.fees);
    assert_eq!(rows.rows[0].fee, None, "a coinbase pays none");
    // A later page, and a spending transaction's own page, come from the block in the cache:
    // the transaction itself is the one read.
    let spending = txid_where(&chain, height, true);
    let before = harness.counts();
    harness
        .reply(ExplorerRequest::BlockTxs {
            hash,
            offset: 1,
            limit: 5,
            with_fees: true,
        })
        .await;
    let ExplorerResponse::Tx(tx) = harness
        .reply(ExplorerRequest::Tx {
            txid: spending,
            resolve: false,
        })
        .await
    else {
        panic!("a transaction page");
    };
    assert_eq!(delta(harness.counts(), before), [0, 0, 0, 0, 1, 0]);
    assert_eq!(harness.v3_reads(), 1);
    assert_eq!(tx.unresolved, 0);
    assert_eq!(
        tx.fee,
        chain.fee(&spending).map(|zat| Shown::Public { zat })
    );
}

#[tokio::test]
async fn a_transaction_page_reads_the_outputs_its_inputs_spend() {
    let harness = harness();
    let chain = harness.chain.borrow().clone();
    let txid = txid_where(&chain, TIP - 10, true);
    let tx = chain.tx(&txid).unwrap();
    let parents: std::collections::HashSet<_> =
        tx.vin.iter().filter_map(|input| input.txid).collect();
    assert!(parents.len() <= 16);
    let ExplorerResponse::Tx(page) = harness
        .reply(ExplorerRequest::Tx {
            txid,
            resolve: false,
        })
        .await
    else {
        panic!("a transaction page");
    };
    assert_eq!(
        harness.counts(),
        [0, 0, 0, 0, 1 + parents.len() as u64, 0],
        "the transaction, then each earlier one once"
    );
    assert_eq!(harness.v3_reads(), 0, "a few parents need no whole block");
    assert_eq!(page.unresolved, 0);
    assert!(!page.resolvable);
    assert!(page.inputs.iter().all(|input| input.spent.is_some()));
    assert_eq!(page.fee, chain.fee(&txid).map(|zat| Shown::Public { zat }));
    // Its flow balances.
    assert!(matches!(
        explorer_model::flow::tx_flow(&page).balance,
        explorer_model::flow::Balance::Balanced { .. }
    ));
    // Again: all from the cache.
    let before = harness.counts();
    harness
        .reply(ExplorerRequest::Tx {
            txid,
            resolve: false,
        })
        .await;
    assert_eq!(delta(harness.counts(), before), [0; 6]);
}

impl Harness {
    /// Gives the node transactions its chain does not have.
    fn add_txs(&self, txs: impl IntoIterator<Item = Tx>) {
        self.extra
            .send_modify(|extra| extra.extend(txs.into_iter().map(|tx| (tx.txid, tx))));
    }
}

/// The `n`-th id of a set of made-up transactions, `set`, which no synthetic one has.
fn made_up_txid(set: u8, n: u32) -> TxId {
    let mut bytes = [0x5e; 32];
    bytes[0] = set;
    bytes[1..5].copy_from_slice(&n.to_be_bytes());
    Hash32(bytes)
}

/// A made-up transaction that spends one output of each of `count` earlier ones, made up
/// too, and pays a fee of 10,000 zatoshis: mined in the block at `height`, or waiting with
/// `None`. Returns it with the earlier ones.
fn spending_from(chain: &SynthChain, set: u8, count: u32, height: Option<u32>) -> (Tx, Vec<Tx>) {
    let template = chain
        .tx(&txid_where(chain, TIP - 3_000, true))
        .expect("a transaction");
    let paying = |zat| TxOut {
        value_zat: zat,
        n: 0,
        script: ScriptPubKey {
            kind: "pubkeyhash".into(),
            addresses: Some(vec!["t1MadeUpForTheExplorerTests000000".into()]),
            hex: HexLen(25),
        },
    };
    let at = |tx: &mut Tx, height: u32| {
        tx.height = Some(i64::from(height));
        tx.confirmations = Some(i64::from(TIP - height + 1));
        tx.block_hash = chain.hash(height);
    };
    let plain = Tx {
        fee: None,
        vin: Vec::new(),
        vout: vec![paying(100_000)],
        sapling_spends: Count(0),
        sapling_outputs: Count(0),
        joinsplits: Vec::new(),
        orchard: None,
        ironwood: None,
        sapling_value_balance: None,
        ..template
    };
    let parents: Vec<Tx> = (0..count)
        .map(|n| {
            let mut parent = Tx {
                txid: made_up_txid(set, n),
                ..plain.clone()
            };
            at(&mut parent, TIP - 3_000);
            parent
        })
        .collect();
    let mut tx = Tx {
        txid: made_up_txid(set, u32::MAX),
        vin: parents
            .iter()
            .map(|parent| TxIn {
                coinbase: None,
                txid: Some(parent.txid),
                vout: Some(0),
                script_sig: Some(ScriptSig { hex: HexLen(107) }),
                prevout: None,
            })
            .collect(),
        vout: vec![paying(100_000 * i64::from(count) - 10_000)],
        ..plain
    };
    match height {
        Some(height) => at(&mut tx, height),
        None => {
            tx.height = None;
            tx.confirmations = None;
            tx.block_hash = None;
            tx.block_time = None;
        }
    }
    (tx, parents)
}

#[tokio::test]
async fn a_transaction_spending_from_many_earlier_ones_has_the_rest_read_on_request() {
    let harness = harness_with(
        SynthChain::new(Network::Mainnet, TIP, NOW).with_big_blocks(10),
        |_| {},
    );
    let chain = harness.chain.borrow().clone();
    let big = TIP - 2_000;
    assert!(chain.is_big(big));
    let (tx, parents) = spending_from(&chain, 1, 20, Some(big));
    let txid = tx.txid;
    harness.add_txs(parents.into_iter().chain([tx]));
    let ask = |txid, resolve| ExplorerRequest::Tx { txid, resolve };
    let page = |response| match response {
        ExplorerResponse::Tx(page) => page,
        other => panic!("a transaction page: {other:?}"),
    };
    // A big block is not read whole: 16 earlier transactions, and the rest on request.
    let first = page(harness.reply(ask(txid, false)).await);
    assert_eq!(
        harness.counts(),
        [0, 1, 0, 0, 1 + 16, 0],
        "the transaction, its block to see that it is big, and 16 earlier ones"
    );
    assert_eq!(harness.v3_reads(), 0);
    assert_eq!((first.unresolved, first.resolvable), (4, true));
    assert_eq!(
        first.fee,
        Some(Shown::Unknown {
            why: UnknownWhy::InputValues
        })
    );
    // Read them all: the other four, past the first 16, on the prefetch lane, two at a time.
    harness.tx_gate.send_replace(false);
    let _ = harness.started();
    let all = harness.ask_later(ask(txid, true));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(harness.started().len(), 2, "two prefetches at a time");
    harness.tx_gate.send_replace(true);
    let all = page(all.await.expect("no panic").expect("a page").response);
    assert_eq!(
        harness.counts(),
        [0, 1, 0, 0, 1 + 20, 0],
        "only the four not read before"
    );
    assert_eq!((all.unresolved, all.resolvable), (0, false));
    assert_eq!(all.fee, Some(Shown::Public { zat: 10_000 }));

    // With `resolve`, at most 1,000 earlier transactions for a mined one...
    let (wide, parents) = spending_from(&chain, 2, 1_010, Some(big));
    let wide_id = wide.txid;
    harness.add_txs(parents.into_iter().chain([wide]));
    let wide = wide_id;
    let before = harness.counts();
    let read = page(harness.reply(ask(wide, true)).await);
    assert_eq!(delta(harness.counts(), before), [0, 0, 0, 0, 1 + 1_000, 0]);
    assert_eq!((read.unresolved, read.resolvable), (10, false));
    // ... and 200 for a waiting one, which has no block to read and is not kept.
    let (waiting, parents) = spending_from(&chain, 3, 210, None);
    let waiting_id = waiting.txid;
    harness.add_txs(parents.into_iter().chain([waiting]));
    let waiting = waiting_id;
    let before = harness.counts();
    let first = page(harness.reply(ask(waiting, false)).await);
    assert_eq!(delta(harness.counts(), before), [0, 0, 0, 0, 1 + 16, 0]);
    assert_eq!((first.unresolved, first.resolvable), (194, true));
    let all = page(harness.reply(ask(waiting, true)).await);
    assert_eq!(
        delta(harness.counts(), before),
        [0, 0, 0, 0, 1 + 16 + 1 + 184, 0]
    );
    assert_eq!((all.unresolved, all.resolvable), (10, false));
}

#[tokio::test]
async fn a_small_block_brings_what_a_transaction_spending_from_many_spends() {
    let harness = harness();
    let chain = harness.chain.borrow().clone();
    let small = TIP - 2_000;
    let (tx, parents) = spending_from(&chain, 4, 20, Some(small));
    let txid = tx.txid;
    harness.add_txs(parents.into_iter().chain([tx]));
    let ask = |txid, resolve| ExplorerRequest::Tx { txid, resolve };
    let page = |response| match response {
        ExplorerResponse::Tx(page) => page,
        other => panic!("a transaction page: {other:?}"),
    };
    // The block, read whole, has every output the transaction spends: no earlier one is read.
    let whole = page(harness.reply(ask(txid, false)).await);
    assert_eq!(harness.counts(), [0, 1, 0, 0, 1, 0]);
    assert_eq!(harness.v3_reads(), 1);
    assert_eq!((whole.unresolved, whole.resolvable), (0, false));
    assert_eq!(whole.fee, Some(Shown::Public { zat: 10_000 }));
    // When the block cannot be read whole, the inputs are left to read one by one on request.
    harness.fail_v3.store(true, Ordering::SeqCst);
    let (other, parents) = spending_from(&chain, 5, 20, Some(small - 1));
    let other_id = other.txid;
    harness.add_txs(parents.into_iter().chain([other]));
    let other = other_id;
    let before = harness.counts();
    let unread = page(harness.reply(ask(other, false)).await);
    assert_eq!(delta(harness.counts(), before), [0, 1, 0, 0, 1, 0]);
    assert_eq!(harness.v3_reads(), 2, "the second block's attempt");
    assert_eq!((unread.unresolved, unread.resolvable), (20, true));
    let all = page(harness.reply(ask(other, true)).await);
    assert_eq!(delta(harness.counts(), before), [0, 1, 0, 0, 1 + 20, 0]);
    assert_eq!((all.unresolved, all.resolvable), (0, false));
    assert_eq!(all.fee, Some(Shown::Public { zat: 10_000 }));
}

#[tokio::test]
async fn a_coinbase_page_claims_from_its_block() {
    let harness = harness();
    let height = TIP - 30;
    let chain = harness.chain.borrow().clone();
    let txid = chain.block(BlockId::Height(height)).unwrap().tx[0];
    let ExplorerResponse::Tx(page) = harness
        .reply(ExplorerRequest::Tx {
            txid,
            resolve: false,
        })
        .await
    else {
        panic!("a transaction page");
    };
    assert_eq!(
        harness.counts(),
        [0, 2, 0, 0, 1, 1],
        "the coinbase, its block and parent, and the subsidy: the Block page's reads"
    );
    let claim = page.coinbase.expect("a claim");
    assert_eq!(
        claim.subsidy,
        u64::try_from(zec_to_zat(chain.subsidy(height).total)).ok()
    );
    assert!(claim.fees.is_some() && claim.burned.is_some());
    assert_eq!(
        explorer_model::flow::tx_flow(&page).balance,
        explorer_model::flow::Balance::Balanced {
            total: claim.subsidy.unwrap() + claim.fees.unwrap()
        }
    );
    assert!(page.burns_fees);
}

#[tokio::test]
async fn a_big_blocks_rows_are_read_one_by_one_and_their_fees_after() {
    let harness = harness_with(
        SynthChain::new(Network::Mainnet, TIP, NOW).with_big_blocks(10),
        |_| {},
    );
    let height = TIP - 2_000;
    let chain = harness.chain.borrow().clone();
    assert!(chain.is_big(height));
    let ExplorerResponse::Block(page) = harness
        .reply(ExplorerRequest::Block {
            id: BlockId::Height(height),
        })
        .await
    else {
        panic!("a block page");
    };
    let hash = page.summary.hash;
    let before = harness.counts();
    let ask = |with_fees| ExplorerRequest::BlockTxs {
        hash,
        offset: 4_000,
        limit: 25,
        with_fees,
    };
    let ExplorerResponse::BlockTxs(first) = harness.reply(ask(false)).await else {
        panic!("a page of rows");
    };
    assert_eq!(first.read, RowsRead::OneByOne);
    assert_eq!((first.offset, first.rows.len()), (4_000, 25));
    assert!(first.fees_pending, "the spent outputs are on their way");
    assert_eq!(harness.v3_reads(), 0, "never the whole block");
    let rows_read = delta(harness.counts(), before)[4];
    assert!(rows_read >= 25, "{rows_read}");
    // Asked again with the fees, the page waits for the outputs the rows spend.
    let ExplorerResponse::BlockTxs(full) = harness.reply(ask(true)).await else {
        panic!("a page of rows");
    };
    assert!(!full.fees_pending);
    for row in &full.rows {
        assert_eq!(
            row.fee,
            chain.fee(&row.txid).map(|zat| Shown::Public { zat }),
            "{}",
            row.index
        );
    }
    let parents: std::collections::HashSet<_> = full
        .rows
        .iter()
        .flat_map(|row| chain.tx(&row.txid).unwrap().vin)
        .filter_map(|input| input.txid)
        .collect();
    assert_eq!(
        delta(harness.counts(), before)[4],
        25 + parents.len() as u64,
        "each row and each earlier transaction once"
    );
    assert_eq!(harness.v3_reads(), 0);
}

impl Harness {
    /// Asks for `request` on a task of its own, so the test goes on while it waits.
    fn ask_later(
        &self,
        request: ExplorerRequest,
    ) -> tokio::task::JoinHandle<Result<ExplorerReply, launcher_types::explorer::ExplorerError>>
    {
        let explorer = self.explorer.clone();
        tokio::spawn(async move { explorer.call(&mainnet(), ExplorerCall::new(request)).await })
    }
}

#[tokio::test]
async fn a_big_blocks_fee_reads_wait_behind_its_rows_and_the_tip() {
    let harness = harness_with(
        SynthChain::new(Network::Mainnet, TIP, NOW).with_big_blocks(10),
        |_| {},
    );
    let chain = harness.chain.borrow().clone();
    let block = chain.block(BlockId::Height(TIP - 2_000)).unwrap();
    let hash = block.hash;
    // The block and the transactions of rows 4,000 to 4,024 are in the cache, but not the
    // earlier transactions they spend from: a card reads its transaction and nothing else.
    harness
        .reply(ExplorerRequest::Block {
            id: BlockId::Hash(hash),
        })
        .await;
    let shown = &block.tx[4_000..4_025];
    for txid in shown {
        harness
            .reply(ExplorerRequest::Card {
                target: CardTarget::Tx { txid: *txid },
            })
            .await;
    }
    let parents: std::collections::HashSet<TxId> = shown
        .iter()
        .flat_map(|txid| chain.tx(txid).unwrap().vin)
        .filter_map(|input| input.txid)
        .collect();
    let next_rows: std::collections::HashSet<TxId> =
        block.tx[4_025..4_050].iter().copied().collect();
    let rows = |offset, with_fees| ExplorerRequest::BlockTxs {
        hash,
        offset,
        limit: 25,
        with_fees,
    };
    let _ = harness.started();
    // The next rows are asked for while transaction reads are held: six start, 19 wait.
    harness.tx_gate.send_replace(false);
    let next = harness.ask_later(rows(4_025, false));
    tokio::time::sleep(Duration::from_millis(50)).await;
    // Shown, the explorer checks the tip, which waits for a permit too.
    harness.show(PageKind::Block).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    // The first rows are asked for again, with their fees: the outputs their inputs spend.
    let fees = harness.ask_later(rows(4_000, true));
    tokio::time::sleep(Duration::from_millis(50)).await;
    harness.tx_gate.send_replace(true);
    let Ok(ExplorerReply {
        response: ExplorerResponse::BlockTxs(page),
        ..
    }) = fees.await.expect("no panic")
    else {
        panic!("a page of rows");
    };
    assert!(!page.fees_pending);
    assert!(
        page.rows
            .iter()
            .all(|row| matches!(row.fee, Some(Shown::Public { .. })))
    );
    next.await.expect("no panic").expect("the next rows");
    // Those reads waited on the prefetch lane: the rows and the tip check went first, though
    // they were asked for before.
    let started = harness.started();
    let read = |wanted: &std::collections::HashSet<TxId>| {
        let wanted = wanted.clone();
        move |call: &ReadCall| matches!(call, ReadCall::Tx(txid) if wanted.contains(txid))
    };
    let first_parent = started
        .iter()
        .position(read(&parents))
        .expect("the earlier transactions were read");
    let last_row = started
        .iter()
        .rposition(read(&next_rows))
        .expect("the next rows were read");
    let tip = started
        .iter()
        .position(|call| *call == ReadCall::Best)
        .expect("the tip was checked");
    assert!(last_row < first_parent, "{started:?}");
    assert!(tip < first_parent, "{started:?}");
}

#[tokio::test]
async fn a_heavy_read_takes_three_of_the_six() {
    let harness = harness();
    let heights = [TIP - 2_000, TIP - 2_002];
    let mut hashes = Vec::new();
    for height in heights {
        let ExplorerResponse::Block(page) = harness
            .reply(ExplorerRequest::Block {
                id: BlockId::Height(height),
            })
            .await
        else {
            panic!("a block page");
        };
        hashes.push(page.summary.hash);
    }
    let chain = harness.chain.borrow().clone();
    let shielded: Vec<TxId> = (TIP - 1_000..TIP - 900)
        .filter_map(|height| {
            let block = chain.block(BlockId::Height(height))?;
            block
                .tx
                .iter()
                .skip(1)
                .copied()
                .find(|txid| chain.tx(txid).is_some_and(|tx| tx.vin.is_empty()))
        })
        .take(5)
        .collect();
    assert_eq!(shielded.len(), 5);
    harness.gate.send_replace(false);
    let before = harness.counts();
    let rows = |hash| {
        let explorer = harness.explorer.clone();
        let request = ExplorerRequest::BlockTxs {
            hash,
            offset: 0,
            limit: 200,
            with_fees: false,
        };
        tokio::spawn(async move { explorer.call(&mainnet(), ExplorerCall::new(request)).await })
    };
    let mut asks = vec![rows(hashes[0])];
    tokio::time::sleep(Duration::from_millis(50)).await;
    for txid in shielded {
        let request = ExplorerRequest::Tx {
            txid,
            resolve: false,
        };
        let explorer = harness.explorer.clone();
        asks.push(tokio::spawn(async move {
            explorer.call(&mainnet(), ExplorerCall::new(request)).await
        }));
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    // One whole block weighs three: three transactions fill the six.
    assert_eq!(harness.v3_reads(), 1);
    assert_eq!(delta(harness.counts(), before)[4], 3);
    // A second whole block waits, while other interactive reads do and while there is no
    // room.
    asks.push(rows(hashes[1]));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(harness.v3_reads(), 1);
    harness.gate.send_replace(true);
    for ask in asks {
        ask.await.expect("no panic").expect("an answer");
    }
    assert_eq!(harness.v3_reads(), 2);
}

#[tokio::test]
async fn show_rpc_lists_the_reads_an_answer_took() {
    let harness = harness();
    let height = TIP - 2_000;
    // By hash, so the second ask needs no check of the tip to find it in the cache.
    let id = BlockId::Hash(harness.chain.borrow().hash(height).unwrap());
    let traced = |request| {
        let explorer = harness.explorer.clone();
        async move {
            explorer
                .call(
                    &mainnet(),
                    ExplorerCall {
                        request,
                        trace: true,
                    },
                )
                .await
                .expect("an answer")
        }
    };
    let first = traced(ExplorerRequest::Block { id }).await;
    let sources = first.sources.expect("the reads");
    let mut methods: Vec<&str> = sources.iter().map(|call| call.method.as_str()).collect();
    methods.sort_unstable();
    assert_eq!(
        methods,
        [
            "getblock",
            "getblock",
            "getblocksubsidy",
            "getrawtransaction"
        ]
    );
    assert!(sources.iter().all(|call| !call.cached && call.ok));
    assert!(
        sources
            .iter()
            .all(|call| call.bytes.is_some_and(|bytes| bytes > 0))
    );
    assert!(
        sources
            .iter()
            .any(|call| call.params == format!(r#"["{id}",1]"#)),
        "{sources:?}"
    );
    assert!(
        sources
            .iter()
            .any(|call| call.params == format!("[{height}]")),
        "{sources:?}"
    );
    // The same page again comes from memory, and says so.
    let again = traced(ExplorerRequest::Block { id }).await;
    let sources = again.sources.expect("the reads");
    assert_eq!(sources.len(), 4);
    assert!(
        sources
            .iter()
            .all(|call| call.cached && call.ms == 0 && call.bytes.is_none())
    );
    // Without `trace`, no list.
    assert_eq!(
        harness
            .ask(ExplorerRequest::Block { id })
            .await
            .unwrap()
            .sources,
        None
    );
    // A missing transaction is listed as a read that failed.
    let missing = harness
        .explorer
        .call(
            &mainnet(),
            ExplorerCall {
                request: ExplorerRequest::Resolve {
                    query: "ab".repeat(32),
                },
                trace: true,
            },
        )
        .await
        .expect("an answer");
    let sources = missing.sources.expect("the reads");
    assert!(sources.iter().all(|call| !call.ok), "{sources:?}");
}

#[tokio::test]
async fn prefetch_and_cards_read_on_their_own_lane_and_fill_the_cache() {
    let harness = harness();
    let height = TIP - 2_000;
    let txid = txid_where(&harness.chain.borrow(), height, false);
    let id = BlockId::Hash(harness.chain.borrow().hash(height).unwrap());
    let ExplorerResponse::Done = harness
        .reply(ExplorerRequest::Prefetch {
            targets: vec![PrefetchTarget::Block { id }],
        })
        .await
    else {
        panic!("answered at once");
    };
    // The reads go on behind the answer.
    let started = tokio::time::Instant::now();
    while harness.counts() != [0, 2, 0, 0, 1, 1] && started.elapsed() < LIMIT {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        harness.counts(),
        [0, 2, 0, 0, 1, 1],
        "the Block page's reads"
    );
    // Once they are in, the page is all from the cache.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let before = harness.counts();
    harness.reply(ExplorerRequest::Block { id }).await;
    assert_eq!(
        delta(harness.counts(), before),
        [0; 6],
        "now from the cache"
    );
    // A card is one read, which the page then finds in the cache.
    let ExplorerResponse::Card(Card::Tx { txid: carded, .. }) = harness
        .reply(ExplorerRequest::Card {
            target: CardTarget::Tx { txid },
        })
        .await
    else {
        panic!("a card");
    };
    assert_eq!(carded, txid);
    assert_eq!(delta(harness.counts(), before), [0, 0, 0, 0, 1, 0]);
    let ExplorerResponse::Card(Card::Block { height: shown, .. }) = harness
        .reply(ExplorerRequest::Card {
            target: CardTarget::Block { id },
        })
        .await
    else {
        panic!("a card");
    };
    assert_eq!(shown, height);
    harness
        .reply(ExplorerRequest::Tx {
            txid,
            resolve: false,
        })
        .await;
    assert_eq!(
        delta(harness.counts(), before),
        [0, 0, 0, 0, 1, 0],
        "the block and the transaction were in the cache"
    );
}

// The time limits at their own values, on Tokio's paused clock: a sleep there takes no time,
// and the actor's limits read the same clock.

#[tokio::test(start_paused = true)]
async fn a_hidden_explorer_shrinks_its_cache_after_ten_minutes() {
    // An empty idle budget, so the shrinking shows; the wait is the default 10 minutes.
    let harness = harness_with(SynthChain::new(Network::Mainnet, TIP, NOW), |config| {
        config.idle_cache_bytes = 0;
    });
    let id = BlockId::Hash(harness.chain.borrow().hash(TIP - 2_000).unwrap());
    harness.reply(ExplorerRequest::Block { id }).await;
    // Shown, then hidden.
    harness.show(PageKind::Block).await;
    harness
        .reply(ExplorerRequest::Interest {
            visible: false,
            page: PageKind::Block,
        })
        .await;
    tokio::time::sleep(Duration::from_secs(9 * 60)).await;
    let before = harness.counts();
    harness.reply(ExplorerRequest::Block { id }).await;
    assert_eq!(
        delta(harness.counts(), before),
        [0; 6],
        "nine minutes: kept"
    );
    // Asking does not show the page: the explorer stays hidden, and shrinks at ten minutes.
    tokio::time::sleep(Duration::from_secs(60 + 5)).await;
    let before = harness.counts();
    harness.reply(ExplorerRequest::Block { id }).await;
    assert_eq!(
        delta(harness.counts(), before),
        [0, 2, 0, 0, 1, 1],
        "read again"
    );
}

#[tokio::test(start_paused = true)]
async fn a_height_near_the_tip_is_read_again_once_the_check_is_ten_seconds_old() {
    let harness = harness();
    let ExplorerResponse::Chain(_) = harness
        .reply(ExplorerRequest::Chain { with_fees: true })
        .await
    else {
        panic!("a chain page");
    };
    let (near, deep) = (BlockId::Height(TIP - 5), BlockId::Height(TIP - 2_000));
    for id in [near, deep] {
        harness.reply(ExplorerRequest::Block { id }).await;
    }
    tokio::time::sleep(Duration::from_secs(9)).await;
    let before = harness.counts();
    for id in [near, deep] {
        harness.reply(ExplorerRequest::Block { id }).await;
    }
    assert_eq!(
        delta(harness.counts(), before),
        [0; 6],
        "nine seconds: from the cache"
    );
    tokio::time::sleep(Duration::from_secs(2)).await;
    let before = harness.counts();
    for id in [near, deep] {
        harness.reply(ExplorerRequest::Block { id }).await;
    }
    assert_eq!(
        delta(harness.counts(), before),
        [0, 1, 0, 0, 0, 0],
        "the block near the tip is read again; the final one is not"
    );
}

#[tokio::test(start_paused = true)]
async fn the_follower_checks_the_tip_every_two_seconds_while_shown() {
    let harness = harness();
    harness.show(PageKind::Chain).await;
    tokio::time::sleep(Duration::from_secs(6)).await;
    let before = harness.counts()[0];
    tokio::time::sleep(Duration::from_secs(30)).await;
    let checks = harness.counts()[0] - before;
    // One check a poll: fifteen in 30 s. A check every other poll would make about seven.
    assert!((14..=16).contains(&checks), "{checks} tip checks in 30 s");
    // Hidden, it checks nothing.
    harness
        .reply(ExplorerRequest::Interest {
            visible: false,
            page: PageKind::Chain,
        })
        .await;
    let before = harness.counts();
    tokio::time::sleep(Duration::from_secs(60)).await;
    assert_eq!(
        delta(harness.counts(), before),
        [0; 6],
        "nothing while hidden"
    );
}

#[tokio::test(start_paused = true)]
async fn zebrad_on_the_old_network_is_not_followed_while_shown() {
    let harness = harness();
    // The node moves to Testnet while zebrad runs on Mainnet, and the Explorer is on screen.
    harness
        .spec
        .send_modify(|spec| spec.network = Network::Testnet);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let before = harness.counts();
    harness.show(PageKind::Chain).await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert_eq!(
        delta(harness.counts(), before),
        [0; 6],
        "nothing of Mainnet is read"
    );
    // The node goes back to Mainnet: the follower checks the tip again.
    harness
        .spec
        .send_modify(|spec| spec.network = Network::Mainnet);
    tokio::time::sleep(Duration::from_secs(6)).await;
    assert!(harness.counts()[0] > before[0], "the tip is checked again");
}

#[tokio::test(start_paused = true)]
async fn another_network_clears_the_cache() {
    let harness = harness();
    let id = BlockId::Hash(harness.chain.borrow().hash(TIP - 2_000).unwrap());
    harness.reply(ExplorerRequest::Block { id }).await;
    let before = harness.counts();
    harness.reply(ExplorerRequest::Block { id }).await;
    assert_eq!(delta(harness.counts(), before), [0; 6], "from the cache");
    // zebrad reports Testnet, then Mainnet again: nothing known about the first chain is
    // kept across.
    for chain in ["test", "main"] {
        harness.stats.send_modify(|stats| {
            if let Some(stats) = stats {
                stats.chain = chain.into();
            }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let before = harness.counts();
    harness.reply(ExplorerRequest::Block { id }).await;
    assert_eq!(
        delta(harness.counts(), before),
        [0, 2, 0, 0, 1, 1],
        "read again"
    );
}

/// Every event's fields, as text, from the explorer's threads.
mod capture {
    use std::{
        fmt,
        sync::{
            atomic::{AtomicU64, Ordering},
            mpsc,
        },
    };

    use tracing::{
        Event, Metadata, Subscriber,
        field::{Field, Visit},
        span::{Attributes, Id, Record},
    };

    pub(super) struct Capture {
        pub(super) lines: mpsc::Sender<String>,
        next_id: AtomicU64,
    }

    impl Capture {
        pub(super) fn new(lines: mpsc::Sender<String>) -> Self {
            Self {
                lines,
                next_id: AtomicU64::new(1),
            }
        }
    }

    struct Fields(String);

    impl Visit for Fields {
        fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
            self.0.push_str(&format!(" {}={value:?}", field.name()));
        }
    }

    impl Subscriber for Capture {
        fn enabled(&self, _: &Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, span: &Attributes<'_>) -> Id {
            let mut fields = Fields(String::new());
            span.record(&mut fields);
            let _ = self.lines.send(fields.0);
            Id::from_u64(self.next_id.fetch_add(1, Ordering::Relaxed))
        }
        fn record(&self, _: &Id, values: &Record<'_>) {
            let mut fields = Fields(String::new());
            values.record(&mut fields);
            let _ = self.lines.send(fields.0);
        }
        fn record_follows_from(&self, _: &Id, _: &Id) {}
        fn event(&self, event: &Event<'_>) {
            let mut fields = Fields(String::new());
            event.record(&mut fields);
            let _ = self.lines.send(fields.0);
        }
        fn enter(&self, _: &Id) {}
        fn exit(&self, _: &Id) {}
    }
}

#[tokio::test(flavor = "current_thread")]
async fn the_logs_never_carry_what_was_asked_for() {
    let (lines, captured) = std::sync::mpsc::channel();
    // While a single dispatcher is registered, tracing caches a callsite's interest with the
    // dispatcher of whichever thread reaches it first, which may be another test's. A second
    // one makes every callsite ask all registered dispatchers, this test's included.
    let _second = tracing::Dispatch::new(capture::Capture::new(lines.clone()));
    let _guard = tracing::subscriber::set_default(capture::Capture::new(lines));
    tracing::callsite::rebuild_interest_cache();
    let harness = harness();
    let (block, txid) = {
        let chain = harness.chain.borrow();
        let block = chain.block(BlockId::Height(TIP - 77)).unwrap();
        (block.hash, block.tx[0])
    };
    let height = TIP - 77;
    harness
        .reply(ExplorerRequest::Block {
            id: BlockId::Height(height),
        })
        .await;
    harness
        .reply(ExplorerRequest::Tx {
            txid,
            resolve: false,
        })
        .await;
    harness
        .reply(ExplorerRequest::Resolve {
            query: block.to_hex(),
        })
        .await;
    let _ = harness
        .ask(ExplorerRequest::Tx {
            txid: Hash32([0x5a; 32]),
            resolve: false,
        })
        .await;
    drop(harness);
    let text: Vec<String> = captured.try_iter().collect();
    assert!(!text.is_empty(), "the reads are logged");
    for line in &text {
        for secret in [
            block.to_hex(),
            txid.to_hex(),
            Hash32([0x5a; 32]).to_hex(),
            height.to_string(),
        ] {
            assert!(!line.contains(&secret), "{line}");
        }
    }
}
