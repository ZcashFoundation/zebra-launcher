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
/// holds them while its gate is closed.
#[derive(Clone)]
struct Synth {
    chain: watch::Receiver<SynthChain>,
    gate: watch::Receiver<bool>,
    counts: Counts,
}

impl ChainSource for Synth {
    async fn read(&self, call: ReadCall) -> Result<ReadResult, SourceError> {
        self.counts[slot(call)].fetch_add(1, Ordering::SeqCst);
        let mut gate = self.gate.clone();
        let _ = gate.wait_for(|open| *open).await;
        let chain = self.chain.borrow().clone();
        let found = match call {
            ReadCall::Best => Some(ReadResult::Best(chain.best())),
            ReadCall::Block(id) => chain
                .block(id)
                .map(|block| ReadResult::Block(Arc::new(block))),
            ReadCall::BlockHash(height) => chain.hash(height).map(ReadResult::BlockHash),
            ReadCall::Header(id) => chain.header(id).map(ReadResult::Header),
            ReadCall::Tx(txid) => chain.tx(&txid).map(|tx| ReadResult::Tx(Arc::new(tx))),
            ReadCall::Subsidy(height) => Some(ReadResult::Subsidy(Arc::new(chain.subsidy(height)))),
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
