//! The agreement actor (docs/HERD.md, Actors and channels): one task per network runs the
//! engine's rounds ([`launcher_herd::herd::Engine`]) over the nodes' tips and publishes the
//! network's [`HerdView`] on a `watch`.
//!
//! ```text
//!   registry ── watch<Vec<MemberInput>> ──▶ agreement[network] ── watch<HerdView> ──▶ `herd`
//!   each node ── status, stats (best), spec ─▶ │ probes through TipSource
//!                       a tip or phase changed  │ (a Semaphore per node, a timeout each)
//! ```
//!
//! A round runs when a node's tip or phase changes (changes within 250 ms make one round)
//! and every 5 s for the engine's timers. Probes run concurrently, at most two per node, each
//! with a 3 s timeout, and their answers reach the engine as they arrive, so a slow node does
//! not hold up the others. A round whose tips moved while it ran runs once more before it is
//! published. No locks: the actor owns the engine and the sources.

use std::{
    collections::{BTreeMap, BTreeSet},
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};

use launcher_herd::herd::{Engine, MemberFacts, Now, Probe, ProbeResult};
use launcher_types::{
    Network, NodeId, NodeStats, Phase, Status,
    herd::{BlockRef, HerdView},
};
use tokio::{
    sync::{Semaphore, mpsc, watch},
    task::{JoinHandle, JoinSet},
    time::{Instant, MissedTickBehavior},
};

use super::tips::{RpcTipSource, TipSource};
use crate::{
    nodes::NodeSpec,
    registry::{NodeHandle, RegistryHandle},
};

/// The networks, each with a herd of its own.
pub const NETWORKS: [Network; 2] = [Network::Mainnet, Network::Testnet];

/// How the actors run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HerdConfig {
    /// A round at least this often, for the timers: the monitor's poll.
    pub every: Duration,
    /// Tip changes this close together make one round.
    pub coalesce: Duration,
    /// How long a probe may take.
    pub probe_timeout: Duration,
    /// Probes in flight per node, so they never queue behind the monitor's poll.
    pub per_node: usize,
}

impl Default for HerdConfig {
    fn default() -> Self {
        HerdConfig {
            every: Duration::from_secs(5),
            coalesce: Duration::from_millis(250),
            probe_timeout: Duration::from_secs(3),
            per_node: 2,
        }
    }
}

/// One node as the actors follow it.
#[derive(Clone, Debug)]
pub struct MemberInput {
    pub id: NodeId,
    /// Tells apart a node removed and added again under the same id.
    pub serial: u64,
    pub spec: watch::Receiver<NodeSpec>,
    pub status: watch::Receiver<Status>,
    pub stats: watch::Receiver<Option<NodeStats>>,
    pub cookie_file: PathBuf,
}

impl MemberInput {
    /// The member for a registry node.
    pub fn of(node: &NodeHandle) -> Self {
        MemberInput {
            id: node.id.clone(),
            serial: node.serial,
            spec: node.watch_spec(),
            status: node.supervisor.watch_status(),
            stats: node.stats.clone(),
            cookie_file: node.paths.rpc_cookie_file(),
        }
    }

    fn network(&self) -> Network {
        self.spec.borrow().network
    }

    fn tip(&self) -> Option<BlockRef> {
        self.stats
            .borrow()
            .as_ref()
            .and_then(|stats| stats.best.clone())
    }

    fn facts(&self) -> MemberFacts {
        let spec = self.spec.borrow().clone();
        MemberFacts {
            id: self.id.clone(),
            name: spec.name,
            kind: spec.kind,
            program: spec.program,
            phase: self.status.borrow().phase.clone(),
            stats: self.stats.borrow().clone(),
        }
    }
}

/// The herds' views. Cheap to clone; the actors stop when every handle and every receiver is
/// gone.
#[derive(Clone, Debug)]
pub struct HerdHandle {
    views: Vec<(Network, watch::Receiver<HerdView>)>,
}

impl HerdHandle {
    /// `network`'s view, following its changes.
    pub fn watch(&self, network: Network) -> watch::Receiver<HerdView> {
        self.views
            .iter()
            .find(|(candidate, _)| *candidate == network)
            .map(|(_, view)| view.clone())
            .expect("every network has an actor")
    }

    /// Every network's view now, Mainnet first: the `get_herd` snapshot.
    pub fn views(&self) -> Vec<HerdView> {
        self.views
            .iter()
            .map(|(_, view)| view.borrow().clone())
            .collect()
    }
}

/// Starts the actors over the registry's nodes and their RPC. Must be called inside a Tokio
/// runtime.
pub fn spawn(registry: &RegistryHandle, config: HerdConfig) -> HerdHandle {
    spawn_with(members_of(registry), config, |addr, cookie: &Path| {
        RpcTipSource::new(addr, cookie.to_path_buf()).ok()
    })
}

/// The registry's nodes as members, kept up to date by a task until the registry or every
/// receiver is gone.
pub fn members_of(registry: &RegistryHandle) -> watch::Receiver<Vec<MemberInput>> {
    let mut nodes = registry.watch_nodes();
    let members = |nodes: &[NodeHandle]| nodes.iter().map(MemberInput::of).collect::<Vec<_>>();
    let (tx, rx) = watch::channel(members(&nodes.borrow_and_update()));
    tokio::spawn(async move {
        loop {
            tokio::select! {
                changed = nodes.changed() => if changed.is_err() {
                    return;
                },
                () = tx.closed() => return,
            }
            tx.send_replace(members(&nodes.borrow_and_update()));
        }
    });
    rx
}

/// Like [`spawn`], with the members and the sources given: `new_source` builds a node's
/// source from the RPC address zebrad opened and its cookie file, for each run.
pub(crate) fn spawn_with<S, F>(
    members: watch::Receiver<Vec<MemberInput>>,
    config: HerdConfig,
    new_source: F,
) -> HerdHandle
where
    S: TipSource,
    F: Fn(SocketAddr, &Path) -> Option<S> + Send + Sync + 'static,
{
    let new_source = Arc::new(new_source);
    let views = NETWORKS
        .iter()
        .map(|&network| {
            let (tx, rx) = watch::channel(HerdView::off(network, SystemTime::now()));
            let (ticks_tx, ticks_rx) = mpsc::channel(1);
            let actor = Actor {
                network,
                members: members.clone(),
                config: config.clone(),
                new_source: new_source.clone(),
                out: tx,
                engine: Engine::new(network),
                start: Instant::now(),
                sources: BTreeMap::new(),
                forwarders: BTreeMap::new(),
                ticks_tx,
                ticks_rx,
            };
            tokio::spawn(actor.run());
            (network, rx)
        })
        .collect();
    HerdHandle { views }
}

/// A node's source for its current run.
struct Source<S> {
    addr: SocketAddr,
    source: S,
    permits: Arc<Semaphore>,
}

struct Actor<S, F> {
    network: Network,
    members: watch::Receiver<Vec<MemberInput>>,
    config: HerdConfig,
    new_source: Arc<F>,
    out: watch::Sender<HerdView>,
    engine: Engine,
    start: Instant,
    sources: BTreeMap<NodeId, Source<S>>,
    /// The task per member that says when its tip, phase or spec changed.
    forwarders: BTreeMap<(NodeId, u64), JoinHandle<()>>,
    ticks_tx: mpsc::Sender<()>,
    ticks_rx: mpsc::Receiver<()>,
}

impl<S, F> Drop for Actor<S, F> {
    fn drop(&mut self) {
        for task in self.forwarders.values() {
            task.abort();
        }
    }
}

impl<S, F> Actor<S, F>
where
    S: TipSource,
    F: Fn(SocketAddr, &Path) -> Option<S> + Send + Sync + 'static,
{
    async fn run(mut self) {
        let mut every = tokio::time::interval(self.config.every);
        every.set_missed_tick_behavior(MissedTickBehavior::Delay);
        self.follow();
        loop {
            tokio::select! {
                changed = self.members.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    self.follow();
                }
                Some(()) = self.ticks_rx.recv() => {
                    tokio::time::sleep(self.config.coalesce).await;
                    while self.ticks_rx.try_recv().is_ok() {}
                }
                _ = every.tick() => {}
                () = self.out.closed() => return,
            }
            self.round().await;
        }
    }

    /// Starts a forwarder for each new member and stops those of members that left.
    fn follow(&mut self) {
        let members = self.members.borrow_and_update().clone();
        let present: BTreeSet<(NodeId, u64)> = members
            .iter()
            .map(|member| (member.id.clone(), member.serial))
            .collect();
        self.forwarders.retain(|key, task| {
            let keep = present.contains(key);
            if !keep {
                task.abort();
            }
            keep
        });
        let ids: BTreeSet<&NodeId> = members.iter().map(|member| &member.id).collect();
        self.sources.retain(|id, _| ids.contains(id));
        for member in members {
            let key = (member.id.clone(), member.serial);
            if !self.forwarders.contains_key(&key) {
                let task = tokio::spawn(forward(member, self.network, self.ticks_tx.clone()));
                self.forwarders.insert(key, task);
            }
        }
    }

    /// One round, and once more if the tips moved meanwhile, then the view.
    async fn round(&mut self) {
        for pass in 0..2 {
            let members: Vec<MemberInput> = self
                .members
                .borrow()
                .iter()
                .filter(|member| member.network() == self.network)
                .cloned()
                .collect();
            self.refresh_sources(&members);
            let facts: Vec<MemberFacts> = members.iter().map(MemberInput::facts).collect();
            let tips: Vec<Option<BlockRef>> = facts
                .iter()
                .map(|fact| fact.stats.as_ref().and_then(|stats| stats.best.clone()))
                .collect();
            let now = Now {
                mono: self.start.elapsed(),
                wall: SystemTime::now(),
            };
            let probes = self.engine.begin(facts, now);
            self.run_probes(probes).await;
            let view = self.engine.finish();
            let moved = members
                .iter()
                .zip(&tips)
                .any(|(member, tip)| member.tip() != *tip);
            if !moved || pass == 1 {
                self.publish(view);
                return;
            }
        }
    }

    /// Asks `probes` and every follow-up the engine asks for, feeding it each answer as it
    /// arrives.
    async fn run_probes(&mut self, probes: Vec<Probe>) {
        let mut running = JoinSet::new();
        let mut ready = self.spawn_probes(&mut running, probes);
        loop {
            let answers = if ready.is_empty() {
                match running.join_next().await {
                    Some(Ok(answer)) => vec![answer],
                    Some(Err(err)) => {
                        tracing::warn!(%err, "a probe task failed");
                        continue;
                    }
                    None => return,
                }
            } else {
                std::mem::take(&mut ready)
            };
            let next = self.engine.answer(answers);
            ready = self.spawn_probes(&mut running, next);
        }
    }

    /// Starts each probe on its node's source, holding one of its permits and within the
    /// timeout. A node without a source answers at once that the probe failed.
    fn spawn_probes(
        &self,
        running: &mut JoinSet<(Probe, ProbeResult)>,
        probes: Vec<Probe>,
    ) -> Vec<(Probe, ProbeResult)> {
        let mut failed = Vec::new();
        for probe in probes {
            let Some(source) = self.sources.get(&probe.node) else {
                failed.push((probe, ProbeResult::Failed));
                continue;
            };
            let (tips, permits) = (source.source.clone(), source.permits.clone());
            let timeout = self.config.probe_timeout;
            running.spawn(async move {
                let result = match permits.acquire_owned().await {
                    Ok(_permit) => {
                        tokio::time::timeout(timeout, tips.hash_at(probe.height, timeout))
                            .await
                            .unwrap_or(ProbeResult::Failed)
                    }
                    Err(_) => ProbeResult::Failed,
                };
                (probe, result)
            });
        }
        failed
    }

    /// A source for each running member, rebuilt when the RPC address changes.
    fn refresh_sources(&mut self, members: &[MemberInput]) {
        for member in members {
            let addr = {
                let status = member.status.borrow();
                (status.phase == Phase::Running)
                    .then_some(status.rpc_addr)
                    .flatten()
            };
            let Some(addr) = addr else {
                self.sources.remove(&member.id);
                continue;
            };
            if self
                .sources
                .get(&member.id)
                .is_some_and(|source| source.addr == addr)
            {
                continue;
            }
            match (self.new_source)(addr, &member.cookie_file) {
                Some(source) => {
                    self.sources.insert(
                        member.id.clone(),
                        Source {
                            addr,
                            source,
                            permits: Arc::new(Semaphore::new(self.config.per_node)),
                        },
                    );
                }
                None => {
                    self.sources.remove(&member.id);
                }
            }
        }
    }

    /// Publishes `view`; receivers hear of it when more than its time changed.
    fn publish(&self, view: HerdView) {
        self.out.send_if_modified(|current| {
            let changed = HerdView {
                updated_at: view.updated_at,
                ..current.clone()
            } != view;
            *current = view;
            changed
        });
    }
}

/// Says when `member`'s tip, phase or spec changed, while it is on `network` (or its spec
/// moved it), until its channels or the actor are gone.
async fn forward(mut member: MemberInput, network: Network, ticks: mpsc::Sender<()>) {
    let tip_of = |stats: &Option<NodeStats>| stats.as_ref().and_then(|stats| stats.best.clone());
    let mut tip = tip_of(&member.stats.borrow_and_update());
    let mut phase = member.status.borrow_and_update().phase.clone();
    member.spec.borrow_and_update();
    loop {
        let changed = tokio::select! {
            changed = member.stats.changed() => {
                if changed.is_err() {
                    return;
                }
                let now = tip_of(&member.stats.borrow_and_update());
                let moved = now != tip;
                tip = now;
                moved && member.network() == network
            }
            changed = member.status.changed() => {
                if changed.is_err() {
                    return;
                }
                let now = member.status.borrow_and_update().phase.clone();
                let moved = now != phase;
                phase = now;
                moved && member.network() == network
            }
            changed = member.spec.changed() => {
                if changed.is_err() {
                    return;
                }
                member.spec.borrow_and_update();
                true
            }
        };
        if changed
            && matches!(
                ticks.try_send(()),
                Err(mpsc::error::TrySendError::Closed(_))
            )
        {
            return;
        }
    }
}
