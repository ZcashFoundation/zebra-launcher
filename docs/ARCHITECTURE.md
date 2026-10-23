# Architecture

Zebra Launcher is a Tauri 2 desktop app that supervises a bundled `zebrad`, streams its
logs, edits its config, and shows node status. A small Rust team must be able to
maintain it for years, so the rules are: Rust only (no Node toolchain), a thin Tauri
layer over testable crates, and no mutexes.

This document describes the target design. It lands milestone by milestone (M1–M12);
sections name the milestone where that matters.

## Overview

```text
+--------------------------- Zebra Launcher (one process) ---------------------------+
|                                                                                    |
|  ui/  Leptos CSR (wasm) --- invoke / Channel<Vec<LogLine>> ---> src-tauri/         |
|   |   Dashboard, Logs, Configure, Settings                       commands, tray,   |
|   |                                                              updater, exit     |
|   +-- <iframe> Grafana (no IPC)                                      |             |
|                                                                      v             |
|  crates/launcher-core  (no Tauri)                                                  |
|    supervisor actor  <-- mpsc<Command>     watch<Status> -->                       |
|      | log ring + broadcast<LogLine>       config, settings actor, RPC monitor     |
|      v                                                                             |
|    spawner thread (std::process::Command, PDEATHSIG / Job Object)                  |
+------|-----------------------------------------------------------------------------+
       | stdin pipe (closed = stop), stdout/stderr drained
       v
   zebrad (sidecar "zebra-launcher-zebrad")
       RPC      127.0.0.1:8232  <-- cookie-auth JSON-RPC poll every 5 s (status strip)
       metrics  127.0.0.1:9999  <-- Prometheus <-- Grafana 127.0.0.1:3000 (optional)
       P2P      [::]:8233
```

## Crates

| Path | Role |
| --- | --- |
| `crates/launcher-types` | serde types shared by backend and UI (`Status`, `LogLine`, `Settings`, `NodeStats`, errors). Builds natively and to wasm; no tokio, no Tauri. |
| `crates/launcher-core` | Logic that does not need Tauri: supervisor actor, process control (unix/windows), log parsing and classifiers, log file, pidfile, restart policy, config, settings, RPC monitor, paths, end-of-support, install kind. |
| `crates/launcher-core/src/bin/fake-zebrad.rs` | Scripted stand-in for zebrad, used by tests and PR bundle builds. |
| `src-tauri` | Thin Tauri 2.12 app: commands, the `Channel` bridge, tray, updater, exit flow, capabilities. Joins the workspace in M5. |
| `ui` | Leptos CSR crate built by trunk into `ui/dist` (M5). Talks to Tauri through about 100 lines of hand-written `wasm_bindgen` externs to `window.__TAURI__`. |

Single sources of truth: `[workspace.package] version` in [`Cargo.toml`](../Cargo.toml)
for the launcher version, [`ZEBRAD_VERSION`](../ZEBRAD_VERSION) for the bundled zebrad,
and [`rust-toolchain.toml`](../rust-toolchain.toml) for the toolchain.

## Supervisor

One actor task owns the zebrad child. Nothing else touches it.

| Channel | Direction | Carries |
| --- | --- | --- |
| `mpsc<Command>` | in | `Start`, `Stop`, `Restart`, `ApplyConfig`, `Shutdown` |
| `watch<Status>` | out | `Stopped`, `Starting`, `Running{pid, since, rpc_ready}`, `Stopping`, `Exited{code, class}`, `Failed{class}` |
| `broadcast<LogLine>` | out | live log lines, each with a monotonic `seq` |
| `oneshot` | reply | `History{since}` from the 10 000-line `VecDeque` ring, for late subscribers |

The actor also writes a rotating `zebrad.log` for post-crash support. The Tauri side
(`src-tauri/src/bridge.rs`) sends the retained history, then batches live lines every
100 ms (or every 1 000 lines) into a `tauri::ipc::Channel<Vec<LogLine>>`. When the
bridge lags behind the broadcast, the dropped lines are replaced by one synthetic line
with `stream: Launcher`, level `WARN`, no timestamp and the message `N lines dropped`,
which takes the `seq` of the last dropped line. The UI shows that marker, together with
any `seq` jump before it, as one "… N lines dropped" row, and a real line later fetched
with `log_history` replaces the marker.

## UI and backend IPC

Commands (`src-tauri/src/commands.rs`) return `launcher-types` structs; errors are strings,
except `apply_config`, which rejects with an `ApplyError {kind, message}`. Events carry
changes the UI did not ask for: `status` (every `Status` change), `node_stats` (every
`Option<NodeStats>` change) and `stale_process` (a zebrad from an earlier launcher run
blocked the start at launch).

At startup the UI subscribes first and then fetches a snapshot (`get_status`,
`get_node_stats`, `log_history`), so no change falls between the two. An event that
arrives while a snapshot request is in flight is newer than the snapshot, so the UI drops
the `get_status` and `get_node_stats` snapshots once an event of the same kind has
arrived. Log batches and history are merged by `seq`, so their order does not matter.
`stale_process` may be emitted before the UI listens, so the stale-process banner is
driven by `status.stale`, which the snapshot carries. A payload that does not deserialize
is shown in the UI's error banner (the first one per event or channel), not only logged.

**Spawning** happens on one dedicated, long-lived OS thread (the spawner) with
`std::process::Command`, then `tokio::process::Child::from_std`. It must not be a tokio
blocking-pool thread, because Linux `PR_SET_PDEATHSIG` fires when the spawning *thread*
exits.

**Exit classification.** Reader tasks drain stdout and stderr to EOF before an exit is
classified. Classifiers are anchored on level and target, not bare substrings:

| Class | Matches | Retried |
| --- | --- | --- |
| `ConfigError` | `Zebra could not load the provided configuration` | no |
| `EosHalt` | `Zebra refuses to run` | no |
| `EosWarn` | `Your Zebra release is too old` | warning only |
| `PortInUse` | address already in use | no |
| `DbLocked` | `Database likely already open` | no |
| `BinaryMissing` | sidecar not found | no |
| crash | anything else | 1 → 60 s backoff, gives up after 5 in 10 min |

Disk full is also deterministic and never retried. The log lines these depend on are
pinned by fixtures captured from real zebrad; see
[`crates/launcher-core/tests/fixtures/README.md`](../crates/launcher-core/tests/fixtures/README.md).

**Spawn environment.** `zebrad -c <app_config_dir>/zebrad.toml start`. Every inherited
`ZEBRA_*` variable is removed with `env_remove` (never `env_clear`, which would drop
`PATH`, `HOME`, `SystemRoot` and `LOCALAPPDATA`). `ZEBRA_TRACING__USE_COLOR=false` is
forced; `ZEBRA_TRACING__PROGRESS_BAR` and `ZEBRA_TRACING__LOG_FILE` are unset.

## Process control per OS

| | Linux | macOS | Windows |
| --- | --- | --- | --- |
| Graceful stop once zebrad has stdin-close (M6) | close stdin | close stdin | close stdin |
| Graceful stop until then | `SIGTERM` | `SIGTERM` | none: terminate the job; the UI says "hard stop" |
| Escalation after the grace period (default 60 s) | `SIGKILL`, `wait()` | `SIGKILL`, `wait()` | `TerminateJobObject`, wait |
| Launcher dies | `PR_SET_PDEATHSIG(SIGTERM)`; stdin EOF | stdin EOF | per-zebrad Job Object; stdin EOF |
| Stale zebrad on start | pidfile | pidfile | pidfile |

- **Stdin-close shutdown (M6)** is an upstream, opt-in zebrad flag: zebrad shuts down
  when its stdin reaches EOF and logs `stdin closed, starting shutdown` (target
  `zebrad::signal`). It is one mechanism on every OS, and a launcher crash closes the
  pipe too. Windows needs it because zebrad there only handles Ctrl-C, which a
  console-less GUI parent cannot deliver reliably.
- **Windows Job Object:** one per zebrad, created with `CREATE_SUSPENDED` → assign →
  resume, as `zebra-test`'s `process_group/windows.rs` does. The launcher itself is never
  in a job (that would kill the NSIS updater and browsers it opened). `KILL_ON_JOB_CLOSE`
  is set only while the pinned zebrad lacks stdin-close; afterwards the job is kept for
  escalation only. `CREATE_NO_WINDOW` is kept.
- **Stale process:** an orphan cannot be adopted, because its pipes are gone. On startup
  the user chooses "wait for it to exit" or "kill".
- **AppImage:** the sidecar is copied out of the FUSE mount into the app data dir
  (hash-checked, per launcher version) and run from there, so zebrad survives the mount
  going away.

### Measured shutdown times

`zebrad 7.0.0-rc.0` (release build, Linux x86_64 under WSL2, 2026-10-23), from `SIGTERM`
to process exit, timed with `date +%s%N`, two runs each:

| Node | Run 1 | Run 2 | Exit code |
| --- | --- | --- | --- |
| Regtest, idle, RPC open for 10 s | 8 ms | 8 ms | 0 |
| Testnet, default peers, ~90 s into its initial checkpoint sync (tips at heights 39 600 and 10 400) | 37 ms | 34 ms | 0 |

In every run zebrad logged `received SIGTERM, starting shutdown`, `shutting down Zebra`
and `stopping zebrad`, and none came near zebrad's 20 s task shutdown timeout. The 60 s
stop grace therefore only matters for a zebrad that hangs. Not measured yet: a node at
the chain tip with a large non-finalized state, and macOS and Windows. The opt-in
contract tests in `crates/launcher-core/tests/real_zebrad.rs` print the stop time of
every run (see [DEVELOPMENT.md](DEVELOPMENT.md#real-zebrad-contract-tests)).

## Config protocol

The launcher writes a **minimal** `zebrad.toml`: the keys it manages plus the user's
edits, never full `zebrad generate` output. Every zebrad section is
`deny_unknown_fields`, so a stale defaulted key would break the next zebrad bump.

| Managed key | Value |
| --- | --- |
| `network.network` | chosen at first run |
| `rpc.listen_addr` | `127.0.0.1:8232` (`18232` on Testnet) |
| `rpc.enable_cookie_auth` | `true` |
| `rpc.cookie_dir` | `<app_local_data_dir>/rpc` |
| `metrics.endpoint_addr` | `127.0.0.1:9999` |
| `state.cache_dir` | `<chosen data dir>/state` |
| `zcashd_compat` | forced off (unsupported) |

The managed keys are launcher-owned: the supervisor and the RPC monitor rely on them.
`config::ensure` rewrites them at every start, and `config::begin_apply_with_managed`
overrides any edit to them before saving, so the editor cannot change them; they change
only through the launcher (first run, `PortInUse` remediation).

Applying an edit:

1. `read_config` returns the text and a compare-and-swap token.
2. `apply_config` checks TOML syntax and the token, so an external edit is never silently
   overwritten. It refuses non-loopback RPC (including the indexer and lightwalletd
   listeners), metrics or health addresses and cookie auth off. It writes atomically (temp file + rename, `.bak` kept) and sets a pending-apply
   marker.
3. If zebrad is not running (stopped, or failed and waiting for the user), the new file
   is committed and read on the next start. Otherwise zebrad restarts gracefully and is
   watched for 20 s. On `ConfigError` the `.bak` is restored, zebrad is restarted, and
   the UI shows zebrad's stderr plus "rolled back". A crash mid-apply rolls back on the
   next launch via the marker.

When the bundled zebrad version changes, the config is validated with a 10 s probe run
first; on failure the managed keys are regenerated, the user's other keys kept, and the
change shown. `PortInUse` remediation offers other ports and writes them into the managed
keys. The RPC address always comes from the written config, never from log lines.

Launcher settings live in `launcher.json` (with `schema_version`), owned by a settings
actor.

## First run and node data

`launcher-core/src/firstrun.rs` holds the Tauri-free logic, tested with temporary
directories; the commands in `src-tauri/src/commands.rs` call it off the async runtime.

| Command | Returns | Does |
| --- | --- | --- |
| `get_first_run_info` | `FirstRunInfo` | first run until `settings.first_run_done`; the default data dir and its free space (`statvfs` / `GetDiskFreeSpaceExW`, at the nearest existing ancestor); alpha.4 leftovers with the state size from a capped walk |
| `pick_data_dir` | `Option<String>` | the native folder picker |
| `choose_data_dir(path)` | `DataDirChoice` | checks `path` is absolute and writable, saves `settings.data_dir`, rewrites `state.cache_dir`; returns the free space, the UI decides whether to warn |
| `import_legacy_state` | `ImportOutcome` | renames `dirs::cache_dir()/zebra/state` into the current state dir; across volumes it fails and asks the user to move it by hand, never copying |
| `delete_node_data` | `u64` (bytes freed) | removes `<data dir>/state` only, and only inside the launcher's data dir or the chosen one |
| `copy_diagnostics` | `String` | versions, OS, status, settings (URL credentials redacted), `zebrad.toml`, last 200 lines of each log; never the RPC cookie |
| `complete_first_run` | `()` | sets `first_run_done`; from then on zebrad starts at launch |

zebrad is not started at launch before the first run is complete. The data commands run
only while zebrad is `Stopped` or `Failed`. A semaphore in `AppState` keeps them apart
from starts: each start (UI, tray, launch, the restart inside `apply_config`) holds one
permit until it finishes, and a data change holds all of them, so zebrad cannot start
while its state is being moved or deleted.

## Window, tray and exit

```text
tray menu: Show | Start zebrad | Stop zebrad | Quit      tooltip: phase (watch<Status>)
window close --(close_to_tray && tray exists)--> hide; zebrad keeps running
            '--(otherwise)--> last window closed -> ExitRequested
Quit / ExitRequested -> prevent_exit -> supervisor.shutdown() capped at grace + 10 s -> exit
Show / second launch (single-instance) / macOS dock -> show + focus the window
```

- The tray is built in `setup` (`src-tauri/src/tray.rs`). On Linux it is
  libayatana-appindicator: menu only, no click events, no tooltip. If the library is
  missing (it panics; the panic is caught) the tray is skipped, `AppState::tray_ready`
  stays false and closing the window quits.
- The exit flow (`src-tauri/src/exit.rs`) runs once, guarded by atomics: a second exit
  request passes through once the shutdown finished; `RunEvent::Exit` blocks on the
  shutdown as a last resort.
- If startup fails (paths, settings, supervisor), the window is hidden and a native error
  dialog names the error and the log directory; dismissing it exits with code 1. A
  `zebrad.toml` that cannot be updated is not fatal: zebrad reports it and the editor can
  fix it.

## Status and metrics

`rpc.rs` and `monitor.rs` poll zebrad's cookie-authenticated RPC every 5 s
(`getblockchaininfo`, `getinfo`, `getmempoolinfo`, `getpeerinfo` count; hourly
`getdeprecationinfo` on Mainnet only) and publish `watch<NodeStats>`. That feeds the
status strip, the primary metrics view. Grafana is optional and run by the user; see
[GRAFANA.md](GRAFANA.md).

On Mainnet, zebrad warns 42 days and halts 56 days after its estimated release height,
so an install that never updates stops working. The UI shows an EOS countdown, a warning
banner, and a blocking halt card with no restart loop.

## Updater (opt-in, M11)

Modes `Off`, `Notify`, `Auto`; Off until the first-run choice, which preselects Notify.
Off makes zero network requests. Notify checks 30 s after launch and every 6 h with
jitter. Auto downloads in the background and installs at the EOS warning or on user
click after a cancellable countdown, never silently mid-session.

```text
check latest.json -> download -> Update::verify (minisign signature)
  -> graceful stop of zebrad -> install -> app.restart()
  -> health gate: Running with RPC within 5 min, else recovery UI
install fails -> restart zebrad, show the error
```

- Endpoint: `https://github.com/ZcashFoundation/zebra-launcher/releases/latest/download/latest.json`,
  the latest non-prerelease release. Release candidates are rehearsed with a build-time
  endpoint override, never a runtime environment variable.
- `allowDowngrades = false`; roll forward only.
- Updatable formats: AppImage, NSIS per-user, `.app.tar.gz` per macOS arch. The `.deb`
  is Notify-only and shows the release link.

## Release pipeline (M9, M10)

```text
ZEBRAD_VERSION
  |- Linux x86_64/aarch64: download the Zebra release tarball,
  |    cosign verify-blob SHA256SUMS, gh attestation verify
  '- macOS aarch64/x86_64, Windows x86_64: zebrad-sidecars.yml builds the Zebra tag,
       smoke-runs it, attests it, caches it unsigned

tag v* -> release.yml
  1. create one draft release; pass its id to the matrix
  2. unprivileged jobs, no secrets: cargo tauri build --no-bundle
  3. environment "release" (required reviewer, no caches): cargo tauri bundle,
     codesign + notarization (macOS), Azure Artifact Signing (Windows app and sidecar),
     updater .sig files
  4. final job: latest.json with all five platform keys, SHA256SUMS,
     keyless cosign bundle, attestations
  5. a human smoke-tests and publishes; publishing makes latest.json live
```

Targets: macOS Apple Silicon, macOS Intel, Windows x64, Linux x64 and arm64. Linux ships
AppImage and deb, built in `ubuntu:22.04` containers (glibc floor 2.35, above zebrad's
2.34). See [RELEASING.md](RELEASING.md) and [SECURITY.md](SECURITY.md).

CI ([`.github/workflows/ci.yml`](../.github/workflows/ci.yml)) runs fmt, clippy with the
mutex ban, tests and `cargo deny` on Linux, and clippy and tests on `windows-2025` and
`macos-15`. `bundle.yml` (M9) builds unsigned bundles with the real zebrad for all five
targets.

## Decisions

Recorded by the maintainer on 2026-10-23:

- Tauri 2 stable, in this repo with its own CI; native installers that need neither Rust
  nor Node on the user's machine.
- Leptos CSR frontend: no SSR or server functions, no `tauri-sys`.
- Metrics: the RPC status strip first; a user-run Grafana in an iframe as the easy path,
  never bundled.
- Graceful stop through an upstream opt-in stdin-close mode in zebrad.
- Signing: Apple Developer ID with notarization; Windows through Azure Artifact Signing.
- Five targets, with separate per-arch macOS builds.
- A long-lived minisign updater key, documented as an exception to Zebra's ADR 0008
  (Tauri's updater has no keyless mode).
- Identifier `org.zfnd.zebra-launcher`; the launcher has its own semver; About shows
  "Zebra Launcher X.Y.Z (zebrad A.B.C)".
- AppImage and deb on Linux (deb is Notify-only); NSIS only on Windows.
- No mutexes in Rust code, enforced by [`clippy.toml`](../clippy.toml).
- No history rewrite; the browser demo is deleted.

Routine choices: minimal managed `zebrad.toml`; an app-owned state dir chosen at first
run, never reusing a CLI zebrad's state; `zcashd_compat` unsupported; a GitHub App token
for bot PRs.

<!-- TODO: link the approved plan once it is committed to the repository. -->
