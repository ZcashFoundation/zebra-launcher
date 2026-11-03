//! Zeal's front door (docs/PUBLIC-SERVICES.md, "The front door"): the one listener of a public
//! service that the internet reaches. It ends TLS, applies the limits and forwards the
//! decrypted bytes to one backend on loopback, fixed at start: zebrad's light wallet endpoint.
//!
//! It runs inside Zeal's process, so it closes when Zeal exits or crashes. The caller gives
//! it its own runtime and thread ([`serve`] is a plain future) and drives it through
//! [`Inputs`]: the certificate, a pending challenge and the [`Mode`] on `watch`es, read at
//! every handshake, so a restarted front door starts from their current values, and
//! [`Command`]s on an mpsc. It reports [`Stats`] on a `watch` and [`Event`]s on an mpsc.
//!
//! | Mode | Accepts |
//! | --- | --- |
//! | [`Mode::ChallengeOnly`] | only ALPN `acme-tls/1` with the pending name |
//! | [`Mode::LocalOnly`] | challenges, and service connections from loopback |
//! | [`Mode::Open`] | everyone, within the limits |
//! | [`Mode::Unavailable`] | challenges; service calls get `UNAVAILABLE` from the front door |
//!
//! A service connection must ask for the configured name (SNI) and ALPN `h2`; anything else
//! is closed after the ClientHello without a certificate. A mode change closes the service
//! connections opened before it. Nothing here logs a client address.

mod bandwidth;
mod forward;
mod limits;
mod tls;
mod unavailable;

use std::{
    io,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::{Duration, SystemTime},
};

use rustls::server::Acceptor;
pub use rustls::sign::CertifiedKey;
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch},
    task::JoinSet,
};
use tokio_rustls::LazyConfigAcceptor;

pub use tls::{ACME_TLS_ALPN, H2_ALPN, certified_key, certified_key_from_pem, provider};

use crate::{
    bandwidth::Bucket,
    limits::{AddressGuard, Counters, LimitsHandle},
};

/// What the front door accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mode {
    /// Only Let's Encrypt's challenge for the pending name.
    ChallengeOnly,
    /// Challenges, and service connections from this computer.
    LocalOnly,
    /// Everyone, within the limits.
    Open,
    /// Challenges; service handshakes complete and every call gets `UNAVAILABLE`.
    Unavailable,
}

/// The connection limits and the upload budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Open connections from all clients together.
    pub max_connections: u32,
    /// Open connections from one IPv4 address or IPv6 /64.
    pub per_address: u32,
    /// New connections a second from one address, after the burst.
    pub new_per_second: u32,
    /// New connections from one address in a burst.
    pub new_burst: u32,
    /// The upload budget for all connections together, in kbit/s; `None` for no limit.
    pub upload_kbit: Option<u32>,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_connections: 128,
            per_address: 8,
            new_per_second: 2,
            new_burst: 16,
            upload_kbit: Some(10_000),
        }
    }
}

/// The front door's timers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timers {
    /// The TLS handshake, and the HTTP/2 one in `Unavailable`.
    pub handshake: Duration,
    /// A connection that moves no byte this long is closed.
    pub idle: Duration,
    /// Every connection is closed at this age; zebrad's HTTP/2 pings would keep it from
    /// idling.
    pub max_age: Duration,
    /// How often [`Stats`] are published.
    pub stats_every: Duration,
}

impl Default for Timers {
    fn default() -> Self {
        Timers {
            handshake: Duration::from_secs(10),
            idle: Duration::from_secs(10 * 60),
            max_age: Duration::from_secs(2 * 3600),
            stats_every: Duration::from_secs(5),
        }
    }
}

/// A pending TLS-ALPN-01 challenge: the name and its challenge certificate (RFC 8737).
#[derive(Clone, Debug)]
pub struct Challenge {
    pub name: String,
    pub key: Arc<CertifiedKey>,
}

/// The front door's counters, published every [`Timers::stats_every`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub open_connections: u32,
    /// Distinct client networks (IPv4 addresses or IPv6 /64s) with an open connection.
    pub client_networks: u32,
    pub handshakes: u64,
    pub handshakes_failed: u64,
    /// Handshakes refused for a wrong or missing name.
    pub refused_sni: u64,
    /// Connections refused by the limits.
    pub refused_limits: u64,
    /// Bytes sent to clients.
    pub bytes_up: u64,
    /// Bytes received from clients.
    pub bytes_down: u64,
}

/// What the caller may tell a running front door.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// New limits: the upload budget and the per-address limits apply at once, the
    /// connection cap to new connections.
    Limits(Limits),
    /// Close the port and every connection now.
    Stop,
}

/// What the front door reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// Listening on `addr`.
    Ready { addr: SocketAddr },
    /// A TLS-ALPN-01 handshake for `name` completed: Let's Encrypt reached this computer.
    ChallengeHit { name: String },
    /// A client from a public address, not one of [`Config::not_outside`], completed a
    /// service handshake: evidence that the service is reachable from outside.
    Outside { at: SystemTime },
    /// The port and every connection are closed.
    Stopped,
}

/// The front door could not run.
#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    #[error("port {0} is already in use")]
    PortInUse(u16),
    #[error("could not listen on port {port}: {source}")]
    Listen { port: u16, source: io::Error },
}

/// Where and what the front door serves.
#[derive(Clone, Debug)]
pub struct Config {
    /// Where it listens: `[::]:8443`, which also takes IPv4.
    pub listen: SocketAddr,
    /// The one backend, on loopback.
    pub upstream: SocketAddr,
    /// The name a service connection must ask for.
    pub name: String,
    pub limits: Limits,
    pub timers: Timers,
    /// Addresses whose connections are no evidence from outside, such as the router's own
    /// public address.
    pub not_outside: Vec<IpAddr>,
}

/// The front door's inputs.
#[derive(Debug)]
pub struct Inputs {
    /// The served certificate; service handshakes wait for one.
    pub cert: watch::Receiver<Option<Arc<CertifiedKey>>>,
    pub challenge: watch::Receiver<Option<Arc<Challenge>>>,
    pub mode: watch::Receiver<Mode>,
    pub commands: mpsc::Receiver<Command>,
}

/// The front door's outputs. Events are sent with `try_send`: a full channel drops them
/// rather than slow the front door.
#[derive(Debug)]
pub struct Outputs {
    pub stats: watch::Sender<Stats>,
    pub events: mpsc::Sender<Event>,
}

/// What every connection task reads.
#[derive(Debug)]
pub(crate) struct Context {
    pub name: String,
    pub upstream: SocketAddr,
    pub timers: Timers,
    pub not_outside: Vec<IpAddr>,
    pub cert: watch::Receiver<Option<Arc<CertifiedKey>>>,
    pub challenge: watch::Receiver<Option<Arc<Challenge>>>,
    pub mode: watch::Receiver<Mode>,
    pub counters: Arc<Counters>,
    pub bucket: Arc<Bucket>,
    pub events: mpsc::Sender<Event>,
}

/// Binds `addr` for the front door. An IPv6 wildcard also takes IPv4 (IPv6-only off).
fn bind(addr: SocketAddr) -> Result<TcpListener, GatewayError> {
    use socket2::{Domain, Protocol, Socket, Type};
    let port = addr.port();
    let listen = |source| GatewayError::Listen { port, source };
    let socket = Socket::new(Domain::for_address(addr), Type::STREAM, Some(Protocol::TCP))
        .map_err(listen)?;
    if addr.is_ipv6() {
        socket.set_only_v6(false).map_err(listen)?;
    }
    // Rebinding over connections in TIME_WAIT after a restart. Not on Windows, where the
    // option lets a socket take a port another one is bound to.
    #[cfg(unix)]
    socket.set_reuse_address(true).map_err(listen)?;
    socket.set_nonblocking(true).map_err(listen)?;
    socket.bind(&addr.into()).map_err(|source| {
        if source.kind() == io::ErrorKind::AddrInUse {
            GatewayError::PortInUse(port)
        } else {
            GatewayError::Listen { port, source }
        }
    })?;
    socket.listen(1024).map_err(listen)?;
    TcpListener::from_std(socket.into()).map_err(listen)
}

/// Whether a client at `ip` is evidence from outside: a public unicast address.
fn is_outside(ip: IpAddr, not_outside: &[IpAddr]) -> bool {
    let ip = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
        ip => ip,
    };
    if not_outside.contains(&ip) || ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
        return false;
    }
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            let cgnat = a == 100 && (64..128).contains(&b);
            !(v4.is_private() || v4.is_link_local() || v4.is_broadcast() || cgnat)
        }
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            let unique_local = first & 0xfe00 == 0xfc00;
            let link_local = first & 0xffc0 == 0xfe80;
            !(unique_local || link_local)
        }
    }
}

/// Runs the front door until [`Command::Stop`], or until the command channel closes.
pub async fn serve(config: Config, inputs: Inputs, outputs: Outputs) -> Result<(), GatewayError> {
    let listener = bind(config.listen)?;
    let addr = listener
        .local_addr()
        .map_err(|source| GatewayError::Listen {
            port: config.listen.port(),
            source,
        })?;
    tracing::info!(target: "zeal::gateway", port = addr.port(), "front door listening");
    let _ = outputs.events.try_send(Event::Ready { addr });

    let Inputs {
        cert,
        challenge,
        mode,
        mut commands,
    } = inputs;
    let counters = Arc::new(Counters::default());
    let limits = LimitsHandle::spawn(
        config.limits,
        counters.clone(),
        outputs.stats,
        config.timers.stats_every,
    );
    let bucket = Bucket::new(config.limits.upload_kbit);
    let ctx = Arc::new(Context {
        name: config.name.trim_end_matches('.').to_ascii_lowercase(),
        upstream: config.upstream,
        timers: config.timers,
        not_outside: config.not_outside,
        cert,
        challenge,
        mode,
        counters: counters.clone(),
        bucket: bucket.clone(),
        events: outputs.events.clone(),
    });
    let total = Arc::new(Semaphore::new(config.limits.max_connections as usize));
    let mut cap = config.limits.max_connections;
    let mut connections = JoinSet::new();

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                while connections.try_join_next().is_some() {}
                let Ok((stream, peer)) = accepted else {
                    // Out of file descriptors and the like: back off a little.
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                };
                let Ok(permit) = total.clone().try_acquire_owned() else {
                    Counters::add(&counters.refused_limits, 1);
                    continue;
                };
                let Some(guard) = limits.admit(peer.ip().to_canonical()).await else {
                    Counters::add(&counters.refused_limits, 1);
                    continue;
                };
                // `[::]` takes IPv4 as mapped addresses: `::ffff:127.0.0.1` is loopback too.
                let ip = peer.ip().to_canonical();
                connections.spawn(connection(stream, ip, ctx.clone(), permit, guard));
            }
            command = commands.recv() => match command {
                Some(Command::Limits(new)) => {
                    limits.set(new).await;
                    bucket.set(new.upload_kbit);
                    if new.max_connections > cap {
                        total.add_permits((new.max_connections - cap) as usize);
                    } else if new.max_connections < cap {
                        let fewer = cap - new.max_connections;
                        let total = total.clone();
                        tokio::spawn(async move {
                            if let Ok(permits) = total.acquire_many(fewer).await {
                                permits.forget();
                            }
                        });
                    }
                    cap = new.max_connections;
                }
                Some(Command::Stop) | None => break,
            },
        }
    }
    // The port closes first, then every connection.
    drop(listener);
    connections.abort_all();
    while connections.join_next().await.is_some() {}
    tracing::info!(target: "zeal::gateway", "front door stopped");
    let _ = outputs.events.try_send(Event::Stopped);
    Ok(())
}

/// One client connection, from the ClientHello to the end.
async fn connection(
    stream: TcpStream,
    peer: IpAddr,
    ctx: Arc<Context>,
    _permit: OwnedSemaphorePermit,
    _guard: AddressGuard,
) {
    let _ = stream.set_nodelay(true);
    let acceptor = LazyConfigAcceptor::new(Acceptor::default(), stream);
    let Ok(Ok(start)) = tokio::time::timeout(ctx.timers.handshake, acceptor).await else {
        Counters::add(&ctx.counters.handshakes_failed, 1);
        return;
    };
    let hello = start.client_hello();
    let name = tls::requested_name(&hello);
    let acme = tls::offers_acme(&hello);

    if acme {
        let challenge = ctx.challenge.borrow().clone();
        let Some(challenge) = challenge.filter(|challenge| Some(&challenge.name) == name.as_ref())
        else {
            Counters::add(&ctx.counters.refused_sni, 1);
            return;
        };
        let Ok(config) = tls::challenge_config(challenge.key.clone()) else {
            return;
        };
        let accept = tokio::time::timeout(ctx.timers.handshake, start.into_stream(config));
        if let Ok(Ok(mut stream)) = accept.await {
            tracing::info!(target: "zeal::gateway", "Let's Encrypt's challenge answered");
            let _ = ctx.events.try_send(Event::ChallengeHit {
                name: challenge.name.clone(),
            });
            let _ = tokio::io::AsyncWriteExt::shutdown(&mut stream).await;
        } else {
            Counters::add(&ctx.counters.handshakes_failed, 1);
        }
        return;
    }

    if name.as_deref() != Some(ctx.name.as_str()) {
        Counters::add(&ctx.counters.refused_sni, 1);
        return;
    }
    let mut mode = ctx.mode.clone();
    let current = *mode.borrow_and_update();
    let allowed = match current {
        Mode::ChallengeOnly => false,
        Mode::LocalOnly => peer.is_loopback(),
        Mode::Open | Mode::Unavailable => true,
    };
    let cert = ctx.cert.borrow().clone();
    let (true, Some(cert)) = (allowed, cert) else {
        return;
    };
    let Ok(config) = tls::service_config(cert) else {
        return;
    };
    let accept = tokio::time::timeout(ctx.timers.handshake, start.into_stream(config));
    let Ok(Ok(stream)) = accept.await else {
        Counters::add(&ctx.counters.handshakes_failed, 1);
        return;
    };
    if stream.get_ref().1.alpn_protocol() != Some(H2_ALPN) {
        Counters::add(&ctx.counters.handshakes_failed, 1);
        return;
    }
    Counters::add(&ctx.counters.handshakes, 1);
    if is_outside(peer, &ctx.not_outside) {
        let _ = ctx.events.try_send(Event::Outside {
            at: SystemTime::now(),
        });
    }
    match current {
        Mode::Unavailable => unavailable::run(stream, ctx.clone(), &mut mode).await,
        _ => forward::run(stream, ctx.clone(), &mut mode).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_public_addresses_are_evidence_from_outside() {
        let router: IpAddr = "203.0.113.10".parse().unwrap();
        for ip in [
            "127.0.0.1",
            "::1",
            "192.168.1.20",
            "10.0.0.2",
            "172.16.5.4",
            "169.254.1.1",
            "100.64.0.1",
            "100.127.255.254",
            "fd00::1",
            "fe80::1",
            "::ffff:192.168.1.1",
            "203.0.113.10",
        ] {
            assert!(!is_outside(ip.parse().unwrap(), &[router]), "{ip}");
        }
        for ip in [
            "198.51.100.7",
            "100.128.0.1",
            "2001:db8::1",
            "::ffff:198.51.100.7",
        ] {
            assert!(is_outside(ip.parse().unwrap(), &[router]), "{ip}");
        }
    }

    #[test]
    fn a_taken_port_is_reported_as_such() {
        let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = taken.local_addr().unwrap();
        assert!(matches!(bind(addr), Err(GatewayError::PortInUse(port)) if port == addr.port()));
    }
}
