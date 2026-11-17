//! The `ZingodRunner` against launcher-core's fake-zingod and fake-zebrad, in real time with
//! short intervals: the supervisor's deadlines use the monotonic clock, so a paused tokio clock
//! can't drive them. zingod runs on Linux only, and a stop sends it `SIGTERM`.
//!
//! The tests take turns: each picks free ports and the runner moves to the next free one, so
//! two at once could take each other's.
//!
//! The fakes are launcher-core's binaries, which `cargo test --workspace` builds into the same
//! target folder as this test; `cargo test -p launcher-public` alone needs
//! `cargo build -p launcher-core --bins` first.

#![cfg(unix)]

use std::{
    net::{Ipv4Addr, SocketAddr, TcpListener},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime},
};

use launcher_core::{
    paths::Paths,
    policy::RestartPolicy,
    process::ZebradCommand,
    supervisor::{self, SupervisorConfig, SupervisorHandle},
    zingod::{ZingodConfig, ZingodNetwork},
};
use launcher_herd::public::zingod::{
    Blocked, DiskFullKind, DiskReading, StartFacts, Want, ZebradRun, ZingodFacts, want,
};
use launcher_public::{
    grpc,
    zingod::{RunnerOptions, ZingodPlan, ZingodRunner, zebrad_facts},
};
use launcher_types::{ExitClass, Phase, Status};
use tokio::{sync::watch, time::timeout};

const LIMIT: Duration = Duration::from_secs(20);

/// One test at a time.
static TURN: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
const GB: u64 = 1_000_000_000;

#[path = "../../launcher-core/src/bin/fake/chain.rs"]
mod chain;

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

fn fast() -> RestartPolicy {
    RestartPolicy {
        initial_backoff: Duration::from_millis(50),
        max_backoff: Duration::from_millis(200),
        window: Duration::from_secs(60),
        max_crashes: 3,
    }
}

/// The scripted disk the runner reads.
#[derive(Clone)]
struct Disk {
    free_bytes: Arc<AtomicU64>,
}

impl Disk {
    fn set(&self, free_bytes: u64) {
        self.free_bytes.store(free_bytes, Ordering::SeqCst);
    }
}

struct Setup {
    _turn: tokio::sync::SemaphorePermit<'static>,
    dir: tempfile::TempDir,
    zebrad: SupervisorHandle,
    cookie: PathBuf,
    plan: watch::Sender<Option<ZingodPlan>>,
    runner: ZingodRunner,
    disk: Disk,
    index_dir: PathBuf,
    port: u16,
}

/// fake-zebrad in `zebrad_mode` through a supervisor, and a runner over fake-zingod in
/// `zingod_mode`, with no plan yet.
async fn setup(
    zebrad_mode: &str,
    zingod_mode: &str,
    tune: impl FnOnce(&mut RunnerOptions),
) -> Setup {
    let turn = TURN.acquire().await.expect("the turns never close");
    let dir = tempfile::tempdir().unwrap();
    let node = dir.path().join("node");
    std::fs::create_dir_all(&node).unwrap();
    let rpc_port = free_port();
    let zebrad_toml = node.join("zebrad.toml");
    std::fs::write(
        &zebrad_toml,
        format!(
            "[rpc]\nlisten_addr = \"127.0.0.1:{rpc_port}\"\ncookie_dir = '{}'\n",
            node.join("rpc").display()
        ),
    )
    .unwrap();
    let mut command = ZebradCommand::new(launcher_core_bin("fake-zebrad"), &zebrad_toml);
    command.env = vec![
        ("FAKE_ZEBRAD_MODE".into(), zebrad_mode.into()),
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

    let paths = Paths::under(dir.path());
    let disk = Disk {
        free_bytes: Arc::new(AtomicU64::new(500 * GB)),
    };
    let reader = disk.clone();
    let mut options = RunnerOptions::new(
        launcher_core_bin("fake-zingod"),
        &paths,
        ZingodNetwork::Testnet,
    );
    options.stop_grace = Duration::from_secs(3);
    options.restart = fast();
    options.status_every = Duration::from_millis(100);
    options.status_timeout = Duration::from_secs(1);
    options.disk_every = Duration::from_millis(100);
    options.size_every = Duration::from_millis(500);
    options.disk = Arc::new(move |_| {
        Ok(DiskReading {
            free_bytes: reader.free_bytes.load(Ordering::SeqCst),
            free_inodes: Some(1_000),
        })
    });
    tune(&mut options);
    std::fs::create_dir_all(options.config_file.parent().unwrap()).unwrap();
    let setup_settings = options.config_file.with_file_name("fake-zingod.env");
    std::fs::write(
        &setup_settings,
        format!(
            "FAKE_ZINGOD_MODE={zingod_mode}\nFAKE_ZINGOD_POLL_MS=50\nFAKE_ZINGOD_FAIL_MS=300\n"
        ),
    )
    .unwrap();
    let (plan, plan_rx) = watch::channel(None);
    let runner = ZingodRunner::spawn(options, plan_rx);
    Setup {
        _turn: turn,
        index_dir: node.join("zingod/testnet"),
        cookie: node.join("rpc/.cookie"),
        dir,
        zebrad,
        plan,
        runner,
        disk,
        port: free_port(),
    }
}

impl Setup {
    /// The plan for zebrad's current run.
    fn plan_for(&self, status: &Status, attempt: u64) -> ZingodPlan {
        ZingodPlan {
            config: ZingodConfig {
                network: ZingodNetwork::Testnet,
                validator: status.rpc_addr.expect("zebrad's RPC address"),
                cookie: self.cookie.clone(),
                listen: SocketAddr::from((Ipv4Addr::LOCALHOST, self.port)),
                index_dir: self.index_dir.clone(),
            },
            zebrad: ZebradRun::of(status).expect("zebrad runs"),
            attempt,
        }
    }

    fn give_plan(&self, attempt: u64) -> ZingodPlan {
        let plan = self.plan_for(&self.zebrad.status(), attempt);
        self.plan.send_replace(Some(plan.clone()));
        plan
    }

    fn settings(&self, lines: &str) {
        let path = self.dir.path().join("data/public/fake-zingod.env");
        std::fs::write(path, lines).unwrap();
    }

    async fn facts_until(
        &self,
        what: &str,
        predicate: impl Fn(&ZingodFacts) -> bool,
    ) -> ZingodFacts {
        let mut facts = self.runner.facts();
        timeout(LIMIT, async {
            loop {
                {
                    let current = facts.borrow_and_update();
                    if predicate(&current) {
                        return current.clone();
                    }
                }
                facts.changed().await.expect("the runner is alive");
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "the runner's facts never showed {what}: {:?}",
                facts.borrow()
            )
        })
    }

    /// The facts stay as `predicate` says for `span`.
    async fn facts_stay(
        &self,
        what: &str,
        span: Duration,
        predicate: impl Fn(&ZingodFacts) -> bool,
    ) {
        let end = tokio::time::Instant::now() + span;
        while tokio::time::Instant::now() < end {
            let facts = self.runner.facts().borrow().clone();
            assert!(predicate(&facts), "{what}: {facts:?}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn finish(self) {
        timeout(LIMIT, self.runner.shutdown()).await.unwrap();
        timeout(LIMIT, self.zebrad.shutdown())
            .await
            .unwrap()
            .unwrap();
    }
}

fn answering(facts: &ZingodFacts, count: u32) -> bool {
    facts
        .status
        .as_ref()
        .is_some_and(|status| status.answering == Some(count))
}

fn running(facts: &ZingodFacts) -> bool {
    facts.phase == Phase::Running
}

/// zebrad 7.0.0 closes the connection on a wrong cookie, so zingod, which rereads its cookie
/// only after a 401, fails for the rest of its run once zebrad restarts. The restart with the
/// new zebrad run is what brings it back.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_zebrad_run_restarts_zingod_and_only_that_cures_the_stale_cookie() {
    let setup = setup("close-on-wrong-cookie", "serving", |_| {}).await;
    let first = setup.give_plan(0);
    let facts = setup
        .facts_until("zingod answering", |f| running(f) && answering(f, 1))
        .await;
    assert_eq!(facts.running_for, Some(first.zebrad));
    assert_eq!(facts.port, Some(setup.port));
    let status = facts.status.unwrap();
    assert_eq!(status.network, "testnet");
    assert_eq!(status.served, Some(chain::TIP));

    timeout(LIMIT, setup.zebrad.restart())
        .await
        .unwrap()
        .unwrap();
    let status = timeout(LIMIT, setup.zebrad.wait_until(|s| s.rpc_addr.is_some()))
        .await
        .unwrap()
        .unwrap();
    assert_ne!(ZebradRun::of(&status), Some(first.zebrad));
    setup
        .facts_until("the stale cookie failing", |f| answering(f, 0))
        .await;
    setup
        .facts_stay(
            "the stale cookie never works",
            Duration::from_millis(800),
            |f| answering(f, 0),
        )
        .await;

    let second = setup.give_plan(0);
    let facts = setup
        .facts_until("zingod back on the new run", |f| {
            f.running_for == Some(second.zebrad) && running(f) && answering(f, 1)
        })
        .await;
    let restarted = facts.last_exit.expect("the first zingod stopped");
    assert!(
        restarted.requested,
        "Zeal stopped it for the new run: {restarted:?}"
    );
    setup.finish().await;
}

/// The barrier is the run's RPC line, not the cookie's time: a cookie older than the start the
/// supervisor recorded (it records the start once the spawn returned) still lets zingod start.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cookie_older_than_the_recorded_start_still_starts_zingod() {
    let setup = setup("ok", "serving", |_| {}).await;
    let cookie = std::fs::File::options()
        .write(true)
        .open(&setup.cookie)
        .unwrap();
    cookie
        .set_modified(SystemTime::now() - Duration::from_secs(3_600))
        .unwrap();
    drop(cookie);
    let status = setup.zebrad.status();
    let modified = std::fs::metadata(&setup.cookie)
        .unwrap()
        .modified()
        .unwrap();
    assert!(
        modified < status.started_at.unwrap(),
        "the cookie is the older"
    );

    let start = |zebrad| StartFacts {
        service_on: true,
        consent: true,
        chosen: true,
        tested: true,
        bundled: true,
        held: false,
        zebrad,
        chain_mismatch: false,
        upgrades_known: true,
    };
    let idle = ZingodFacts {
        disk_allows: true,
        ..ZingodFacts::default()
    };
    // Before the run's RPC line the status has no address: not yet.
    let early = Status {
        rpc_addr: None,
        ..status.clone()
    };
    assert_eq!(
        want(
            &start(zebrad_facts(&early, true, &setup.cookie, true)),
            &idle
        ),
        Want::Stop(Blocked::NoRpc)
    );
    let run = ZebradRun::of(&status).unwrap();
    assert_eq!(
        want(
            &start(zebrad_facts(&status, true, &setup.cookie, true)),
            &idle
        ),
        Want::Run(run)
    );

    setup.give_plan(0);
    setup
        .facts_until("zingod reading zebrad", |f| running(f) && answering(f, 1))
        .await;
    setup.finish().await;
}

/// A crash is retried after the supervisor's backoff, into a crash loop; a new attempt starts
/// it again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn crashes_back_off_then_wait_for_a_new_attempt() {
    let setup = setup("ok", "crash", |_| {}).await;
    setup.give_plan(0);
    setup
        .facts_until("a backoff", |f| matches!(f.phase, Phase::Backoff { .. }))
        .await;
    setup
        .facts_until("the crash loop", |f| {
            f.phase
                == Phase::Failed {
                    class: ExitClass::Crash,
                }
        })
        .await;
    setup
        .facts_stay(
            "no restart without a new attempt",
            Duration::from_millis(500),
            |f| matches!(f.phase, Phase::Failed { .. }),
        )
        .await;
    setup.settings("FAKE_ZINGOD_MODE=serving\nFAKE_ZINGOD_POLL_MS=50\n");
    setup.give_plan(1);
    setup.facts_until("zingod running again", running).await;
    setup.finish().await;
}

/// zingod that ignores `SIGTERM` is killed after the grace.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_zingod_that_ignores_sigterm_is_killed_after_the_grace() {
    let setup = setup("ok", "ignore-sigterm", |options| {
        options.stop_grace = Duration::from_millis(500);
    })
    .await;
    setup.give_plan(0);
    setup.facts_until("zingod running", running).await;
    let asked = tokio::time::Instant::now();
    setup.plan.send_replace(None);
    let facts = setup
        .facts_until("zingod stopped", |f| f.phase == Phase::Stopped)
        .await;
    assert!(asked.elapsed() >= Duration::from_millis(500));
    let exit = facts.last_exit.unwrap();
    assert_eq!(exit.class, ExitClass::Clean);
    assert_eq!(exit.detail.as_deref(), Some("killed by the launcher"));
    setup.finish().await;
}

/// Below 10 GB free zingod stops; it starts again only above 20 GB.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_low_disk_stops_zingod_until_there_is_room() {
    let setup = setup("ok", "serving", |_| {}).await;
    setup.disk.set(30 * GB);
    setup.give_plan(0);
    setup.facts_until("zingod running", running).await;
    setup.disk.set(5 * GB);
    let facts = setup
        .facts_until("zingod stopped", |f| {
            f.phase == Phase::Stopped && !f.disk_allows
        })
        .await;
    assert!(facts.last_exit.unwrap().requested, "Zeal stopped it");
    setup.disk.set(15 * GB);
    setup
        .facts_stay("between the marks", Duration::from_millis(500), |f| {
            f.phase == Phase::Stopped
        })
        .await;
    setup.disk.set(25 * GB);
    setup.facts_until("zingod running again", running).await;
    setup.finish().await;
}

/// A stale zingod Zeal can't end (here a zombie, which no signal removes) fails the start once;
/// the runner doesn't start zingod again until the attempt changes, even once it is gone.
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stale_zingod_that_cant_be_ended_waits_for_a_new_attempt() {
    let setup = setup("ok", "serving", |options| {
        options.stop_grace = Duration::from_millis(500);
    })
    .await;
    // A child that has exited and that the test doesn't reap yet: its pid stays alive.
    let mut zombie = std::process::Command::new("true").spawn().unwrap();
    let pid = zombie.id();
    timeout(LIMIT, async {
        while !std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .is_ok_and(|stat| stat.contains(") Z "))
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the child exits");
    let pidfile = setup.dir.path().join("data/public/zingod-testnet.pid");
    std::fs::write(
        &pidfile,
        format!("{pid}\n{}\n", launcher_core_bin("fake-zingod").display()),
    )
    .unwrap();

    setup.give_plan(0);
    // Its SIGTERM, 5 s, its SIGKILL and 5 s more: within the 20 s limit.
    let facts = setup
        .facts_until("the stale zingod", |f| f.stale.is_some())
        .await;
    assert_eq!(facts.stale.unwrap().pid, pid);
    zombie.wait().unwrap();
    setup
        .facts_stay(
            "no start again for the same attempt",
            Duration::from_millis(800),
            |f| f.phase == Phase::Stopped,
        )
        .await;

    setup.give_plan(1);
    let facts = setup.facts_until("zingod running", running).await;
    assert_eq!(facts.stale, None);
    setup.finish().await;
}

/// zingod doesn't start before its index's disk is read: a disk below the low mark keeps it
/// from ever starting.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn zingod_waits_for_the_first_reading_of_its_disk() {
    let setup = setup("ok", "serving", |options| {
        let read = options.disk.clone();
        options.disk = Arc::new(move |path| {
            std::thread::sleep(Duration::from_millis(300));
            read(path)
        });
    })
    .await;
    setup.disk.set(5 * GB);
    setup.give_plan(0);
    setup
        .facts_stay("no start on a low disk", Duration::from_secs(1), |f| {
            f.phase == Phase::Stopped && f.last_exit.is_none()
        })
        .await;
    assert!(!setup.runner.facts().borrow().disk_allows);
    setup.disk.set(25 * GB);
    setup.facts_until("zingod running", running).await;
    setup.finish().await;
}

/// After `DiskFull` zingod restarts by itself only when bytes ran out and came back; a quota
/// waits for the user's Try again, a new attempt.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_full_disk_restarts_zingod_only_on_evidence() {
    let setup = setup("ok", "disk-full", |options| {
        options.disk_every = Duration::from_millis(700);
    })
    .await;
    setup.disk.set(30 * GB);
    setup.give_plan(0);
    setup.facts_until("zingod running", running).await;
    setup.disk.set(5 * GB);
    let failed = setup
        .facts_until("the full disk", |f| {
            f.phase
                == Phase::Failed {
                    class: ExitClass::DiskFull,
                }
                && f.disk_full.is_some()
        })
        .await;
    let evidence = failed.disk_full.unwrap();
    assert_eq!(
        evidence.kind,
        DiskFullKind::Unstated,
        "zingod's commit text"
    );
    assert_eq!(evidence.at_failure.unwrap().free_bytes, 5 * GB);
    setup.settings("FAKE_ZINGOD_MODE=serving\nFAKE_ZINGOD_POLL_MS=50\n");
    setup
        .facts_stay("waiting for space", Duration::from_millis(800), |f| {
            !running(f)
        })
        .await;
    setup.disk.set(25 * GB);
    setup
        .facts_until("zingod running again by itself", running)
        .await;
    setup.finish().await;

    let quota = setup_quota().await;
    quota.give_plan(0);
    let failed = quota
        .facts_until("the quota", |f| {
            f.phase
                == Phase::Failed {
                    class: ExitClass::DiskFull,
                }
                && f.disk_full.is_some()
        })
        .await;
    assert_eq!(failed.disk_full.unwrap().kind, DiskFullKind::Quota);
    quota.settings("FAKE_ZINGOD_MODE=serving\nFAKE_ZINGOD_POLL_MS=50\n");
    quota
        .facts_stay("a quota needs the user", Duration::from_millis(800), |f| {
            matches!(f.phase, Phase::Failed { .. })
        })
        .await;
    quota.give_plan(1);
    quota
        .facts_until("zingod running after Try again", running)
        .await;
    quota.finish().await;
}

async fn setup_quota() -> Setup {
    setup("ok", "disk-quota", |_| {}).await
}

/// A second `DiskFull` under the same plan waits for its own evidence: a quota after bytes ran
/// out and came back isn't restarted on the first failure's reading.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_full_disk_waits_for_its_own_evidence() {
    let setup = setup("ok", "disk-full", |options| {
        options.disk_every = Duration::from_millis(700);
    })
    .await;
    setup.disk.set(30 * GB);
    setup.give_plan(0);
    setup.facts_until("zingod running", running).await;
    setup.disk.set(5 * GB);
    setup
        .facts_until("the full disk", |f| {
            f.disk_full
                .is_some_and(|e| e.kind == DiskFullKind::Unstated)
        })
        .await;
    setup
        .settings("FAKE_ZINGOD_MODE=disk-quota\nFAKE_ZINGOD_POLL_MS=50\nFAKE_ZINGOD_FAIL_MS=300\n");
    setup.disk.set(25 * GB);
    setup
        .facts_until("the quota", |f| {
            f.phase
                == Phase::Failed {
                    class: ExitClass::DiskFull,
                }
                && f.disk_full.is_some_and(|e| e.kind == DiskFullKind::Quota)
        })
        .await;
    setup
        .facts_stay("a quota needs the user", Duration::from_millis(800), |f| {
            matches!(f.phase, Phase::Failed { .. })
        })
        .await;
    assert_eq!(
        starts(&setup).await,
        2,
        "the bytes' restart, and none on the quota"
    );
    setup.finish().await;
}

/// How many times zingod was started.
async fn starts(setup: &Setup) -> usize {
    lines_starting(setup, "zingod started (pid ").await
}

/// How many lines of zingod's log start with `prefix`.
async fn lines_starting(setup: &Setup, prefix: &str) -> usize {
    let history = timeout(LIMIT, setup.runner.subscribe())
        .await
        .unwrap()
        .expect("zingod ran")
        .history;
    history
        .iter()
        .filter(|line| line.message.starts_with(prefix))
        .count()
}

/// After `PortInUse` zingod moves to the next free port, once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_taken_port_moves_zingod_once() {
    let mut setup = setup("ok", "serving", |_| {}).await;
    // Held from the start, so no other test can take it first.
    let holder = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let taken = holder.local_addr().unwrap().port();
    setup.port = taken;
    setup.give_plan(0);
    let facts = setup
        .facts_until("zingod on another port", |f| {
            running(f) && f.port != Some(taken)
        })
        .await;
    let moved = facts.port.unwrap();
    assert!(moved > taken);
    assert_eq!(facts.ports_tried, [taken, moved]);

    // The move holds for the attempt: a new zebrad run restarts zingod on the moved port.
    timeout(LIMIT, setup.zebrad.restart())
        .await
        .unwrap()
        .unwrap();
    timeout(LIMIT, setup.zebrad.wait_until(|s| s.rpc_addr.is_some()))
        .await
        .unwrap()
        .unwrap();
    let second = setup.give_plan(0);
    let facts = setup
        .facts_until("zingod on the new run", |f| {
            running(f) && f.running_for == Some(second.zebrad)
        })
        .await;
    assert_eq!(facts.port, Some(moved));
    assert_eq!(facts.ports_tried, [taken, moved]);
    assert_eq!(
        lines_starting(&setup, "not starting zingod").await,
        1,
        "the taken port was tried once"
    );
    drop(holder);
    setup.finish().await;
}

/// A failure that waits for the user stays through zebrad's restarts: only a new attempt
/// starts zingod again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failure_waits_for_a_new_attempt_not_a_new_zebrad_run() {
    let setup = setup("ok", "index-broken", |_| {}).await;
    let broken = Phase::Failed {
        class: ExitClass::IndexBroken,
    };
    setup.give_plan(0);
    setup
        .facts_until("the damaged index", |f| f.phase == broken)
        .await;
    timeout(LIMIT, setup.zebrad.restart())
        .await
        .unwrap()
        .unwrap();
    timeout(LIMIT, setup.zebrad.wait_until(|s| s.rpc_addr.is_some()))
        .await
        .unwrap()
        .unwrap();
    setup.give_plan(0);
    setup
        .facts_stay(
            "no start on a new zebrad run",
            Duration::from_millis(800),
            |f| f.phase == broken,
        )
        .await;
    assert_eq!(starts(&setup).await, 1);

    setup.settings("FAKE_ZINGOD_MODE=serving\nFAKE_ZINGOD_POLL_MS=50\n");
    setup.give_plan(1);
    setup
        .facts_until("zingod running after Try again", running)
        .await;
    setup.finish().await;
}

/// Delete removes `indexes/` and `state/` inside the recorded folder and nothing else: a link
/// inside goes as a link. zingod stays stopped until the plan changes; Rebuild starts it again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deletion_stays_inside_the_folder() {
    let setup = setup("ok", "serving", |_| {}).await;
    setup.give_plan(0);
    setup.facts_until("zingod running", running).await;
    let outside = setup.dir.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("precious"), b"keep").unwrap();
    let indexes = setup.index_dir.join("indexes");
    std::fs::create_dir_all(indexes.join("tree_state")).unwrap();
    std::fs::write(indexes.join("tree_state/seg-1"), vec![7u8; 4096]).unwrap();
    std::os::unix::fs::symlink(&outside, indexes.join("elsewhere")).unwrap();
    let state = setup.index_dir.join("state");
    let _ = std::fs::remove_dir_all(&state);
    std::os::unix::fs::symlink(&outside, &state).unwrap();
    setup
        .facts_until("the index's size", |f| {
            f.index_bytes.is_some_and(|(b, _)| b >= 4096)
        })
        .await;

    timeout(LIMIT, setup.runner.delete())
        .await
        .unwrap()
        .expect("the index is deleted");
    assert!(!indexes.exists());
    assert!(
        std::fs::symlink_metadata(&state).is_err(),
        "the link is gone"
    );
    assert_eq!(std::fs::read(outside.join("precious")).unwrap(), b"keep");
    assert!(setup.index_dir.join("home").is_dir(), "zingod's home stays");
    setup
        .facts_stay("stopped after Delete", Duration::from_millis(500), |f| {
            f.phase == Phase::Stopped
        })
        .await;

    setup.give_plan(1);
    setup.facts_until("zingod running again", running).await;
    timeout(LIMIT, setup.runner.rebuild())
        .await
        .unwrap()
        .expect("the index is rebuilt");
    setup
        .facts_until("zingod running after Rebuild", running)
        .await;
    setup.finish().await;
}

/// fake-zingod's `GetLatestBlock` sends fake-zebrad's `getblockhash` at the served height, in
/// internal byte order; `wrong-hash` sends another. The runner also reads the process tree.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_served_tip_matches_zebrads_hash() {
    for (mode, matches) in [("serving", true), ("wrong-hash", false)] {
        let setup = setup("ok", mode, |_| {}).await;
        setup.give_plan(0);
        let facts = setup
            .facts_until("zingod serving", |f| running(f) && answering(f, 1))
            .await;
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, facts.port.unwrap()));
        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let block = grpc::get_latest_block(stream, &addr.to_string(), false)
            .await
            .expect("GetLatestBlock");
        assert_eq!(block.height, chain::TIP);
        assert_eq!(
            block.hash == chain::block_hash_bytes(block.height),
            matches,
            "{mode}"
        );
        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let info = grpc::get_lightd_info(stream, &addr.to_string(), false)
            .await
            .expect("GetLightdInfo");
        assert_eq!(
            (info.vendor.as_str(), info.chain_name.as_str()),
            ("zingod", "test")
        );
        if cfg!(target_os = "linux") {
            setup
                .facts_until("zingod's resources", |f| f.resources.is_some())
                .await;
        }
        setup.finish().await;
    }
}
