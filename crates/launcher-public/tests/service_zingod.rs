//! The service actor with zingod as the backend, against launcher-core's fake-zebrad (through a
//! supervisor) and fake-zingod (through the actor's runner), with a fake app that counts what
//! it is asked to apply. In real time with short intervals: the supervisor's deadlines use the
//! monotonic clock and the actor's rules the wall clock, so a paused tokio clock can drive
//! neither; the mismatch span is shortened instead.
//!
//! The fakes are launcher-core's binaries, which `cargo test --workspace` builds into the same
//! target folder as this test; `cargo test -p launcher-public` alone needs
//! `cargo build -p launcher-core --bins` first. The tests take turns: each picks free ports.

#![cfg(unix)]

use std::{
    future::Future,
    net::{Ipv4Addr, SocketAddr, TcpListener},
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
    time::{Duration, SystemTime},
};

use launcher_core::{
    config::LightwalletdKey,
    nodes::NodeSpec,
    paths::{NodePaths, Paths},
    policy::RestartPolicy,
    process::ZebradCommand,
    settings::{SettingsHandle, SettingsStore},
    supervisor::{self, SupervisorConfig, SupervisorHandle},
};
use launcher_herd::public::zingod::DiskReading;
use launcher_public::{
    EndpointHost, Options, PublicServices, Wiring,
    ddns::{Fetch, Fetched},
    secrets::FileStore,
    zingod::{IndexDisk, RunnerOptions, cross_check},
};
use launcher_types::{
    Network, NodeDetails, NodeStats, Phase,
    public::{
        CONSENT_VERSION, CheckId, CheckOutcome, Finding, LightBackend, PublicStatus, StepAnswer,
    },
};
use tokio::{sync::watch, time::timeout};

#[path = "../../launcher-core/src/bin/fake/chain.rs"]
mod chain;

const LIMIT: Duration = Duration::from_secs(30);
const GB: u64 = 1_000_000_000;

/// One test at a time.
static TURN: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

/// A launcher-core binary in this test's target folder.
fn launcher_core_bin(name: &str) -> PathBuf {
    let exe = std::env::current_exe().expect("this test's path");
    let dir = exe
        .parent()
        .and_then(Path::parent)
        .expect("the target folder");
    let path = dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    assert!(
        path.exists(),
        "{} is missing: build launcher-core's fakes first (`cargo build -p launcher-core --bins`), as `cargo test --workspace` does",
        path.display()
    );
    path
}

fn free_port() -> u16 {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// The app: records each endpoint it is asked to apply on a watch, and restarts nothing.
struct Host {
    applied: watch::Sender<Vec<Option<u16>>>,
}

impl EndpointHost for Host {
    fn apply(
        &self,
        _node: launcher_types::NodeId,
        port: Option<u16>,
        _restart: bool,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> {
        self.applied.send_modify(|applied| applied.push(port));
        Box::pin(async { Ok(()) })
    }
}

struct NoNetwork;

impl Fetch for NoNetwork {
    fn get(&self, _url: String) -> Fetched {
        Box::pin(async { Err(()) })
    }
}

/// The runner's intervals for a test, and a roomy disk.
fn tune(options: &mut RunnerOptions) {
    options.stop_grace = Duration::from_secs(3);
    options.restart = RestartPolicy {
        initial_backoff: Duration::from_millis(50),
        max_backoff: Duration::from_millis(200),
        window: Duration::from_secs(60),
        max_crashes: 3,
    };
    options.status_every = Duration::from_millis(100);
    options.status_timeout = Duration::from_secs(1);
    options.disk_every = Duration::from_millis(100);
    options.size_every = Duration::from_millis(300);
    options.disk = Arc::new(|_| {
        Ok(DiskReading {
            free_bytes: 500 * GB,
            free_inodes: Some(1_000_000),
        })
    });
}

/// The actor's own reading of the index folder: a roomy local disk.
fn roomy(_dir: &Path) -> IndexDisk {
    IndexDisk {
        disk: Some(DiskReading {
            free_bytes: 500 * GB,
            free_inodes: Some(1_000_000),
        }),
        network_fs: false,
        index_bytes: None,
    }
}

struct Setup {
    _turn: tokio::sync::SemaphorePermit<'static>,
    paths: Paths,
    node: NodePaths,
    settings: SettingsHandle,
    services: PublicServices,
    zebrad: SupervisorHandle,
    applied: watch::Receiver<Vec<Option<u16>>>,
    _dir: tempfile::TempDir,
    _stats: watch::Sender<Option<NodeStats>>,
    _details: watch::Sender<Option<NodeDetails>>,
    _node: watch::Sender<NodeSpec>,
}

/// A Testnet first node run by fake-zebrad at its tip, and the actor with fake-zingod in
/// `zingod_mode` bundled.
async fn setup(zingod_mode: &str) -> Setup {
    let turn = TURN.acquire().await.expect("the turns never close");
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::under(dir.path());
    paths.ensure_dirs().unwrap();
    let settings = SettingsStore::open(&paths).unwrap();
    let spec = NodeSpec::first(Network::Testnet);
    settings.save_nodes(vec![spec.clone()]).await.unwrap();
    let node = spec.paths(&paths);
    std::fs::create_dir_all(node.config_dir()).unwrap();
    std::fs::write(
        node.zebrad_config(),
        format!(
            "[rpc]\nlisten_addr = \"127.0.0.1:{}\"\ncookie_dir = '{}'\n",
            free_port(),
            node.rpc_cookie_dir().display()
        ),
    )
    .unwrap();
    let mut command = ZebradCommand::new(launcher_core_bin("fake-zebrad"), node.zebrad_config());
    command.env = vec![
        ("FAKE_ZEBRAD_MODE".into(), "ok".into()),
        ("FAKE_ZEBRAD_HEARTBEAT_MS".into(), "60000".into()),
    ];
    let mut config = SupervisorConfig::new(command);
    config.stop_grace = Duration::from_secs(3);
    config.exit_drain = Duration::from_millis(300);
    let zebrad = supervisor::spawn(config).unwrap();
    timeout(LIMIT, zebrad.start()).await.unwrap().unwrap();
    timeout(LIMIT, zebrad.wait_until(|s| s.rpc_addr.is_some()))
        .await
        .unwrap()
        .unwrap();

    let (stats_tx, stats) = watch::channel(Some(NodeStats {
        height: chain::TIP,
        estimated_height: chain::TIP,
        best: None,
        verification_progress: 1.0,
        peers: 8,
        mempool_transactions: 0,
        chain: "test".into(),
        zebrad_version: "7.0.0".into(),
        last_error: None,
        eos: None,
        updated_at: SystemTime::now(),
    }));
    let details: NodeDetails = serde_json::from_value(serde_json::json!({
        "updated_at": SystemTime::now(),
        "upgrades": [{
            "name": "NU7",
            "branch_id": "77190ad9",
            "activation_height": 4_465_026,
            "status": "active",
        }],
    }))
    .unwrap();
    let (details_tx, details) = watch::channel(Some(details));
    let (node_tx, node_rx) = watch::channel(spec);

    let public = paths.data_dir().join("public");
    std::fs::create_dir_all(&public).unwrap();
    std::fs::write(
        public.join("fake-zingod.env"),
        format!("FAKE_ZINGOD_MODE={zingod_mode}\nFAKE_ZINGOD_POLL_MS=50\nFAKE_ZINGOD_BOOT_MS=50\n"),
    )
    .unwrap();
    let mut options = Options::new(&paths, "7.0.0");
    options.supported = true;
    options.listen_ip = Ipv4Addr::LOCALHOST.into();
    options.acme_staging = "https://acme.invalid/dir".into();
    options.acme_production = "https://acme.invalid/dir".into();
    options.duckdns_api = "https://duckdns.invalid".into();
    options.zingod_binary = Some(launcher_core_bin("fake-zingod"));
    options.tune_zingod = Some(tune);
    options.cross_check_every = Duration::from_millis(200);
    options.index_disk_every = Duration::from_millis(100);
    options.read_index_disk = roomy;
    options.mismatch_span = Duration::from_millis(600);
    let (applied_tx, applied) = watch::channel(Vec::new());
    let services = PublicServices::spawn(
        options,
        Wiring {
            settings: settings.clone(),
            node: node_rx,
            status: zebrad.watch_status(),
            stats,
            details,
            machine: watch::channel(None).1,
            host: Arc::new(Host {
                applied: applied_tx,
            }),
            secrets: Arc::new(FileStore::new(dir.path().join("secrets"))),
            fetch: Arc::new(NoNetwork),
        },
        false,
    );
    Setup {
        _turn: turn,
        paths,
        node,
        settings,
        services,
        zebrad,
        applied,
        _dir: dir,
        _stats: stats_tx,
        _details: details_tx,
        _node: node_tx,
    }
}

impl Setup {
    async fn wait_for(&self, what: &str, f: impl Fn(&PublicStatus) -> bool) -> PublicStatus {
        let mut watch = self.services.watch();
        match timeout(
            LIMIT,
            watch.wait_for(|statuses| statuses.first().is_some_and(&f)),
        )
        .await
        {
            Ok(statuses) => statuses.expect("alive")[0].clone(),
            Err(_) => panic!(
                "the status never showed {what}: {:#?}",
                self.services.statuses()
            ),
        }
    }

    fn applied(&self) -> Vec<Option<u16>> {
        self.applied.borrow().clone()
    }

    fn index_dir(&self) -> PathBuf {
        self.node.data_dir().join("zingod/testnet")
    }

    fn zingod_pid(&self) -> Option<u32> {
        let path = self.paths.data_dir().join("public/zingod-testnet.pid");
        std::fs::read_to_string(path)
            .ok()?
            .lines()
            .next()?
            .trim()
            .parse()
            .ok()
    }

    /// Sets up the light client server on zebrad's endpoint, then chooses zingod with its
    /// consent, as the consent screen does.
    async fn choose_zingod(&self) {
        self.services.begin_setup(Network::Testnet).await.unwrap();
        self.services
            .answer(StepAnswer::Consent {
                version: CONSENT_VERSION,
            })
            .await
            .unwrap();
        let mut applied = self.applied.clone();
        timeout(
            LIMIT,
            applied.wait_for(|applied| applied.iter().any(Option::is_some)),
        )
        .await
        .expect("zebrad's endpoint applied")
        .expect("the host is alive");
        self.services
            .answer(StepAnswer::Backend {
                backend: LightBackend::Zingod,
            })
            .await
            .unwrap();
        self.services
            .answer(StepAnswer::Consent {
                version: CONSENT_VERSION,
            })
            .await
            .unwrap();
    }

    async fn finish(self) {
        timeout(LIMIT, self.services.shutdown()).await.unwrap();
        timeout(LIMIT, self.services.stop_indexer()).await.unwrap();
        let _ = timeout(LIMIT, self.zebrad.shutdown()).await;
    }
}

fn outcome(status: &PublicStatus, id: CheckId) -> Option<CheckOutcome> {
    status
        .steps
        .iter()
        .flat_map(|step| &step.checks)
        .find(|check| check.id == id)
        .map(|check| check.outcome.clone())
}

fn index_ready(status: &PublicStatus) -> bool {
    outcome(status, CheckId::IndexReady) == Some(CheckOutcome::Pass)
}

/// Choosing zingod drops zebrad's endpoint with one apply, which the app turns into one
/// zebrad restart; records zingod's port and folder; starts zingod once zebrad is at the tip;
/// and moves the light monitor, and so the front door's upstream, to zingod's port.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn choosing_zingod_drops_zebrads_endpoint_and_serves_from_zingod() {
    let s = setup("serving").await;
    s.choose_zingod().await;
    let spec = timeout(LIMIT, async {
        loop {
            let spec = s.settings.current().public.light_server().cloned();
            if let Some(spec) = spec.filter(|spec| spec.zingod.is_some()) {
                return spec;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("zingod recorded");
    assert_eq!(spec.backend, LightBackend::Zingod);
    let recorded = spec.zingod.clone().unwrap();
    assert!(recorded.port >= 18137, "{recorded:?}");
    assert_eq!(recorded.dir, s.index_dir());
    assert_eq!(
        spec.consent.as_ref().map(|consent| consent.backend),
        Some(LightBackend::Zingod)
    );

    let status = s
        .wait_for("zingod serving", |status| {
            index_ready(status) && outcome(status, CheckId::TipMatches) == Some(CheckOutcome::Pass)
        })
        .await;
    assert_eq!(
        status.light.as_ref().and_then(|light| light.endpoint),
        Some(SocketAddr::from((Ipv4Addr::LOCALHOST, recorded.port))),
        "the light monitor and the front door's upstream follow zingod"
    );
    let indexer = status.indexer.expect("the Server card's facts");
    assert_eq!(indexer.phase, Phase::Running);
    assert_eq!(indexer.dir, s.index_dir());

    let spec = s.settings.current().public.light_server().cloned().unwrap();
    assert_eq!(spec.backend_port, None, "zebrad's endpoint is gone");
    assert_eq!(
        LightwalletdKey::for_public(&s.settings.current().public, Network::Testnet),
        LightwalletdKey::Absent
    );
    let applied = s.applied();
    let after_endpoint = applied
        .iter()
        .position(Option::is_some)
        .expect("the endpoint was applied first");
    assert_eq!(
        &applied[after_endpoint + 1..],
        [None],
        "one apply, one zebrad restart: {applied:?}"
    );

    // zingod's tip has zebrad's hash, and a height above zebrad's is skipped.
    let rpc = s.zebrad.status().rpc_addr.unwrap();
    let zingod = SocketAddr::from((Ipv4Addr::LOCALHOST, recorded.port));
    let cookie = s.node.rpc_cookie_file();
    assert_eq!(
        cross_check(zingod, rpc, cookie.clone(), Some(chain::TIP)).await,
        Some((chain::TIP, launcher_herd::public::zingod::TipCheck::Match))
    );
    assert_eq!(
        cross_check(zingod, rpc, cookie, Some(chain::TIP - 1)).await,
        Some((chain::TIP, launcher_herd::public::zingod::TipCheck::Skipped))
    );
    s.finish().await;
}

/// Quitting closes the front door first and leaves zingod running; zingod's stop comes after,
/// beside the nodes'.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn zingod_stops_after_the_front_door_closes() {
    let s = setup("serving").await;
    s.choose_zingod().await;
    s.wait_for("zingod serving", index_ready).await;
    let pid = s.zingod_pid().expect("zingod's pid file");
    timeout(LIMIT, s.services.shutdown()).await.unwrap();
    assert!(
        launcher_core::pidfile::is_alive(pid),
        "zingod outlives the front door's close"
    );
    timeout(LIMIT, s.services.stop_indexer()).await.unwrap();
    assert!(!launcher_core::pidfile::is_alive(pid), "and then stops");
    let _ = timeout(LIMIT, s.zebrad.shutdown()).await;
}

/// "Turn off and forget" with the box ticked stops zingod and removes its index and state
/// from the recorded folder.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn forget_removes_the_index() {
    let s = setup("serving").await;
    s.choose_zingod().await;
    s.wait_for("zingod serving", index_ready).await;
    let index = s.index_dir();
    assert!(
        index.join("indexes").is_dir(),
        "fake-zingod's lock is in it"
    );
    // zingod's hold and overload markers.
    std::fs::create_dir_all(index.join("state")).unwrap();
    std::fs::write(index.join("state/held"), b"").unwrap();
    let pid = s.zingod_pid().expect("zingod's pid file");
    s.services.forget(false, true).await.unwrap();
    assert!(
        !launcher_core::pidfile::is_alive(pid),
        "zingod stopped first"
    );
    assert!(!index.join("indexes").exists(), "the index is gone");
    assert!(!index.join("state").exists());
    assert!(s.settings.current().public.light_server().is_none());
    s.finish().await;
}

/// zingod's tip with another hash, three times over the span: ChainMismatch, zingod stops,
/// and the spec keeps it through a change of backend. Rebuild clears it and starts zingod on a
/// new index.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_mismatching_tips_stop_zingod_until_a_rebuild() {
    let s = setup("wrong-hash").await;
    s.choose_zingod().await;
    let mismatch = |status: &PublicStatus| {
        matches!(
            outcome(status, CheckId::TipMatches),
            Some(CheckOutcome::Fail {
                finding: Finding::ChainMismatch { .. }
            })
        )
    };
    let status = s.wait_for("the mismatch", mismatch).await;
    assert_eq!(
        outcome(&status, CheckId::TipMatches),
        Some(CheckOutcome::Fail {
            finding: Finding::ChainMismatch { at: chain::TIP }
        })
    );
    s.wait_for("zingod stopped", |status| {
        status
            .indexer
            .as_ref()
            .is_some_and(|indexer| !indexer.phase.is_active())
    })
    .await;
    let kept = || {
        s.settings
            .current()
            .public
            .light_server()
            .and_then(|spec| spec.zingod.as_ref().and_then(|zingod| zingod.mismatch))
    };
    timeout(LIMIT, async {
        while kept().is_none() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the spec keeps the mismatch");
    assert_eq!(kept(), Some(chain::TIP));
    // Zebra's built-in server and back: the index is the same, and so is its chain.
    for backend in [LightBackend::ZebradEndpoint, LightBackend::Zingod] {
        s.services
            .answer(StepAnswer::Backend { backend })
            .await
            .unwrap();
        s.services
            .answer(StepAnswer::Consent {
                version: CONSENT_VERSION,
            })
            .await
            .unwrap();
    }
    s.wait_for("the mismatch again, with zingod stopped", |status| {
        status.backend == LightBackend::Zingod
            && mismatch(status)
            && status
                .indexer
                .as_ref()
                .is_some_and(|indexer| !indexer.phase.is_active())
    })
    .await;
    s.services.rebuild_index().await.unwrap();
    assert_eq!(kept(), None, "the rebuild clears it");
    s.wait_for("zingod again", |status| {
        status
            .indexer
            .as_ref()
            .is_some_and(|indexer| indexer.phase == Phase::Running)
            && !matches!(
                outcome(status, CheckId::TipMatches),
                Some(CheckOutcome::Fail { .. })
            )
    })
    .await;
    s.finish().await;
}

/// Switching back to zebrad's endpoint stops zingod and keeps its index, which Delete then
/// removes; Delete is refused while zingod serves.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn switching_back_keeps_the_index_until_delete() {
    let s = setup("serving").await;
    s.choose_zingod().await;
    s.wait_for("zingod serving", index_ready).await;
    assert!(s.services.delete_index().await.is_err(), "zingod serves");
    let pid = s.zingod_pid().expect("zingod's pid file");
    s.services
        .answer(StepAnswer::Backend {
            backend: LightBackend::ZebradEndpoint,
        })
        .await
        .unwrap();
    s.services
        .answer(StepAnswer::Consent {
            version: CONSENT_VERSION,
        })
        .await
        .unwrap();
    timeout(LIMIT, async {
        while launcher_core::pidfile::is_alive(pid) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("zingod stops");
    let index = s.index_dir();
    assert!(index.join("indexes").is_dir(), "the index stays");
    let spec = s.settings.current().public.light_server().cloned().unwrap();
    assert!(spec.zingod.is_some(), "and its folder is remembered");
    s.wait_for("the index's folder on the status", |status| {
        status.backend == LightBackend::ZebradEndpoint && status.indexer.is_some()
    })
    .await;
    s.services.delete_index().await.unwrap();
    assert!(!index.join("indexes").exists());
    assert!(!index.join("state").exists());
    s.finish().await;
}

/// A Rebuild whose removal fails still starts zingod again: the runner holds zingod stopped
/// after its removal until the plan changes, and the rebuild's new attempt is that change.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rebuild_that_cant_remove_the_index_still_starts_zingod() {
    use std::os::unix::fs::PermissionsExt;
    let s = setup("serving").await;
    s.choose_zingod().await;
    s.wait_for("zingod serving", index_ready).await;
    let pid = s.zingod_pid().expect("zingod's pid file");
    // `indexes/` can't leave a folder that can't change.
    let index = s.index_dir();
    let mode = |mode| std::fs::set_permissions(&index, std::fs::Permissions::from_mode(mode));
    mode(0o555).unwrap();
    if std::fs::write(index.join("probe"), b"").is_ok() {
        // Root writes there anyway: nothing to see.
        mode(0o755).unwrap();
        s.finish().await;
        return;
    }
    let rebuilt = s.services.rebuild_index().await;
    mode(0o755).unwrap();
    assert!(rebuilt.is_err(), "{rebuilt:?}");
    s.wait_for("zingod started again", |status| {
        status
            .indexer
            .as_ref()
            .is_some_and(|indexer| indexer.phase.is_active())
            && s.zingod_pid().is_some_and(|new| new != pid)
    })
    .await;
    s.finish().await;
}

/// "Turn off and forget" goes through when zingod's index can't be removed: the switch-off is
/// saved first, the service is gone, and what is left is reported.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn forget_goes_through_when_the_index_cant_be_removed() {
    use std::os::unix::fs::PermissionsExt;
    let s = setup("serving").await;
    s.choose_zingod().await;
    s.wait_for("zingod serving", index_ready).await;
    let index = s.index_dir();
    let mode = |mode| std::fs::set_permissions(&index, std::fs::Permissions::from_mode(mode));
    mode(0o555).unwrap();
    if std::fs::write(index.join("probe"), b"").is_ok() {
        // Root writes there anyway: nothing to see.
        mode(0o755).unwrap();
        s.finish().await;
        return;
    }
    let still = s.services.forget(false, true).await;
    mode(0o755).unwrap();
    let still = still.expect("the forget goes through");
    assert!(
        still
            .index_left
            .as_deref()
            .is_some_and(|left| left.contains("indexes")),
        "{still:?}"
    );
    assert!(s.settings.current().public.light_server().is_none());
    s.finish().await;
}
