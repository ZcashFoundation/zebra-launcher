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
    secrets::{FileStore, SecretError, SecretStore},
};
use launcher_types::{
    Network, NodeStats, Phase, Status,
    public::{CONSENT_VERSION, PauseReason, PublicState, PublicStatus, StepAnswer, StepId},
};
use tokio::sync::watch;

const LIMIT: Duration = Duration::from_secs(10);

/// The app: applying "restarts zebrad" by publishing the endpoint it was given.
struct Host {
    status: watch::Sender<Status>,
    applied: Arc<AtomicU32>,
}

impl EndpointHost for Host {
    fn apply(
        &self,
        node: launcher_types::NodeId,
        port: Option<u16>,
        _restart: bool,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> {
        assert_eq!(
            node.as_str(),
            "zebrad-mainnet",
            "the node the facts come from"
        );
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
    dir: tempfile::TempDir,
    settings: SettingsHandle,
    services: PublicServices,
    applied: Arc<AtomicU32>,
    _status: watch::Sender<Status>,
    _stats: watch::Sender<Option<NodeStats>>,
    _node: watch::Sender<launcher_core::nodes::NodeSpec>,
}

async fn setup() -> Setup {
    setup_with(|dir| Arc::new(FileStore::new(dir.join("secrets")))).await
}

async fn setup_with(secrets: impl FnOnce(&std::path::Path) -> Arc<dyn SecretStore>) -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::under(dir.path());
    let settings = SettingsStore::open(&paths).unwrap();
    let (status_tx, status) = watch::channel(Status {
        phase: Phase::Running,
        ..Status::default()
    });
    let (stats_tx, stats) = watch::channel(None::<NodeStats>);
    let applied = Arc::new(AtomicU32::new(0));
    let (node_tx, node) = watch::channel(launcher_core::nodes::NodeSpec::first(Network::Mainnet));
    let host = Host {
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
            node,
            status,
            stats,
            host: Arc::new(host),
            secrets: secrets(dir.path()),
            fetch: Arc::new(NoNetwork),
        },
        false,
    );
    Setup {
        dir,
        settings,
        services,
        applied,
        _status: status_tx,
        _stats: stats_tx,
        _node: node_tx,
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
    assert!(settings.contains("dns:duckdns:duckdns.org"));
    let statuses = serde_json::to_string(&s.services.statuses()).unwrap();
    assert!(!statuses.contains(token));
}

const TOKEN: &str = "a7c4d0ad-114e-40ef-ba1d-d217904a50f2";

/// The token files left in the setup's store.
fn tokens_left(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir.join("secrets"))
        .map(|entries| {
            entries
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

async fn name(s: &Setup, name: &str, provider: launcher_types::public::DnsProvider) {
    s.services
        .answer(StepAnswer::Name {
            name: name.into(),
            provider,
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn forget_deletes_the_token_after_a_new_name_and_provider() {
    use launcher_types::public::DnsProvider;
    let s = setup().await;
    s.services.begin_setup(Network::Mainnet).await.unwrap();
    name(&s, "zec-alyce", DnsProvider::DuckDns).await;
    s.services
        .answer(StepAnswer::Token {
            token: TOKEN.into(),
        })
        .await
        .unwrap();
    // The token serves every name of the DuckDNS account: one entry, kept across names.
    name(&s, "zec-alice", DnsProvider::DuckDns).await;
    assert_eq!(tokens_left(s.dir.path()), ["dns_duckdns_duckdns.org"]);
    let spec = s.settings.current().public.light_server().cloned().unwrap();
    assert_eq!(spec.secret.unwrap().account, "dns:duckdns:duckdns.org");
    name(&s, "zec.example.com", DnsProvider::Manual).await;

    let still = s.services.forget(true).await.unwrap();
    assert!(!still.token_left);
    assert_eq!(tokens_left(s.dir.path()), Vec::<String>::new());
}

#[tokio::test]
async fn a_token_kept_for_an_old_name_moves_to_the_zone() {
    use launcher_types::public::DnsProvider;
    let s = setup().await;
    s.services.begin_setup(Network::Mainnet).await.unwrap();
    name(&s, "zec-old", DnsProvider::DuckDns).await;
    // An older Zeal kept the token under the name.
    let store = FileStore::new(s.dir.path().join("secrets"));
    store
        .set("dns:duckdns:zec-old.duckdns.org".into(), TOKEN.into())
        .await
        .unwrap();
    s.settings
        .update(|settings| {
            settings.public.light_server_mut().unwrap().secret =
                Some(launcher_types::public::SecretRef {
                    provider: DnsProvider::DuckDns,
                    account: "dns:duckdns:zec-old.duckdns.org".into(),
                });
        })
        .await
        .unwrap();
    name(&s, "zec-new", DnsProvider::DuckDns).await;
    assert_eq!(tokens_left(s.dir.path()), ["dns_duckdns_duckdns.org"]);
    assert_eq!(
        store.get("dns:duckdns:duckdns.org".into()).await.unwrap(),
        TOKEN
    );
    s.services.forget(true).await.unwrap();
    assert_eq!(tokens_left(s.dir.path()), Vec::<String>::new());
}

/// A keychain that keeps tokens and refuses to delete them, as a locked one does.
struct Locked(FileStore);

impl SecretStore for Locked {
    fn set(
        &self,
        account: String,
        token: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), SecretError>> + Send>> {
        self.0.set(account, token)
    }

    fn get(
        &self,
        account: String,
    ) -> Pin<Box<dyn Future<Output = Result<String, SecretError>> + Send>> {
        self.0.get(account)
    }

    fn delete(
        &self,
        _account: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), SecretError>> + Send>> {
        Box::pin(async { Err(SecretError::Unavailable("locked".into())) })
    }
}

#[tokio::test]
async fn forget_says_when_the_keychain_kept_the_token() {
    let s = setup_with(|dir| Arc::new(Locked(FileStore::new(dir.join("secrets"))))).await;
    s.services.begin_setup(Network::Mainnet).await.unwrap();
    name(
        &s,
        "zec-alice",
        launcher_types::public::DnsProvider::DuckDns,
    )
    .await;
    s.services
        .answer(StepAnswer::Token {
            token: TOKEN.into(),
        })
        .await
        .unwrap();
    assert!(s.services.forget(true).await.unwrap().token_left);
    assert!(!s.settings.current().public.light_server().is_some());
}

/// launcher.json cannot be written: the off still holds.
#[cfg(unix)]
#[tokio::test]
async fn an_off_that_cannot_be_saved_still_closes_the_service() {
    use std::os::unix::fs::PermissionsExt;
    let s = setup().await;
    s.services.begin_setup(Network::Mainnet).await.unwrap();
    s.services
        .answer(StepAnswer::Consent {
            version: CONSENT_VERSION,
        })
        .await
        .unwrap();
    wait_for(&s.services, |status| {
        status
            .light
            .as_ref()
            .is_some_and(|light| light.endpoint.is_some())
    })
    .await;
    let config_dir = Paths::under(s.dir.path())
        .settings_file()
        .parent()
        .unwrap()
        .to_path_buf();
    std::fs::set_permissions(&config_dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    if std::fs::write(config_dir.join("probe"), b"").is_ok() {
        // Permissions do not bind this user (root): nothing to test.
        std::fs::set_permissions(&config_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        return;
    }
    let applied = s.applied.load(Ordering::SeqCst);
    let off = s.services.set_switch(false).await;
    // Several looks later, the service is still paused by the user, and zebrad's endpoint is
    // gone although the stored spec still has its port.
    for _ in 0..3 {
        s.services.run_checks(StepId::Consent).await.unwrap();
    }
    let mut watch = s.services.watch();
    let removed = tokio::time::timeout(
        LIMIT,
        watch.wait_for(|statuses| {
            statuses.first().is_some_and(|status| {
                status
                    .light
                    .as_ref()
                    .is_some_and(|light| light.endpoint.is_none())
            })
        }),
    )
    .await
    .is_ok();
    let status = s.services.statuses()[0].clone();
    let stored = backend_port(&s.settings);
    std::fs::set_permissions(&config_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(off.is_err(), "the off was not saved");
    assert!(stored.is_some(), "the port could not be cleared either");
    assert!(removed, "the endpoint is removed all the same");
    assert!(s.applied.load(Ordering::SeqCst) > applied);
    assert!(
        matches!(
            status.state,
            PublicState::Paused {
                why: PauseReason::ByYou,
                ..
            }
        ),
        "{:?}",
        status.state
    );
    assert_eq!(status.gateway, None, "the front door stays closed");
    // Once the file can be written again, on resumes.
    s.services.set_switch(true).await.unwrap();
    wait_for(&s.services, |status| {
        !matches!(status.state, PublicState::Paused { .. })
    })
    .await;
}

#[tokio::test]
async fn what_was_proven_belongs_to_its_name() {
    use launcher_types::public::{DnsProvider, Reachability};
    // An outside connection a minute ago, for zec-old only.
    let at = std::time::SystemTime::now() - Duration::from_secs(60);
    let state = serde_json::json!({
        "names": {
            "zec-old.duckdns.org": {
                "evidence": [{ "source": "outside_tls_connection", "at": at }],
                "staging_validated_at": at,
            }
        }
    });
    let s = setup_with(move |dir| {
        std::fs::create_dir_all(dir.join("public")).unwrap();
        std::fs::write(dir.join("public/state.json"), state.to_string()).unwrap();
        Arc::new(FileStore::new(dir.join("secrets")))
    })
    .await;
    s.services.begin_setup(Network::Mainnet).await.unwrap();
    name(&s, "zec-new", DnsProvider::DuckDns).await;
    let status = wait_for(&s.services, |status| status.name.is_some()).await;
    assert_eq!(
        status.reach,
        Reachability::NotChecked,
        "nothing proven for zec-new"
    );
    name(&s, "zec-old", DnsProvider::DuckDns).await;
    let status = wait_for(&s.services, |status| {
        status
            .name
            .as_ref()
            .is_some_and(|name| name.as_str() == "zec-old.duckdns.org")
    })
    .await;
    assert!(
        matches!(status.reach, Reachability::Proven { .. }),
        "{:?}",
        status.reach
    );
    // Forgotten, nothing stays proven.
    s.services.forget(false).await.unwrap();
    let saved = std::fs::read_to_string(s.dir.path().join("public/state.json")).unwrap();
    assert!(!saved.contains("zec-old"), "{saved}");
}
