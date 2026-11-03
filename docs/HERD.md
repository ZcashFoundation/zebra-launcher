# The herd: nodes that check each other

## Status

> **Proposal, 2026-11-03.** A design for review before any code. It builds on
> [MULTI-NODE.md](MULTI-NODE.md) (the node registry, kinds, ports and phases) and describes
> the launcher's current shape where needed. [ARCHITECTURE.md](ARCHITECTURE.md) stays the
> description of what is built and is updated phase by phase as this lands. Facts marked
> "unconfirmed" are to be checked before the phase that relies on them.

The maintainer described the product as a private node wallet with a whole zeal of nodes
behind it: tooling to manage the nodes and watch their resource use and behaviour, and the
assurance of being connected to the network several times over, probably through mostly
pruned nodes and one or a handful of full archive nodes with a full block explorer. They
added: "an optional DNS seeder, pull in zeeder".

MULTI-NODE.md lets Zeal run several nodes. This document is about what they do together,
the herd: which role each node plays, which herds to recommend for a laptop, a desktop and a
small server, how Zeal checks that the nodes are on one chain and reach the network through
different peers, and how zeeder fits in as an optional seeder. The block explorer, per-node
resource monitoring and the Zallet wallet have designs of their own; this one says what they
take from the herd.

## Goals and non-goals

### Goals

- Say in one sentence per network whether the user's nodes are on the same chain, whether
  that chain is moving, and whether the nodes see the network through different peers.
- Catch a node that falls behind, stalls, forks off or loses its peers, and two
  implementations that disagree about the rules, early and with one clear action.
- Recommend herds that fit the computer (laptop, desktop, small server), with honest budgets
  for disk, memory, downloads and time, and the truth about macOS and Windows.
- Give every node a role with settings that suit it: archives keep history, pruned nodes
  watch the network cheaply, a seeder counts the network.
- Make zeeder an optional kind: a census of the network and, on a server, a public DNS seed.
- Give the wallet and the explorer facts they can show: "on 3 of 3 nodes", "this node is
  behind your others".
- Keep the project's rules: no mutexes, a thin Tauri layer over testable crates, nothing sent
  off the computer, every view demoable in the mock.

### Non-goals, for now

- Deciding which chain is right. Zeal reports a disagreement and its facts; it never picks a
  side for the user and never moves a node to another chain.
- Protection against whoever controls the user's internet connection, against majority hash
  power, or against a bug every node shares (see [what the herd protects
  against](#what-the-herd-protects-against-and-what-it-does-not)).
- Nodes on other machines, or a VPN or Tor per node (a MULTI-NODE non-goal).
- Automatic restarts or peer resets in the first releases: Zeal proposes, the user acts.
- Getting a home seeder into Zebra's default seeder list.

## Roles

### What a node carries besides its kind

`NodeKind::role()` (`FullNode`, `Indexer`, `Wallet`, `Custom`, in
`crates/launcher-types/src/nodes.rs`) stays the coarse category the UI already uses. The
herd adds:

- **Storage**, stored in the node spec for full nodes: `Archive` or
  `Pruned { tx_retention }`. One kind can be either (Zakura), so it can't come from the kind.
- **Role**, derived from the kind and the storage, never stored: archive, pruned, seeder,
  service or wallet. Deriving it means no spec can say "pruned zebrad".
- **Vote**, derived from the kind and role; the user can override it per node. It says
  whether the node counts when Zeal says "k of n agree":
  - **Witness**: its own peers and its own validation (zebrad, Zakura). Counts.
  - **Validator**: its own consensus code, but it hears only its parent node (the zcashd
    compatibility build). Its rejection of a block is an alarm; it is not in "k of n".
  - **Observer**: shown and flagged, never counted unless promoted. Zebend (young and
    unaudited; its project says not to rely on it alone), a generic node with a
    zcashd-compatible RPC, a zebrad the user marks "don't count" (a development build under
    test), and a node bootstrapping from the user's own archive.
  - **None**: Zaino, Zallet, zeeder.
- **Family**: the consensus code a node runs: zebra (the bundled zebrad and any zebrad file),
  zakura, zcashd, zebend or other. It tells a node's own problem from two implementations
  disagreeing ([Agreement](#support-and-splits)).
- **Trust origin**, stored for full nodes: `Genesis`, or
  `Snapshot { publisher, height, sha256 }`. A witness checked from the first block is the
  herd's auditor; that is a chip on the node, not a role.

### The roles

| Role | What it is for | Kinds today | Runs on | Plan for (Mainnet) | Cannot |
| --- | --- | --- | --- | --- | --- |
| **Archive** | keeps every block: the explorer's history, the backend of Zallet, Zaino and a zcashd sidecar, and the only role that serves history to other nodes | the bundled zebrad; a zebrad file (a development build, or one built with the `indexer` feature for Zallet); Zakura with `storage_mode = "archive"` | zebrad everywhere; Zakura on Linux | 400 GB of disk (282 GB used at height 3,529,825, about 20 GB more a year at today's block rate, more after NU7: unconfirmed), 3 GB of memory at the tip, 8 GB or more while syncing | — |
| **Pruned** | watches the network cheaply through its own peers and checks the other nodes' chain: tips, block hashes and confirmations at every height | Zakura with `storage_mode.pruned`; Zebend (an observer); zebrad once a release can prune | Zakura: Linux x86-64 with BMI2 and ADX, or arm64; Zebend: macOS on Apple Silicon | 25 GB (about 15 GB used), about 3 GB of memory (unconfirmed) | serve old blocks; back the explorer's history or address pages; back Zallet, Zaino, or a zcashd sidecar that starts from scratch |
| **Seeder** | counts the reachable network and, on a server, answers DNS queries as a public seed | zeeder, a new kind | Linux x86-64 and aarch64 | 1 GB, 0.2 GB; claims its network's default P2P port | anything about chain state; it never votes |
| **Service** | serves something on top of a node: light-wallet gRPC (Zaino), zcashd's RPCs (the compatibility build) | Zaino; the zcashd compatibility build | Linux | per kind; the compatibility build plans like an archive (zebrad's own sidecar mode checks for 4 CPUs, 16 GiB and 275 GiB) | — |
| **Wallet** | keys and transactions | Zallet | Linux | small (unconfirmed) | run on a pruned node, on Zakura, or on a zebrad built without `indexer` |

The facts behind the table:

- zebrad 7.0.0 cannot prune: `zebra-state/src/config.rs` at v7.0.0 has no such setting, and
  `ephemeral` only deletes the state on exit. Zebra's pruning was merged into its `next`
  branch on 2026-10-30 (PRs #12086 to #12093, merged by #12132) and is in no release; it also
  moves the database to format 30 even with pruning off and refuses zcashd sidecars once
  pruning is set.
- Zakura 1.8.0 prunes with `[state] storage_mode = "pruned"` or
  `[state.storage_mode.pruned] tx_retention = N`. N is at least 10,000 on Mainnet and Testnet
  (`MIN_PRUNING_RETENTION`; a lower value fails at startup) and 1,001 on Regtest. It deletes
  raw transaction bytes only and keeps headers, the height-to-hash index, the txid indexes and
  all consensus state; "pruned finalized blocks remain visible to chain-identity queries"
  (Zakura CHANGELOG, #133). A pruned database cannot become an archive again. Under
  checkpoint sync it drops the transparent address indexes for good (#1366).
- A pruned Zakura advertises no services, not `NODE_NETWORK`, unless zcashd compatibility is
  on (`crates/zakurad/src/commands/start.rs` at v1.8.0). Zebra 7 peers never redial an
  address known to lack `NODE_NETWORK`, and zeeder lists only peers that advertise it, so a
  pruned node lives on outbound connections.
- Each Zakura binary halts at its end of support (1.8.0 at height 3,643,309, early December
  2026), so a herd of Zakura nodes needs a new binary every few weeks until verified
  downloads land (MULTI-NODE phase 6).
- Zallet needs a zebrad 7 or later built with the `indexer` feature, on this computer, with
  the same database format, and a zebrad built without the feature deletes an existing
  spending index when it starts (Zebra book, `user/zallet.md` at v7.0.0).

### Which kind can take which role

`add_node` and `update_node` enforce this matrix with a pure `validate_spec`, and the start
path checks it again:

| Kind | Storage | Role | Vote | Depends on |
| --- | --- | --- | --- | --- |
| zebrad, bundled or a file | archive only | archive | witness; observer if marked "don't count" | — |
| Zakura | archive, or pruned keeping at least 10,000 blocks (Regtest 1,001) | archive or pruned | witness | — |
| zcashd compatibility build | — | service | validator of its parent | one zebrad or Zakura on the same network: an archive; a pruned parent only behind an advanced warning, since the sidecar can't recover from an outage longer than the retention |
| Zebend | pruned (fixed) | pruned | observer | — |
| Zaino | — | service | none | an archive (a Zakura archive is unconfirmed) |
| Zallet | — | wallet | none | an archive zebrad whose binary passed the `indexer` probe (below) |
| zeeder | — | seeder | none | — |
| Generic | declared: archive, pruned or unknown | as declared | observer if it declares a zcashd-compatible RPC, otherwise none | optional |

`NodeKind::uses()` stays the filter the add-node dialog lists; a storage-aware
`dependency_ok` refines it, so Zaino and Zallet refuse a pruned node.

### Defaults per role

| Role | P2P listener | Peer target (outbound limit) | Starts from | Keys Zeal writes |
| --- | --- | --- | --- | --- |
| Archive, the network's first (its **primary listener**) | `[::]:8233` (Testnet `[::]:18233`): the only role where the default port matters, because DNS seeders list only default-port peers | 25 (75 out, 125 in): Zebra's default | the first block; a Zcash Foundation archive snapshot (274.6 GB) is opt-in, with the trust wording below | managed: `network.listen_addr`, `network.cache_dir`; for Zakura also `state.storage_mode = "archive"` and `network.identity_dir = <node>/identity` |
| Archive, any other | assigned: 28233, 38233 and so on | 12 for zebrad (36 out); 16 for Zakura (24 out) | the first block | the same |
| Pruned (Zakura) | assigned; it makes outbound connections almost only | 16 (Zakura's outbound limit is 1.5 times the target: 24) | Zakura's pruned snapshot (12.2 GB), except the herd's auditor, which starts from the first block | managed: `network.listen_addr`, `network.cache_dir`, `network.identity_dir`, `network.p2p_stack = "legacy"`, `state.storage_mode.pruned.tx_retention` |
| Seeder (zeeder) | its crawler always binds `[::]:8233` or `[::]:18233`; no setting changes it | zeeder's own: 25, up to 75 out | — | the whole `zeeder.toml` ([The seeder](#the-seeder)) |
| Service, wallet | none: the compatibility build connects only to its parent, Zallet reads the database, Zaino uses RPC | — | — | the dependency's address, port and cookie, as MULTI-NODE says |

- **Peer targets are written once**, when the node is added, as ordinary keys the user may
  edit. They are tuning, not something the supervisor or the monitor rely on, which is where
  ARCHITECTURE.md draws the line for managed keys. zebrad 7's crawler keeps dialling until it
  reaches its outbound limit, not its target ("fill the peer set up to the outbound connection
  limit", `zebra-network/src/peer_set/initialize.rs` at v7.0.0), so for zebrad the limits are
  what the herd actually spends. Zakura's own default target is 100 (up to 150 outbound).
  Zakura 1.8.0's crawler differs: it keeps outbound peers at 80 % of the target (13 for a
  target of 16) and dials toward its 1.5× limit only when no peer is ready
  (`outbound_peer_replenishment_demand` in `crates/zakura-network/src/peer_set/initialize.rs`),
  so for Zakura the target sets the steady-state spend.
- **Retention is 10,000 blocks** by default: 2.9 days of transactions after NU7, 8.7 days
  before. That is Zakura's floor and what its snapshot holds; whether a node restored from
  the snapshot accepts a larger value is unconfirmed. The checks need only headers and
  hashes, which are kept at every height, and the reorg window is 1,000 blocks. 30,000 blocks
  (8.7 days after NU7, at most about 1 GB more) is offered for a pruned node checked from the
  first block that fronts a zcashd sidecar. Node settings show retention as time: "Keeps the
  transactions of the last 10,000 blocks: about 3 days."
- **Storage changes.** Zakura archive to pruned is a one-way change behind `TypedConfirm`
  ("Frees about 250 GB. Can't be undone. The explorer and the wallet stop using this node."),
  and takes a heavy-job permit (below). Pruned to archive means a new folder or an archive
  snapshot. A node that others depend on can't change role.
- **The wallet backend.** The archive zebrad that Zallet reads has its program pinned. Every
  zebrad 7.0.0 accepts `rpc.indexer_listen_addr` and serves the indexer gRPC whether or not it
  was built with `indexer` (`zebrad/src/commands/start.rs` at v7.0.0; Zebra book
  `user/zallet.md`, Known issues; zebra #11155), so neither the key nor an open gRPC port
  proves anything. `inspect_binary` starts a newly picked zebrad once on Regtest with an
  ephemeral state and reads the "Diagnostic metadata:" block that zebrad logs at startup
  (`zebrad/src/application.rs` at v7.0.0; the format is in
  `crates/launcher-core/tests/fixtures/zebrad-7.0.0-regtest-start-shutdown.stdout.log`). Only a
  build with the feature lists `indexer` among its comma-separated `features` and reports its
  `running state version` with `+indexer` (`zebra-state/src/constants.rs`); a binary missing
  either mark fails the probe. Zeal refuses to start the wallet backend's folder with a binary
  that failed the probe.

### Runtime role checks

- Zakura reports `pruned` and `pruneheight` in `getblockchaininfo`, the call the monitor
  already makes. A pruned node that reports `pruned: false` gets the node health rule "Set up
  as pruned but keeps everything: check its storage mode" (attention); an archive that reports
  `true` gets "This archive deletes old transactions: the explorer and the wallet can't use
  it" (problem). The node page shows "keeps blocks from H".
- The trust origin shows on the node page: "Checked from the first block (with the
  checkpoints built into the node)", or "History below block 3,539,128 trusted to Zakura's
  published snapshot (checked against its checksum only)". A herd with no witness checked
  from the first block gets the attention item "All your nodes started from snapshots. Add a
  pruned node that checks from the first block (Linux, about 25 GB, 4 to 6 hours)."
- Why the first block matters: Zebra "does not re-validate the blocks it finds in its cached
  state" (Zebra book, `user/snapshots.md` at v7.0.0). A snapshot moves the trust for
  everything below its height, the UTXO set and nullifiers included, to whoever cut it, and a
  checksum proves only that the download is intact. A tampered snapshot that keeps its node
  on the honest chain, with a fabricated note for example, shows up in no comparison of tips.
  So a wallet backend starts from the first block by default, and every herd keeps at least
  one witness checked from the first block.

### Ports: a fix first

`assign_ports` moves a second Mainnet node's 8233 up to 18233 today (the test
`ports_move_out_of_each_others_way` expects `[18233, 18232, 19999]`), which takes Testnet's
default port and leaves no default port for a later Testnet primary listener. Port
assignment gains a reserved list:

- 8233 and 18233 are kept for each network's primary listener, or for a seeder that claims one.
  The reservation covers every port role. Zebend's RPC, which its project puts on
  127.0.0.1:18233, gets an assigned port if its RPC port can be set (unconfirmed); otherwise
  Zebend and a Testnet primary listener both need 18233, and the add-node dialog says so.
- A P2P port never lands where zebra-network refuses to dial: 8232 and 18232 (zebra-network
  7.0.0 rejects the RPC ports here, where it seems to mean the P2P ones,
  `peer/priority.rs`), 18344, 8033, 18033, 16125 and 26125.
- Other nodes get 28233, 38233, 48233 and so on.
- Ports already stored are kept.

## Recommended herds

Two witnesses can only detect a disagreement. Three can usually name the odd one out, unless
the sides line up with implementations ([Support and splits](#support-and-splits)).

Per-node budgets, Mainnet, for planning (not measurements):

| Node | Disk to plan | Memory at the tip | Memory while syncing | One-time download | Ready in |
| --- | --- | --- | --- | --- | --- |
| Archive zebrad | 400 GB | 3 GB | 8 GB or more | about 300 GB from the first block | about a day (20 h 46 min in Zakura's benchmark of Zebra; "close to a day" in Gemini's deployments) |
| Pruned Zakura from the snapshot | 25 GB | 3 GB (unconfirmed) | about the same (unconfirmed) | 12.2 GB | about 16 min at 100 Mbps, then a short catch-up |
| Pruned Zakura from the first block | 25 GB | 3 GB (unconfirmed) | about the same (unconfirmed) | about 300 GB: every block is downloaded and checked | 4 to 6 h (vendor and third-party figures; hardware not stated) |
| zcashd compatibility build | about 300 GB (unconfirmed) | unconfirmed | — | — | — |
| Testnet archive | 30 GB | 1 to 2 GB | — | — | — |
| zeeder | 1 GB | 0.2 GB | — | — | — |

Every herd runs one heavy job at a time, more on bigger machines: an initial sync, a snapshot
restore, a storage conversion, an index rebuild. Initial sync is the memory peak.

### Laptop

Assumes 16 GB of memory, a 1 TB SSD, NAT (no inbound connections), and that it sleeps.

- **Linux:** the archive zebrad on 8233 (the explorer, and the wallet backend if it is the
  `indexer` build) and one pruned Zakura from the snapshot. A second pruned node is offered
  as a tiebreaker.
  - Disk 425 GB (450 with the tiebreaker). Memory 6 GB at the tip (9 GB), 11 GB or more while
    the archive syncs. One heavy job at a time. Download 12.2 GB per pruned node. Outbound
    connections from the herd: 75 + 13 (+ 13) in steady state, at most 75 + 24 (+ 24).
  - What it can say: "Your 2 nodes agree"; it detects a disagreement but can't tell which
    node is right.
- **macOS and Windows:** one archive zebrad, as today. Zeal can only run zebrad there, and
  zebrad 7.0.0 can't prune, so a second node means a second full copy of the same software.
  No panel ("One node checks the chain"), and the setup flow says so: "On this Mac, a second
  node means another 400 GB of the same software. Most people keep one node. Zeal will offer
  pruned nodes when Zebra ships pruning." A zakurad the user built for macOS can be added by
  hand (unconfirmed that it works).

### Desktop

Assumes Linux, 32 GB, a 2 TB SSD, 8 cores, always on.

- The archive zebrad on 8233 (the explorer; the `indexer` build if Zallet runs) and two
  pruned Zakura nodes: the auditor from the first block and one from the snapshot. Optional:
  a third pruned node, the zcashd compatibility build as a validator (a third consensus
  implementation), Zallet, Zaino once a release follows NU7, a Testnet archive.
  - Disk 450 GB (plus 25 for a third pruned node, 300 for the validator, 30 for Testnet).
    Memory 9 GB at the tip, plus the services. Two heavy jobs at once. Download 12.2 GB plus
    about 300 GB for the auditor. Outbound connections from the herd: 75 + 13 + 13 in steady
    state, at most 75 + 24 + 24.
  - What it can say: a majority of three when a Zakura node is the odd one, and "Zebra and
    Zakura disagree" when the archive is; the validator adds zcashd's consensus code.
- **macOS and Windows:** the archive zebrad and, with at least 1 TB free, an optional second
  archive zebrad: 800 GB, 6 GB at the tip. It detects stalls, lag and a node cut off from the
  network, but not Zebra bugs, and the panel says "the same software on both nodes".

### Small server

Assumes Linux, 8 vCPUs, 32 to 64 GB, 2 TB of NVMe, a static public IP and an unmetered link.

- The archive zebrad on 8233, open to inbound connections (the explorer, and a service to the
  network); a second archive (an archive Zakura for a second implementation with history, or
  the zebrad `indexer` build for Zallet and Zaino); three pruned Zakura nodes, at least one
  from the first block; the zcashd compatibility build as a validator; Zaino. Optional:
  zeeder.
  - Disk about 1.2 TB plus Zaino's store (unconfirmed); provision 2 TB. Memory about 15 GB at
    the tip plus the services: 32 GB with three heavy jobs at once, 64 GB comfortable. Traffic
    up to 300 GB a month for each archive that serves inbound peers (Zebra book), and 0.65 to
    6.5 TB a month for a public zeeder (2 to 20 Mbps, zeeder `docs/operations.md`). Outbound
    connections from the herd: about 130 to 150 in steady state, at most about 180.
  - What it can say: a majority of five across two implementations, unless the sides line up
    with them; the validator adds zcashd's consensus code.
  - zeeder can't run beside the public archive on 8233 ([The seeder](#the-seeder)): until
    its crawler's port can be configured, a public seed belongs on another IP or a VPS.

### Planning

- `plan::recommend(goal, machine, existing) -> Vec<HerdPlan>`, a pure function, picks from
  these and reuses the nodes the user has: most users already have a synced archive.
- **Machine class**: a laptop has a battery or less than 24 GB; a desktop has no battery and
  24 GB or more; a server is what the user says it is (a public address is asked, never
  guessed). Heavy jobs at once: 1, 2 and 3, editable from 1 to 4.
- **The bill** sums each member's disk per folder, memory at the tip plus 5 GB per heavy job,
  the downloads and the time until the herd can cross-check. A plan never leaves less than
  50 GB or 15 % free on a disk, whichever is more; optional members go first. Per-role disk
  estimates replace the flat 300 GB `LOW_SPACE_BYTES` warning (`ui/src/format.rs`), which one
  archive now almost fills.
- Zakura on x86-64 needs BMI2 and ADX (checked with `is_x86_feature_detected!`); without
  them the plan says "needs Zakura's portable build".

**The heavy-job gate.** A `tokio::sync::Semaphore` owned by the registry, one permit per heavy
job.

- A start takes a permit when the node never synced, is more than 10,000 blocks behind the
  herd's top, restores a snapshot, converts its storage, or is the wallet backend (whose
  restart re-checks its spending index, zebra #11090).
- Waiting, the node is in a new phase, `Queued { behind }`, which names the node holding the
  permit and offers Start now: "One heavy job at a time on this laptop: Zebra · Mainnet is
  syncing, 62 %".
- `start` returns as soon as the node is `Queued`, and while it waits the node holds no permit
  of the app's data gate (`start_permit`); `Queued` is not settled, so data commands ask to
  cancel the start first. A restart inside `apply_config` that would queue is refused instead
  ("Zebra · Mainnet waits for Zakura · Mainnet to finish; apply then, or Start now"), so the
  rollback window never commits a file zebrad hasn't read. The post-update health gate counts
  its 5 minutes from when the node leaves `Queued`, as it already restarts the count once a
  stale zebrad is resolved.
- The permit drops when the node passes probation at the tip ([states](#states-of-a-node)), or
  its own progress reaches 0.9999 whatever its vote or relation (observers, validators and
  nodes on another branch included), or it stops, fails or is removed.
- No deadlock: a node waits only before it starts, never while it holds a permit; a running
  node is never stopped to free one; "Start now" bypasses the gate and marks the node "over
  budget".
- While a heavy job runs, the other nodes' lag stays at attention: "Your computer is busy
  setting up Zakura · Mainnet 2".

## Agreement

"Connected to the network several times over" makes three claims. Zeal checks each one with
loopback RPC to the user's own nodes, and asks no outside service:

1. **Same chain**: two or more independent witnesses are on one chain, within a block or two
   of each other, compared by block hash, never by height alone.
2. **Moving**: that chain gains blocks at about the rate the block spacing predicts, so the
   nodes aren't agreeing on a stale view.
3. **Independent views**: the witnesses reach the network through many different peers
   ([Peer diversity](#peer-diversity)).

### Inputs

- **The tip.** The monitor's 5 s poll already calls `getblockchaininfo`, which returns
  `blocks` and `bestblockhash` from one state read (`ReadRequest::TipPoolValues` in
  `zebra-rpc/src/methods.rs` at v7.0.0; Zakura 1.8.0's handler reads the same). The monitor
  publishes the two together as `NodeStats.best: Option<BlockRef>`, so the herd adds no tip
  polling and never pairs a new height with an old hash. When that read fails, zebrad answers
  height 0 and the genesis hash; a node that was at the tip and suddenly reports genesis is
  `Unknown` for the round, not a reorg.
- **Static facts** from the spec: kind, family, vote, role, network, P2P port.
- **From the runtime's watches**: the phase, whether the stats are fresh (younger than
  `STALE_AFTER`, 30 s), `verification_progress`, the peer count in `NodeStats.peers` every
  round, and the peer list in `NodeDetails.peers` every 30 s for diversity. The monitor
  already polls all of them.
- **Probes**: `getblockhash(h)`, one index read on the node's best chain
  (`ReadRequest::BestChainBlockHash`). Above the node's tip it fails with −32602 ("Provided
  index is greater than the current tip"); a missing block is −8 ("Block not found").
  Zakura's is the same read.
- **Not used to decide**: `getblockheader`. In 7.0.0 it resolves a hash only on the best
  chain and the finalized database (`ReadRequest::BlockHeader` in `zebra-state/src/service.rs`),
  so a side-chain hash most likely fails with −5 ("block height not in best chain"), and
  `confirmations: -1` appears only when the block leaves the best chain between its two reads.
  Zeal's earlier notes say −1 off the best chain; a contract test pins the answer and nothing
  depends on it. `chainwork` is always 0, and zebrad has no `getchaintips`.

### A round

One actor per network. A round runs when a witness's tip changes (changes within 250 ms are
taken together), and every 5 s for the timers.

1. **Take this round's tips.** Equal hashes need no probe. If no tip changed and no fork is
   open, only the timers advance.
2. **Place each distinct tip, highest first** (then by node count, then by id). For a tip t,
   ask the top node of each known branch, largest first, for `getblockhash(height(t))`.
   - Equal: t is on that branch, behind its top by the difference in height.
   - Different from every branch: t starts a new branch, and a fork search runs against the
     largest branch's top.
   - An error or a timeout decides nothing: the round is incomplete and the next one tries
     again. Height never decides by itself.
   - A node syncing far below the others is placed with one read: its tip is deep in their
     history.
3. **Check for a reorg.** A node whose tip changed is asked for its hash at its previous
   tip's height (if it is now lower, at the remembered tips below). A different hash is a
   reorg; its depth comes from the node's last 16 remembered tips, "at least N" when they run
   out.
4. **Corroborate, while a branch is open.** A node that has `getchaintips` (Zakura 1.8.0, the
   compatibility build) is asked once, and the status it gives the other branch's top is
   recorded: `valid-fork` (it saw the block and accepts it), `invalid` (it rejects it),
   `headers-only`, or absent (it never saw it, which fits blocks held back). Whether Zakura
   lists a block it rejected on consensus grounds as `invalid` is unconfirmed; its doc comment
   mentions "recently invalidated branches".
5. **Did tips move meanwhile?** Then the round's fork conclusions are dropped and the round
   runs once more.
6. **Classify, debounce and publish** the `HerdView` and the new events.

Load: each node allows two probe calls in flight (a `Semaphore`), each with a 3 s timeout, so
probes never queue behind the monitor's poll. In the steady state a round costs about one
`getblockhash` per node behind the top, about once per block. Answers more than 1,000 blocks
below the top are cached across rounds, since nothing that deep changes.

### The fork search

For two nodes A and B whose tips are not on one chain: `hi`, the lower of their heights, is
where they differ (the failed placement). `lo` is the pair's last fork point if they still
agree there (two calls), otherwise `hi − 1000`.

1. Walk down `hi − 1`, `hi − 2`, `hi − 3`. Most races end here.
2. Gallop down by 4, 8, 16 and so on, as far as `lo`.
3. Bisect between the last height where they agree and the first where they differ.

Each step asks both nodes for one hash. Two chains that agree at a height agree at every
height below it, because each block commits to its parent, so the bisection is exact. The
result is the fork point f, the last common height, and each side's branch length (its
height minus f). If the nodes differ even at `hi − 1000`, the fork is **deep**: below
zebrad's rollback window (`MAX_BLOCK_REORG_HEIGHT = 1000`,
`zebra-chain/src/parameters/constants.rs` at v7.0.0), so neither node can rejoin by itself.

Cost: about 20 steps, 40 calls, at worst, and only when a branch appears; two to four calls a
round while it stays open. The work on each branch, summed from the headers' `bits` (at most
120 headers a branch), stands in for the missing `chainwork` as a hint. It never decides.

### Support and splits

- A branch's **support** is the number of witnesses whose tips lie on it above its fork point
  with the rival branch. Nodes at or below the fork point are on the shared trunk and count
  for neither side. A witness ahead of the others on the same chain is never in the
  minority: the others are behind it.
- The **main branch** has the most support. It is a **majority** when its support reaches
  half the witnesses at the tip, rounded down, plus one. Witnesses at the tip are those past
  probation, lagging, stalled and forked ones included, catching-up ones not. "k of n" on the
  panel counts every running witness, so "2 of 3 nodes agree · 1 is catching up" is a 2-of-2
  majority.
- **A split that lines up with implementations has no majority.** When every witness of one
  family is on one side, every witness of another family is on the other, and one side holds
  two or more witnesses, Zeal reports "Zebra and Zakura disagree since block H", a problem
  shown first, and counts no nodes. With an archive zebrad and two pruned Zakura nodes, the
  count would only reflect how many of each the user runs, and would call the archive "the
  minority" in what is a disagreement about the rules. The same holds for a stall: when every
  node of one family (two or more) stops at the same height while another family moves three or
  more blocks past it on the same chain, Zeal reports "Your Zebra nodes stopped at block H
  while Zakura moved on: Zebra may reject block H+1".
- **Two witnesses of different implementations** that split can't tell the causes apart, and
  the copy says both: "Zebra · Mainnet and Zakura · Mainnet disagree since block H: one of
  them may have lost touch with the network, or they disagree about the rules."
- **A side whose nodes started from the same snapshot** gets the note "the nodes on this side
  started from the same snapshot": they share its trust below its height.

### States of a node

S is the block spacing ([thresholds](#block-spacing-and-thresholds)). A node changes state
only when the new state's condition has held for 10 s across at least two rounds, and leaves
an alarm only after 10 s clean; until then it keeps its state, while its word shows the
current numbers. Excluded applies at once.

| State | When | Shown as | Level |
| --- | --- | --- | --- |
| Excluded | stopped, starting, restarting (still counted in n), stats stale, halted at its end of support, on another network, a service or a seeder, or not counted | "not checking (stopped)" | none |
| CatchingUp | not through probation yet: progress below 0.9999, or not yet within 2 blocks of the top for 2 min straight; or more than 1,000 blocks behind while its tip moves. A Stalled node stays Stalled however far behind it falls. A restart resets probation; a node already within 2 blocks when Zeal first sees it needs only 10 s | "catching up 34 % · same chain" | none; a problem if its placement read shows another chain ("another network or a damaged database") |
| InAgreement | on the main branch, within 2 blocks of its top | "agrees", "1 block behind" | ok |
| Racing | off the main branch, where the longer side is 1 block past the fork point at any age, or 2 blocks for less than 3 S | "agrees"; the event log says "competing block at H (normal)" | ok |
| Lagging | past probation, on the main branch, more than 2 blocks behind for 4 S, and still advancing | "15 blocks behind" | attention; a problem at more than 12 behind for 12 S |
| Stalled | tip unchanged for 5 S while the main branch gained 3 or more blocks. This includes a node still holding one block the others replaced: it is placed at the fork point and judged like a node behind, its unchanged time runs from when it took that block, and until 5 S have passed it keeps its last state | "stuck at block H", "stuck on block H, which your other nodes replaced" | attention; a problem at 24 S |
| Forked | off the main branch with a branch of its own at least 2 blocks long, beyond the race tolerance (the longer side 3 or more blocks past the fork point, or 2 for 3 S) | "on a different chain since block H" | a problem, shown first if the fork is deep, or if a witness reports the other branch `invalid` |
| Rejected (validators) | stuck at H while its parent is 3 or more blocks past H for 3 S | "zcashd rejects block H+1" | a problem, shown first once its `getchaintips` shows H+1 `invalid` (unconfirmed for the compatibility build) or the parent is 12 blocks past |
| Unknown | probes failed for 2 rounds, or a sudden genesis tip | "not answering" | attention after 30 s |

Progress alone can't separate these states: at 3.5 million blocks, 0.9999 is about 350
blocks, the same reading 3 or 300 blocks behind (`SYNCED_PROGRESS` in `ui/src/format.rs`).
Block counts decide, and probation keeps CatchingUp and Lagging apart.

### States of the herd

One per network:

| State | When | Headline |
| --- | --- | --- |
| Off | fewer than 2 nodes with a vote | no panel |
| Checking | no witness through probation | "Checking your nodes…" |
| Single | one witness at the tip | "One node checks the chain" |
| Agreed | every running witness is InAgreement or Racing | "Your 3 nodes agree" |
| Degraded | a majority agrees; the others are behind, stuck, catching up, restarting or not answering | "2 of 3 nodes agree · 1 is catching up" |
| Split | a witness is forked | "1 of your 3 nodes is on a different chain", "Zebra and Zakura disagree since block H", or "Your nodes disagree, and Zeal can't tell which is right" |
| Stalled | no witness at the tip saw a new block for 12 S (attention) or 24 S (a problem); never in the first 12 S after launch or a wake | "No new block for 6 min" |

When several rows hold, the first of Off, Checking, Split, Stalled, Single, Degraded and Agreed
applies.

A validator line rides along with every state: "zcashd accepts their blocks", "zcashd
rejects block H+1", or nothing.

**When the whole herd stalls**, Zeal can't tell a stalled network from an eclipse of the
user's address without an outside reference. `getpeerinfo` lists only peers heard from in the
last 119 s (`recently_live_peers`, `zebra-rpc/src/methods.rs`), so the signal is that count. If
it collapses toward 0 on every node: "Your nodes stopped hearing from their peers: check this
computer's internet connection." If it holds: "Your nodes still hear from 58 peers: the
network may be slow, or every peer is holding blocks back. Zeal can't tell which from here."

### Block spacing and thresholds

- S is the target spacing at the herd's top: 75 s below NU7 (Mainnet 3,543,405, Testnet
  4,465,026) and 25 s from it (`POST_NU7_POW_TARGET_SPACING`,
  `zebra-chain/src/parameters/network_upgrade.rs` at v7.0.0; ZIP 218). Nothing hard-codes 25
  or 75.
- **Every time threshold uses S_eff**: the mean gap between recent blocks at the herd's top,
  held between S and 3 S. Once Zeal has seen 30 new top blocks arrive, it takes the gaps it
  measured with its own clock over the last 100 of them; until then it reads the header times
  of the top block and of the block 100 below it (two `getblockheader` calls). It recomputes
  at every new top block, from finished gaps only, so a stall in progress never raises its
  own threshold.
- Why: ZIP 218 describes no rescaling of the difficulty at NU7. The spacing adjusts to the
  new target "at the normal rate", and the first adjustments are limited by
  `PoWMaxAdjustDown`, so blocks should come more slowly than every 25 s for a while after
  activation (how long is unconfirmed). A drop in hash power does the same until the
  102-block averaging window catches up. The cap at 3 S keeps a long slowdown from hiding a
  real stall.
- On Regtest every time rule is off.

| Rule | Value | At S = 25 s | At S = 75 s | Basis |
| --- | --- | --- | --- | --- |
| Round | when a tip changes, and every 5 s | | | the monitor's 5 s poll |
| Debounce | 10 s across 2 rounds | | | a reading taken while a block arrives must not flip a state |
| Agreement tolerance | 2 blocks | | | zebrad's `ready_max_blocks_behind = 2` (fixture `zebrad-7.0.0-generate.toml`) |
| Race | the longer side 1 block past the fork at any age; 2 blocks for less than 3 S | 75 s | 225 s | ZIP 218 puts the stale rate at 25 s at about 3.26 % in theory and 4.86 % on a 99-node devnet: a competing block every 20 to 30 blocks, settled by the next one. About 5 % of next blocks take longer than 3 S, so a single competing block never times out |
| Lag | more than 2 behind for 4 S (attention); more than 12 for 12 S (a problem) | 100 s; 5 min | 5 min; 15 min | |
| Node stall | 5 S unchanged while the branch gained 3 (attention); 24 S (a problem) | 2 min; 10 min | 6 min; 30 min | `STALL_ATTENTION` and `STALL_PROBLEM` in `ui/src/health.rs`, at 25 s |
| Herd stall | 12 S (attention); 24 S (a problem) | 5 min; 10 min | 15 min; 30 min | the chance of no block for k S is e^−k. At 3,456 blocks a day, 12 S happens by chance about once in 47 days (on the panel only); 24 S about once in 20,000 years, so an alert means a real outage |
| Probation | 2 min within tolerance; 10 s for a node already within tolerance when first seen | | | ends the flapping at the end of a sync, without 2 min of "checking" after every launch |
| Startup and wake grace | 12 S | 5 min | 15 min | when the last block came before Zeal looked is unknown |
| Reorg events | depth 1 or 2 info, 3 to 9 attention, 10 or more a problem | | | 10 is Zallet's default confirmations for untrusted outputs |
| Deep fork | the forked node's own branch longer than 1,000 blocks (its height minus the fork point), past its rollback window; always the case when the two nodes differ at `hi − 1000`. `Forked.deep` is `own > 1000` | | | `MAX_BLOCK_REORG_HEIGHT` |
| Testnet | races up to 6 blocks; stalls at attention only; no alerts by default | | | minimum-difficulty blocks make sibling forks common |

Timing rules against false alarms:

- A syncing node never raises herd lag or stall alarms; its own health rules cover it. Its
  one placement read still answers "same chain or not".
- A node Zeal or the user is restarting is `Excluded { why: Restarting }` until its stats are
  fresh again: it is in no branch's support but still counts in n, so the herd shows Degraded
  ("2 of 3 nodes agree · 1 is restarting"), never Agreed or Split.
- A wake is a round that arrives more than 30 s late by the wall clock. Durations use the
  monotonic clock, which stops during suspend on Linux and macOS (unconfirmed on Windows).
  After a wake the nodes read "catching up after sleep" for the grace.
- A fork point within one block of an activation height gets the note "at NU7's activation:
  this node may not know NU7". An end-of-support halt (`Failed{EosHalt}`) excludes the node,
  which has its own banner.
- A node the user stopped never alerts. After "Stop all" the panel reads "Not checking: your
  nodes are stopped".
- The thresholds are starting values. From the first live phase the backend logs locally how
  often each attention and problem condition fired, and only that evidence moves them.

### Examples

Mainnet after NU7, S_eff = 25 s.

1. **Normal.** A (an archive zebrad) and B (a pruned Zakura) are at 3,561,204 `…a41f`; C (a
   pruned Zakura) is at 3,561,203. A answers `getblockhash(3,561,203)` with C's hash. "Your 3
   nodes agree"; C reads "1 block behind".
2. **A race.** A and B hold block X at 3,561,205, C holds Y at the same height. A's hash at
   3,561,204 is Y's parent, so the fork point is 3,561,204 and each side is 1 block long: C is
   Racing and reads "agrees". Twenty seconds later all three are on a child of X, and the
   event log reads "Competing block at 3,561,205; settled in 20 s (normal)". Had A reached
   3,561,207 while C kept Y with nothing on top, C would be Stalled "on block 3,561,205, which
   your other nodes replaced"; had C's side grown to 3 blocks, Forked.
3. **A node cut off.** C is 40 blocks behind, gaining a block a minute: Lagging (a problem)
   with the flag Thin (3 outbound peers, all in one /16), so "Zakura · Mainnet 2 may be cut
   off: 40 blocks behind, 3 peers, all in one network", with Reset peers and restart, and Show
   peers.
4. **Syncing.** D, a new zebrad at 1,200,000, gets one read: A's hash at 1,200,000 is D's
   tip. D reads "catching up 34 % · same chain (checked at block 1,200,000)", doesn't vote and
   raises nothing.
5. **One implementation stops.** The two Zakura nodes are at 3,561,520 and the archive zebrad
   is stuck at 3,561,500 on the same chain. With one zebrad it is that node's stall: "Zebra ·
   Mainnet is stuck at block 3,561,500 while your other nodes moved 20", with Restart, and "if
   it stays stuck, Zebra may reject block 3,561,501: check for updates". With two zebrads stuck
   at the same block it is the herd's: "Your Zebra nodes stopped at block 3,561,500 while
   Zakura moved on", a problem, shown first.
6. **Implementations disagree.** The archive zebrad follows X from 3,561,498, both Zakura nodes
   follow Y, and both sides grow. The split lines up with implementations: "Zebra and Zakura
   disagree since block 3,561,498", a problem, shown first, with no majority. If Zakura's
   `getchaintips` marks X `invalid`, the alert says Zakura rejects Zebra's block.
7. **No majority.** Two zebrads: A at 3,561,500 on X, B at 3,561,501 on Y, fork point
   3,561,497. "Your two nodes disagree since block 3,561,498, and Zeal can't tell which is
   right."
8. **The validator rejects a block.** The compatibility build V follows A. A goes from
   3,561,300 to 3,561,305 while V stays at 3,561,300 for 75 s: "zcashd stopped following Zebra
   · Mainnet at block 3,561,300" (a problem), then "zcashd rejected block 3,561,301 that Zebra
   accepted" (a problem, shown first) once confirmed. The witnesses still read "Your 3 nodes
   agree"; the validator line reads "rejects".
9. **Just after NU7.** Blocks come more slowly than every 25 s while the difficulty adjusts.
   S_eff follows the measured gap, up to 75 s, so the herd-stall line waits up to 15 min
   instead of 5.

### What the herd protects against, and what it does not

| Threat | A herd on one computer |
| --- | --- |
| One node stalls at the tip: a bug, or peers that stop sending it blocks | **detects it**; a restart usually fixes it |
| One node lags: CPU, disk, poor peers | **detects it** |
| One node's peers are taken over: a poisoned address book, its slots filled | **detects it** while the others aren't taken over too, and raises the cost: each node has its own address book and peer cache (once Zeal stops sharing the cache, [Peer diversity](#peer-diversity)), and staggered restarts remove the restart that the attack of Heilman et al. waits for. The same botnet can still sit in every node's inbound slots, and the same gossip reaches every address book |
| A consensus bug in one implementation (2026's Zebra and zcashd divergences; the 2013 Bitcoin split in BIP 50) | **detects it**, if the herd mixes zebrad with Zakura (partly independent: a fork of Zebra) or with the zcashd compatibility build (mostly: its block and transaction checks are its own C++, but zebrad 7.0.0 checks transparent scripts with zcashd's C++ interpreter through `libzcash_script`, and the compatibility build's shielded proof and signature crates are Zakura's forks (`zakura-orchard`, `zakura-sapling-crypto`, `zakura-bellman` in `zebra-compat-v1.3.0`) of the libraries zebrad uses) |
| Poisoned DNS seeders at bootstrap | **partly**: different seeders per node, the peer caches, the census |
| A bug in code every node shares, Zakura's and zebrad's shared code included, and the script interpreter and proof libraries the compatibility build shares with them | no |
| An ISP, a hostile Wi-Fi network, a BGP or AS-level attacker (Apostolaki et al. 2017; Tran et al. 2020) | **no**: every node has the same address and route and sees what the attacker shows |
| Majority hash power, deep reorgs | no; it reports the reorg |
| A compromised computer, or a wrong clock | no; they are shared |
| A tampered snapshot that keeps its node on the honest chain | no: comparing tips can't see a fabricated UTXO or note, and nodes restored from one snapshot share its trust below its height. Hence a witness from the first block, and a wallet backend from the first block |
| The herd's own load: one heavy sync slowing the rest | not a threat but a source of false alarms: lag stays at attention while a heavy job runs |

The panel's honesty line: "Your nodes check each other, so one that falls behind, forks or
loses its peers shows up here. They share this computer's internet connection, so someone who
controls that connection could mislead all of them at once."

## Peer diversity

### One connection per address

zebrad 7.0.0 keeps at most `max_connections_per_ip` peers (default 1) per IPv4 address or IPv6
/64, inbound and outbound together, and drops a newcomer over the limit (`num_peers_with_ip` in
`zebra-network/src/peer_set/set.rs`); Zakura 1.8.0 does the same per exact address
(`crates/zakura-network/src/peer_set/set.rs`). Both refuse an inbound attempt from an address
that tried in the last 119 s (`RecentByIp`, `MIN_PEER_RECONNECTION_DELAY`). Both let the zcashd
compatibility sidecar skip the 119 s rule through a reserved inbound slot, but only zebrad also
exempts it from the per-address limit, so a sidecar on a Zakura parent can be dropped while its
previous connection still counts. So:

- All the user's nodes behind one address split the reachable network between them: each
  remote Zebra or Zakura node keeps one of them. More local nodes don't add reach; they
  divide it. This replaces MULTI-NODE.md's warning on adding a second node of a network with
  a line in the plan's bill.
- Their remote peer sets are disjoint by construction. Exact-address overlap between two
  nodes should be 0; a nonzero count means a peer that isn't Zebra-family, a raised limit or
  a VPN. What matters is the union and how widely it spreads.
- The first node to reach a remote keeps it, and the others waste handshakes on it.
  Staggered starts and smaller targets for the nodes after the first help.
- The herd spends up to the sum of its outbound limits from one address (each zebrad its limit,
  each Zakura 80 % of its target until it needs more), on a network whose size nobody has
  measured lately (an undated forum figure says about 140 reachable nodes; the census measures
  it).

### What Zeal measures

Per witness, every 30 s, from `NodeDetails.peers`. That needs no new RPC: zebrad and Zakura
return real addresses over RPC and redact them only in their logs.

1. Set aside loopback, link-local, private (RFC 1918, ULA) and unspecified addresses. One that
   matches another herd node's P2P port raises PeeredWithHerd. `::ffff:a.b.c.d` counts as IPv4.
   Also set aside peers whose IP is one of this computer's addresses: its interface addresses
   and, on the server preset, the public address the user gave. Such a peer on another herd
   node's P2P port, or an inbound peer from such an address, raises PeeredWithHerd.
2. Count outbound and inbound peers. The trust numbers use outbound peers only: Zebra's own
   constants warn that "an attacker can easily become a majority of a node's peers" through
   inbound connections (`INBOUND_PEER_LIMIT_MULTIPLIER`, `zebra-network/src/constants.rs`).
   Behind NAT, 0 inbound is normal.
3. The outbound groups: IPv4 /16 and IPv6 /32, the grouping zeeder's ADR 0007 uses.
4. `top_share`: the largest group's share of the outbound peers.
5. The user-agent mix ("/Zebra:7.0.0/ 70 %"), and how many peers advertise `NODE_NETWORK`,
   for which the monitor's peer decoding gains `services`.

Per herd: distinct peers, distinct groups, groups reached by two or more nodes, groups only
one node reaches, and exact overlaps.

### Flags

Starting values, at attention only until they are calibrated on real herds:

| Flag | Rule | Shown as |
| --- | --- | --- |
| NoPeers | 0 peers for 2 min (`NO_PEERS_FOR`) | "no peers" |
| Thin | fewer than 8 outbound peers or fewer than 6 groups, for 5 min; 4 and 3 when three or more herd nodes share the address | "few peers" |
| Concentrated | `top_share` above 25 %, with 8 or more outbound | "relies on one network" |
| InboundHeavy | more inbound than outbound peers, and fewer than 8 outbound | "mostly peers it didn't choose" |
| PeeredWithHerd | one of its peers is another herd node | "connected to your own Zakura · Mainnet: their views aren't independent" (info) |
| Herd thin | fewer than 12 groups across the herd, after zeeder's 12-group quorum | "Your nodes reach only 7 networks; 12 or more is better" |

A flag alone never raises an alert. A flag together with a lag problem, held for 10 s, makes
**"may be cut off"**: a problem alert whose first action is Reset peers and restart. A forked
or stuck node keeps its own rule.

In the copy, a "network" is an address block (/16), roughly one provider or region. The panel
never shows addresses; the herd page shows groups (`a.b.0.0/16`) and counts; Copy report
carries counts only unless "include peer networks" is ticked.

### Settings per role

Zeal writes these so the nodes' views stay independent:

| Setting | Primary archive | Other archives | Pruned (Zakura) | Seeder |
| --- | --- | --- | --- | --- |
| `network.cache_dir` (managed, new; a migrated node may keep its own `false` or path) | `<node>` | `<node>` | `<node>` | `XDG_CACHE_HOME=<node>/cache` |
| `network.listen_addr` (managed) | `[::]:8233` | `[::]:<assigned>` | `[::]:<assigned>` | fixed by zeeder |
| `network.peerset_initial_target_size` (written once) | 25 | 12 (zebrad), 16 (Zakura) | 16 | zeeder's own |
| `network.initial_*_peers` | Zebra's public seeders, unchanged | 3 of the 5 Mainnet seeders (all 3 Testnet ones), rotating | the same rotation | Zebra's defaults |
| Zakura `network.identity_dir` (managed) | `<node>/identity` for a Zakura primary | `<node>/identity` | `<node>/identity` | — |
| `network.max_connections_per_ip` | 1 | 1 | 1 | 1 |

- **A peer cache per node (a prerequisite).** zebrad's default `network.cache_dir = true` means
  "all Zebra instances run by the same user will share a single peer cache"
  (`zebra-network/src/config.rs` at v7.0.0), and Zeal manages only `state.cache_dir`
  (`crates/launcher-core/src/config.rs`). Every zebrad the user runs, and a zeeder, would start
  by dialling the same 75 addresses. Zeal manages `network.cache_dir = <node>` for every zebrad
  and Zakura it creates, and for a migrated node whose file had `true` or no key; a migrated
  node whose file says `false` or a custom path keeps that value as its own key, which
  `config::ensure` leaves alone. zebra-network appends `network/<net>.peers` to it, and Zakura
  does the same (`peer_cache_file_path` in `zebra-network/src/config/cache_dir.rs`), so the file is
  `<node>/network/<net>.peers`; `peers.rs` derives the path the same way, with a unit test that
  pins it. On migration a v2 node keeps what its `zebrad.toml` says:
  `network.cache_dir = false` stays false (there is no peer file, and Reset peers is refused
  for that node), a custom path stays its cache folder, and `true` or no key becomes `<node>`,
  with a copy of the shared file from zebrad's default cache directory, made once and not
  moved. A custom `network.listen_addr` becomes the node's stored P2P address. Nodes Zeal
  creates get Zeal's values.
- **Zakura's identity.** Its `identity_dir` defaults to `~/.zakura`, shared by every Zakura
  node; Zakura's own docs say to keep it outside snapshot paths, or snapshots can clone a
  node's long-term P2P identity.
- **Spreading the seeders.** Setting `initial_mainnet_peers` replaces Zebra's five defaults
  (serde fills only missing keys), so Zeal writes the managed list in full: 3 of the bundled
  zebrad's 5 Mainnet seeders per node after the primary, rotating so that no two share the same
  three, computed at every start from the bundled zebrad's defaults (which the config-form
  schema test already pins). Each Mainnet node keeps a majority of honest seeders if one goes
  bad; Testnet nodes keep all three. Turning "Spread seeders" off hands the key back to the
  user.
- **Staggered starts.** Launch and "Start all" start a network's witnesses one at a time, the
  primary archive first; the next one starts when the previous has 8 outbound peers, or after
  2 min.
- **Rolling restarts.** "Restart one by one" restarts a node, waits until it agrees again (at
  most 10 min), then moves on. A launcher update, the only way the bundled zebrad changes,
  stops every node and relaunches Zeal (`src-tauri/src/updater.rs`), so it doesn't roll: the
  nodes come back through the staggered start. Restart one by one serves a node whose own
  program changed, such as a new zakurad file.
- **No peering between herd nodes by default.** Herd nodes never list each other in
  `initial_*_peers`. `apply_config` warns about an entry that is another node's loopback port,
  and about `max_connections_per_ip` above 1, which also turns off the outbound per-address
  check. Under the default a node can hold only one loopback peer anyway, and loopback
  addresses leak to remote peers through gossip.
- **Bootstrap from my archive** (a later phase; explicit and temporary). A new node syncs from
  the local archive with Zebra's isolated setup (`initial_*_peers = ["127.0.0.1:<archive
  P2P port>"]`, a target of 1, `crawl_new_peer_interval = "10d"`, from
  `book/src/dev/profiling-and-benchmarking.md`). It still validates every block itself, but
  it is an observer while it copies, because its view is the archive's. At the tip Zeal
  restores its public peers and restarts it once.
- **Reset peers**, on the node page and in alerts: stop the node, move
  `<node>/network/<net>.peers` aside as `<net>.peers.old`, replacing the one an earlier reset
  kept (or write a census slice, [The seeder](#the-seeder)), start it. Bans live in the node's
  memory, so the restart clears them too. Refused for a node without its own cache folder.
- **Removing a node** with its data deletes `<node>/state`, `network`, `identity`, `cache` and
  `run`, with `delete_node_data`'s safety rules (inside the allowed roots, symbolic links
  resolved), and a node id stays taken while any of them exists. Node settings says what each
  folder holds: peer addresses, and for Zakura its P2P key.
- **Never** give an internet-facing node a specific loopback or LAN `listen_addr`, which
  would be gossiped, and never use 8232, 18232, 18344, 8033, 18033, 16125 or 26125 as a P2P
  port.

## The seeder

### What zeeder is

zeeder ([ZcashFoundation/zeeder](https://github.com/ZcashFoundation/zeeder), v1.8.0,
2026-10-22) is the Zcash Foundation's DNS seeder, in production behind
`mainnet.seeder.zfnd.org` and `testnet.seeder.zfnd.org`.

- One process runs a zebra-network crawler per network and an authoritative DNS server with
  one zone per network. It keeps no chain state.
- An answer carries at most 25 shuffled A or AAAA records of **servable** peers: handshaked
  in the last 119 s, at or above the current protocol floor, advertising `NODE_NETWORK`,
  routable, on the default port, not learned from an inbound connection, not banned
  (`src/crawl/servability.rs`).
- It rate-limits each client address (10 queries a second, a burst of 20, excess dropped
  silently) and exports `zeeder_peers_servable{network,addr_family}`,
  `zeeder_peers_unservable{network,reason}`, `zeeder_peers_known`,
  `zeeder_min_protocol_version` and `zeeder_activation_*`, plus `/health` and `/ready`.
- Releases: Linux x86-64 and aarch64 tarballs of about 4 MB, with a `SHA256SUMS` signed
  keylessly with cosign, and Docker images. No macOS or Windows builds.

Three facts decide what it can do for a herd:

- **Its crawler's P2P listener is fixed** at `[::]:8233` for a Mainnet zone and `[::]:18233`
  for a Testnet zone (`ZcashNetwork::network_config` in `src/config.rs`), and zebra-network
  panics when the port is taken ("Opening Zcash network protocol listener … failed",
  `zebra-network/src/peer_set/initialize.rs`). It can't start beside a node on that network's
  default port.
- **It can't seed the herd from the herd's own address.** Its answers are the peers its
  crawler heard from in the last 119 s (zeeder #43), roughly the peers it is connected to now.
  Each of those remote nodes already gives this address its one slot, and refuses the herd's
  nodes while zeeder holds it.
- **zebrad can't be pointed at a DNS server.** An `initial_*_peers` entry is `host:port`,
  resolved through the OS resolver (`tokio::net::lookup_host`, `zebra-network/src/config.rs`),
  and `addnode` works only on Regtest. A local seeder reaches a node only as literal
  `IP:8233` entries that Zeal writes, or through an OS resolver forward for the zone, which
  needs admin rights and differs on every OS.

So zeeder is not how the herd finds peers in normal running: the five public seeders and each
node's own peer cache already do that. It gives the user three things:

1. **A census** of the reachable network, which nothing else gives Zeal without asking an
   outside service.
2. **A public seed** for other people's nodes, on a server with a domain.
3. **Fresh peers for a reset**, sequenced so that the one-connection rule can't bite.

### How Zeal runs it

- **A separate process, never linked in.** zeeder holds zebra-network's address book as
  `Arc<std::sync::Mutex<AddressBook>>`, the type zebra-network's `init` returns. Linking it
  would put that mutex in Zeal's own code, which `clippy.toml` bans, and zebra-network's
  panic on a taken port would take Zeal down with it. A child process also keeps zeeder's
  upgrade cadence apart from Zeal's: it must be updated before every network upgrade, and
  Mainnet NU7 nodes refuse zeeder 1.7.0 and older.
- **The binary.** A file the user provides, as MULTI-NODE.md prescribes for every kind but
  zebrad. Later, a verified download through the same cosign check Zeal's release pipeline
  runs on zebrad's Linux tarball, or bundling it on Linux as a second pinned sidecar ([Open
  questions](#open-questions-for-the-maintainer)).
- **The kind.** `NodeKind::Zeeder` (slug `zeeder`, program `zeeder`) with `NodeRole::Seeder`,
  on Linux only; elsewhere its card stays visible, greyed out: "no zeeder builds for this
  system". `PortRole` gains `Dns` and `Health`. One census node per network; a public node may
  carry both zones, since only one process can hold port 53 on an address.
- **Command and environment.** `zeeder -c <node>/zeeder.toml start`, run in `<node>/run`, an
  empty folder, because zeeder reads a `.env` file from its working directory and a malformed
  one fails the start (`src/main.rs`). Every inherited `ZEEDER__*` variable is removed, since
  they override the file; `XDG_CACHE_HOME=<node>/cache` keeps it off zebrad's
  `~/.cache/zebra/network/*.peers`; `NO_COLOR=1` and `RUST_LOG=info` are set, because an
  invalid `RUST_LOG` aborts the start. Before a start Zeal runs
  `zeeder -c <node>/zeeder.toml print-config` to check the file. It stops on SIGTERM.
- **The managed `zeeder.toml`**, whose sections reject unknown keys:

```toml
[dns]
listen_addr = "127.0.0.1:<dns port>"     # public mode: the LAN address the router forwards port 53 to
[zones.mainnet]
domain = "mainnet.seeder.zeal.invalid"   # an RFC 6761 name that never resolves; public mode: the user's zone
nameserver = "ns.seeder.zeal.invalid"    # must be outside the zone
ttl = 600
[metrics]
endpoint_addr = "127.0.0.1:<metrics port>"  # never 9999, zebrad's metrics default
[health]
endpoint_addr = "127.0.0.1:<health port>"   # never 8080
ready_threshold = 1                         # `/ready` only shows the crawler is up; Zeal sets the run's length
```

- **Ports.** DNS on UDP and TCP from 1053 up, never 5353 (mDNS); metrics and health on
  assigned loopback ports; and a **fixed** claim on the network's default P2P port, a new
  `NodePort.fixed` that `assign_ports` may not move.
- **The port claim** is an `add_node` rule. If a node holds the zone's default port, the dialog
  offers: move that node to an assigned port, seed Testnet only, or cancel. It shows evidence
  from the agreement actor's record of that node's inbound peers (its highest count in each
  hour of the last 24 h, kept in memory), and states the window Zeal actually saw: "Zebra ·
  Mainnet had no inbound connections in the 40 min Zeal has watched it", or "Zebra · Mainnet
  has 31 inbound peers now. DNS seeders list it on 8233; on 28233 they stop." It says 24 h only
  once Zeal has watched the node that long. A home server behind a router can keep the archive
  reachable as WAN 8233 forwarded to LAN 28233, with zebrad's `external_addr` advertising port
  8233 (unconfirmed how peers learn the port).
- **Exit classes.** "failed to load configuration" is `ConfigError`; "failed to bind DNS" and
  zebra-network's listener panic are `PortInUse`; anything else is classed by its exit code
  (`Crash`, with backoff).
- **Capabilities.** Metrics through Zeal's existing Prometheus parser, readiness from
  `/ready`, and its "crawler status" log line every 10 min. No RPC, no tip, no vote.
- **Version.** Zeal reads the version with `zeeder --version` when the binary is picked, and
  checks it again against the `version` label of `zeeder_build_info`. It compares that with the
  first zeeder release that supports the network's newest upgrade (Mainnet NU7: 1.8.0; Testnet
  NU7: 1.7.0). An older zeeder gets "This zeeder doesn't know NU7: update it".
  `zeeder_min_protocol_version` is the floor zeeder enforces on the peers it serves, and it
  stays at the previous upgrade's version until zeeder's own observer confirms the new one
  (zeeder `docs/network-upgrades.md`). The census card shows it next to
  `zeeder_activation_confirmed`, never as an update prompt.

### Census and public seed

| | Census (the default) | Public seed (servers only, a later phase) |
| --- | --- | --- |
| DNS | `127.0.0.1:<port>`, an `.invalid` zone | the user's delegated zone, on the LAN address, or on 1053 behind a router forward of WAN 53; binding `0.0.0.0:53` collides with systemd-resolved's stub resolver |
| Runs | 15 min at a time: on request ("Count the network now"), or every 6 h if the user turns that on; never all the time | always |
| Needs | nothing | a static public IP (asked, never inferred), an NS record for the zone, UDP and TCP port 53 reachable; the Zcash Foundation recommends at least two independent seeders |
| Costs | while it runs, up to 75 outbound connections from the herd's address, which take remote nodes' one slot from the herd's nodes; up to 64 probe handshakes a sweep while it confirms a network upgrade | 2 to 20 Mbps (0.65 to 6.5 TB a month); the IP published as a nameserver and kept by passive-DNS archives; a DDoS target the herd shares; amplification (a 50-byte query gets 450 to 750 bytes back), so its rate limit stays on |

### How the herd uses it

- **The census card** on the herd page: servable peers over IPv4 and IPv6, known addresses,
  unservable peers by reason, the protocol floor, when it last ran, and "at least about N
  reachable nodes; your nodes reach M". The crawler can hold only remote nodes the herd
  doesn't hold (the one-connection rule), so the herd's distinct outbound default-port
  `NODE_NETWORK` peers plus zeeder's servable peers is a lower bound. `zeeder_peers_known`
  (at most 5,000 addresses) is an upper bound on addresses, not on nodes. The card feeds the
  peer-target advice: "The network is small: lower the targets of your nodes after the
  first."
- **Peers for a reset or a new node.**
  1. Finish a census run, then stop zeeder.
  2. Wait 120 s, past remote nodes' 119 s window.
  3. Read zeeder's own peer file, `<node>/cache/zebra/network/<net>.peers`: at most 75
     addresses seen in the last 3 h. That needs no DNS client and meets no rate limit.
  4. Drop the groups other herd nodes already use.
  5. While the target node is stopped, write at most 25 addresses, spread over as many groups
     as possible, into its `<node>/network/<net>.peers`; zebrad merges that file with its DNS
     seeds at start.
  6. Start the node.

  The public seeders stay in its config: one crawl from one vantage point never replaces
  them. Zeal logs counts, never addresses.
- **Nodes on other machines**, for a public seed: their operators add
  `"seed.example.com:8233"` to `initial_mainnet_peers`, keeping Zebra's five defaults in the
  list, since setting the key replaces them. The setup flow shows that line.
- It never votes and never takes part in agreement.

### Security and privacy

- A public seed makes the home IP permanent public infrastructure, tied to the domain's
  registrar account. The crawler's user agent, `zeeder/<version>`, tells every peer it
  contacts that this IP runs a seeder, and rate-limit warnings log client IPs.
- The operator chooses peers for strangers, with no DNSSEC, and a lapsed or hijacked seed
  name lets someone else choose them. Zeal says so before public mode starts, shows
  `zeeder_dns_rate_limited_total`, and offers one action to stop serving publicly.
- Zeal can't test reachability from outside without contacting an outside service, so it
  shows the `dig` command to run from elsewhere.
- Getting into Zebra's default seeder list is a separate step that has to be earned (a pull
  request, judged against zcashd's DNS seed operator policy). A home seeder on a dynamic IP
  shouldn't ask, and the UI says so.
- zeeder serves private addresses it handshakes on the default port (its `NotRoutable` check
  covers loopback, unspecified and multicast addresses only), so herd nodes must never be
  pointed at each other by LAN address on 8233.

### Upstream asks

Filed with the census phase:

- a configurable crawler `listen_addr`, where port 0 is a listener that is never advertised;
- a cap on the crawler's peers, for census use;
- serving peers verified in the last 3 h rather than the last 119 s (#43);
- a JSON census endpoint listing peers with their group, version, user agent and start
  height: the only way to tell a stalled network from an eclipsed address;
- advertising no services (#106: it advertises `NODE_NETWORK` but never answers
  `getblocks`);
- macOS and Windows builds.

## Wallet and explorer

### What the wallet takes

The herd never changes Zallet's balances and never blocks anything by itself. It gives the
wallet module facts and a recommendation, computed in the backend.

- **The view.** `HerdView`, and a pure projection onto the wallet design's vocabulary,
  `wallet_view(&HerdView, wallet_node) -> WalletHerd`:

| Herd | `WalletHerd` |
| --- | --- |
| Checking | `Starting` |
| Single, or Off with one full node | `Alone`: "Cross-check off: one node" |
| Agreed | `Agree { tip }` |
| Degraded | `Lagging { node, blocks }` when the wallet's node is behind; `Isolated { node }` when it may be cut off; otherwise `Agree` |
| Split | `Split { fork_height, branches, side }`, where `side` is majority, minority, no majority or implementations |
| Stalled | `Stalled { since }`, a value the wallet design doesn't have yet |

- **`corroborate(network, height, hash) -> Corroboration`.** One `getblockhash(height)` per
  witness, sorted into `same` (with its depth), `behind`, `different` and `unreachable`. It
  works on pruned witnesses, because pruning keeps the height-to-hash index; that a live
  pruned node answers below its `pruneheight` is unconfirmed until the Zakura contract test
  runs. Answers are cached per node and height until that node's tip changes, and for good
  more than 1,000 below the top.
- **Confirmation states.** Pending: in k nodes' mempools. Confirming: below Zallet's policy
  depth (3 for trusted outputs, 10 for untrusted, `zallet-core/src/config.rs`). Confirmed:
  the wallet's node is at the depth, a majority of witnesses hold the same block, and none
  holds a different one beyond the race tolerance. Disputed: "Not confirmed by your other
  nodes", kept out of Spendable. Reorged out. The copy reads "12 confirmations · on 3 of 3
  nodes".
- **`mempool_seen(network, txid) -> (seen, of)`**, from `getrawmempool` on the witnesses:
  every 5 s for 2 min after a broadcast, then on demand. The wallet broadcasts once, through
  its own node. "Reached 2 of 3 nodes" means something only because herd nodes aren't peered
  with each other, so Zeal withholds it while any node is PeeredWithHerd. Broadcasting from
  every node would make the transaction easier to trace to this address (Biryukov and
  Tikhomirov, 2019).
- **Reorgs.** A reorg that touches a block holding a wallet transaction raises a wallet
  event; one at least as deep as the policy depth raises the herd's reorg alert.
- **The send check**, `send_check(wallet) -> SendCheck { level, reason, facts }`. The wallet
  module owns the decision and enforces it in launcher-core; the herd recommends:

| Situation (W is the wallet's node) | Recommended |
| --- | --- |
| Agreed, and W agrees | clear |
| Alone | a note, and "Add a pruned node" |
| Degraded, and W agrees | an inline note |
| W more than 2 blocks behind, stuck, or Isolated (possibly cut off); or the herd stalled | a warning: "may not confirm until your nodes catch up", with Reset peers and restart when W is Isolated |
| Split, with W on the majority side | a warning and an explicit confirm |
| Split, with W in the minority or no majority; implementations disagree; the validator rejects a block in W's chain | the strongest response: a typed override (`TypedConfirm`) that shows the fork height, the branch lengths, which nodes are on each side and the branch-work hint; automatic retries off |
| W syncing, or Zallet catching up | the wallet's own block; Zallet refuses anyway |

The wallet design asks for a hard block when the wallet's node is in the minority. This
design recommends the typed override instead. A local majority can be manufactured, or can
simply reflect the herd's make-up (two Zakura nodes outvote one zebrad in an implementation
split), and a hard block would let anyone who can upset the user's cheaper nodes freeze the
user's funds. The maintainer decides ([Open questions](#open-questions-for-the-maintainer)).

- **Privacy.** Loopback RPC with cookies only; no outside lookups.

### What the explorer takes

The explorer design gives each node an explorer tab that reads only that node. The herd adds:

- **A default explorer node per network** (`explorer_source` in the herd settings): the primary
  archive, otherwise any archive. Only archives qualify: a pruned node serves no history, and a
  pruned Zakura synced with checkpoints also has no transparent address indexes (#1366). A
  pruned node's tab shows "recent blocks only".
- **A banner** when the node being viewed isn't in agreement: "This explorer reads Zebra ·
  Mainnet, which is 15 blocks behind your other nodes", or "… which is on a different chain
  since block H: your other nodes disagree with what you see here", with Open herd and, when
  another archive agrees, "Switch to Zakura · Mainnet". Zeal never switches by itself.
- **"On 3 of 3 nodes"** on blocks within 1,000 of the top (the explorer's provisional
  blocks), from `corroborate`, fetched when the block page opens. Deeper blocks read "final for
  your nodes".
- **Cross-node events** in its fork log: "Zakura · Mainnet 2 held another block at 3,561,203
  for 20 s (replaced)".

## Backend

### Crates and modules

The engine has to run in the mock, which builds to wasm, and `launcher-core` can't (tokio,
reqwest). A module in `launcher-types` would turn the wire-types crate into a logic crate. So
the engine goes in a new pure crate (a maintainer decision):

```text
crates/launcher-herd/        new · no tokio · native and wasm32 · depends on launcher-types and serde
  roles.rs        Storage, HerdRole, Vote, Family, TrustOrigin; validate_spec, dependency_ok, role defaults
  spacing.rs      the target spacing at a height; S_eff
  agreement/      place.rs (placement, the sans-IO fork search), classify.rs (node states, debounce,
                  probation, grace, wake), verdict.rs (the herd's state, headlines, sentences)
  diversity.rs    groups, flags, the herd's union; each node's highest inbound peer count per hour
                  for 24 h, which the agreement actor keeps in memory
  alerts.rs       the alert list, derived from views and events
  plan.rs         recommend(goal, machine, existing); budgets; bring-up order
  wallet.rs       wallet_view, corroboration states, send_check
  explorer.rs     explorer_source
  report.rs       the plain-text herd report, never an IP address
  script.rs       feature "script": scripted chains and peers for the tests and the mock
crates/launcher-core/src/herd/
  mod.rs          HerdHandle, spawn
  agreement.rs    the actor, one per network
  tips.rs         the TipSource trait, RpcTipSource
  heavy.rs        HeavyGate
  peers.rs        per-node peer files: the copy on migration, a reset, a slice (on the blocking pool)
  census.rs       one actor per zeeder node: metrics, runs, slices
  machine.rs      MachineProfile, sharing the per-OS readers with resource monitoring
  ops.rs          one task per long operation (apply a plan, restart one by one, reset peers,
                  staggered start), started by new HerdCmd variants; each reads watch<HerdView>
                  to decide when to move on, drives the registry, and publishes progress on a
                  watch<PlanProgress>
crates/launcher-core/src/kinds/zeeder.rs   the adapter, on MULTI-NODE phase 4's machinery
src-tauri/src/herd.rs                      event forwarding only; the event names join
                                           launcher_types::events
```

The engine is sans-IO: it asks for probes and takes their answers, has no clock or randomness
of its own, and uses only ordered collections. Every transition is a native unit test with a
fake clock, and the mock runs the same code.

```rust
pub struct Engine { /* tips, branches, timers, the event ring */ }
impl Engine {
    pub fn new(network: Network, thresholds: Thresholds) -> Self;
    /// Starts a round with this round's facts and returns the first probes.
    pub fn begin(&mut self, round: RoundId, facts: Vec<MemberFacts>, now: Now) -> Vec<Probe>;
    /// Takes answers and returns follow-up probes (fork search, reorg depth); empty when done.
    pub fn answer(&mut self, round: RoundId, answers: Vec<(Probe, ProbeResult)>) -> Vec<Probe>;
    /// Classifies, debounces, and returns the view and the new events.
    pub fn finish(&mut self, round: RoundId, moved: bool, now: Now) -> (HerdView, Vec<ChainEvent>);
}
pub enum Probe { HashAt { node: NodeId, height: u32 }, ChainTips { node: NodeId }, HeaderTime { node: NodeId, height: u32 } }
pub enum ProbeResult { Hash(BlockHash), NotFound, AboveTip, Tips(Vec<ChainTip>), Time(i64), Failed }
/// Durations come from `mono`; `wall` only detects a wake.
pub struct Now { pub mono: Duration, pub wall: SystemTime }
```

Answers for an older `RoundId` are ignored, and a round asks at most 64 probes.

### Actors and channels

No locks: each actor owns its state, views travel on `watch`, slots are semaphores, and
`clippy.toml` already bans `Mutex` and `RwLock`.

```text
registry ──── watch<Vec<NodeSummary>> ──────────┐
node monitor ─ watch<Option<NodeStats>> (best) ─┼─▶ herd::Agreement[network] ─ watch<HerdView> ─▶ Tauri `herd`
node monitor ─ watch<Option<NodeDetails>> ──────┘     │ probes through TipSource              ├─▶ registry: heavy-gate release,
herd::Census[zeeder node] ─ watch<Option<Census>> ────┤ (a Semaphore(2) per node)             │   summaries, tray line
                                                      │                                       └─▶ wallet module, explorer actors
                                                      └─ broadcast<ChainEvent>(256) ─▶ Tauri `herd_event`
HerdHandle ── mpsc<HerdCmd> ──▶ Corroborate, MempoolSeen, SendCheck, Events{since}, Report   (each with a oneshot reply)
                                and one variant per long operation, each run as a task in herd/ops.rs
HeavyGate: an Arc<Semaphore> in the registry; each NodeRuntime holds its OwnedSemaphorePermit
```

- The registry starts an agreement actor for each network as nodes arrive (MULTI-NODE phase
  2) and sends it `MemberAdded { id, vote, family, p2p_port, status, stats, details, tips }`,
  `MemberChanged` and `MemberRemoved` on a bounded mpsc. A network with fewer than two voting
  nodes publishes `Off`.
- The actor `select!`s over the coalesced "a tip changed" (`watch::Receiver::changed` on each
  node's stats), a 5 s interval, its commands and the registry's messages. Probes run
  concurrently in a `JoinSet`, each taking its node's permit, each with a 3 s timeout.
- `TipSource` is a trait with three calls (`hash_at`, answering a hash, not found or above
  the tip; `chain_tips`, `None` where the kind has no `getchaintips`; `header_time`), so tests
  use a scripted `FakeChain`, the way the monitor takes `new_client`. `RpcTipSource` wraps the
  node's own `RpcClient`, which reads the cookie per session, and is rebuilt when
  `Status.rpc_addr` changes. The tip itself comes from `NodeStats.best`, not from the trait.
- A Tauri receiver that falls behind the `broadcast` gets `Lagged` and fetches the missed
  events through `HerdCmd::Events { since }`, the call behind `herd_events`. This is new: the
  log bridge only puts a gap marker in place of the lines it dropped.
- The census actor reads `/metrics` every 30 s while zeeder runs, runs the scheduled censuses
  (start; wait for `/ready`, at most 10 min, which only shows that the crawler has found a
  servable peer; let it crawl until 15 min after the start; read `/metrics`; stop), and keeps
  the last result's counts and time in `<app data>/herd/census.json`. "Count the network now"
  uses the same 15-min run.
- A panicked actor is started again by the registry, with the launcher line "herd checks
  restarted".

### Types

In `launcher-types`: serde, clean for wasm.

```rust
pub struct BlockRef { pub height: u32, pub hash: BlockHash }   // BlockHash: 32 bytes, hex on the wire
pub enum Storage { Archive, Pruned { tx_retention: u32 } }
pub enum HerdRole { Archive, Pruned, Seeder, Service, Wallet }
pub enum Vote { Witness, Validator, Observer, None }
pub enum TrustOrigin { Genesis, Snapshot { publisher: String, height: u32, sha256: String } }
// NodeStats gains `best: Option<BlockRef>`. NodeInfo gains `storage`, `origin` and
// `vote_override` (all Option, #[serde(default)]). NodePort gains `fixed: bool`. NodeKind gains
// Zeeder, NodeRole gains Seeder, PortRole gains Dns and Health.
// `Phase` gains `Queued { behind: NodeId }`, the node holding the permit it waits for; it starts
// without the user doing anything, so `Phase::is_active` counts it.

pub struct HerdView {
    pub network: Network,
    pub state: HerdState,              // Off, Checking, Single, Agreed, Degraded, Split, Stalled
    pub top: Option<BlockRef>,         // the main branch's top
    pub spacing_secs: u32,
    pub effective_spacing_secs: u32,   // S_eff
    pub checks: [Check; 3],            // same chain, moving, independent: a level and one sentence each
    pub nodes: Vec<NodeInHerd>,        // role, vote, family, origin, relation, level, word, sentence, peers, flags
    pub branches: Vec<Branch>,         // fork point, top, nodes, growing, work hint
    pub validator: Option<ValidatorLine>,
    pub diversity: HerdDiversity,
    pub census: Option<CensusSummary>,
    pub alerts: Vec<HerdAlert>,        // id, level, urgent, headline, reason, if_ignored, actions
    pub recent: Vec<ChainEvent>,       // the newest 20
    pub updated_at: SystemTime,
}
pub enum Relation {
    Excluded { why: ExcludedWhy }, CatchingUp { progress: f64, same_chain: Option<bool> },
    InAgreement { behind: u32 }, Racing { at: u32 }, Lagging { blocks: u32 },
    Stalled { at: u32, replaced_block: bool },
    Forked { at: u32, own: u32, main: u32, side: Side, deep: bool },
    Rejected { at: u32, confirmed: bool }, Unknown,
}
pub enum Side { Minority, NoMajority, Implementations }
// `ExcludedWhy` includes `Restarting`: a node being restarted still counts in n.
```

The levels are `Level` (healthy, neutral, attention, urgent, problem), moved from
`ui/src/health.rs` to `launcher-types` with its `word` and `tone` methods and given `Serialize`
and `Deserialize`; `health.rs` re-exports it, and the herd adds no level type of its own.
Problem is the highest; Urgent is attention with a deadline, such as an end of support days
away, shown with the word "Attention", and the herd doesn't use it. The herd's gravest
conditions (implementations that disagree, one implementation's nodes stopping together, a
confirmed validator rejection, a deep fork) are problems that the alert bar shows before the
other problems, marked by `HerdAlert`'s `urgent: bool`, which is not health.rs's Urgent.
`STALE_AFTER`, `NO_PEERS_FOR` and `SYNCED_PROGRESS` move to `launcher-types` too, so the UI and
launcher-core read one definition. The sidebar's `Tone` (`ui/src/node_summary.rs`) gains an
attention value for the herd words.

### Events

| Event | Payload |
| --- | --- |
| `herd` | one network's `HerdView`, at most once a second per network |
| `herd_event` | a `ChainEvent { seq, at, level, kind, nodes, text }`: a competing block, a race settled, a reorg, a fork found or resolved, a stall or lag and its recovery, the herd stalled or resumed, a validator rejection, a role mismatch, herd nodes peered, a wake |
| `census` | an `Option<Census>`, with its network |
| `herd_plan` | a `PlanProgress` while a recommended herd is being set up: each step's state and the time until the herd can cross-check |

The UI subscribes first, then calls `get_herd`, and drops a network's part of the snapshot
once that network's `herd` event arrives: ARCHITECTURE.md's rule for every snapshot.

### Commands

In `src-tauri/src/commands.rs`, beside the other commands, a few lines each over `HerdHandle`
or the registry; the long operations run in `herd/ops.rs`. Errors are strings, like the other
commands. build.rs's `check_command_lists` accepts only `commands::<name>` entries in
`generate_handler!`, and each command is also added to `COMMANDS` in `src-tauri/build.rs` and
allowed in `src-tauri/capabilities/main.json`.

| Command | Returns | Does |
| --- | --- | --- |
| `get_herd()` | `Vec<HerdView>` | the snapshot |
| `herd_events(network, since)` | `Vec<ChainEvent>` | the in-memory 24-hour ring (at most 2,000 events) after `since` |
| `herd_report(network, include_peer_networks)` | `String` | plain text, counts only by default; `copy_diagnostics` appends it |
| `corroborate(network, refs)` | `Vec<Corroboration>` | for the wallet and the explorer |
| `mempool_seen(network, txid)` | `(u32, u32)` | for the wallet |
| `send_check(wallet)` | `SendCheck` | the recommendation and its facts |
| `reset_peers(node, use_census)` | `()` | stop, move the peer file aside or write a slice, start |
| `restart_one_by_one(network)` | `()` | the rolling restart |
| `run_census(node)` | `Census` | one census run |
| `machine_profile()` | `MachineProfile` | for the setup flow |
| `recommend_herd(network, goal)` | `Vec<HerdPlan>` | the plans that fit, with their bills |
| `apply_herd_plan(network, plan)` | plan id | adds and starts the members in order; progress on a `herd_plan` event |
| `start_now(node)` | `()` | bypasses the heavy-job gate |
| `add_node`, `update_node` | as in MULTI-NODE | plus `validate_spec`'s problems: `ClaimsDefaultPort { held_by }`, `NeedsArchive`, `NeedsIndexerBuild` |

The herd settings travel in `set_settings`, in a new `herd` section of `launcher.json` v3.
Serde defaults mean files written before it load unchanged:

```json
"herd": { "agreement": true, "spread_seeders": true, "heavy_jobs": 1,
          "explorer_source": { "mainnet": "zebrad-mainnet" },
          "census": { "every_hours": null } }
```

## UI

### Principles

- **One node looks like today**: no panel and no herd words.
- **Plain words first**: "different chain" (the details say fork), "stuck" (stall), "may be
  cut off" (eclipse); node names, never ids; "block 3,561,498"; relative times, with the
  absolute time in a tooltip.
- **One cause per problem**, cause before symptom, and every warning says what to do.
- **Per network.** Each network has its own herd. The Mainnet and Testnet tabs (MULTI-NODE
  phase 1, `ui/src/components/sidebar.rs`) scope the window to one network, so the panel is the
  top of that network's overview.

### The overview panel

The panel shows for a network with two or more voting nodes. With one witness and other
nodes on the network, it shrinks to one line: "One node checks the chain · [Add a pruned
node]".

```text
MAINNET · Your 3 nodes agree                                                   [Open herd]
Block 3,561,204 · 9 s ago
 ✓ Same chain     all 3 on block 3,561,204                        zcashd accepts their blocks
 ✓ Moving         a new block 9 s ago (one about every 25 s)
 ✓ Independent    75 peers in 38 networks
   ● Zebra · Mainnet      archive · from the start     agrees           41 peers · 23 networks
   ● Zakura · Mainnet     pruned · from a snapshot     agrees           18 peers · 14 networks
   ● Zakura · Mainnet 2   pruned · from the start      1 block behind   16 peers · 12 networks
Your nodes check each other. They share this computer's internet connection. [What this means]
```

With a problem:

```text
MAINNET · 1 of your 3 nodes is on a different chain                            [Open herd]
 ✗ Same chain     Zakura · Mainnet 2, since block 3,561,498 (4 blocks); the other 2 agree
 ✓ Moving         a new block 12 s ago
 ! Independent    Zakura · Mainnet 2 has 3 peers, all in one network
 What to do: give it fresh peers.                    [Reset peers and restart]  [Details]
```

### The herd page

A "Herd" item under the network's Overview in the sidebar.

- **Header**: the state, the three checks, the software ("Zebra 7.0.0 · Zakura 1.8.0 ×2", or
  "the same software on every node"), which nodes were checked from the first block, and
  [Restart one by one] [Copy report].
- **Chain strip**: an inline SVG in `charts.rs`'s style, with keyboard navigation and a table
  view. It shows the last 12 or so heights with a marker per node, and draws a fork as a
  labelled branch ("different chain since 3,561,498", "competing block, settled in 20 s").
- **Nodes table**: role, word, tip, last new block, peers out and in, networks, networks only
  it reaches, user agent and trust origin; row actions Open, Reset peers and Explorer.
- **Reach**: the union figures, a bar per group showing how many nodes reach it, and an
  overlap matrix for up to 6 nodes.
- **Census**, when there is a zeeder.
- **Events** from the last 24 hours.
- **What this protects against**, collapsed but always there: the table above in plain words,
  ending "Everything here uses only your own nodes; nothing is sent anywhere."

### Sidebar, tray and node pages

- **Sidebar.** The network's tab gets a check or warning glyph. A witness at the tip shows its
  herd word instead of "at tip" ("agrees", "6 behind", "stuck", "other chain"), and its dot
  takes that word's tone.
- **Tray.** The tooltip on macOS and Windows reads "Zeal: 4 running · Mainnet: nodes agree".
  Linux's tray has no tooltip (`src-tauri/src/tray.rs`), so every OS also gets a disabled menu
  line: "Mainnet: 3 of 3 nodes agree", or "Mainnet: nodes disagree — Open…".
- **Node pages.** Header chips for the role, "not counted" and the trust origin; an "In the
  herd" card on the dashboard (the word and its sentence, the last check, the peer groups,
  Reset peers); and in Node settings the storage (one-way changes behind `TypedConfirm`), the
  retention as time, the peer target and "Count this node in agreement".
- **Health verdict.** In `ui/src/health.rs` the first matching rule wins, cause before
  symptom. Forked and the validator's Rejected go right after the silent-RPC rule (7) and
  before "no peers"; "may be cut off" just before "few peers"; the herd's Stalled before "no
  new block at the tip"; Lagging before "slow sync". An urgent end-of-support warning keeps
  its place above the attention-level herd rules. `STALL_AT_TIP`, which reasons in 75 s
  blocks today, and the other time rules derive from S_eff. `summary_text` gains the herd's
  lines, counts only.

### Alerts

- **The budget.** Problem conditions raise an alert, the gravest first, except Lagging's
  problem tier, which colours the panel and feeds "may be cut off"; attention lives on the
  panel and the herd page; info goes to the events. A healthy herd should raise no alert in a
  typical month, which dogfooding measures.
- **Every alert** has one cause, what happens if it is ignored, and at least one action, the
  recommended one first. Alerts are held back during probation, the startup and wake grace,
  restarts, stops the user asked for, and syncing, and are off on Testnet by default. An
  alert that comes back within an hour of resolving is the same alert, with a repeat count and
  no new banner.
- **Where.** An alert bar under the page header, one alert at a time ("1 of 2 ›"), and the
  list on the herd page. Alerts are part of `HerdView`, computed by the pure crate, so the
  mock shows exactly the same ones. Acknowledging and snoozing, the tray icon's state and
  opt-in OS notifications come with a backend alerts actor in a later phase.

| Alert | Level | Headline | If ignored | Actions |
| --- | --- | --- | --- | --- |
| a node on a minority chain | problem | "Zakura · Mainnet 2 is on a different chain since block H; your other 2 agree" | "Its checks don't count. If it backs your wallet, balances may be wrong." | Reset peers and restart · Show logs · Copy report |
| implementations disagree | problem (shown first) | "Zebra and Zakura disagree since block H" | "One of them disagrees about the rules. Don't rely on new payments until it's resolved." | Check for updates · Show logs · Copy report |
| one implementation stopped | problem (shown first) | "Your Zebra nodes stopped at block H while Zakura moved on" | "Zebra may reject block H+1. Don't rely on new payments until it's resolved." | Check for updates · Show logs · Copy report |
| no majority | problem | "Your nodes disagree since block H, and Zeal can't tell which is right" | "Payments may be undone when it settles." | Details · Copy report |
| the validator stopped following | problem | "zcashd stopped following Zebra · Mainnet at block H" | "It may reject block H+1, which would mean Zebra and zcashd disagree about the rules." | Open zcashd's log · Copy report |
| the validator rejects a block | problem (shown first) | "zcashd rejected block H+1 that Zebra · Mainnet accepted" | "Zebra and zcashd disagree about the rules. Don't rely on new payments until it's resolved." | Open zcashd's log · Check for updates · Copy report |
| a deep fork | problem (shown first) | "Zakura · Mainnet 2 can't rejoin the chain by itself" | "It stays on its own chain." | Start over from a snapshot (Zakura) · Remove |
| a node stuck | problem | "Zebra · Mainnet 2 is stuck: no new block for 10 min while your other nodes moved 24" | "Its checks don't count." | Restart · Show logs |
| a node may be cut off | problem | "Zakura · Mainnet 2 may be cut off: 40 blocks behind, 3 peers, all in one network" | "Someone may be showing it a different chain." | Reset peers and restart · Show peers |
| the herd stuck | problem | "No new block on any node for 10 min", with the peer-count sentence | "New payments won't confirm." | Show peers |
| a deep reorg | problem, once | "Your nodes replaced 12 blocks at H" | "Payments in those blocks are being checked again." | Details |
| a role mismatch | problem | "This archive deletes old transactions" | "The explorer and the wallet can't use it." | Node settings |

On the panel only, never as alerts: N blocks behind; few peers or relies on one network;
fewer than 12 networks across the herd; peered with your own node; no node checked from the
first block; the same software on every node (info); a seeder that is out of date; a pruned
node that keeps everything.

### Setting up a recommended herd

"Set up a herd…" sits in Settings → Nodes, in the tray menu and on the overview's one-line
card. It never nags.

```text
1 What for?      (•) Check the chain several times over   ( ) Run my own block explorer
                 ( ) Back a private wallet (Linux)         ( ) Help the network (a server)
2 This computer  Linux laptop · 8 cores · 16 GB · 640 GB free on /home · CPU runs Zakura ✓
                 [ ] This is a server with a public address
3 Recommended    Zebra · Mainnet    archive  you have it    keeps the whole chain; your explorer reads it
               + Zakura · Mainnet   pruned   25 GB · 3 GB · 12 GB download · ready in about 30 min
               [ ] A second pruned node      +25 GB · +3 GB   a tiebreaker: with 3 nodes Zeal can usually tell which one is odd;
                                                              if Zebra and Zakura disagree, it reports both sides
                 New: 25 GB of 640 GB free · memory at the tip 6 GB of 16 GB
                 Starts from Zakura's published snapshot, checked against its checksum only; the
                 snapshot's host sees your IP address. Your archive checked the chain from the
                 first block, so your herd keeps an independent check.
4 Programs       zakurad  [Choose file…]
5 Review         2 nodes, their ports, folders and config · [Set up]
```

After "Set up", the overview shows a plan card: "Setting up your herd · Zakura · Mainnet:
downloading the snapshot, 41 % · Zakura · Mainnet 2: waiting its turn (this laptop sets up one
node at a time) · Ready to cross-check in about 25 min". On macOS and Windows step 3 offers one
archive and says why. **Settings → Herd** holds the agreement checks, spreading the seeders,
heavy jobs at once, the census schedule and the default explorer node per network.

## Testing

**`launcher-herd`**, native and fast, with a fake clock:

- **The fork search**, on 10,000 seeded random block trees with forks 1 to 1,200 blocks deep
  (SplitMix64, as `updates::Jitter` uses; the tree has no proptest): it finds the true fork
  point, never asks above the lower tip, stays within its call bound, and reports deep forks.
- **States**: placement doesn't depend on the order of nodes; nodes on one chain are never
  Forked; a one-round blip changes nothing; examples 1 to 9 as scripted rounds; thresholds
  follow S_eff across the NU7 height and through a stretch of slow blocks; Testnet's
  tolerances; the startup and wake grace; short probation; reorg depth; a genesis tip from a
  node at the tip is Unknown; splits and stalls that line up with implementations, with one
  and with two nodes a side; the validator; observers left out.
- **Diversity**: IPv4 /16, IPv6 /32, mapped IPv4, local addresses, PeeredWithHerd by port,
  PeeredWithHerd by this computer's public address, the thresholds.
- **Alerts**: the debounce; cause before symptom; an alert that comes back is the same alert;
  every alert has an action and an if-ignored sentence, and a headline of at most 90 characters
  with node names of up to 20.
- **Roles and plans**: the `validate_spec` matrix; the reserved and refused ports; macOS and
  Windows never get Zakura; a laptop gets an archive and one pruned node, a desktop two; a
  disk that falls short drops the optional members and keeps the headroom; a CPU without BMI2
  or ADX gets the portable-build note; an existing archive is reused.
- **Wallet and explorer**: the projection, the corroboration states, the send-check table,
  the default explorer node.

**The `launcher-core::herd` actors**, under `tokio::test(start_paused = true)` (tokio's
`test-util` is already a dev-dependency of launcher-core) with `FakeChain`: at most two probes
in flight per node; a slow node doesn't hold up the others; nodes added or removed mid-round; a
tip that moves mid-round reruns the round; RPC errors give Unknown; replies to `HerdCmd`;
recovery after `broadcast` lag; the heavy-job gate (a second syncing node waits, "Start now"
bypasses it, a stop releases the permit, no deadlock); `peers.rs` in temporary folders (the
peer file's path, the copy on migration, a reset, a slice); each long operation in `ops.rs` (a
census-based reset waits 120 s, a rolling restart gives up on a node after 10 min, a plan stops
at a member that fails, a staggered start moves on at 8 outbound peers or 2 min).

**fake-zebrad** answers `getblockchaininfo` (its `blocks`, `bestblockhash`, and a
`verificationprogress` of 1 at the tip) and `getpeerinfo` from a scripted chain and peer list
(`FAKE_ZEBRAD_CHAIN=<json>`: branches and a timeline; `FAKE_ZEBRAD_PEERS`), and gains
`getblockhash`, `getblockheader` and `getchaintips`, answered from the same chain. An
integration test runs three fakes through the real `RpcClient` and the registry, and sees a
race settle, and a fork raise an alert and resolve, within three rounds.

**Opt-in contract tests**, `#[ignore]`, in `crates/launcher-core/tests/real_zebrad.rs`:

- **zebrad** (`ZEBRAD_BIN`): two isolated Regtest zebrads mine 5 and 8 blocks with `generate`
  (`invalidateblock` forces a side branch if their blocks come out the same, which is
  unconfirmed). The test pins `getblockhash` (−8, −32602), the `getblockchaininfo` pair, and
  what `getblockheader` returns for the other node's hash; then it joins them with `addnode`
  (Regtest only) and expects Forked, then a reorg 5 blocks deep. The "Diagnostic metadata:"
  block is captured as a fixture from the bundled release and from an `indexer` build, for the
  `indexer` probe.
- **Zakura** (`ZAKURAD_BIN`, Linux): a pruned Regtest node keeping 1,001 blocks mines 1,200;
  `getblockhash(1)` still answers, `getblockchaininfo` reports `pruned: true`, and
  `getchaintips` decodes.
- **The compatibility build**: whether `getchaintips` marks a rejected block `invalid`; and,
  against a Zakura parent, that a restarted sidecar reconnects.
- **zeeder** (`ZEEDER_BIN`): it starts with the managed config on loopback ports, `/health`
  answers, the metric families exist, SIGTERM exits with 0, and a taken 8233 is classed as
  `PortInUse`. Crawling needs the internet, so counts aren't checked. Fixtures of `/metrics`
  and the "crawler status" line are captured from zeeder 1.8.0, as zebrad's were.

**UI**: the panel's and the herd page's view models are pure modules tested natively, like
`health.rs`; the order of the health rules; a snapshot of `herd_report`. Every phase is
reviewed from screenshots of the mock.

**The mock** runs the real engine over a chain simulator (`launcher-herd`'s `script`
feature): a block every S with exponential jitter, scripted forks and reorgs, rules for how
each node follows, and peers in documentation ranges (`192.0.2.0/24`, `198.51.100.0/24`,
`203.0.113.0/24`, and RFC 9637's `3fff::/20` for many IPv6 groups), which the mock treats as
public.

- `?herd` keeps today's extra nodes; `?herd=laptop`, `desktop`, `server` or `mac` picks a
  herd.
- `&agree=` picks a scenario: `race`, `lag`, `stall`, `fork`, `split`, `impl` (implementations
  disagree), `implstall`, `reject`, `eclipse`, `herdstall`, `thin`, `peered`, `sleep`,
  `deepfork`, `reorg`, `nu7` (the top crosses the activation height and S_eff follows),
  `rolemismatch` or `queued` (the heavy-job gate).
- `?seeder=census`, `public` or `portclash`; `?setup&machine=laptop`, `desktop`, `server`,
  `mac`, `win`, `smalldisk` or `nobmi2`; `?fast` sets S to 2.5 s for demos.

## Phases

Each phase is one pull request, or a short series, with its tests, its mock scenarios and
screenshots, and ARCHITECTURE.md updated for what landed.

| Phase | Ships | Needs (MULTI-NODE.md phases) | Screenshots from |
| --- | --- | --- | --- |
| **H0 Independent nodes** | a per-node `network.cache_dir`, with the shared file copied once on migration; a managed per-node `network.listen_addr`; a v2 node keeping its own `network.cache_dir` (`false` or a custom path) and listen address on migration; `assign_ports` keeping 8233 and 18233 for primary listeners, in every port role, and skipping refused ports; a peer target of 12 for every zebrad archive after the primary, written when it is added; Zakura's `identity_dir` when Zakura lands | rides with phase 2, so a second zebrad never shares the first one's peer cache | the add-node dialog's port line showing 28233 for a second Mainnet node, in the mock without `?herd` (opened from Settings → Nodes, phase 2's single-node entry point; with `?herd`, Zakura · Mainnet already holds 28233 and a new Mainnet node gets 38233 before and after H0); `ports_move_out_of_each_others_way` expects P2P 28233 for a second Mainnet node |
| **H1 The herd in the mock** | first a move-only commit that moves `Level` and its `impl` from `ui/src/health.rs` to `launcher-types` (re-exported from `ui::health`), with `STALE_AFTER` and `NO_PEERS_FOR` from the same file and `SYNCED_PROGRESS` from `ui/src/format.rs`, then a commit that adds the serde derives the herd types need; `launcher-herd` (roles, spacing, the engine, diversity, alerts, plans, the wallet projection, the report); the panel, the herd page, the alert bar, the sidebar words and the setup flow, all against the mock. Users see nothing new | phase 1 (merged); can run beside phase 2 | `?herd=desktop&agree=fork`, `?herd=desktop&agree=impl`, `?setup&machine=mac` |
| **H2 Live agreement for zebrad herds** | `NodeStats.best`; the agreement actor; the `herd` and `herd_event` events; the commands `get_herd`, `herd_events`, `herd_report`, `restart_one_by_one` and `reset_peers`; Reset peers without a census (`use_census` stays false until H5); the panel and page go live; the node states in the health verdict; time rules from S_eff, `STALL_AT_TIP` included; staggered starts and Restart one by one; wake detection; the Regtest contract test; the local counts of conditions that fired | phase 2 | a release zebrad beside a development build on every OS, with `?herd=mac` |
| **H3 Pruned nodes and recommended herds (Linux)** | storage, origin and roles in the spec, with `validate_spec`; Zakura archive and pruned, with their managed keys and their peer targets (16, or 25 for a primary archive), written when each is added; the snapshot flow with its trust wording and recorded origin, and the download (below the table); the runtime role check (the monitor decodes `pruned` and `pruneheight`); `getchaintips` corroboration; the heavy-job gate and `start_now`; disk estimates per role; `MachineProfile` and the setup flow with a zakurad the user picks, with `machine_profile`, `recommend_herd` and `apply_herd_plan` | phase 3 (Zakura) | `?herd=laptop`, `?setup&machine=laptop`, `&agree=queued` |
| **H4 Peer diversity** | the flags and "may be cut off"; spreading the seeders; the 24-hour inbound record; the Reach section and its events | H2 | `&agree=eclipse`, `&agree=thin` |
| **H5 The census** | the zeeder kind in census mode; fixed port claims and the 8233 dialog; census runs and the census card, with `run_census`; peer slices for Reset peers (`use_census`) and new nodes; the upstream asks filed | phase 4 (the adapter machinery) and H4 | `?seeder=census`, `?seeder=portclash` |
| **H6 Validator, wallet and explorer hooks** | the compatibility build's Rejected; `corroborate`, `mempool_seen`, `send_check`, `wallet_view`; reorg events for wallet transactions; the explorer's default node, banner and "on k of n" | phase 5 (zcashd, Zallet) and the wallet and explorer designs' phases | `?herd=desktop&agree=reject`, the wallet mock with `&agree=split` |
| **H7 Later** | verified downloads of zakurad and zeeder; zeeder's public mode on the server preset; bootstrap from my archive; a backend alerts actor (acknowledge, snooze, the tray icon, opt-in OS notifications); an opt-in automatic restart of a stuck pruned node | phase 6 | `?seeder=public` |

H6 can run beside H3 to H5 once H2 has landed; H5 needs H4 but neither H6 nor H7.

**The snapshot download (H3).** Zeal downloads a snapshot in launcher-core over HTTPS, with a
client and TLS stack chosen against the reqwest 0.12 hold, and unpacks the `.tar.zst` itself
(zstd and tar are new dependencies). It reads the expected SHA-256 from the publisher's
manifest: Zakura's `latest.json`, or the Zcash Foundation's `snapshots.json` and the archive's
`.sha256`. It streams the archive into the node's folder, the Zebra book's way to avoid holding
both copies, hashing as it goes and deleting what it unpacked on a mismatch; staging instead
means the bill counts both copies (about 560 GB at the peak for the archive snapshot). Until
this lands, the user downloads the archive and Zeal checks and unpacks it.

## Open questions for the maintainer

1. **A new crate**, `launcher-herd` (pure, builds to wasm), shared by the backend and the
   mock. Agreed?
2. **Pruned retention**: 10,000 blocks (Zakura's floor and its snapshot's, about 3 days after
   NU7), as proposed, or about 30,000 (about 9 days)?
3. **The send check** when the wallet's node is in the minority or implementations disagree:
   a hard block, as the wallet design asks, or a typed override, as this design recommends?
4. **zeeder's binary**: a file the user provides (H5), a verified download, or bundled on
   Linux as a second pinned sidecar (about 4 MB, the same cosign check as zebrad, one more pin
   to bump before every network upgrade)? Which did "pull in" mean?
5. **zeeder's crawler port**: ship the census with the 8233 dialog and file the upstream ask
   (proposed), or wait for a configurable crawler listener before offering zeeder next to an
   archive?
6. **Public seeding at home**: offer it at all, or only on the server preset, suggesting a
   VPS?
7. **Automatic restarts**: should Zeal ever restart a stuck pruned node by itself (opt-in, at
   most once every 6 h, never during a herd stall), or always only propose it? zebrad
   resolves its DNS seeds again at every start, so a restart mostly helps; the design leaves it
   off until the alert counts say it is needed.
8. **One `indexer` archive for both the explorer and Zallet**, and should Zeal bundle that
   build (MULTI-NODE question 2)?
9. **A wallet backend from a snapshot**: allowed behind a typed confirmation, or never?
10. **macOS**: accept a zakurad the user built, as a pruned node, with a caveat?
11. **Once zebrad ships pruning**, should it replace Zakura as the default pruned kind? It
    runs everywhere, but its database moves to format 30, which matters for Zallet and for
    the Zcash Foundation's snapshots.
12. **Words**: "pruned node", or a friendlier name such as "Lookout", for the role in the UI;
    "Herd" as the page's name with "your nodes" in sentences; names for the recommended herds
    (for example "Pair" and "Trio").
13. **Spreading the seeders**: 3 of the 5 public seeders for each node after the first
    (proposed), or all five everywhere?
14. **Peer targets**: until a census measures the reachable Mainnet after zcashd's end of
    support, are 25 for the primary archive, 12 for other zebrads and 16 for Zakura nodes the
    right budget, and should the primary archive come down in a big herd?

## Sources

In this repository, on `main`: [MULTI-NODE.md](MULTI-NODE.md),
[ARCHITECTURE.md](ARCHITECTURE.md), `crates/launcher-types/src/nodes.rs` and `lib.rs`,
`crates/launcher-core/src/config.rs`, `monitor.rs` and `rpc.rs`, `ui/src/health.rs`,
`ui/src/format.rs`, `ui/src/mock/herd.rs`, `crates/launcher-core/tests/fixtures/zebrad-7.0.0-generate.toml`,
`clippy.toml`, `ui/src/components/sidebar.rs`, `ui/src/node_summary.rs`, `src-tauri/build.rs`,
`src-tauri/src/commands.rs`, `src-tauri/src/updater.rs`, `src-tauri/capabilities/main.json`,
`crates/launcher-core/tests/fixtures/zebrad-7.0.0-regtest-start-shutdown.stdout.log`.

Zebra at tag v7.0.0 (commit e8db87eb):

- `zebra-network/src/constants.rs`, `config.rs`, `config/cache_dir.rs`, `address_book.rs`,
  `meta_addr.rs`, `peer/priority.rs`, `peer_set/set.rs`, `peer_set/initialize.rs` and
  `peer_set/initialize/recent_by_ip.rs`;
- `zebra-rpc/src/methods.rs` and `methods/types/peer_info.rs`; `zebra-state/src/service.rs` and
  `constants.rs`;
- `zebra-chain/src/parameters/constants.rs` and `network_upgrade.rs`;
- `zebrad/src/commands/start.rs` and `application.rs`;
- the book: `user/snapshots.md`, `user/zallet.md`, `user/requirements.md`,
  `dev/profiling-and-benchmarking.md`;
- pruning: issue #8559, PRs #12086 to #12093 and #12132; issues #11090 and #11155;
- advisories: https://github.com/ZcashFoundation/zebra/security/advisories.

Zakura at tag v1.8.0 (https://github.com/zakura-core/zakura):
`crates/zakura-state/src/config.rs`, `crates/zakura-network/src/config.rs`, `constants.rs`,
`peer_set/set.rs` and `peer_set/initialize.rs`, `crates/zakurad/src/commands/start.rs`,
`crates/zakura-rpc/src/methods.rs`, `CHANGELOG.md` (#133, #470, #1366, #1725); snapshots at
https://zakura.com/snapshots/.

zeeder at tag v1.8.0 (https://github.com/ZcashFoundation/zeeder): `README.md`, `src/config.rs`,
`src/main.rs`, `src/seeder.rs`, `src/crawl/servability.rs`, `docs/operations.md`,
`docs/network-upgrades.md`, `docs/adr/0004` and `0007`; issues #43, #103 and #106; the v1.8.0
release assets.

Zcash and Zallet: ZIP 218, https://zips.z.cash/zip-0218; zcashd's DNS seed operator policy,
`doc/book/src/dev/dnsseed-policy.md` in zcash/zcash; Zallet's `zallet-core/src/config.rs`
(https://github.com/zcash/zallet); the zcashd compatibility build's `Cargo.toml` at tag
`zebra-compat-v1.3.0` (https://github.com/ZcashFoundation/zcashd).

Snapshots: https://snapshots.zfnd.org/.

Papers and precedents:

- Heilman, Kendler, Zohar and Goldberg, "Eclipse Attacks on Bitcoin's Peer-to-Peer Network",
  USENIX Security 2015.
- Gervais, Ritzdorf, Karame and Čapkun, "Tampering with the Delivery of Blocks and
  Transactions in Bitcoin", CCS 2015.
- Apostolaki, Zohar and Vanbever, "Hijacking Bitcoin: Routing Attacks on Cryptocurrencies",
  IEEE S&P 2017.
- Tran, Choi, Moon, Vu and Kang, "A Stealthier Partitioning Attack against Bitcoin
  Peer-to-Peer Network" (Erebus), IEEE S&P 2020.
- Biryukov and Tikhomirov, "Deanonymization and Linkability of Cryptocurrency Transactions
  Based on Network Analysis", EuroS&P 2019.
- BIP 50, the March 2013 chain fork post-mortem.
