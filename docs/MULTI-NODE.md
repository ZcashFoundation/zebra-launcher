# Running several nodes

> **Status: phase 1 and phase 2a built, 2026-11-03; the rest is a proposal.** Zeal runs
> up to eight zebrad nodes; the other kinds are still a design.
> [ARCHITECTURE.md](ARCHITECTURE.md#nodes) describes what is built, and
> [Built differently](#built-differently) lists where the code went another way than this
> document.

Zeal runs one bundled `zebrad` today. The maintainer asked for "great UX and support for
running multiple node implementations at once": several zebrad nodes (Mainnet and Testnet,
or the release next to a development build), zcashd, Zakura, Zebend, the Z3 services Zaino
and Zallet wired to a node, and any other binary through a generic adapter. This document
proposes how.

## Goals

- Run several Zcash nodes and services side by side from one window, each with its own
  data folder, ports, configuration, logs and restart policy.
- Keep the single-node case as simple as it is now. Someone who wants one Zebra node goes
  through the same first run and sees one node.
- Make the dangerous combinations impossible or obvious before they happen: two nodes on
  one port, two nodes on one data folder, more chain data than the disk holds.
- Show how the nodes relate: an indexer or a wallet and the node it reads.
- Keep the project's rules: Rust only, a thin Tauri layer over testable crates, no mutexes.

## Non-goals, for now

- Nodes on other machines. Every node is a child process of Zeal on this computer.
- Containers. Docker images exist for some kinds; a container runtime would be a later
  "where it runs" option, not a node kind.
- Building or downloading binaries other than the bundled zebrad. Other kinds use a binary
  the user already has; verified downloads are a later phase (see [Phases](#phases)).
- Being a wallet. Zeal supervises Zallet; it does not show balances or send funds.

## Concepts

### Node

A node is one supervised process and everything Zeal keeps for it:

| Field | Meaning |
| --- | --- |
| `id` | A short, stable slug (`zebrad-mainnet`, `zakura-mainnet-2`), unique, used in paths, commands and events. Never reused while the node's data exists. |
| `name` | What the UI shows ("Zebra · Mainnet"), editable. |
| `kind` | The implementation: `zebrad`, `zakura`, `zcashd`, `zebend`, `zaino`, `zallet` or `generic`. Decides the adapter (below). |
| `network` | `Mainnet`, `Testnet` or, later, `Regtest`. |
| `program` | Where the binary comes from: `Bundled` (zebrad only), `File(path)` (a binary the user picked; its version is read with `--version` where the kind supports it), or `Command { program, args, working_dir }` for kinds run through a wrapper (Zebend's Python supervisor, the generic adapter). |
| `data_dir` | The node's chain state. Defaults to `<app data>/nodes/<id>`; a node may use a folder of its own, but no two nodes may share one. |
| `ports` | The ports the node listens on (P2P, RPC, metrics, gRPC), assigned when the node is added and kept until the user changes them (see [Ports](#ports)). |
| `depends_on` | For a service or a sidecar: the node it talks to (Zaino and Zallet read a zebrad node; the zcashd compatibility build follows a zebrad or Zakura node). |
| `autostart`, `stop_grace_secs` | Today's global settings, per node. |

### Kind adapter

Everything that differs between implementations sits behind one adapter per kind in
`launcher-core`, so the supervisor, the registry and the UI stay generic:

| The adapter answers | zebrad today |
| --- | --- |
| Default ports per network, and which config keys or flags carry them | 8233/8232/9999 on Mainnet, 18233/18232 on Testnet |
| The command line and environment | `zebrad -c <file> start`, `ZEBRA_*` removed, colour off |
| The managed configuration: file name, format, the keys Zeal owns | `zebrad.toml`, TOML, the managed keys in ARCHITECTURE.md |
| How to read the node: RPC flavour, auth, metrics page | zebrad JSON-RPC with a cookie, Prometheus `/metrics` |
| How to read its logs: line format, exit classes, milestones | `logs.rs`, `insights.rs` |
| How to stop it | stdin close or `SIGTERM`, job termination on Windows |
| Which UI it gets | the full dashboard, the configuration form, log analysis |

An adapter declares capabilities (`config_form`, `metrics`, `sync_progress`, `peers`,
`log_analysis`) and the UI shows what a kind supports. A kind without a tailored
dashboard still gets the status, the RPC chain facts if its RPC is zcashd-compatible, the
log view and the node's settings.

## The kinds

What each kind needs from Zeal, from its project's releases, documentation and source as of
2026-11-02. Facts marked "unconfirmed" are to be checked before the phase that adds the kind.

| Kind | Runs on | Binaries | Uses | NU7 (Mainnet 3,543,405) |
| --- | --- | --- | --- | --- |
| zebrad | Linux, macOS, Windows | bundled with Zeal | — | 7.0.0 follows it |
| Zakura | Linux x86-64 and arm64 | signed release tarballs | — | from 1.7.0 |
| zcashd | Linux x86-64 | the Zcash Foundation's compatibility build | a zebrad or Zakura node | the compatibility build only |
| Zebend | macOS on Apple Silicon | none yet (needs an unpublished Bend fork) | — | unconfirmed |
| Zaino | Linux (Docker amd64), from source elsewhere | crates.io, Docker | a zebrad node | not in 0.10.2 |
| Zallet | Linux | tarballs, .deb, Docker | a zebrad built with the `indexer` feature | from beta.4 |
| Generic | wherever the program runs | the user's | optional | the program's |

Only zebrad runs everywhere. The add-node dialog therefore shows each kind's platforms and
explains why a kind cannot run on this computer instead of hiding it.

### zebrad

The Zcash Foundation's node, bundled and pinned (`ZEBRAD_VERSION`, 7.0.0, which follows
NU7 on Mainnet). Everything Zeal does today. A second zebrad node uses the same bundled
binary with its own config, data folder and ports; a development build from a Zebra git ref
is a `File` program picked by the user (CI already produces such builds, see
[SIDECARS.md](SIDECARS.md)). zebrad halts about 56 days after its release, so every node's
end of support is shown on its card, not only the bundled one's.

### Zakura

A Zcash full node forked from Zebra by Valar Group and Project Tachyon
([zakura-core/zakura](https://github.com/zakura-core/zakura)), built for scale.

- Releases (1.8.0, 2026-11-02) ship Linux x86-64 and arm64 tarballs with a `SHA256SUMS`
  signed with minisign; nothing for macOS or Windows. Default x86-64 builds need the BMI2
  and ADX CPU extensions.
- `zakurad -c <file> start`, with `zakura.toml` in a schema that mirrors zebrad's: the keys
  Zeal manages for zebrad all exist, and sections reject unknown keys as zebrad's do. A
  `zakura` adapter derives from the zebrad one with its own binary, file name and keys.
- It reads `ZAKURA_*` and, deprecated, `ZEBRA_*` variables: Zeal removes both from its
  environment, as it removes `ZEBRA_*` for zebrad.
- RPC and metrics are off until configured; cookie auth is on by default. P2P is 8233 or
  18233, plus an experimental QUIC listener on 8234 that is on by default off Mainnet, which
  port assignment has to count.
- Pruning: `[state] storage_mode = "pruned"` (keeping at least 10,000 blocks of
  transactions), one way only. Pruned snapshots (about 12 GiB) and archive ones (256 GiB) are
  published with a `latest.json` and SHA-256 sums but no signature: Zeal can offer "start
  from a snapshot", saying it is checked against the published checksum only.
- Its database format (v30 in 1.8.0) differs from zebrad 7's (v29): a Zakura node never
  reuses a Zebra state folder. Like zebrad it halts at its end of support (1.8.0 at height
  3,643,309, early December 2026).

### zcashd

Stock zcashd cannot run any more: 6.20.0, the last release, reached its end-of-support halt
on Mainnet in July 2026 and does not know NU6.3 or NU7, so it cannot follow Testnet either.
What remains is the Zcash Foundation's **compatibility build**
([ZcashFoundation/zcashd](https://github.com/ZcashFoundation/zcashd), `zebra-compat-v1.3.0`,
Linux x86-64 only), which follows NU7 but runs only as a sidecar: it refuses to start
unless it connects (`-connect`) to exactly one zebrad or Zakura node, and opens no P2P
listener of its own.

So a zcashd node in Zeal is that build, attached to one of the user's zebrad or Zakura nodes
on the same network, for the zcashd RPCs (and its wallet) that some tools still need. zebrad
can also supervise such a sidecar itself (`[zcashd_compat]`, which Zeal turns off today);
that mode checks for at least 4 CPUs, 16 GiB of memory and 275 GiB of disk. Which of the two
Zeal uses is an open question below.

### Zebend

An independent Zcash node whose consensus and cryptography are written in Bend
([betterclever/zebend](https://github.com/betterclever/zebend)), run by a Python supervisor
from a checkout: `python3 -u node/ops/supervise.py run node/ops/zebend.json`.

- Mainnet only, outbound connections only, pruned. Developed on Apple Silicon (its GPU paths
  need Metal); Linux is unconfirmed and Windows unlikely.
- Others cannot build it yet: it needs a Bend fork that is not published, and its release
  has no binaries. `zebend.json` names its author's own paths, so Zeal would write its own.
- Its RPC is a zcashd-compatible subset on 127.0.0.1:18233 (Testnet's P2P port) with no
  authentication, and its mempool service is on 18234. State lives in the checkout (about
  15 GB).
- Its project says it is young, unaudited software not to be used as the only validator for
  funds, and Zeal says so when it is added.

A `Command` program over the generic adapter, with these ports as a preset, once it can be
built outside its author's machine.

### Zaino

An indexer ([zingolabs/zaino](https://github.com/zingolabs/zaino)): `zainod start -c
zainod.toml` (`zainod generate-config` writes one).

- Reads a Zebra node over JSON-RPC (cookie or user and password) by default, or Zebra's state
  directly (which Zaino 0.10's pinned Zebra crates cannot do for Zebra 7's database).
- Serves the lightwalletd gRPC on 127.0.0.1:8137; its own JSON-RPC is off unless configured.
  It defaults to Testnet, so Zeal always writes the network.
- No release binaries for 0.10.2 (crates.io and an amd64 Docker image). Releases up to 0.10.2
  stop syncing at the NU7 activation height; the fix is not released yet. Zeal reads the
  version and warns while it predates NU7 support.

### Zallet

The Z3 wallet ([zcash/zallet](https://github.com/zcash/zallet), 0.1.0-beta.4, 2026-11-02).

- Linux only: static tarballs, `.deb` packages and a Docker image. On macOS and Windows it
  cannot sync.
- It talks to zebrad only (beta.4 dropped Zaino), and only to a zebrad 7 or later **built with
  the `indexer` feature**, which Zebra's release binaries (and so Zeal's bundled zebrad) do
  not include, with `rpc.indexer_listen_addr` set; it opens zebrad's database read-only, so
  both run on this computer with the same database format.
- Setting a wallet up takes three commands before the first start
  (`generate-encryption-identity`, `init-wallet-encryption`, `generate-mnemonic`). The
  add-node flow runs them, and the recovery phrase is the user's to write down: Zeal never
  stores or logs it.
- Its RPC is off by default (the documentation's example is 127.0.0.1:28232) and uses a cookie.
- Beta: an upgrade may require recreating the wallet.

### Generic

Any program Zeal is told how to run: program, arguments, working directory, environment,
an optional zcashd-compatible RPC endpoint and credentials, and optional ports to check.
Zeal supervises it (start, stop, restart, crash backoff by exit code), keeps its log, and
shows the chain facts its RPC answers. Presets for known kinds are generic specs with the
right defaults filled in.

### What this means for the design

- **Platforms.** Every kind but zebrad is Linux-first; the dialog says so per kind.
- **Several nodes, one address.** Zebra accepts one connection per IP address by default
  (`max_connections_per_ip`), so two nodes on the same network behind one address compete for
  the same peers: each remote peer takes only one of them. Zeal warns when a second node of a
  network is added, and suggests it only where it is wanted (a Testnet node, a node under
  test).
- **Ends of support.** zebrad and Zakura halt at a known height; Zeal shows each node's and,
  for programs the user provides, links to the project's releases. Verified downloads of
  other projects' releases (Zakura's signed sums, Zallet's packages) are a later phase.
- **Authentication varies.** Cookies for zebrad, Zakura, zcashd and Zallet; none at all for
  Zebend, whose RPC Zeal only ever reaches on loopback.

## Backend

### Registry

A `NodeRegistry` actor in `launcher-core` owns the node list (persisted in `launcher.json`)
and one `NodeRuntime` per node: today's supervisor actor, RPC monitor, metrics poller and
config handle, created from the node's spec and its kind adapter. Commands are routed by
node id over channels; the registry publishes a `watch<Vec<NodeSummary>>` (id, name, kind,
network, phase, height, tip, peers, last block, disk) for the overview and the tray. No
locks: each node's actors own their state, as the single node's do now.

The supervisor changes little. It already takes its command line as configuration
(`SupervisorConfig { command: ZebradCommand, .. }`); that becomes a `LaunchSpec` (program,
arguments, environment, working directory, stop method) plus the kind's exit classifier,
so zebrad's classes (`ConfigError`, `DbLocked`, `PortInUse`, `EosHalt`) stay zebrad's and a
generic node is classified by its exit code. Each node keeps its own log ring, log file,
pidfile and restart policy under `<app data>/nodes/<id>/`.

### Ports

Every Zcash node defaults to 8233 (P2P) and 8232 (RPC) on Mainnet, so two of them collide
unless Zeal assigns ports. When a node is added, Zeal takes the kind's defaults if no other
node claims them and they can be bound, otherwise the next free port in a per-kind range
(for example 28233 and up for a second Mainnet P2P listener), and stores the result in the
node's spec. The managed config keys or flags carry the ports, so zebrad's managed
`rpc.listen_addr` becomes "this node's RPC port" instead of a constant. Ports are checked
again at every start; a port taken by something outside Zeal fails the start as
`PortInUse` with the owner named where the OS tells. The overview lists every node's ports
in one table.

### Data and disk

Each node has its own data folder; adding a node that points at another node's folder is
refused, and zebrad's `DbLocked` class still catches anything outside Zeal. The add-node
flow shows the free space of the chosen folder and an estimate for the kind and network;
when several nodes share a disk, the overview sums their estimates against the free space
and warns before the disk fills, not after.

### Dependencies

A service names the node it depends on. Zeal starts the dependency first and waits until
its RPC answers, stops dependents before their dependency, and shows a dependent whose
dependency is down as "waiting for Zebra · Mainnet" instead of letting it crash-loop. The
dependency's connection details (address, port, cookie path) are written into the
dependent's managed config, so changing a node's RPC port updates its dependents.

### Settings and migration

`launcher.json` goes to schema version 3: the global settings stay (updates, appearance,
close to tray, Grafana) and a `nodes` list holds the specs. A version 2 file becomes one
node, `zebrad-<network>`, that keeps today's paths: the same `zebrad.toml`, the same state
folder, the same ports. Nothing moves on upgrade. Removing a node asks whether to keep or
delete its data; deleting uses today's `delete_node_data` rules for that node's folder.

## Commands and events

Every node-scoped command takes a `node` id: `start_node`, `stop_node`, `restart_node`,
`read_config`, `apply_config`, `reset_config`, the snapshot commands and the log
subscription. New commands manage the list: `list_nodes`, `add_node` (validates a draft
spec: ports, folder, binary, dependency), `update_node`, `remove_node`, `inspect_binary`
(runs `--version` and guesses the kind) and `suggest_ports`. Events carry the node id
(`status { node, status }`, and so on), and `nodes` publishes the summaries. The UI keeps
one log subscription per node it shows and folds each node's log analysis separately.

## User interface

### Principles

- **One node looks like today.** With a single node there is no overview to get through:
  the window opens on that node's dashboard. The way to add another is one click away, not
  in the way: an "Add a node…" button in a Nodes section of Settings, and the same item in
  the tray menu, open the add-node dialog; once a second node exists the sidebar takes over.
- **Every node is recognisable at a glance.** A node keeps a colour and a kind badge
  everywhere it appears: overview, sidebar, tray, log lines in a combined view.
- **Problems are explained before they happen.** Port, folder and disk conflicts show in
  the add-node flow with the fix ("use port 28233 instead"), not as a failed start.
- **Relations are visible.** A service says what it uses ("reads Zebra · Mainnet"), and a
  node says what depends on it before it is stopped or removed.

### Layout

A sidebar replaces the top tab bar once there is more than one node:

```text
+------------------------+------------------------------------------------------------+
| (zebra) Zeal           |  Zebra · Mainnet  zebrad 7.0.0  [Running] [Stop] [Restart] |
|                        |  Dashboard   Logs   Configure   Node settings              |
| [Mainnet 4|Testnet 2]  | +--------------------------------------------------------+ |
|                        | |  (today's dashboard, for this node)                    | |
|  Overview          3/4 | |                                                        | |
|  * Zebra          94 % | |                                                        | |
|  * Zakura         97 % | |                                                        | |
|  * Zaino       serving | |                                                        | |
|  o zcashd      stopped | |                                                        | |
|                        | |                                                        | |
|                        | |                                                        | |
|  + Add node            | |                                                        | |
|                        | |                                                        | |
|  Grafana               | |                                                        | |
|  Settings              | +--------------------------------------------------------+ |
+------------------------+------------------------------------------------------------+
```

- Mainnet and Testnet tabs at the top of the sidebar, each with its node count, scope the
  whole window to one network: the sidebar, the overview, the node pages, and later the
  explorer, the herd's health and the wallet. Testnet is marked everywhere in the warning
  tone (a band over the page, the tab, the network chips), so Testnet coins are never
  mistaken for real ones; a single Testnet node, which keeps the tab bar, gets the band too.
  The window shows the backend node's network and moves with it when that changes; the
  add-node dialog starts on the network shown, and a node added on the other network takes
  the window there.
- The sidebar lists the network's nodes with a status dot and one fact (sync percentage,
  "at tip", "serving", "stopped"). Under 900 px it folds to a rail of dots.
- A node's page has today's tabs scoped to it (Dashboard, Logs, Configure) plus Node
  settings (name, autostart, stop grace, data folder, ports, binary, remove). The status
  strip becomes the page header.
- Grafana and Settings stay global.
- The pages' breakpoints follow the page's width, not the window's (CSS container queries
  on the page area), so the sidebar narrows them as it narrows the page: a window of
  1,400 px has a page 1,184 px wide, and the dashboard lays out two cards across, not
  three. With one node the page is as wide as the window, so nothing changes there. Every
  webview Zeal supports has container queries (WebKitGTK 2.38, Safari 16, WebView2); an
  older one would ignore them and keep each page's base layout, which still works.

The tray follows: its tooltip summarises the nodes ("Zeal: 3 running, 1 stopped"), and its
menu has Show, a submenu per node with Start and Stop, Start all, Stop all and Quit. Quit
stops every node, dependents first, each within its own stop grace.

### Overview

```text
  Mainnet · 4 nodes · 3 running · 1 stopped           [Start all] [Stop all] [+ Add node]

  +-----------------------------+ +-----------------------------+ +---------------------+
  | * Zebra · Mainnet    zebrad | | * Zakura · Mainnet   zakura | | o zcashd · Mainnet  |
  | Syncing 94.5 %              | | Syncing 97.1 %              | | Stopped 2 days ago  |
  | ##############....  2.95M   | | ###############...  3.03M   | |                     |
  | 12 peers · block 4 s ago    | | 9 peers · block 6 s ago     | | 290 GB              |
  | [Open] [Stop] [...]         | | [Open] [Stop] [...]         | | [Start] [...]       |
  +-----------------------------+ +-----------------------------+ +---------------------+
  +-----------------------------+
  | * Zaino · Mainnet   indexer |
  | Serving gRPC on :8137       |
  | reads Zebra · Mainnet       |
  +-----------------------------+

  Disk   /mnt/zebra  1.2 TB free, 540 GB used by 2 nodes
  Ports  8233 8232 9999 Zebra · 18233 18232 19999 Zakura · 8137 Zaino · 28232 zcashd
```

Each card carries the node's verdict from today's health rules where the kind supports
them, so "Sync stalled" or "Crash loop" shows on the overview, and its actions.

### Adding a node

A dialog in three steps, each validated as it is filled in:

1. **Kind**: a card per kind with what it is, what it needs (bundled, or a program you
   provide) and where it runs. A kind that cannot run on this computer stays visible, with
   the reason, and cannot be picked.
2. **Details**: the network (Zebend: Mainnet only), the name, the program (the bundled
   zebrad, a binary picked from disk that Zeal runs with `--version` and shows what it
   found, or a checkout or command line), the node it uses (services and the zcashd
   sidecar), the data folder with its free space, and the ports, assigned around the
   existing nodes' with each move explained ("P2P 18233 instead of 8233, which Zebra ·
   Mainnet uses"). The kind's caveats show here: Zebend unaudited, Zallet beta, Zaino
   before NU7, Zakura's CPU requirement.
3. **Review**: everything Zeal will set up, including the config it will write, then
   "Add" or "Add and start".

### First run

Unchanged for one zebrad node: the steps end by creating `zebrad-mainnet` (or Testnet).
The last step mentions that other nodes can be added later from Settings → Nodes.

## Phases

1. **Overview and node pages in the UI, against the mock.** The sidebar, overview, node
   header and add-node dialog, built on a node list that the real backend reports as one
   node until phase 2 lands, so nothing changes for users in between. The mock's `?herd`
   flag adds five nodes. Screenshotted for review. Mainnet and Testnet tabs at the top of the
   sidebar scope the whole window to one network, with Testnet marked in the warning tone.
2. **Several zebrad nodes**, in two parts.
   - **2a, built**: the registry, per-node runtimes, port assignment and the port check at
     every start, settings v3 and its migration, node-scoped commands and events, the
     add-node dialog on the real backend (zebrad on Mainnet or Testnet, running the bundled
     zebrad; the other kinds visible as coming later), Node settings with the name, the
     start at launch and Remove, Settings → Nodes as the single-node entry point to the
     dialog, an update that stops and starts every node, Quit that stops every node, and the
     tray's node counts with Start all and Stop all.
   - **2b**: a tray submenu per node and "Add a node…" in the tray, the stop grace, data
     folder and ports in Node settings, the warning for a second node on one network, and
     the first run's pointer to Settings → Nodes.
3. **Zakura**, as a variant of the zebrad adapter: its binary, `zakura.toml`, pruning and
   start from a snapshot.
4. **The generic adapter** and fake-zebrad's mode that behaves like a zcashd-compatible
   binary for tests; Zebend as a preset once it can be built elsewhere.
5. **Zaino, Zallet and the zcashd sidecar**, with dependencies, Zallet's wallet setup and
   a zebrad built with the `indexer` feature.
6. **Later**: verified downloads of other projects' releases, tailored dashboards for kinds
   other than zebrad, containers.

## Open questions

Decided on 2026-11-03, for phase 2:

1. Zeal bundles only zebrad. Every other kind comes through a program the user picks, in
   the phase that adds it; Zakura's signed release sums make it the first candidate for a
   verified download later.
2. Zallet's zebrad with the `indexer` feature: decided with Zaino, Zallet and the zcashd
   sidecar, which come later (phase 5).
3. zcashd as its own node or zebrad's `[zcashd_compat]` supervision: also phase 5.
4. Every zebrad node in phase 2 runs the bundled zebrad; a zebrad the user picks comes with
   the programs of the other kinds.
5. No Regtest yet.
6. At most eight nodes (`MAX_NODES`).

## Built differently

Where phase 2a's code differs from the design above:

- **Commands are not routed through the registry.** The registry publishes the node handles
  in a `watch`; a command looks its node up by id and goes straight to that node's actors.
  Only changes to the list (add, update, data folder, remove) go through the registry actor.
- **The node summary is the node's info and phase.** `NodeSummary` carries no height, tip,
  peers or disk: the UI has those from each node's own events, and the tray needs only the
  phase.
- **Events are `{node, value}`.** Every node-scoped event carries a `NodeEvent { node,
  value }` (`status`, `node_stats`, `node_details`, `metrics`, `stale_process`), and `nodes`
  carries the list as `NodeInfo`s.
- **No kind adapter or `LaunchSpec` yet.** With zebrad the only kind, the supervisor still
  takes zebrad's command line; the adapter comes with the second kind.
- **`add_node` fills in the draft.** A draft may leave out the id (the first free one for the
  kind and network, skipping removed nodes' kept folders), the ports (the kind's defaults
  moved around every other node's and around ports in use) and the data folder (a folder of
  its own). There is no `inspect_binary` or `suggest_ports` yet: the bundled zebrad needs no
  inspection, and the dialog shows the ports `assign_ports` gives around the known nodes,
  with a note that the backend checks them again.
- **`update_node` also takes the network and the stop grace,** which the single-node Settings
  page and the first run edit; a node's own Node settings has the name and the start at
  launch so far.
- **Files.** The first node keeps the launcher's own locations (the `zebrad.toml` in the
  config folder, and so on); only nodes added later have `<app data>/nodes/<id>/`, which
  holds their config, pid file, cookie, log and, unless another folder is chosen, `state/`.
- **The P2P port is managed only when it has to be.** `network.listen_addr`'s port is
  written only when it differs from zebrad's default for the network, so a single node's
  `zebrad.toml` stays as it is.
- **The port check names the port, not its owner.** A port held by another program fails
  the start as `PortInUse` with the address; the owning process is not looked up.
- **Removing.** The last node cannot be removed. The node leaves the list before anything is
  deleted, so a quit during a long deletion leaves no listed node with part of its state
  gone; what cannot be deleted is named in the error. Deleting a node's data removes its
  chain state with `delete_node_data`'s rules and then the rest of its own folder; for the
  first node, the launcher's own files (its `zebrad.toml` in the config folder) stay.
- **The first node's id during the first run** follows the network chosen there
  (`zebrad-testnet`), as nothing depends on it yet; afterwards ids never change.
- **Node settings** is a tab only with several nodes; with one node, Settings holds the same
  choices.
- **The updater treats the nodes as one**: an update stops every node that runs, the marker
  lists them, and the health gate checks each of them after the update.
