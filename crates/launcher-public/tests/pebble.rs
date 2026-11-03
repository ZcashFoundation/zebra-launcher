//! Certificates against Pebble, Let's Encrypt's test ACME server, through the real front
//! door: TLS-ALPN-01 answered on the front door's port, staging then production, the chain
//! verified and served.
//!
//! The test runs when `PEBBLE` and `PEBBLE_CHALLTESTSRV` name the two binaries (CI's `pebble`
//! job sets them); without them it prints a note and passes, so `cargo test` needs neither,
//! unless `PEBBLE_REQUIRED` is set, as in that job, which makes it fail instead:
//!
//! ```sh
//! PEBBLE=/path/to/pebble PEBBLE_CHALLTESTSRV=/path/to/pebble-challtestsrv \
//!   cargo test -p launcher-public --test pebble -- --nocapture
//! ```
//!
//! pebble-challtestsrv answers every A query with 127.0.0.1, so Pebble validates against the
//! front door listening there. Pebble's own HTTPS uses a test CA made here.

use std::{
    net::{Ipv4Addr, SocketAddr, TcpListener},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Arc,
    time::{Duration, SystemTime},
};

use bytes::Bytes;
use http_body_util::{BodyExt, Empty};
use launcher_gateway::{
    Command as DoorCommand, Config, Event, Inputs, Limits, Mode, Outputs, Stats, Timers,
};
use launcher_public::{
    acme::{self, AcmeConfig, AcmeHandle},
    store::PublicPaths,
};
use launcher_types::public::CertIssuer;
use rustls::pki_types::{CertificateDer, pem::PemObject};
use tokio::sync::{mpsc, watch};

const NAME: &str = "light.zeal-test.com";
const LIMIT: Duration = Duration::from_secs(120);

fn binaries() -> Option<(PathBuf, PathBuf)> {
    let pebble = std::env::var_os("PEBBLE").filter(|v| !v.is_empty())?;
    let challtestsrv = std::env::var_os("PEBBLE_CHALLTESTSRV").filter(|v| !v.is_empty())?;
    Some((pebble.into(), challtestsrv.into()))
}

fn free_port() -> u16 {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Kills the child when the test ends, however it ends.
struct Killed(Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A CA and a certificate for 127.0.0.1 and localhost signed by it, for Pebble's HTTPS.
fn pebble_tls(dir: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let ca_key = rcgen::KeyPair::generate().unwrap();
    let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "Zeal test CA");
    let ca = ca_params.self_signed(&ca_key).unwrap();
    let issuer = rcgen::Issuer::new(ca_params, ca_key);
    let key = rcgen::KeyPair::generate().unwrap();
    let mut params = rcgen::CertificateParams::new(vec!["localhost".to_string()]).unwrap();
    params
        .subject_alt_names
        .push(rcgen::SanType::IpAddress(Ipv4Addr::LOCALHOST.into()));
    let cert = params.signed_by(&key, &issuer).unwrap();
    let (ca_path, cert_path, key_path) = (
        dir.join("ca.pem"),
        dir.join("cert.pem"),
        dir.join("key.pem"),
    );
    std::fs::write(&ca_path, ca.pem()).unwrap();
    std::fs::write(&cert_path, cert.pem()).unwrap();
    std::fs::write(&key_path, key.serialize_pem()).unwrap();
    (ca_path, cert_path, key_path)
}

/// GET over HTTPS trusting `ca`.
async fn get(url: &str, ca: &Path) -> Option<String> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(CertificateDer::from_pem_file(ca).ok()?).ok()?;
    let config = rustls::ClientConfig::builder_with_provider(launcher_gateway::provider())
        .with_safe_default_protocol_versions()
        .ok()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connector = hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config(config)
        .https_only()
        .enable_http1()
        .build();
    let client = hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
        .build::<_, Empty<Bytes>>(connector);
    // Pebble refuses requests without a User-Agent.
    let request = http::Request::get(url)
        .header("user-agent", "zeal-test")
        .body(Empty::new())
        .ok()?;
    let response = match client.request(request).await {
        Ok(response) => response,
        Err(err) => {
            eprintln!("GET {url}: {err:?}");
            return None;
        }
    };
    if !response.status().is_success() {
        let status = response.status();
        let body = response.into_body().collect().await.ok()?.to_bytes();
        eprintln!(
            "GET {url}: HTTP {status}: {}",
            String::from_utf8_lossy(&body)
        );
        return None;
    }
    let body = response.into_body().collect().await.ok()?.to_bytes();
    String::from_utf8(body.to_vec()).ok()
}

struct Door {
    port: u16,
    challenge: watch::Sender<Option<Arc<launcher_gateway::Challenge>>>,
    _mode: watch::Sender<Mode>,
    commands: mpsc::Sender<DoorCommand>,
    events: mpsc::Receiver<Event>,
}

/// The front door in ChallengeOnly on a free port, with its served certificate fed from
/// `cert`.
async fn front_door(cert: watch::Receiver<Option<Arc<launcher_gateway::CertifiedKey>>>) -> Door {
    let port = free_port();
    let (challenge_tx, challenge) = watch::channel(None);
    let (mode_tx, mode) = watch::channel(Mode::ChallengeOnly);
    let (commands, commands_rx) = mpsc::channel(4);
    let (events_tx, mut events) = mpsc::channel(64);
    let (stats, _stats) = watch::channel(Stats::default());
    tokio::spawn(launcher_gateway::serve(
        Config {
            listen: SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
            upstream: SocketAddr::from((Ipv4Addr::LOCALHOST, 9)),
            name: NAME.into(),
            limits: Limits::default(),
            timers: Timers::default(),
            not_outside: Vec::new(),
        },
        Inputs {
            cert,
            challenge,
            mode,
            commands: commands_rx,
        },
        Outputs {
            stats,
            events: events_tx,
        },
    ));
    match tokio::time::timeout(LIMIT, events.recv()).await {
        Ok(Some(Event::Ready { .. })) => {}
        other => panic!("front door: {other:?}"),
    }
    Door {
        port,
        challenge: challenge_tx,
        _mode: mode_tx,
        commands,
        events,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tls_alpn_01_through_the_front_door_from_staging_to_production() {
    let Some((pebble, challtestsrv)) = binaries() else {
        assert!(
            std::env::var_os("PEBBLE_REQUIRED").is_none(),
            "PEBBLE_REQUIRED is set, but PEBBLE and PEBBLE_CHALLTESTSRV do not name the binaries"
        );
        eprintln!("skipped: PEBBLE and PEBBLE_CHALLTESTSRV are not set");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let (ca, cert, key) = pebble_tls(dir.path());

    let (_served_tx, served) = watch::channel(None);
    let mut door = front_door(served).await;

    let dns_port = free_port();
    let _challtestsrv = Killed(
        Command::new(&challtestsrv)
            .args([
                "-defaultIPv4",
                "127.0.0.1",
                "-defaultIPv6",
                "",
                "-dnsserver",
                &format!("127.0.0.1:{dns_port}"),
                "-http01",
                "",
                "-https01",
                "",
                "-tlsalpn01",
                "",
                "-doh",
                "",
                "-management",
                &format!("127.0.0.1:{}", free_port()),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start pebble-challtestsrv"),
    );
    let (acme_port, management_port) = (free_port(), free_port());
    let config = serde_json::json!({
        "pebble": {
            "listenAddress": format!("127.0.0.1:{acme_port}"),
            "managementListenAddress": format!("127.0.0.1:{management_port}"),
            "certificate": cert,
            "privateKey": key,
            "httpPort": free_port(),
            "tlsPort": door.port,
            "ocspResponderURL": "",
            "externalAccountBindingRequired": false,
        }
    });
    let config_path = dir.path().join("pebble.json");
    std::fs::write(&config_path, config.to_string()).unwrap();
    let _pebble = Killed(
        Command::new(&pebble)
            .arg("-config")
            .arg(&config_path)
            .args(["-dnsserver", &format!("127.0.0.1:{dns_port}")])
            .env("PEBBLE_VA_NOSLEEP", "1")
            .env("PEBBLE_WFE_NONCEREJECT", "0")
            .env("PEBBLE_AUTHZREUSE", "0")
            .stdout(std::fs::File::create(dir.path().join("pebble.log")).unwrap())
            .stderr(Stdio::null())
            .spawn()
            .expect("start pebble"),
    );
    let directory = format!("https://127.0.0.1:{acme_port}/dir");
    let mut ready = false;
    for _ in 0..100 {
        if get(&directory, &ca).await.is_some() {
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let log = std::fs::read_to_string(dir.path().join("pebble.log")).unwrap_or_default();
    assert!(ready, "Pebble did not start:\n{log}");
    let root = get(&format!("https://127.0.0.1:{management_port}/roots/0"), &ca)
        .await
        .expect("Pebble's issuing root");
    let root = CertificateDer::from_pem_slice(root.as_bytes()).unwrap();

    let paths = PublicPaths::new(dir.path());
    let config = AcmeConfig {
        paths: paths.clone(),
        name: NAME.into(),
        staging: directory.clone(),
        production: directory,
        server_root: Some(ca.clone()),
        extra_roots: vec![root.clone()],
        order_timeout: Duration::from_secs(60),
    };

    // One order on its own: the challenge goes through the front door.
    let issued = tokio::time::timeout(
        LIMIT,
        acme::order(&config, CertIssuer::Staging, &door.challenge),
    )
    .await
    .expect("order in time")
    .expect("staging order");
    assert!(issued.validated, "a new name is validated, not reused");
    let mut hit = false;
    while let Ok(event) = door.events.try_recv() {
        hit |= matches!(event, Event::ChallengeHit { ref name } if name == NAME);
    }
    assert!(hit, "Pebble reached the front door");
    assert!(
        door.challenge.borrow().is_none(),
        "the challenge is withdrawn"
    );
    acme::check_chain(&issued.chain_pem, NAME, std::slice::from_ref(&root)).expect("verifies");
    assert!(
        acme::check_chain(
            &issued.chain_pem,
            "other.zeal-test.com",
            std::slice::from_ref(&root)
        )
        .is_err()
    );
    assert!(issued.not_after > SystemTime::now());
    assert!(paths.account("staging").exists());

    // The actor: staging, then production, then the certificate on disk and in the watch.
    let (can_validate_tx, can_validate) = watch::channel(true);
    let mut actor = AcmeHandle::spawn(config.clone(), door.challenge.clone(), can_validate, None);
    actor
        .commands
        .send(acme::AcmeCmd::Order { asked: true })
        .await
        .unwrap();
    let status = tokio::time::timeout(
        LIMIT,
        actor
            .status
            .wait_for(|s| s.not_after.is_some() || s.last_problem.is_some()),
    )
    .await
    .expect("issued in time")
    .unwrap()
    .clone();
    assert_eq!(status.last_problem, None, "{status:?}");
    assert!(status.staging_validated_at.is_some());
    let renew = status.renew_at.expect("renewal scheduled");
    assert!(renew > SystemTime::now() && renew < status.not_after.unwrap());
    assert!(actor.cert.borrow().is_some());
    assert!(actor.validated.borrow().is_some(), "evidence from outside");
    assert!(paths.cert(NAME).exists() && paths.key(NAME).exists());

    // A restart loads the stored certificate and orders nothing.
    drop(actor);
    let stored = acme::load_stored(&paths, NAME, SystemTime::now()).expect("stored");
    acme::check_chain(&stored.chain_pem, NAME, &[root]).expect("stored chain verifies");
    let (_tx, quiet) = watch::channel(true);
    let restarted = AcmeHandle::spawn(config, door.challenge.clone(), quiet, None);
    assert!(restarted.cert.borrow().is_some());
    assert_eq!(restarted.status.borrow().ordering, None);

    drop(can_validate_tx);
    door.commands.send(DoorCommand::Stop).await.unwrap();
}
