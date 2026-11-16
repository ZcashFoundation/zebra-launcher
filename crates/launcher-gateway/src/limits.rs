//! The connection limits, in one actor task: open connections in all and per IPv4 address or
//! IPv6 /64, and new connections per address through a token bucket. When every slot is
//! taken, a newcomer from a network holding fewer connections than the largest holder closes
//! that holder's oldest connection, so a few networks cannot keep everyone else out.
//! Addresses are keyed by SipHash-2-4 under a random key per run and never written down. A
//! network without an open connection is forgotten once its bucket is full again, and at most
//! [`IDLE_PER_SLOT`] of them per connection slot are kept, so neither the table nor the work
//! of an admission grows with the number of networks that tried.

use std::{
    collections::{HashMap, HashSet},
    hash::Hasher,
    net::IpAddr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use siphasher::sip::SipHasher24;
use tokio::{
    sync::{mpsc, oneshot, watch},
    time::Instant,
};

use crate::{Limits, Stats};

/// An idle address with no open connection is forgotten after this long at most.
const PRUNE_AFTER: Duration = Duration::from_secs(120);

/// The networks without an open connection kept per connection slot, at most: past that, the
/// ones touched longest ago are forgotten. A forgotten network gets a fresh bucket, as a new
/// one does, so this gives a client with many networks nothing it does not have already.
const IDLE_PER_SLOT: usize = 16;

/// The networks without an open connection the table keeps, at most.
fn idle_cap(limits: &Limits) -> usize {
    (limits.max_connections as usize).max(1) * IDLE_PER_SLOT
}

/// The network a client address counts against: an IPv4 address (an IPv4-mapped IPv6
/// address counts as its IPv4 address), or an IPv6 /64.
pub(crate) fn network_of(ip: IpAddr) -> [u8; 17] {
    let mut out = [0u8; 17];
    match ip {
        IpAddr::V6(v6) if v6.to_ipv4_mapped().is_none() => {
            out[0] = 6;
            out[1..9].copy_from_slice(&v6.octets()[..8]);
        }
        ip => {
            let v4 = match ip {
                IpAddr::V4(v4) => v4,
                IpAddr::V6(v6) => v6
                    .to_ipv4_mapped()
                    .unwrap_or(std::net::Ipv4Addr::UNSPECIFIED),
            };
            out[0] = 4;
            out[1..5].copy_from_slice(&v4.octets());
        }
    }
    out
}

/// The counters every connection adds to.
#[derive(Debug, Default)]
pub(crate) struct Counters {
    pub handshakes: AtomicU64,
    pub handshakes_failed: AtomicU64,
    pub refused_sni: AtomicU64,
    pub refused_limits: AtomicU64,
    pub bytes_up: AtomicU64,
    pub bytes_down: AtomicU64,
}

impl Counters {
    pub(crate) fn add(counter: &AtomicU64, n: u64) {
        counter.fetch_add(n, Ordering::Relaxed);
    }
}

/// One admitted connection.
#[derive(Debug)]
struct Open {
    id: u64,
    since: Instant,
    /// Closes the connection.
    evict: oneshot::Sender<()>,
}

/// One network's connections and token bucket.
#[derive(Debug)]
struct Entry {
    /// Oldest first.
    open: Vec<Open>,
    tokens: f64,
    refilled: Instant,
    touched: Instant,
}

impl Entry {
    fn new(limits: &Limits, now: Instant) -> Self {
        Entry {
            open: Vec::new(),
            tokens: f64::from(limits.new_burst.max(1)),
            refilled: now,
            touched: now,
        }
    }

    /// Refills the bucket to `now`: whether the network may open one more connection.
    fn fits(&mut self, limits: &Limits, now: Instant) -> bool {
        let rate = f64::from(limits.new_per_second);
        let burst = f64::from(limits.new_burst.max(1));
        let elapsed = now.duration_since(self.refilled).as_secs_f64();
        self.tokens = (self.tokens + elapsed * rate).min(burst);
        self.refilled = now;
        self.touched = now;
        self.open.len() < limits.per_address as usize && self.tokens >= 1.0
    }

    /// Whether the bucket is full again at `now`: the network is then no different from a new
    /// one.
    fn refilled(&self, limits: &Limits, now: Instant) -> bool {
        let rate = f64::from(limits.new_per_second);
        let burst = f64::from(limits.new_burst.max(1));
        self.tokens + now.duration_since(self.refilled).as_secs_f64() * rate >= burst
    }
}

/// Every network with an open connection or a recent one.
#[derive(Debug, Default)]
struct Table {
    entries: HashMap<u64, Entry>,
    /// The networks with an open connection: no more than the open connections.
    holders: HashSet<u64>,
    /// The open connections in all.
    open: usize,
    next_id: u64,
}

impl Table {
    fn open_connections(&self) -> usize {
        self.open
    }

    /// The networks kept without an open connection.
    fn idle(&self) -> usize {
        self.entries.len().saturating_sub(self.holders.len())
    }

    /// Admits a connection from network `key` at `now`: its id and the receiver that hears
    /// when it is closed for a newcomer, or `None` when it is refused. With every slot taken,
    /// the network holding the most connections loses its oldest one, if it holds more than
    /// `key`'s network does. A network that is refused before it held anything is not kept.
    fn admit(
        &mut self,
        key: u64,
        limits: &Limits,
        now: Instant,
    ) -> Option<(u64, oneshot::Receiver<()>)> {
        let (fits, mine) = match self.entries.get_mut(&key) {
            Some(entry) => (entry.fits(limits, now), entry.open.len()),
            None => (Entry::new(limits, now).fits(limits, now), 0),
        };
        if !fits {
            return None;
        }
        if self.open >= limits.max_connections as usize {
            // The largest holder; among equals, the one with the oldest connection.
            let (victim, held) = self
                .holders
                .iter()
                .filter_map(|key| Some((*key, self.entries.get(key)?)))
                .filter(|(_, entry)| !entry.open.is_empty())
                .max_by(|(_, a), (_, b)| {
                    a.open
                        .len()
                        .cmp(&b.open.len())
                        .then_with(|| b.open[0].since.cmp(&a.open[0].since))
                })
                .map(|(key, entry)| (key, entry.open.len()))?;
            if held <= mine {
                return None;
            }
            let largest = self.entries.get_mut(&victim)?;
            let oldest = largest.open.remove(0);
            let _ = oldest.evict.send(());
            self.open -= 1;
            if largest.open.is_empty() {
                self.holders.remove(&victim);
            }
        }
        if !self.entries.contains_key(&key) && self.idle() >= idle_cap(limits) {
            self.prune(limits, now);
        }
        let entry = self
            .entries
            .entry(key)
            .or_insert_with(|| Entry::new(limits, now));
        entry.tokens -= 1.0;
        let id = self.next_id;
        self.next_id += 1;
        let (evict, evicted) = oneshot::channel();
        entry.open.push(Open {
            id,
            since: now,
            evict,
        });
        self.open += 1;
        self.holders.insert(key);
        Some((id, evicted))
    }

    fn closed(&mut self, key: u64, id: u64, now: Instant) {
        if let Some(entry) = self.entries.get_mut(&key) {
            // A connection closed for a newcomer freed its slot then.
            if let Some(at) = entry.open.iter().position(|open| open.id == id) {
                entry.open.remove(at);
                self.open -= 1;
                if entry.open.is_empty() {
                    self.holders.remove(&key);
                }
            }
            entry.touched = now;
        }
    }

    /// Forgets the networks without an open connection whose bucket is full again or that were
    /// idle for [`PRUNE_AFTER`]. When that leaves [`idle_cap`] of them or more, the ones touched
    /// longest ago go too, down to half the cap, so making room again waits for as many
    /// newcomers and an admission's share of the work stays the same however many networks try.
    fn prune(&mut self, limits: &Limits, now: Instant) {
        self.entries.retain(|_, entry| {
            !entry.open.is_empty()
                || (!entry.refilled(limits, now) && now.duration_since(entry.touched) < PRUNE_AFTER)
        });
        let cap = idle_cap(limits);
        if self.idle() < cap {
            return;
        }
        let mut touched: Vec<Instant> = self
            .entries
            .values()
            .filter(|entry| entry.open.is_empty())
            .map(|entry| entry.touched)
            .collect();
        if let Some(last) = touched.len().checked_sub(cap / 2 + 1) {
            let (_, cutoff, _) = touched.select_nth_unstable(last);
            let cutoff = *cutoff;
            self.entries
                .retain(|_, entry| !entry.open.is_empty() || entry.touched > cutoff);
        }
    }
}

enum Msg {
    Admit {
        key: u64,
        reply: oneshot::Sender<Option<(u64, oneshot::Receiver<()>)>>,
    },
    Set(Limits),
}

/// Asks the limits actor. Cheap to clone; the actor stops when the last handle is dropped.
#[derive(Clone, Debug)]
pub(crate) struct LimitsHandle {
    tx: mpsc::Sender<Msg>,
    closed: mpsc::UnboundedSender<(u64, u64)>,
    keys: (u64, u64),
}

/// Held by an admitted connection; dropping it frees its slot.
#[derive(Debug)]
pub(crate) struct AddressGuard {
    key: u64,
    id: u64,
    closed: mpsc::UnboundedSender<(u64, u64)>,
}

impl Drop for AddressGuard {
    fn drop(&mut self) {
        // The actor is gone only when the front door stopped.
        let _ = self.closed.send((self.key, self.id));
    }
}

/// An admitted connection's slot, and the receiver that hears when a full front door closes
/// the connection for a newcomer.
#[derive(Debug)]
pub(crate) struct Admitted {
    pub guard: AddressGuard,
    pub evicted: oneshot::Receiver<()>,
}

impl LimitsHandle {
    /// Starts the actor. It publishes `stats` every `every` from `counters` and its own counts.
    pub(crate) fn spawn(
        limits: Limits,
        counters: Arc<Counters>,
        stats: watch::Sender<Stats>,
        every: Duration,
    ) -> Self {
        let (tx, rx) = mpsc::channel(256);
        let (closed, closed_rx) = mpsc::unbounded_channel();
        tokio::spawn(run(limits, rx, closed_rx, counters, stats, every));
        let random = ring::rand::SystemRandom::new();
        let mut bytes = [0u8; 16];
        // Without the OS's random source the keys are fixed; the limits still work.
        let _ = ring::rand::SecureRandom::fill(&random, &mut bytes);
        let k0 = u64::from_le_bytes(bytes[..8].try_into().unwrap_or_default());
        let k1 = u64::from_le_bytes(bytes[8..].try_into().unwrap_or_default());
        LimitsHandle {
            tx,
            closed,
            keys: (k0, k1),
        }
    }

    fn key(&self, ip: IpAddr) -> u64 {
        let mut hasher = SipHasher24::new_with_keys(self.keys.0, self.keys.1);
        hasher.write(&network_of(ip));
        hasher.finish()
    }

    /// A slot for a new connection from `ip`, or `None` when the limits refuse it.
    pub(crate) async fn admit(&self, ip: IpAddr) -> Option<Admitted> {
        let key = self.key(ip);
        let (reply, answer) = oneshot::channel();
        self.tx.send(Msg::Admit { key, reply }).await.ok()?;
        let (id, evicted) = answer.await.ok()??;
        Some(Admitted {
            guard: AddressGuard {
                key,
                id,
                closed: self.closed.clone(),
            },
            evicted,
        })
    }

    pub(crate) async fn set(&self, limits: Limits) {
        let _ = self.tx.send(Msg::Set(limits)).await;
    }
}

async fn run(
    mut limits: Limits,
    mut rx: mpsc::Receiver<Msg>,
    mut closed: mpsc::UnboundedReceiver<(u64, u64)>,
    counters: Arc<Counters>,
    stats: watch::Sender<Stats>,
    every: Duration,
) {
    let mut table = Table::default();
    let mut tick = tokio::time::interval(every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            // A freed slot counts before a newcomer that came after it.
            biased;
            Some((key, id)) = closed.recv() => table.closed(key, id, Instant::now()),
            msg = rx.recv() => match msg {
                Some(Msg::Admit { key, reply }) => {
                    let _ = reply.send(table.admit(key, &limits, Instant::now()));
                }
                Some(Msg::Set(new)) => limits = new,
                None => break,
            },
            _ = tick.tick() => {
                table.prune(&limits, Instant::now());
                let load = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
                let next = Stats {
                    open_connections: table.open_connections() as u32,
                    client_networks: table.holders.len() as u32,
                    handshakes: load(&counters.handshakes),
                    handshakes_failed: load(&counters.handshakes_failed),
                    refused_sni: load(&counters.refused_sni),
                    refused_limits: load(&counters.refused_limits),
                    bytes_up: load(&counters.bytes_up),
                    bytes_down: load(&counters.bytes_down),
                };
                stats.send_if_modified(|current| {
                    let changed = *current != next;
                    *current = next;
                    changed
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_count_per_ipv4_address_or_ipv6_64() {
        let a: IpAddr = "203.0.113.10".parse().unwrap();
        let mapped: IpAddr = "::ffff:203.0.113.10".parse().unwrap();
        assert_eq!(network_of(a), network_of(mapped));
        assert_ne!(network_of(a), network_of("203.0.113.11".parse().unwrap()));
        let v6a: IpAddr = "2001:db8:1:2::1".parse().unwrap();
        let v6b: IpAddr = "2001:db8:1:2:ffff::9".parse().unwrap();
        let v6c: IpAddr = "2001:db8:1:3::1".parse().unwrap();
        assert_eq!(network_of(v6a), network_of(v6b));
        assert_ne!(network_of(v6a), network_of(v6c));
    }

    fn key(n: u8) -> u64 {
        u64::from(n)
    }

    #[test]
    fn the_bucket_allows_a_burst_then_the_rate() {
        let limits = Limits {
            per_address: 100,
            new_per_second: 2,
            new_burst: 3,
            ..Limits::default()
        };
        let start = Instant::now();
        let mut table = Table::default();
        let mut admit = |at: Duration| table.admit(key(1), &limits, start + at).is_some();
        assert!(admit(Duration::ZERO));
        assert!(admit(Duration::ZERO));
        assert!(admit(Duration::ZERO));
        assert!(!admit(Duration::ZERO), "burst spent");
        assert!(admit(Duration::from_millis(500)));
        assert!(!admit(Duration::from_millis(600)));
        // Tokens never exceed the burst.
        let later = Duration::from_secs(100);
        assert!(admit(later));
        assert!(admit(later));
        assert!(admit(later));
        assert!(!admit(later));
    }

    #[test]
    fn open_connections_are_capped_per_address() {
        let limits = Limits {
            per_address: 2,
            new_per_second: 100,
            new_burst: 100,
            ..Limits::default()
        };
        let now = Instant::now();
        let mut table = Table::default();
        let (first, _) = table.admit(key(1), &limits, now).unwrap();
        assert!(table.admit(key(1), &limits, now).is_some());
        assert!(table.admit(key(1), &limits, now).is_none());
        assert!(
            table.admit(key(2), &limits, now).is_some(),
            "another address"
        );
        table.closed(key(1), first, now);
        assert!(table.admit(key(1), &limits, now).is_some());
    }

    #[test]
    fn a_full_front_door_closes_the_largest_holders_oldest_connection() {
        let limits = Limits {
            max_connections: 4,
            per_address: 8,
            new_per_second: 100,
            new_burst: 100,
            ..Limits::default()
        };
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut table = Table::default();
        let (a1_id, mut a1) = table.admit(key(1), &limits, at(0)).unwrap();
        let (_, mut a2) = table.admit(key(1), &limits, at(1)).unwrap();
        let (_, mut b1) = table.admit(key(2), &limits, at(2)).unwrap();
        let (_, mut b2) = table.admit(key(2), &limits, at(3)).unwrap();

        // Full, and every network holds as many as a newcomer from one of them would.
        assert!(table.admit(key(1), &limits, at(4)).is_none());
        assert!(a1.try_recv().is_err() && b1.try_recv().is_err());

        // A network with none: the oldest connection of the largest holders goes.
        let (_, mut c1) = table.admit(key(3), &limits, at(5)).unwrap();
        assert_eq!(a1.try_recv(), Ok(()), "the oldest connection is closed");
        assert!(a2.try_recv().is_err() && b1.try_recv().is_err());
        assert_eq!(table.open_connections(), 4);
        // Its guard drops later: the slot was freed already.
        table.closed(key(1), a1_id, at(5));
        assert_eq!(table.open_connections(), 4);

        // Now network 2 holds the most.
        assert!(table.admit(key(1), &limits, at(6)).is_some());
        assert_eq!(b1.try_recv(), Ok(()));
        assert!(b2.try_recv().is_err() && c1.try_recv().is_err());
        // One each but network 1, which holds two: a newcomer from 2 takes one of them.
        assert!(table.admit(key(2), &limits, at(7)).is_some());
        assert_eq!(a2.try_recv(), Ok(()));
        assert_eq!(table.open_connections(), 4);
    }

    #[test]
    fn networks_without_a_connection_are_bounded_however_many_try() {
        let limits = Limits::default();
        let now = Instant::now();
        let mut table = Table::default();
        let cap = limits.max_connections as usize + idle_cap(&limits);
        // Each closes at once, so the front door never fills.
        for n in 0..10_000 {
            let (id, _) = table.admit(1_000 + n, &limits, now).expect("a new network");
            table.closed(1_000 + n, id, now);
            assert!(table.entries.len() <= cap, "{}", table.entries.len());
        }
        assert_eq!(table.open_connections(), 0);
        // Each stays, so every newcomer past the 128th closes another network's connection.
        for n in 0..10_000 {
            assert!(table.admit(100_000 + n, &limits, now).is_some());
            assert!(table.entries.len() <= cap, "{}", table.entries.len());
        }
        assert_eq!(table.open_connections(), limits.max_connections as usize);
        assert_eq!(table.holders.len(), limits.max_connections as usize);
    }

    #[test]
    fn a_network_is_forgotten_once_its_bucket_is_full_again() {
        let limits = Limits::default();
        let start = Instant::now();
        let mut table = Table::default();
        for _ in 0..limits.new_burst {
            let (id, _) = table.admit(key(1), &limits, start).unwrap();
            table.closed(key(1), id, start);
        }
        assert!(table.admit(key(1), &limits, start).is_none(), "burst spent");
        // Half the burst is back: it still counts.
        table.prune(&limits, start + Duration::from_secs(4));
        assert!(table.entries.contains_key(&key(1)));
        // All of it: no different from a new network.
        table.prune(&limits, start + Duration::from_secs(8));
        assert!(!table.entries.contains_key(&key(1)));
        // A network refused before it held anything is not kept.
        let closed = Limits {
            per_address: 0,
            ..limits
        };
        assert!(table.admit(key(2), &closed, start).is_none());
        assert!(table.entries.is_empty());
    }

    #[tokio::test]
    async fn the_actor_admits_until_full_and_frees_slots() {
        let limits = Limits {
            max_connections: 1,
            ..Limits::default()
        };
        let (stats, _) = watch::channel(Stats::default());
        let handle = LimitsHandle::spawn(
            limits,
            Arc::new(Counters::default()),
            stats,
            Duration::from_secs(1),
        );
        let ip: IpAddr = "203.0.113.10".parse().unwrap();
        let other: IpAddr = "198.51.100.7".parse().unwrap();
        let first = handle.admit(ip).await.expect("a free slot");
        assert!(
            handle.admit(ip).await.is_none(),
            "full, and the same network"
        );
        let mut first_evicted = first.evicted;
        let second = handle.admit(other).await.expect("a network with none");
        assert_eq!((&mut first_evicted).await, Ok(()));
        drop(first.guard);
        drop(second);
        // The freed slot takes a new connection.
        assert!(handle.admit(ip).await.is_some());
    }
}
