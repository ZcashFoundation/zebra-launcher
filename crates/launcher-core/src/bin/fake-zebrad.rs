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
//! `start --shutdown-on-stdin-close` also shuts down when stdin reaches EOF,
//! mirroring the flag proposed for zebrad.

use std::{
    io::{Read, Write},
    path::PathBuf,
    process::ExitCode,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use tokio::sync::oneshot;

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
    runtime.block_on(run(
        config,
        mode,
        startup_delay,
        heartbeat,
        shutdown_on_stdin_close,
    ))
}

async fn run(
    config: Option<PathBuf>,
    mode: String,
    startup_delay: Duration,
    heartbeat: Duration,
    shutdown_on_stdin_close: bool,
) -> ExitCode {
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
    tokio::time::sleep(startup_delay).await;
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
    log(
        "INFO",
        None,
        "zebra_rpc::server::cookie",
        "RPC auth cookie written to disk",
    );
    log(
        "INFO",
        None,
        "zebra_rpc::server",
        &format!("Opened RPC endpoint at {RPC_ADDR}"),
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

    let mut shutdown = shutdown_requests(shutdown_on_stdin_close);
    let mut height: u64 = 1_000;
    loop {
        tokio::select! {
            request = &mut shutdown => {
                let request = request.unwrap_or(Shutdown::StdinClosed);
                if mode == "hang-on-signal" {
                    log("WARN", None, "fake_zebrad", "shutdown requested, ignoring in hang-on-signal mode");
                    shutdown = pending_forever();
                    continue;
                }
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
                log("INFO", None, "zebrad::components::tokio", "waiting for async tokio tasks to shut down");
                log("INFO", None, "zebrad::components::tokio", "shutting down Zebra");
                log("INFO", None, "zebrad::commands::start", "stopping zebrad");
                return ExitCode::SUCCESS;
            }
            _ = tokio::time::sleep(heartbeat) => {
                height += 1;
                log(
                    "INFO",
                    None,
                    "zebrad::components::sync::progress",
                    &format!("estimated progress to chain tip sync_percent=12.345 % current_height=Height({height}) network_upgrade=Nu6 remaining_sync_blocks=2500000 time_since_last_state_block=0s"),
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
    }
}

/// Resolves on the first shutdown request: a signal, or stdin EOF when enabled.
fn shutdown_requests(
    shutdown_on_stdin_close: bool,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Shutdown, oneshot::error::RecvError>>>>
{
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        let request = wait_for_signal().await;
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

fn pending_forever()
-> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Shutdown, oneshot::error::RecvError>>>>
{
    Box::pin(std::future::pending())
}

#[cfg(unix)]
async fn wait_for_signal() -> Shutdown {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler can be installed");
    let mut int = signal(SignalKind::interrupt()).expect("SIGINT handler can be installed");
    tokio::select! {
        _ = term.recv() => Shutdown::Signal { kind: 15, name: "SIGTERM" },
        _ = int.recv() => Shutdown::Signal { kind: 2, name: "SIGINT" },
    }
}

#[cfg(windows)]
async fn wait_for_signal() -> Shutdown {
    use tokio::signal::windows::{ctrl_break, ctrl_c};
    let mut ctrl_c = ctrl_c().expect("Ctrl-C handler can be installed");
    let mut ctrl_break = ctrl_break().expect("Ctrl-Break handler can be installed");
    tokio::select! {
        _ = ctrl_c.recv() => Shutdown::Signal { kind: 0, name: "Ctrl-C" },
        _ = ctrl_break.recv() => Shutdown::Signal { kind: 1, name: "Ctrl-Break" },
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
    Duration::from_millis(
        std::env::var(name)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(default),
    )
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
