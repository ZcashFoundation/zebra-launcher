# The wallet: Zallet on your own nodes

## Status

> **Deferred, 2026-11-03: an experimental feature for later.** The explorer, the herd,
> resource monitoring and the light client server come first; no wallet code is planned until
> they ship, and the wallet then starts as an experimental, Testnet-only feature. This is a
> design for review before any code. The maintainer chose to "run zallet and add a UI with
> leptos". It builds on [MULTI-NODE.md](MULTI-NODE.md) (the node
> registry, kinds, dependencies, ports and phases) and [HERD.md](HERD.md) (roles, agreement
> across nodes, the seeder). [ARCHITECTURE.md](ARCHITECTURE.md) stays the description of what is
> built and is updated phase by phase. Zallet's behaviour here was read from its source, not
> observed running; facts marked "unconfirmed" are checked before the phase that relies on them.

Paths starting `Z/` are in zcash/zallet at tag v0.1.0-beta.4, paths starting `Zb/` in
ZcashFoundation/zebra at tag v7.0.0, other paths in this repository ([Sources](#sources)).

The maintainer described the product as a private node wallet with a zeal of nodes behind it,
plus an optional DNS seeder (zeeder). The wallet is the centre: it reads an archive zebrad of the
user's own herd, whose view of the chain the other nodes check. Zeal runs Zallet
([zcash/zallet](https://github.com/zcash/zallet), the Z3 wallet, 0.1.0-beta.4 of 2026-11-02) as a
supervised node of kind `zallet` that depends on one zebrad node, and adds wallet pages to its
Leptos frontend. The webview calls Tauri commands only; launcher-core talks to Zallet's JSON-RPC
on loopback and holds every rule.

It starts small and safe: a Testnet wallet that can't spend comes first, custody, review and
hardening rules set the bar for Mainnet, and the decisions below say what was chosen and why.

### Decisions in brief

| Question | Decision | Rather than |
| --- | --- | --- |
| The engine | Zallet 0.1.0-beta.4, run as `zallet-zebra` by absolute path, a kind that depends on one zebrad node | linking wallet crates into Zeal |
| The node it reads | an archive zebrad on this computer, same network, format v29, synced from the first block; Testnet's shielded-only phases may use the bundled release zebrad, while transparent funds and Mainnet need an `indexer` build that Zeal builds and attests (proposed, [question 3](#open-questions-for-the-maintainer)) | the `indexer` build from the first phase |
| The recovery phrase | Zallet generates it; Zeal decrypts `export-mnemonic`'s output and shows the words once; the user answers Zallet's own `confirm-backup` quiz; `require_backup` stays on | Zeal generating and importing the seed; no backup check |
| Secrets for setup commands | pipes while there is no passphrase; from W4 one PTY runner (only the controlling terminal) with `PINENTRY_PROGRAM` pointing nowhere | a pinentry helper, which can't serve `import-mnemonic` |
| Custody | no passphrase on Testnet only (W1 to W3, recreated later); a passphrase required on Mainnet; nothing stays unlocked: each keyed operation unlocks, makes one call, locks and checks | a "stay unlocked" session; the passphrase in the Linux keyring |
| Where the passphrase is typed | a native GTK prompt showing Rust's summary of what it authorizes (spiked in W0); at least a `tauri-plugin-dialog` confirmation drawn from Rust's review, with the webview field's risk named | a webview field alone |
| Sending | the PCZT methods, so the reviewed transaction is the one signed, broadcast by Zeal through the wallet's zebrad; never `z_sendfromaccount`; `z_sendmany` only for TEX on Testnet, labelled; Mainnet v1 refuses TEX | one-shot sends without a review |
| Broadcast | once, through the wallet's node, and again from Zeal's outbox through that node; through another node only when the user asks, with a privacy note (HERD.md) | rebroadcasting through every herd node |
| The herd and sending | refused while the wallet's node is on a minority or contested branch; an explicit confirmation on the majority side of a split; an acknowledgement on every send while it is the only witness | HERD.md's typed override in the send flow ([question 4](#open-questions-for-the-maintainer)) |
| The backing zebrad exits unexpectedly | Zeal stops Zallet and blocks sending; Zallet starts again only after zebrad passes the dependency gate, listener ownership included | leaving Zallet redialling whatever holds those ports |
| Restore | on Testnet in W4; a Testnet restore drill gates Mainnet | restore after the Mainnet beta |
| The Zallet binary | the user's file, checked against a pinned SHA-256, copied into Zeal's data, hashed at every start; Mainnet takes pinned hashes only | bundling it now; trusting `PATH` |
| Wallet data in the webview | command replies and per-call `tauri::ipc::Channel`s, never `app.emit` | a broadcast snapshot event |
| Thresholds | times in S_eff, HERD.md's effective spacing (the measured mean block gap, held between S and 3 S; S, the target spacing, is 25 s from NU7); fee bounds from a table pinned per Zallet version | numbers tuned for 75 s blocks; a fixed fee floor |
| zeeder | HERD.md's optional seeder kind; never a voter, never in the wallet's trust path | a wallet-specific seeder design |

## Goals and non-goals

### Goals

- Keys, history and the chain view stay on the user's computer; no third-party server by
  default (no price feed, no outside explorer, no telemetry).
- The herd checks the wallet's node: "confirmed on 3 of 3 nodes", warnings, and no sending
  while the user's nodes disagree about the wallet's chain.
- What the user reviews is what is signed and broadcast, exact to the zatoshi.
- Setup, backup and restore a non-expert can finish, with Zallet's own backup check kept.
- Safe phases: each gives a Testnet user something to run, nothing that moves money exists
  before the phase that needs it, and Mainnet follows a Testnet beta and an external review.
- The project's rules: no mutexes, a thin Tauri layer, the cargo-deny allow-list, every view in
  the mock.

### Non-goals, for now

- A wallet on macOS or Windows: Zallet has no chain backend there.
- A wallet on another computer, or a remote Zallet: its RPC stays on loopback.
- Light-wallet modes; Zakura, pruned or remote backends, which Zallet can't read.
- Key and viewing-key imports, zcashd wallet migration, multisig. Viewing-key export, TEX on
  Mainnet and hardware identities come later.
- Fiat values, links to outside explorers, notification services.
- Recovering a lost passphrase or phrase; holding keys for the user; changing Zallet itself.

## How Zeal runs Zallet

### What Zallet needs from zebrad

Zallet 0.1.0-beta.4 has one chain backend, `zebra`, which reads a zebrad on the same computer
three ways (Zebra book `user/zallet.md`; `Z/backends/zebra`):

| Channel | For | So |
| --- | --- | --- |
| zebrad's RocksDB, opened read-only as a secondary (`zebra_state_path`) | blocks, transactions, the Sapling, Orchard and Ironwood trees, UTXOs, the spending-transaction index | same computer, same format: beta.4 reads only v29 (zebrad 7.0.0 and later) and never writes it. Each start leaves a folder in `$TMPDIR` (`Zb/zebra-state/src/service/finalized_state/disk_db.rs:1050-1066`) |
| the indexer gRPC (`rpc.indexer_listen_addr`; plain h2c, no authentication) | following the tip | loopback only; unreachable at start means exit 1 (`Zb/zebra-rpc/src/sync.rs:239`) |
| JSON-RPC with zebrad's cookie | `getblockchaininfo` at start, `getrawmempool` every second, `getrawtransaction`, `sendrawtransaction` | the cookie is read again on every request (`Z/backends/zebra/src/chain/rpc.rs:36-90`) |

So the wallet's node is an archive zebrad (7.0.0 can't prune) on this computer and network.
Zakura (format v30), pruned nodes and remote nodes can check it but not back it. Zallet calls
itself synced within `sync.lock_threshold` (100) blocks of that one zebrad's tip and never asks
whether zebrad is at the network's tip or on the right chain
(`Z/zallet-core/src/components/sync/status.rs`); the herd answers that.

### Obtaining the binary

Zallet publishes Linux x86-64 and aarch64 builds only: tarballs, `.deb` packages on apt.z.cash
and Docker images, with GPG signatures, SBOMs and SLSA provenance. A tarball holds `zallet`, a
launcher that finds `zallet-<backend>` beside itself or on `PATH` and `exec`s it
(`Z/zallet/src/main.rs`), and `zallet-zebra`, the wallet; both static-pie musl. Zeal runs
**`zallet-zebra` by absolute path**: one binary to pin, no `PATH`, and the supervised pid is the
wallet's.

- **Pins** in `crates/launcher-core/src/wallet/pins.rs`, bumped like `ZEBRAD_COMMIT`: the
  version and per architecture the SHA-256 of the tarball and of `zallet-zebra`. Known: amd64
  tarball `1a8f8b20358ea3aed644a5b6581f1a346016a73f85e9bbf1bbde9c164d94f509` (its GPG signature
  checked), amd64 `.deb` `b32a4d2f4d7699077f68e0091108ec9daea38df03ef7b45be2f96615384c444b`; the
  rest are computed at the first bump. There is no `SHA256SUMS`; hashes sit in each asset's
  `.provenance.json` and `.sbom.spdx`.
- **At bump time** the maintainer runs `gpg --verify` with the ZODL key from apt.z.cash (primary
  `0338 34DD 49DE CF9D BB99 34BC 6C93 CA8E 58E2 6AB1`; Zallet's Debian page still names the old
  ECC key, which `Z/book/src/slsa/slsa.md` says no longer signs) and `gh attestation verify`
  with `--signer-workflow
  github.com/zcash/zallet/.github/workflows/binaries-and-deb-release.yml@refs/heads/release/v0.1.0-beta.4`,
  since beta.4 was re-released from a branch (not run yet). Zeal does no OpenPGP at runtime.
- **When a wallet is added**, the user picks the tarball, a `zallet-zebra` file or the `.deb`'s
  `/usr/bin/zallet-zebra`. Zeal first refuses a file or folder others can write, copies the file
  into a private staging folder under `<app data>/programs/zallet/`, hashes the copy and compares
  it with the pins. Mainnet stops there unless it matches; Testnet asks for the typed "unverified
  build" confirmation first. Only then does Zeal run `--version` (expected `zallet 0.1.0-beta.4`,
  unconfirmed) on the copy, which becomes `<app data>/programs/zallet/<version>/zallet-zebra`
  (0700, 0500). The folder name comes from the pin's version, or for an unverified Testnet build
  from the copy's SHA-256, never from the program's output. Zeal runs that copy and hashes it
  before every start; a change fails the start as `BinaryChanged`.
- **Later**, a verified download the user starts, in src-tauri (launcher-core's reqwest has no
  TLS by design). Bundling is [question 2](#open-questions-for-the-maintainer).

### Files of a wallet node

Under `<app data>/nodes/<id>/` (`zallet-testnet`, say), created 0700 before any Zallet command:

| Path | Holds | Mode |
| --- | --- | --- |
| `data/` | Zallet's datadir: `zallet.toml`, `wallet.db` with its `-wal` and `-shm`, `.cookie` (new at each start, gone after a clean stop), `.lock` | 0700 |
| `keys/encryption-identity.txt` | the age identity, outside `data/` so a copy of the datadir never carries it (an absolute `keystore.encryption_identity`; a relative one is joined to the datadir, `Z/zallet-core/src/commands.rs:105-110`) | 0700, 0600 |
| `tmp/` | `TMPDIR`, emptied before each start | 0700 |
| `wallet.json` | setup progress; the binary's version and hash; the identity's kind and public key; each seed's fingerprint and backup state; each account's UUID, name, seed, ZIP 32 index and birthday, which no RPC returns | 0600 |
| `outbox/<txid>.json` | a transaction extracted but not mined or expired (W2) | 0600 |
| `contacts.json` | the address book (W2) | 0600 |
| `logs/zallet.log` | stderr, redacted on ingest, rotated | 0600 |

None of Zeal's files holds a secret. Folders on filesystems without POSIX permissions (vfat,
exfat, ntfs, fuse) are refused.

### `zallet.toml`

Every table rejects unknown keys and some are required even when empty
(`Z/zallet-core/src/config.rs`; `Z/backends/zebra/tests/cmd/example_config.out/zallet.toml`), so
Zeal renders the whole file before the first setup command (so the identity lands in `keys/`)
and before every start, and never merges edits. Configure shows it read-only.

```toml
# Managed by Zeal. Rewritten before every start; edits are lost.
backend = "zebra"

[builder]
trusted_confirmations = 3
untrusted_confirmations = 10          # Mainnet: open question 7

[builder.limits]

[consensus]
network = "test"                      # "main" on Mainnet; fixed for the wallet's life

[database]

[external]

[features]
as_of_version = "0.1.0-beta.4"

[features.deprecated]

[features.experimental]

[indexer]
validator_address = "127.0.0.1:18232"                  # the zebrad node's rpc.listen_addr
validator_cookie_path = "<zebrad node>/rpc/.cookie"

[indexer.read_state_service]
grpc_address = "127.0.0.1:18230"                       # its rpc.indexer_listen_addr
zebra_state_path = "<zebrad node's state.cache_dir>"   # absolute; holds state/v29/<network>

[keystore]
encryption_identity = "<node>/keys/encryption-identity.txt"

[note_management]

[rpc]
bind = ["127.0.0.1:28232"]            # exactly one loopback address, from port assignment
timeout = 120                         # seconds
```

- `rpc.bind` holds exactly one address: Zallet asserts it
  (`Z/zallet-core/src/components/json_rpc/server.rs:45-46`), and an empty list starts no server.
- `rpc.timeout` covers Zallet's whole HTTP service; its 30 s default is less than a first
  `pczt_prove` can take (`Z/.../json_rpc/methods/pczt_prove.rs:7-14`).
- Defaults kept: `keystore.require_backup` (true on Mainnet and Testnet) and
  `sync.lock_threshold` (100), so balances stay readable a few blocks behind; Zeal's send gate
  requires the wallet's tip to equal its node's.
- Never written: `rpc.allow_insecure_remote_bind`, `[[rpc.auth]]`, a validator user or password,
  `indexer.db_path`, `external.notify`.
- `features.as_of_version` isn't compared in beta.4 but follows each Zallet bump.
- A change to the zebrad node's ports or folders rewrites the file and restarts Zallet. A golden
  test pins the render; a test that parses Zallet's `example-config` output fails CI when a bump
  adds or drops a table.

### The child's environment

- `env_clear()`, then only `PATH=/usr/bin:/bin`, `HOME=<node>`, `TMPDIR=<node>/tmp`,
  `LC_ALL=C.UTF-8`, `RUST_LOG=info`, `NO_COLOR=1`, `PINENTRY_PROGRAM=/nonexistent/zeal-no-pinentry`.
  `RUST_LOG` is never inherited: at `trace` jsonrpsee-server 0.24.11 logs whole requests, the
  cookie's `Authorization` header included (`server/src/server.rs:1038`). The missing pinentry
  makes age fall back to the terminal instead of opening a desktop dialog (age 0.12.1
  `age/src/cli_common.rs:99-150`; pinentry 0.8.0 `src/lib.rs:93-108`). Zallet colours its output
  regardless (inferred from `Z/zallet-core/src/components/tracing.rs`), so Zeal strips SGR codes.
  No secret goes in argv or the environment. ARCHITECTURE.md's "never `env_clear`" protects
  zebrad on Windows; it records Zallet, Linux-only, as the exception.
- On the existing Spawner thread, so `PR_SET_PDEATHSIG(SIGTERM)` holds; `RLIMIT_CORE` 1;
  `RLIMIT_NOFILE` at least 1,024 (the secondary needs 512).
- `start` gets its own process group (`process_group(0)`, like zebrad) and a null stdin. Setup
  commands get a new session, `setsid()` in `pre_exec`, instead: `std` applies the process group
  before `pre_exec`, and `setsid` fails with `EPERM` in a group leader. With no controlling
  terminal, a command can't stop on `SIGTTIN` when Zeal runs from a shell, and an unexpected
  prompt fails at once.

### Setup, in order

Setup commands take the datadir lock that `start` holds, so they run with the wallet stopped;
the `SetupActor` holds every permit of the wallet node's data gate meanwhile. This is a
data-change permit, as `AppState::data_change_permit` takes them today: it needs the node stopped
or failed, and no start can begin while it is held. The actor holds it from step 1 through step
6, or through a restore's `import-mnemonic`, and drops it before the start in step 7. A start
permit would not do: it is one of 64 shared permits and keeps out data changes only
(`src-tauri/src/app_state.rs:26-27`, `99-121`). `generate-encryption-identity` and `backup` take
no lock. zebrad is needed from step 7. B is `<copy of zallet-zebra> -d <node>/data`.

| # | Command | Input | Zeal keeps | Done when |
| --- | --- | --- | --- | --- |
| 1 | `B generate-encryption-identity` (W4: `-p --passphrase-file -`) | W4: the passphrase and `\n` on stdin | `Public key: age1…` from stderr | exit 0; on a rerun, exit 1 with `An encryption identity already exists at`, which counts as done since the identity is never overwritten |
| 2 | `B init-wallet-encryption` | W4: the passphrase on the PTY | — | exit 0, or `Keystore age recipients already initialized` |
| 3 | `B generate-mnemonic` | — | `^Seed fingerprint: (zip32seedfp1[02-9ac-hj-np-z]+)$` from stdout; the seed is Unconfirmed | exit 0 |
| 4 | `B export-mnemonic --seedfp FP --armor` | W4: the passphrase on the PTY | the armored age message, in a `Zeroizing` buffer, decrypted in process with the identity (W4: and the passphrase) into 24 words checked against the BIP 39 English list, held for the reveal only | 24 valid words, else the step fails closed |
| 5 | the reveal ([Onboarding](#onboarding)) | — | — | the user has written the words; Zeal zeroizes them and the page drops them |
| 6 | `B confirm-backup --seedfp FP` | W4: the passphrase on the PTY; then three `Enter word #N: ` prompts (no newline), each answered with the user's word and `\n` on stdin (`Z/zallet-core/src/commands/confirm_backup.rs:76-95`) | — | `Backup confirmed.` and exit 0; a wrong word exits 1 and a new run asks new positions |
| 7 | [Start](#start); wait for `getwalletstatus.wallet_tip` | — | — | account calls answer −28 "Wallet sync required" until then |
| 8 | `getwalletstatus`, then `z_getnewaccount ["Main"]` (W4: in KeyGuard), then `z_listaccounts` | — | the UUID, the ZIP 32 index, and as birthday the lower of `wallet_tip` and `node_tip` from the read **before** the call, Zallet's own rule (`get_new_account.rs:57-70`), so the record is never later than the real one | −18 means the backup isn't confirmed: back to step 6 |
| 9 | `z_getaddressforaccount [uuid, ["orchard"]]` | — | — | the first address and the recovery sheet |

**Restore** (W4): steps 1 and 2, then `B import-mnemonic` on the PTY (rpassword reads `/dev/tty`,
`Z/zallet-core/src/commands/import_mnemonic.rs:26`); Zeal writes the words once echo is off. The
seed is stored Confirmed, so there is no quiz. Zeal checks the printed fingerprint against its
own ZIP 32 computation, starts Zallet and runs `z_recoveraccounts` in KeyGuard for each chosen
account index with the birthday. Whether that catches up on the zebra backend is unconfirmed:
zcash/zallet#588 (open) reports a recovered account that never did, in a Regtest test adding a
second account with a birthday near the tip that needs the Ironwood tree there. The Testnet
restore drill decides ([Testing](#testing)).

- **The setup runner isn't the supervisor.** Output goes to `Zeroizing` buffers capped at 1 MiB,
  never to the log ring, the log file or diagnostics; only the exit status and allow-listed
  fields (the fingerprint, the public key) survive, and failures log a fixed message. Each step
  has 120 s, then `SIGTERM` and `SIGKILL` to its session; a quiz ends after 5 minutes idle.
- **Version-gated parsers.** Fixtures from the real beta.4 pin every pattern for that version
  only; on any mismatch the step stops and shows Zallet's own commands to run by hand.
- **Resumable.** `wallet.json` records each step, and each can run again. A rerun of step 1
  prints no `Public key:` (from W4 it still reads the passphrase from stdin before it refuses), so
  Zeal takes the key from the file's `# public key:` comment. In W4 that comment is inside the
  passphrase-encrypted file, which Zeal decrypts with the passphrase it holds; this also shows that
  the passphrase is the one protecting the file. At launch, a seed Zallet generated whose backup
  is unconfirmed takes the user back to step 4. Zeal never generates a second seed by itself or
  leaves −18 unexplained.
- **The PTY runner** (W4, Linux): `openpty`, then `setsid()` and `TIOCSCTTY` in `pre_exec`; stdin,
  stdout and stderr stay pipes, so the armored export keeps its bytes (a PTY would turn `\n` into
  `\r\n`). Age's `read_secret` and rpassword prompt on the PTY. Zeal writes a secret only once
  `tcgetattr` on the master shows echo off (unconfirmed on Linux; a W0 spike) and discards the
  transcript.
- **Pauses.** Showing the phrase again (steps 4 and 5) or confirming a backup later (step 6)
  stops Zallet, runs the command and starts it: "Zeal pauses your wallet for a few seconds".
  Refused while a send is between `pczt_sign` (from W4, the unlock before it) and its outbox
  entry.

### Start

1. **The dependency gate**, or `WaitingFor { node, why }` instead of a crash loop, since Zallet
   fails fast with exit 1 on each (`Z/zallet-core/src/commands/start.rs:131-215`): the zebrad
   node is Running in its current run on the wallet's network; its `getblockchaininfo` answers
   with its current cookie and the right `chain`; its RPC and indexer ports accept connections
   from listeners owned by its pid ([requirement 6](#requirements-in-priority-order));
   `<state.cache_dir>/state/v29/<net>/version` exists with a major in Zeal's compatibility table
   (Zallet 0.1.0-beta.4: zebrad 7.0.0 or later, v29) and a variant that suits the phase
   (`+indexer` from W3 for transparent features, and always on Mainnet); Zallet's port is free
   and the binary's hash is the recorded one.
2. Empty `tmp/`, delete a leftover `.cookie`, rewrite `zallet.toml`, spawn `B start`.
3. **Running** once `Opened RPC endpoint at 127.0.0.1:P` is logged, `data/.cookie` exists and the
   listener on P is the child's. Only the backend reads the cookie, again after each start.
4. `getwalletstatus` every 5 s (2 s while catching up) and at each `New chain tip:` line. Sending
   waits for `Proving caches warmed for consensus branch …`.

### Supervision

Exit classes, from Zallet's messages (`Z/zallet-core/i18n/en-US/zallet_core.ftl`) and exit codes
read from the source; fixtures from a real run pin them before W1.

| Exit | Class | Then |
| --- | --- | --- |
| 0 after Zeal's `SIGTERM` | `Clean` | Stopped |
| 0 unasked, after "The chain has reached height H, where the backing full node's consensus rules diverge…" | `UpgradeRequired` | "This Zallet can't follow the chain past block H"; never restarted (`Z/zallet-core/src/components/sync.rs:835-846`) |
| 1 with `no zebra-state v` or `follows consensus rules that this zallet build cannot` | `UpgradeRequired` | needs the Zallet that matches the node |
| 1 with `Cannot obtain a lock on data directory` | `DbLocked` | another Zallet uses the folder |
| 1 with `Failed to parse configuration file at`, `requires an [indexer.read_state_service]` or `The config file selects the` | `ConfigError` | a Zeal bug, since Zeal owns the file |
| 1 with `Encryption identity file could not be located at` or `The wallet has not been set up to store key material securely` | `SetupRequired` | Resume setup |
| 1 with `Refusing to open the JSON-RPC endpoint on non-loopback address`, or a bind error | `PortInUse` | Failed |
| Zeal's own checks fail | `ListenerMismatch`, `BinaryChanged` | Failed before anything is sent |
| any other 1, 101 (a panic), a signal | `Crash` | backoff; restarted only once the gate passes again |

`An ongoing Zallet task exited; cancelling the remaining tasks` precedes a fatal exit; its detail
is kept.

- **A planned** stop, restart, config apply or update of the zebrad node waits while a send is
  between `pczt_sign` (from W4, the unlock before it) and its outbox entry (from W3 also while
  `z_listoperationids` lists an executing operation), stops Zallet, then zebrad; zebrad starts
  first, Zallet after the gate. The node's page and stop dialog say "Wallet · Testnet reads this
  node".
- **An unplanned** zebrad exit stops Zallet at once; balances turn stale and sending is blocked.
  Zallet starts again when zebrad passes the gate, listener ownership included. Left running, it
  would redial the unauthenticated gRPC port every 5 s and send zebrad's cookie to whatever
  answered on the RPC port (`Zb/zebra-rpc/src/sync.rs:31-70,302`), so a local user who took a
  port in the gap could feed it invented blocks. Nothing then relies on the RocksDB secondary
  following a restarted primary (unconfirmed).
- **A stale view**: if Zallet's `node_tip` stays put for 12 S_eff while the node's tip moves,
  Zeal restarts Zallet.
- **Watched**: `failed to subscribe to non-finalized state changes`, `non-finalized state change
  stream ended unexpectedly`, `error fetching mempool`; `Chain reorg detected, rewinding to H`
  (Recovering; sending pauses); `wallet_tip` against `node_tip`; memory, CPU, and the sizes of
  `data/` and `tmp/`.

### Stop

A stop waits while a send is between `pczt_sign` (from W4, the unlock before it) and its outbox
entry, and warns before losing an operation id (W3). Then `SIGTERM`, 30 s (Tokio gives tasks
20 s, `Z/zallet-core/src/application.rs:50`), and `SIGKILL` to the group; SQLite survives a kill.
The `stop` RPC works only on Regtest.

The exception to that wait is a failed KeyGuard lock check: Zallet stops at once, even between
signing and the outbox entry. Nothing is stored before `pczt_extract`, so the signed PCZT is
dropped and the review is proposed again.

### Upgrades

**Zallet**, never automatic: the backup is confirmed, a fresh encrypted snapshot exists, nothing
is in flight, the outbox is empty, the wallet is locked; Zeal shows the release notes (before 1.0
"all releases should be considered breaking changes", and an upgrade "may require recreating the
wallet"), stops Zallet, checks the new binary's pin, swaps it, rewrites `as_of_version` and
starts. `wallet.db` migrates forward only and records every version that opened it, so Zeal
never starts an older Zallet.

**The zebrad node**: stop Zallet; update zebrad in its variant; let it finish moving its
database; check that the matching Zallet is in place; start Zallet. A zebrad that changes the
database major (v29 to v30) deletes v29 after migrating (`state.delete_old_database`, default
true), and beta.4 can't read v30. Nothing is lost, since `wallet.db` doesn't depend on zebrad's
database, but the wallet waits in `UpgradeRequired` ("Needs Zallet X for Zebra's new database")
until the matching Zallet is installed. Zeal releases move both pins together and carry the
compatibility table. zebrad 7.0.0 halts on Mainnet 56 days after its estimated release height
3,526,000, counted at the target spacing of each height
(`Zb/zebrad/src/components/sync/end_of_support.rs`): around mid to late December 2026
(approximate); Testnet has no such halt. The pair's upgrade is rehearsed on Testnet first.

### The zebrad a wallet depends on

| Requirement | Why |
| --- | --- |
| zebrad 7.0.0 or later, format v29, on this computer and the wallet's network | Zallet opens its database files and reads only v29 (`Z/backends/zebra/CHANGELOG.md`) |
| an archive synced from the first block | zebrad 7.0.0 can't prune. A snapshot moves the trust below its height to whoever cut it (HERD.md), and Zallet reads transactions from that database, so a tampered snapshot could show payments every block-hash comparison confirms: never on Mainnet; on Testnet with a note |
| a managed loopback `rpc.indexer_listen_addr` | the gRPC has no authentication |
| for transparent funds, and always on Mainnet, the `indexer` build | the spend index, below |

**What `indexer` does**, read at v7.0.0. The gRPC isn't behind the feature: zebrad serves it
whenever `rpc.indexer_listen_addr` is set (`Zb/zebrad/src/commands/start.rs:557-573`; the doc
comment in `zebra-rpc/src/config/rpc.rs` saying otherwise is stale). The feature
(`Zb/zebrad/Cargo.toml:74`) writes the spending-transaction index as blocks are committed
(`Zb/zebra-state/src/service/finalized_state/zebra_db/transparent.rs`, `shielded.rs`). Zallet reads
it to find the transaction that spent one of the wallet's transparent outputs, once the UTXO set
says it is spent (`Z/backends/zebra/src/chain/reader.rs:388`, `chain.rs:589-603`). Without it,
Zallet syncs but such an output stays `SpentSpenderUnknown`, queued again forever and logged at
debug level only (zebra#11155). Shielded scanning doesn't use the index. So W1 and W2, Testnet and
shielded only, run on the bundled release zebrad plus the indexer port, with no transparent
address and no transparent source; a real-binary test must receive and spend shielded funds on a
release build first, or W1 waits for an `indexer` zebrad (W3's `zeal-zebrad-indexer`, or a user
build whose version shows `+indexer`), and that part of W3 moves ahead of W1. Zallet and Zebra
both call the `indexer` build required (`Z/backends/zebra/src/main.rs:5-6`,
`Z/backends/zebra/CHANGELOG.md:39-40`, Zebra book `user/zallet.md`), so this runs outside
upstream support, on the behaviour zebra#11155 reports as a bug. That issue asks for a read-only
open of a database without `+indexer` to fail. Zallet builds `zebra-state` with `indexer`, so if
the fix lands, a Zallet built on it stops at startup on a release build. The compatibility table
records, per Zallet version, whether a release build works, and a bump that drops it moves the
wallet's node to the `indexer` build first. A bug seen on a release build is reproduced on an
`indexer` build before it is reported upstream. From W3, and always on Mainnet, the node runs
`indexer`.

**Variants.** The node's spec records `Release` or `Indexer` (Linux). "Use this node for a
wallet" switches Release to Indexer one way: one restart, then the index fills. Indexer to Release
is refused while a wallet reads the node, because a release build deletes the index at its next
start (`started removing indexes for spending tx ids`,
`Zb/zebra-state/src/service/finalized_state/disk_format/upgrade.rs:486-505`). The user's archive
is converted rather than a second 300 GB one added; it can serve the explorer too. Other nodes
keep Zebra's tarball.

**Telling the builds apart.** Every zebrad 7.0.0 accepts the key and opens the port, so neither
proves anything. The build metadata does: at every start zebrad logs a `Diagnostic metadata:`
block whose `features:` line lists `indexer` and whose `running state version:` reads
`29.0.0+indexer` only in that build (`Zb/zebrad/src/application.rs`; the format is in
`crates/launcher-core/tests/fixtures/zebrad-7.0.0-regtest-start-shutdown.stdout.log`). HERD.md's
`inspect_binary` probe reads it once on Regtest, and the zebrad detector at every start. As for
the index: a database an `indexer` build creates gets `29.0.0+indexer` in
`<state.cache_dir>/state/v29/<net>/version` at once (`marked database format as newly created`,
`upgrade.rs:690-715`) and is indexed as blocks arrive; on an existing database the build writes
`+indexer` **before** filling the index, then logs `started checking/adding indexes for spending
tx ids` and `finished …` (`upgrade.rs:460-484`), at every start.

| Spend index | When | The wallet |
| --- | --- | --- |
| `Missing` | the version lacks `+indexer` | shielded only; transparent sources blocked, transparent balances "incomplete" |
| `Building` | this run logged `started checking/adding…` but not `finished…` | transparent sources blocked: "Zebra is checking its wallet index" |
| `Ready` | this run created the database, or logged `finished…` | everything |

That a new database needs no filling is read from the source; the real-zebrad test pins it. The
check at each start costs about 10 minutes of CPU on Mainnet (zebra#11090, on 6.2.2), so the
node's restart takes one of HERD.md's heavy-job permits. Size and fill time are unmeasured.

**Where the build comes from.** Zebra ships no `indexer` build, and Linux zebrad is Zebra's own
signed tarball today (`docs/SIDECARS.md`). Proposed: a Linux sidecar `zeal-zebrad-indexer`, built
in Zeal's CI from `ZEBRAD_COMMIT` with `--features "default-release-binaries indexer"`
(`scripts/build-zebrad.sh:250` passes the first only) on Ubuntu 22.04 for the glibc 2.35 floor,
smoke-tested and attested like the macOS and Windows builds; about 50 MB more in the Linux bundle.
SECURITY.md then says the wallet's chain view comes from a program Zeal's CI built. Meanwhile a
zebrad the user provides is accepted once its version shows `+indexer`
([question 3](#open-questions-for-the-maintainer)).

**Managed keys** added to that node's `zebrad.toml`:

| Key | Value |
| --- | --- |
| `rpc.indexer_listen_addr` | `127.0.0.1:<port>`, a new `PortRole::Indexer`: 8230 on Mainnet (the Zebra book's), 18230 on Testnet (no upstream convention) |
| `health.listen_addr` | loopback, with `enforce_on_test_networks = true` on Testnet; `/ready` (2 blocks, 300 s) feeds the send gate |
| `metrics.endpoint_addr` | removed while a wallet reads the node: zebrad counts every RPC call by method on that unauthenticated page (`rpc_requests_total{method="sendrawtransaction"}`, `rpc_errors_total`; `Zb/zebra-rpc/src/server/rpc_metrics.rs:89,102-108`), so any local user could see when the wallet broadcast and whether the node refused it; that node's dashboard loses its metrics tiles |
| `rpc.lightwalletd_listen_addr` | refused on this node: no further unauthenticated endpoint |
| `rpc.cookie_dir`, `rpc.listen_addr`, `state.cache_dir`, `network.network` | already managed; Zallet's `validator_cookie_path`, `validator_address`, `zebra_state_path` and network follow them |

`config.rs` already refuses non-loopback values here (`LOOPBACK_KEYS`, line 292). Opening the port
restarts the node once, which the add-wallet review says. Removing the node, changing its network
or folder, or switching it from Indexer to Release is refused while a wallet reads it; Release to
Indexer stops Zallet, restarts the node and starts Zallet once the gate passes.

## Platforms

| | Linux x86-64, aarch64 | macOS 12 and later | Windows 10, 11 |
| --- | --- | --- | --- |
| The wallet | runs | no | no |
| Why | Zallet's Tier 1, static-pie musl (StageX on amd64, Nix on arm64), so Zeal's glibc floor doesn't apply | Tier 2: the libraries build and test, but "No chain backend currently runs on macOS" since beta.4 removed Zaino | Tier 3: no backend, no builds, Windows hardening behind Unix |

- Linux-only is Zallet's policy and CI scope (`Z/book/src/guide/platform-support.md`; CI builds
  the zebra backend only on Linux), not a code gate. Zeal's wallet code builds everywhere and
  `availability()` decides; the PTY, `/proc` and GTK parts are `cfg(target_os = "linux")`.
- Under X11 any client reads the screen and keys, so the phrase screen and the prompt say so;
  tao 0.37.2, the version Zeal locks, can't keep a window out of screen captures on Linux
  (`src/window.rs:1212-1217`). WSL2 should behave like Linux (unconfirmed).
- Resources, none measured: binaries about 105 MB; `wallet.db` up to hundreds of MB; recovery
  peaks near `sync.recover_batch_size` × 2 MiB (2 GiB by default); HERD.md's 400 GB Mainnet
  archive (30 GB on Testnet) plus the spend index. The add-wallet review sums them.

**On macOS and Windows** the Zallet card in the add-node dialog stays visible and can't be picked:
"Zallet 0.1.0-beta.4 runs on Linux only: its chain backend reads a Zebra node's database on the
same computer, and Zallet publishes no macOS or Windows builds." First run makes no wallet offer,
the sidebar has no wallet, and Settings → Nodes says the same; the herd and the explorer work as
on Linux. A Mac can't use a wallet on a Linux computer, since Zallet's RPC stays on loopback. A
remote backend is only "proposed separately" upstream (zcash/zallet#899). The mock shows every
wallet screen on every OS.

## Security model

### Threat model

Assets: **spend authority** (the seed, the age identity and its passphrase, keys in Zallet's
memory); **privacy** (`wallet.db` holds history, addresses and viewing keys in plaintext by
design, `Z/book/src/security/threat-model.md`; txids, amounts, the link between this computer's
address and a transaction); **integrity** of what the user sees and signs; **recoverability**;
**live credentials** (Zallet's cookie opens the whole wallet RPC; zebrad's); **code** (Zeal,
`zallet-zebra`, the `indexer` zebrad).

| Adversary | Can | Zallet already | Zeal adds |
| --- | --- | --- | --- |
| P2P peers, an eclipse | show a minority chain, hold back blocks or transactions | zebrad validates | the herd gate; "confirmed" needs other nodes |
| Web pages in the user's browser | reach loopback ports (Electrum's CVE-2018-1000022) | HTTP Basic auth | on the wallet's node only the indexer gRPC (plain HTTP/2, which a browser page can't speak) and health (readiness only) are unauthenticated, and there is no metrics page |
| Other local users | connect to loopback ports, take a port before Zallet or zebrad binds it, read world-readable files and argv | 0600 and 0700 on its files | listener ownership; 0600 and 0700 on Zeal's files; nothing secret in argv or the environment |
| Malware running as the user | read `wallet.db`, copy the identity to guess offline, use the cookie, add an age recipient, edit binaries; under X11 log keys and read the screen | the passphrase lock; each decrypted secret checked against its row | short unlock windows, no passive leaks; beyond that out of scope, said plainly |
| A stolen disk, backups, swap, core dumps | read `wallet.db`, guess passphrases, recover secrets | keys encrypted | no core dumps, encrypted backups, a warning without full-disk encryption |
| Supply chain, updates | ship a malicious Zeal, Zallet or zebrad | signed, reproducible releases | pins, an attested `indexer` build, the updater's wallet rules |
| Shoulder surfing, clipboard hijackers | read the phrase, swap a pasted address | — | reveal while held; Rust clipboard; chunked comparison |
| User error, phishing | the wrong network, a decimal slip, a link in a memo, public diagnostics | — | network checks, typed amounts, plain-text memos, private diagnostics |
| A compromised webview | show one thing and send another; read what is typed in it | — | checks in Rust; a confirmation and a passphrase prompt it can't touch |

| Identity | Another local user | Malware, wallet locked | Malware, during an unlock | An offline copy of the home folder |
| --- | --- | --- | --- | --- |
| no passphrase (Testnet only) | nothing | spend at any time | spend | spend |
| a passphrase (Mainnet) | nothing | read history; guess offline (scrypt, about 1 s a guess); wait | spend within the 30 s window: the cookie can't tell Zeal from malware | read history; guess |

A passphrase in the Linux Secret Service would turn the second row into the first, since any
process in the session can read it, so Zeal doesn't offer it.

### Requirements, in priority order

**Tier 0, every build that runs Zallet (from W1):**

1. No secret in argv or the environment; `env_clear` and a pinned `RUST_LOG=info`.
2. The setup runner outside the log pipeline: `Zeroizing` buffers, allow-listed fields only.
3. The phrase never stored or logged by Zeal; shown once, again only after a new export (from
   W4 also the passphrase); zeroized before the quiz.
4. A separate `wallet` capability with typed commands and a Rust allow-list of Zallet methods,
   no passthrough; nothing returns a key, the identity, a cookie, a PCZT or raw transaction hex;
   key export and import and `signmessage` unreachable; no wallet data in `emit` events.
5. Each phase's allow-list pinned by a test: W1's has no method that spends, signs or exports a
   key.
6. **Listener ownership** (Linux): the port's LISTEN row in `/proc/net/tcp` or `tcp6`, owned by
   the user, with an inode among `/proc/<child pid>/fd`. Checked when Zallet becomes ready and
   after any connection error, before each passphrase request, before showing a receive
   address, on zebrad's RPC and indexer ports before Zallet starts, and on zebrad's RPC port
   before a broadcast. A mismatch, or a check that can't run, refuses; Zeal never goes ahead.
7. Folders 0700 and files 0600, created so; no filesystems without POSIX permissions.
8. `zallet.toml` managed whole: one loopback bind, no insecure bind, no `[[rpc.auth]]`, the
   identity outside the datadir.
9. Redaction on ingest for every child; wallet logs and the raw `zallet.toml` out of diagnostics
   by default (the summary carries it with paths as `<path>`); a preview before copying.
10. Integer zatoshis end to end, no `f64`, every address checked against the network, ZEC or
    TAZ on every amount.
11. Memos, errors and release notes as plain text: no `inner_html`, no links, bidi and control
    characters removed; Zallet errors mapped to fixed messages.
12. No passphrase only on Testnet, decided from the identity file's first non-comment line:
    `AGE-SECRET-KEY-1…` is the plaintext kind, `-----BEGIN AGE ENCRYPTED FILE-----` the
    passphrase kind, and anything else is refused.

**Tier 1, before Mainnet funds (W5):**

13. A passphrase identity, required; KeyGuard around every keyed call, Zallet stopped if the
    lock check fails; a lock and check at quit, update and suspend.
14. A confirmation the webview can't draw (the GTK prompt, at least a `tauri-plugin-dialog`
    confirmation rendered from Rust's review); the review and delta checks; one send at a time;
    the outbox before a broadcast.
15. The send gate with HERD.md's H2 and H6 and thresholds in S_eff; the fail-closed coupling with
    the zebrad node; the `indexer` build; a pinned Zallet and the compatibility table.
16. The updater's real minisign key (`src-tauri/tauri.conf.json:82` holds
    `REPLACE_WITH_MINISIGN_PUBLIC_KEY`) and its [wallet rules](#updates).
17. `PR_SET_DUMPABLE=0`, `RLIMIT_CORE` 1, the Rust clipboard, warnings under X11 and without
    full-disk encryption.
18. Webview hardening: an `on_navigation` guard, `freezePrototype: true`, CSP `form-action
    'none'`, `frame-src` narrowed and Grafana never on wallet pages, CI keeping Tauri's
    `devtools` feature off, `build.rs` refusing `remote` capabilities and dangerous names in the
    wallet capability.
19. A passed restore drill, a rehearsed paired upgrade, SECURITY.md's wallet section, and an
    external review of this list.

**Tier 2, after the beta:** the [upstream asks](#upstream-asks); a hardware identity once
`init-wallet-encryption` takes age plugins; Zeal decoding PCZTs itself with the `pczt` crate; a
second signature on Zeal updates.

### Key custody

- **Two kinds of identity**: an X25519 key in clear (`AGE-SECRET-KEY-1…`) or an armored age file
  under a passphrase (scrypt tuned to about 1 s; empty refused). Zeal reads the first
  non-comment line and refuses a Mainnet wallet with the first kind: "if Zallet is running, funds
  can be spent at any time" (`Z/book/src/cli/init-wallet-encryption.md`).
- **Testnet W1 to W3 has no passphrase**, so setup needs no terminal: `unlock_on_terminal` does
  nothing for such an identity (`Z/zallet-core/src/components/keystore.rs:432-447`), and Zallet's
  own test drives setup over pipes (`Z/backends/zebra/tests/acceptance.rs:56-175`). The phrase is
  labelled "Testnet only: never use these words for real funds", since the same words derive
  Mainnet keys. Zallet can't add a passphrase later, so these wallets are recreated from W4, as
  the page says up front.
- **Passphrases**: 12 characters or more on Mainnet, 8 on Testnet, no line breaks, a strength
  hint, and "This passphrase protects this computer's copy; your recovery phrase restores your
  funds anywhere. Zeal can't recover either."
- **KeyGuard**: the prompt gives the passphrase; `walletpassphrase(passphrase, 30)` (−14 if
  wrong); exactly one call: `pczt_sign`, `z_getnewaccount`, `z_recoveraccounts`, or on Testnet
  `z_sendmany`, which derives its key before it returns an operation id
  (`methods/z_send_many.rs:167-181`); `walletlock`; then `getwalletinfo.unlocked_until` must read
  0, which it does once `lock()` drops the relock task (`keystore.rs:449-466`,
  `get_wallet_info.rs:113-119`). Otherwise Zeal stops Zallet, whose memory is the only place
  decrypted keys live. There is no "stay unlocked".
- **Setup commands** that need the passphrase (`init-wallet-encryption`, `export-mnemonic`,
  `confirm-backup`) run with Zallet stopped and take it on the PTY. `import-mnemonic` asks only for
  the words, on the same PTY, because it encrypts to the recipients already stored. The
  `SetupActor` keeps the passphrase in a `SecretBox` through setup, dropping it after 10 minutes
  idle.
- **Decrypting the export** puts the decrypted identity, the key to every secret in
  `wallet.db`, in Zeal for about a second, in memory that can't be dumped, dropped at once; Zeal
  holds the passphrase then anyway. Generating the seed in Zeal was rejected: Zeal would own the
  entropy and still need this decryption to show the phrase again.
- **Showing the phrase again** takes the passphrase, a pause and a new export. No OS keyring on
  Linux in any phase. Removing a wallet never deletes the identity alone.

### Signing exactly what was reviewed

`pczt_extract` skips the check `z_sendmany` makes of transparent outputs against keys derived
again from the seed (`methods/z_send_many.rs:219-221`: "addresses come from wallet database
records that are not integrity-protected"). So Rust checks `pczt_inspect` against the draft and
refuses unless:

1. every transparent output is a requested recipient at its exact value: a shielded send has no
   transparent change;
2. each requested recipient appears once, at its exact `value_zat`, with the memo's bytes;
3. every other output is change Zallet marks as the wallet's (how it marks change is
   unconfirmed; a Regtest fixture pins it before W2);
4. the fee is within the ZIP 317 fee of the inspected shape under the schedule pinned for this
   Zallet version, never a fixed floor: 5,000 zat a logical action, 2 grace actions in
   `zcash_primitives` 0.31.0, the crates.io release Zallet locks
   (`src/transaction/fees/zip317.rs:19,24`), as in 0.31.0-pre.2 and librustzcash's main branch.
   A Zallet that follows zebrad's 1,000 zat
   (`Zb/zebra-rpc/src/methods/types/get_standard_fee.rs:8-34`) needs a new row. A fixture pins
   how Ironwood actions count. A fee above 0.001 ZEC needs a typed confirmation;
5. the expiry is the target height plus the default delta of its consensus branch, 40 blocks
   before NU7 and 120 from NU7 (ZIP 218; `zcash_primitives` 0.31.0 `default_tx_expiry_delta`), so
   after the tip and at most 41 blocks ahead of it before NU7, 121 from NU7. Zallet sets no expiry
   of its own (`Z/zallet-core/src/components/json_rpc/methods/pczt_create.rs:231`) and ignores
   `builder.tx_expiry_delta` (`Z/zallet-core/src/commands/start.rs:148-149`). The exception is a
   ZIP 318 canonical crossing: one Orchard note paying a single {1, 2, 5} × 10^k zatoshi amount
   into Ironwood, which `zcash_client_backend` 0.25.0's `propose_transfer` tries first for such a
   payment. It carries ZIP 318's rolling expiry for the target height (`zcash_protocol` 0.11.0
   `zip318::expiry_height`), 30 to 60 days ahead. Zeal recomputes that height and accepts exactly
   it, since Zallet picks the shape and refusing would block the payment, and the review shows
   when it expires;
6. the consensus branch is the next block's, from Zeal's pinned activation table, with a
   warning when the expiry reaches the next activation;
7. `wallet_created` is set and the signing hints name this account;
8. the policy the PCZT records is the one the user acknowledged.

The proven PCZT is inspected again and must match. After `pczt_extract`, `z_viewtransaction` on
the stored, unbroadcast transaction (it reads `wallet.db`, `view_transaction.rs`) must show this
account's delta as exactly −(payments + fee) and no other account negative, or nothing is
broadcast and the funds return at expiry. That catches a Zallet bug or a damaged PCZT, not a
consistent edit of `wallet.db` (zcash/zallet#807, #790).

### Secrets that never cross IPC

Never sent to the webview: either cookie, an `Authorization` header, Zallet's URL; the identity
in either form; the export ciphertext; seeds and spending or viewing keys; PCZTs and raw
transaction hex; the passphrase; unredacted log lines, Zallet's raw errors, RPC methods and
parameters; the files `wallet.json`, `outbox/` and `contacts.json` as stored, and an outbox
entry's hex. Their fields cross only as typed replies (`AccountSummary`, `BackupState`,
`BinaryCheck`, `SetupState`, `Contact`, `WalletTxStatus`).

- **Inbound**, as command arguments only: quiz words, restore words (W4), and the passphrase only
  if the GTK prompt falls back to the webview; each a `Deserialize`-only type over
  `SecretString`, moved straight to its owner.
- **Outbound once**: the 24 words, in `wallet_reveal_phrase`'s reply.
- **Wallet data** (balances, addresses, txids, memos): replies and per-call channels, never
  `app.emit`, which broadcasts. The CSP's `connect-src 'self' ipc: http://ipc.localhost` keeps
  the webview off Zallet's port anyway.

Copies nobody can zeroize, documented: Tauri's IPC buffers, the JavaScript strings of shown and
typed words, hyper's buffers for `walletpassphrase`, GTK's entry buffer, Zallet's own buffers.

### Redaction in logs, files and diagnostics

- **On ingest, for every child**: `logs::sanitize` runs first in `push_line` and `push_note`,
  before the file, the ring, the detector and the broadcast. It strips SGR codes and redacts
  `Authorization:` lines, `__cookie__:\S+`, `AGE-SECRET-KEY-1…`, age armour blocks,
  `secret-extended-key-…`, WIF-shaped strings and runs of 12 or more BIP 39 words.
- **A wallet's lines are private**: Zallet's INFO lines name the wallet's txids (`Getting status
  of`, `Enhancing`, `Recovering spend of`; `Scanning mempool tx` names every mempool transaction,
  `Z/zallet-core/src/components/sync.rs:1052-1396`). They stay in the 0600 log and the Logs tab.
  Zallet logs method names, not parameters; zebrad logs `sendrawtransaction` at debug level only
  (`Zb/zebra-rpc/src/methods.rs:1550-1555`).
- **Copy diagnostics** leaves out the wallet's log and raw `zallet.toml` and adds a summary:
  version, whether the hash is pinned, phase, last exit class, heights, keystore and backup
  state, the herd's verdict, the number of accounts, no balances; `zallet.toml` with paths as
  `<path>`. Opting in adds the log tail with txids, 64-hex strings, addresses, viewing keys,
  seed fingerprints, UUIDs and amounts replaced by per-report tokens (`txid#1`). Everything is
  previewed before copying.
- **Zeal's own** wallet tracing (targets `launcher_core::wallet` and `zeal::wallet`) uses
  `#[instrument(skip_all)]` and logs no identifier or amount. Both targets join
  `EXCLUDED_TARGETS` in `src-tauri/src/logging.rs`, so their events reach the launcher log only,
  never a node's log stream.

### Memory and clipboard hygiene

- Secrets live in `secrecy` and `zeroize` types without `Debug`, `Clone` or `Serialize` (a
  `static_assertions` test), each owned by one actor and moved, never in a `watch` or an `Arc`.
- Zeal on Linux sets `PR_SET_DUMPABLE=0` (no core dumps, no same-user ptrace) and locks secret
  buffers in memory where it can; how this sits with WebKitGTK is a W0 spike. Children get
  `RLIMIT_CORE` 1, which stops piped core dumps where 0 would still start the helper
  (unconfirmed per distribution).
- The phrase screen reveals while held (keyboard: 30 s), hides on blur, has no copy, selection
  or context menu, warns under X11, and its DOM is cleared when the step closes.
- The clipboard is written from Rust with arboard 3.7, for addresses, txids and payment links
  only, kept out of clipboard history (`x-kde-passwordManagerHint` on Linux; GNOME and Klipper
  unconfirmed), and cleared after 60 s if it still holds Zeal's value. A webview clear is
  unreliable: WebKit allows clipboard writes only after a gesture. A pasted recipient is shown
  in groups of four, "Compare with where you copied it".

### Updates

- **Zeal's updater**, while a Mainnet wallet exists, never installs by itself, Auto mode's
  end-of-support countdown included; it installs only with the wallet locked, nothing in flight
  and the outbox empty, in this order: lock and check, stop Zallet, stop zebrad, install. It
  never unlocks afterwards. The prompt says "This update replaces the app that handles your
  wallet". The real updater key lands before W5.
- **Zallet** updates only by hand, under the rules in [Upgrades](#upgrades); **zebrad** moves with
  it through the compatibility table.

### Backups

The phrase and its recovery sheet (seed fingerprint, ZIP 32 account index, birthday height,
shown at setup and on Backup) restore the funds; with no key imports, that is complete for funds.
History, contacts, memos and an outbox entry aren't in the phrase.

- **Back up wallet file**: `B backup <node>/tmp` (an online SQLite backup, checked, 0600, safe
  while running, `Z/book/src/cli/backup.md`), encrypted with `age` to the wallet's own public key
  (no passphrase needed) as `zeal-wallet-<network>-<UTC>.db.age` in a folder chosen in a native
  dialog; the plaintext is deleted. Zeal doesn't restore this file. By hand: stop the wallet,
  decrypt it with `keys/encryption-identity.txt` (from W4 with its passphrase), and put it in
  place of `data/wallet.db`. In W1 to W3 it can be read only with this computer's key file, which
  Zeal can't save before passphrases arrive.
- **Save key file copy** (W4): the passphrase-protected identity, to keep apart from snapshots.
- A confirmed phrase and a fresh snapshot come before any Zallet upgrade.
- **Removing a wallet** shows the balance and backup state and takes the wallet's name typed into
  the GTK prompt (from W4); before W4, on Testnet only, into the webview's `TypedConfirm` (given a
  runtime word), which a compromised webview could fill, followed by a plain `tauri-plugin-dialog`
  confirmation from Rust that names the wallet and its balance.

### Testnet first, and Mainnet guardrails

W1 to W4 are Testnet only. Mainnet comes in W5, opt-in, labelled "beta, small amounts", after
Tier 1 and an external review, with:

- a passphrase, a pinned Zallet, and an `indexer` node synced from the first block;
- the herd gate, and an acknowledgement on every send while one node checks the chain;
- the native confirmation, the amount typed again above a threshold (1 ZEC by default) and for
  Max, warnings on a first payment, a look-alike of a contact and a transparent recipient; TEX
  refused in v1;
- the network on every review; another network's address refused;
- "Zeal never asks you to visit a link or to share your phrase" beside memos;
- a note above a set balance (10 ZEC by default): "Zallet is beta software: keep small amounts";
- an honest fee line: zebrad's standard is 1,000 zat an action on Testnet now and on Mainnet from
  height 3,590,000, about two weeks after NU7, so "that they switch together instead of
  revealing their wallet software by the fee they pay" (`get_standard_fee.rs`); while Zallet
  pays 5,000, the review says "Zallet pays more than the network's standard fee: this payment
  stands out".

### zeeder and nodes that don't vote

zeeder is HERD.md's optional seeder ([The seeder](HERD.md#the-seeder)). For the wallet it never
votes, never backs or feeds the wallet, has no path into wallet folders, runs as the user (never
root) with DNS on loopback above port 1,024, and while it crawls takes remote nodes' one slot per
address from the herd's nodes, so HERD.md runs it on request. Zallet, Zaino and zeeder have no
vote; Zebend and nodes marked "don't count" are observers; the zcashd compatibility build is a
validator whose rejection is an alarm.

### Out of scope, and said so

Malware running as the user beyond the windows above; anyone who can write `wallet.db`, who can
add an age recipient for future secrets or swap a viewing key (zcash/zallet#807, #790); X11 key
loggers; whoever controls the internet connection every herd node shares; majority hash power.
Broadcasts leave from the user's own address: there is no Tor.

## Backend

### Crates and modules

```text
crates/launcher-types/src/wallet.rs        wire types, wasm-clean
crates/launcher-herd/src/wallet.rs         HERD.md's wallet_view, confirmation states, send_check (pure; the mock runs it)
crates/launcher-core/src/kinds/zallet.rs   ZalletAdapter
crates/launcher-core/src/kinds/zebrad.rs   + variants, the indexer and health keys, the spend-index detector
crates/launcher-core/src/logs/sanitize.rs  redaction on ingest
crates/launcher-core/src/wallet/           pins (versions, hashes, the compatibility table, fee schedules,
                                           activation heights, expiry deltas) · binary · config · setup
                                           (SetupActor) · tty (W4) · phrase · meta (wallet.json) · client ·
                                           amount · monitor (WalletActor) · spendable · activity · recipient,
                                           review, gate, send, outbox, contacts (W2) · keyguard, birthday
                                           (W4) · listener · backup · secret
crates/launcher-core/src/bin/fake-zallet.rs   a test double, never bundled
src-tauri/src/wallet.rs, native_prompt.rs  the commands and channels; the GTK prompt and dialog confirmations
src-tauri/capabilities/wallet.json         the wallet capability
```

New dependencies, checked against `deny.toml` (MIT, Apache-2.0 or BSD expected): `age` 0.12 with
`armor`, `secrecy`, `zeroize`, `sha2`, `bip0039` (licence unconfirmed), `zip32`, `zcash_address`
and `zip321` (W2), `qrcode` (UI, no default features), `arboard` (src-tauri), nix's `term`
feature (W4), serde_json's `raw_value`. Not serde_json's `arbitrary_precision`, which would
change how Tauri parses IPC. A direct `gtk` 0.18 dependency may need the advisory exception its
transitive use has (RUSTSEC-2024-0370; unconfirmed).

### Actors and channels

No locks: actors own state, views go on `watch`, sends and KeyGuard sections each take a
one-permit `Semaphore`, and `clippy.toml` bans `Mutex` and `RwLock`.

```text
supervisor (zallet) ── watch<Status>, broadcast<LogLine> ──┐
zebrad node ── watch<Status>, watch<NodeStats>, events ────┼─▶ WalletActor ── watch<WalletSnapshot> ─▶ channels
herd ── watch<HerdView>, HerdHandle ───────────────────────┘        │ ZalletClient
UI ── commands ─▶ WalletHandle ── mpsc<WalletCmd>, oneshot ─────────┼─▶ SendActor (W2): reviews, outbox, Semaphore(1)
                                                                    └─▶ SetupActor: while setup, a reveal or a quiz runs
```

- **WalletActor**, one per wallet: the client and cached views (status, balances, accounts,
  recent activity, corroboration per height). It `select!`s over commands, a 5 s tick, the `New
  chain tip` and `Chain reorg detected` events, and the zebrad node's and the herd's watches.
  Slow calls run in spawned tasks that report back, so the loop never waits on Zallet.
- **SetupActor**: holds the wallet node's data-change permit while a lock-taking command, the
  reveal or the quiz runs (never across the start in step 7), and owns the passphrase (W4), the
  words until the reveal closes, and the quiz child.
- **SendActor** (W2): `HashMap<ReviewId, Prepared>` (PCZT, proven copy, draft, check results,
  expiry), the outbox and the slot. A review expires after 10 minutes, on a reorg, when its notes
  are spent, or when the gate starts blocking.
- **KeyGuard** (W4) runs one section at a time per wallet: each section takes the wallet's
  one-permit key `Semaphore` once the prompt returns, and holds it from `walletpassphrase` to the
  `unlocked_until` check. The lock at quit, update and suspend, and `wallet_lock`, take the same
  permit. Zallet has one unlock state (a second `walletpassphrase` resets the timer, `walletlock`
  clears every identity: `Z/zallet-core/src/components/keystore.rs:355-356,449-466`), so
  overlapping sections would fail with −13 or trip the lock check. Its prompt is a `SecretPrompt`
  trait, implemented by the GTK prompt and by a fake in tests. Secrets move between owners and
  never enter a `watch`, an `Arc` or a `Clone` type.

### The Zallet RPC client

Built on `rpc::RpcClient` (Zallet's cookie is the same `__cookie__:` Basic scheme), changed in
place: `decode` accepts `"result": null` for `T = ()` (today it says "no result",
`crates/launcher-core/src/rpc.rs:409-433`, which would fail `walletpassphrase` and `walletlock`);
per-call timeouts replace the 10 s `REQUEST_TIMEOUT` (`rpc.rs:25`): 10 s for polls, 30 s for lists
and `walletpassphrase` (scrypt may take 16 times its tuned second), 125 s for `pczt_prove`,
`z_getnewaccount` and `z_recoveraccounts`, above the managed 120 s; `pczt_prove` stores nothing,
so a timeout retries once; the `Authorization` value sits in `Zeroizing`.

Typed methods only, from an allow-list, `enum ZalletMethod`, that grows by phase:

| Phase | Methods |
| --- | --- |
| W1 | `getwalletstatus`; `getwalletinfo` (only `unlocked_until` and `mnemonic_seedfps`; its balances are always 0); `z_listaccounts`, `z_getaccount`; `z_getbalances`; `z_listunspent`; `z_listtransactions`, `z_viewtransaction`; `z_getaddressforaccount` with shielded receivers; `z_getnewaccount` in setup |
| W2 | `pczt_create`, `pczt_inspect`, `pczt_prove`, `pczt_sign`, `pczt_extract` |
| W3 | `z_listunifiedreceivers`; `z_sendmany` for TEX on Testnet; `z_getoperationstatus`, `z_getoperationresult`, `z_listoperationids` |
| W4 | `walletpassphrase` and `walletlock`, from `keyguard.rs` only; `z_recoveraccounts` |

Never: `z_sendfromaccount` (not sync-gated, `Z/.../json_rpc/methods.rs:1422-1441`, and blocking
through the proof), `z_shieldcoinbase`, `z_exportkey`, `z_exportviewingkey`, `z_importkey`,
`z_importviewingkey`, `z_importaddress`, `signmessage`, `stop`. Errors map to a kind and the text
the UI shows:

| Code | Kind | The UI says |
| --- | --- | --- |
| −10 | `CatchingUp` | "Your wallet is catching up (N blocks behind). Sending is paused until it finishes." |
| −2 | `Recovering` | "Your wallet is recovering from a chain reorganization. Sending resumes in a moment." |
| −13, −14, −15 | `Locked`, `WrongPassphrase`, `NoPassphrase` | "That passphrase isn't right." for −14 |
| −18 | `BackupNeeded` | "Back up your recovery phrase first." |
| −28 | `WarmingUp` | "Zallet is still starting." |
| −4 "Failed to propose transaction" | `CantPropose` (insufficient funds lands here; −6 is unused, zcash/zallet#256) | "Not enough spendable funds for this amount and the fee." |
| −4 "Transaction commit failed" | `BroadcastRejected` | "Zebra didn't accept the transaction." |
| −8 | `PolicyOrParameter` | normally caught at review |
| −5, −7, other | `InvalidAddress`, `Busy`, `Wallet` | "Zallet is busy; try again in a moment." for −7 |

Amounts come from the zatoshi fields (`valueZat`, `fee_zat`, all of `z_listtransactions`);
ZEC-only fields such as `z_viewtransaction`'s `fee` are read as `RawValue` and their text parsed;
outgoing amounts are decimal strings made from zatoshis (`Z/.../json_rpc/utils.rs:345-361`).
`wallet/` denies `clippy::float_arithmetic` and `clippy::cast_precision_loss`.

### The kind adapter

`ZalletAdapter` implements MULTI-NODE.md's adapter. The gate is pure: the registry gathers the
facts, the adapter decides.

```rust
impl KindAdapter for ZalletAdapter {
    fn availability(&self) -> Availability;          // Linux x86-64 and aarch64
    fn capabilities(&self) -> Caps;                  // wallet: true, config_form: false, log_analysis: true
    fn inspect(&self, program: &Path) -> Result<BinaryInfo, KindError>;  // permissions, SHA-256 against the pins, then --version on the copy
    fn write_config(&self, node: &NodeSpec, dep: &DependencyEndpoint) -> Result<(), KindError>;
    fn launch_spec(&self, node: &NodeSpec, dep: &DependencyEndpoint) -> LaunchSpec;
    fn detector(&self) -> Box<dyn KindDetector>;
    fn classify_exit(&self, exit: &ExitInfo, seen: &[LogEvent], requested: bool) -> ExitClass;
    fn gate(&self, facts: &DependencyFacts) -> Result<(), WaitReason>;
}
pub struct DependencyEndpoint { pub node: NodeId, pub network: Network, pub rpc: SocketAddr,
    pub cookie: PathBuf, pub indexer: SocketAddr, pub state_dir: PathBuf }
pub struct DependencyFacts { pub running: bool, pub pid: Option<u32>, pub chain: Option<Network>,
    pub rpc_answers: bool, pub indexer_open: bool, pub listeners_owned: Option<bool>,
    pub state: Option<ZebradStateInfo> }
pub struct ZebradStateInfo { pub major: u64, pub variant: ZebradVariant, pub spend_index: SpendIndexState }
pub enum ZebradVariant { Release, Indexer }
```

The Zallet detector yields `LogEvent::{WalletRpcOpened, ProvingWarm, NewTip, Reorg, AtTip,
GrpcLost, MempoolError, ConsensusDivergence, TaskExited}`; the zebrad detector adds
`BuildInfo { features, state_version }` from the diagnostic metadata, `SpendIndex { Started |
Finished }` and `DatabaseCreated`.

### The send pipeline

From W2, in the `SendActor`; the webview holds only a `ReviewId`.

1. **The draft** is checked in Rust with `zcash_address` and `zip321`: kind and receivers,
   network, ZIP 316 expiry, links, invisible and bidi characters, unknown `req-` parameters
   (refused, as ZIP 321 requires), own address, contacts and look-alikes (same first and last 6
   characters), a first payment. Memos go only to shielded recipients, at most 512 bytes.
2. **`pczt_create(from = account UUID, amounts, no minconf, "NoPrivacy", fund_source)`** needs no
   keys; no `minconf` means ZIP 315's 3 and 10. It returns the least policy the transaction
   needs, recorded in the PCZT; input selection doesn't depend on the policy passed
   (`Z/.../json_rpc/payments.rs:180-182`), so one call is enough.
3. **`pczt_inspect`**, the [review checks](#signing-exactly-what-was-reviewed), and a review
   under a random single-use id.
4. **`pczt_prove`** runs while the user reads (no keys; Zallet's own `z_sendfromaccount` proves
   before signing, `methods/z_send_from_account.rs:94-161`); the proven PCZT is inspected again.
5. **Send**: the slot, the gate again, then the confirmation the webview can't draw (the dialog
   in W2 and W3, the GTK prompt with the passphrase from W4).
6. **`pczt_sign(proven, acknowledged policy, strict = true)`**, in KeyGuard from W4; it refuses an
   acknowledgement weaker than the recorded policy (`methods/pczt_sign.rs`).
7. **`pczt_extract`**: `stored` must be true, and the inputs now count as spent. Extract is
   sync-gated but sign isn't (`methods.rs:1488,1500-1517`), so a −2 or −10 here means a reorg or
   lag in between: the review is dropped and proposed again. The signed PCZT is kept until
   extract returns.
8. **The account-delta check**; on a mismatch nothing is broadcast.
9. **The outbox**: `outbox/<txid>.json { hex, expiry_height }`, atomic, 0600, before any broadcast.
10. **Broadcast** with the wallet's zebrad `sendrawtransaction`, after its listener check, then
    HERD.md's `mempool_seen`. Zeal sends again from the outbox through that node at launch and
    whenever the transaction is in neither its mempool nor a block, until mined or expired;
    through another witness only when the user picks it, told "This makes the payment easier to
    link to this computer" (HERD.md; Biryukov and Tikhomirov, 2019).
11. **Progress** on the caller's channel: Proving, Waiting for you, Signing, Recorded, Broadcast,
    Seen by n of m, Mined, Confirmed.

**Max** proposes, reads `fee_zat` and proposes again for the total less the fee (Zallet has no
"send all"). **Shield** (W3) uses `fund_source = [taddr]`, one transparent address per transaction
(ZIP 315), pays the total less the fee to the account's own shielded address under
`AllowRevealedSenders`, and waits for the spend index. **TEX** (W3, Testnet) can't be a PCZT, since
it makes two transactions: `z_sendmany` runs under the same gate and unlock rules with its
operation id tracked, and the review says "This payment can't be shown to you exactly before it
is signed. It makes two transactions, and both are public."

### The send gate

`send_gate(&GateInputs) -> SendGate { blockers, warnings }`, pure, for the review and again
before signing. HERD.md's `send_check` supplies the herd's facts and sentences; the wallet module
decides. S_eff is HERD.md's effective spacing.

| Condition | Result |
| --- | --- |
| Zallet catching up (`getwalletstatus.locked`) or recovering, or `wallet_tip` ≠ `node_tip` | blocked |
| the wallet's node behind: `/ready` not 200, HERD.md's `Lagging` or "may be cut off", or its tip older than 12 S_eff while others move | blocked |
| `Split` with the wallet's node in the minority, no majority, or implementations disagreeing; a validator rejecting a block in its chain; a witness with another hash at `node_tip − 2` | blocked, with the facts and the herd's actions (restart, reset peers, mark a node "don't count"), none in the send flow |
| `Split` with the wallet's node on the majority side | an explicit confirmation showing the fork |
| the herd `Stalled`, attention / problem | a warning / blocked |
| `Alone` | "Not cross-checked: one node", acknowledged on every send |
| `Isolated` without lag | a warning |
| a reorg on the wallet's node in the last 12 S_eff; proving caches not warm | blocked |
| the node past its end of support; a transparent source before the index is Ready | blocked |
| another send in flight, or a listener not verified | blocked |
| on Mainnet: an unpinned binary, no passphrase, or a Release node | blocked |

HERD.md recommends a typed override for the minority case, since a local majority can be made or
can merely reflect which nodes the user runs, and a block lets whoever upsets the cheaper nodes
freeze sends. This design keeps "send anyway" out of the send flow, where social engineering
aims, and leaves a deliberate way out on the herd's pages
([question 4](#open-questions-for-the-maintainer)).

### What the wallet takes from the herd

HERD.md's interface, unchanged: `wallet_view(&HerdView, wallet_node) -> WalletHerd` (`Starting`,
`Alone`, `Agree`, `Lagging`, `Isolated`, `Split { fork_height, branches, side }`, and `Stalled {
since }`, which this design adds as HERD.md asks); `corroborate(network, refs) ->
Vec<Corroboration>` (same with depth, behind, different, unreachable); its confirmation states,
Pending, Confirming (below Zallet's 3 or 10), Confirmed (the wallet's node at depth, a majority
of witnesses on the same block, none on another beyond the race tolerance), Disputed ("Not
confirmed by your other nodes", out of Spendable now) and Reorged out; `mempool_seen`, withheld
while a node is PeeredWithHerd; reorg events for wallet transactions. Zeal also compares Zallet's
`node_tip` with the zebrad node's `NodeStats.best`. H6's wallet hooks come with W2.

### Balances and activity

- **Pools** from `z_getbalances(minconf = 1)`: spendable, pending and dust, Ironwood apart from
  Orchard.
- **Spendable now** matches what Send can spend, which that call doesn't (its `minconf` is
  symmetric). From `z_listunspent(minconf = 0)`: shielded notes with `trusted_confirmations` if
  `walletInternal`, else `untrusted_confirmations` (using `walletInternal` for ZIP 315's
  "trusted" is unconfirmed), above the schedule's dust threshold, not in a Disputed block. The
  rest is Pending; transparent value is never spendable.
- **Activity** from `z_listtransactions` (experimental: oldest first, a row per transaction and
  account), cached per account, refreshed from 100 blocks below the last height seen plus the
  mempool, merged by txid, newest first. Statuses come from `mined_height`, the tip,
  `expiry_height`, `expired_unmined`, the outbox and the herd. Valid UTF-8 memos show as text,
  others as "Binary memo (n bytes)": Zallet's text and hex memos look alike.

### Changes to existing code

In place where possible; a pure move, if needed, is its own commit before behaviour changes.

| Where | Change |
| --- | --- |
| `process.rs`, `process/unix.rs` | `ZebradCommand` and `spawn_here` become MULTI-NODE.md's `LaunchSpec { program, args, env: EnvPolicy, stdin, session: Group \| Session, rlimits, stop }`; zebrad keeps its command line |
| `logs.rs`, `supervisor/actor.rs` | `strip_sgr` before `parse_line`; `trait KindDetector`, today's code as `ZebradDetector`; `logs::sanitize` first in `push_line` and `push_note` |
| `rpc.rs` | null results, per-call timeouts, a `Zeroizing` authorization |
| `config.rs` | `ManagedKeys` gains `indexer_addr` and `health_addr`; `entries()`, a fixed array of 6 (line 94), becomes a `Vec`; config paths per node; where a wallet reads, `apply_config` refuses `lightwalletd_listen_addr` and `ensure()` deletes `metrics.endpoint_addr` at every start (leaving it out of `entries()` would not remove it: `ensure()` keeps every key it doesn't set, lines 169-172) |
| `fsutil.rs`, `paths.rs`, `logfile.rs` | `write_private` (0600, fsync, rename) and `create_private_dir` (0700); today `write_atomic` and `ensure_dirs` follow the umask |
| `launcher-types/src/nodes.rs` | `PortRole::Indexer` and `Health` |
| `launcher-types/src/lib.rs` | `ExitClass` (line 237) gains `UpgradeRequired`, `SetupRequired`, `ListenerMismatch` and `BinaryChanged`. Each is added to `is_deterministic()` (lines 257-266, a `matches!` the compiler doesn't check; `policy.rs`'s `on_exit` retries every class it doesn't list), so none is retried. Each gets an arm in the UI's exhaustive matches (`format.rs` `exit_class_text`, `health.rs` `failed`, `insights/events.rs` `exit_words`) and in `exit_class_named` (`insights/events.rs:79-90`), whose `_` arm would drop them |
| `ui/src/app.rs`, `herd_header.rs`, `node_summary.rs` | `Tab::NODE` (line 86) and `node_tabs(primary)` become tabs per kind; a wallet's word comes from its phase |
| `ui/src/components/add_node.rs` | the Zallet card. The requirement (line 25) becomes "Your zallet-zebra · Linux · reads a Zebra node on this computer". The program prompt (line 46) asks for the tarball or `zallet-zebra` (`/usr/bin/zallet-zebra`). The caveat (lines 66-67) drops "which the bundled zebrad is not" and says that transparent funds and Mainnet need a zebrad built with the `indexer` feature. Off Linux the card stays visible and can't be picked, with the sentence in [Platforms](#platforms) (the dialog disables no kind today) |
| `src-tauri/build.rs` | check every capability file, not only `capabilities/main.json` (line 51); refuse `remote` sections in all of them, and, among the commands in `capabilities/wallet.json`, names containing `rpc`, `call`, `raw`, `export_key`, `import` or `sign_message`; accept `wallet::` paths in `generate_handler!` (`handler_commands`, lines 105-128, panics on any entry that isn't a `commands::` path) |
| `src-tauri/src/logging.rs` | `EXCLUDED_TARGETS` (line 42) gains `launcher_core::wallet` and `zeal::wallet`, and its doc comment says why |
| `src-tauri/tauri.conf.json` | `freezePrototype: true`; CSP `form-action 'none'`; `frame-src` narrowed from any loopback port (line 36) |

## Types and Tauri commands

Wire types go in `crates/launcher-types/src/wallet.rs`, serde only and wasm-clean, shared by the
backend, the UI and the mock. `NodeId`, `Network`, `BlockRef`, `BlockHash`, `ExitClass`,
`WalletHerd`, `Corroboration` and `SendCheck` exist in `launcher-types` or come with HERD.md.
`TxId` and `Pool` come with EXPLORER.md and are reused here; wallet views show four of its pools.

```rust
/// Zatoshis; on the wire the decimal string of the integer ("125000000"), never a float.
#[serde(try_from = "String", into = "String")]
pub struct Zatoshis(u64);
impl Zatoshis {
    pub const MAX: u64 = 21_000_000 * 100_000_000;
    pub fn parse_decimal(text: &str) -> Result<Self, AmountError>; // ≤ 8 decimals, ASCII digits, no sign or exponent, never rounds
    pub fn to_decimal(self) -> String;                               // 8 fixed decimals
}
pub struct ZatDelta(i64);                                            // signed, same wire form
pub enum Unit { Zec, Taz }
pub struct AccountId(String); pub struct SeedFingerprint(String); // validated
pub struct ReviewId(u64); pub struct SendId(u64); pub struct QuizId(u64);
// TxId and Pool are EXPLORER.md's, in launcher-types; wallet views show four of its pools.

pub struct WalletSnapshot { pub node: NodeId, pub network: Network, pub phase: WalletPhase,
    pub sync: WalletSync, pub keystore: KeystoreState, pub backup: BackupState, pub herd: WalletHerd,
    pub gate: SendGate, pub balances: Option<Balances>, pub spend_index: SpendIndexState,
    pub binary: BinaryCheck, pub updated_at: SystemTime }
pub enum WalletPhase { Unavailable { why: Unavailable }, NotSetUp, SettingUp { step: SetupStep },
    WaitingFor { node: NodeId, why: WaitReason }, Starting, WarmingUp, CatchingUp, Recovering, Ready,
    Paused { why: PauseReason }, Stopped, UpgradeRequired { why: UpgradeWhy }, Failed { class: ExitClass } }
pub enum WaitReason { NodeStopped, NodeStarting, RpcNotAnswering, IndexerPortClosed, NoStateYet,
    NetworkMismatch, ListenerUnverified, UnsupportedPair }
pub enum UpgradeWhy { ConsensusDivergence { height: u32 }, StateFormat { major: u64 }, Consensus }
pub struct WalletSync { pub wallet_tip: Option<BlockRef>, pub node_tip: Option<BlockRef>,
    pub network_tip: Option<u32>, pub scan: Option<(u64, u64)> }
pub enum KeystoreState { NoPassphrase, Locked }              // no "unlocked": nothing stays unlocked
pub enum BackupState { NoSeed, Unconfirmed { seedfp: SeedFingerprint }, Confirmed { seedfp: SeedFingerprint } }
pub enum SpendIndexState { Missing, Building, Ready }
pub struct BinaryCheck { pub version: String, pub sha256: String, pub pinned: bool }
pub struct Balances { pub as_of_height: u32, pub stale: bool, pub spendable_now: Zatoshis,
    pub pending: Zatoshis, pub transparent: Zatoshis, pub immature_coinbase: Zatoshis,
    pub dust: Zatoshis, pub disputed: Zatoshis, pub pools: Vec<PoolBalance> }
pub struct PoolBalance { pub pool: Pool, pub spendable: Zatoshis, pub pending: Zatoshis, pub dust: Zatoshis }
pub struct AccountSummary { pub id: AccountId, pub name: String, pub seedfp: Option<SeedFingerprint>,
    pub zip32_index: Option<u32>, pub birthday: Option<u32> }
pub enum ReceiveKind { Shielded, Compatible, Transparent }  // ["orchard"], ["orchard","sapling"], from W3
pub struct TxRow { pub txid: TxId, pub delta: ZatDelta, pub fee: Option<Zatoshis>,
    pub status: WalletTxStatus, pub memo: Option<String> /* cleaned; None if binary */,
    pub mined_height: Option<u32> }
pub enum WalletTxStatus { NotBroadcast, InMempool { seen: Option<(u8, u8)> },
    Confirming { confirmations: u32, needed: u32 }, Confirmed { on: Option<(u8, u8)> }, Expired { at: u32 },
    Disputed { fork_height: u32 }, ReorgedOut }
// Also: ReceiveAddress; ActivityQuery and ActivityPage; TxDetail (a TxRow with block hash, expiry and
// Vec<(NodeId, Corroboration)>); RecipientCheck (kind, receivers, network, own, contact, look-alike, problems).

pub enum SetupStep { Identity, WalletDb, Seed, Reveal, Quiz, Start, Account, Restore }
pub enum SetupState { NotStarted, Running { step: SetupStep }, NeedsReveal { seedfp: SeedFingerprint },
    NeedsQuiz { seedfp: SeedFingerprint }, NeedsAccount, Done { account: AccountId, birthday: u32 },
    Failed { step: SetupStep, error: WalletErrorKind } }
pub struct CreateOptions { pub account_name: String, pub protection: Protection }
pub enum Protection { NoPassphraseTestnet, Passphrase }      // the passphrase goes into the native prompt
pub enum QuizStep { Ask { quiz: QuizId, position: u8, number: u8 }, Confirmed, WrongWord, Expired }
pub enum BirthdayChoice { Height(u32), Month { year: u16, month: u8 }, Unknown }

pub struct SendDraft { pub account: AccountId, pub to: String, pub amount: AmountInput,
    pub memo: Option<String>, pub source: FundSource }
pub enum AmountInput { Exact(Zatoshis), Max }
pub enum FundSource { Shielded, Transparent { address: String } }
pub struct Review { pub id: ReviewId, pub network: Network, pub outputs: Vec<ReviewOutput>,
    pub fee: Zatoshis, pub standard_fee: Option<Zatoshis>, pub total: Zatoshis, pub change: Zatoshis,
    pub policy: PrivacyPolicy, pub reveals: Vec<Reveal>, pub expiry_height: u32, pub herd: WalletHerd,
    pub gate: SendGate, pub tex: bool, pub typed_amount: bool, pub valid_until: SystemTime }
pub enum PrivacyPolicy { FullPrivacy, AllowRevealedAmounts, AllowRevealedRecipients,
    AllowRevealedSenders, AllowFullyTransparent, AllowLinkingAccountAddresses, NoPrivacy }
pub struct SendAck { pub review: ReviewId, pub policy: PrivacyPolicy, pub not_cross_checked: bool,
    pub split_confirmed: bool, pub typed_amount: Option<String> }
pub struct SendGate { pub blockers: Vec<SendBlocker>, pub warnings: Vec<SendWarning> } // the gate's rows
pub enum SendStep { Proving, WaitingForYou, Signing, Recorded { txid: TxId }, Broadcast { via: NodeId },
    Seen { by: u8, of: u8 }, Mined { height: u32 }, Confirmed, Expired { at: u32 }, Failed { error: WalletErrorKind } }
pub struct WalletError { pub kind: WalletErrorKind, pub message: String }  // Zeal's fixed text
pub enum WalletEvent { Snapshot(WalletSnapshot), ActivityChanged, Setup(SetupState) }
```

Secret types stay in launcher-core (`wallet::secret`): `SecretWord`, `PhraseEntry` and, only if
the GTK spike fails, `PassphraseArg` are `Deserialize`-only over `SecretString`;
`RevealedPhrase { words, seedfp, account_index: Option<u32>, birthday: Option<u32>, testnet_only }`
is `Serialize`-only and goes out once, with `account_index` and `birthday` `None` at first setup.
None is `Clone`; `Debug` prints `[redacted]`. The UI sends secrets as strings and clears them.

Commands live in `src-tauri/src/wallet.rs`. Each takes the wallet's `node: NodeId`, returns
`Result<_, WalletError>`, and appears in `build.rs`'s `COMMANDS`, `generate_handler!` and
`capabilities/wallet.json`. A phase registers only its own, so a W1 build has no send command.

| Phase | Command | Returns |
| --- | --- | --- |
| W1 | `wallet_snapshot(node)` | `WalletSnapshot` |
| W1 | `wallet_subscribe(node, channel: Channel<WalletEvent>)` | `()`; a channel per webview with a generation counter, like `subscribe_logs` |
| W1 | `wallet_accounts(node)` | `Vec<AccountSummary>` |
| W1 | `wallet_receive_address(node, account: AccountId, kind: ReceiveKind, fresh: bool)` | `ReceiveAddress`, after the listener check |
| W1 | `wallet_activity(node, query: ActivityQuery)`; `wallet_transaction(node, txid: TxId)` | `ActivityPage`; `TxDetail` |
| W1 | `wallet_setup_state(node)`; `wallet_setup_create(node, options: CreateOptions)`; `wallet_finish_setup(node)` | `SetupState` |
| W1 | `wallet_reveal_phrase(node, seedfp: SeedFingerprint)` | `RevealedPhrase`, the one secret that goes out |
| W1 | `wallet_quiz_start(node, seedfp)`; `wallet_quiz_answer(node, quiz: QuizId, word: SecretWord)`; `wallet_quiz_cancel(node, quiz)` | `QuizStep`; `QuizStep`; `()` |
| W1 | `wallet_backup_file(node)`; `wallet_copy(node, what: CopyTarget)` | `BackupReport` (Rust opens the dialog); `()` (the Rust clipboard) |
| W2 | `wallet_check_recipient(node, text: String)` | `RecipientCheck` |
| W2 | `wallet_propose(node, draft: SendDraft)`; `wallet_discard_review(node, review: ReviewId)` | `Review`; `()` |
| W2 | `wallet_send(node, ack: SendAck, progress: Channel<SendStep>)` | `SendId`; Rust shows the confirmation (from W4 with the passphrase) |
| W2 | `wallet_rebroadcast(node, txid: TxId, via: Option<NodeId>)` | `()` |
| W2 | `wallet_payment_request(node, request: RequestInput)` | `PaymentRequest`: a ZIP 321 URI and its QR matrix |
| W2 | `wallet_contacts(node)`; `wallet_save_contact(node, draft: ContactDraft)`; `wallet_delete_contact(node, id: ContactId)` | `Vec<Contact>`; `Contact`; `()` |
| W3 | `wallet_shield_propose(node, account: AccountId, from: String)` | `Review` |
| W4 | `wallet_setup_restore(node, words: PhraseEntry, birthday: BirthdayChoice, accounts: Vec<u32>)` | `SetupState` |
| W4 | `wallet_estimate_birthday(node, year: u16, month: u8)`; `wallet_add_account(node, name: String)`; `wallet_lock(node)` | `u32`; `AccountSummary`; `()` |

Adding, starting, stopping and removing the wallet use MULTI-NODE.md's registry commands
(`add_node` with a Zallet draft naming its zebrad, `inspect_binary`, `start_node`, `stop_node`,
`remove_node`) with the wallet's stop guard. The UI subscribes, then takes a snapshot, as
ARCHITECTURE.md prescribes. No `emit` event carries wallet data; the sidebar's word comes through
the registry's `nodes` summaries as a phase.

## UI

Pages in `ui/src/pages/wallet/`, components in `ui/src/components/wallet/` (amount, address
chunks, QR, word grid, quiz, trust chip, next step), a pure view model `ui/src/wallet_model.rs`
tested natively like `node_summary.rs`, and `ui/styles/wallet.css`. Text nodes only, never
`inner_html`; status in words, not colour alone; labelled amounts; every flow by keyboard.

### Placement

- The Mainnet and Testnet tabs scope the window (MULTI-NODE.md). The network's wallet comes first
  in the sidebar with one word: Ready, Catching up 62 %, Back up needed, Waiting for Zebra,
  Paused, Needs a Zallet update, Problem. No balance in the sidebar (a setting) or the tray.
- With a wallet, the window opens on Wallet Home; the overview card shows its state, "reads
  Zebra · Testnet" and the herd's line, no balance.
- Tabs from the kind: Home, Send (W2), Receive, Activity, Backup; under "⋯" Accounts, Contacts
  (W2), Logs, Configure (read-only), Node settings.
- The header: `◆ Wallet · Testnet  [Main ▾]  ✓ 3 of 3 nodes agree · block 4,471,230`, with
  HERD.md's sentences ("Cross-check off: one node", "Zebra · Testnet is 6 blocks behind your
  other nodes") and a drawer of each witness's block at the wallet's height. With a passphrase:
  "Locked · unlocks only to sign".

### Onboarding

From the add-node dialog's Zallet card (its review shows `zallet.toml`, the ports and "Zeal opens
a wallet port on Zebra · Testnet and restarts it once"), the first-run offer on Linux, or the
overview card. One question per screen, with Back, resuming from `wallet.json`:

1. **A private wallet on your own node**: keys stay here, the wallet reads your Zebra node and no
   server, your other nodes check it. A checklist, each line with its fix: Linux; a Zebra node on
   this network here ([Add one]); the wallet port; free disk; the `zallet-zebra` file and whether
   its hash is pinned. "Zallet is beta: an upgrade may need a new wallet. Use small amounts."
   [Create a new wallet] [Restore a wallet] (W4).
2. **Protection.** W1 to W3: "On Testnet this wallet's key file is kept without a passphrase:
   anything running as you can spend these test coins. You'll make a new wallet when passphrases
   arrive." From W4, the passphrase, in the native prompt.
3. **Creating**: identity, database, phrase, each with a fixed error and Retry.
4. **Before you see your phrase**: on paper; nobody watching or recording; never into a website.
5. **Your recovery phrase**: 24 numbered words revealed while held, with the seed fingerprint
   ("write it next to your phrase"), "Testnet only" on Testnet, the X11 warning; [I've written it
   down] after 10 s.
6. **Check your backup**: "Word #4?" three times, Zallet choosing; BIP 39 completion;
   `autocomplete`, `spellcheck` and `autocapitalize` off. A wrong word: "That's not word #11.
   Check what you wrote; Zallet will ask three new words", with [Try again] and [Show my phrase
   again] (a new export, never beside the quiz). [Back up later] stops setup before the account:
   Zallet refuses `z_getnewaccount` with −18 until the backup is confirmed
   (`Z/zallet-core/src/components/json_rpc/methods/get_new_account.rs:83`), so until the quiz
   passes the wallet has no account, address, balance or activity (setup stays at
   `NeedsQuiz { seedfp }`), and Home shows only the backup step.
7. **Starting your wallet**: a live list (the port, Zallet starting, scanning 62 %, the first
   address); "Payments appear once Zebra · Testnet reaches the network" when it is behind.
8. **Done**: the recovery sheet (fingerprint, account 0, birthday: "write these next to your
   phrase"), the first address and its QR, then Receive.

**Restore** (W4): 24 fields with completion; a paste is split, with a warning that the clipboard
held the words. The birthday is a month, turned into a height by bisecting block times on the
user's own archive less a week, a block height, or "I don't know" (scanning from Sapling
activation for hours). Accounts: 0, and "Also restore account 1…".

### Home

```text
 Spendable now   12.34000000 TAZ                                          [Send] [Receive]
 Pending         +0.50000000     1 payment · 7 of 10 confirmations · about a minute
 Transparent      0.20000000     public until shielded                    [Shield…]
 ▸ Pools: Ironwood 11.90 · Orchard 0.44 · Sapling 0 · Transparent 0.20 · dust 0.00002
 ! Back up your recovery phrase: Zallet creates no account until you do.     [Back up]
 Recent  ↓ +0.5    Shielded payment · "invoice 42" · Pending 7/10
         ↑ −1.25   Alice · Confirmed · on 3 of 3 nodes                       [All activity]
 Wallet 4,471,230 · Zebra · Testnet 4,471,230 · your nodes 4,471,230
```

- 8 decimals, truncated, never rounded; an eye hides amounts; no fiat. W1 has no Send button,
  and transparent value shows only if some arrives, "shielding comes in a later version".
- One next step at a time: a node or herd problem with its fix; an unfinished backup; catching
  up in two stages ("Zebra 94.5 % of the network · wallet scan 62 %"); transparent funds to
  shield; funds in the old Orchard pool ("they move to Ironwood when spent, and the amount moved
  is public", unconfirmed); Alone ("Add a pruned node to cross-check"); a coming end of support
  or Zallet update.
- Until the backup is confirmed the wallet has no account, so the backup row shows alone,
  without the balance rows or activity; `?wallet=unbacked` mocks that state.
- Catching up greys the last amounts "as of block N" and disables Send with its reason;
  Waiting names the node ([Start it]); UpgradeRequired says "Zallet stopped at block N: a network
  upgrade this version can't follow" [Choose a newer zallet-zebra]; each Failed class has one
  sentence ("Zeal refused to talk to the program on port P: it isn't your wallet").

### Receive

A unified address with an Orchard receiver only, so payments land in Ironwood and later sends
keep full privacy: a QR code drawn as SVG from a `qrcode` matrix, the address in groups of four
with the first and last ten characters stressed, [Copy] through Rust, and [New address], since a
shielded-only request takes a time-based index and gives an address no one can link to the
others (how often it changes is unconfirmed, so a repeat is tolerated). [Request an amount] (W2)
makes a ZIP 321 link and QR. "Other address types": one with a Sapling receiver too, and from W3
a transparent address tagged PUBLIC ("anyone can see funds sent here; Zeal will ask you to
shield them"), kept in `wallet.json` to spare the gap limit. Without a confirmed backup: "Back up
your recovery phrase to get an address" [Back up]. No address while the listener is unverified.

### Send, Review, Progress

From W2. **To** takes an address, a ZIP 321 link or a contact, with a chip: "Shielded · arrives
in Ironwood", "Sapling, an older pool", "Transparent · public", "TEX: not supported yet", "Your
own address", "Contact: Alice", a look-alike warning with the different characters marked, "This
is a Mainnet address; this wallet is on Testnet", "Contains invisible characters". **Amount**
with [Max]; a **memo** for shielded recipients, counted in bytes. Review stays disabled with the
gate's sentence while a blocker holds.

```text
 Review payment · Testnet
 To        Alice (contact)  u1qz…8k2m  [show all]         shielded · arrives in Ironwood
 Amount    1.25000000 TAZ           Memo "Invoice 42" (10 of 512 bytes)
 Fee       0.00015000 TAZ           3 actions (ZIP 317) · more than the standard 0.00003000
 Total     1.25015000 TAZ from Main · 0.37000000 change back to you, shielded
 Privacy   High: only the fee is public
 Expires   if not mined by block 4,471,351 (about 50 min)
 Network   ✓ 3 of 3 nodes agree at block 4,471,230
                                                           [Cancel]  [Send 1.25 TAZ (3)]
```

- Below High privacy the row turns amber with a fixed sentence and a required box ("The 0.44
  moving from Orchard to Ironwood is public; sender and recipient stay private"). Warnings are
  rows: a first payment, a transparent recipient, an expiry past an upgrade, "Not cross-checked:
  one node".
- Send counts down 3 s; the native confirmation repeats the review's lines; on Mainnet the amount
  is typed again above the threshold and for Max.
- Progress, which the user may leave: Proofs ready, Signed, Saved to your wallet, Sent through
  Zebra · Testnet, Seen by 2 of 3 nodes, Mined in block N, Confirmed. Failures: "Zebra didn't
  accept it" with the cleaned detail folded and [Send again]; "Not seen by your other nodes after
  2 min" with [Send again] and [Send through Zakura · Testnet…]; "Expired at block N; the funds are
  spendable again".
- Shield (W3): "From your transparent address t…; to your shielded address. Privacy: Low; it
  links this address to the shielding transaction."

### History

Activity, newest first, grouped by day, filtered by direction, memo or account, searched locally
over contacts and memos. A row: direction, the contact or "Shielded payment" (shielded senders
are unknown), a memo excerpt, the signed amount, a status: Not sent yet, In 2 of 3 mempools,
Pending 7/10, Confirmed · on 3 of 3 nodes, Expired (struck through), Not confirmed by your other
nodes, Undone by a reorg. The detail drawer adds the fee, the time, the memo as plain text or
"Binary memo (n bytes)" with "Zeal never asks you to visit a link or share your phrase", [Save
contact], the txid with [Copy], the user's nodes (same block, behind, different, not answering),
and **[Open in explorer]**: the explorer's `tx/<txid>` route on the wallet's node through
`AppState.explorer_request` (EXPLORER.md). No link leaves Zeal.

### Accounts, Backup and Settings

- **Accounts** (under "⋯"): name, fingerprint, ZIP 32 index and birthday per account; a header
  switcher from the second account. [Add account] (W4) runs in KeyGuard, needs a confirmed backup,
  and says each account adds scanning and accounts aren't labels
  (`Z/book/src/concepts/accounts.md`). Zallet can't rename or delete one; neither can Zeal.
- **Backup**: the seed card (fingerprint, date, backup state, account index, birthday); [Show
  recovery phrase] (a pause; from W4 the passphrase); [Confirm backup] while unconfirmed; [Back up
  wallet file…] ("history; restoring it needs the key file"); from W4 [Save key file
  copy…]; "Zallet 0.1.0-beta.4 can't change a passphrase"; Remove wallet ([Backups](#backups)).
- **Settings → Wallet**: hide amounts at start, the balance in the sidebar (off), the Mainnet
  thresholds. The wallet's **Node settings**: the binary, its version and pin; the node it reads,
  its variant and index; ports; the folder; a lower `sync.recover_batch_size`; a health card
  (uptime, memory, CPU, sizes of `data/` and `tmp/`, the last exit in words).

### Testnet

The warning band ("Testnet · coins have no value"), TAZ on every amount, "Testnet" in the window
title, a one-time "Testnet coins are worthless", and the network in every review. "Catching up"
is only Zallet's sync lock, "Locked" only the keystore.

### The mock

`ui/src/mock/wallet.rs`, flags in `ui/src/mock.rs` and `ui/README.md`. `?wallet` adds a Testnet
Zallet node and, when no herd flag already provides it, the Testnet zebrad it reads
(`zebrad-testnet`, the Zebra · Testnet that `?herd` adds); with `?herd`, the wallet reads the
herd's `zebrad-testnet`. It combines with HERD.md's `?herd=…&agree=…`.
Synthetic data only: Testnet addresses, invented txids, and the BIP 39 vector of zero entropy
("abandon" ×23, "art") marked TEST VECTOR; Mainnet addresses fail their checksum on purpose, and
pages carry a MOCK DATA band. A modal "native prompt (mock)" accepts "test". `?wallet=` takes
`new`, `restore`, `quiz-wrong`, `unbacked`, `waiting`, `catching-up`, `recovering`, `ready`,
`empty`, `expired`, `memo` (bidi characters), `transparent`, `spendindex`, `send`, `policy`,
`proving`, `pending`, `notseen`, `tex`, `locked`, `upgrade`, `listener`, `mainnet` and
`unsupported`; the headless-Chromium workflow screenshots each.

## Testing

- **Pure tests on all three OSes**: `Zatoshis` ("-1", "1e3", "0.000000001" refused); the
  `zallet.toml` golden file; the sanitize corpus through `push_line`, absent from the ring, the
  file, the broadcast and diagnostics; detectors and exit classes against fixtures; parsers
  failing closed; the error map; the gate table; the review checks against deceptive PCZTs (an
  extra transparent output, a changed amount, an excess fee, a wrong branch); recipient vectors;
  the birthday bisection; secret types not `Clone`, `Debug` or `Serialize`; no wallet data in
  event payloads; each phase's allow-list.
- **fake-zallet** (`crates/launcher-core/src/bin/fake-zallet.rs`), built with rpassword 7.5.4 and
  age 0.12.1 so the PTY runner meets their real behaviour: every setup command with Zallet's
  strings and colours, a real age export of the test vector, the quiz, `.lock`, `.cookie`; `start`
  with cookie JSON-RPC moving from −28 to −10 to synced, −18 until the backup, the W1 to W4
  methods, `pczt_*` with opaque tokens; modes for every exit class, divergence at a height, a slow
  first proof, a refused broadcast, a deceptive PCZT, and a dump of its argv and environment.
  fake-zebrad gains the indexer port, a `version` file (`+indexer` on request), the spend-index
  lines and a record of `sendrawtransaction`.
- **Integration**: setup killed at every step and resumed; a version mismatch failing closed; the
  gate and waiting states; a zebrad crash stopping Zallet until the gate passes; stop order;
  timeouts; one send at a time.
- **Real binaries** (`crates/launcher-core/tests/real_zallet.rs`, `#[ignore]`, run on bump PRs;
  `ZALLET_BIN`, `ZEBRAD_BIN`, `ZEBRAD_INDEXER_BIN`) on Regtest, with `consensus.regtest_nuparams`
  matching zebrad (syntax unconfirmed) and `require_backup = true` set, since Regtest turns it off:
  setup through Zeal's runner; coins from `generatetoaddress` (`Zb/zebra-rpc/src/methods.rs:788-804`)
  shielded by a test-only client; a PCZT send on the release build (the shielded-only premise);
  prove, sign, extract and the delta check; −13 while locked, `unlocked_until` after the lock; the
  outbox after a kill between extract and broadcast; zebrad restarted under Zeal's coupling;
  `SIGTERM`, exit 0, no cookie left; a second Zallet on the folder; the spend index on a new and a
  converted database. These runs capture the fixtures (codes, stderr bytes, colours, prompts,
  `--version`), and need a hidden Regtest network in launcher-core (MULTI-NODE.md question 5).
- **Testnet, before each phase ships**: set up and back up; a faucet payment from Pending to
  Confirmed on every node; zebrad restarted, then Zallet; Zeal quit during setup and resumed; a
  wallet-file backup; from W2 sends, mempools, expiry and a herd split; from W4 the **restore
  drill**: a fresh install restores from the phrase and recovery sheet, with a birthday weeks back,
  and finds every payment.
- **CI**: the hermetic suite on Linux, macOS and Windows; a Linux wallet job, nightly and on a
  label, with the pinned tarball checked by SHA-256 and the `indexer` build cached.

## Phases

Each phase is a pull request or a short series with its tests, mock states and screenshots, and
ARCHITECTURE.md updated. Order: after the explorer, the herd, resource monitoring and the light
client server have shipped ([Status](#status)), W0, W1, W2, then W3 and W4 side by side, W5, W6.
HERD.md's H6 wallet hooks come with W2. Zallet stays in MULTI-NODE.md's phase 5.

| Phase | Ships | Needs | Safe because | Screenshots |
| --- | --- | --- | --- | --- |
| **W0 Groundwork** | hardening for every child (redaction, 0700 and 0600, `PR_SET_DUMPABLE`, `RLIMIT_CORE`, the webview items, `build.rs`); the `rpc.rs`, `LaunchSpec` and detector seams; the indexer keys and variant detection; the wire types and every wallet screen in the mock; written spikes on Regtest with the real beta.4 (fixtures, shielded use on a release zebrad, the PTY runner, the GTK prompt, prove-sign-extract, `z_viewtransaction` on a stored transaction, a zebrad restart, the index's cost, #588); these doc changes | MULTI-NODE.md phases 1 and 2; the explorer, HERD.md's H2, resource monitoring and the light client server | no wallet runs; the hardening also protects zebrad users | every `?wallet=` state |
| **W1 Testnet: set up, back up, receive, history** (Linux) | the kind, with a pinned or accepted binary; `zallet.toml`; supervision, exit classes, coupling; the gate on the bundled zebrad and its indexer port; setup over pipes without a passphrase; the reveal and Zallet's quiz; `wallet.json`; the encrypted wallet-file backup; Home with Spendable now, Receive (shielded), Activity, Accounts, Backup; the Rust clipboard; HERD.md's view | MULTI-NODE.md phase 2 and its dependencies | no RPC can spend, sign or export a key (a test pins the allow-list); the phrase leaves Rust only in `wallet_reveal_phrase`; Testnet only | the app on Testnet; `?wallet=new`, `unbacked`, `catching-up`, `ready` |
| **W2 Testnet send** | the PCZT pipeline and its checks, background proving, the native dialog, the outbox, broadcast, `mempool_seen`; recipients, memos, ZIP 321, Max, contacts; the gate with HERD.md's H6 wallet hooks; still no passphrase, no TEX | W1; HERD.md's H2 to cross-check | Testnet; the signed transaction is the reviewed one | `?wallet=send`, `policy`, `proving`, `pending`, with `&agree=split` |
| **W3 The indexer zebrad** (Testnet) | `zeal-zebrad-indexer` built and attested, or a user build accepted by its version; the one-way switch and index states; the transparent address; Shield; TEX through `z_sendmany`, labelled, with operation ids | W2; HERD.md's H3 (the heavy-job gate an `indexer` restart takes); beside W4 | Testnet; transparent features wait for Ready | `?wallet=transparent`, `spendindex`, `tex` |
| **W4 Passphrase, KeyGuard, restore** (Testnet) | the PTY runner; passphrase identities (the Testnet default; older wallets recreated); the GTK prompt or its fallback, residual named; KeyGuard; the phrase again with the passphrase; restore with a birthday from a month; Add account; locking at quit, update, suspend; the updater's wallet rules; the restore drill | W1, and W2 for signing | Mainnet custody tried on worthless coins | `?wallet=locked`, `restore`, `quiz-wrong` |
| **W5 Mainnet beta**, opt-in | Mainnet with all of Tier 1: a passphrase, a pinned Zallet, an `indexer` node from the first block, the herd gate, native confirmation, typed amounts, the fee line, the balance note; the first-run offer on Linux | W2 to W4; HERD.md's H2, H3 and H6; the real updater key; a passed drill; a rehearsed paired upgrade; an external review | every gate fails closed; "beta, small amounts" | `?wallet=mainnet`, `&agree=split` |
| **W6 Later** | TEX on Mainnet; coinbase shielding; viewing-key export behind consent; zcashd migration; a verified Zallet download; a hardware identity; ZIP 318's Orchard to Ironwood migration once Zallet has it; no PTY once Zallet takes secrets over a descriptor | per item | per item | per item |

zeeder is HERD.md's H5 and needs none of these.

## Changes to other documents

- **MULTI-NODE.md**: drop the non-goal "Being a wallet". Zallet's section: `indexer` builds the
  spend index and doesn't gate the gRPC; setup also needs `export-mnemonic` and `confirm-backup`,
  since `require_backup` is on, and passphrase prompts need a PTY; Linux-only is policy, not code;
  add the leftover `$TMPDIR` folder, the mempool poll every second, exit 0 at a divergence, no
  rebroadcast, lost operation ids, stopping Zallet before zebrad updates. Point question 2 to this
  design's [question 3](#open-questions-for-the-maintainer) (proposed: a Zeal-built `indexer`
  sidecar); tabs per kind.
- **HERD.md**: its role matrix allows a Release zebrad behind a Testnet wallet in W1 and W2,
  shielded only; its probe of the startup diagnostic metadata is the variant check used here.
  Question 8: one `indexer` archive serves the explorer and the wallet. Question 9: a wallet
  backend never starts from a snapshot on Mainnet. Question 3 stays open; it is this design's
  question 4.
- **EXPLORER.md**: "Zeal is not a wallet" becomes "the explorer decrypts nothing; the wallet is
  its own kind"; its open item closes (the gRPC starts without the feature); the wallet links in
  through `AppState.explorer_request`.
- **SECURITY.md**: the threat model, with a wallet section (assets, the local attacker, residual
  copies, out of scope, and the `indexer` build's provenance if question 3 settles on the
  Zeal-built sidecar) and the updater key before W5.
- **ARCHITECTURE.md**: the `env_clear` exception, the new exit classes, the managed keys of a
  wallet's zebrad, wallet data on per-call channels. **SIDECARS.md**: the
  `zeal-zebrad-indexer` row, if question 3 settles on that sidecar. **ui/README.md**: the
  `?wallet` flags.

### Upstream asks

- **zcash/zallet**: secrets over a file descriptor for the setup commands (#474) and
  machine-readable setup output; an RPC for a seed's backup state; a Unix-socket RPC that checks
  its peer; `pczt_extract` checking transparent outputs as `z_sendmany` does, and `pczt_inspect`
  marking change; `z_getbalances` applying ZIP 315; a sync gate on `z_sendfromaccount`;
  `PR_SET_DUMPABLE`; traces without headers; a passphrase change; zebrad's fee schedule;
  readiness (#366) and graceful shutdown (#184).
- **Zebra**: `indexer` release builds; the stale doc comment in `zebra-rpc/src/config/rpc.rs`; a
  Testnet indexer port convention.

## Open questions for the maintainer

1. **Showing the phrase.** Zallet "never displays a recovery phrase" (`confirm_backup.rs`); this
   shows it once, held, with no copy, and again only after a new export. Agreed, or only "save an
   encrypted copy"?
2. **The Zallet binary.** A user file matching the pin (proposed through W5), a verified download
   on request, or a Linux bundle (about 70 MB, moving with Zeal and so with zebrad's format)?
3. **The `indexer` zebrad.** A Zeal-built sidecar for the nodes wallets read (proposed), that
   build for every Linux node, or user builds only? And ask Zebra to ship one?
4. **Sending while the wallet's node is in the minority.** A block in the send flow with the way
   out on the herd's pages (proposed), or HERD.md's typed override inside the send flow?
5. **Mainnet with one witness.** An acknowledgement per send (proposed), or two witnesses required?
6. **TEX on Mainnet v1.** Refused (proposed), though exchange deposit addresses use it, or the
   labelled `z_sendmany` flow?
7. **Confirmations after NU7.** At 25 s, 10 blocks are about 4 minutes and a third of the work they
   were. Keep Zallet's 3 and 10, or write `untrusted_confirmations = 30` on Mainnet to keep the old
   work and time (proposed; Testnet keeps 10, trusted keeps 3)?
8. **The passphrase prompt.** Native GTK (proposed, if the spike holds on Wayland and X11), or the
   webview field with a native confirmation and its risk named?
9. **Restore before Mainnet.** Working in Zeal (proposed), or, if zcash/zallet#588 blocks it, a
   documented restore in another ZIP 32 wallet that the drill has proven?
10. **No passphrase on Testnet in W1 to W3**, recreated in W4, and a hidden Regtest network for the
    contract tests: agreed?

## Sources

In this repository: [MULTI-NODE.md](MULTI-NODE.md), [HERD.md](HERD.md),
[ARCHITECTURE.md](ARCHITECTURE.md), [SECURITY.md](SECURITY.md), [SIDECARS.md](SIDECARS.md), the
EXPLORER.md draft, and the files cited inline.

Zallet at v0.1.0-beta.4 (commit 7b06224; https://github.com/zcash/zallet/releases/tag/v0.1.0-beta.4):
`zallet/`, `zallet-core/src/` (config, commands, keystore, sync, json_rpc and its methods, i18n),
`backends/zebra/` (source, CHANGELOG, tests), the book, `CHANGELOG.md`, `Cargo.lock` (age 0.12.1,
rpassword 7.5.4, pinentry 0.8.0, jsonrpsee 0.24.11, zcash_primitives 0.31.0); issues #184, #256,
#366, #474, #588, #643, #790, #807, #899.

Zebra at v7.0.0 (commit e8db87eb): `zebrad` (Cargo.toml, start, end of support), `zebra-rpc`
(sync, methods, config, `get_standard_fee.rs`, `server/rpc_metrics.rs`, the indexer proto),
`zebra-chain` (zip317, network upgrades), `zebra-state` (constants, disk_db, zebra_db,
`disk_format/upgrade.rs`), the book (`user/zallet.md`, `user/health.md`, `user/requirements.md`);
issues #10806, #11090, #11155.

Others: age 0.12.1, pinentry 0.8.0, rpassword 7.5.4, jsonrpsee-server 0.24.11; librustzcash's
`zcash_primitives/src/transaction/fees/zip317.rs` at 0.31.0-pre.2 and main; `zcash_primitives`
0.31.0 (`transaction/builder.rs`, `transaction/fees/zip317.rs`), `zcash_client_backend` 0.25.0 (`data_api/wallet.rs`) and
`zcash_protocol` 0.11.0 (`zip318.rs`); tao 0.37.2; arboard 3.7; ZIPs 218, 315 to 318, 320, 321
and the draft fee ZIP (zcash/zips#1352); Zodl, Zingo, Ywallet, Bitcoin Core, Electrum and Sparrow
for the screens; Biryukov and Tikhomirov, EuroS&P 2019.
