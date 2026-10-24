# Architecture

Zebra Launcher is a Tauri 2 desktop app that supervises a bundled `zebrad`, streams its
logs, edits its config, and shows node status. A small Rust team must be able to
maintain it for years, so the rules are: Rust only (no Node toolchain), a thin Tauri
layer over testable crates, and no mutexes.

This document describes the launcher as built. The plan it grew from is [PLAN.md](PLAN.md),
kept as the historical design record; where the two differ, this file describes the code.

## Overview

```text
+--------------------------- Zebra Launcher (one process) ---------------------------+
|                                                                                    |
|  ui/  Leptos CSR (wasm) --- invoke / Channel<Vec<LogLine>> ---> src-tauri/         |
|   |   Dashboard, Logs, Grafana, Configure, Settings              commands, tray,   |
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
| `src-tauri` | Thin Tauri 2.12 app: commands, the `Channel` bridge, tray, updater, exit flow, capabilities. |
| `ui` | Leptos CSR crate built by trunk into `ui/dist`. Talks to Tauri through about 100 lines of hand-written `wasm_bindgen` externs to `window.__TAURI__`. |

Single sources of truth: `[workspace.package] version` in [`Cargo.toml`](../Cargo.toml)
for the launcher version, [`ZEBRAD_VERSION`](../ZEBRAD_VERSION) for the bundled zebrad,
and [`rust-toolchain.toml`](../rust-toolchain.toml) for the toolchain.

## Supervisor

One actor task owns the zebrad child. Nothing else touches it.

| Channel | Direction | Carries |
| --- | --- | --- |
| `mpsc<Cmd>` | in | `Start{options}`, `Stop`, `Shutdown`, `Subscribe`, `History{since}` (`crates/launcher-core/src/supervisor.rs`). `SupervisorHandle::restart` is a `Stop` followed by a `Start`; applying a config is the `apply_config` Tauri command, built on the same handle. |
| `watch<Status>` | out | `phase` (`Stopped`, `Starting`, `Running`, `Stopping`, `Backoff{until, crashes}`, `Failed{class}`), `pid`, `started_at`, the `rpc_addr` and `metrics_addr` zebrad logged as opened, `last_exit` (`ExitInfo{code, class, detail, requested}`) and `stale` (`crates/launcher-types/src/lib.rs`) |
| `broadcast<LogLine>` | out | live log lines, each with a monotonic `seq` |
| `oneshot` | reply | each command's answer; `History{since}` returns lines from the 10 000-line `VecDeque` ring, for late subscribers |

The actor also writes a rotating `zebrad.log` for post-crash support. The Tauri side
(`src-tauri/src/bridge.rs`) sends the retained history, then batches live lines every
100 ms (or every 1 000 lines) into a `tauri::ipc::Channel<Vec<LogLine>>`. When the
bridge lags behind the broadcast, the dropped lines are replaced by one synthetic line
with `stream: Launcher`, level `WARN`, no timestamp and the message `N lines dropped`,
which takes the `seq` of the last dropped line. The UI shows that marker, together with
any `seq` jump before it, as one "… N lines dropped" row; a real line with the same
`seq` arriving later (the `log_history` command exists for that) replaces the marker.

## UI and backend IPC

Commands (`src-tauri/src/commands.rs`) return `launcher-types` structs; errors are strings,
except `apply_config`, which rejects with an `ApplyError {kind, message}`. Events carry
changes the UI did not ask for: `status` (every `Status` change), `node_stats` (every
`Option<NodeStats>` change), `stale_process` (a zebrad from an earlier launcher run
blocked the start at launch), and for the updater `update_info` (every `UpdateInfo`
change), `update_state` (its `UpdateState`, when that changed) and
`update_health_failed` (an `UpdateHealthFailure` after an update that did not come back
healthy).

At startup the UI subscribes first and then fetches a snapshot (`get_status`,
`get_node_stats`, `get_update_info`), so no change falls between the two; the log
subscription itself streams the retained history before live lines, so logs need no
snapshot request.
An event that arrives while a snapshot request is in flight is newer than the snapshot,
so the UI drops the `get_status`, `get_node_stats` and `get_update_info` snapshots once
an event of the same kind has arrived (for the updater: `update_info`; an
`update_state` that arrives before any `update_info` replaces the snapshot's state).
Log batches are merged by `seq`, so their order does not matter.
`stale_process` may be emitted before the UI listens, so the stale-process banner is
driven by `status.stale`, which the snapshot carries; likewise `update_health_failed`
is also in `UpdateInfo.health_failure`. A payload that does not deserialize is shown in
the UI's error banner (the first one per event or channel), not only logged.

The updater UI: a header banner while an update is available, downloading, ready,
counting down (not dismissible) or installing (not dismissible), or failed; Settings →
Updates with the mode, "Check now" (every mode, Off included), the stale-check warning
and the post-update recovery panel (open logs; reset `zebrad.toml` with `reset_config`
while zebrad is stopped or failed, with the manual steps as a fallback while it runs; the
releases page); and "Check for updates" on the
dashboard's end-of-support banner once the halt is under 14 days away or zebrad halted.
Where `UpdateInfo.can_install` is false (`.deb`, development builds) the UI offers
"Open releases page" instead of Install.

**Spawning** happens on one dedicated, long-lived OS thread (the spawner) with
`std::process::Command`, then `tokio::process::Child::from_std`. It must not be a tokio
blocking-pool thread, because Linux `PR_SET_PDEATHSIG` fires when the spawning *thread*
exits.

**Exit classification.** Reader tasks drain stdout and stderr to EOF before an exit is
classified (`logs::classify_exit`, then `finalize` in `supervisor/actor.rs`). The
deterministic causes are detected from zebrad's own lines and win over the exit code
(zebrad exits 1 for a config error and aborts with 134 for the panics, which alone say
little). `ExitClass` has these variants:

| Class | Set when | Then |
| --- | --- | --- |
| `ConfigError` | stderr `Zebra could not load the provided configuration` | `Failed`, not retried |
| `EosHalt` | panic `Zebra refuses to run` | `Failed`, not retried |
| `DbLocked` | panic `Database likely already open` | `Failed`, not retried |
| `PortInUse` | panic `... already in use` while opening a listener | `Failed`, not retried |
| `SpawnFailed` | the process could not be started, for example because the sidecar is missing | `Failed`, not retried |
| `Crash` | any other exit the launcher did not ask for: non-zero code, signal or panic | `Backoff`: 1 s doubling to 60 s; `Failed{Crash}` after 5 crashes in 10 min |
| `Clean` | exit code 0, or any exit after a stop the launcher requested that has none of the causes above (a kill after the grace period is `Clean` with `detail: "killed by the launcher"`) | `Stopped` |

The end-of-support warning (`Your Zebra release is too old`) is a log event
(`LogEvent::EosWarn`), not an exit class: zebrad keeps running and the UI shows a banner.
There is no disk-full class; a full disk ends the run as whatever zebrad does then,
normally `Crash`. The log lines these depend on are pinned by fixtures captured from real
zebrad; see
[`crates/launcher-core/tests/fixtures/README.md`](../crates/launcher-core/tests/fixtures/README.md).

**Spawn environment.** `zebrad -c <app_config_dir>/zebrad.toml start`. Every inherited
`ZEBRA_*` variable is removed with `env_remove` (never `env_clear`, which would drop
`PATH`, `HOME`, `SystemRoot` and `LOCALAPPDATA`). `ZEBRA_TRACING__USE_COLOR=false` and
`ZEBRA_TRACING__FORCE_USE_COLOR=false` are forced; `ZEBRA_TRACING__PROGRESS_BAR` and
`ZEBRA_TRACING__LOG_FILE` are unset.

## Process control per OS

| | Linux | macOS | Windows |
| --- | --- | --- | --- |
| Graceful stop with `LAUNCHER_STDIN_CLOSE=1` (zebrad started with `--shutdown-on-stdin-close`: fake-zebrad, or a zebrad with the proposed upstream flag) | close stdin | close stdin | close stdin |
| Graceful stop otherwise (the pinned zebrad today) | `SIGTERM` | `SIGTERM` | none: the Job Object is terminated at once |
| Escalation after the grace period (default 60 s) | `SIGKILL` to the process group, `wait()` | `SIGKILL` to the process group, `wait()` | `TerminateJobObject`, wait |
| Launcher exits | the exit flow stops zebrad first; `PR_SET_PDEATHSIG(SIGTERM)` covers a crash | the exit flow stops zebrad first; after a crash zebrad runs on until the next launch finds it through the pidfile | `KILL_ON_JOB_CLOSE` on the per-zebrad Job Object |
| Stale zebrad on start | pidfile | pidfile | pidfile |

- **Stdin-close shutdown** is a proposed upstream, opt-in zebrad flag: zebrad shuts down
  when its stdin reaches EOF and logs `stdin closed, starting shutdown` (target
  `zebrad::signal`). It is one mechanism on every OS, and a launcher crash closes the
  pipe too. Windows needs it because zebrad there only handles Ctrl-C, which a
  console-less GUI parent cannot deliver reliably. The pinned zebrad does not have the
  flag yet, so the launcher passes it only when `LAUNCHER_STDIN_CLOSE=1` is set
  ([DEVELOPMENT.md](DEVELOPMENT.md#run-the-app)); fake-zebrad implements it. Until then a
  stop on Windows is a hard stop: zebrad is terminated without running its shutdown (no
  `stopping zebrad` lines), and the UI does not label it differently.
- **Windows Job Object:** one per zebrad, created with `CREATE_SUSPENDED` → assign →
  resume, as `zebra-test`'s `process_group/windows.rs` does. The launcher itself is never
  in a job (that would kill the NSIS updater and browsers it opened). `KILL_ON_JOB_CLOSE`
  is always set today (`ZebradCommand::kill_on_launcher_exit`); once the pinned zebrad has
  stdin-close it can be dropped, so a launcher crash lets zebrad shut down through stdin
  EOF while the job stays for escalation. `CREATE_NO_WINDOW` is kept.
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

**Configuration form.** The Configure page shows every key of the bundled zebrad's
`zebrad.toml` as a form beside the file itself (`ui/src/config_form.rs`, schema in
`ui/src/config_form/schema.rs`: section, kind,
default, control and a plain-words explanation for the tooltip, derived from zebrad's doc
comments at the pinned commit). The form reads the file with `toml_edit`, shows a field's
value or its default, validates per kind (bounds, loopback addresses, humantime durations,
peer lists) and renders only the changed keys back into the same document, so comments and
unknown keys survive and a field reset to its default is removed from the file. Managed
keys and the keys the launcher forces through environment variables are read-only. The form
and the text view edit one draft; saving goes through `apply_config`, with conflict
detection and rollback. A unit test pins the schema to the `zebrad generate`
fixture, so a zebrad bump that adds, removes or re-defaults a key fails `cargo test -p ui`
until the schema is updated.

The managed keys are launcher-owned: the supervisor and the RPC monitor rely on them.
`config::ensure` rewrites them at every start, and `config::begin_apply_with_managed`
overrides any edit to them before saving, so the editor cannot change them; they change
only through the launcher (the network and the data directory chosen at first run or in
Settings).

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

`reset_config` (Configure → "Reset to managed defaults", and the post-update recovery
panel) replaces the file with the minimal one `config::ensure` creates, keeping the old
file as the same `zebrad.toml.bak`, and returns the new `ConfigDocument`. It runs only
while zebrad is `Stopped` or `Failed`, holding the data-change gate (see below) so zebrad
cannot start meanwhile, and refuses while an apply is in flight or its marker is pending.

A bundled zebrad version change is not special-cased: the next start runs the existing
file against the new zebrad, and a key the new version rejects fails that start as
`ConfigError`, for the user to fix in Configure. `PortInUse` is not remediated either:
zebrad fails, the launcher does not retry, and the RPC and metrics ports cannot be moved in
the launcher, so the user frees the port
([TROUBLESHOOTING.md](TROUBLESHOOTING.md#a-port-is-already-in-use)). The RPC monitor polls
the address zebrad logged as opened (`Status.rpc_addr`), which is the managed one.

Launcher settings live in `launcher.json` (with `schema_version`), owned by a settings
actor. `set_settings` saves everything the UI sends except `first_run_done` and
`data_dir`, which it keeps as stored: only `complete_first_run` and `choose_data_dir`
change them, so a stale UI copy cannot undo a data directory choice or skip the checks
`choose_data_dir` makes. A `launcher.json` older than schema version 2 (or without a
`schema_version`) predates the first-run flow: `SettingsStore::open` sets
`first_run_done`, bumps the version and writes it back, so upgrading users keep their
autostart. Without a file, `first_run_done` starts false.

## First run and node data

`launcher-core/src/firstrun.rs` holds the Tauri-free logic, tested with temporary
directories; the commands in `src-tauri/src/commands.rs` call it off the async runtime.

| Command | Returns | Does |
| --- | --- | --- |
| `get_first_run_info` | `FirstRunInfo` | first run until `settings.first_run_done`; the default data dir and its free space (`statvfs` / `GetDiskFreeSpaceExW`, at the nearest existing ancestor); alpha.4 leftovers with the state size from a walk capped at 50k entries (then a lower bound), done once per session |
| `pick_data_dir` | `Option<String>` | the native folder picker |
| `choose_data_dir(path)` | `DataDirChoice` | checks `path` is absolute and writable, saves `settings.data_dir`, rewrites `state.cache_dir`; returns the free space, the UI decides whether to warn |
| `free_space(path)` | `Option<u64>` | read-only: the free space at `path` or its nearest existing ancestor; `None` for a relative path, when nothing of it exists, or when it cannot be read. Never creates or saves anything; the wizard and Settings use it for a folder chosen earlier |
| `import_legacy_state` | `ImportOutcome` | renames `dirs::cache_dir()/zebra/state` into the current state dir; across volumes it fails and asks the user to move it by hand, never copying |
| `delete_legacy_state` | `u64` (bytes freed) | removes `dirs::cache_dir()/zebra/state` only (the peer cache stays), with the rules of `delete_node_data` and the zebra cache dir as the only root; refuses when the launcher's own state dir is inside that cache dir |
| `delete_node_data` | `u64` (bytes freed) | removes `<data dir>/state` only, and only inside the launcher's data dir or the chosen one; the bytes come from a size measured earlier in the session, 0 (unknown) otherwise, never from a walk of the whole state |
| `copy_diagnostics` | `String` | versions, OS, status, settings (URL credentials redacted), `zebrad.toml`, last 200 lines of each log; never the RPC cookie |
| `complete_first_run` | `()` | sets `first_run_done`; from then on zebrad starts at launch |

zebrad is not started before the first run is complete: `firstrun::start_allowed` is the
one rule, checked at launch, by the tray (whose Start item stays disabled until then) and
by `start_zebrad`, `start_zebrad_killing_stale` and `restart_zebrad`. The data commands run
only while zebrad is `Stopped` or `Failed`. A semaphore in `AppState` keeps them apart
from starts: each start (UI, tray, launch, the restart inside `apply_config`) holds one
permit until it finishes, and a data change holds all of them, so zebrad cannot start
while its state is being moved or deleted.

## Window, tray and exit

```text
tray menu: Show | Start zebrad | Stop zebrad | Quit      tooltip: phase (watch<Status>)
window close --(close_to_tray && tray exists)--> hide; zebrad keeps running
            '--(otherwise)--> last window closed -> ExitRequested
Quit / ExitRequested -> prevent_exit -> supervisor.shutdown() capped at max(grace + 10 s, 70 s) -> exit
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
  dialog names the error and the log directory; dismissing it exits with code 1, and so
  does a dialog that could not be shown or got no answer within 60 s. A
  `zebrad.toml` that cannot be updated is not fatal: zebrad reports it and the editor can
  fix it.

## Status and metrics

`rpc.rs` and `monitor.rs` poll zebrad's cookie-authenticated RPC every 5 s
(`getblockchaininfo`, `getinfo`, `getmempoolinfo`, `getpeerinfo` count; hourly
`getdeprecationinfo` on Mainnet only) and publish `watch<NodeStats>`. That feeds the
status strip and the dashboard, the primary metrics view. Grafana is optional and run by
the user, embedded in its own tab; see [GRAFANA.md](GRAFANA.md).

**Dashboard.** The UI keeps every published `NodeStats` in `ui/src/stats_history.rs`
(up to 4 h of 5 s samples, since the launcher opened; nothing is persisted). From it the
dashboard draws a sync meter with the block rate and the time to the tip, stat tiles
with sparklines, and inline SVG line charts of height, peers and mempool size
(`ui/src/charts.rs`, no chart library) over a range the user picks (15 min to all).
Every chart has a hover crosshair, keyboard navigation and a table view.

**Log analysis.** `ui/src/insights.rs` folds every log batch, in `seq` order, into a
summary of the run: the sync stage from zebrad's `sync::progress` lines (syncing, near
the tip, at the tip, slow, stalled, waiting for genesis), deduplicated warnings, errors
and stderr lines with counts, lines per minute by level over the last hour, and the
startup milestones of the current run (state opened, peers, RPC, first progress report)
with the peer, checkpoint and mempool facts. The Logs page shows these cards in a
collapsible analysis panel and the dashboard shows the stage, the recent problems and
the activity chart; picking a problem filters the Logs page to it. The Logs tab carries
a count of problems that arrived while the page was not shown.

**Appearance.** `Settings.theme` (`system`, `light`, `dark`) is resolved against the
system preference in `ui/src/theme.rs` and applied as `data-theme` on `<html>`; every
colour in the stylesheets is a token, and the embedded Grafana follows the same theme.

On Mainnet, zebrad warns 42 days and halts 56 days after its estimated release height,
so an install that never updates stops working. The UI shows an EOS countdown, a warning
banner, and a blocking halt card with no restart loop.

## Updater (opt-in)

Modes `Off`, `Notify`, `Auto` (`settings.update_mode`); Off until the first-run choice,
which preselects Notify.

- **Off** makes zero network requests: nothing is scheduled and no plugin `Updater` is
  built. Only an explicit "Check for updates" (`check_for_update`) reaches the network.
- **Notify** checks 30 s after launch, then every 6 h ± 10 % (seeded jitter), and shows
  what it found. Installing is always a user click.
- **Auto** also downloads a found update in the background. It installs
  only on a user click, or by itself when zebrad's end-of-support warning is on (the
  estimated halt is within 14 days, or zebrad already refused to run), and then only
  after a visible 60 s countdown that `cancel_update` stops. Never silently mid-session.
- The `.deb` cannot update itself (root, package manager): it is Notify-only and the UI
  links to the releases page (`UpdateInfo.can_install = false`). Development builds
  cannot install either; on macOS only a release build running from inside a `.app`
  bundle can (`bundle_type()` reports `App` for every macOS build).
- A check times out after 30 s; a download after 1 h (the plugin copies the check's
  timeout into the `Update`, so the backend replaces it before downloading). A download
  can be cancelled sooner.

### Updater actor

The policy and the flow are Tauri-free, in `launcher-core::updates`: pure policy
functions (schedule, auto-install rule, stale-check warning) and an actor that applies
them. `src-tauri/src/updater.rs` implements the actor's `UpdateBackend` with
`tauri-plugin-updater` and forwards its state to the UI.

```text
  commands ──mpsc<Cmd>──▶ updater actor ──▶ watch<UpdateInfo> ──▶ events update_info,
  watch<Settings> ──────▶     │  ▲                                  update_state
  watch<NodeStats> (EOS) ─▶   │  │ results (job id)
  watch<Status> ────────▶     ▼  │
                         child tasks: check | download | install
                                    │
                    UpdateBackend (tauri-plugin-updater)   NodeControl (supervisor)
```

States (`UpdateState`, tagged `state`):

```text
Idle ──check──▶ Checking ──▶ UpToDate | Error | Available
Available ──install (or Auto)──▶ Downloading ──done──▶ ReadyToInstall{countdown: None}
ReadyToInstall ──Auto + EOS warning──▶ ReadyToInstall{countdown: Some(60..1)} ──0──▶ Installing
ReadyToInstall ──install──▶ Installing ──▶ (launcher restarts)
cancel: Checking → the found update or the state before the check (UpToDate, Error, Idle),
        Downloading → Available, countdown → ReadyToInstall{None}
update_mode → Off: abort everything except an install in progress → Idle
```

Install sequence, in a child task:

```text
Update::verify (minisign, signed version; spawn_blocking)  -- fails: Error, zebrad untouched
  -> supervisor stop (graceful, kill after the grace period), capped at
     supervisor::shutdown_cap = max(grace + 10 s, 70 s), the exit flow's cap
  -> write <config>/pending_update_check {from, to, zebrad_was_running}
  -> Update::install -> app.request_restart()  (Windows: the installer exits the app)
stop fails -> start zebrad again if it was running, Error
stop exceeds the cap -> keep waiting for it (up to the cap again), then start zebrad
                        again if it was running (never on top of a stop in flight), Error
install fails -> remove the marker, start zebrad again if it was running, Error
```

The launcher verifies the package once, right before stopping zebrad; the plugin also
verifies inside `download` and `install`. The marker is written only after zebrad
stopped, so a crash or failed stop before the install leaves no marker behind.

At the next launch, if `pending_update_check` exists, the health gate checks that the
new version is running and, if zebrad was running before, that it reaches `Running`
with an RPC address within 5 minutes (starting it if `start_node_on_launch` is off).
Then the marker is removed. On failure the `update_health_failed` event (and
`UpdateInfo.health_failure`) carries a message and recovery hints:

| Outcome | Recovery |
|---|---|
| still the old version: the update did not complete | open logs, releases page |
| zebrad `Failed { EosHalt }`: past its end of support, update required | releases page |
| zebrad failed otherwise, or not `Running` with RPC in 5 minutes | open logs, reset config, releases page |
| a stale zebrad (`Status.stale`) still blocks the start at the limit; names its pid and binary | open logs |

A stale zebrad from an earlier launcher run is not an update failure: the gate's start
(and the restart after a failed install) emits `stale_process` like the start at
launch, the gate waits for the user to resolve it, and the 5 minutes start again once
it is gone. When the update did not complete, zebrad is still started again if it was
running and `start_node_on_launch` is off.

`UpdateInfo.stale_check_warning` is set when the end of support is within 28 days (or
zebrad halted) and no check succeeded in the last 14 days; the last successful check
is kept in `<config>/updater.json`.

- Endpoint: `https://github.com/ZcashFoundation/zebra-launcher/releases/latest/download/latest.json`,
  the latest non-prerelease release, from `plugins.updater.endpoints`. Release
  candidates are rehearsed with a build-time endpoint override (`TAURI_CONFIG` or
  `--config`, see [RELEASING.md](RELEASING.md#update-rehearsal)), never a runtime
  environment variable; only debug builds read `LAUNCHER_UPDATE_ENDPOINT`.
- `allowDowngrades = false` and `requireSignedVersion = true`; roll forward only. What
  `verify` checks is in [SECURITY.md](SECURITY.md#what-the-updater-verifies).
- Updatable formats: AppImage, NSIS per-user, `.app.tar.gz` per macOS arch. The plugin's
  `zip` feature is off, so zipped NSIS and gzipped AppImage updates are refused.

## Release pipeline

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

CI ([`.github/workflows/ci.yml`](../.github/workflows/ci.yml)) runs, on Linux,
`scripts/check-versions.sh`, fmt, clippy with the mutex ban, the tests, the UI crate's
clippy for `wasm32-unknown-unknown` and `trunk build --release`, `cargo deny`, and
shellcheck and actionlint over the scripts and workflows; clippy and tests on
`windows-2025` and `macos-15`; and, on pushes to `main` and on pull requests that change
the zebrad pin, the real-zebrad contract tests against the verified release binary.
`bundle.yml` builds unsigned bundles with the real zebrad for all five targets.

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

The full plan these decisions come from is [PLAN.md](PLAN.md).
