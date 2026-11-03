//! Contract tests against a real zebrad's light wallet endpoint, through the front door.
//!
//! Ignored by default, like launcher-core's `real_zebrad.rs`; CI runs them with the fetched
//! release binary when the zebrad pin changes. Run them by hand with:
//!
//! ```sh
//! ZEBRAD_BIN=/path/to/zebrad cargo test -p launcher-public --test real_light_server -- --include-ignored --nocapture
//! ```
//!
//! zebrad runs on Regtest through the supervisor with the endpoint on loopback, as Zeal's
//! managed key writes it, and the front door serves a test certificate.

use std::{
    net::{Ipv4Addr, SocketAddr, TcpListener},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use launcher_core::{
    process::ZebradCommand,
    supervisor::{self, SupervisorConfig},
};
use launcher_gateway::{Config, Event, Inputs, Limits, Mode, Outputs, Stats, Timers};
use launcher_public::grpc;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};
use tokio::sync::{mpsc, watch};

const LIMIT: Duration = Duration::from_secs(120);
const NAME: &str = "light.zeal-test.com";

fn zebrad_bin() -> PathBuf {
    let path = std::env::var_os("ZEBRAD_BIN")
        .filter(|v| !v.is_empty())
        .expect("ZEBRAD_BIN is not set: run with --include-ignored and ZEBRAD_BIN");
    PathBuf::from(path)
}

fn free_port() -> u16 {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn toml_path(path: &Path) -> String {
    format!("'{}'", path.display())
}

/// zebrad on Regtest with every listener on loopback and the endpoint at `endpoint`.
fn regtest(dir: &Path, endpoint: SocketAddr) -> SupervisorConfig {
    let config = dir.join("zebrad.toml");
    let toml = format!(
        "[network]\nnetwork = \"Regtest\"\nlisten_addr = \"127.0.0.1:0\"\ncache_dir = {state}\n\n\
         [state]\ncache_dir = {state}\n\n\
         [rpc]\nlisten_addr = \"127.0.0.1:0\"\ncookie_dir = {cookie}\nlightwalletd_listen_addr = \"{endpoint}\"\n\n\
         [metrics]\nendpoint_addr = \"127.0.0.1:{metrics}\"\n\n[tracing]\nuse_color = false\n",
        state = toml_path(&dir.join("state")),
        cookie = toml_path(&dir.join("rpc")),
        metrics = free_port(),
    );
    std::fs::write(&config, toml).unwrap();
    SupervisorConfig::new(ZebradCommand::new(zebrad_bin(), &config))
}

#[ignore = "needs ZEBRAD_BIN; run with -- --include-ignored"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wallets_reach_zebrads_endpoint_through_the_front_door() {
    let dir = tempfile::tempdir().unwrap();
    let endpoint = SocketAddr::from((Ipv4Addr::LOCALHOST, free_port()));
    let handle = supervisor::spawn(regtest(dir.path(), endpoint)).unwrap();
    tokio::time::timeout(LIMIT, handle.start())
        .await
        .unwrap()
        .expect("zebrad starts");
    let status = tokio::time::timeout(LIMIT, handle.wait_until(|s| s.lightwalletd_addr.is_some()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.lightwalletd_addr, Some(endpoint));

    // Straight to the endpoint, over HTTP/2 with prior knowledge, as the light monitor asks.
    let tcp = tokio::net::TcpStream::connect(endpoint).await.unwrap();
    let raw = grpc::get_lightd_info_bytes(tcp, &endpoint.to_string(), false)
        .await
        .expect("GetLightdInfo");
    let info = grpc::decode_lightd_info(&raw).unwrap();
    eprintln!("GetLightdInfo: {info:?}");
    let hex: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    eprintln!("GetLightdInfo bytes: {hex}");
    assert_eq!(info.vendor, "ZcashFoundation/zebra");
    assert_eq!(info.chain_name, "test", "Regtest reports the test chain");
    assert!(
        info.version.trim_start_matches('v').starts_with("7."),
        "{info:?}"
    );
    assert!(
        launcher_herd::public::zebrad_tested(&info.version),
        "{info:?}"
    );
    assert_eq!(info.consensus_branch_id.len(), 8);

    // Through the front door, over TLS with the name, as a wallet connects.
    let generated = rcgen::generate_simple_self_signed(vec![NAME.to_string()]).unwrap();
    let trust = generated.cert.der().clone();
    let key = launcher_gateway::certified_key(
        vec![trust.clone()],
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
            generated.signing_key.serialize_der(),
        )),
    )
    .unwrap();
    let (_cert_tx, cert) = watch::channel(Some(key));
    let (_challenge_tx, challenge) = watch::channel(None);
    let (mode_tx, mode) = watch::channel(Mode::LocalOnly);
    let (commands_tx, commands) = mpsc::channel(4);
    let (events_tx, mut events) = mpsc::channel(16);
    let (stats, _stats_rx) = watch::channel(Stats::default());
    tokio::spawn(launcher_gateway::serve(
        Config {
            listen: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            upstream: endpoint,
            name: NAME.into(),
            limits: Limits::default(),
            timers: Timers::default(),
            not_outside: Vec::new(),
        },
        Inputs {
            cert,
            challenge,
            mode,
            commands,
        },
        Outputs {
            stats,
            events: events_tx,
        },
    ));
    let Some(Event::Ready { addr }) = events.recv().await else {
        panic!("the front door did not start");
    };
    let tls = |trust: CertificateDer<'static>| async move {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(trust).unwrap();
        let mut config = rustls::ClientConfig::builder_with_provider(launcher_gateway::provider())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        config.alpn_protocols = vec![b"h2".to_vec()];
        let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
        tokio_rustls::TlsConnector::from(Arc::new(config))
            .connect(ServerName::try_from(NAME).unwrap(), tcp)
            .await
            .unwrap()
    };
    let through = grpc::get_lightd_info(tls(trust.clone()).await, NAME, true)
        .await
        .expect("GetLightdInfo through the front door");
    assert_eq!(through.vendor, info.vendor);
    assert_eq!(through.chain_name, info.chain_name);
    let latest = grpc::get_latest_block(tls(trust.clone()).await, NAME, true)
        .await
        .expect("GetLatestBlock through the front door");
    assert_eq!(latest.height, 0, "a fresh Regtest chain");
    assert_eq!(latest.hash.len(), 32);

    // Unavailable: the front door answers, zebrad sees nothing.
    mode_tx.send_replace(Mode::Unavailable);
    let refused = grpc::get_lightd_info(tls(trust).await, NAME, true).await;
    assert!(
        matches!(refused, Err(grpc::GrpcError::Grpc { code: 14, .. })),
        "{refused:?}"
    );

    commands_tx
        .send(launcher_gateway::Command::Stop)
        .await
        .unwrap();
    tokio::time::timeout(LIMIT, handle.stop())
        .await
        .unwrap()
        .unwrap();
    handle.shutdown().await.unwrap();
}
