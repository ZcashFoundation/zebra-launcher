//! A scripted stand-in for `zebrad`, used by the supervisor tests and by
//! pull-request bundle builds that must not download the real node.
//!
//! It accepts the subset of zebrad's command line the launcher uses
//! (`--version`, `generate -o FILE`, `-c FILE start`), prints the same log
//! lines the launcher's detectors key on, and shuts down on the same signals.
//! `FAKE_ZEBRAD_MODE` selects a scenario:
//!
//! | mode | behaviour |
//! |---|---|
//! | `ok` (default) | starts, logs a heartbeat, shuts down cleanly on request |
//! | `crash` | exits with code 2 right after start-up |
//! | `config-error` | prints zebrad's config error to stderr and exits 1 |
//! | `eos-warn` | like `ok`, plus the end-of-support warning on every heartbeat |
//! | `eos-halt` | aborts with the end-of-support panic after start-up |
//! | `port-in-use` | aborts with the metrics-port panic before `Starting zebrad` |
//! | `db-locked` | aborts with the database-lock panic during start-up |
//! | `hang-on-signal` | ignores shutdown requests, so the supervisor must kill it |
//! | `stderr-flood` | writes 20 000 lines to stderr after start-up, then behaves like `ok` |
//! | `non-utf8` | writes a line of invalid UTF-8 to stdout, then behaves like `ok` |
//!
//! `FAKE_ZEBRAD_STARTUP_DELAY_MS` delays the lines after `Starting zebrad`, and
//! `FAKE_ZEBRAD_HEARTBEAT_MS` sets the heartbeat interval (default 500 ms).
//! `FAKE_ZEBRAD_LINGER_MS` makes the fake leave a grandchild behind when it
//! exits (except by a panic) that inherits stdout and stderr and lives that
//! long, the way a zebrad subprocess could keep the pipes open: the
//! supervisor then sits in its exit drain. `start --shutdown-on-stdin-close`
//! also shuts down when stdin reaches EOF, mirroring the flag proposed for
//! zebrad.
//!
//! When the `-c` config sets `rpc.listen_addr`, the fake also serves a minimal
//! JSON-RPC endpoint there (port 0 picks a free port, and the bound address is
//! printed in the `Opened RPC endpoint at` line). With `rpc.cookie_dir` set and
//! `rpc.enable_cookie_auth` not `false`, it writes a random cookie to
//! `<cookie_dir>/.cookie` in zebrad's format and answers `401` to requests
//! without the matching Basic credentials. It answers `getblockchaininfo`,
//! `getinfo`, `getmempoolinfo`, `getpeerinfo` and `getdeprecationinfo` with
//! canned values (see [`canned`]), `fakegetcallcounts` with the number of calls
//! per method so far, and `-32601` to anything else.
//! `FAKE_ZEBRAD_DEPRECATION_FAILURES=N` makes the first `N` `getdeprecationinfo`
//! calls fail with the JSON-RPC error code in
//! `FAKE_ZEBRAD_DEPRECATION_ERROR_CODE` (default `-32000`).

use std::{
    hash::{BuildHasher, Hasher, RandomState},
    io::{Read, Write},
    net::SocketAddr,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    sync::oneshot,
};

const VERSION: &str = "7.0.0";
const RPC_ADDR: &str = "127.0.0.1:8232";
const METRICS_ADDR: &str = "127.0.0.1:9999";

/// Why the fake is shutting down, worded like zebrad's `zebrad::signal` lines.
enum Shutdown {
    Signal { kind: i32, name: &'static str },
    StdinClosed,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut config: Option<PathBuf> = None;
    let mut rest: Vec<String> = Vec::new();
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--version" | "-V" => {
                println!("zebrad {VERSION}");
                return ExitCode::SUCCESS;
            }
            "-c" | "--config" => config = iter.next().map(PathBuf::from),
            _ => rest.push(arg),
        }
    }

    match rest.first().map(String::as_str) {
        Some("generate") => generate(&rest[1..]),
        Some("start") | None => start(config, rest.get(1..).unwrap_or(&[])),
        // The grandchild of `FAKE_ZEBRAD_LINGER_MS`: holds the inherited
        // pipes open for the given number of milliseconds.
        Some("linger") => {
            let ms = rest.get(1).and_then(|ms| ms.parse().ok()).unwrap_or(0);
            std::thread::sleep(Duration::from_millis(ms));
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("error: unrecognized subcommand '{other}'");
            ExitCode::from(2)
        }
    }
}

fn generate(args: &[String]) -> ExitCode {
    let output = args
        .iter()
        .position(|a| a == "-o" || a == "--output-file")
        .and_then(|i| args.get(i + 1));
    let config = "# Default configuration for zebrad (fake).\n\n[network]\nnetwork = \"Mainnet\"\n\n[rpc]\nenable_cookie_auth = true\n\n[metrics]\n".to_string();
    match output {
        Some(path) => match std::fs::write(path, config) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("error: could not write {path}: {err}");
                ExitCode::from(1)
            }
        },
        None => {
            print!("{config}");
            ExitCode::SUCCESS
        }
    }
}

fn start(config: Option<PathBuf>, args: &[String]) -> ExitCode {
    let shutdown_on_stdin_close = args.iter().any(|a| a == "--shutdown-on-stdin-close");
    let mode = std::env::var("FAKE_ZEBRAD_MODE").unwrap_or_else(|_| "ok".to_string());
    let startup_delay = env_millis("FAKE_ZEBRAD_STARTUP_DELAY_MS", 0);
    let heartbeat = env_millis("FAKE_ZEBRAD_HEARTBEAT_MS", 500);

    if mode == "config-error" {
        eprintln!(
            "error: Zebra could not load the provided configuration file and/or environment variables.This might mean you are using a deprecated format of the file, or are attempting to\n                     configure deprecated or unknown fields via environment variables.You can generate a valid config by running \"zebrad generate\", and diff it against yours to examine any format inconsistencies."
        );
        eprintln!(
            "error: zebrad fatal error: I/O operation failed: Configuration error: unknown field `bogus`, expected one of `listen_addr`, `indexer_listen_addr`, `lightwalletd_listen_addr`, `parallel_cpu_threads`, `debug_force_finished_sync`, `cookie_dir`, `enable_cookie_auth`, `max_response_body_size` for key `rpc`"
        );
        return ExitCode::from(1);
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime can always be built");
    let code = runtime.block_on(run(
        config,
        mode,
        startup_delay,
        heartbeat,
        shutdown_on_stdin_close,
    ));
    spawn_lingerer();
    code
}

/// With `FAKE_ZEBRAD_LINGER_MS` set, starts the `linger` grandchild that keeps
/// this process's stdout and stderr open after it exits.
fn spawn_lingerer() {
    let linger = env_millis("FAKE_ZEBRAD_LINGER_MS", 0);
    if linger.is_zero() {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    // stdout and stderr are inherited; the handle is dropped so the
    // grandchild outlives this process.
    let _ = std::process::Command::new(exe)
        .arg("linger")
        .arg(linger.as_millis().to_string())
        .stdin(std::process::Stdio::null())
        .spawn();
}

async fn run(
    config: Option<PathBuf>,
    mode: String,
    startup_delay: Duration,
    heartbeat: Duration,
    shutdown_on_stdin_close: bool,
) -> ExitCode {
    // Like zebrad, handle shutdown requests from the very start, so a stop
    // during start-up takes the graceful `zebrad::signal` path.
    let mut shutdown = shutdown_requests(shutdown_on_stdin_close);
    log(
        "INFO",
        None,
        "zebrad::components::tracing::component",
        "started tracing component filter=\"info\" TRACING_STATIC_MAX_LEVEL=LevelFilter::INFO LOG_STATIC_MAX_LEVEL=Info",
    );
    log("INFO", None, "zebrad::application", "Diagnostic metadata:");
    println!("version: {VERSION}+fake");
    println!("Zcash network: Regtest");
    println!("features: fake");
    match &config {
        Some(path) => log(
            "INFO",
            None,
            "zebrad::application",
            &format!("Using config file at: {:?}", path.display().to_string()),
        ),
        None => log(
            "INFO",
            None,
            "zebrad::application",
            "No config file provided, using default configuration",
        ),
    }
    let global = Some("{zebrad=\"fake\" net=\"Regtest\"}");
    log(
        "INFO",
        global,
        "zebrad::components::metrics",
        &format!("Trying to open metrics endpoint at {METRICS_ADDR}..."),
    );
    if mode == "port-in-use" {
        panic_and_abort(
            "zebrad/src/components/metrics.rs:38:27",
            &format!(
                "Opening metrics endpoint listener {METRICS_ADDR} failed: FailedToCreateHTTPListener(\"Address already in use (os error 98)\"). Hint: Check if another zebrad or zcashd process is running. Try changing the metrics endpoint_addr in the Zebra config."
            ),
        );
    }
    log(
        "INFO",
        global,
        "zebrad::components::metrics",
        &format!("Opened metrics endpoint at {METRICS_ADDR}"),
    );
    log("INFO", None, "zebrad::commands::start", "Starting zebrad");
    log(
        "INFO",
        None,
        "zebrad::commands::start",
        "initializing node state",
    );
    log(
        "INFO",
        None,
        "zebrad::commands::start",
        "opening database, this may take a few minutes",
    );
    if sleep_unless_shutdown(startup_delay, &mut shutdown, &mode).await {
        return ExitCode::SUCCESS;
    }
    if mode == "db-locked" {
        panic_and_abort(
            "zebra-state/src/service/finalized_state/disk_db.rs:1089:83",
            "Database likely already open \"<DATA_DIR>/state/state/v29/regtest\" Hint: Check if another zebrad process is running.",
        );
    }
    log(
        "INFO",
        Some("open_listener{addr=127.0.0.1:0}"),
        "zebra_network::peer_set::initialize",
        "Opened Zcash protocol endpoint at 127.0.0.1:18233",
    );
    let (rpc_addr, cookie_written) = match config.as_deref().and_then(rpc_config) {
        Some(rpc) => match start_rpc_server(rpc).await {
            Ok(started) => started,
            Err(err) => panic_and_abort(
                "zebra-rpc/src/server.rs:1:1",
                &format!("Unexpected error starting RPC server: {err}"),
            ),
        },
        // Without a configured listener, only print the lines the log
        // detectors key on.
        None => (RPC_ADDR.to_string(), true),
    };
    if cookie_written {
        log(
            "INFO",
            None,
            "zebra_rpc::server::cookie",
            "RPC auth cookie written to disk",
        );
    }
    log(
        "INFO",
        None,
        "zebra_rpc::server",
        &format!("Opened RPC endpoint at {rpc_addr}"),
    );
    log(
        "INFO",
        None,
        "zebrad::commands::start",
        "spawned initial Zebra tasks",
    );

    match mode.as_str() {
        "crash" => {
            eprintln!("fake-zebrad: simulated crash");
            return ExitCode::from(2);
        }
        "eos-halt" => panic_and_abort(
            "zebrad/src/components/sync/end_of_support.rs:91:9",
            &format!(
                "Zebra refuses to run if the release date is older than 56 days. \nRelease name: {VERSION}, Estimated release height: 3526000 \nHint: Download and install the latest Zebra release from: https://github.com/ZcashFoundation/zebra/releases/latest"
            ),
        ),
        "non-utf8" => {
            let mut stdout = std::io::stdout().lock();
            let _ = stdout.write_all(b"\xff\xfe invalid utf-8 bytes on this line\n");
            let _ = stdout.flush();
        }
        "stderr-flood" => {
            let mut stderr = std::io::stderr().lock();
            for i in 0..20_000 {
                let _ = writeln!(stderr, "flood line {i}");
            }
            let _ = stderr.flush();
        }
        _ => {}
    }

    let mut height: u64 = 1_000;
    loop {
        if sleep_unless_shutdown(heartbeat, &mut shutdown, &mode).await {
            return ExitCode::SUCCESS;
        }
        height += 1;
        log(
            "INFO",
            None,
            "zebrad::components::sync::progress",
            &format!(
                "estimated progress to chain tip sync_percent=12.345 % current_height=Height({height}) network_upgrade=Nu6 remaining_sync_blocks=2500000 time_since_last_state_block=0s"
            ),
        );
        if mode == "eos-warn" {
            log(
                "WARN",
                None,
                "zebrad::components::sync::end_of_support",
                "Your Zebra release is too old and it will stop running at block 3706000. Hint: Download and install the latest Zebra release from: https://github.com/ZcashFoundation/zebra/releases/latest",
            );
        }
    }
}

type ShutdownFuture = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<Shutdown, oneshot::error::RecvError>>>,
>;

/// Sleeps for `duration` unless a shutdown request arrives first. On a request
/// prints zebrad's shutdown lines and returns true, except in `hang-on-signal`
/// mode, which ignores requests and keeps sleeping.
async fn sleep_unless_shutdown(
    duration: Duration,
    shutdown: &mut ShutdownFuture,
    mode: &str,
) -> bool {
    let sleep = tokio::time::sleep(duration);
    tokio::pin!(sleep);
    loop {
        tokio::select! {
            request = &mut *shutdown => {
                if mode == "hang-on-signal" {
                    log("WARN", None, "fake_zebrad", "shutdown requested, ignoring in hang-on-signal mode");
                    *shutdown = pending_forever();
                    continue;
                }
                log_shutdown(request.unwrap_or(Shutdown::StdinClosed));
                return true;
            }
            _ = &mut sleep => return false,
        }
    }
}

/// Prints the lines zebrad logs while shutting down gracefully.
fn log_shutdown(request: Shutdown) {
    match request {
        Shutdown::Signal { kind, name } => log(
            "INFO",
            Some(&format!("sig{{kind=SignalKind({kind}) name=\"{name}\"}}")),
            "zebrad::signal",
            &format!("received {name}, starting shutdown"),
        ),
        Shutdown::StdinClosed => log(
            "INFO",
            None,
            "zebrad::signal",
            "stdin closed, starting shutdown",
        ),
    }
    log(
        "INFO",
        None,
        "zebrad::components::tokio",
        "waiting for async tokio tasks to shut down",
    );
    log(
        "INFO",
        None,
        "zebrad::components::tokio",
        "shutting down Zebra",
    );
    log("INFO", None, "zebrad::commands::start", "stopping zebrad");
}

/// The `[rpc]` settings the fake RPC server uses.
struct RpcConfig {
    listen_addr: SocketAddr,
    /// Where to write the cookie; `None` disables authentication.
    cookie_dir: Option<PathBuf>,
}

/// Reads `rpc.listen_addr`, `rpc.cookie_dir` and `rpc.enable_cookie_auth`.
/// Returns `None` when the file is unreadable or sets no listener.
fn rpc_config(path: &Path) -> Option<RpcConfig> {
    let doc: toml_edit::DocumentMut = std::fs::read_to_string(path).ok()?.parse().ok()?;
    let rpc = doc.get("rpc")?.as_table_like()?;
    let listen_addr = rpc.get("listen_addr")?.as_str()?.parse().ok()?;
    let auth = rpc
        .get("enable_cookie_auth")
        .and_then(|item| item.as_bool())
        .unwrap_or(true);
    let cookie_dir = rpc
        .get("cookie_dir")
        .and_then(|item| item.as_str())
        .filter(|_| auth)
        .map(PathBuf::from);
    Some(RpcConfig {
        listen_addr,
        cookie_dir,
    })
}

/// Binds the RPC listener, writes the cookie, and serves requests in the
/// background. Returns the bound address and whether a cookie was written.
async fn start_rpc_server(config: RpcConfig) -> std::io::Result<(String, bool)> {
    let listener = TcpListener::bind(config.listen_addr).await?;
    let addr = listener.local_addr()?;
    let expected_auth = match &config.cookie_dir {
        Some(dir) => {
            let credentials = format!("__cookie__:{}", random_cookie());
            std::fs::create_dir_all(dir)?;
            std::fs::write(dir.join(".cookie"), &credentials)?;
            Some(format!("Basic {}", STANDARD.encode(credentials)))
        }
        None => None,
    };
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let expected_auth = expected_auth.clone();
                    tokio::spawn(async move {
                        let _ = tokio::time::timeout(
                            Duration::from_secs(10),
                            serve_connection(stream, expected_auth),
                        )
                        .await;
                    });
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
    });
    Ok((addr.to_string(), config.cookie_dir.is_some()))
}

/// 32 random bytes in base64, like zebrad's cookie, without a `rand` dependency.
fn random_cookie() -> String {
    let state = RandomState::new();
    let mut bytes = Vec::with_capacity(32);
    for i in 0..4u64 {
        let mut hasher = state.build_hasher();
        hasher.write_u64(i);
        hasher.write_u128(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
        );
        bytes.extend_from_slice(&hasher.finish().to_le_bytes());
    }
    STANDARD.encode(bytes)
}

/// Serves one HTTP/1.1 request, then closes the connection.
async fn serve_connection(stream: TcpStream, expected_auth: Option<String>) -> std::io::Result<()> {
    const MAX_HEADER_LINES: usize = 100;
    const MAX_BODY: usize = 1024 * 1024;

    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let mut line = String::new();
    if reader.read_line(&mut line).await? == 0 {
        return Ok(());
    }
    let mut content_length = 0usize;
    let mut authorization = None;
    for _ in 0..MAX_HEADER_LINES {
        line.clear();
        if reader.read_line(&mut line).await? == 0 {
            return Ok(());
        }
        let header = line.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            let value = value.trim();
            match name.trim().to_ascii_lowercase().as_str() {
                "content-length" => content_length = value.parse().unwrap_or(usize::MAX),
                "authorization" => authorization = Some(value.to_string()),
                _ => {}
            }
        }
    }

    let (status, body) = if content_length > MAX_BODY {
        ("413 Payload Too Large", String::new())
    } else if expected_auth.is_some() && authorization != expected_auth {
        ("401 Unauthorized", String::new())
    } else {
        let mut body = vec![0; content_length];
        reader.read_exact(&mut body).await?;
        ("200 OK", handle_rpc(&body).to_string())
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    write.write_all(response.as_bytes()).await?;
    write.shutdown().await
}

/// The methods the fake answers, in the order of [`CALL_COUNTS`].
const METHODS: [&str; 5] = [
    "getblockchaininfo",
    "getinfo",
    "getmempoolinfo",
    "getpeerinfo",
    "getdeprecationinfo",
];

/// How often each of [`METHODS`] was called.
static CALL_COUNTS: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];

/// Answers one JSON-RPC request body.
fn handle_rpc(body: &[u8]) -> Value {
    let Ok(request) = serde_json::from_slice::<Value>(body) else {
        return json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": "Parse error" } });
    };
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    if method == "fakegetcallcounts" {
        let counts: serde_json::Map<String, Value> = METHODS
            .iter()
            .zip(&CALL_COUNTS)
            .map(|(m, c)| (m.to_string(), json!(c.load(Ordering::Relaxed))))
            .collect();
        return json!({ "jsonrpc": "2.0", "id": id, "result": counts });
    }
    match METHODS.iter().position(|m| *m == method) {
        Some(i) => {
            let previous = CALL_COUNTS[i].fetch_add(1, Ordering::Relaxed);
            if method == "getdeprecationinfo"
                && previous < env_u64("FAKE_ZEBRAD_DEPRECATION_FAILURES", 0)
            {
                let code = std::env::var("FAKE_ZEBRAD_DEPRECATION_ERROR_CODE")
                    .ok()
                    .and_then(|v| v.parse::<i64>().ok())
                    .unwrap_or(-32000);
                return json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": code, "message": "simulated failure" },
                });
            }
            json!({ "jsonrpc": "2.0", "id": id, "result": canned(method) })
        }
        None => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32601, "message": "Method not found" },
        }),
    }
}

/// zebrad-shaped responses, trimmed to a plausible subset of fields.
fn canned(method: &str) -> Value {
    match method {
        "getblockchaininfo" => json!({
            "chain": "main",
            "blocks": 2_500_000,
            "headers": 2_500_000,
            "difficulty": 1.0,
            "verificationprogress": 0.8333,
            "chainwork": 0,
            "pruned": false,
            "size_on_disk": 0,
            "commitments": 0,
            "bestblockhash": "0000000000000000000000000000000000000000000000000000000000000000",
            "estimatedheight": 3_000_000,
        }),
        "getinfo" => json!({
            "version": 7_000_050,
            "build": format!("v{VERSION}"),
            "subversion": format!("/Zebra:{VERSION}/"),
            "protocolversion": 170_140,
            "blocks": 2_500_000,
            "connections": 8,
            "difficulty": 1.0,
            "testnet": false,
            "paytxfee": 0.0,
            "relayfee": 0.000001,
            "errors": "no errors",
            "errorstimestamp": 0,
        }),
        "getmempoolinfo" => json!({ "size": 42, "bytes": 32_500, "usage": 41_000 }),
        "getpeerinfo" => Value::Array(
            (0..8)
                .map(|i| {
                    json!({
                        "addr": format!("192.0.2.{i}:8233"),
                        "inbound": false,
                        "connection_state": "connected",
                    })
                })
                .collect(),
        ),
        "getdeprecationinfo" => json!({
            "end_of_service": { "block_height": 3_706_000, "estimated_time": 1_800_000_000 },
        }),
        _ => Value::Null,
    }
}

/// Resolves on the first shutdown request: a signal, or stdin EOF when enabled.
///
/// The signal handlers are installed before this returns, so a signal sent
/// any time afterwards is caught rather than taking the default action.
fn shutdown_requests(shutdown_on_stdin_close: bool) -> ShutdownFuture {
    let (tx, rx) = oneshot::channel();
    let signal = wait_for_signal();
    tokio::spawn(async move {
        let request = signal.await;
        let _ = tx.send(request);
    });
    if shutdown_on_stdin_close {
        let (stdin_tx, stdin_rx) = oneshot::channel();
        // A plain thread, as zebrad will do it: a blocking read must not hold a
        // runtime worker, and read errors count as EOF.
        std::thread::spawn(move || {
            let mut sink = [0u8; 1024];
            let mut stdin = std::io::stdin().lock();
            loop {
                match stdin.read(&mut sink) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
            let _ = stdin_tx.send(Shutdown::StdinClosed);
        });
        return Box::pin(async move {
            tokio::select! {
                signal = rx => signal,
                stdin = stdin_rx => stdin,
            }
        });
    }
    Box::pin(rx)
}

fn pending_forever() -> ShutdownFuture {
    Box::pin(std::future::pending())
}

/// Installs the handlers now and returns a future for the first signal.
#[cfg(unix)]
fn wait_for_signal() -> impl std::future::Future<Output = Shutdown> + 'static {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler can be installed");
    let mut int = signal(SignalKind::interrupt()).expect("SIGINT handler can be installed");
    async move {
        tokio::select! {
            _ = term.recv() => Shutdown::Signal { kind: 15, name: "SIGTERM" },
            _ = int.recv() => Shutdown::Signal { kind: 2, name: "SIGINT" },
        }
    }
}

/// Installs the handlers now and returns a future for the first signal.
#[cfg(windows)]
fn wait_for_signal() -> impl std::future::Future<Output = Shutdown> + 'static {
    use tokio::signal::windows::{ctrl_break, ctrl_c};
    let mut ctrl_c = ctrl_c().expect("Ctrl-C handler can be installed");
    let mut ctrl_break = ctrl_break().expect("Ctrl-Break handler can be installed");
    async move {
        tokio::select! {
            _ = ctrl_c.recv() => Shutdown::Signal { kind: 0, name: "Ctrl-C" },
            _ = ctrl_break.recv() => Shutdown::Signal { kind: 1, name: "Ctrl-Break" },
        }
    }
}

/// Prints a line in zebrad's `tracing` Full format.
fn log(level: &str, spans: Option<&str>, target: &str, message: &str) {
    let spans = spans.map(|s| format!("{s}: ")).unwrap_or_default();
    println!("{} {level:>5} {spans}{target}: {message}", timestamp());
}

/// Mimics a release-build panic: Rust's panic header on stderr, the message on
/// the next line, then an abort (release zebrad uses `panic = "abort"`).
fn panic_and_abort(location: &str, message: &str) -> ! {
    eprintln!(
        "\nthread 'main' ({}) panicked at {location}:",
        std::process::id()
    );
    eprintln!("{message}");
    eprintln!("note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace");
    std::process::abort()
}

fn env_millis(name: &str, default: u64) -> Duration {
    Duration::from_millis(env_u64(name, default))
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// RFC 3339 UTC with microseconds, like `tracing-subscriber`'s default timer.
fn timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let micros = now.subsec_micros();
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (year, month, day) = civil_from_days(days as i64);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{micros:06}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Howard Hinnant's days-to-civil algorithm.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}
