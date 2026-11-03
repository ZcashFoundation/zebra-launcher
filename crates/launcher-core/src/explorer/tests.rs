//! The explorer actor over a synthetic chain: what each page costs the node, merging and the
//! cache, the node and network rules, a reorganisation followed at the tip, and logs that
//! never carry what was asked for.

use std::{
    net::{Ipv4Addr, SocketAddr},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime},
};

use explorer_model::synth::SynthChain;
use launcher_types::{
    EosInfo, Network, NodeDetails, NodeId, NodeStats, Phase, Settings, Status,
    explorer::{
        BlockId, ExplorerCall, ExplorerErrorKind, ExplorerReply, ExplorerRequest, ExplorerResponse,
        ExplorerRoute, Hash32, LedgerCheck, LiveState, NotFoundHint, PageKind, Resolution,
        SecretKind, TxStatus,
    },
};
use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::watch;

use super::{
    ExplorerConfig, ExplorerHandle, ExplorerInputs,
    source::{ChainSource, ReadCall, ReadResult, SourceError},
    spawn_with,
};

const NOW: i64 = 1_800_000_000;
const TIP: u32 = 3_551_000;
const LIMIT: Duration = Duration::from_secs(10);

/// Calls per method, in [`slot`]'s order.
type Counts = Arc<[AtomicU64; 6]>;

fn slot(call: ReadCall) -> usize {
    match call {
        ReadCall::Best => 0,
        ReadCall::Block(_) => 1,
        ReadCall::BlockHash(_) => 2,
        ReadCall::Header(_) => 3,
        ReadCall::Tx(_) => 4,
        ReadCall::Subsidy(_) => 5,
    }
}

/// A source over a synthetic chain the test can mine and reorganise, that counts its calls and
/// holds them while its gate is closed. Every answer goes through zebrad's JSON and back, as
/// an answer from zebrad's RPC is decoded.
#[derive(Clone)]
struct Synth {
    chain: watch::Receiver<SynthChain>,
    gate: watch::Receiver<bool>,
    counts: Counts,
}

/// `value` as zebrad writes it and the explorer reads it back.
fn through_json<T: Serialize + DeserializeOwned>(value: T) -> T {
    serde_json::from_value(serde_json::to_value(value).expect("serializes")).expect("decodes")
}

impl ChainSource for Synth {
    async fn read(&self, call: ReadCall) -> Result<ReadResult, SourceError> {
        self.counts[slot(call)].fetch_add(1, Ordering::SeqCst);
        let mut gate = self.gate.clone();
        let _ = gate.wait_for(|open| *open).await;
        let chain = self.chain.borrow().clone();
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
        };
        found.ok_or(SourceError::NotFound)
    }
}

struct Harness {
    explorer: ExplorerHandle,
    chain: watch::Sender<SynthChain>,
    gate: watch::Sender<bool>,
    counts: Counts,
    status: watch::Sender<Status>,
    stats: watch::Sender<Option<NodeStats>>,
    _details: watch::Sender<Option<NodeDetails>>,
    _settings: watch::Sender<Settings>,
}

fn stats_for(chain: &SynthChain) -> NodeStats {
    NodeStats {
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
    let counts: Counts = Arc::new(std::array::from_fn(|_| AtomicU64::new(0)));
    let (status_tx, status) = watch::channel(running());
    let (stats_tx, stats) = watch::channel(Some(stats));
    let (details_tx, details) = watch::channel(Some(details));
    let (settings_tx, settings) = watch::channel(Settings::default());
    let source = Synth {
        chain: chain_rx,
        gate: gate_rx,
        counts: counts.clone(),
    };
    let mut explorer_config = ExplorerConfig::new(PathBuf::from("/nonexistent/.cookie"));
    config(&mut explorer_config);
    let explorer = spawn_with(
        ExplorerInputs {
            status,
            stats,
            details,
            settings,
        },
        explorer_config,
        move |_, _| Ok(source.clone()),
    );
    Harness {
        explorer,
        chain: chain_tx,
        gate: gate_tx,
        counts,
        status: status_tx,
        stats: stats_tx,
        _details: details_tx,
        _settings: settings_tx,
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
            self.explorer.call(&mainnet(), ExplorerCall { request }),
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
}

fn delta(after: [u64; 6], before: [u64; 6]) -> [u64; 6] {
    std::array::from_fn(|index| after[index] - before[index])
}

#[tokio::test]
async fn the_chain_page_stays_within_its_budget_and_a_revisit_is_free() {
    let harness = harness();
    let ExplorerResponse::Chain(page) = harness.reply(ExplorerRequest::Chain).await else {
        panic!("a chain page");
    };
    assert_eq!(page.blocks.len(), 25);
    assert_eq!(page.blocks[0].height, TIP, "newest first");
    assert_eq!(page.blocks[24].height, TIP - 24);
    assert!(page.blocks.iter().all(|block| block.ledger.fees.is_some()));
    assert!(page.average_interval.is_some());
    assert!(page.burned_since_nu7.is_some(), "the tip is past NU7");
    assert_eq!(page.target_spacing, 25);
    let [best, blocks, hashes, headers, txs, subsidies] = harness.counts();
    assert_eq!(best, 1, "one tip");
    assert!(blocks <= 27, "26 blocks and the one before NU7: {blocks}");
    assert_eq!(hashes, 0);
    assert_eq!(headers, 1);
    assert_eq!(txs, 25, "one coinbase per row");
    assert!(subsidies <= 2);
    // Again, a moment later: everything from the cache.
    let before = harness.counts();
    let ExplorerResponse::Chain(again) = harness.reply(ExplorerRequest::Chain).await else {
        panic!("a chain page");
    };
    assert_eq!(again, page);
    assert_eq!(delta(harness.counts(), before), [0; 6]);
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
    let ExplorerResponse::BlockTxs(txids) = harness
        .reply(ExplorerRequest::BlockTxs {
            hash: page.summary.hash,
            offset: 0,
            limit: 200,
        })
        .await
    else {
        panic!("a page of txids");
    };
    assert_eq!(txids.txids, page.txids.txids);
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
async fn a_transaction_page_is_one_read() {
    let harness = harness();
    let txid = {
        let chain = harness.chain.borrow();
        chain.block(BlockId::Height(TIP - 10)).unwrap().tx[1]
    };
    let ExplorerResponse::Tx(page) = harness.reply(ExplorerRequest::Tx { txid }).await else {
        panic!("a transaction page");
    };
    assert_eq!(page.txid, txid);
    assert!(matches!(page.status, TxStatus::Mined { height, .. } if height == TIP - 10));
    assert_eq!(harness.counts(), [0, 0, 0, 0, 1, 0]);
    let unknown = harness
        .ask(ExplorerRequest::Tx {
            txid: Hash32([7; 32]),
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
async fn only_a_running_node_on_the_served_network_answers() {
    let harness = harness();
    let call = |node: &'static str| {
        let explorer = harness.explorer.clone();
        async move {
            explorer
                .call(
                    &NodeId::new(node).unwrap(),
                    ExplorerCall {
                        request: ExplorerRequest::Chain,
                    },
                )
                .await
        }
    };
    let other = call("zebrad-testnet").await.expect_err("the other network");
    assert_eq!(other.kind, ExplorerErrorKind::NodeNotRunning);
    assert!(other.message.contains("next start"), "{}", other.message);
    let unknown = call("zakura-mainnet").await.expect_err("another node");
    assert_eq!(unknown.kind, ExplorerErrorKind::UnknownNode);

    harness.status.send_replace(Status::default());
    tokio::time::sleep(Duration::from_millis(20)).await;
    let stopped = harness
        .ask(ExplorerRequest::Chain)
        .await
        .expect_err("stopped");
    assert_eq!(stopped.kind, ExplorerErrorKind::NodeNotRunning);
    let ExplorerResponse::Status(live) = harness.reply(ExplorerRequest::Status).await else {
        panic!("a status");
    };
    assert_eq!(live.state, LiveState::NotRunning);
    assert_eq!(harness.counts(), [0; 6], "nothing was read");

    // zebrad comes back on Testnet: the Testnet id answers and the Mainnet id waits.
    harness
        .chain
        .send_replace(SynthChain::new(Network::Testnet, 4_500_000, NOW));
    harness.stats.send_modify(|stats| {
        if let Some(stats) = stats {
            stats.chain = "test".into();
        }
    });
    harness.status.send_replace(running());
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(call("zebrad-testnet").await.is_ok());
    let mainnet = harness
        .ask(ExplorerRequest::Chain)
        .await
        .expect_err("now Testnet");
    assert_eq!(mainnet.kind, ExplorerErrorKind::NodeNotRunning);
    assert_eq!(
        harness.explorer.live().borrow().node.as_str(),
        "zebrad-testnet"
    );
}

#[tokio::test]
async fn a_reorganisation_at_the_tip_is_followed_once() {
    let harness = harness_with(SynthChain::new(Network::Mainnet, TIP, NOW), |config| {
        config.tip_poll = Duration::from_millis(30);
    });
    let ExplorerResponse::Chain(_) = harness.reply(ExplorerRequest::Chain).await else {
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
    let ExplorerResponse::Chain(page) = harness.reply(ExplorerRequest::Chain).await else {
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
    let ExplorerResponse::Chain(_) = harness.reply(ExplorerRequest::Chain).await else {
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
    let ExplorerResponse::Chain(_) = harness.reply(ExplorerRequest::Chain).await else {
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
    let ExplorerResponse::Chain(_) = harness.reply(ExplorerRequest::Chain).await else {
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
    let ExplorerResponse::Tx(before) = harness.reply(ExplorerRequest::Tx { txid }).await else {
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
    let ExplorerResponse::Tx(after) = harness.reply(ExplorerRequest::Tx { txid }).await else {
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
    let ExplorerResponse::Chain(_) = harness.reply(ExplorerRequest::Chain).await else {
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
    let ExplorerResponse::Chain(_) = harness.reply(ExplorerRequest::Chain).await else {
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
    let ExplorerResponse::Chain(_) = harness.reply(ExplorerRequest::Chain).await else {
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
    let ExplorerResponse::Chain(_) = harness.reply(ExplorerRequest::Chain).await else {
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
                    async move { explorer.call(&mainnet(), ExplorerCall { request }).await },
                )
            })
            .collect();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(harness.counts()[1] - before, u64::from(limit));
        harness.gate.send_replace(true);
        for page in pages {
            page.await.expect("no panic").expect("a block page");
        }
    }
}

#[tokio::test]
async fn a_new_run_of_zebrad_keeps_the_cache() {
    let harness = harness();
    let ExplorerResponse::Chain(first) = harness.reply(ExplorerRequest::Chain).await else {
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
    let ExplorerResponse::Chain(again) = harness.reply(ExplorerRequest::Chain).await else {
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
async fn a_hidden_explorer_shrinks_its_cache() {
    let harness = harness_with(SynthChain::new(Network::Mainnet, TIP, NOW), |config| {
        config.idle_after = Duration::from_millis(20);
        config.idle_cache_bytes = 0;
        config.tip_poll = Duration::from_millis(10);
    });
    let id = BlockId::Hash(harness.chain.borrow().hash(TIP - 2_000).unwrap());
    harness.reply(ExplorerRequest::Block { id }).await;
    let before = harness.counts();
    harness.reply(ExplorerRequest::Block { id }).await;
    assert_eq!(delta(harness.counts(), before), [0; 6], "from the cache");
    // Shown, then hidden for longer than `idle_after`.
    harness.show(PageKind::Block).await;
    harness
        .reply(ExplorerRequest::Interest {
            visible: false,
            page: PageKind::Block,
        })
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let before = harness.counts();
    harness.reply(ExplorerRequest::Block { id }).await;
    assert_eq!(
        delta(harness.counts(), before),
        [0, 2, 0, 0, 1, 1],
        "read again"
    );
}

#[tokio::test]
async fn a_height_near_the_tip_is_read_again_once_the_check_is_stale() {
    let fresh = Duration::from_millis(100);
    let harness = harness_with(SynthChain::new(Network::Mainnet, TIP, NOW), |config| {
        config.fresh = fresh;
    });
    let ExplorerResponse::Chain(_) = harness.reply(ExplorerRequest::Chain).await else {
        panic!("a chain page");
    };
    let (near, deep) = (BlockId::Height(TIP - 5), BlockId::Height(TIP - 2_000));
    for id in [near, deep] {
        harness.reply(ExplorerRequest::Block { id }).await;
    }
    let before = harness.counts();
    for id in [near, deep] {
        harness.reply(ExplorerRequest::Block { id }).await;
    }
    assert_eq!(delta(harness.counts(), before), [0; 6], "from the cache");
    tokio::time::sleep(fresh + Duration::from_millis(50)).await;
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

#[tokio::test]
async fn the_follower_checks_the_tip_at_every_poll() {
    let poll = Duration::from_millis(100);
    let harness = harness_with(SynthChain::new(Network::Mainnet, TIP, NOW), |config| {
        config.tip_poll = poll;
    });
    harness
        .reply(ExplorerRequest::Interest {
            visible: true,
            page: PageKind::Chain,
        })
        .await;
    tokio::time::sleep(poll * 3).await;
    let before = harness.counts()[0];
    tokio::time::sleep(poll * 15).await;
    let checks = harness.counts()[0] - before;
    // One check a poll; a check that skipped every other poll would make about seven.
    assert!(checks >= 11, "{checks} tip checks in 15 polls");
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
    harness.reply(ExplorerRequest::Tx { txid }).await;
    harness
        .reply(ExplorerRequest::Resolve {
            query: block.to_hex(),
        })
        .await;
    let _ = harness
        .ask(ExplorerRequest::Tx {
            txid: Hash32([0x5a; 32]),
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
