//! Contract tests against a real zingod and a real zebrad (docs/PUBLIC-SERVICES.md, "Tests for
//! zingod"), through Zeal's own pieces: zebrad on Regtest through the supervisor, zingod through
//! the `ZingodRunner` with the configuration Zeal writes, and the real front door with a test
//! certificate.
//!
//! Ignored by default, like `real_light_server.rs`. Run them by hand with:
//!
//! ```sh
//! ZEBRAD_BIN=/path/to/zebrad ZINGOD_BIN=/path/to/zingod \
//!   cargo test -p launcher-public --test real_zingod -- --include-ignored --nocapture
//! ```
//!
//! `PREV_ZINGOD_BIN`, the previous pin's zingod, also checks that the index it wrote opens with
//! this one. `ZINGOD_CAPTURE=<folder>` also writes the zingod fixtures of
//! `launcher-core/tests/fixtures` there, scrubbed as that README says.
//!
//! One test runs every case in turn: it sets `ZINGOD_CONFIG_NETWORK` in its own environment
//! first, which zingod must never see.

#![cfg(unix)]

use std::{
    collections::BTreeMap,
    net::{Ipv4Addr, SocketAddr, TcpListener},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use bytes::{Buf, Bytes, BytesMut};
use launcher_core::{
    logfile::RotatingLogConfig,
    paths::Paths,
    process::ZebradCommand,
    rpc::{RpcClient, RpcError},
    supervisor::{self, SupervisorConfig, SupervisorHandle},
    zingod::{ZingodCommand, ZingodConfig, ZingodNetwork},
};
use launcher_gateway::{Config, Event, Inputs, Limits, Mode, Outputs, Stats, Timers};
use launcher_herd::public::zingod::{ZebradRun, ZingodFacts};
use launcher_public::{
    grpc,
    zingod::{RunnerOptions, ZingodPlan, ZingodRunner, read_status},
};
use launcher_types::{ExitClass, Phase, zingod::IndexerState};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
    sync::{mpsc, watch},
    time::timeout,
};

const LIMIT: Duration = Duration::from_secs(120);
const NAME: &str = "light.zeal-test.com";
/// A Regtest address for zebrad's `generate`.
const MINER: &str = "tmSRd1r8gs77Ja67Fw1JcdoXytxsyrLTPJm";
/// Blocks behind the tip zingod finalises on Regtest, so a short chain reaches its files.
const DEPTH: u32 = 10;

fn bin(name: &str) -> PathBuf {
    let path = std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| panic!("{name} is not set: run with --include-ignored and {name}"));
    PathBuf::from(path)
}

fn free_port() -> u16 {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn loopback(port: u16) -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, port))
}

/// Whether `err` says zebrad isn't ready for a call yet, so the call is made again: its state
/// holds no block, its RPC refuses the connection, or its cookie isn't there to read. Never a
/// call that may have reached zebrad, a timeout included: `generate` made twice would mine
/// twice.
fn not_ready(err: &RpcError) -> bool {
    match err {
        RpcError::Http(err) => err.is_connect(),
        RpcError::Cookie { .. } => true,
        err => err.to_string().contains("state is empty"),
    }
}

#[ignore = "needs ZEBRAD_BIN and ZINGOD_BIN; run with -- --include-ignored"]
#[test]
fn real_zingod_contract() {
    // SAFETY: the only test in this binary, which sets this before its runtime or any other
    // thread that could read the environment starts.
    unsafe { std::env::set_var("ZINGOD_CONFIG_NETWORK", "mainnet") };
    let zebrad = bin("ZEBRAD_BIN");
    let zingod = bin("ZINGOD_BIN");
    let previous = std::env::var_os("PREV_ZINGOD_BIN")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let capture = std::env::var_os("ZINGOD_CAPTURE")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    launcher_public::install_crypto_provider();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let cases = Cases { zebrad, zingod };
        cases.serving_through_the_runner_and_the_front_door().await;
        cases.a_zebrad_restart_is_cured_by_the_restart().await;
        cases.sigterm_exits_within_the_grace().await;
        cases.failures_have_their_classes().await;
        cases.a_kill_mid_build_reopens_and_serves().await;
        if let Some(previous) = previous {
            cases.the_previous_index_opens(&previous).await;
        } else {
            eprintln!("note: PREV_ZINGOD_BIN is not set; the previous pin's index isn't checked");
        }
        if let Some(folder) = capture {
            cases.capture(&folder).await;
        }
    });
}

struct Cases {
    zebrad: PathBuf,
    zingod: PathBuf,
}

/// zebrad on Regtest through the supervisor, with its RPC and cookie.
struct Zebrad {
    handle: SupervisorHandle,
    rpc: SocketAddr,
    cookie: PathBuf,
}

impl Zebrad {
    async fn start(binary: &Path, dir: &Path) -> Self {
        let node = dir.join("node");
        std::fs::create_dir_all(&node).unwrap();
        let config = node.join("zebrad.toml");
        let quote = |path: &Path| format!("'{}'", path.display());
        std::fs::write(
            &config,
            format!(
                "[network]\nnetwork = \"Regtest\"\nlisten_addr = \"127.0.0.1:0\"\ncache_dir = {peers}\n\n\
                 [state]\ncache_dir = {state}\n\n\
                 [rpc]\nlisten_addr = \"127.0.0.1:{port}\"\ncookie_dir = {cookie}\n\n\
                 [mining]\nminer_address = \"{MINER}\"\n\n[tracing]\nuse_color = false\n",
                peers = quote(&node.join("peers")),
                state = quote(&node.join("state")),
                cookie = quote(&node.join("rpc")),
                port = free_port(),
            ),
        )
        .unwrap();
        let mut supervisor = SupervisorConfig::new(ZebradCommand::new(binary, &config));
        supervisor.stop_grace = Duration::from_secs(20);
        let handle = supervisor::spawn(supervisor).unwrap();
        timeout(LIMIT, handle.start())
            .await
            .unwrap()
            .expect("zebrad starts");
        let status = timeout(LIMIT, handle.wait_until(|s| s.rpc_addr.is_some()))
            .await
            .unwrap()
            .unwrap();
        Zebrad {
            handle,
            rpc: status.rpc_addr.unwrap(),
            cookie: node.join("rpc/.cookie"),
        }
    }

    fn client(&self) -> RpcClient {
        RpcClient::new(self.rpc, self.cookie.clone()).unwrap()
    }

    /// Mines `blocks` with `generate`, once zebrad is ready for it ([`not_ready`]). The mine
    /// may outlast the client's 10 s request timeout (5,000 blocks did on the aarch64 runner),
    /// so the call gets the whole `LIMIT`.
    async fn generate(&self, blocks: u64) {
        let hashes: Vec<String> = timeout(LIMIT, async {
            loop {
                let called = match self.client().session().await {
                    Ok(session) => {
                        session
                            .call_timeout("generate", json!([blocks]), LIMIT)
                            .await
                    }
                    Err(err) => Err(err),
                };
                match called {
                    Ok(hashes) => return hashes,
                    Err(err) if not_ready(&err) => {
                        tokio::time::sleep(Duration::from_millis(200)).await;
                    }
                    Err(err) => panic!("zebrad mines on Regtest: {err}"),
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("zebrad didn't mine {blocks} blocks in {LIMIT:?}"));
        assert_eq!(hashes.len() as u64, blocks);
    }

    async fn height(&self) -> u64 {
        let info: Value = self
            .client()
            .call("getblockchaininfo", json!([]))
            .await
            .unwrap();
        info["blocks"].as_u64().unwrap()
    }

    async fn hash(&self, height: u64) -> String {
        self.client()
            .call("getblockhash", json!([height]))
            .await
            .unwrap()
    }

    fn run(&self) -> ZebradRun {
        ZebradRun::of(&self.handle.status()).unwrap()
    }

    async fn stop(self) {
        timeout(LIMIT, self.handle.shutdown())
            .await
            .unwrap()
            .unwrap();
    }
}

/// zingod through the runner, with Zeal's Regtest configuration.
struct Zingod {
    runner: ZingodRunner,
    plan: watch::Sender<Option<ZingodPlan>>,
    config: ZingodConfig,
    options: RunnerOptions,
}

impl Zingod {
    fn new(binary: &Path, dir: &Path, zebrad: &Zebrad) -> Self {
        let config = ZingodConfig {
            network: ZingodNetwork::Regtest {
                finalised_depth: Some(DEPTH),
            },
            validator: zebrad.rpc,
            cookie: zebrad.cookie.clone(),
            listen: loopback(free_port()),
            index_dir: dir.join("node/zingod/regtest"),
        };
        let mut options = RunnerOptions::new(binary, &Paths::under(dir), config.network);
        options.status_every = Duration::from_millis(200);
        let (plan, plan_rx) = watch::channel(None);
        let runner = ZingodRunner::spawn(options.clone(), plan_rx);
        let zingod = Zingod {
            runner,
            plan,
            config,
            options,
        };
        zingod.follow(zebrad, 0);
        zingod
    }

    fn follow(&self, zebrad: &Zebrad, attempt: u64) {
        self.plan.send_replace(Some(ZingodPlan {
            config: self.config.clone(),
            zebrad: zebrad.run(),
            attempt,
        }));
    }

    fn addr(&self) -> SocketAddr {
        self.config.listen
    }

    async fn facts_until(&self, what: &str, f: impl Fn(&ZingodFacts) -> bool) -> ZingodFacts {
        let mut facts = self.runner.facts();
        timeout(LIMIT, async {
            loop {
                {
                    let now = facts.borrow_and_update();
                    if f(&now) {
                        return now.clone();
                    }
                }
                facts.changed().await.unwrap();
            }
        })
        .await
        .unwrap_or_else(|_| panic!("zingod never showed {what}: {:?}", facts.borrow()))
    }

    async fn serving_at(&self, height: u64) -> ZingodFacts {
        self.facts_until(&format!("serving at {height}"), |f| {
            f.status
                .as_ref()
                .is_some_and(|s| s.state == IndexerState::Serving && s.served == Some(height))
        })
        .await
    }

    async fn stop(self) {
        timeout(LIMIT, self.runner.shutdown()).await.unwrap();
    }
}

impl Cases {
    async fn serving_through_the_runner_and_the_front_door(&self) {
        eprintln!("== serving, on loopback and through the front door");
        let dir = tempfile::tempdir().unwrap();
        let zebrad = Zebrad::start(&self.zebrad, dir.path()).await;
        zebrad.generate(300).await;
        let zingod = Zingod::new(&self.zingod, dir.path(), &zebrad);
        let facts = zingod.serving_at(300).await;
        let status = facts.status.unwrap();
        assert_eq!(
            status.network, "regtest",
            "ZINGOD_CONFIG_NETWORK never reached zingod"
        );
        assert_eq!(status.version, launcher_types::zingod::version());
        assert_eq!(facts.phase, Phase::Running);

        // GetLightdInfo: zingod's vendor, the protocol version and zingod's name for Regtest.
        let addr = zingod.addr();
        let raw = grpc::get_lightd_info_bytes(connect(addr).await, &addr.to_string(), false)
            .await
            .expect("GetLightdInfo");
        let fields = strings(&raw);
        eprintln!("GetLightdInfo: {fields:?}");
        assert_eq!(fields.get(&2).map(String::as_str), Some("zingod"));
        assert_eq!(
            fields.get(&1).map(String::as_str),
            Some(launcher_types::zingod::version())
        );
        assert_eq!(fields.get(&4).map(String::as_str), Some("regtest"));
        assert_eq!(fields.get(&18).map(String::as_str), Some("v0.6.0"));

        // GetLatestBlock: zebrad's height and hash, in internal byte order.
        let latest = grpc::get_latest_block(connect(addr).await, &addr.to_string(), false)
            .await
            .expect("GetLatestBlock");
        assert_eq!(latest.height, zebrad.height().await);
        let mut hash = latest.hash.clone();
        hash.reverse();
        assert_eq!(hex(&hash), zebrad.hash(latest.height).await);

        // HTTP/1.1 reaches no gRPC method.
        let (code, _) = http1(
            addr,
            "POST",
            "/cash.z.wallet.sdk.rpc.CompactTxStreamer/GetLightdInfo",
        )
        .await;
        assert_eq!(code, 404);

        // A slow client gets the whole range.
        let heights = block_range(connect(addr).await, &addr.to_string(), false, 1, 300).await;
        assert_eq!(heights, (1..=300).collect::<Vec<_>>());

        // Through the front door, over TLS with the name.
        let door = FrontDoor::start(addr).await;
        let through = grpc::get_lightd_info(door.connect().await, NAME, true)
            .await
            .expect("GetLightdInfo through the front door");
        assert_eq!(through.vendor, "zingod");
        let heights = block_range(door.connect().await, NAME, true, 1, 300).await;
        assert_eq!(heights.len(), 300, "the range through the front door");
        door.stop().await;

        zingod.stop().await;
        zebrad.stop().await;
        nothing_under_home(dir.path());
    }

    async fn a_zebrad_restart_is_cured_by_the_restart(&self) {
        eprintln!("== a zebrad restart: the stale cookie, and the restart that cures it");
        let dir = tempfile::tempdir().unwrap();
        let zebrad = Zebrad::start(&self.zebrad, dir.path()).await;
        zebrad.generate(20).await;
        let zingod = Zingod::new(&self.zingod, dir.path(), &zebrad);
        zingod.serving_at(20).await;
        let answering =
            |n| move |f: &ZingodFacts| f.status.as_ref().is_some_and(|s| s.answering == Some(n));
        timeout(LIMIT, zebrad.handle.restart())
            .await
            .unwrap()
            .unwrap();
        timeout(LIMIT, zebrad.handle.wait_until(|s| s.rpc_addr.is_some()))
            .await
            .unwrap()
            .unwrap();
        zingod
            .facts_until("its polls failing on the old cookie", answering(0))
            .await;
        let asked = Instant::now();
        zingod.follow(&zebrad, 0);
        zingod
            .facts_until("zingod reading zebrad again", answering(1))
            .await;
        eprintln!("back {:?} after the restart", asked.elapsed());
        assert!(asked.elapsed() < Duration::from_secs(60));
        zingod.stop().await;
        zebrad.stop().await;
    }

    async fn sigterm_exits_within_the_grace(&self) {
        eprintln!("== SIGTERM, with and without an open connection");
        let dir = tempfile::tempdir().unwrap();
        let zebrad = Zebrad::start(&self.zebrad, dir.path()).await;
        for open in [false, true] {
            let zingod = Zingod::new(&self.zingod, dir.path(), &zebrad);
            zingod.serving_at(0).await;
            // An h2c connection whose driver never runs: it never answers zingod's GOAWAY, so
            // zingod's drain waits for it.
            let held = if open {
                Some(
                    h2::client::handshake(connect(zingod.addr()).await)
                        .await
                        .unwrap(),
                )
            } else {
                None
            };
            let asked = Instant::now();
            zingod.plan.send_replace(None);
            let facts = zingod
                .facts_until("zingod stopped", |f| f.phase == Phase::Stopped)
                .await;
            let took = asked.elapsed();
            let exit = facts.last_exit.unwrap();
            eprintln!("stopped in {took:?} with a connection open: {open}: {exit:?}");
            assert_eq!((exit.code, exit.class), (Some(0), ExitClass::Clean));
            assert!(took < zingod.options.stop_grace, "{took:?}");
            drop(held);
            zingod.stop().await;
        }
        zebrad.stop().await;
    }

    async fn failures_have_their_classes(&self) {
        eprintln!("== a held lock, a taken port, an unknown key, a damaged page");
        let dir = tempfile::tempdir().unwrap();
        let zebrad = Zebrad::start(&self.zebrad, dir.path()).await;
        zebrad.generate(300).await;
        let zingod = Zingod::new(&self.zingod, dir.path(), &zebrad);
        zingod.serving_at(300).await;

        let second = |name: &str, config: &ZingodConfig| -> PathBuf {
            let path = dir.path().join(format!("{name}.toml"));
            config.write(&path).unwrap();
            path
        };
        let mut lock = zingod.config.clone();
        lock.listen = loopback(free_port());
        let locked = self.fail(&second("lock", &lock), &lock.home_dir()).await;
        assert_eq!(locked.0, ExitClass::DbLocked, "{locked:?}");

        let mut port = zingod.config.clone();
        port.index_dir = dir.path().join("other/zingod/regtest");
        let taken = self.fail(&second("port", &port), &port.home_dir()).await;
        assert_eq!(taken.0, ExitClass::PortInUse, "{taken:?}");

        let path = second("unknown", &port);
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, format!("bogus = 1\n{text}")).unwrap();
        let refused = self.fail(&path, &port.home_dir()).await;
        assert_eq!(refused.0, ExitClass::ConfigError, "{refused:?}");
        assert!(refused.1.contains("unknown field `bogus`"), "{refused:?}");

        let config = zingod.config.clone();
        zingod.stop().await;
        let blocks = config.indexes_dir().join("compact_block/blocks.dat");
        let mut bytes = std::fs::read(&blocks).expect("compact_block's data file");
        // The file may be grown past its durable length with zeros: the last byte written is
        // in the tail page.
        let last = bytes
            .iter()
            .rposition(|byte| *byte != 0)
            .expect("compact_block holds data");
        bytes[last] ^= 0xff;
        std::fs::write(&blocks, bytes).unwrap();
        let broken = self
            .fail(&second("broken", &config), &config.home_dir())
            .await;
        assert_eq!(broken.0, ExitClass::IndexBroken, "{broken:?}");
        assert!(
            broken.1.ends_with("tail page fails its checksum"),
            "{broken:?}"
        );
        zebrad.stop().await;
    }

    /// zingod on `config` once, through a supervisor without the port check: the class and
    /// the detail it failed with.
    async fn fail(&self, config: &Path, home: &Path) -> (ExitClass, String) {
        let handle = supervisor::spawn(SupervisorConfig::new(ZingodCommand::new(
            &self.zingod,
            config,
            home,
        )))
        .unwrap();
        let _ = timeout(LIMIT, handle.start()).await.unwrap();
        let failed = timeout(
            LIMIT,
            handle.wait_until(|s| matches!(s.phase, Phase::Failed { .. })),
        )
        .await;
        let Ok(Ok(status)) = failed else {
            for line in handle.subscribe().await.unwrap().history {
                eprintln!("[{:?}] {}", line.stream, line.message);
            }
            panic!("zingod on {} never failed", config.display());
        };
        handle.shutdown().await.unwrap();
        let exit = status.last_exit.unwrap();
        eprintln!("{}: {:?}: {:?}", config.display(), exit.class, exit.detail);
        (exit.class, exit.detail.unwrap_or_default())
    }

    async fn a_kill_mid_build_reopens_and_serves(&self) {
        eprintln!("== SIGKILL during the build");
        let dir = tempfile::tempdir().unwrap();
        let zebrad = Zebrad::start(&self.zebrad, dir.path()).await;
        zebrad.generate(5_000).await;
        let zingod = Zingod::new(&self.zingod, dir.path(), &zebrad);
        zingod
            .facts_until("zingod running", |f| f.phase == Phase::Running)
            .await;
        let pid = zingod_pid(&zingod).await;
        // Read /status as often as it answers, so the kill lands during the build.
        let state = timeout(LIMIT, async {
            loop {
                if let Some(status) = read_status(zingod.addr(), Duration::from_secs(1)).await {
                    match &status.state {
                        IndexerState::Syncing { progress: Some(p) } if p.height > 0 => {
                            return status.state;
                        }
                        IndexerState::Serving => return status.state,
                        _ => {}
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        eprintln!("killed at {state:?}");
        if state == IndexerState::Serving {
            eprintln!("note: zingod built the index before the kill");
        }
        let killed = std::process::Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .status()
            .unwrap();
        assert!(killed.success());
        // A kill is a crash: the supervisor starts it again after its backoff.
        zingod.serving_at(5_000).await;
        zingod.stop().await;
        zebrad.stop().await;
    }

    async fn the_previous_index_opens(&self, previous: &Path) {
        eprintln!("== the previous pin's index opens with this zingod");
        let dir = tempfile::tempdir().unwrap();
        let zebrad = Zebrad::start(&self.zebrad, dir.path()).await;
        zebrad.generate(100).await;
        let old = Zingod::new(previous, dir.path(), &zebrad);
        old.serving_at(100).await;
        old.stop().await;
        let new = Zingod::new(&self.zingod, dir.path(), &zebrad);
        new.serving_at(100).await;
        new.stop().await;
        zebrad.stop().await;
    }

    /// Writes the zingod fixtures to `folder`, as launcher-core/tests/fixtures/README.md lists
    /// them.
    async fn capture(&self, folder: &Path) {
        eprintln!("== capturing the fixtures into {}", folder.display());
        std::fs::create_dir_all(folder).unwrap();
        let version = launcher_types::zingod::version();
        let dir = tempfile::tempdir().unwrap();
        let scrub = |text: &str, home: Option<&Path>| {
            let mut text = text.to_string();
            if let Some(home) = home {
                text = text.replace(&home.display().to_string(), "<HOME>");
            }
            text.replace(&dir.path().display().to_string(), "<DIR>")
        };
        let write = |name: &str, text: &str| {
            let path = folder.join(format!("zingod-{version}-{name}"));
            std::fs::write(&path, text).unwrap();
            eprintln!("wrote {}", path.display());
        };

        // generate-config, offline, in an empty home.
        let home = dir.path().join("gen/home");
        std::fs::create_dir_all(&home).unwrap();
        let generated = dir.path().join("gen/generate.toml");
        let command = ZingodCommand::new(&self.zingod, "unused", &home);
        let out = std::process::Command::new(&self.zingod)
            .arg("generate-config")
            .arg("-o")
            .arg(&generated)
            .env_clear()
            .envs(command.env())
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        write(
            "generate.toml",
            &scrub(&std::fs::read_to_string(&generated).unwrap(), Some(&home)),
        );

        let zebrad = Zebrad::start(&self.zebrad, dir.path()).await;
        zebrad.generate(3).await;

        // A start and a stop, from the log file.
        let zingod = Zingod::new(&self.zingod, dir.path(), &zebrad);
        zingod.serving_at(3).await;
        write(
            "status-serving.json",
            &format!("{}\n", raw_status(zingod.addr()).await),
        );
        let log = zingod.options.log.path.clone();
        // Long enough for zingod's state line after it serves.
        tokio::time::sleep(Duration::from_secs(3)).await;
        zingod.stop().await;
        let text = std::fs::read_to_string(&log).unwrap();
        let stdout: String = text
            .lines()
            .filter(|line| !line.starts_with('['))
            .map(|line| format!("{line}\n"))
            .collect();
        write("regtest-start-shutdown.stdout.log", &scrub(&stdout, None));

        // The failures' stderr, through a supervisor with a log file.
        let failure = |name: &'static str, config: ZingodConfig, edit: fn(String) -> String| {
            let dir = dir.path().to_path_buf();
            async move {
                let path = dir.join(format!("{name}.toml"));
                config.write(&path).unwrap();
                let text = std::fs::read_to_string(&path).unwrap();
                std::fs::write(&path, edit(text)).unwrap();
                let log = dir.join(format!("{name}.log"));
                let mut supervisor = SupervisorConfig::new(ZingodCommand::new(
                    &self.zingod,
                    &path,
                    config.home_dir(),
                ));
                supervisor.log_file = Some(RotatingLogConfig::new(&log));
                let handle = supervisor::spawn(supervisor).unwrap();
                let _ = timeout(LIMIT, handle.start()).await.unwrap();
                timeout(
                    LIMIT,
                    handle.wait_until(|s| matches!(s.phase, Phase::Failed { .. })),
                )
                .await
                .unwrap()
                .unwrap();
                handle.shutdown().await.unwrap();
                std::fs::read_to_string(&log)
                    .unwrap()
                    .lines()
                    .filter_map(|line| line.strip_prefix("[stderr] "))
                    .map(|line| format!("{line}\n"))
                    .collect::<String>()
            }
        };
        let keep = |text: String| text;
        let holder = Zingod::new(&self.zingod, dir.path(), &zebrad);
        holder.serving_at(3).await;
        let config = holder.config.clone();
        let mut other = config.clone();
        other.index_dir = dir.path().join("other/zingod/regtest");
        write(
            "port-in-use.stderr.log",
            &scrub(&failure("port", other.clone(), keep).await, None),
        );
        let mut lock = config.clone();
        lock.listen = loopback(free_port());
        write(
            "lock-held.stderr.log",
            &scrub(&failure("lock", lock, keep).await, None),
        );
        holder.stop().await;
        write(
            "config-error.stderr.log",
            &scrub(
                &failure("unknown", other.clone(), |text| {
                    format!("bogus = 1\n{text}")
                })
                .await,
                None,
            ),
        );
        let mut cookie = other.clone();
        cookie.cookie = dir.path().join("cookie-folder/.cookie");
        std::fs::create_dir_all(&cookie.cookie).unwrap();
        write(
            "cookie-dir.stderr.log",
            &scrub(&failure("cookie", cookie, keep).await, None),
        );

        // /status with nothing at the validator's address: Testnet syncs with no target, and
        // Regtest's boot waits for the validator.
        for (name, network) in [
            ("status-syncing-null.json", ZingodNetwork::Testnet),
            (
                "status-no-validator.json",
                ZingodNetwork::Regtest {
                    finalised_depth: None,
                },
            ),
        ] {
            let mut alone = other.clone();
            alone.network = network;
            alone.validator = loopback(free_port());
            alone.index_dir =
                dir.path()
                    .join(format!("{}/zingod/{}", network.name(), network.name()));
            let running = self.run_alone(&alone, dir.path()).await;
            tokio::time::sleep(Duration::from_secs(3)).await;
            write(name, &format!("{}\n", raw_status(alone.listen).await));
            running.shutdown().await.unwrap();
        }

        // At the tip while zebrad's mempool is off, through a proxy that answers as zebrad does
        // then.
        let proxy = mempool_off_proxy(zebrad.rpc).await;
        let mut behind = config.clone();
        behind.validator = proxy;
        behind.index_dir = dir.path().join("proxy/zingod/regtest");
        let running = self.run_alone(&behind, dir.path()).await;
        tokio::time::sleep(Duration::from_secs(5)).await;
        write(
            "status-syncing-tip.json",
            &format!("{}\n", raw_status(behind.listen).await),
        );
        running.shutdown().await.unwrap();
        zebrad.stop().await;
    }

    /// zingod on `config` through a plain supervisor, running.
    async fn run_alone(&self, config: &ZingodConfig, dir: &Path) -> SupervisorHandle {
        let path = dir.join(format!("alone-{}.toml", config.listen.port()));
        config.write(&path).unwrap();
        config.ensure_dirs().unwrap();
        let handle = supervisor::spawn(SupervisorConfig::new(ZingodCommand::new(
            &self.zingod,
            &path,
            config.home_dir(),
        )))
        .unwrap();
        timeout(LIMIT, handle.start()).await.unwrap().unwrap();
        handle
    }
}

async fn zingod_pid(zingod: &Zingod) -> u32 {
    let subscription = zingod.runner.subscribe().await.expect("zingod's log");
    subscription
        .history
        .iter()
        .rev()
        .find_map(|line| {
            line.message
                .strip_prefix("zingod started (pid ")?
                .strip_suffix(')')?
                .parse()
                .ok()
        })
        .expect("the start note")
}

/// zingod writes nothing under the home Zeal gives it.
fn nothing_under_home(dir: &Path) {
    let home = dir.join("node/zingod/regtest/home");
    let mut left = Vec::new();
    let mut stack = vec![home.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            if entry.path().is_dir() {
                stack.push(entry.path());
            } else {
                left.push(entry.path());
            }
        }
    }
    assert!(
        left.is_empty(),
        "zingod wrote under {}: {left:?}",
        home.display()
    );
}

async fn connect(addr: SocketAddr) -> TcpStream {
    timeout(LIMIT, TcpStream::connect(addr))
        .await
        .unwrap()
        .unwrap()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The string fields of a protobuf message, by number.
fn strings(mut message: &[u8]) -> BTreeMap<u64, String> {
    let mut out = BTreeMap::new();
    while !message.is_empty() {
        let key = varint(&mut message);
        match key & 7 {
            0 => {
                varint(&mut message);
            }
            2 => {
                let len = varint(&mut message) as usize;
                let (value, rest) = message.split_at(len);
                if let Ok(text) = std::str::from_utf8(value) {
                    out.insert(key >> 3, text.to_string());
                }
                message = rest;
            }
            other => panic!("wire type {other}"),
        }
    }
    out
}

fn varint(input: &mut &[u8]) -> u64 {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = input[0];
        *input = &input[1..];
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            break;
        }
    }
    value
}

/// One HTTP/1.1 request on a new connection: the status code and the body.
async fn http1(addr: SocketAddr, method: &str, path: &str) -> (u16, String) {
    let mut stream = connect(addr).await;
    stream
        .write_all(
            format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
    let mut answer = Vec::new();
    timeout(LIMIT, stream.read_to_end(&mut answer))
        .await
        .unwrap()
        .unwrap();
    let answer = String::from_utf8_lossy(&answer).to_string();
    let code = answer.split_whitespace().nth(1).unwrap().parse().unwrap();
    let body = answer
        .split_once("\r\n\r\n")
        .map_or("", |(_, b)| b)
        .to_string();
    (code, body)
}

/// `/status` as zingod sent it, checked against the parser.
async fn raw_status(addr: SocketAddr) -> String {
    let (code, body) = http1(addr, "GET", "/status").await;
    assert_eq!(code, 200, "{body}");
    assert!(
        read_status(addr, Duration::from_secs(2)).await.is_some(),
        "{body}"
    );
    body.trim().to_string()
}

/// `GetBlockRange(start..=end)` read slowly, a pause between frames: the heights it streamed.
async fn block_range<S>(io: S, authority: &str, tls: bool, start: u64, end: u64) -> Vec<u64>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let block_id = |height: u64| {
        let mut out = vec![0x08];
        put_varint(&mut out, height);
        out
    };
    let mut range = Vec::new();
    for (field, height) in [(1u8, start), (2, end)] {
        let id = block_id(height);
        range.push(field << 3 | 2);
        put_varint(&mut range, id.len() as u64);
        range.extend_from_slice(&id);
    }
    let (client, connection) = h2::client::handshake(io).await.unwrap();
    let driver = tokio::spawn(connection);
    let scheme = if tls { "https" } else { "http" };
    let request = http::Request::post(format!(
        "{scheme}://{authority}/cash.z.wallet.sdk.rpc.CompactTxStreamer/GetBlockRange"
    ))
    .header("content-type", "application/grpc")
    .header("te", "trailers")
    .body(())
    .unwrap();
    let mut client = client.ready().await.unwrap();
    let (response, mut body) = client.send_request(request, false).unwrap();
    let mut frame = BytesMut::new();
    frame.extend_from_slice(&[0]);
    frame.extend_from_slice(&(range.len() as u32).to_be_bytes());
    frame.extend_from_slice(&range);
    body.send_data(frame.freeze(), true).unwrap();
    let response = timeout(LIMIT, response).await.unwrap().unwrap();
    assert_eq!(response.status(), 200);
    let mut stream = response.into_body();
    let mut data = BytesMut::new();
    let mut heights = Vec::new();
    while let Some(chunk) = timeout(LIMIT, stream.data()).await.unwrap() {
        let chunk: Bytes = chunk.unwrap();
        tokio::time::sleep(Duration::from_millis(5)).await;
        let _ = stream.flow_control().release_capacity(chunk.len());
        data.extend_from_slice(&chunk);
        while data.len() >= 5 {
            let len = u32::from_be_bytes([data[1], data[2], data[3], data[4]]) as usize;
            if data.len() < 5 + len {
                break;
            }
            data.advance(5);
            let message = data.split_to(len);
            heights.push(compact_block_height(&message));
        }
    }
    let trailers = stream.trailers().await.unwrap().unwrap_or_default();
    assert_eq!(
        trailers.get("grpc-status").and_then(|v| v.to_str().ok()),
        Some("0"),
        "{trailers:?}"
    );
    driver.abort();
    heights
}

/// `CompactBlock.height`, field 2.
fn compact_block_height(mut message: &[u8]) -> u64 {
    while !message.is_empty() {
        let key = varint(&mut message);
        match key & 7 {
            0 if key >> 3 == 2 => return varint(&mut message),
            0 => {
                varint(&mut message);
            }
            2 => {
                let len = varint(&mut message) as usize;
                message = &message[len..];
            }
            5 => message = &message[4..],
            1 => message = &message[8..],
            other => panic!("wire type {other}"),
        }
    }
    0
}

fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

/// The front door in `LocalOnly` with a test certificate, in front of `upstream`.
struct FrontDoor {
    addr: SocketAddr,
    trust: CertificateDer<'static>,
    commands: mpsc::Sender<launcher_gateway::Command>,
    _cert: watch::Sender<Option<Arc<rustls::sign::CertifiedKey>>>,
    _challenge: watch::Sender<Option<Arc<launcher_gateway::Challenge>>>,
    _mode: watch::Sender<Mode>,
    _stats: watch::Receiver<Stats>,
}

impl FrontDoor {
    async fn start(upstream: SocketAddr) -> Self {
        let generated = rcgen::generate_simple_self_signed(vec![NAME.to_string()]).unwrap();
        let trust = generated.cert.der().clone();
        let key = launcher_gateway::certified_key(
            vec![trust.clone()],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
                generated.signing_key.serialize_der(),
            )),
        )
        .unwrap();
        let (cert_tx, cert) = watch::channel(Some(key));
        let (challenge_tx, challenge) = watch::channel(None);
        let (mode_tx, mode) = watch::channel(Mode::LocalOnly);
        let (commands, commands_rx) = mpsc::channel(4);
        let (events_tx, mut events) = mpsc::channel(16);
        let (stats, stats_rx) = watch::channel(Stats::default());
        tokio::spawn(launcher_gateway::serve(
            Config {
                listen: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
                upstream,
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
        let Some(Event::Ready { addr }) = events.recv().await else {
            panic!("the front door did not start");
        };
        FrontDoor {
            addr,
            trust,
            commands,
            _cert: cert_tx,
            _challenge: challenge_tx,
            _mode: mode_tx,
            _stats: stats_rx,
        }
    }

    async fn connect(&self) -> tokio_rustls::client::TlsStream<TcpStream> {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(self.trust.clone()).unwrap();
        let mut config = rustls::ClientConfig::builder_with_provider(launcher_gateway::provider())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        config.alpn_protocols = vec![b"h2".to_vec()];
        tokio_rustls::TlsConnector::from(Arc::new(config))
            .connect(
                ServerName::try_from(NAME).unwrap(),
                connect(self.addr).await,
            )
            .await
            .unwrap()
    }

    async fn stop(self) {
        let _ = self.commands.send(launcher_gateway::Command::Stop).await;
    }
}

/// A loopback proxy in front of zebrad's RPC that answers `getrawmempool`, alone or in a
/// batch, with the error zebrad gives while its mempool is off. One request a connection.
async fn mempool_off_proxy(upstream: SocketAddr) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let _ = proxy_one(stream, upstream).await;
            });
        }
    });
    addr
}

/// Reads one HTTP/1.1 message: its head and its `Content-Length` body.
async fn read_message(stream: &mut TcpStream) -> std::io::Result<(String, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte).await? == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        buf.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&buf).to_string();
    let length = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    let mut body = vec![0; length];
    stream.read_exact(&mut body).await?;
    Ok((head, body))
}

async fn proxy_one(mut client: TcpStream, upstream: SocketAddr) -> std::io::Result<()> {
    let (head, body) = read_message(&mut client).await?;
    let auth = head
        .lines()
        .find(|line| line.to_ascii_lowercase().starts_with("authorization:"))
        .map(|line| format!("{line}\r\n"))
        .unwrap_or_default();
    let mut server = TcpStream::connect(upstream).await?;
    server
        .write_all(
            format!(
                "POST / HTTP/1.1\r\nHost: {upstream}\r\nContent-Type: application/json\r\n{auth}Content-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .as_bytes(),
        )
        .await?;
    server.write_all(&body).await?;
    let (answer_head, answer) = read_message(&mut server).await?;
    let code = answer_head
        .split_whitespace()
        .nth(1)
        .unwrap_or("502")
        .to_string();
    let off = json!({ "code": -1, "message": "mempool is not active: wait for Zebra to sync to the tip" });
    let patched = match (
        serde_json::from_slice::<Value>(&body),
        serde_json::from_slice::<Value>(&answer),
    ) {
        (Ok(request), Ok(mut answer)) => {
            let requests = request
                .as_array()
                .cloned()
                .unwrap_or_else(|| vec![request.clone()]);
            let mempool: Vec<Value> = requests
                .iter()
                .filter(|r| r["method"] == "getrawmempool")
                .map(|r| r["id"].clone())
                .collect();
            let patch = |item: &mut Value| {
                if mempool.contains(&item["id"]) {
                    item.as_object_mut().map(|o| o.remove("result"));
                    item["error"] = off.clone();
                }
            };
            match answer.as_array_mut() {
                Some(items) => items.iter_mut().for_each(patch),
                None => patch(&mut answer),
            }
            serde_json::to_vec(&answer).unwrap()
        }
        _ => answer,
    };
    client
        .write_all(
            format!(
                "HTTP/1.1 {code} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                patched.len()
            )
            .as_bytes(),
        )
        .await?;
    client.write_all(&patched).await?;
    client.shutdown().await
}
