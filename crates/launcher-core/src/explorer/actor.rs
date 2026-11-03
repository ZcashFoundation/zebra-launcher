//! The explorer actor: one task owns the cache, the chain index, the scheduler and the
//! permits. Request tasks, read tasks and the tip follower talk to it over one channel, so
//! nothing is shared and nothing is locked.

use std::{
    collections::HashMap,
    net::SocketAddr,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use explorer_model::{
    shapes::{BestBlock, BlockV1, Header, Subsidy, Tx},
    upgrades::UpgradeHeights,
};
use launcher_types::{
    Network, NodeId, NodeKind, Phase,
    explorer::{
        BlockHash, BlockId, ChainView, ExplorerCall, ExplorerError, ExplorerErrorKind,
        ExplorerLive, ExplorerLiveEvent, ExplorerReply, ExplorerRequest, ExplorerResponse,
        LiveState, ReorgNotice, TxId,
    },
};
use tokio::{
    sync::{Semaphore, mpsc, oneshot, watch},
    time::MissedTickBehavior,
};

use super::{
    ExplorerConfig, ExplorerInputs, assemble,
    cache::Lru,
    chain_index::{ChainIndex, FRESH, ForkSearch, REORG_WINDOW, TipUpdate},
    sched::{Lane, Limits, Scheduler},
    source::{ChainSource, ReadCall, ReadResult, SourceError},
};
use crate::rpc::RpcError;

/// What an [`ExplorerHandle`](super::ExplorerHandle) asks.
pub(super) enum Command {
    Call {
        node: NodeId,
        call: ExplorerCall,
        reply: oneshot::Sender<Result<ExplorerReply, ExplorerError>>,
    },
}

type ReadReply = oneshot::Sender<Result<ReadResult, ExplorerError>>;
type TipReply = oneshot::Sender<Result<ChainView, ExplorerError>>;

/// What request tasks, read tasks and the follower tell the actor.
pub(super) enum Internal {
    /// A read a request needs.
    Read {
        call: ReadCall,
        lane: Lane,
        reply: ReadReply,
    },
    /// The tip, checked within `max_age`.
    Tip { max_age: Duration, reply: TipReply },
    /// A read task ended.
    Done {
        call: ReadCall,
        lane: Lane,
        result: Result<ReadResult, SourceError>,
    },
    /// The follower checked the tip.
    Followed {
        generation: u64,
        result: Result<TipUpdate, ExplorerError>,
    },
}

/// A request task's line to the actor.
#[derive(Clone)]
pub(super) struct Fetcher {
    tx: mpsc::UnboundedSender<Internal>,
}

/// A read that was asked for and not yet awaited, so several go out together.
pub(super) struct Pending(oneshot::Receiver<Result<ReadResult, ExplorerError>>);

impl Pending {
    async fn result(self) -> Result<ReadResult, ExplorerError> {
        self.0.await.map_err(|_| stopped())?
    }

    pub(super) async fn block(self) -> Result<Arc<BlockV1>, ExplorerError> {
        match self.result().await? {
            ReadResult::Block(block) => Ok(block),
            _ => Err(mismatch()),
        }
    }

    pub(super) async fn tx(self) -> Result<Arc<Tx>, ExplorerError> {
        match self.result().await? {
            ReadResult::Tx(tx) => Ok(tx),
            _ => Err(mismatch()),
        }
    }

    pub(super) async fn subsidy(self) -> Result<Arc<Subsidy>, ExplorerError> {
        match self.result().await? {
            ReadResult::Subsidy(subsidy) => Ok(subsidy),
            _ => Err(mismatch()),
        }
    }

    pub(super) async fn header(self) -> Result<Header, ExplorerError> {
        match self.result().await? {
            ReadResult::Header(header) => Ok(header),
            _ => Err(mismatch()),
        }
    }

    async fn best(self) -> Result<BestBlock, ExplorerError> {
        match self.result().await? {
            ReadResult::Best(best) => Ok(best),
            _ => Err(mismatch()),
        }
    }

    async fn hash(self) -> Result<BlockHash, ExplorerError> {
        match self.result().await? {
            ReadResult::BlockHash(hash) => Ok(hash),
            _ => Err(mismatch()),
        }
    }
}

impl Fetcher {
    /// Asks for `call` now; await the answer later.
    pub(super) fn request(&self, call: ReadCall, lane: Lane) -> Pending {
        let (reply, response) = oneshot::channel();
        // A closed channel drops `reply`, which reads as a stopped explorer.
        let _ = self.tx.send(Internal::Read { call, lane, reply });
        Pending(response)
    }

    pub(super) fn block(&self, id: BlockId) -> Pending {
        self.request(ReadCall::Block(id), Lane::Interactive)
    }

    pub(super) fn tx(&self, txid: TxId) -> Pending {
        self.request(ReadCall::Tx(txid), Lane::Interactive)
    }

    pub(super) fn subsidy(&self, height: u32) -> Pending {
        self.request(ReadCall::Subsidy(height), Lane::Interactive)
    }

    pub(super) fn header(&self, id: BlockId) -> Pending {
        self.request(ReadCall::Header(id), Lane::Interactive)
    }

    /// The tip, checked against the node within `max_age`.
    pub(super) async fn tip(&self, max_age: Duration) -> Result<ChainView, ExplorerError> {
        let (reply, response) = oneshot::channel();
        let _ = self.tx.send(Internal::Tip { max_age, reply });
        response.await.map_err(|_| stopped())?
    }
}

fn stopped() -> ExplorerError {
    ExplorerError::new(ExplorerErrorKind::Internal, "the explorer has stopped")
}

fn mismatch() -> ExplorerError {
    ExplorerError::new(
        ExplorerErrorKind::Internal,
        "a read answered the wrong call",
    )
}

fn not_running() -> ExplorerError {
    ExplorerError::new(
        ExplorerErrorKind::NodeNotRunning,
        "zebrad is not running, or has not opened its RPC port yet",
    )
}

/// The error a page shows for a failed read.
pub(super) fn read_error(err: SourceError) -> ExplorerError {
    match err {
        SourceError::NotFound => {
            ExplorerError::new(ExplorerErrorKind::NotFound, "Your node does not have it")
        }
        SourceError::Rpc { code } => ExplorerError::new(
            ExplorerErrorKind::Rpc { code },
            format!("zebrad answered with error {code}"),
        ),
        SourceError::Timeout => {
            ExplorerError::new(ExplorerErrorKind::Timeout, "zebrad did not answer in time")
        }
        SourceError::Unavailable(why) => ExplorerError::new(
            ExplorerErrorKind::NodeNotRunning,
            format!("zebrad is not answering: {why}"),
        ),
        SourceError::Invalid => ExplorerError::new(
            ExplorerErrorKind::Internal,
            "zebrad's answer could not be read",
        ),
    }
}

/// What a request task knows when it starts.
pub(super) struct Context {
    pub(super) fetch: Fetcher,
    pub(super) network: Network,
    pub(super) heights: UpgradeHeights,
    /// The tip as the index had it.
    pub(super) chain: Option<ChainView>,
    /// The node's own height and zebrad's estimate of the network's.
    pub(super) node_height: Option<u32>,
    pub(super) estimated_height: Option<u32>,
    pub(super) syncing: bool,
    pub(super) end_of_support: Option<u64>,
}

/// Where cached answers live: blocks by hash, transactions by id, subsidies by height, and
/// headers by height: for good at final heights, within the epoch above them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum CacheKey {
    Block(BlockHash),
    Tx(TxId),
    Subsidy(u32),
    FinalHeader(u32),
    Header { height: u32, epoch: u64 },
}

/// The source of one zebrad run.
struct Run<S> {
    addr: SocketAddr,
    started_at: Option<SystemTime>,
    source: S,
}

/// Reports a read's end to the actor, also when its task panics, so the lane's count settles.
struct DoneGuard {
    tx: mpsc::UnboundedSender<Internal>,
    call: ReadCall,
    lane: Lane,
    sent: bool,
}

impl DoneGuard {
    fn finish(mut self, result: Result<ReadResult, SourceError>) {
        self.sent = true;
        let _ = self.tx.send(Internal::Done {
            call: self.call,
            lane: self.lane,
            result,
        });
    }
}

impl Drop for DoneGuard {
    fn drop(&mut self) {
        if !self.sent {
            let _ = self.tx.send(Internal::Done {
                call: self.call,
                lane: self.lane,
                result: Err(SourceError::Unavailable("the read stopped".into())),
            });
        }
    }
}

struct Actor<S, F> {
    config: ExplorerConfig,
    new_source: F,
    inputs: ExplorerInputs,
    run: Option<Run<S>>,
    cache: Lru<CacheKey, ReadResult>,
    index: ChainIndex,
    sched: Scheduler<ReadCall>,
    permits: Arc<Semaphore>,
    inflight: HashMap<ReadCall, Vec<ReadReply>>,
    tip_waiters: Vec<TipReply>,
    following: bool,
    /// Bumped when the chain is forgotten, so a follower that started before is ignored.
    generation: u64,
    visible: bool,
    hidden_since: Option<Instant>,
    /// The network zebrad last reported, kept while it is stopped.
    reported: Option<Network>,
    network: Network,
    reorg: Option<ReorgNotice>,
    live: watch::Sender<ExplorerLiveEvent>,
    internal: mpsc::UnboundedSender<Internal>,
}

/// Starts the actor. Returns its command channel and its live watch.
pub(super) fn start<S: ChainSource>(
    inputs: ExplorerInputs,
    config: ExplorerConfig,
    new_source: impl Fn(SocketAddr, PathBuf) -> Result<S, RpcError> + Send + 'static,
) -> (mpsc::Sender<Command>, watch::Receiver<ExplorerLiveEvent>) {
    let network = inputs.settings.borrow().network;
    let (live, live_rx) = watch::channel(ExplorerLiveEvent {
        node: node_id(network),
        live: ExplorerLive::default(),
    });
    let (commands_tx, commands) = mpsc::channel(64);
    let (internal, internal_rx) = mpsc::unbounded_channel();
    let actor = Actor {
        cache: Lru::new(config.cache_bytes),
        config,
        new_source,
        inputs,
        run: None,
        index: ChainIndex::default(),
        sched: Scheduler::new(Limits::NORMAL),
        permits: Arc::new(Semaphore::new(Limits::NORMAL.total as usize)),
        inflight: HashMap::new(),
        tip_waiters: Vec::new(),
        following: false,
        generation: 0,
        visible: false,
        hidden_since: None,
        reported: None,
        network,
        reorg: None,
        live,
        internal,
    };
    tokio::spawn(actor.run(commands, internal_rx));
    (commands_tx, live_rx)
}

/// The id the explorer serves while zebrad runs on `network`.
fn node_id(network: Network) -> NodeId {
    NodeId::suggest(NodeKind::Zebrad, network, &Vec::new())
}

fn network_of(chain: &str) -> Option<Network> {
    match chain {
        "main" => Some(Network::Mainnet),
        "test" => Some(Network::Testnet),
        _ => None,
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

impl<S, F> Actor<S, F>
where
    S: ChainSource,
    F: Fn(SocketAddr, PathBuf) -> Result<S, RpcError> + Send + 'static,
{
    async fn run(
        mut self,
        mut commands: mpsc::Receiver<Command>,
        mut internal: mpsc::UnboundedReceiver<Internal>,
    ) {
        let mut tick = tokio::time::interval(self.config.tip_poll);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let (mut stats_open, mut details_open, mut settings_open) = (true, true, true);
        self.refresh();
        loop {
            tokio::select! {
                command = commands.recv() => match command {
                    Some(command) => self.on_command(command),
                    None => return,
                },
                Some(message) = internal.recv() => self.on_internal(message),
                changed = self.inputs.status.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    self.refresh();
                }
                changed = self.inputs.stats.changed(), if stats_open => {
                    stats_open = changed.is_ok();
                    self.refresh();
                }
                changed = self.inputs.details.changed(), if details_open => {
                    details_open = changed.is_ok();
                }
                changed = self.inputs.settings.changed(), if settings_open => {
                    settings_open = changed.is_ok();
                    self.refresh();
                }
                _ = tick.tick() => self.on_tick(),
            }
        }
    }

    fn fetcher(&self) -> Fetcher {
        Fetcher {
            tx: self.internal.clone(),
        }
    }

    fn at_tip(&self) -> bool {
        self.inputs.stats.borrow().as_ref().is_some_and(|stats| {
            launcher_types::at_tip(
                stats.height,
                stats.estimated_height,
                stats.verification_progress,
            )
        })
    }

    fn chain_view(&self) -> Option<ChainView> {
        self.index.tip().map(|(height, hash)| ChainView {
            height,
            hash,
            epoch: self.index.epoch(),
            stale: self.run.is_none(),
        })
    }

    fn heights(&self) -> UpgradeHeights {
        match self.inputs.details.borrow().as_ref() {
            Some(details) if !details.upgrades.is_empty() => {
                UpgradeHeights::from_node(&details.upgrades, self.network)
            }
            _ => UpgradeHeights::defaults(self.network),
        }
    }

    /// Follows a change of the status, the statistics or the settings: the source, the
    /// network, the limits and what the explorer publishes.
    fn refresh(&mut self) {
        let status = self.inputs.status.borrow().clone();
        let wanted = match status.phase {
            Phase::Running => status.rpc_addr,
            _ => None,
        };
        match wanted {
            Some(addr) => {
                let same = self
                    .run
                    .as_ref()
                    .is_some_and(|run| run.addr == addr && run.started_at == status.started_at);
                if !same {
                    // A new run: a new source with a fresh cookie; the cache is kept.
                    match (self.new_source)(addr, self.config.cookie_file.clone()) {
                        Ok(source) => {
                            self.run = Some(Run {
                                addr,
                                started_at: status.started_at,
                                source,
                            });
                        }
                        Err(err) => {
                            tracing::warn!(%err, "the explorer could not create its RPC client");
                            self.stop_reading();
                        }
                    }
                }
            }
            None => self.stop_reading(),
        }

        let reported = self
            .inputs
            .stats
            .borrow()
            .as_ref()
            .and_then(|stats| network_of(&stats.chain));
        if reported.is_some() {
            self.reported = reported;
        }
        let network = self
            .reported
            .unwrap_or_else(|| self.inputs.settings.borrow().network);
        if network != self.network {
            // Another chain: nothing known about this one applies.
            self.network = network;
            self.cache.clear();
            self.index.clear();
            self.reorg = None;
            self.generation += 1;
        }
        self.sched.set_limits(if self.at_tip() {
            Limits::NORMAL
        } else {
            Limits::SYNCING
        });
        self.publish();
    }

    /// zebrad stopped: queued reads fail; reads in flight report back on their own.
    fn stop_reading(&mut self) {
        self.run = None;
        for call in self.sched.clear_queue() {
            for waiter in self.inflight.remove(&call).unwrap_or_default() {
                let _ = waiter.send(Err(not_running()));
            }
        }
        for waiter in self.tip_waiters.drain(..) {
            let _ = waiter.send(Err(not_running()));
        }
    }

    fn publish(&mut self) {
        let state = if self.run.is_none() {
            LiveState::NotRunning
        } else {
            match self.inputs.stats.borrow().as_ref() {
                None => LiveState::NotRunning,
                Some(stats)
                    if launcher_types::at_tip(
                        stats.height,
                        stats.estimated_height,
                        stats.verification_progress,
                    ) =>
                {
                    LiveState::AtTip
                }
                Some(stats) => LiveState::Syncing {
                    height: u32::try_from(stats.height).unwrap_or(u32::MAX),
                    estimated: u32::try_from(stats.estimated_height).unwrap_or(u32::MAX),
                },
            }
        };
        let next = ExplorerLiveEvent {
            node: node_id(self.network),
            live: ExplorerLive {
                state,
                tip: self.chain_view(),
                reorg: self.reorg,
            },
        };
        self.live.send_if_modified(|current| {
            let changed = *current != next;
            if changed {
                *current = next;
            }
            changed
        });
    }

    /// The node ids this explorer answers: the one of the network zebrad runs on. The other
    /// network's zebrad id is the backend's node too, after its next start.
    fn check_node(&self, node: &NodeId) -> Result<(), ExplorerError> {
        if *node == node_id(self.network) {
            return Ok(());
        }
        let other = match self.network {
            Network::Mainnet => Network::Testnet,
            Network::Testnet => Network::Mainnet,
        };
        if *node == node_id(other) {
            return Err(ExplorerError::new(
                ExplorerErrorKind::NodeNotRunning,
                format!(
                    "zebrad runs on {}; it changes network at its next start",
                    self.network
                ),
            ));
        }
        Err(ExplorerError::new(
            ExplorerErrorKind::UnknownNode,
            "this node has no explorer",
        ))
    }

    fn on_command(&mut self, command: Command) {
        let Command::Call { node, call, reply } = command;
        if let Err(err) = self.check_node(&node) {
            let _ = reply.send(Err(err));
            return;
        }
        let chain = self.chain_view();
        match call.request {
            ExplorerRequest::Status => {
                let live = self.live.borrow().live.clone();
                let _ = reply.send(Ok(ExplorerReply {
                    chain,
                    response: ExplorerResponse::Status(live),
                }));
            }
            ExplorerRequest::Interest { visible, .. } => {
                self.set_visible(visible);
                let _ = reply.send(Ok(ExplorerReply {
                    chain,
                    response: ExplorerResponse::Done,
                }));
            }
            request => {
                if self.run.is_none() {
                    let _ = reply.send(Err(not_running()));
                    return;
                }
                let stats = self.inputs.stats.borrow().clone();
                let context = Context {
                    fetch: self.fetcher(),
                    network: self.network,
                    heights: self.heights(),
                    chain,
                    node_height: stats
                        .as_ref()
                        .and_then(|stats| u32::try_from(stats.height).ok()),
                    estimated_height: stats
                        .as_ref()
                        .and_then(|stats| u32::try_from(stats.estimated_height).ok()),
                    syncing: !self.at_tip(),
                    end_of_support: stats
                        .as_ref()
                        .and_then(|stats| stats.eos.as_ref())
                        .map(|eos| eos.block_height),
                };
                tokio::spawn(async move {
                    let _ = reply.send(assemble::handle(request, context).await);
                });
            }
        }
    }

    fn set_visible(&mut self, visible: bool) {
        if visible == self.visible {
            return;
        }
        self.visible = visible;
        if visible {
            self.hidden_since = None;
            self.cache.set_budget(self.config.cache_bytes);
            if self.at_tip() && self.run.is_some() {
                self.follow();
            }
        } else {
            self.hidden_since = Some(Instant::now());
        }
    }

    fn on_tick(&mut self) {
        let now = Instant::now();
        if self.visible
            && self.run.is_some()
            && self.at_tip()
            && !self.index.checked_within(self.config.tip_poll, now)
        {
            self.follow();
        }
        if let Some(since) = self.hidden_since
            && now.saturating_duration_since(since) >= self.config.idle_after
        {
            self.cache.set_budget(self.config.idle_cache_bytes);
        }
    }

    fn cache_key(&self, call: ReadCall, now: Instant) -> Option<CacheKey> {
        match call {
            ReadCall::Block(BlockId::Hash(hash)) => Some(CacheKey::Block(hash)),
            ReadCall::Block(BlockId::Height(height)) => {
                self.index.hash_at(height, now).map(CacheKey::Block)
            }
            ReadCall::Tx(txid) => Some(CacheKey::Tx(txid)),
            ReadCall::Subsidy(height) => Some(CacheKey::Subsidy(height)),
            ReadCall::Header(BlockId::Height(height)) => self.header_key(height, now),
            ReadCall::Header(BlockId::Hash(_)) | ReadCall::Best | ReadCall::BlockHash(_) => None,
        }
    }

    /// A header's place in the cache: by height at a final height, by height and epoch while
    /// the tip was checked a moment ago, else none.
    fn header_key(&self, height: u32, now: Instant) -> Option<CacheKey> {
        let (tip, _) = self.index.tip()?;
        if height > tip {
            None
        } else if tip - height >= REORG_WINDOW {
            Some(CacheKey::FinalHeader(height))
        } else if self.index.checked_within(FRESH, now) {
            Some(CacheKey::Header {
                height,
                epoch: self.index.epoch(),
            })
        } else {
            None
        }
    }

    fn on_internal(&mut self, message: Internal) {
        match message {
            Internal::Read { call, lane, reply } => self.read(call, lane, reply),
            Internal::Tip { max_age, reply } => {
                if let Some(view) = self
                    .chain_view()
                    .filter(|_| self.index.checked_within(max_age, Instant::now()))
                {
                    let _ = reply.send(Ok(view));
                    return;
                }
                if self.run.is_none() {
                    let _ = reply.send(Err(not_running()));
                    return;
                }
                self.tip_waiters.push(reply);
                self.follow();
            }
            Internal::Done { call, lane, result } => {
                self.sched.done(lane);
                let waiters = self.inflight.remove(&call).unwrap_or_default();
                let result = match result {
                    Ok(value) => {
                        self.remember(call, &value);
                        Ok(value)
                    }
                    Err(err) => Err(read_error(err)),
                };
                for waiter in waiters {
                    let _ = waiter.send(result.clone());
                }
                self.pump();
            }
            Internal::Followed { generation, result } => {
                self.following = false;
                if generation != self.generation {
                    // The chain was forgotten meanwhile: check again for whoever waits.
                    if !self.tip_waiters.is_empty() {
                        self.follow();
                    }
                    return;
                }
                let result = result.map(|update| self.apply(update));
                for waiter in self.tip_waiters.drain(..) {
                    let _ = waiter.send(result.clone());
                }
                self.publish();
            }
        }
    }

    fn read(&mut self, call: ReadCall, lane: Lane, reply: ReadReply) {
        if self.run.is_none() {
            let _ = reply.send(Err(not_running()));
            return;
        }
        let now = Instant::now();
        let key = self.cache_key(call, now);
        if let Some(value) = key.and_then(|key| self.cache.get(&key)) {
            let _ = reply.send(Ok(value.clone()));
            return;
        }
        // A height the index knows is read by its hash, so it joins reads of that hash.
        let call = match (call, key) {
            (ReadCall::Block(BlockId::Height(_)), Some(CacheKey::Block(hash))) => {
                ReadCall::Block(BlockId::Hash(hash))
            }
            _ => call,
        };
        match self.inflight.get_mut(&call) {
            Some(waiters) => {
                waiters.push(reply);
                if lane == Lane::Interactive {
                    self.sched.promote(&call);
                }
            }
            None => {
                self.inflight.insert(call, vec![reply]);
                self.sched.push(call, lane);
                self.pump();
            }
        }
    }

    /// Starts every queued read the limits allow.
    fn pump(&mut self) {
        let Some(run) = &self.run else {
            return;
        };
        while let Some((call, lane)) = self.sched.next() {
            let Ok(permit) = self.permits.clone().try_acquire_owned() else {
                // Every permit is out: put the read back for the next report.
                self.sched.done(lane);
                self.sched.push(call, lane);
                break;
            };
            let source = run.source.clone();
            let guard = DoneGuard {
                tx: self.internal.clone(),
                call,
                lane,
                sent: false,
            };
            tokio::spawn(async move {
                let started = Instant::now();
                let result = source.read(call).await;
                tracing::debug!(
                    method = call.method(),
                    ms = started.elapsed().as_millis() as u64,
                    ok = result.is_ok(),
                    "explorer read"
                );
                // Free the permit before reporting, so the actor can start the next read.
                drop(permit);
                guard.finish(result);
            });
        }
    }

    /// Keeps what a read found: cacheable answers, and the hashes it shows at heights.
    fn remember(&mut self, call: ReadCall, value: &ReadResult) {
        match value {
            ReadResult::Block(block) => {
                if block.confirmations > 0
                    && let Some(height) = block.height
                {
                    self.index.learn(height, block.hash);
                    if let (Some(previous), Some(parent)) = (block.previous, height.checked_sub(1))
                    {
                        self.index.learn(parent, previous);
                    }
                }
                let bytes = 400 + 32 * block.tx.len();
                self.cache
                    .insert(CacheKey::Block(block.hash), value.clone(), bytes);
            }
            ReadResult::Tx(tx) => {
                // Only a transaction of a best-chain block keeps its place.
                let mined = tx.height.is_some_and(|height| height >= 0)
                    && tx
                        .confirmations
                        .is_some_and(|confirmations| confirmations > 0);
                if mined {
                    let bytes = 300 + 80 * (tx.vin.len() + tx.vout.len());
                    self.cache
                        .insert(CacheKey::Tx(tx.txid), value.clone(), bytes);
                }
            }
            ReadResult::Subsidy(_) => {
                if let ReadCall::Subsidy(height) = call {
                    self.cache
                        .insert(CacheKey::Subsidy(height), value.clone(), 600);
                }
            }
            ReadResult::Header(header) => {
                if matches!(call, ReadCall::Header(BlockId::Height(_)))
                    && let Some(key) = self.header_key(header.height, Instant::now())
                {
                    self.cache.insert(key, value.clone(), 120);
                }
            }
            ReadResult::BlockHash(hash) => {
                if let ReadCall::BlockHash(height) = call {
                    self.index.learn(height, *hash);
                }
            }
            ReadResult::Best(_) => {}
        }
    }

    /// Checks the tip with the node, once at a time.
    fn follow(&mut self) {
        if self.following || self.run.is_none() {
            return;
        }
        self.following = true;
        let fetch = self.fetcher();
        let old = self.index.tip();
        let window = self.index.window();
        let generation = self.generation;
        let tx = self.internal.clone();
        tokio::spawn(async move {
            let result = follow(fetch, old, window).await;
            let _ = tx.send(Internal::Followed { generation, result });
        });
    }

    /// Applies a checked tip; a reorganisation drops the transactions of the replaced blocks
    /// from the cache and is published.
    fn apply(&mut self, update: TipUpdate) -> ChainView {
        if let Some(replaced) = self.index.apply(update, Instant::now()) {
            let fork = i64::from(replaced.fork);
            self.cache.retain(|_, value| match value {
                ReadResult::Tx(tx) => tx.height.is_none_or(|height| height <= fork),
                _ => true,
            });
            self.reorg = Some(ReorgNotice {
                fork_height: replaced.fork,
                replaced: replaced.blocks,
                epoch: self.index.epoch(),
                at: unix_now(),
            });
            tracing::info!(
                blocks = replaced.blocks,
                "the explorer followed a reorganisation"
            );
        }
        self.chain_view().expect("a checked tip was just applied")
    }
}

/// Checks the node's tip against what the index knew: a block on top, a jump, or a
/// reorganisation, whose fork a binary search of the known heights finds.
async fn follow(
    fetch: Fetcher,
    old: Option<(u32, BlockHash)>,
    window: Vec<(u32, BlockHash)>,
) -> Result<TipUpdate, ExplorerError> {
    let best = fetch.request(ReadCall::Best, Lane::Prefetch).best().await?;
    let tip = (best.height, best.hash);
    let plain = |learned| TipUpdate {
        tip,
        fork: None,
        reset: false,
        learned,
    };
    let Some((height, hash)) = old else {
        return Ok(plain(Vec::new()));
    };
    if tip == (height, hash) {
        return Ok(plain(Vec::new()));
    }
    if best.height > height {
        if best.height - height > REORG_WINDOW {
            return Ok(TipUpdate {
                reset: true,
                ..plain(Vec::new())
            });
        }
        let block = fetch
            .request(ReadCall::Block(BlockId::Hash(best.hash)), Lane::Prefetch)
            .block()
            .await?;
        if best.height == height + 1 && block.previous == Some(hash) {
            return Ok(plain(vec![(height, hash)]));
        }
        let at_old = fetch
            .request(ReadCall::BlockHash(height), Lane::Prefetch)
            .hash()
            .await?;
        if at_old == hash {
            return Ok(plain(Vec::new()));
        }
    } else if window.contains(&tip) {
        // The node went back to a block the index knows: what was above it is gone.
        return Ok(TipUpdate {
            fork: Some(best.height),
            ..plain(Vec::new())
        });
    }
    let shared = height.min(best.height);
    let mut search = ForkSearch::new(window.into_iter().filter(|(at, _)| *at <= shared).collect());
    let mut learned = Vec::new();
    while let Some((at, known)) = search.probe() {
        let found = fetch
            .request(ReadCall::BlockHash(at), Lane::Prefetch)
            .hash()
            .await?;
        search.answer(found == known);
        learned.push((at, found));
    }
    // Every answer is the node's own hash at its height, so all of them hold.
    Ok(TipUpdate {
        fork: Some(search.fork()),
        ..plain(learned)
    })
}
