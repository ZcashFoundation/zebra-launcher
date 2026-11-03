//! The service actor against a fake app: the endpoint key exists only while the service is
//! switched on with consent, every change is applied through the app, and off and forget take
//! it away again.

use std::{
    future::Future,
    net::{Ipv4Addr, SocketAddr},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};

use launcher_core::{
    paths::Paths,
    settings::{SettingsHandle, SettingsStore},
};
use launcher_public::{
    EndpointHost, Options, PublicServices, Wiring,
    ddns::{Fetch, Fetched},
    secrets::FileStore,
};
use launcher_types::{
    Network, NodeStats, Phase, Status,
    public::{CONSENT_VERSION, PauseReason, PublicState, PublicStatus, StepAnswer, StepId},
};
use tokio::sync::watch;

const LIMIT: Duration = Duration::from_secs(10);

/// The app: applying reads the stored spec and "restarts zebrad" by publishing the endpoint
/// it would open.
struct Host {
    settings: SettingsHandle,
    status: watch::Sender<Status>,
    applied: Arc<AtomicU32>,
}

impl EndpointHost for Host {
    fn apply(&self, _restart: bool) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> {
        let port = self
            .settings
            .current()
            .public
            .light_server()
            .and_then(|spec| spec.backend_port);
        self.status.send_modify(|status| {
            status.lightwalletd_addr =
                port.map(|port| SocketAddr::from((Ipv4Addr::LOCALHOST, port)));
        });
        self.applied.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
}

struct NoNetwork;

impl Fetch for NoNetwork {
    fn get(&self, _url: String) -> Fetched {
        Box::pin(async { Err(()) })
    }
}

struct Setup {
    _dir: tempfile::TempDir,
    settings: SettingsHandle,
    services: PublicServices,
    applied: Arc<AtomicU32>,
    _status: watch::Sender<Status>,
    _stats: watch::Sender<Option<NodeStats>>,
}

async fn setup() -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::under(dir.path());
    let settings = SettingsStore::open(&paths).unwrap();
    let (status_tx, status) = watch::channel(Status {
        phase: Phase::Running,
        ..Status::default()
    });
    let (stats_tx, stats) = watch::channel(None::<NodeStats>);
    let applied = Arc::new(AtomicU32::new(0));
    let host = Host {
        settings: settings.clone(),
        status: status_tx.clone(),
        applied: applied.clone(),
    };
    let mut options = Options::new(dir.path(), "7.0.0");
    options.supported = true;
    options.listen_ip = Ipv4Addr::LOCALHOST.into();
    options.acme_staging = "https://acme.invalid/dir".into();
    options.acme_production = "https://acme.invalid/dir".into();
    options.duckdns_api = "https://duckdns.invalid".into();
    let services = PublicServices::spawn(
        options,
        Wiring {
            settings: settings.clone(),
            status,
            stats,
            host: Arc::new(host),
            secrets: Arc::new(FileStore::new(dir.path().join("secrets"))),
            fetch: Arc::new(NoNetwork),
        },
        false,
    );
    Setup {
        _dir: dir,
        settings,
        services,
        applied,
        _status: status_tx,
        _stats: stats_tx,
    }
}

async fn wait_for(services: &PublicServices, f: impl Fn(&PublicStatus) -> bool) -> PublicStatus {
    let mut watch = services.watch();
    tokio::time::timeout(
        LIMIT,
        watch.wait_for(|statuses| statuses.first().is_some_and(&f)),
    )
    .await
    .expect("in time")
    .expect("alive")[0]
        .clone()
}

fn backend_port(settings: &SettingsHandle) -> Option<u16> {
    settings
        .current()
        .public
        .light_server()
        .and_then(|spec| spec.backend_port)
}

#[tokio::test]
async fn the_endpoint_exists_only_while_switched_on_with_consent() {
    let s = setup().await;
    assert!(s.services.statuses().is_empty(), "nothing set up yet");
    assert_eq!(
        s.services.begin_setup(Network::Testnet).await,
        Err("Zeal runs zebrad on the other network; switch to Testnet first".into())
    );
    let node = s.services.begin_setup(Network::Mainnet).await.unwrap();
    assert_eq!(node.as_str(), "zebrad-mainnet");

    let status = wait_for(&s.services, |status| {
        status.state
            == PublicState::SettingUp {
                step: StepId::Consent,
            }
    })
    .await;
    assert!(status.switch_on);
    assert_eq!(
        backend_port(&s.settings),
        None,
        "no endpoint before consent"
    );

    s.services
        .answer(StepAnswer::Consent {
            version: CONSENT_VERSION,
        })
        .await
        .unwrap();
    let status = wait_for(&s.services, |status| {
        status
            .light
            .as_ref()
            .is_some_and(|light| light.endpoint.is_some())
    })
    .await;
    let port = backend_port(&s.settings).expect("the endpoint is on");
    assert!(port >= 9067, "{port}");
    assert_eq!(
        status.light.unwrap().endpoint,
        Some(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
    );
    assert!(s.applied.load(Ordering::SeqCst) >= 1);

    // Off: paused at once, the key removed through the app.
    let before = s.applied.load(Ordering::SeqCst);
    s.services.set_switch(false).await.unwrap();
    let status = wait_for(&s.services, |status| {
        matches!(
            status.state,
            PublicState::Paused {
                why: PauseReason::ByYou,
                ..
            }
        ) && status
            .light
            .as_ref()
            .is_some_and(|light| light.endpoint.is_none())
    })
    .await;
    assert_eq!(status.gateway, None, "the front door is closed");
    assert_eq!(backend_port(&s.settings), None);
    assert!(s.applied.load(Ordering::SeqCst) > before);

    // On again: the endpoint comes back.
    s.services.set_switch(true).await.unwrap();
    wait_for(&s.services, |status| {
        status
            .light
            .as_ref()
            .is_some_and(|light| light.endpoint.is_some())
    })
    .await;

    // Forget: the key goes first, then the spec.
    let still = s.services.forget(true).await.unwrap();
    assert_eq!(still.name, None);
    assert_eq!(backend_port(&s.settings), None);
    assert!(s.settings.current().public.is_empty());
    let mut watch = s.services.watch();
    tokio::time::timeout(LIMIT, watch.wait_for(Vec::is_empty))
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn answers_shape_the_spec_and_tokens_stay_out_of_it() {
    let s = setup().await;
    s.services.begin_setup(Network::Mainnet).await.unwrap();
    s.services
        .answer(StepAnswer::Consent {
            version: CONSENT_VERSION,
        })
        .await
        .unwrap();
    assert!(
        s.services
            .answer(StepAnswer::Consent { version: 0 })
            .await
            .is_err()
    );
    s.services
        .answer(StepAnswer::Name {
            name: "zeal-test".into(),
            provider: launcher_types::public::DnsProvider::DuckDns,
        })
        .await
        .unwrap();
    let spec = s.settings.current().public.light_server().cloned().unwrap();
    assert_eq!(spec.name.unwrap().as_str(), "zeal-test.duckdns.org");
    assert_eq!(
        spec.consent.unwrap().hostname,
        "zeal-test.duckdns.org",
        "the first name binds to the consent"
    );
    assert!(
        s.services
            .answer(StepAnswer::Token {
                token: "not a token".into()
            })
            .await
            .is_err()
    );
    let token = "a7c4d0ad-114e-40ef-ba1d-d217904a50f2";
    s.services
        .answer(StepAnswer::Token {
            token: token.into(),
        })
        .await
        .unwrap();
    let settings = serde_json::to_string(&s.settings.current()).unwrap();
    assert!(!settings.contains(token), "{settings}");
    assert!(settings.contains("dns:duckdns:zeal-test.duckdns.org"));
    let statuses = serde_json::to_string(&s.services.statuses()).unwrap();
    assert!(!statuses.contains(token));
}
