//! The front door against an echo backend and real TLS clients: the modes, the name and ALPN
//! checks, the challenge, certificate rotation, the limits, the timers and stopping.
//!
//! Every await is bounded by [`within`], so a hang fails the test instead of stalling CI.

use std::{
    net::{Ipv4Addr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};

use launcher_gateway::{
    ACME_TLS_ALPN, CertifiedKey, Challenge, Command, Config, Event, GatewayError, H2_ALPN, Inputs,
    Limits, Mode, Outputs, Stats, Timers, certified_key, provider,
};
use rustls::{
    ClientConfig, RootCertStore,
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{mpsc, watch},
};
use tokio_rustls::{TlsConnector, client::TlsStream};

const NAME: &str = "zec.test";
const LIMIT: Duration = Duration::from_secs(10);

async fn within<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(LIMIT, future)
        .await
        .expect("timed out")
}

/// A self-signed certificate for `name`, as the front door serves it and as a client trusts
/// it.
fn cert(name: &str) -> (Arc<CertifiedKey>, CertificateDer<'static>) {
    let generated = rcgen::generate_simple_self_signed(vec![name.to_string()]).expect("generate");
    let der = generated.cert.der().clone();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
        generated.signing_key.serialize_der(),
    ));
    (certified_key(vec![der.clone()], key).expect("key"), der)
}

/// A backend that echoes every byte back.
async fn echo_backend() -> SocketAddr {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let (mut r, mut w) = stream.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });
    addr
}

/// A backend that sends `bytes` zeros to every connection.
async fn download_backend(bytes: usize) -> SocketAddr {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let _ = stream.write_all(&vec![0u8; bytes]).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    addr
}

struct Door {
    addr: SocketAddr,
    cert: watch::Sender<Option<Arc<CertifiedKey>>>,
    challenge: watch::Sender<Option<Arc<Challenge>>>,
    mode: watch::Sender<Mode>,
    commands: mpsc::Sender<Command>,
    events: mpsc::Receiver<Event>,
    stats: watch::Receiver<Stats>,
    trust: CertificateDer<'static>,
    task: tokio::task::JoinHandle<Result<(), GatewayError>>,
}

fn quick_timers() -> Timers {
    Timers {
        handshake: Duration::from_millis(500),
        idle: Duration::from_secs(30),
        max_age: Duration::from_secs(60),
        stats_every: Duration::from_millis(50),
    }
}

async fn start(mode: Mode, upstream: SocketAddr, limits: Limits, timers: Timers) -> Door {
    let (key, trust) = cert(NAME);
    let (cert_tx, cert_rx) = watch::channel(Some(key));
    let (challenge_tx, challenge_rx) = watch::channel(None);
    let (mode_tx, mode_rx) = watch::channel(mode);
    let (commands_tx, commands_rx) = mpsc::channel(8);
    let (events_tx, mut events) = mpsc::channel(64);
    let (stats_tx, stats) = watch::channel(Stats::default());
    let config = Config {
        listen: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        upstream,
        name: NAME.into(),
        limits,
        timers,
        not_outside: Vec::new(),
    };
    let inputs = Inputs {
        cert: cert_rx,
        challenge: challenge_rx,
        mode: mode_rx,
        commands: commands_rx,
    };
    let outputs = Outputs {
        stats: stats_tx,
        events: events_tx,
    };
    let task = tokio::spawn(launcher_gateway::serve(config, inputs, outputs));
    let addr = match within(events.recv()).await {
        Some(Event::Ready { addr }) => addr,
        other => panic!("expected Ready, got {other:?}"),
    };
    Door {
        addr,
        cert: cert_tx,
        challenge: challenge_tx,
        mode: mode_tx,
        commands: commands_tx,
        events,
        stats,
        trust,
        task,
    }
}

fn client_config(trust: &CertificateDer<'static>, alpn: &[u8]) -> Arc<ClientConfig> {
    let mut roots = RootCertStore::empty();
    roots.add(trust.clone()).expect("root");
    let mut config = ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .expect("versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![alpn.to_vec()];
    Arc::new(config)
}

async fn connect_as(
    door: &Door,
    name: ServerName<'static>,
    trust: &CertificateDer<'static>,
    alpn: &[u8],
) -> std::io::Result<TlsStream<TcpStream>> {
    let tcp = TcpStream::connect(door.addr).await?;
    TlsConnector::from(client_config(trust, alpn))
        .connect(name, tcp)
        .await
}

async fn connect(door: &Door) -> std::io::Result<TlsStream<TcpStream>> {
    let name = ServerName::try_from(NAME).unwrap().to_owned();
    connect_as(door, name, &door.trust, H2_ALPN).await
}

async fn echo(stream: &mut TlsStream<TcpStream>, bytes: &[u8]) -> Vec<u8> {
    stream.write_all(bytes).await.expect("write");
    let mut back = vec![0u8; bytes.len()];
    stream.read_exact(&mut back).await.expect("read");
    back
}

/// Waits until the stream ends, by EOF or error.
async fn closes(stream: &mut TlsStream<TcpStream>) {
    let mut buf = [0u8; 64];
    loop {
        match stream.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
    }
}

async fn wait_stats(door: &Door, f: impl Fn(&Stats) -> bool) -> Stats {
    let mut stats = door.stats.clone();
    within(stats.wait_for(|s| f(s)))
        .await
        .expect("stats")
        .clone()
}

#[tokio::test]
async fn open_forwards_bytes_both_ways() {
    let door = start(
        Mode::Open,
        echo_backend().await,
        Limits::default(),
        quick_timers(),
    )
    .await;
    let mut stream = within(connect(&door)).await.expect("handshake");
    assert_eq!(stream.get_ref().1.alpn_protocol(), Some(H2_ALPN));
    assert_eq!(
        within(echo(&mut stream, b"PRI * HTTP/2.0")).await,
        b"PRI * HTTP/2.0"
    );
    let stats = wait_stats(&door, |s| s.bytes_up >= 14 && s.open_connections == 1).await;
    assert_eq!(stats.handshakes, 1);
    assert_eq!(stats.client_networks, 1);
    drop(stream);
    wait_stats(&door, |s| s.open_connections == 0).await;
}

#[tokio::test]
async fn a_wrong_or_missing_name_gets_no_certificate() {
    let door = start(
        Mode::Open,
        echo_backend().await,
        Limits::default(),
        quick_timers(),
    )
    .await;
    let (_, other_trust) = cert("other.test");
    let other = ServerName::try_from("other.test").unwrap().to_owned();
    assert!(
        within(connect_as(&door, other, &other_trust, H2_ALPN))
            .await
            .is_err()
    );
    // An address as the server name sends no SNI.
    let bare = ServerName::from(std::net::IpAddr::from(Ipv4Addr::LOCALHOST));
    assert!(
        within(connect_as(&door, bare, &door.trust, H2_ALPN))
            .await
            .is_err()
    );
    // ALPN other than h2 is refused too.
    let name = ServerName::try_from(NAME).unwrap().to_owned();
    assert!(
        within(connect_as(&door, name, &door.trust, b"http/1.1"))
            .await
            .is_err()
    );
    wait_stats(&door, |s| s.refused_sni == 2).await;
}

#[tokio::test]
async fn challenge_only_answers_the_challenge_alone() {
    let mut door = start(
        Mode::ChallengeOnly,
        echo_backend().await,
        Limits::default(),
        quick_timers(),
    )
    .await;
    assert!(within(connect(&door)).await.is_err(), "no service yet");

    // No challenge pending: an acme-tls/1 handshake is refused as well.
    let name = ServerName::try_from(NAME).unwrap().to_owned();
    let (challenge_key, challenge_trust) = cert(NAME);
    assert!(
        within(connect_as(
            &door,
            name.clone(),
            &challenge_trust,
            ACME_TLS_ALPN
        ))
        .await
        .is_err()
    );
    door.challenge.send_replace(Some(Arc::new(Challenge {
        name: NAME.into(),
        key: challenge_key,
    })));
    let stream = within(connect_as(&door, name, &challenge_trust, ACME_TLS_ALPN))
        .await
        .expect("the challenge handshake completes");
    assert_eq!(stream.get_ref().1.alpn_protocol(), Some(ACME_TLS_ALPN));
    // The challenge certificate, not the served one.
    let presented = stream.get_ref().1.peer_certificates().unwrap()[0].clone();
    assert_eq!(presented, challenge_trust);
    loop {
        match within(door.events.recv()).await {
            Some(Event::ChallengeHit { name }) => {
                assert_eq!(name, NAME);
                break;
            }
            Some(_) => {}
            None => panic!("events closed"),
        }
    }
    // The service stays closed in this mode.
    assert!(within(connect(&door)).await.is_err());
}

#[tokio::test]
async fn local_only_serves_this_computer() {
    let door = start(
        Mode::LocalOnly,
        echo_backend().await,
        Limits::default(),
        quick_timers(),
    )
    .await;
    let mut stream = within(connect(&door)).await.expect("loopback is local");
    assert_eq!(within(echo(&mut stream, b"ping")).await, b"ping");
}

#[tokio::test]
async fn unavailable_answers_every_call_itself() {
    let door = start(
        Mode::Unavailable,
        echo_backend().await,
        Limits::default(),
        quick_timers(),
    )
    .await;
    let stream = within(connect(&door)).await.expect("handshake");
    let (mut client, connection) = within(h2::client::handshake(stream)).await.expect("h2");
    tokio::spawn(connection);
    for _ in 0..2 {
        let request = http::Request::post(
            "https://zec.test/cash.z.wallet.sdk.rpc.CompactTxStreamer/GetLightdInfo",
        )
        .header("content-type", "application/grpc")
        .header("te", "trailers")
        .body(())
        .unwrap();
        client = within(client.ready()).await.expect("ready");
        let (response, _body) = client.send_request(request, true).expect("send");
        let response = within(response).await.expect("response");
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["grpc-status"], "14");
        assert_eq!(response.headers()["content-type"], "application/grpc");
    }
}

#[tokio::test]
async fn a_mode_change_closes_open_connections() {
    let door = start(
        Mode::Open,
        echo_backend().await,
        Limits::default(),
        quick_timers(),
    )
    .await;
    let mut stream = within(connect(&door)).await.expect("handshake");
    assert_eq!(within(echo(&mut stream, b"x")).await, b"x");
    door.mode.send_replace(Mode::Unavailable);
    within(closes(&mut stream)).await;
    // The next connection gets the new mode.
    within(connect(&door))
        .await
        .expect("handshake in Unavailable");
}

#[tokio::test]
async fn a_new_certificate_reaches_new_handshakes_only() {
    let door = start(
        Mode::Open,
        echo_backend().await,
        Limits::default(),
        quick_timers(),
    )
    .await;
    let mut old = within(connect(&door)).await.expect("handshake");
    let (key, trust) = cert(NAME);
    door.cert.send_replace(Some(key));
    // The open connection carries on with the old certificate.
    assert_eq!(within(echo(&mut old, b"still here")).await, b"still here");
    let name = ServerName::try_from(NAME).unwrap().to_owned();
    let new = within(connect_as(&door, name.clone(), &trust, H2_ALPN))
        .await
        .expect("the new certificate");
    assert_eq!(new.get_ref().1.peer_certificates().unwrap()[0], trust);
    assert!(
        within(connect_as(&door, name, &door.trust, H2_ALPN))
            .await
            .is_err(),
        "the old certificate is gone"
    );
}

#[tokio::test]
async fn the_connection_limits_refuse_the_excess() {
    let per_address = Limits {
        per_address: 2,
        ..Limits::default()
    };
    let door = start(
        Mode::Open,
        echo_backend().await,
        per_address,
        quick_timers(),
    )
    .await;
    let _a = within(connect(&door)).await.expect("first");
    let _b = within(connect(&door)).await.expect("second");
    assert!(
        within(connect(&door)).await.is_err(),
        "third from one address"
    );
    wait_stats(&door, |s| s.refused_limits == 1).await;

    let total = Limits {
        max_connections: 1,
        ..Limits::default()
    };
    let door = start(Mode::Open, echo_backend().await, total, quick_timers()).await;
    let first = within(connect(&door)).await.expect("first");
    assert!(within(connect(&door)).await.is_err(), "over the total");
    drop(first);
    wait_stats(&door, |s| s.open_connections == 0).await;
    within(connect(&door)).await.expect("a slot came free");

    let burst = Limits {
        new_burst: 2,
        new_per_second: 0,
        ..Limits::default()
    };
    let door = start(Mode::Open, echo_backend().await, burst, quick_timers()).await;
    for _ in 0..2 {
        drop(within(connect(&door)).await.expect("within the burst"));
    }
    assert!(within(connect(&door)).await.is_err(), "past the burst");
}

#[tokio::test]
async fn a_slow_handshake_is_closed() {
    let door = start(
        Mode::Open,
        echo_backend().await,
        Limits::default(),
        quick_timers(),
    )
    .await;
    let mut silent = TcpStream::connect(door.addr).await.unwrap();
    let started = Instant::now();
    let mut buf = [0u8; 8];
    let read = within(silent.read(&mut buf)).await;
    assert!(matches!(read, Ok(0) | Err(_)), "{read:?}");
    assert!(started.elapsed() >= Duration::from_millis(400));
    wait_stats(&door, |s| s.handshakes_failed == 1).await;
}

#[tokio::test]
async fn connections_age_and_idle_out() {
    let aging = Timers {
        max_age: Duration::from_millis(300),
        ..quick_timers()
    };
    let door = start(Mode::Open, echo_backend().await, Limits::default(), aging).await;
    let mut stream = within(connect(&door)).await.expect("handshake");
    let started = Instant::now();
    within(closes(&mut stream)).await;
    assert!(started.elapsed() >= Duration::from_millis(250));

    let idling = Timers {
        idle: Duration::from_millis(300),
        ..quick_timers()
    };
    let door = start(Mode::Open, echo_backend().await, Limits::default(), idling).await;
    let mut stream = within(connect(&door)).await.expect("handshake");
    // Activity keeps it open past the idle time.
    for _ in 0..4 {
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(within(echo(&mut stream, b"k")).await, b"k");
    }
    within(closes(&mut stream)).await;
}

#[tokio::test]
async fn the_upload_budget_slows_the_download() {
    // 1,024 kbit/s is 125 KiB a second, and the bucket starts full.
    let budget = Limits {
        upload_kbit: Some(1_024),
        ..Limits::default()
    };
    let door = start(
        Mode::Open,
        download_backend(200 * 1024).await,
        budget,
        quick_timers(),
    )
    .await;
    let mut stream = within(connect(&door)).await.expect("handshake");
    let started = Instant::now();
    let mut got = Vec::new();
    within(stream.read_to_end(&mut got)).await.ok();
    assert_eq!(got.len(), 200 * 1024);
    let took = started.elapsed();
    assert!(took >= Duration::from_millis(500), "{took:?}");
}

#[tokio::test]
async fn stop_closes_the_port_and_every_connection_at_once() {
    let mut door = start(
        Mode::Open,
        echo_backend().await,
        Limits::default(),
        quick_timers(),
    )
    .await;
    let mut stream = within(connect(&door)).await.expect("handshake");
    let started = Instant::now();
    door.commands.send(Command::Stop).await.unwrap();
    within(closes(&mut stream)).await;
    within(&mut door.task).await.unwrap().expect("clean stop");
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(TcpStream::connect(door.addr).await.is_err(), "port closed");
    loop {
        match within(door.events.recv()).await {
            Some(Event::Stopped) => break,
            Some(_) => {}
            None => panic!("no Stopped event"),
        }
    }
}

#[tokio::test]
async fn a_taken_port_fails_the_start() {
    let taken = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let (_cert_tx, cert) = watch::channel(None);
    let (_challenge_tx, challenge) = watch::channel(None);
    let (_mode_tx, mode) = watch::channel(Mode::Open);
    let (_commands_tx, commands) = mpsc::channel(1);
    let (events, _events_rx) = mpsc::channel(1);
    let (stats, _stats_rx) = watch::channel(Stats::default());
    let result = within(launcher_gateway::serve(
        Config {
            listen: taken.local_addr().unwrap(),
            upstream: taken.local_addr().unwrap(),
            name: NAME.into(),
            limits: Limits::default(),
            timers: quick_timers(),
            not_outside: Vec::new(),
        },
        Inputs {
            cert,
            challenge,
            mode,
            commands,
        },
        Outputs { stats, events },
    ))
    .await;
    assert!(
        matches!(result, Err(GatewayError::PortInUse(_))),
        "{result:?}"
    );
}
