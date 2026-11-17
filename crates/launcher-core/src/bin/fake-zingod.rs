//! A scripted stand-in for `zingod`, used by the zingod tests and installed as the development
//! sidecar `zeal-zingod` on Linux by `scripts/dev-sidecar.sh`. Release bundles never ship it.
//!
//! | command | behaviour |
//! |---|---|
//! | `--version`, `-V` | prints `zingod <ZINGOD_VERSION>`, the pin, and exits 0 |
//! | `generate-config -o FILE` (or `--output FILE`) | writes a small TOML to `FILE` and exits 0 |
//! | `-c FILE start` (or `start -c FILE`) | runs as below |
//!
//! Anything else exits 2, as zingod does for a command it doesn't know.
//!
//! `start` reads the file as zingod 1.1.0 does: a key that isn't in the `generate-config`
//! output captured from it (or one of the keys it doesn't print) fails with zingod's `unknown
//! field` text and exit 1. A `cookie_path` that exists but can't be read fails with zingod's
//! `cannot read validator cookie` text. It binds `grpc_listen_address` (a taken one fails with
//! zingod's `binding the gRPC listener` text), prints zingod's logfmt lines (`Starting`,
//! `Listening … component=Grpc`, `State changed`, `Shutdown complete`), locks
//! `<compact_block's folder>/../utxo_set/LOCK` (a held lock fails with zingod's text), and
//! stops on `SIGTERM` with exit 0. On its port it answers HTTP/1.1 `GET /status` and
//! `/readyz` with zingod's captured JSON (tests/fixtures), adjusted to the mode, 404 for any
//! other HTTP/1.1 request, and over h2c `GetLightdInfo` and `GetLatestBlock`, whose hash is
//! fake-zebrad's `getblockhash` at the served height (`fake/chain.rs`).
//!
//! Like zingod 1.1.0 it polls its validator's `getblockchaininfo` every second with the cookie,
//! which it reads at the first poll and again only after a `401`: against a zebrad that closes
//! the connection on a wrong cookie, it fails for the rest of its run once zebrad restarts.
//!
//! Zeal clears zingod's environment, so the settings below come from the environment or,
//! failing that, from `fake-zingod.env` beside the `-c` file, one `NAME=VALUE` a line.
//! `FAKE_ZINGOD_MODE` selects a scenario:
//!
//! | mode | behaviour |
//! |---|---|
//! | `serving` (default) | `provisioning`, then `serving` at the validator's tip once it answers |
//! | `syncing-then-serving` | `syncing` with a detail for `FAKE_ZINGOD_SYNC_MS` (2000), then `serving` |
//! | `syncing-null-detail` | stays `syncing` with a `null` detail |
//! | `mempool-off` | stays `syncing` at the tip, as zingod does while zebrad's mempool is off |
//! | `stuck` | stays `syncing`, served 100 blocks below the validator's tip |
//! | `wrong-hash` | like `serving`, but `GetLatestBlock` sends another hash |
//! | `no-validator` | never polls: `serving` with no validator answering |
//! | `disk-full` | serves, then after `FAKE_ZINGOD_FAIL_MS` (500) panics with zingod's commit text and aborts |
//! | `disk-quota` | the same with `segment io: Disk quota exceeded (os error 122)` on stderr, exit 1 |
//! | `index-broken` | fails at open with `MANIFEST checksum mismatch`, exit 1 |
//! | `permission-denied` | fails at open with `manifest io: Permission denied (os error 13)`, exit 1 |
//! | `crash` | after `FAKE_ZINGOD_FAIL_MS`, exits 1 with zingod's `task ended before shutdown` |
//! | `abort` | after `FAKE_ZINGOD_FAIL_MS`, panics and aborts |
//! | `ignore-sigterm` | logs a `SIGTERM` and carries on, so the supervisor must kill it |
//! | `abort-on-sigterm` | like `serving`, but on `SIGTERM` panics with header sync's assertion and aborts, as zingod 1.1.0 can (`logs::SIGTERM_RACE`) |
//! | `print-env` | prints `env NAME=VALUE` for each variable it got, then runs like `serving` |
//! | `txid` | like `serving`, and logs zingod's `Transaction not accepted` with a `txid` every `FAKE_ZINGOD_HEARTBEAT_MS` (200) |
//!
//! `FAKE_ZINGOD_POLL_MS` (1000) sets the poll interval and `FAKE_ZINGOD_BOOT_MS` (200) how
//! long it stays `provisioning`.

use std::{
    collections::BTreeMap,
    io::Write as _,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::ExitCode,
    time::{Duration, Instant, SystemTime},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use bytes::Bytes;
use launcher_core::clock::rfc3339;
use launcher_types::Network;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    sync::watch,
};

#[path = "fake/chain.rs"]
mod chain;

/// What `generate-config` writes: two of zingod's top-level keys, at zingod's defaults.
const CONFIG: &str = "\
# Written by fake-zingod, a stand-in for zingod in Zeal's development builds.
network = \"mainnet\"
validator_client = \"legacy_rpc\"
";

/// zingod 1.1.0's `generate-config` output: the keys it accepts.
const GENERATED: &str = include_str!("../../tests/fixtures/zingod-1.1.0-generate.toml");

/// zingod 1.1.0's top-level keys, in its order, for the `unknown field` text.
const TOP_LEVEL: [&str; 11] = [
    "network",
    "validator_client",
    "metrics",
    "trusted_validators",
    "serve",
    "grpc",
    "submission",
    "p2p",
    "sync",
    "index",
    "snapshot",
];

/// `/status` answers captured from zingod 1.1.0, which the fake adjusts.
const SERVING: &str = include_str!("../../tests/fixtures/zingod-1.1.0-status-serving.json");
const SYNCING_NULL: &str =
    include_str!("../../tests/fixtures/zingod-1.1.0-status-syncing-null.json");

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["--version" | "-V"] => {
            println!("zingod {}", launcher_types::zingod::version());
            ExitCode::SUCCESS
        }
        ["generate-config", "-o" | "--output", path] => match std::fs::write(path, CONFIG) {
            Ok(()) => {
                println!("Generated config file: {path}");
                ExitCode::SUCCESS
            }
            Err(err) => {
                eprintln!("fake-zingod: cannot write {path}: {err}");
                ExitCode::FAILURE
            }
        },
        ["-c" | "--config", path, "start"] | ["start", "-c" | "--config", path] => {
            start(Path::new(path))
        }
        _ => {
            eprintln!("fake-zingod: expected --version, generate-config -o FILE or -c FILE start");
            ExitCode::from(2)
        }
    }
}

/// The fake's settings: the environment first, then `fake-zingod.env` beside the config.
struct Settings {
    file: BTreeMap<String, String>,
}

impl Settings {
    fn read(config: &Path) -> Self {
        let path = config
            .parent()
            .unwrap_or(Path::new("."))
            .join("fake-zingod.env");
        let file = std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .filter(|line| !line.trim_start().starts_with('#'))
            .filter_map(|line| line.split_once('='))
            .map(|(name, value)| (name.trim().to_string(), value.trim().to_string()))
            .collect();
        Settings { file }
    }

    fn get(&self, name: &str) -> Option<String> {
        std::env::var(name)
            .ok()
            .or_else(|| self.file.get(name).cloned())
    }

    fn mode(&self) -> String {
        self.get("FAKE_ZINGOD_MODE")
            .unwrap_or_else(|| "serving".to_string())
    }

    fn millis(&self, name: &str, default: u64) -> Duration {
        Duration::from_millis(
            self.get(name)
                .and_then(|v| v.parse().ok())
                .unwrap_or(default),
        )
    }
}

/// What the fake read from its `-c` file.
struct Config {
    network: String,
    validator: String,
    cookie: Option<PathBuf>,
    listen: SocketAddr,
    compact_block: PathBuf,
}

fn start(path: &Path) -> ExitCode {
    let settings = Settings::read(path);
    let mode = settings.mode();
    if mode == "print-env" {
        for (name, value) in std::env::vars_os() {
            println!("env {}={}", name.to_string_lossy(), value.to_string_lossy());
        }
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime can always be built");
    runtime.block_on(run(path, settings, mode))
}

/// Prints one logfmt line as zingod's logger does, with `component` last.
fn log(level: &str, message: &str, fields: &[(&str, &str)], component: Option<&str>) {
    let quote = |value: &str| {
        let needs = value.is_empty()
            || value
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '=' | '"' | '\\'));
        if needs {
            format!("\"{}\"", value.escape_debug())
        } else {
            value.to_string()
        }
    };
    let mut line = format!(
        "ts={} level={level} message={}",
        rfc3339(SystemTime::now()),
        quote(message)
    );
    for (key, value) in fields {
        line.push_str(&format!(" {key}={}", quote(value)));
    }
    if let Some(component) = component {
        line.push_str(&format!(" component={component}"));
    }
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

/// zingod's start failure: the `Startup failed` line, `Error: <text>` on stderr, exit 1.
fn startup_failed(text: &str) -> ExitCode {
    log(
        "error",
        "Startup failed",
        &[("error", text)],
        Some("Zingod"),
    );
    eprintln!("Error: {text}");
    ExitCode::FAILURE
}

/// Where zingod 1.1.0's index commit panics.
const COMMIT_PANIC: &str = "packages/zingod-persistence/src/error.rs:41:9";

/// Rust's report of a panic at `at` on stderr, then an abort: zingod's panic hook.
fn panic_and_abort(at: &str, message: &str) -> ! {
    eprintln!(
        "\nthread 'tokio-rt-worker' ({}) panicked at {at}:",
        std::process::id()
    );
    eprintln!("{message}");
    eprintln!("note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace");
    std::process::abort()
}

/// The dotted keys zingod 1.1.0 accepts, with the keys of each table for the error text.
fn known_keys() -> Vec<String> {
    fn walk(prefix: &str, item: &toml_edit::Item, out: &mut Vec<String>) {
        let join = |key: &str| {
            if prefix.is_empty() {
                key.to_string()
            } else {
                format!("{prefix}.{key}")
            }
        };
        match item {
            toml_edit::Item::Table(table) => {
                for (key, item) in table.iter() {
                    out.push(join(key));
                    walk(&join(key), item, out);
                }
            }
            toml_edit::Item::ArrayOfTables(tables) => {
                for table in tables.iter() {
                    for (key, item) in table.iter() {
                        out.push(join(key));
                        walk(&join(key), item, out);
                    }
                }
            }
            _ => {}
        }
    }
    let doc: toml_edit::DocumentMut = GENERATED.parse().expect("the captured file is TOML");
    let mut out = Vec::new();
    walk("", doc.as_item(), &mut out);
    out.extend(
        launcher_core::zingod::config::UNPRINTED_KEYS
            .iter()
            .map(|key| key.to_string()),
    );
    out.push("snapshot".to_string());
    out
}

/// The first key of `doc` zingod would refuse, as its text.
fn unknown_key(doc: &toml_edit::DocumentMut) -> Option<String> {
    let known = known_keys();
    fn walk(
        prefix: &str,
        table: &dyn toml_edit::TableLike,
        known: &[String],
    ) -> Option<(String, String)> {
        for (key, item) in table.iter() {
            let path = if prefix.is_empty() {
                key.to_string()
            } else {
                format!("{prefix}.{key}")
            };
            if !known.contains(&path) {
                return Some((prefix.to_string(), key.to_string()));
            }
            if let Some(inner) = item.as_table_like() {
                if let Some(found) = walk(&path, inner, known) {
                    return Some(found);
                }
            } else if let Some(tables) = item.as_array_of_tables() {
                for inner in tables.iter() {
                    if let Some(found) = walk(&path, inner, known) {
                        return Some(found);
                    }
                }
            }
        }
        None
    }
    let (table, key) = walk("", doc.as_table(), &known)?;
    let expected: Vec<String> = if table.is_empty() {
        TOP_LEVEL.iter().map(|key| format!("`{key}`")).collect()
    } else {
        known
            .iter()
            .filter_map(|path| path.strip_prefix(&format!("{table}.")))
            .filter(|rest| !rest.contains('.'))
            .map(|key| format!("`{key}`"))
            .collect()
    };
    let at = if table.is_empty() {
        String::new()
    } else {
        format!(" for key `{table}`")
    };
    Some(format!(
        "configuration error: parsing configuration: unknown field `{key}`, expected one of {}{at}",
        expected.join(", ")
    ))
}

fn load(path: &Path) -> Result<Config, String> {
    let text = std::fs::read_to_string(path).map_err(|_| {
        format!(
            "configuration error: loading configuration: configuration file \"{}\" not found",
            path.display()
        )
    })?;
    let doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|err| format!("configuration error: loading configuration: {err}"))?;
    if let Some(unknown) = unknown_key(&doc) {
        return Err(unknown);
    }
    let validator = doc
        .get("trusted_validators")
        .and_then(|item| item.as_array_of_tables())
        .and_then(|tables| tables.get(0));
    let listen = doc
        .get("serve")
        .and_then(|serve| serve.get("grpc_listen_address"))
        .and_then(|item| item.as_str())
        .unwrap_or("127.0.0.1:8137");
    Ok(Config {
        network: doc
            .get("network")
            .and_then(|item| item.as_str())
            .unwrap_or("mainnet")
            .to_string(),
        validator: validator
            .and_then(|table| table.get("jsonrpc_address"))
            .and_then(|item| item.as_str())
            .unwrap_or("127.0.0.1:8232")
            .to_string(),
        cookie: validator
            .and_then(|table| table.get("cookie_path"))
            .and_then(|item| item.as_str())
            .map(PathBuf::from),
        listen: listen
            .parse()
            .map_err(|err| format!("configuration error: parsing configuration: {err}"))?,
        compact_block: doc
            .get("index")
            .and_then(|index| index.get("compact_block"))
            .and_then(|table| table.get("path"))
            .and_then(|item| item.as_str())
            .map_or_else(|| PathBuf::from("indexes/compact_block"), PathBuf::from),
    })
}

/// What the fake knows of its validator, shared through a `watch`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Validator {
    /// The last poll got an answer.
    answering: bool,
    /// The tip of the last answer.
    tip: Option<u64>,
}

/// What the HTTP and h2c answers are made from.
#[derive(Clone)]
struct Serve {
    mode: String,
    network: String,
    started: Instant,
    boot: Duration,
    sync: Duration,
    validator: watch::Receiver<Validator>,
}

impl Serve {
    /// The height the fake serves, if any.
    fn served(&self) -> Option<u64> {
        let tip = self.validator.borrow().tip?;
        Some(match self.mode.as_str() {
            "stuck" => tip.saturating_sub(100),
            "syncing-then-serving" if self.started.elapsed() < self.boot + self.sync => {
                let left = (self.boot + self.sync).saturating_sub(self.started.elapsed());
                tip.saturating_sub(
                    5_000 * left.as_millis() as u64 / self.sync.as_millis().max(1) as u64,
                )
            }
            _ => tip,
        })
    }

    /// `/status`, from the captured answers.
    fn status(&self) -> Value {
        let validator = *self.validator.borrow();
        let mut status: Value = if self.started.elapsed() < self.boot {
            json!({ "state": "provisioning", "detail": null,
                "chain": { "network": "", "tip": null, "served": null, "validators": null, "alarms": [] } })
        } else if validator.tip.is_none() || self.mode == "syncing-null-detail" {
            serde_json::from_str(SYNCING_NULL).expect("the captured answer")
        } else {
            let tip = validator.tip.unwrap_or_default();
            let served = self.served().unwrap_or_default();
            let mut status: Value = serde_json::from_str(SERVING).expect("the captured answer");
            status["chain"]["tip"] = json!({ "height": tip, "hash": chain::block_hash(tip) });
            status["chain"]["served"] = json!(served);
            status["chain"]["alarms"] = json!([]);
            let syncing = match self.mode.as_str() {
                "mempool-off" => {
                    Some(json!({ "height": served, "target": tip, "blocks_per_sec": null }))
                }
                "stuck" => Some(json!({ "height": served, "target": tip, "blocks_per_sec": 0.0 })),
                "syncing-then-serving" if served < tip => {
                    Some(json!({ "height": served, "target": tip, "blocks_per_sec": 2_500.0 }))
                }
                _ => None,
            };
            if let Some(detail) = syncing {
                status["state"] = json!("syncing");
                status["detail"] = detail;
            }
            status
        };
        status["chain"]["network"] = json!(self.network);
        if !status["chain"]["validators"].is_null() || self.started.elapsed() >= self.boot {
            status["chain"]["validators"] =
                json!({ "trusted": 1, "answering": u32::from(validator.answering) });
        }
        status["build"] = json!({ "version": launcher_types::zingod::version() });
        status
    }

    fn lightd_info(&self) -> Option<Vec<u8>> {
        let served = self.served()?;
        let tip = self.validator.borrow().tip.unwrap_or(served);
        let network = if self.network == "mainnet" {
            Network::Mainnet
        } else {
            Network::Testnet
        };
        let chain_name = match self.network.as_str() {
            "regtest" => "regtest",
            _ => launcher_herd::public::wallet::chain_name(network),
        };
        let mut out = Vec::new();
        let string = |out: &mut Vec<u8>, number: u64, value: &str| {
            put_varint(out, number << 3 | 2);
            put_varint(out, value.len() as u64);
            out.extend_from_slice(value.as_bytes());
        };
        let varint = |out: &mut Vec<u8>, number: u64, value: u64| {
            put_varint(out, number << 3);
            put_varint(out, value);
        };
        string(&mut out, 1, launcher_types::zingod::version());
        string(&mut out, 2, "zingod");
        varint(&mut out, 3, 1);
        string(&mut out, 4, chain_name);
        varint(
            &mut out,
            5,
            launcher_herd::public::wallet::sapling_activation_height(network),
        );
        string(
            &mut out,
            6,
            &launcher_herd::public::wallet::branch_id_at(network, served)
                .unwrap_or_else(|| "00000000".into()),
        );
        varint(&mut out, 7, served);
        varint(&mut out, 12, tip);
        string(&mut out, 18, "v0.6.0");
        Some(out)
    }

    fn latest_block(&self) -> Option<Vec<u8>> {
        let served = self.served()?;
        let hash = if self.mode == "wrong-hash" {
            chain::block_hash_bytes(served ^ 1)
        } else {
            chain::block_hash_bytes(served)
        };
        let mut out = Vec::new();
        put_varint(&mut out, 1 << 3);
        put_varint(&mut out, served);
        put_varint(&mut out, 2 << 3 | 2);
        put_varint(&mut out, hash.len() as u64);
        out.extend_from_slice(&hash);
        Some(out)
    }
}

fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

async fn run(path: &Path, settings: Settings, mode: String) -> ExitCode {
    let version = launcher_types::zingod::version();
    let mut shutdown = shutdown_signal();
    log("info", "Starting", &[("version", version)], Some("Zingod"));
    let config = match load(path) {
        Ok(config) => config,
        Err(text) => {
            eprintln!("Error: {text}");
            return ExitCode::FAILURE;
        }
    };
    log(
        "info",
        "Config loaded",
        &[("path", &path.display().to_string())],
        Some("Zingod"),
    );
    log(
        "warn",
        "In development, not for production mainnet use",
        &[],
        Some("Zingod"),
    );
    log(
        "info",
        "Open-file limit",
        &[("limit", "1048576")],
        Some("Zingod"),
    );
    if let Some(cookie) = &config.cookie
        && let Err(err) = std::fs::read(cookie)
        && err.kind() != std::io::ErrorKind::NotFound
    {
        return startup_failed(&format!(
            "cannot read validator cookie {}: {err}",
            cookie.display()
        ));
    }
    let listener = match TcpListener::bind(config.listen).await {
        Ok(listener) => listener,
        Err(err) => return startup_failed(&format!("binding the gRPC listener: {err}")),
    };
    let endpoint = listener
        .local_addr()
        .map_or_else(|_| config.listen.to_string(), |addr| addr.to_string());
    log(
        "info",
        "Listening",
        &[
            ("endpoint", &endpoint),
            ("network", &config.network),
            ("tls", "off"),
        ],
        Some("Grpc"),
    );
    log(
        "info",
        "State changed",
        &[("to", "provisioning")],
        Some("Zingod"),
    );
    let utxo_set = config
        .compact_block
        .parent()
        .unwrap_or(Path::new("."))
        .join("utxo_set");
    let _lock = match lock(&utxo_set) {
        Ok(lock) => lock,
        Err(text) => return startup_failed(&text),
    };
    match mode.as_str() {
        "index-broken" => return startup_failed("MANIFEST checksum mismatch"),
        "permission-denied" => {
            return startup_failed("manifest io: Permission denied (os error 13)");
        }
        _ => {}
    }

    let (validator_tx, validator_rx) = watch::channel(Validator::default());
    if mode != "no-validator" {
        tokio::spawn(poll_validator(
            config.validator.clone(),
            config.cookie.clone(),
            settings.millis("FAKE_ZINGOD_POLL_MS", 1_000),
            validator_tx,
        ));
    }
    let serve = Serve {
        mode: mode.clone(),
        network: config.network.clone(),
        started: Instant::now(),
        boot: settings.millis("FAKE_ZINGOD_BOOT_MS", 200),
        sync: settings.millis("FAKE_ZINGOD_SYNC_MS", 2_000),
        validator: validator_rx,
    };
    tokio::spawn(accept(listener, serve.clone()));
    tokio::spawn(log_states(serve.clone()));

    let fail_after = settings.millis("FAKE_ZINGOD_FAIL_MS", 500);
    let heartbeat = settings.millis("FAKE_ZINGOD_HEARTBEAT_MS", 200);
    let mut ticker = tokio::time::interval(heartbeat);
    let failing = tokio::time::sleep(fail_after);
    tokio::pin!(failing);
    loop {
        tokio::select! {
            () = &mut shutdown => {
                log("info", "Shutdown signal received", &[("signal", "SIGTERM")], None);
                if mode == "ignore-sigterm" {
                    log("warn", "fake-zingod ignores the signal", &[], Some("FakeZingod"));
                    shutdown = Box::pin(std::future::pending());
                    continue;
                }
                if mode == "abort-on-sigterm" {
                    panic_and_abort(
                        "packages/zingod-sync/src/follower.rs:143:17",
                        launcher_core::zingod::logs::SIGTERM_RACE,
                    );
                }
                log("info", "Shutdown complete", &[], Some("Zingod"));
                return ExitCode::SUCCESS;
            }
            () = &mut failing => {
                let index = config.compact_block.display().to_string();
                match mode.as_str() {
                    "disk-full" => panic_and_abort(
                        COMMIT_PANIC,
                        &format!("compact_block index commit failed: disk {index} full"),
                    ),
                    "disk-quota" => {
                        log("error", "Stopped with error", &[("error", "segment io: Disk quota exceeded (os error 122)")], Some("Zingod"));
                        eprintln!("Error: segment io: Disk quota exceeded (os error 122)");
                        return ExitCode::FAILURE;
                    }
                    "crash" => {
                        log("error", "Stopped with error", &[("error", "sync task ended before shutdown")], Some("Zingod"));
                        eprintln!("Error: sync task ended before shutdown");
                        return ExitCode::FAILURE;
                    }
                    "abort" => panic_and_abort(COMMIT_PANIC, "fake-zingod: a violated invariant"),
                    _ => failing.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(86_400)),
                }
            }
            _ = ticker.tick(), if mode == "txid" => {
                log(
                    "warn",
                    "Transaction not accepted",
                    &[
                        ("txid", &chain::block_hash(7)),
                        ("attempts", "4"),
                        ("verdict", "rejected"),
                    ],
                    Some("Submit"),
                );
            }
        }
    }
}

/// Locks `dir/LOCK` as zingod's stores do: one process at a time.
#[cfg(unix)]
fn lock(dir: &Path) -> Result<nix::fcntl::Flock<std::fs::File>, String> {
    let path = dir.join("LOCK");
    let io = |err: std::io::Error| format!("manifest io: {err}");
    std::fs::create_dir_all(dir).map_err(io)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(io)?;
    nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusiveNonblock).map_err(
        |(_, err)| {
            if err == nix::errno::Errno::EWOULDBLOCK {
                format!("manifest io: {} is held by another process", path.display())
            } else {
                format!("manifest io: {err}")
            }
        },
    )
}

/// zingod doesn't run on Windows; the fake opens `dir/LOCK` there but takes no lock.
#[cfg(not(unix))]
fn lock(dir: &Path) -> Result<std::fs::File, String> {
    let io = |err: std::io::Error| format!("manifest io: {err}");
    std::fs::create_dir_all(dir).map_err(io)?;
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("LOCK"))
        .map_err(io)
}

/// Logs zingod's `State changed` lines as `/status` moves.
async fn log_states(serve: Serve) {
    let mut last = "provisioning".to_string();
    loop {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let state = serve.status()["state"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if state != last {
            log(
                "info",
                "State changed",
                &[("from", &last), ("to", &state)],
                Some("Zingod"),
            );
            last = state;
        }
    }
}

/// Polls the validator's `getblockchaininfo` as zingod does, reading the cookie at the first
/// poll and again only after a 401.
async fn poll_validator(
    address: String,
    cookie: Option<PathBuf>,
    every: Duration,
    out: watch::Sender<Validator>,
) {
    let mut credentials: Option<String> = None;
    loop {
        if credentials.is_none()
            && let Some(cookie) = &cookie
        {
            credentials = std::fs::read_to_string(cookie)
                .ok()
                .map(|text| text.trim().to_string())
                .filter(|text| !text.is_empty());
        }
        let answer = tokio::time::timeout(
            Duration::from_secs(2),
            blockchain_info(&address, credentials.as_deref()),
        )
        .await
        .unwrap_or(Err(Poll::NoAnswer));
        let validator = match answer {
            Ok(tip) => Validator {
                answering: true,
                tip: Some(tip),
            },
            Err(Poll::Refused) => {
                credentials = None;
                Validator {
                    answering: false,
                    tip: out.borrow().tip,
                }
            }
            Err(Poll::NoAnswer) => Validator {
                answering: false,
                tip: out.borrow().tip,
            },
        };
        if !validator.answering {
            log(
                "warn",
                "Validator poll failed",
                &[("validator", &address)],
                Some("Traffic"),
            );
        }
        out.send_if_modified(|current| std::mem::replace(current, validator) != validator);
        tokio::time::sleep(every).await;
    }
}

enum Poll {
    /// 401 or 403: read the cookie again.
    Refused,
    /// No answer, or one without a tip: keep the cookie.
    NoAnswer,
}

/// One `getblockchaininfo` over its own HTTP/1.1 connection: the validator's `blocks`.
async fn blockchain_info(address: &str, cookie: Option<&str>) -> Result<u64, Poll> {
    let mut stream = TcpStream::connect(address)
        .await
        .map_err(|_| Poll::NoAnswer)?;
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"getblockchaininfo","params":[]}"#;
    let auth = cookie
        .map(|cookie| format!("Authorization: Basic {}\r\n", STANDARD.encode(cookie)))
        .unwrap_or_default();
    let request = format!(
        "POST / HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\n{auth}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|_| Poll::NoAnswer)?;
    let mut answer = Vec::new();
    stream
        .read_to_end(&mut answer)
        .await
        .map_err(|_| Poll::NoAnswer)?;
    let answer = String::from_utf8_lossy(&answer);
    let status = answer
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or(Poll::NoAnswer)?;
    if matches!(status, 401 | 403) {
        return Err(Poll::Refused);
    }
    let (_, body) = answer.split_once("\r\n\r\n").ok_or(Poll::NoAnswer)?;
    let value: Value = serde_json::from_str(body).map_err(|_| Poll::NoAnswer)?;
    value["result"]["blocks"].as_u64().ok_or(Poll::NoAnswer)
}

async fn accept(listener: TcpListener, serve: Serve) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                tokio::spawn(connection(stream, serve.clone()));
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
        }
    }
}

/// HTTP/2 with prior knowledge, or HTTP/1.1, by the first bytes.
async fn connection(stream: TcpStream, serve: Serve) {
    const PREFACE: &[u8] = b"PRI * HTTP/2.0";
    let mut first = [0u8; 14];
    let peeked = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let n = stream.peek(&mut first).await?;
            if n == 0 || n >= first.len() || !PREFACE.starts_with(&first[..n]) {
                return std::io::Result::Ok(n);
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    match peeked {
        Ok(Ok(n)) if n >= PREFACE.len() && first == *PREFACE => {
            let _ = h2c(stream, serve).await;
        }
        Ok(Ok(n)) if n > 0 => {
            let _ = tokio::time::timeout(Duration::from_secs(10), http1(stream, serve)).await;
        }
        _ => {}
    }
}

/// One HTTP/1.1 request: `/status` and `/readyz` for `GET`, 404 for anything else.
async fn http1(stream: TcpStream, serve: Serve) -> std::io::Result<()> {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let request: Vec<&str> = line.split_whitespace().take(2).collect();
    let mut header = String::new();
    for _ in 0..100 {
        header.clear();
        if reader.read_line(&mut header).await? == 0 || header.trim_end().is_empty() {
            break;
        }
    }
    let (code, body) = match request.as_slice() {
        ["GET", "/status"] => ("200 OK", serve.status().to_string()),
        ["GET", "/readyz"] => {
            let status = serve.status();
            let ready = matches!(status["state"].as_str(), Some("serving" | "degraded"));
            let body = json!({ "state": status["state"], "detail": status["detail"] }).to_string();
            (
                if ready {
                    "200 OK"
                } else {
                    "503 Service Unavailable"
                },
                body,
            )
        }
        _ => ("404 Not Found", String::new()),
    };
    let response = format!(
        "HTTP/1.1 {code}\r\nContent-Type: application/json\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    write.write_all(response.as_bytes()).await?;
    write.shutdown().await
}

/// The h2c side: `GetLightdInfo` and `GetLatestBlock`; any other method `UNIMPLEMENTED`.
async fn h2c(stream: TcpStream, serve: Serve) -> Result<(), h2::Error> {
    let mut connection = h2::server::handshake(stream).await?;
    while let Some(request) = connection.accept().await {
        let (request, respond) = request?;
        tokio::spawn(answer(request, respond, serve.clone()));
    }
    Ok(())
}

async fn answer(
    request: http::Request<h2::RecvStream>,
    mut respond: h2::server::SendResponse<Bytes>,
    serve: Serve,
) -> Result<(), h2::Error> {
    let method = request
        .uri()
        .path()
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_string();
    let mut body = request.into_body();
    while let Some(chunk) = body.data().await {
        let chunk = chunk?;
        let _ = body.flow_control().release_capacity(chunk.len());
    }
    let (message, code) = match method.as_str() {
        "GetLightdInfo" => (serve.lightd_info(), 14),
        "GetLatestBlock" => (serve.latest_block(), 14),
        _ => (None, 12),
    };
    let response = http::Response::builder()
        .status(200)
        .header("content-type", "application/grpc")
        .body(())
        .expect("a valid response");
    let mut send = respond.send_response(response, false)?;
    let code = match message {
        Some(message) => {
            let mut frame = Vec::with_capacity(5 + message.len());
            frame.push(0);
            frame.extend_from_slice(&(message.len() as u32).to_be_bytes());
            frame.extend_from_slice(&message);
            send.send_data(Bytes::from(frame), false)?;
            0
        }
        None => code,
    };
    let mut trailers = http::HeaderMap::new();
    trailers.insert(
        "grpc-status",
        http::HeaderValue::from_str(&code.to_string()).expect("a number"),
    );
    send.send_trailers(trailers)
}

/// The first shutdown request: `SIGTERM` or `SIGINT` on Unix, Ctrl-C on Windows. The handlers
/// are installed before this returns.
fn shutdown_signal() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()>>> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler can be installed");
        let mut int = signal(SignalKind::interrupt()).expect("SIGINT handler can be installed");
        Box::pin(async move {
            tokio::select! {
                _ = term.recv() => {}
                _ = int.recv() => {}
            }
        })
    }
    #[cfg(not(unix))]
    {
        Box::pin(async {
            let _ = tokio::signal::ctrl_c().await;
        })
    }
}
