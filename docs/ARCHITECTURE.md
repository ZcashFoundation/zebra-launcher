# Architecture

Zeal is a Tauri 2 desktop app that supervises one or more Zcash nodes (for now up to
eight zebrad nodes, each running the bundled `zebrad`), streams their logs, edits their
config, and shows their status. A small Rust team must be able to maintain it for years,
so the rules are: Rust only (no Node toolchain), a thin Tauri layer over testable
crates, and no mutexes.

This document describes the launcher as built. The plan it grew from is [PLAN.md](PLAN.md),
kept as the historical design record; where the two differ, this file describes the code.

## Overview

```text
+-------------------------------- Zeal (one process) --------------------------------+
|                                                                                    |
|  ui/  Leptos CSR (wasm) --- invoke / Channel<Vec<LogLine>> ---> src-tauri/         |
|   |   Dashboard, Logs, Grafana, Configure, Settings              commands, tray,   |
|   |                                                              updater, exit     |
|   +-- <iframe> Grafana (no IPC)                                      |             |
|                                                                      v             |
|  crates/launcher-core  (no Tauri)                                                  |
|    node registry: the node list, settings actor, one runtime per node:             |
|      supervisor actor  <-- mpsc<Command>     watch<Status> -->                     |
|        | log ring + broadcast<LogLine>       config, RPC monitor, metrics poller   |
|        v                                                                           |
|      spawner thread (std::process::Command, PDEATHSIG / Job Object)                |
+--------|---------------------------------------------------------------------------+
         | stdin pipe (closed = stop), stdout/stderr drained
         v
   zebrad (sidecar "zeal-zebrad"), one process per node; the first node's ports:
       RPC      127.0.0.1:8232  <-- cookie-auth JSON-RPC poll every 5 s (status strip)
       metrics  127.0.0.1:9999  <-- Prometheus <-- Grafana 127.0.0.1:3000 (optional)
       P2P      [::]:8233
```

## Crates

| Path | Role |
| --- | --- |
| `crates/launcher-types` | serde types shared by backend and UI (`Status`, `LogLine`, `Settings`, `NodeStats`, errors). Builds natively and to wasm; no tokio, no Tauri. |
| `crates/launcher-core` | Logic that does not need Tauri: the node registry and node specs, supervisor actor, process control (unix/windows), log parsing and classifiers, log file, pidfile, restart policy, config, settings, RPC monitor, resource samplers, paths, end-of-support, install kind. |
| `crates/launcher-core/src/bin/fake-zebrad.rs` | Scripted stand-in for zebrad, used by tests and PR bundle builds. |
| `src-tauri` | Thin Tauri 2.12 app: commands, the `Channel` bridge, tray, updater, exit flow, capabilities. |
| `ui` | Leptos CSR crate built by trunk into `ui/dist`. Talks to Tauri through about 100 lines of hand-written `wasm_bindgen` externs to `window.__TAURI__`. |

Single sources of truth: `[workspace.package] version` in [`Cargo.toml`](../Cargo.toml)
for the launcher version, [`ZEBRAD_VERSION`](../ZEBRAD_VERSION) for the bundled zebrad,
and [`rust-toolchain.toml`](../rust-toolchain.toml) for the toolchain.

## Nodes

Zeal runs a list of nodes, each a zebrad with its own files, ports, configuration, log
and restart policy ([MULTI-NODE.md](MULTI-NODE.md) is the design, phase 2 the part
built so far). The node registry (`crates/launcher-core/src/registry.rs`) is an actor
that owns the list and a runtime per node: the node's supervisor, RPC monitor, metrics
poller, gates (the semaphores below, per node) and log subscription counter, created
from the node's spec (`NodeSpec`, `crates/launcher-core/src/nodes.rs`).

```text
  RegistryHandle ──Cmd──▶ registry actor ──▶ watch<Arc<Vec<NodeHandle>>>  (lookups by id)
       │                    │   ▲        ──▶ watch<Vec<NodeSummary>>      (the list, phases)
       │                    │   └── phase changes, finished removals
       │                    ▼
       │          settings actor: launcher.json, settings and node list
       ▼
  NodeHandle ──▶ the node's supervisor, RPC monitor, metrics poller and gates
```

- **Commands by id.** Callers look a node up by id in the published handles (no round
  trip to the actor) and act on its own actors: start, stop, configure, snapshots, logs.
  The actor only changes the list and saves it: `add`, `update`, `choose_data_dir`,
  `remove`. Long work (a removal that stops the node and deletes its state, the shutdown
  at quit) runs in a task that reports back, so it never holds up the others.
- **Summaries.** A small task per node forwards its phase changes, and the actor
  publishes each node's `NodeInfo` with its phase, which the tray and the `nodes` event
  follow.
- **Files.** The first node, migrated from a single-node launcher or created at the first
  run, keeps the launcher's own locations (`NodeLayout::Launcher`: `zebrad.toml` in the
  config directory, pid file, cookie and state in the data directory, `zebrad.log` in the
  log directory). Every node added later has a folder of its own, `<data>/nodes/<id>`
  (`NodeLayout::Folder`), with its `zebrad.toml` (and its `.bak` and `.pending`), pid file,
  `rpc/` cookie, `zebrad.log` and, unless another data folder is chosen, `state/`.
- **Adding** (`plan_new_node`): at most eight nodes (`MAX_NODES`); zebrad only for now
  (`NodeKind::addable`), running the bundled zebrad; a name no other node has; an id no
  other node has and that no removed node's kept folder uses (the backend picks the first
  free one, `zebrad-testnet`, `zebrad-mainnet-2`, when the draft leaves it out); ports no
  other node has and no other program holds (assigned from the kind's defaults with
  `assign_ports` around every node's ports, then around ports in use, when the draft
  leaves them out); and a data folder that is not another node's and whose state does not
  overlap another node's. The node's folder and minimal `zebrad.toml` are written before
  the list is saved, and removed again if the save fails.
- **Changing** (`plan_update`): a name, start at launch, stop grace (next launch), or
  network. A new network moves the ports to that network's defaults around the other
  nodes', renames a default name ("Zebra · Testnet"), and rewrites the managed keys at
  once (refused while a configuration change is being applied); it takes effect at the
  node's next start. Ids are stable, except while the first run is not done: then the
  first node takes its network's id (`zebrad-testnet`), as nothing ran or depends on it
  yet, and the registry publishes it as a new handle.
- **Removing**: refused for the last node. The removal takes all of the node's start
  permits and its config permit (no start or apply can begin), stops it, deletes its
  chain state with `delete_node_data`'s rules when asked (and then the rest of a folder
  node's own folder, once its supervisor has closed the log file), and only then takes it
  off the list. Kept data stays where it is, and its id is not given to a new node while
  its folder exists.
- **Ports at every start.** The supervisor tries binding each of the node's addresses
  (P2P on `0.0.0.0`, RPC and metrics on loopback) before it spawns zebrad: one that
  another program holds fails the start as `PortInUse`, naming the port, before zebrad
  runs; zebrad's own report of a port in use stays the backstop. The addresses come from a watch, so a network change needs no new supervisor (a
  new one would restart the log's sequence numbers, which the UI merges by).
- **Launcher lines** go to the node they are about: an event with a `node` field, or
  inside a tracing span that has one (each node command that changes something runs in
  one, blocking work and a removal's task included), goes to that node's log; any other
  to every node's.
- **The updater** sees the nodes as one: `herd_phase` folds their phases (halted at the
  end of support when any node is, running while any is active), the end-of-support estimate
  comes from the first Mainnet zebrad node, and a stop for an update stops every node
  that runs and remembers which, to start those again.

## Supervisor

Each node has one actor task that owns its zebrad child. Nothing else touches it.

| Channel | Direction | Carries |
| --- | --- | --- |
| `mpsc<Cmd>` | in | `Start{options}`, `Stop`, `Shutdown`, `Subscribe`, `History{since}`, `Note` (a launcher line for the ring, sent with `try_send`) (`crates/launcher-core/src/supervisor.rs`). `SupervisorHandle::restart` is a `Stop` followed by a `Start`; applying a config is the `apply_config` Tauri command, built on the same handle. |
| `watch<Vec<Listener>>` | in | the addresses zebrad will listen on, tried before every spawn (`SupervisorConfig::listeners`) |
| `watch<Status>` | out | `phase` (`Stopped`, `Starting`, `Running`, `Stopping`, `Backoff{until, crashes}`, `Failed{class}`), `pid`, `started_at`, the `rpc_addr` and `metrics_addr` zebrad logged as opened, `last_exit` (`ExitInfo{code, class, detail, requested}`), `stale`, `restarts` (automatic restarts this launcher session) and `last_stop` (`StopReport{requested_at, exited_at, forced}` for the last requested stop) (`crates/launcher-types/src/lib.rs`) |
| `watch<Option<TreeProbe>>` | out | the current run's process tree for the resource sampler (`SupervisorHandle::watch_probe`): run id, pid, and the spawn on the wall and monotonic clocks, read once in `install_run` with `Status.started_at`. `Some` from the spawn until `finalize`, after the exit drain |
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
except `apply_config`, which rejects with an `ApplyError {kind, message}`. Every command
about one node takes its id as `node`: `start_node`, `start_node_killing_stale`,
`stop_node`, `restart_node`, `read_config`, `apply_config`, `reset_config`, the snapshot
commands `get_status`, `get_node_stats`, `get_node_details` and `get_metrics`,
`subscribe_logs`, `log_history`, `choose_data_dir`, `delete_node_data`, and `open_path`
for a node's folders and metrics page. `list_nodes`, `add_node` (a `NodeDraft`),
`update_node` (a `NodeUpdate`) and `remove_node` manage the list.

Events carry changes the UI did not ask for: `nodes` (the list, a `Vec<NodeInfo>`,
whenever a node's info changes), and for each node, as a `NodeEvent {node, value}`,
`status` (every `Status` change), `node_stats` (every `Option<NodeStats>` change),
`node_details` (every `Option<NodeDetails>` change: the details behind the stats, from
the same RPC poll), `metrics` (every `Option<MetricsSnapshot>` change: zebrad's
Prometheus page, read every 10 s while it runs) and `stale_process` (a zebrad from an
earlier launcher run blocked the start at launch). The bridge starts a node's forwarders
when it appears and stops them when it leaves (by handle serial, so a node that gets a
new id during the first run gets new ones). The resources carry no node id:
`node_resources` (every `Option<NodeResources>` change of the first node: its process
tree, read every 5 s from the spawn until the run is finalized) and `machine_resources`
(every `Option<MachineResources>` change: the computer's CPU and memory, every 5 s), with
the snapshots `get_node_resources` and `get_machine_resources`. For the updater there are
`update_info` (every `UpdateInfo` change), `update_state` (its `UpdateState`, when that
changed) and `update_health_failed` (an `UpdateHealthFailure` after an update that did not
come back healthy), with the snapshot `get_update_info`.

At startup the UI subscribes to every event first and then fetches the node list
(`list_nodes`), `get_node_resources`, `get_machine_resources` and `get_update_info`, so no
change falls between the two; each node, once
the list brings it, fetches its own snapshots (`get_status`, `get_node_stats`,
`get_node_details`, `get_metrics`) and subscribes to its log, which streams the retained
history before live lines, so logs need no snapshot request. An event for a node the list
has not brought yet is dropped; that node's snapshots cover it.
An event that arrives while a snapshot request is in flight is newer than the snapshot,
so the UI drops a node's snapshot once an event of the same kind has arrived for that
node, the list snapshot once a `nodes` event has, and the resource snapshots once an event
of their kind has (for the updater: `update_info`; an `update_state` that arrives before
any `update_info` replaces the snapshot's state).
Log batches are merged by `seq`, so their order does not matter.
`stale_process` may be emitted before the UI listens, so the stale-process banner is
driven by `status.stale`, which the snapshot carries; likewise `update_health_failed`
is also in `UpdateInfo.health_failure`. A payload that does not deserialize is shown in
the UI's error banner (the first one per event or channel), not only logged.

**Launcher lines in the log stream.** The launcher's own decisions appear in the same
stream as zebrad's output (of the node they are about, see [Nodes](#nodes)), as
`LogLine`s with `stream: launcher`: the supervisor's notes
(`zebrad started (pid N)`, `stop requested (stdin closed)`, `restarting zebrad in 4s`,
`not restarting zebrad (ConfigError)`) and every `tracing` event at INFO or louder from
the rest of the launcher (a configuration applied or rolled back, an update check, a
stale process handled, startup), which the layer in `src-tauri/src/logging.rs` forwards
through `RegistryHandle::note` to the node's `SupervisorHandle::note`. Such a line has a `timestamp` from the launcher's clock
in zebrad's RFC 3339 UTC format, a `level`, the `tracing` `target`
(`launcher_core::supervisor` for the supervisor's notes, `zeal::updater` and so
on for the rest) and the `message` with the event's fields appended as `key=value`;
`spans` is empty. Forwarding never blocks a logging call: the layer puts the event on a
bounded queue with `try_send` and the queue feeds the actor the same way, so a flood
drops the newest launcher lines instead of stalling. Targets that log per batch
forwarded to the UI (`zeal::bridge`) are left out, or each line would log
another. The launcher log file on disk is unchanged (it keeps everything its `RUST_LOG`
filter admits); the zebrad log file gets the launcher lines in zebrad's line format
behind its `[launcher] ` marker. A message of several lines (a TOML parse error, a panic)
is one stamped line followed by continuation lines, each behind the marker on disk and
without timestamp, level or target in the stream, as zebrad's own multi-line messages are.

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
| `PortInUse` | panic `... already in use` while opening a listener, or the check before the spawn found one of the node's ports held | `Failed`, not retried |
| `SpawnFailed` | the process could not be started, for example because the sidecar is missing | `Failed`, not retried |
| `Crash` | any other exit the launcher did not ask for: non-zero code, signal or panic | `Backoff`: 1 s doubling to 60 s; `Failed{Crash}` after 5 crashes in 10 min |
| `Clean` | exit code 0, or a kill by the launcher after the grace period (`detail: "killed by the launcher"`); a non-zero exit or panic during a requested stop keeps its own class and is still not restarted | `Stopped` |

The end-of-support warning (`Your Zebra release is too old`) is a log event
(`LogEvent::EosWarn`), not an exit class: zebrad keeps running and the UI shows a banner.
There is no disk-full class; a full disk ends the run as whatever zebrad does then,
normally `Crash`. The log lines these depend on are pinned by fixtures captured from real
zebrad; see
[`crates/launcher-core/tests/fixtures/README.md`](../crates/launcher-core/tests/fixtures/README.md).

**Spawn environment.** `zebrad -c <the node's zebrad.toml> start`. Every inherited
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
| Process tree for resource monitoring ([RESOURCES.md](RESOURCES.md)) | from a `/proc` scan every 5 s: the root (checked once against the spawn time), its process group, children of members by parent link (started no earlier than the parent) and members seen before; read parent first | not measured yet: the card stays hidden | not measured yet (the job object will hold the tree) |
| CPU, memory, disk I/O and counts of the tree | `stat` (utime + stime, and cutime + cstime of the reaped children), `status` (VmRSS, VmHWM), `io` (read_bytes, write_bytes), the entries of `fd`; the computer from `/proc/stat` and `/proc/meminfo` | — | — |

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

The launcher writes a **minimal** `zebrad.toml` for each node: the keys it manages plus
the user's edits, never full `zebrad generate` output. Every zebrad section is
`deny_unknown_fields`, so a stale defaulted key would break the next zebrad bump. The
managed keys carry the node's own network, ports, cookie and state folder, so two nodes
never share a port, a folder or a cookie (`NodeSpec::managed_keys`).

| Managed key | Value |
| --- | --- |
| `network.network` | the node's network, chosen at first run for the first node |
| the port of `network.listen_addr` | the node's P2P port (8233, or 18233 on Testnet, for a single node); the address stays as the user wrote it, and the key is written only when the port differs from the one zebrad uses without it, so a file that never set it is left as it is |
| `rpc.listen_addr` | `127.0.0.1:<the node's RPC port>`: `8232` (`18232` on Testnet) for a single node |
| `rpc.enable_cookie_auth` | `true` |
| `rpc.cookie_dir` | the node's `rpc` folder: `<app_local_data_dir>/rpc` for the first node |
| `metrics.endpoint_addr` | `127.0.0.1:<the node's metrics port>`: `9999` for a single node |
| `state.cache_dir` | `<the node's data folder>/state` |
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
`config::ensure` brings them up to date for every node at launch (and when the node's
network, ports or data folder change), and `config::begin_apply_with_managed` overrides
any edit to them before saving, so the editor cannot change them; they change only
through the launcher (the network and the data folder chosen at first run or in
Settings, the ports given to a node when it is added).

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
the start fails, the launcher does not retry, and a node's ports cannot be moved in the
launcher yet (except with its network), so the user frees the port
([TROUBLESHOOTING.md](TROUBLESHOOTING.md#a-port-is-already-in-use)). The RPC monitor polls
the address zebrad logged as opened (`Status.rpc_addr`), which is the managed one.

Launcher settings live in `launcher.json` (schema version 3), owned by a settings actor
that is the file's only writer: the launcher's own settings (updates, close to tray,
Grafana, appearance, the first-run marker) at the top level and the nodes in a `nodes`
list, which the registry saves through it (`SettingsHandle::save_nodes`; a list that
breaks the rules of `nodes::check_list`, such as a repeated id, is refused, and a file
with one counts as corrupt). `set_settings` saves everything the UI sends except
`first_run_done`, which it keeps as stored: only `complete_first_run` sets it. A
`launcher.json` older than schema version 2 (or without a `schema_version`) predates the
first-run flow: `SettingsStore::open` sets `first_run_done`, so upgrading users keep
their autostart. A version 2 file ran one node: its `network`, `data_dir`,
`start_node_on_launch` and `stop_grace_secs` become the first node, `zebrad-<network>`,
in the launcher's own locations and with the ports it has always used (the P2P one as its
`zebrad.toml` gives it), so its managed keys are the ones that file already has and
nothing moves or is rewritten. A migrated file is written back. Without a file,
`first_run_done` starts false and the list holds the first node on Mainnet, which the
first run may move to Testnet.

## First run and node data

`launcher-core/src/firstrun.rs` holds the Tauri-free logic, tested with temporary
directories; the commands in `src-tauri/src/commands.rs` call it off the async runtime.

| Command | Returns | Does |
| --- | --- | --- |
| `get_first_run_info` | `FirstRunInfo` | first run until `settings.first_run_done`; the default data dir and its free space (`statvfs` / `GetDiskFreeSpaceExW`, at the nearest existing ancestor); alpha.4 leftovers with the state size from a walk capped at 50k entries (then a lower bound), done once per session |
| `pick_data_dir` | `Option<String>` | the native folder picker |
| `choose_data_dir(node, path)` | `NodeDataDirChoice` | checks `path` is absolute and writable and not another node's, saves it as the node's data folder (its default restores the default), rewrites its `state.cache_dir`; returns the node and the free space, the UI decides whether to warn |
| `free_space(path)` | `Option<u64>` | read-only: the free space at `path` or its nearest existing ancestor; `None` for a relative path, when nothing of it exists, or when it cannot be read. Never creates or saves anything; the wizard and Settings use it for a folder chosen earlier |
| `import_legacy_state` | `ImportOutcome` | renames `dirs::cache_dir()/zebra/state` into the first node's state dir; across volumes it fails and asks the user to move it by hand, never copying |
| `delete_legacy_state` | `u64` (bytes freed) | removes `dirs::cache_dir()/zebra/state` only (the peer cache stays), with the rules of `delete_node_data` and the zebra cache dir as the only root; refuses when a node's own state dir is inside that cache dir |
| `delete_node_data(node)` | `u64` (bytes freed) | removes the node's `<data folder>/state` only, and only inside the launcher's data dir or the node's data folder; the bytes come from a size measured earlier in the session, 0 (unknown) otherwise, never from a walk of the whole state |
| `copy_diagnostics` | `String` | versions, OS, the resolved zebrad binary, settings (URL credentials redacted), the updater record and pending-update marker, the launcher log, and for every node its spec, the stop grace in effect and in its settings, status and any stale zebrad, node statistics, `zebrad.toml` and the last 200 lines of its log, then the newest resource samples of the first node, with one line per process, and of the computer; never the RPC cookie |
| `complete_first_run` | `()` | sets `first_run_done`; from then on the nodes that start at launch do |

No node is started before the first run is complete: `firstrun::start_allowed` is the
one rule, checked at launch, by the tray (whose Start item stays disabled until then) and
by every start (`NodeHandle::start`: `start_node`, `start_node_killing_stale`,
`restart_node`). The data commands run only while their node is `Stopped` or `Failed`.
Each node's gates (`launcher_core::gates::NodeGates`) keep them apart from starts: each
start (UI, tray, launch, the restart inside `apply_config`) holds one permit until it
finishes, and a data change holds all of them, so zebrad cannot start while its state is
being moved or deleted. A removal holds all of them whatever the phase, so nothing starts
the node while it is stopped and removed.

## Window, tray and exit

```text
tray menu: Show | Start zebrad | Stop zebrad | Quit      tooltip: "Zeal: zebrad running"
  (several nodes: Show | Start all | Stop all | Quit     tooltip: "Zeal: 2 running, 1 stopped")
window close --(close_to_tray && tray exists)--> hide; the nodes keep running
            '--(otherwise)--> last window closed -> ExitRequested
Quit / ExitRequested -> prevent_exit -> registry.shutdown(): every node side by side, each
  within its own stop grace, capped at the longest max(grace + 10 s, 70 s) -> exit
Show / second launch (single-instance) / macOS dock -> show + focus the window
```

- The tray is built in `setup` (`src-tauri/src/tray.rs`) and follows the registry's
  summaries: Start (all) starts every node that is stopped or failed, Stop (all) stops
  every node that runs; with one node the items and the tooltip read as they always did.
  Submenus per node come later. On Linux it is
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

Each node has its own RPC monitor and metrics poller, which follow its supervisor's
status. `rpc.rs` and `monitor.rs` poll zebrad's cookie-authenticated RPC every 5 s
(`getblockchaininfo`, `getinfo`, `getmempoolinfo`, `getpeerinfo`; hourly
`getdeprecationinfo` on Mainnet only) and publish `watch<Option<NodeStats>>` and, from
the same calls, `watch<Option<NodeDetails>>`: the peer list (address, direction, user
agent, protocol version, state, last seen, ping), the best block hash, headers,
difficulty, size on disk, the chain supply and value pools in ZEC, the active network
upgrade, zebrad's user agent and protocol version, and the mempool's bytes and memory.
`metrics.rs` parses the Prometheus text exposition format and `metrics/poller.rs` reads
zebrad's `/metrics` page every 10 s while it runs, publishing
`watch<Option<MetricsSnapshot>>` with the families and samples as exported
(`zcash_net_peers`; zebrad registers dotted names and its exporter writes underscores);
the snapshot is `None` while zebrad is not running and after three failed reads in a
row. That feeds the status strip and the dashboard, the primary metrics view;
`OpenTarget::MetricsEndpoint` opens the raw page in the browser. Grafana is optional and
run by the user, embedded in its own tab; see [GRAFANA.md](GRAFANA.md).

**Resources.** `resources.rs` measures zebrad's process tree and the computer
([RESOURCES.md](RESOURCES.md)). `ResourceSampler` follows a supervisor's probe watch (the
app runs one, on the first node's): from the spawn until the run is finalized it reads the
tree at once and then every 5 s, and publishes `watch<Option<NodeResources>>` (`None`
between runs); unlike the RPC monitor and the metrics poller it does not wait for
`Running`. `MachineSampler` reads the computer's CPU and memory every 5 s into
`watch<Option<MachineResources>>`. Each read is a
`spawn_blocking` task that takes its inputs by value and returns an owned reading, and each
sampler is an actor that owns its state. `resources/fold.rs` keeps the tree's totals
continuous across exits: each live process counts its own CPU and its reaped children's,
and a process that left without reaching a live member's rollup moves its last-seen
counters to a lost total; the published totals never fall within a run. A read that spans
a probe change is dropped, failures are logged once per run, and a reader panic stops
sampling until the next run; each tick logs its read time at debug (about 4 µs per process
on the computer: 0.6 ms for 150 processes, 11 ms for 2,600). Linux reads `/proc`
(`resources/linux.rs`, no `unsafe`); macOS and Windows report the reading unsupported, so
the samplers publish `None` and the dashboard's Resources card stays hidden there.

**Dashboard.** The UI keeps three histories since the launcher opened; nothing is persisted.
`ui/src/stats_history.rs` holds every published `NodeStats` as a `Sample` (up to 4 h of the
monitor's 5 s polls; the peer list of the same poll adds the inbound and outbound counts)
and answers, over a window, the block rate, the ETA to the tip, the last height change and
how long the peer count has been low. `ui/src/metrics_history.rs` holds every
`MetricsSnapshot` reduced to the families the page derives series from (`KEPT_FAMILIES`:
disk and memory bytes, the network byte counters, ready and unready peers, downloads in
flight, the checkpoint height and so on; 4 h of 10 s snapshots), the newest kept whole for
the one-off gauges. `ui/src/resource_history.rs` holds the samples of the node's process tree
(4 h of 5 s samples, the newest minute with their process lists) and answers the one-minute
rates of CPU and disk I/O, each process's rates, the sparklines of the current run and the
last run's summary. Tiles and charts read them over the range the user picks (15 min, 1 h,
4 h, all); the sync hero's rate and ETA use a fixed 15 min window (`HERO_RATE_WINDOW`), so
they do not move with the range control.

**Health verdict.** `ui/src/health.rs` folds the status, the latest statistics, both
histories, the log analysis and the updater state (`HealthInput`) into one `Verdict`: a
level (healthy, neutral, attention, urgent, problem), the rule that fired, a headline, a
reason sentence and the actions that fix it (Start, Stop it and start, Start now, Restart,
Show in logs with a filter, Open Configure, Reset the configuration, Check for updates,
Install, Cancel, and the recovery actions of a failed launcher update). The 18 rules are
tried in order and the first match wins, cause before symptom: a failed launcher update
(1); every phase but running (2 to 6: failed, backoff or crash loop, stopping, stopped or
an earlier zebrad still running, starting or a slow start); what can be wrong while running
(7 to 16: a silent RPC, no peers, a stalled sync, no new block at the tip, an urgent end of
support or an install countdown, few peers, a slow sync, recent errors in the log, a
restart after a crash, the end-of-support warning); then syncing (17) and healthy at the
tip (18). Rule 0, `Verdict::connecting`, stands in before the first status. The thresholds
are the named constants at the top of the file (`STALE_AFTER` 30 s, `RPC_FIRST_ANSWER`
60 s, `RPC_SILENT_PROBLEM` and `SLOW_START` 5 min, `NO_PEERS_FOR` 2 min, `FEW_PEERS` 3 for
`FEW_PEERS_FOR` 5 min, `STALL_ATTENTION` 2 min and `STALL_PROBLEM` 10 min behind the tip,
`STALL_AT_TIP` 10 min, `RECENT_ERRORS` 10 min, `RECENT_CRASH` 1 h, `CRASH_LOOP` 3,
`EOS_URGENT` 3 days). `NodeState.phase_since` records when the phase or the rule last
changed, for the "for N min" in the reason. `summary_text` builds the plain-text report
that "Copy summary" puts on the clipboard (verdict, versions, phase, sync, peers, mempool,
disk, ports, end of support, the newest problems and the run's events, with absolute local
times) from the same input, so it cannot disagree with the page.

**Dashboard page.** `ui/src/pages/dashboard.rs` holds the signals the cards share, the
chart data and the page order; the cards are in `ui/src/pages/dashboard/`. `status_card.rs`
shows the verdict with its actions and the Copy summary button beside the sync hero
(figure, meter, rate, ETA, last block, and the checkpoint or full-validation line from the
metrics and the log). `tiles.rs` holds the range row with its freshness sentence and the
six stat tiles with sparklines (Height, Blocks per minute, Peers, Mempool, Disk, Uptime)
from the statistics history, the metrics (Disk) and the status (Uptime). The page itself
mounts the two inline SVG line charts (`ui/src/charts.rs`, no chart library; hover
crosshair, keyboard navigation and a table view): "Blocks behind the tip" and "Peers" by
direction, whose table adds the peer set's ready and unready counts from the metrics.
`resources.rs` is the Resources card under the charts (see
[RESOURCES.md](RESOURCES.md)): four tiles (CPU, Memory, Disk or I/O, Processes) with rates
over the last minute, the table of the node's processes in tree order, and a line about the
computer's CPUs, memory and swap (commit on Windows, memory pressure on macOS). It stays
hidden until the node's first sample, a stopped node shows its last run instead of live
values, and a running node whose sampler stopped after a panic says it is not measured until
the next run; none of its figures takes a tone. `activity.rs` is the activity row: Recent problems and This run (views from
`ui/src/components/log_insights.rs`) and the Peers card with its caption, the table of
connected peers from `NodeDetails` and the handshake and obsolete-peer counters from the
metrics. `details.rs` holds the Node and Chain cards, each with Copy, from the status, the
statistics, the details, the settings, the metrics and the log facts. Every other slot
renders in every state: loading, running, stopped and refetching replace values in place.

**Log analysis.** `ui/src/insights.rs` folds every log batch, in `seq` order, into a
summary of the run: the sync stage from zebrad's `sync::progress` lines (syncing, near
the tip, at the tip, slow, stalled, waiting for genesis), deduplicated warnings, errors
and stderr lines with counts, lines per minute by level over the last hour, and the
startup milestones of the current run (state opened, peers, RPC, first progress report)
with the peer, checkpoint and mempool facts. The Logs page shows these cards in a
collapsible analysis panel, with the stage card and the activity chart; the dashboard
shows the recent problems and This run; picking a problem filters the Logs page to it. The
Logs tab carries a count of problems that arrived while the page was not shown.

**Run events.** This run lists the newest events of the current run, newest first, from two
sources (`Insights::events`, `ui/src/insights/events.rs`). The log gives the run's
milestones (those within 2 s of each other on one row), the first appearance of each ERROR
and stderr problem, one line per earlier run, and the launcher's own notes since the
previous run stopped: `launcher_event` reads the supervisor's `zebrad started (pid N)`,
`stop requested`, `zebrad exited (...)`, `restarting zebrad in` and `not restarting zebrad`
lines and its reports of a stale zebrad terminated or a start, stop or kill that failed.
The session gives what the UI observed itself, recorded with
`Insights::record_session_event`: status transitions (a new phase, a new last exit, a stale
zebrad found; `status_events`), the updater's steps (`update_events`) and the configuration
changes (`config_outcome_event`, `config_replaced_event`). `merge_events` puts both in one
list: a session event that repeats a logged one of the same kind within `DEDUPE_WINDOW`
(120 s; problems must also match in text) is dropped, except for exits and restarts, where
the session's copy carries the backend's exit time and crash count and the log's copy goes
instead.

**Mock states.** Outside Tauri (`trunk serve`), `ui/src/mock.rs` stands in for the backend
and the page URL's query picks the state it starts in; the table at the top of that file
lists every flag, and [ui/README.md](../ui/README.md#develop-in-a-browser-mock-mode)
describes the states. The dashboard's verdicts come from `?running` (zebrad is already
running when the page loads), `?stalled` (the height stops while the tip estimate creeps
on: "Sync stalled"), `?nopeers` (0 peers everywhere and an empty peer list: "No peers" once
two minutes of samples agree), `?crashloop` (zebrad panics about 15 s after every start and
is restarted after 5, 10 and 20 s: "zebrad crashed", then "Crash loop", with the exits and
restarts on This run), `?eos=soon` and `?eos=halt` (the end-of-support warning and halt),
`?stale` (an earlier zebrad still running), and `?update=countdown` and `?update=health`
(the install countdown and a launcher update that did not come back healthy). The mock samples
its main zebrad's resources every 5 s while it has a pid; `?os=macos` and `?os=windows` give the
Resources card those platforms' figures and labels.

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
  estimated halt is within 14 days, read from the first Mainnet zebrad node, or a node's
  zebrad already refused to run), and then only
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

Install sequence, in a child task. "zebrad" is every node: the updater's `NodeControl` is
the herd (`src-tauri/src/updater.rs`), whose stop stops every node that runs, side by side,
and whose start starts those again.

```text
Update::verify (minisign, signed version; spawn_blocking)  -- fails: Error, zebrad untouched
  -> stop every node (graceful, kill after its grace period), capped at the longest
     supervisor::shutdown_cap = max(grace + 10 s, 70 s), the exit flow's cap
  -> write <config>/pending_update_check {from, to, zebrad_was_running, running_nodes}
  -> Update::install -> app.request_restart()  (Windows: the installer exits the app)
stop fails -> start the nodes again that ran, Error
stop exceeds the cap -> keep waiting for it (up to the cap again), then start the nodes
                        again that ran (never on top of a stop in flight), Error
install fails -> remove the marker, start the nodes again that ran, Error
```

The launcher verifies the package once, right before stopping zebrad; the plugin also
verifies inside `download` and `install`. The marker is written only after zebrad
stopped, so a crash or failed stop before the install leaves no marker behind.

At the next launch, if `pending_update_check` exists, the health gate checks that the
new version is running and that every node the marker lists as running (a marker from a
single-node launcher means the first node) reaches `Running` with an RPC address within 5
minutes, each checked side by side and started first unless it starts at launch by
itself. Then the marker is removed. On failure the `update_health_failed` event (and
`UpdateInfo.health_failure`) carries a message, led by the node's name when several were
checked, and recovery hints:

| Outcome | Recovery |
|---|---|
| still the old version: the update did not complete | open logs, releases page |
| zebrad `Failed { EosHalt }`: past its end of support, update required | releases page |
| zebrad failed otherwise, or not `Running` with RPC in 5 minutes | open logs, reset config, releases page |
| a stale zebrad (`Status.stale`) still blocks the start at the limit; names its pid and binary | open logs |

A stale zebrad from an earlier launcher run is not an update failure: the gate's start
(and the restart after a failed install) emits `stale_process` like the start at
launch, the gate waits for the user to resolve it, and the 5 minutes start again once
it is gone. When the update did not complete, the nodes that ran are still started again
unless they start at launch.

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
- Identifier `org.zfnd.zeal`; the launcher has its own semver; About shows
  "Zeal X.Y.Z (zebrad A.B.C)".
- AppImage and deb on Linux (deb is Notify-only); NSIS only on Windows.
- No mutexes in Rust code, enforced by [`clippy.toml`](../clippy.toml).
- No history rewrite; the browser demo is deleted.

Routine choices: minimal managed `zebrad.toml`; an app-owned state dir chosen at first
run, never reusing a CLI zebrad's state; `zcashd_compat` unsupported; a GitHub App token
for bot PRs.

The full plan these decisions come from is [PLAN.md](PLAN.md).
