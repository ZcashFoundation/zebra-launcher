//! The per-address limits, in one actor task: open connections per IPv4 address or IPv6 /64,
//! and new connections per address through a token bucket. Addresses are keyed by SipHash-2-4
//! under a random key per run, pruned when idle, and never written down.

use std::{
    collections::HashMap,
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

/// An idle address with no open connection is forgotten after this long.
const PRUNE_AFTER: Duration = Duration::from_secs(120);

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

#[derive(Debug)]
struct Entry {
    open: u32,
    tokens: f64,
    refilled: Instant,
    touched: Instant,
}

enum Msg {
    Admit {
        key: u64,
        reply: oneshot::Sender<bool>,
    },
    Set(Limits),
}

/// Asks the limits actor. Cheap to clone; the actor stops when the last handle is dropped.
#[derive(Clone, Debug)]
pub(crate) struct LimitsHandle {
    tx: mpsc::Sender<Msg>,
    closed: mpsc::UnboundedSender<u64>,
    keys: (u64, u64),
}

/// Held by an admitted connection; dropping it frees its slot.
#[derive(Debug)]
pub(crate) struct AddressGuard {
    key: u64,
    closed: mpsc::UnboundedSender<u64>,
}

impl Drop for AddressGuard {
    fn drop(&mut self) {
        // The actor is gone only when the front door stopped.
        let _ = self.closed.send(self.key);
    }
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

    /// A slot for a new connection from `ip`, or `None` when its network is at its limits.
    pub(crate) async fn admit(&self, ip: IpAddr) -> Option<AddressGuard> {
        let key = self.key(ip);
        let (reply, answer) = oneshot::channel();
        self.tx.send(Msg::Admit { key, reply }).await.ok()?;
        answer.await.ok()?.then(|| AddressGuard {
            key,
            closed: self.closed.clone(),
        })
    }

    pub(crate) async fn set(&self, limits: Limits) {
        let _ = self.tx.send(Msg::Set(limits)).await;
    }
}

/// Whether a new connection fits `entry` under `limits` at `now`, taking its token if so.
fn admit(entry: &mut Entry, limits: &Limits, now: Instant) -> bool {
    let rate = f64::from(limits.new_per_second);
    let burst = f64::from(limits.new_burst.max(1));
    let elapsed = now.duration_since(entry.refilled).as_secs_f64();
    entry.tokens = (entry.tokens + elapsed * rate).min(burst);
    entry.refilled = now;
    entry.touched = now;
    if entry.open >= limits.per_address || entry.tokens < 1.0 {
        return false;
    }
    entry.tokens -= 1.0;
    entry.open += 1;
    true
}

async fn run(
    mut limits: Limits,
    mut rx: mpsc::Receiver<Msg>,
    mut closed: mpsc::UnboundedReceiver<u64>,
    counters: Arc<Counters>,
    stats: watch::Sender<Stats>,
    every: Duration,
) {
    let mut entries: HashMap<u64, Entry> = HashMap::new();
    let mut tick = tokio::time::interval(every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Some(Msg::Admit { key, reply }) => {
                    let now = Instant::now();
                    let entry = entries.entry(key).or_insert(Entry {
                        open: 0,
                        tokens: f64::from(limits.new_burst.max(1)),
                        refilled: now,
                        touched: now,
                    });
                    let _ = reply.send(admit(entry, &limits, now));
                }
                Some(Msg::Set(new)) => limits = new,
                None => break,
            },
            Some(key) = closed.recv() => {
                if let Some(entry) = entries.get_mut(&key) {
                    entry.open = entry.open.saturating_sub(1);
                    entry.touched = Instant::now();
                }
            }
            _ = tick.tick() => {
                let now = Instant::now();
                entries.retain(|_, entry| entry.open > 0 || now.duration_since(entry.touched) < PRUNE_AFTER);
                let load = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
                let next = Stats {
                    open_connections: entries.values().map(|entry| entry.open).sum(),
                    client_networks: entries.values().filter(|entry| entry.open > 0).count() as u32,
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

    #[test]
    fn the_bucket_allows_a_burst_then_the_rate() {
        let limits = Limits {
            per_address: 100,
            new_per_second: 2,
            new_burst: 3,
            ..Limits::default()
        };
        let start = Instant::now();
        let mut entry = Entry {
            open: 0,
            tokens: 3.0,
            refilled: start,
            touched: start,
        };
        assert!(admit(&mut entry, &limits, start));
        assert!(admit(&mut entry, &limits, start));
        assert!(admit(&mut entry, &limits, start));
        assert!(!admit(&mut entry, &limits, start), "burst spent");
        assert!(admit(
            &mut entry,
            &limits,
            start + Duration::from_millis(500)
        ));
        assert!(!admit(
            &mut entry,
            &limits,
            start + Duration::from_millis(600)
        ));
        // Tokens never exceed the burst.
        assert!(admit(&mut entry, &limits, start + Duration::from_secs(100)));
        assert!(admit(&mut entry, &limits, start + Duration::from_secs(100)));
        assert!(admit(&mut entry, &limits, start + Duration::from_secs(100)));
        assert!(!admit(
            &mut entry,
            &limits,
            start + Duration::from_secs(100)
        ));
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
        let mut entry = Entry {
            open: 0,
            tokens: 100.0,
            refilled: now,
            touched: now,
        };
        assert!(admit(&mut entry, &limits, now));
        assert!(admit(&mut entry, &limits, now));
        assert!(!admit(&mut entry, &limits, now));
        entry.open -= 1;
        assert!(admit(&mut entry, &limits, now));
    }
}
