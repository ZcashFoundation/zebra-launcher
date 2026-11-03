# Public services: a light client server and a DNS seeder

## Status

> **Proposal, 2026-11-03.** A design for review before any code. It builds on
> [MULTI-NODE.md](MULTI-NODE.md) (the node registry, kinds, ports and phases) and
> [HERD.md](HERD.md) (roles, the zeeder kind and its census), and describes the launcher's
> current shape where needed. [ARCHITECTURE.md](ARCHITECTURE.md) stays the description of
> what is built and is updated phase by phase as this lands. Facts marked "unconfirmed" are
> to be checked before the phase that relies on them; numbers marked "proposal" are starting
> values, to be tuned.

The maintainer asked for two optional services that a user can turn on at home. First: "an
optional DNS seeder, pull in zeeder", and "The DNS seeder should let you expose the port and
direct your domain name to your server at home so other nodes can bootstrap". Then: "Add a
light client server too that can easily be enabled like zeeder for users to expose the port
and serve their own light client server from their home server". Asked which server, they
answered: "Zaino specifically".

So the light client server starts as zebrad's own light-wallet endpoint, served from the
zebrad Zeal already runs, and becomes [Zaino](https://github.com/zingolabs/zaino), run by Zeal
as a node that reads an archive node of the user's herd, once a Zaino release can follow NU7.
The seeder is [zeeder](https://github.com/ZcashFoundation/zeeder), HERD.md's seeder kind, in a
public mode. lightwalletd appears only as a note.

Three upstream facts shape the plan (checked 2026-11-03):

- **Zaino can't follow NU7 yet.** Every release up to 0.10.2 stops at the block before NU7's
  activation (Mainnet height 3,543,405, about 2026-11-05) and retries forever (zaino#1731).
  The fix, zaino#1655 "Feat: NU7", is open with `mergeable_state: blocked`, and there is no
  release candidate.
- **zebrad 7.0.0 serves light wallets itself, and follows NU7.** Its
  `[rpc] lightwalletd_listen_addr` starts a gRPC server inside zebrad that implements the
  lightwalletd protocol, in plaintext and without authentication, and that upstream calls
  experimental. So the first phase serves wallets from it, on loopback behind Zeal's TLS front
  door, with no extra binary; Zaino replaces it once a Zaino release with NU7 passes Zeal's
  tests: Testnet first, then Mainnet.
- **zeeder can't run beside a zebrad on its network's default port.** Its crawler always
  binds `[::]:8233` (Testnet `[::]:18233`), with no setting, and zebra-network panics when the
  port is taken (`ZcashNetwork::network_config`, zeeder `src/config.rs` at v1.8.0). Until
  zeeder has a setting for that listener, a public seeder goes through HERD.md's 8233 dialog.

Both services share one guided flow for what they have in common: the name, the router, the
firewall and the checks from outside.

## Goals and non-goals

### Goals

- Two optional services, each off until the user turns it on, each with one switch and a
  guided setup that checks every step and shows the fix first when a step fails.
- Only the service is reachable. zebrad's light-wallet endpoint, Zaino, zebrad's RPC and
  Zallet stay on loopback; the only internet-facing listeners are Zeal's TLS front door and
  zeeder's DNS port; no plaintext leaves the computer.
- Less exposure is one click and acts at once; more takes the guided steps, and the backend
  re-checks every gate rather than trusting the screen.
- Nothing public stays open after a pause or after Zeal stops, router mappings included.
- "Reachable from the internet" only with evidence, with its source and age; what becomes
  public, for good, said before anything does.
- Limits on by default, sized for a home connection.
- Zeal's rules: Rust only, a thin Tauri layer over testable crates, no mutexes, cargo-deny's
  licence allow-list, and no third party the user hasn't chosen.

### Non-goals

- **lightwalletd** (Go; it can't read zebrad's cookie, so its README asks for
  `enable_cookie_auth = false`, and its releases carry no binaries; [note](#lightwalletd)).
- **zebrad's light-wallet endpoint anywhere but loopback.** Zeal keeps refusing a non-loopback
  `lightwalletd_listen_addr`; the internet reaches the endpoint only through the front door.
- **A headless or system-service install.** Zeal is a desktop app: a home server here is a
  computer that stays on with someone signed in.
- **Getting listed** in Zebra's default seeders or a wallet's built-in servers, or serving at
  scale.
- **Paths without a domain and a real certificate**: IP-address or self-signed certificates,
  HTTP-01, plaintext on the LAN. Tailscale Serve, onion services, tunnels and Cloudflare's gRPC
  proxy are notes for later ([Phases](#phases)).
- **Changing the router or the OS behind the user's back.** Zeal never turns UPnP on, never
  runs `sudo`, and elevates on Windows only on a click.

## The light client server

### What it is

```text
phone wallet ──TLS, ALPN h2──▶ router :443 ──▶ this computer [::]:8443      the front door, in Zeal
                                                 │ Let's Encrypt certificate, limits
                                                 ▼ the same HTTP/2 frames, plaintext, on loopback
                                           zebrad 127.0.0.1:9067            its light-wallet endpoint
                                                 │ in process: zebrad's state, mempool, RPC methods
                                                 ▼
                                           the chain zebrad keeps           the node Zeal runs
```

zebrad 7.0.0 serves the lightwalletd gRPC protocol (`cash.z.wallet.sdk.rpc.CompactTxStreamer`)
itself: `[rpc] lightwalletd_listen_addr` starts a gRPC server inside zebrad. Zeal binds it to
loopback, on a port it assigns, only while the service is on. The **front door** (its name in
the UI), a small TLS server of Zeal's own that runs inside the app
([The front door](#the-front-door)), takes the internet's connections, ends TLS, applies the
limits and forwards the decrypted bytes to that port. No other binary is involved, and the
endpoint follows NU7 with zebrad.

Zaino takes zebrad's place behind the same front door once a Zaino release follows NU7 and
passes Zeal's tests ([Later: Zaino](#later-zaino-as-the-backend)); wallets keep the name, the
port and the certificate. PS1 ships the first part on Linux; front door v2, macOS, Zaino and
Windows follow ([Phases](#phases)).

### What zebrad's endpoint serves

The server came in Zebra 6.4.0 (2026-09-23, zebra#10953; `CHANGELOG.md:167-172`). It is tonic
0.14.5 with gRPC reflection beside the service (`zebra-rpc/src/lightwalletd/server.rs:80-103`)
and implements all 20 methods of its `service.proto`: the wallet-shaped ones by calling
zebrad's JSON-RPC methods in process, compact blocks and the mempool straight from the state
and mempool services (`server.rs:34-38`). Checked at v7.0.0; paths are under
`zebra-rpc/src/lightwalletd/` unless given.

| Method | What zebrad does | Where |
| --- | --- | --- |
| `GetLatestBlock` | the best tip's height and hash; `UNAVAILABLE` before there is a tip | `methods.rs:81-94` |
| `GetBlock`, `GetBlockNullifiers` | one compact block from the state, by height, or by hash in internal byte order | `methods.rs:96-118, 606-633` |
| `GetBlockRange`, `GetBlockRangeNullifiers` | heights only, ascending or descending, one block after another; the stream ends at the first error | `methods.rs:120-146, 753-801` |
| `GetTransaction` | by hash only, as lightwalletd: the best chain, then the mempool (height 0), then a side chain (height 2^64 − 1) | `methods.rs:148-218` |
| `SendTransaction` | `sendrawtransaction`; at most 2,000,000 bytes; a refusal comes back in `SendResponse`, as lightwalletd does | `methods.rs:48-51, 220-250` |
| `GetTaddressTxids`, `GetTaddressTransactions` | the same code: `getaddresstxids`, then each transaction from the state in block order; a height of 0 means the whole chain; no mempool; more than 100,000 matches is `INVALID_ARGUMENT` | `methods.rs:252-272, 805-863`; `../methods.rs:2512-2531, 4142` |
| `GetTaddressBalance`, `GetTaddressBalanceStream` | `getaddressbalance`; at most 10,000 addresses, a streamed one checked as it arrives | `methods.rs:274-294, 899-951`; `../methods.rs:4138` |
| `GetAddressUtxos`, `GetAddressUtxosStream` | `getaddressutxos`, honouring `startHeight` and `maxEntries`; with `maxEntries` 0, more than 100,000 outputs is refused | `methods.rs:541-559, 958-990`; `../methods.rs:486-496` |
| `GetMempoolTx` | the mempool's transactions that have shielded data, as compact transactions, minus up to 20,000 excluded suffixes | `methods.rs:41-46, 296-329` |
| `GetMempoolStream` | the mempool, then each new transaction, closing at the next block, as lightwalletd does | `methods.rs:331-410` |
| `GetTreeState`, `GetLatestTreeState` | `z_gettreestate`, by height or by hash (internal byte order, where lightwalletd reads it reversed); the latest by the tip's hash | `methods.rs:412-445, 606-615` |
| `GetSubtreeRoots` | `z_getsubtreesbyindex` for Sapling, Orchard or Ironwood, then one block-hash lookup per root | `methods.rs:447-539` |
| `GetLightdInfo` | from `getblockchaininfo` and `getinfo`: vendor `ZcashFoundation/zebra`, zebrad's build as `version` and `zcashdBuild`, its subversion, the chain name, the Sapling activation height, the tip's branch id, the heights; `gitCommit`, `branch`, `buildDate`, `buildUser` and `donationAddress` empty | `methods.rs:561-595` |
| `Ping` | `UNIMPLEMENTED` ("Ping is disabled"), as in a production lightwalletd | `methods.rs:597-601` |

**What it leaves out.** Zebra's proto files are lightwallet-protocol v0.3.6 with v0.5.0's
Ironwood fields at the same numbers (`zebra-rpc/proto/`, against the protocol's
`CHANGELOG.md`). Without v0.4.0 (2025-12-03) and v0.6.0 (2026-10-19), there is:

- no transparent data in compact blocks: `poolTypes` in `BlockRange` and `GetMempoolTxRequest`
  is an unknown field that zebrad ignores; `CompactTx` never carries `vin` or `vout` ("Fields 7
  and 8 are `vin` and `vout` upstream; they are left free here",
  `zebra-rpc/proto/compact_formats.proto:60-62`); and a transaction without shielded data isn't
  in a compact block at all (`zebra-rpc/src/lightwalletd.rs:35-42, 96-110`), `GetBlock`'s
  included, which v0.4.1 says returns every pool;
- no `upgradeName`, `upgradeHeight` or `lightwalletProtocolVersion` in `LightdInfo` (fields 16
  to 18);
- no `GetStandardFee`, which tonic answers `UNIMPLEMENTED`, although zebrad has the
  `getstandardfee` JSON-RPC (`zebra-rpc/src/methods.rs:721-722`).

The rest differs from lightwalletd in small ways: `CompactBlock.header` is empty
(`lightwalletd.rs:119`), which the current protocol asks for anyway; `CompactTx.fee` is unset
(`lightwalletd.rs:176-178`), which the protocol allows; v0.4.0's renames (`hash` to `txid`,
`Exclude` to `GetMempoolTxRequest`) don't change the wire.

**Limits it sets**: 20 concurrent streams per HTTP/2 connection (`server.rs:20-24`); an HTTP/2
ping every 30 s, closing a connection that doesn't answer within 10 s (`server.rs:26-30`);
requests up to 4 MiB and responses unbounded, tonic's defaults, which zebrad leaves alone
(tonic 0.14.5 `src/codec/mod.rs:101-102`); for the streams a task feeds (`GetBlockRange`,
`GetBlockRangeNullifiers`, `GetTaddressTxids`, `GetTaddressTransactions`, `GetMempoolStream`,
`GetSubtreeRoots`), a 64-message buffer and a 60 s wait for a client that has stopped reading,
after which the task stops, though the messages already queued stay until the client reads
them or the stream closes (`methods.rs:38-39, 53-58, 1096-1104`); `GetAddressUtxos`,
`GetAddressUtxosStream` and `GetMempoolTx` build the whole answer first, up to 100,000 outputs
or every shielded mempool transaction, and hold it until the client reads it or the stream
closes (`methods.rs:296-329, 541-559, 958-990`); and the request caps in the table. It sets no
connection limit, no per-client rate, no deadline and no per-method concurrency, and behind the
front door every client is 127.0.0.1 to it. It logs requests at DEBUG only, in a span with the
method and no address (`tracing.rs:55-117`).

**Also on the port**: `grpc.reflection.v1.ServerReflection` (`server.rs:80-82, 100`), which
describes the same public protocol.

### What a wallet may hit

Upstream calls the server **experimental**: "its behaviour and configuration may change in any
release, including in ways that break clients", and "Do not depend on it for a service you
operate for others yet: it has not been tested against the light-client SDKs that mobile
wallets are built on" (`book/src/user/lightwalletd.md:216-223`;
`zebra-rpc/src/config/rpc.rs:61-62`). That testing is listed as follow-up work in zebra#11158,
closed on 2026-08-18 when the server landed; whether another issue tracks it now is
unconfirmed. Zeal labels the service experimental wherever it appears, and its own wallet runs
([Testing](#testing)) are the only test with real wallets. From the code, a wallet may hit:

- **No transparent data in compact blocks.** A wallet that sets `poolTypes` with `TRANSPARENT`,
  or expects `vin` and `vout` from `GetBlock`, gets shielded data only, and no error. The
  protocol asks clients to check that a server can return transparent data first; with
  `lightwalletProtocolVersion` empty, a careful client treats zebrad as an older server
  (inferred).
  librustzcash 0.25.0's `sync` sends an empty `poolTypes` and reads transparent funds with
  `GetAddressUtxosStream` (`zcash_client_backend/src/sync.rs:352, 526`), which zebrad serves;
  what Zodl's SDKs, Zingo and Zkool send is unconfirmed.
- **A stream that ends early without an error.** When a client stops reading for 60 s, the
  producer gives up and drops its end of the channel, so the stream ends with status `OK`,
  short of the range asked for (`methods.rs:792-796, 1096-1104`; inferred from the code,
  untested). A phone that suspends the wallet mid-sync is the likely case; whether the SDKs
  notice the gap is unconfirmed.
- **Slow first syncs.** `GetBlockRange` reads each block and its tree sizes before the next
  (`methods.rs:776-797`); pipelining those reads is follow-up work in #11158. The wallet runs
  record the first-sync time.
- **Busy transparent addresses.** More than 100,000 transactions or unspent outputs for the
  addresses asked is `INVALID_ARGUMENT`.
- **`GetTreeState` by hash.** A client that sends the hash reversed, as lightwalletd reads it,
  gets `NOT_FOUND`; librustzcash asks by height (`sync.rs:381-385`).
- **Mempool calls before the tip.** While zebrad syncs, its mempool is off: `GetMempoolTx` and
  `GetMempoolStream` fail with `INTERNAL` ("mempool is not active: wait for Zebra to sync to
  the tip"), and `SendTransaction` comes back refused
  (`zebrad/src/components/mempool.rs:1151-1156, 1162-1170`). The front door stays `Unavailable`
  until zebrad is at the tip.
- **No upgrade notice, no fee, no donation address.** `upgradeName` and `upgradeHeight` are
  missing; `GetStandardFee` is `UNIMPLEMENTED`, though librustzcash's copy of the protocol
  doesn't have it yet (read 2026-11-03); and with no donation address to send, Share offers
  none while zebrad serves.

### The loopback port on a home server

The Zebra book says "Do not run it on a user's machine": of a port bound to `127.0.0.1`, "any
local process can reach it, and so can a web page loaded in a browser on that machine", and the
port has no authentication (`book/src/user/lightwalletd.md:249-259`). A home server is a user's
machine, so this is the risk that remains, and what Zeal does about it.

**Browsers can't call it.** Browsers speak HTTP/2 only over TLS, so they send HTTP/1.1 to a
plaintext port. zebrad's server accepts HTTP/2 with prior knowledge only: tonic's
`accept_http1` defaults to false (tonic 0.14.5 `src/transport/server/mod.rs:140`) and zebrad
doesn't set it (`server.rs:95-103`); zebra-rpc doesn't depend on tonic-web
(`zebra-rpc/Cargo.toml`); and with `http2_only`, hyper-util serves every connection as HTTP/2
without looking for HTTP/1 (hyper-util 0.1.20 `src/server/conn/auto/mod.rs:121-125, 231-235`),
so an HTTP/1.1 request fails the HTTP/2 preface and reaches no method. A page can still open a
connection, and so learn that something listens on the port. This is tonic's default, not a
promise Zebra documents, so Zeal's contract test sends an HTTP/1.1 `POST` to the endpoint at
every zebrad pin and fails if a gRPC answer comes back.

**Local programs can.** Any process on this computer can call every method without a password:
read the chain and the mempool, look up transparent addresses, submit transactions, and load
zebrad, which has no per-client limits. Programs that run as the same user gain nothing new:
they can read zebrad's cookie, which is 0600 (`zebra-rpc/src/server/cookie.rs:43-80`), and use
its JSON-RPC, which serves the same reads and `sendrawtransaction`. What is new is access for
other accounts on the computer and for sandboxed apps that may open connections but can't read
the user's files. Nothing on the endpoint holds keys or spends, and it keeps nothing per
client, so such a caller can't learn what the owner's wallets ask for; reading the plaintext
leg between the front door and zebrad needs packet capture, that is root.

**What Zeal does:**

- once a light client server is set up, the endpoint exists only while the service is on:
  Zeal writes the key then and removes it otherwise ([How Zeal runs it](#how-zeal-runs-it));
- it binds `127.0.0.1` only, never `[::]` or a LAN address, and `config::validate` keeps
  refusing any other value, as it does today (`crates/launcher-core/src/config.rs:291-332`);
- the front door is the only listener others can reach: the router forward and the firewall
  step name its port, and its SNI check refuses a connection for any other name, so a page that
  rebinds a name of its own to this computer gets nothing either;
- the consent screen says what remains: "While the light client server is switched on,
  including during setup and automatic pauses, programs on this computer can use Zebra's light
  wallet port without a password."

Only zebrad can close the gap for local programs, with an authenticated or Unix-socket
listener; it has neither (an upstream ask, [Phases](#phases)).

### How Zeal runs it

- **Which zebrad.** In PS1, the bundled zebrad 7.0.0 that Zeal runs today, on its network;
  every zebrad 7 keeps the whole chain (HERD.md), so restores can read old blocks. With several
  nodes (MULTI-NODE.md phase 2), a zebrad of the network whose version is on Zeal's tested list,
  preferring one that isn't Zallet's backend. Zakura 1.8.0 has no such endpoint: its start
  command spawns no lightwalletd server, only the indexer one (checked in a local copy of its
  `start.rs`).
- **The managed key.** Today Zeal doesn't manage `rpc.lightwalletd_listen_addr`: `validate`
  refuses a non-loopback value and leaves a loopback one to the user
  (`crates/launcher-core/src/config.rs:291-332`). Once a light client server is set up, Zeal
  owns the key: `rpc.lightwalletd_listen_addr = "127.0.0.1:<port>"`, written only while the
  service is on and removed otherwise; without one, a loopback value set by hand stays, as
  today. The port is 9067 on Mainnet and 19067 on Testnet (proposal), or the next free one
  above, checked when the key is written and kept in the service's spec, with the existing
  `PortRole::Grpc` (`crates/launcher-types/src/nodes.rs:270-279`); MULTI-NODE.md's allocator
  takes this over when it lands. `ManagedKeys` (`config.rs:60-73`) gains an optional address,
  and `ensure` and `begin_apply_with_managed` learn to remove the key, since `set_key` only
  inserts or replaces (`config.rs:211-250`). A hand edit to it is overridden like the other
  managed keys, and Configure shows it read-only, "managed by the light client server".
- **Restarts.** zebrad reads the key only at start, so turning the service on or off restarts
  zebrad: on Linux a SIGTERM stop, 8 to 37 ms in ARCHITECTURE.md's runs ("Measured shutdown
  times": an idle Regtest node and an early Testnet sync; a node at the tip, as a serving one
  is, isn't measured yet), then a start whose time to the endpoint on a synced node is
  unmeasured; the Testnet and Mainnet wallet runs time both. When the service resumes at
  launch, `ensure` writes the key before zebrad's first start, so there is no second restart.
  Automatic pauses close only the front door and keep the endpoint, except a network change
  (below); the switch off, "Turn off and forget" and a launch that doesn't resume remove it.
- **A network change.** In PS1 a light client server belongs to the network it was set up on,
  and the endpoint key is written only while `Settings.network` is that network. Saving the
  other network in Settings → Node pauses the service (`PauseReason::NetworkChanged`) and
  closes the front door, and the `config::ensure` that writes the new network also removes the
  endpoint's key, which other automatic pauses keep, before zebrad's next start. The spec is
  kept, and switching back offers Resume. The chain-name check runs at every zebrad start, and
  a mismatch pauses the service the same way.
- **Its address.** Zeal takes it from zebrad's `Opened lightwalletd gRPC endpoint at <addr>`
  (target `zebra_rpc::lightwalletd::server`; `zebra-rpc/src/server.rs:85`,
  `zebra-rpc/src/lightwalletd/server.rs:92`), as it reads the RPC address today
  (`crates/launcher-core/src/logs.rs:251-253`), and the front door connects to that address
  only. A captured fixture pins the line.
- **A taken port** fails zebrad's start with an error, not a panic: `init` returns the bind
  error, the start command returns it (`zebrad/src/commands/start.rs:576-596`), and zebrad logs
  `shutting down Zebra due to an error` at ERROR before it exits
  (`zebrad/src/components/tokio.rs:101-106`). Zeal's classifier finds `PortInUse` in panics
  only (`logs.rs:285-295`), so it gains a rule for this line, from a captured fixture (the exact
  text is unconfirmed). Zeal also checks the port when it writes the key.
- **The endpoint is part of zebrad**: if its task ends, zebrad shuts down
  (`start.rs:789-793, 883-888`).
- **The front door reaches it** with a plain TCP connection to the logged address and copies
  the decrypted bytes both ways: HTTP/2 over TLS and HTTP/2 with prior knowledge carry the same
  frames, and zebrad's server speaks only the latter.
- **Health** is zebrad's own: running, at the tip by Zeal's RPC monitor, and the endpoint
  opened. Zeal's small gRPC client also calls `GetLightdInfo` over h2c on loopback at start, at
  Open and every 10 minutes, and checks the chain name, the Sapling activation height and the
  branch id, which must equal `getblockchaininfo`'s `consensus.chaintip` (`TipConsensusBranch`,
  `zebra-rpc/src/methods.rs`): Zodl's own comparison.

### What wallets need

| Wallet | What the user enters | Certificate check | Without TLS |
| --- | --- | --- | --- |
| Zodl Android 3.19.0 (formerly Zashi) | Settings → Advanced → Choose a server → Custom: `host:port`, port required | Android's system store (grpc-java) | `grpc://`, or a `.onion` host over Tor |
| Zodl iOS | `host:port` | iOS's trust store, always TLS | none |
| Zingo mobile 3.0.0, Zingo PC 2.0.27 | `https://host:port` | Mozilla roots compiled into zingolib | `http://` to localhost only |
| Zkool 6.36.0 | `https://host:port` | tonic's `tls-webpki-roots` | `http://` to any host |

Ywallet 1.15.3 dropped Zcash after Ironwood, Edge hard-codes `zec.rocks:443` (that its UI has
no server field is unconfirmed), and Nighthawk is no longer maintained. So the server needs:

- **a name with a publicly trusted certificate**: self-signed fails everywhere ("do not use a
  self-signed one, our SDK will reject those", lightwalletd's README);
- **HTTP/2 over TLS, with ALPN `h2`**, as gRPC clients require;
- **the port in what users type**: Zodl requires it and Zingo appends 9067 without one
  (unconfirmed), so Zeal always shares `zec.example.com:443` (Zodl) and
  `https://zec.example.com:443` (Zingo, Zkool);
- **to pass Zodl's check**: `SdkSynchronizer.validateServerEndpoint` compares `chainName`,
  `saplingActivationHeight`, and `consensusBranchId` with the SDK's branch at the server's
  height, so a server stuck before NU7 fails it (inferred). Zeal runs the same check;
- **the methods wallets call**, all of which zebrad 7.0.0's endpoint serves, as does Zaino
  0.10.2: `GetLightdInfo`, `GetLatestBlock`, `GetBlockRange`, `GetTreeState`, `GetSubtreeRoots`,
  `GetTransaction`, `SendTransaction`, `GetMempoolStream`, `GetAddressUtxosStream`,
  `GetTaddressTxids` or `GetTaddressTransactions`.

Share says three things out loud: Zodl's Automatic mode broadcasts transactions to every
bundled server, and only Manual uses just the chosen one (`ServerSelectionPolicy.kt`); the
wallet must know NU7 too (Zodl 3.19.0 does; Zingo 3.0.0 and Zkool 6.36.0 unconfirmed); and the
server is Zebra's experimental one.

### NU7

zebrad 7.0.0 follows NU7: Mainnet activates it at 3,543,405 and Testnet did at 4,465,026
(`zebra-chain/src/parameters/constants.rs:76, 106`), with branch id `0x77190ad9`
(`network_upgrade.rs:245`). The endpoint is part of zebrad, so it crosses every upgrade zebrad
does, and `GetLightdInfo` reports the tip's branch id. On Mainnet, what stops it is zebrad's
end of support: 7.0.0 halts about 56 days after its estimated release height
(`CHANGELOG.md:30`), a halted zebrad serves nothing, and the service pauses until a Zeal update
brings a newer zebrad (ARCHITECTURE.md's end-of-support banners). On Testnet zebrad has no end
of support (`zebrad/src/components/sync/end_of_support.rs:53-60, 71-73`): a zebrad that
doesn't know a Testnet upgrade stays at the block before it, and the tip watch keeps the front
door `Unavailable` until a Zeal update.

### Later: Zaino as the backend

Zaino stays the server the maintainer chose. It takes zebrad's place behind the same front door
once a Zaino release follows NU7 and passes the gate below (PS5); the archive's
`lightwalletd_listen_addr` is then removed, and wallets keep the name, the port and the
certificate. Facts checked at tag `zainod-0.10.2` (`5ced0ae8`) on 2026-11-03.

**NU7.** 0.10.2 can't follow it: its `zcash_protocol` 0.10 has a placeholder NU7 branch id, so
it can't parse a block carrying `0x77190ad9`, and stops at the block before activation (#1731,
reproduced on Regtest); on Testnet, past NU7 at 4,465,026, it is presumably stuck too
(inferred). The fix, #1655 (head `aaf8122a`, updated 2026-11-02, `mergeable_state: blocked`,
no release candidate), moves to zebra-chain 15 and `zcash_protocol` 0.11, refuses to start on a
validator branch the build doesn't know, and adds `GetStandardFee`; it is marked breaking,
which suggests 0.11.0 (unconfirmed). First syncs are fragile anyway: 0.10.2's `GetSubtreeRoots`
fetches a block per subtree root (1,128 Sapling and 770 Orchard on Mainnet, #1665) under the
stream cap (#1537); they failed or crawled on a public Zaino, on zcash-devtool and on a
Raspberry Pi.

**The gate.** The pin moves only with a tagged release containing #1655, the contract test
(`real_zainod.rs`: a Regtest zebrad with NU7 at height 10, as in #1731, the pinned zainod and
the front door) and the Testnet wallet run; Mainnet follows after two weeks on Testnet with no
pause caused by Zaino. Whatever its version, Zeal compiles in the upgrades each tested version
knows and reads the scheduled and active ones from the archive's `getblockchaininfo`: a build
that doesn't know an active upgrade isn't started ("This Zaino doesn't know NU7, which is
active: it would stop at block 3,543,404"); one that doesn't know an upgrade due within 7 days
can't be public, and an open server pauses at activation minus one, so wallets get an error
instead of a frozen chain; a countdown shows from 14 days; untested versions run locally only.
`GetLightdInfo`'s `consensusBranchId` must equal the archive's `consensus.chaintip`. A pure
**stall rule** calls Zaino stuck when its tip hasn't moved for 2 minutes while the archive
advanced 4 blocks or more, when it sits at the block before an upgrade the archive has
activated, or when it logs `invalid consensus branch id`; the front door then says
"unavailable".

**The binary.** Recommendation: Zeal bundles `zeal-zainod`, built by its own CI from a pinned
tag. It revises MULTI-NODE.md's non-goal "Building or downloading binaries other than the
bundled zebrad", so it is the maintainer's call. Upstream publishes no binaries for any OS
(GitHub releases carry no assets, the latest is 0.10.1, 2026-09-29; 0.10.2 is a tag, a
crates.io release and an amd64 Docker image; the planned rewrite drops crates.io). A local
build tells every wallet its builder's login: `zaino-state/build.rs` sets `BUILD_USER` from
`whoami::username()` with no override, and `GetLightdInfo` returns it, where a CI build says
`runner` (Zeal asks upstream for a `ZAINO_BUILD_USER` override). And a non-expert can't build
Zaino.

- The build follows SIDECARS.md: `ZAINOD_VERSION` and `ZAINOD_COMMIT` pins checked by
  `scripts/check-versions.sh`; a `zainod-sidecars.yml` with Zaino's pinned toolchain (1.96.0),
  `cargo build --locked --release -p zainod --features prometheus`, `SOURCE_DATE_EPOCH` from the
  tag, `ZAINO_GIT_COMMIT_ID` and `ZAINO_GIT_BRANCH` set, paths remapped, stripped and attested;
  a smoke test of `--version` and `generate-config`; never the `no_tls_use_unencrypted_traffic`
  or `allow_unencrypted_public_json_rpc_bind` features. Licence Apache-2.0: Zaino's `LICENSE` is
  the short Apache notice and there is no `NOTICE` file, so the bundle carries the notice and
  the licence text.
- Linux x86-64 and aarch64 first; macOS once the CI build and the contract test pass (Zaino's
  CI is Linux-only; a macOS fix merged in #1267; unconfirmed); Windows later: Zaino handles only
  Ctrl-C there, and LMDB's preallocation there is unconfirmed.
- Per-platform sidecars go in Tauri's platform config files, which merge by JSON Merge Patch
  (RFC 7396) and replace arrays, so `tauri.linux.conf.json` (which exists) lists every Linux
  `externalBin`, zebrad's included.
- A zainod the user picks (`File(path)`) still works locally. It can be public only when its
  version is on Zeal's tested list and the user has accepted the `buildUser` wallets see.

**What backs it.** An archive on the same network and computer: any zebrad 7, or a Zakura
archive (reported working over RPC with Zakura 1.6.0 by zeckyapp/rock-it; unconfirmed for
1.8.0). Never a pruned node: restores (`GetBlockRange`), old transactions (`GetTransaction`)
and 0.10.2's `GetSubtreeRoots`, which calls `getblock` at each subtree's end height, all read
old blocks, and without those roots a Zodl wallet never gets past startup.
`NodeKind::Zaino.uses()` becomes `[Zebrad, Zakura]`, and HERD.md's `dependency_ok` refuses a
pruned one. A pure `pick_backing(network, herd)`, run again when the herd changes, prefers an
archive that isn't Zallet's backend, because Zaino without its own index turns every wallet
read into an RPC call to its archive; then the explorer's source (HERD.md's
`explorer_source`, once HERD H6 has landed), the bundled zebrad, the one nearest the tip.
With only the wallet's archive, Zaino uses it and says so ("Wallets that use your server will
share Zebra · Mainnet with your own wallet. It stays correct, but may sync more slowly when
many people connect").
With none: "Zaino needs a node that keeps the whole chain" and
[Add a Zebra node (about 400 GB)]. The archive keeps Zeal's managed RPC: loopback
`rpc.listen_addr`, `enable_cookie_auth = true`, a known `cookie_dir`.

**How Zeal runs it.**

- `zeal-zainod start -c <app data>/nodes/<id>/zainod.toml`, in the node's folder, with every
  `ZAINO_*` variable removed: the environment overrides the file (`load_config_with_env`), and
  the prefix also covers the rewrite's `ZAINO_CONFIG_*`.
- **Start** once the archive's RPC answers and `<cookie_dir>/.cookie` exists: Zaino refuses to
  load without it. **Stop**: SIGTERM, graceful since 0.10.1. On macOS, which has no PDEATHSIG,
  a crashed Zeal leaves zainod running until the next launch finds it by its pidfile; nothing
  outside can reach it, because the front door died with Zeal.
- **Restart with the archive.** zebrad writes a fresh cookie at every start
  (`zebra-rpc/src/server/cookie.rs` at v7.0.0), and Zaino reads it once, when it builds its RPC
  client (`auth_from_parts` in `zaino-rpc/src/probe.rs`, and the comment on `rpc_client` in
  `zaino-state/src/chain_index/validator_source.rs`). So when the archive's pid changes, Zeal
  stops Zaino, waits for the RPC and the cookie, and starts it again: a new adapter flag,
  `restart_with_dependency`, in MULTI-NODE.md's dependency rules. The front door stays up and
  says "unavailable" meanwhile. A cookie path wins over `validator_user` and
  `validator_password` (`auth_from_parts`), so Zeal writes only the path.
- **Health**: Zeal's gRPC client over h2c on loopback, `GetLatestBlock` every 10 s and
  `GetLightdInfo` every 10 min, plus the log's `Zaino status check`, `sync_to_height` and the
  NU7 stall line (`… invalid consensus branch id`).

Zaino prints `Error: <message>` and exits 1 for every error, so the adapter classifies by the
message:

| Zaino's last line | Class |
| --- | --- |
| `TLS required when connecting to external addresses.` | `ConfigError`, and a Zeal bug: Zeal never writes a public bind |
| `Zaino may only connect to Zebra with private IP addresses.` | `ConfigError` |
| `Validator cookie authentication is enabled, but cookie path … does not exist.` | `WaitingForDependency` (new): neither a crash nor `Failed`; "waiting for Zebra · Mainnet" |
| `gRPC bind failed: …` | `PortInUse` |
| `Configuration parsing failed` | `ConfigError`: "This Zaino reads a different configuration than Zeal writes" |
| LMDB `MapFull`, `Cannot allocate memory` | `ResourceLimit` (new, deterministic), with Zaino's own index only |
| a refusal to start on an unknown upgrade (after #1655; message unconfirmed) | `TooOld` (new, deterministic) |
| anything else | `Crash`, with today's backoff |

**The managed `zainod.toml`.** Zaino 0.10.2 refuses unknown keys only at the top level and in
`[storage.database]` (`deny_unknown_fields` on `ZainodConfig` and `DatabaseConfig`).
`[grpc_settings]`, `[validator_settings]`, `[service]` and `[storage]` ignore them, and
`[validator_settings]` also fills a missing key from its default, so a misspelt or renamed
`validator_cookie_path` leaves Zaino on user and password `xxxxxx` with no parse error. Zeal
writes exactly these keys (checked in `zainod/src/config.rs` and `zaino-common/src/config/` at
the tag), the pinned checks below cover each one, and the Zaino adapter rewrites the file
before every zainod start:

```toml
# Managed by Zeal: rewritten at every start.
backend = "rpc"                     # never "direct": its zebra-state 13 reader can't open zebrad 7's database
network = "Mainnet"                 # or "Testnet"; Zaino's default is Testnet, so it is always written
ephemeral_finalised_state = true    # no index of Zaino's own at first
metrics_endpoint = "127.0.0.1:9998" # assigned; a build without `prometheus` accepts the key and ignores it

[grpc_settings]
listen_address = "127.0.0.1:8137"   # assigned, loopback only; never a [grpc_settings.tls] table

[validator_settings]
validator_jsonrpc_listen_address = "127.0.0.1:8232"   # the archive's managed RPC port
validator_cookie_path = "<archive's cookie_dir>/.cookie"

[service]
timeout = 900                       # 0.10.2 caps a whole stream at 4 × timeout: 60 min
channel_size = 32

[storage.database]
path = "<app data>/nodes/<id>/data" # explicit: the XDG default suits neither macOS nor Windows
size = 64                           # GB of LMDB map; the 384 default fails on small machines
```

- **Left out**: `[json_server_settings]` (Zaino's JSON-RPC stays off), the validator user and
  password, `zebra_db_path`, and `donation_address` unless the user sets one in Share.
- **The timeout**: the default 30 gives streams 120 s (`zaino-state/src/indexer.rs`, lines 676
  and 859), which a Mainnet wallet's first sync runs into (#1537). 900 also lengthens unary
  calls, which front door v2's deadlines bound.
- **Pinned**: a test runs `generate-config` at the pinned version and fails if a key Zeal
  writes is missing from it, except the keys whose default is `None`, which the TOML serializer
  leaves out (`metrics_endpoint`, `validator_settings.validator_cookie_path`,
  `donation_address`); those are checked against the config structs at the tag, the top-level
  two are refused at start if unknown, and `real_zainod.rs` confirms that Zaino authenticated
  to the archive with the cookie. Whether ephemeral mode reads `[storage.database]` is
  unconfirmed.
- **The rewrite** (merged and reverted in #1678 and #1684, proposed again as #1688 to #1709; its
  changesets are unreleased) replaces the file: `network = "mainnet"`, one
  `[[trusted_validators]]` with `jsonrpc_address` and `cookie_path`, `[serve]
  grpc_listen_address`, `[metrics] listen_address`. It drops `ephemeral_finalised_state` and
  always keeps indexes of its own, so it needs a disk budget and a first sync, under HERD.md's
  heavy-job gate. Its `[grpc]` limits include `max_connections_per_ip` (32) and
  `max_streams_per_connection` (8): every front-door connection comes from 127.0.0.1, so Zeal
  sets the first above the front door's connection cap and the second at least as high as the
  front door's streams per connection. Zeal never uses its `trusted_proxies` (a PROXY-protocol
  header with each client's address). The adapter picks its writer by schema (`V0_10` or
  `V0_11`) from `--version`, with a fixture each.
- **No index of Zaino's own, at first.** With `ephemeral_finalised_state = true` Zaino is ready
  in about a minute and uses no disk, and every read goes to the archive (rock-it's findings):
  a restore from an old birthday becomes one `getblock` per block, which front door v2's
  per-method limits keep from starving the owner's wallet. "Build Zaino's own index" is a later
  opt-in on Linux or macOS and local disk only: LMDB runs with `MDB_NOSYNC`, so a hard kill or a
  network filesystem can corrupt it (#1299). It used 43 GB at height 1.77M on 0.10.1 (#1193;
  the full size is undocumented), takes two to three days on a Raspberry Pi 5, and its write
  budgets are scaled to memory (2, 4 or 8 GiB for 8, 16, or 32+ GB; proposal).

**What it needs from the front door.** Zaino's own TLS isn't used: it reads its certificate
once, at spawn (`get_valid_tls` in `zaino-serve/src/server/config.rs`, called from
`server/grpc.rs`), so every renewal would restart Zaino and drop every wallet; it can't answer
TLS-ALPN-01; and its server sets no limits (`Server::builder()` with no connection, stream,
timeout or keep-alive settings). It accepts a plaintext bind on any RFC 1918 address
(`is_private_listen_addr`, `zainod/src/config.rs`), so a router forward to `192.168.x.y:8137`
would serve plaintext to the internet: a non-loopback `grpc_settings.listen_address`, a
`[grpc_settings.tls]` table, `[json_server_settings]` and a non-loopback `metrics_endpoint` are
managed keys that Zeal refuses, as it refuses zebrad's non-loopback RPC today (ARCHITECTURE.md,
"Config protocol"). And because 0.10.2 pins h2 0.4.15 and serves its own extension on the same
port ([limits](#the-light-client-servers-limits)), Zaino faces the internet only through front
door v2 (PS2).

### lightwalletd

lightwalletd isn't planned. It can't read zebrad's cookie, so its README asks for
`enable_cookie_auth = false`, which Zeal refuses; and it publishes no binaries: its GitHub
releases, v0.6.1 (2026-10-27) and the four before it, carry no assets (GitHub API, read
2026-11-03). For the record, v0.6.1 has no NU7 changelog entry but doesn't check the branch id
it parses, so it likely follows NU7 (unconfirmed).

## The DNS seeder in public

### What changes from the census

HERD.md adds zeeder as `NodeKind::Zeeder` in **census** mode: loopback, a placeholder
`.invalid` zone, short runs that count the network. **Public** mode, behind the seeder's
switch, runs all the time, answers for a zone the user owns that is delegated to this
computer, listens on the LAN address the router forwards WAN port 53 to, and is watched for
abuse.

zeeder v1.8.0 (2026-10-22, MIT OR Apache-2.0) runs the Zcash Foundation's seeders. It answers
SOA and NS at the zone apex, up to 25 shuffled A and 25 AAAA records of servable peers, NODATA
with the SOA for everything else, REFUSED outside its zones, and never recurses. A peer is
servable when the crawler handshaked it in the last 119 s, it is at or above the protocol
floor, advertises `NODE_NETWORK`, listens on the default port and was reached outbound
(`src/crawl/servability.rs`).

### The binary

- **Recommendation: bundle `zeal-zeeder` on Linux** (x86-64 and aarch64, about 4 MB), pinned by
  `ZEEDER_VERSION` and `ZEEDER_COMMIT`. This answers HERD.md's open question 4 the way "pull
  in zeeder" reads. `scripts/fetch-zeeder-linux.sh` applies `fetch-zebrad-linux.sh`'s checks:
  the cosign signature on `SHA256SUMS` from zeeder's `release.yml` at the pinned commit, the
  tarball's line in it, the provenance attestation, and `--version`. It needs glibc 2.34+.
- **macOS** once Zeal's CI builds it and a smoke test passes (zeeder has no macOS CI:
  unconfirmed). Public mode there also waits for zeeder to stop when its stdin closes: macOS
  has no PDEATHSIG, and a crashed Zeal must not leave a public DNS server behind. **Windows**
  later: off Unix zeeder stops only on Ctrl-C (`src/seeder.rs`).
- **A process, never linked**: zeeder is a binary crate only.
- **Version**: the pin moves with `ZEBRAD_VERSION` before every network upgrade ("after NU7,
  upgraded Mainnet nodes refuse handshakes from a seeder that advertises an older protocol
  version", v1.8.0's notes). Zeal stops a public seeder before an upgrade its zeeder doesn't
  know, and never runs `attest-activation` by itself.

### The crawler's port

zeeder's crawler binds `[::]:8233` for a Mainnet zone (`[::]:18233` for Testnet) with no
setting, and zebra-network panics when it can't (`peer_set/initialize.rs` at v7.0.0), so
whichever of zebrad and zeeder starts second dies.

- **Upstream ask, preferred**: a crawler `listen_addr` that takes loopback or port 0 and is
  never advertised, and a `cache_dir`. The crawler's inbound side answers nothing yet
  advertises `NODE_NETWORK` (#106), so not listening is better for a seeder.
- **Until then, HERD.md's 8233 dialog** moves the node on 8233 to an assigned port, with the
  evidence of its inbound peers ("DNS seeders list it on 8233; on 28233 they stop"). Public mode
  adds one line: a router forward of WAN 8233 to this computer must move to the archive's new
  port (with zebrad's `external_addr` advertising 8233; how peers learn the port behind that
  translation is unconfirmed) or go. WAN 8233 never goes to zeeder.

### Configuration

Public mode's managed `zeeder.toml` (its sections reject unknown keys):

```toml
# Managed by Zeal.
[dns]
listen_addr = "192.168.1.20:1053"   # the LAN address the router forwards WAN 53 to; 127.0.0.1 while warming up

[zones.mainnet]
domain = "mainnet.seed.example.com"
nameserver = "ns-seed.example.com"  # outside the zone: zeeder refuses in-zone names and serves no glue
ttl = 600                           # zcashd's seeder policy asks for at least 60

[rate_limit]                        # on by default (SeederConfig::default); written so Zeal shows what applies
queries_per_second = 10
burst_size = 20

[metrics]
endpoint_addr = "127.0.0.1:19999"   # assigned, loopback; never 9999, zebrad's metrics port

[health]
endpoint_addr = "127.0.0.1:18080"   # assigned, loopback; never 8080
ready_threshold = 8                 # servable peers per zone before Zeal publishes anything (proposal)
```

- **The LAN address, not `0.0.0.0`**: the forward targets it and no other interface needs the
  seeder. Zeal re-checks it at every start and pauses when it changes.
- **The adapter refuses** non-loopback metrics or health addresses, a file without
  `[rate_limit]`, a nameserver inside the zone, and port 5353 (mDNS). Ports come from the
  allocator; the crawler keeps HERD.md's fixed claim.
- **Process** (HERD.md, "How Zeal runs it"): `zeal-zeeder -c <node>/zeeder.toml start` in an
  empty `<node>/run` (zeeder reads a `.env` from its working directory, and a malformed one
  aborts it); `ZEEDER__*` removed; `NO_COLOR=1`; an explicit `RUST_LOG`; on Linux
  `XDG_CACHE_HOME=<node>/cache`; `print-config` before every start; SIGTERM to stop.
- **The log is redacted.** zeeder logs `Rate limit exceeded for {client_ip}` at WARN for each
  dropped query, and answers every query inside a `dns_query` span carrying `client_addr`,
  which its default `fmt` layer prints on every line logged meanwhile
  (`src/dns/request_handler.rs`, `src/commands.rs`). So Zeal sets
  `RUST_LOG=info,hickory_server=warn,zeeder::dns::request_handler=error`: hickory-server,
  zeeder's DNS server, logs every answered query at INFO with the client's address and port
  (`ReportingResponseHandler::send_response`, hickory-server 0.26.1). The adapter's log reader
  also replaces every IPv4 or IPv6 address on a zeeder line with `<address>` before the ring or
  the log file, counting drops instead.
- **One public zeeder per computer**: WAN 53 forwards to one port, and one process serves every
  zone. Mainnet first; a Testnet zone joins the same process later, and the Testnet tab's card
  says so. While it runs, HERD.md's census reads its metrics instead of running its own, and
  Reset peers moves the peer file aside instead of offering a peer slice, since the crawler
  holds those remote peers' one slot per address all the time.

### Delegation

zeeder's nameserver must be a name outside its zone whose address the parent zone serves ("All
configured authoritative nameservers must be outside their zone because Zeeder does not serve
glue or address records for nameserver hostnames", ADR 0002). So the seeder needs **a domain
the user owns**, at a DNS host that accepts NS records on a subdomain:

```text
mainnet.seed.example.com.   3600  NS    ns-seed.example.com.  ; the delegation, published last
ns-seed.example.com.         300  A     203.0.113.10          ; the home address, kept current by Zeal
; or, when Zeal has no API for the parent zone:
mainnet.seed.example.com.   3600  NS    myhome.duckdns.org.   ; a dynamic-DNS name as the NS target
```

- **Free dynamic-DNS names can't be delegated**: DuckDNS documents no NS records, and deSEC
  forum answers say NS delegation under `dedyn.io` "is no longer allowed" (authors not
  identified as staff: unconfirmed). Such a name can be the NS target if it is an address
  record, not a CNAME (RFC 2181 §10.3).
- Delegate the seed name itself; no DS record (zeeder doesn't sign, and a signed parent proves
  the delegation insecure); one nameserver is accepted (DNSViz warns; `seeder.zec.rocks`, in
  Zebra's default list, has one); A records only for the NS target until IPv6 (PS6).
- **A dynamic home address is allowed**, unlike HERD.md's "a static public IP (asked, never
  inferred)", because most home lines change address. Zeal keeps the NS target's A record
  current at a TTL of 300 s or less; after a change resolvers may use the old address that
  long, and zebrad resolves all its seeders at once and carries on with the others.
- **The order avoids cached empty answers**: resolvers cache NODATA for the SOA minimum (the
  zone TTL, 600 s), so Zeal warms zeeder up on loopback with the real zone, opens it on the
  LAN address, waits for `/ready`, and only then publishes the delegation.

### What other nodes do

- **Only nodes that list the name use it.** Operators append it to Zebra's defaults, because
  setting the key replaces them (serde fills only missing keys):

  ```toml
  [network]
  initial_mainnet_peers = [
    "dnsseed.str4d.xyz:8233", "dnsseed.z.cash:8233", "mainnet.seeder.shieldedinfra.net:8233",
    "mainnet.seeder.zfnd.org:8233", "seeder.zec.rocks:8233",
    "mainnet.seed.example.com:8233",
  ]
  ```

  zebrad resolves every entry through the system resolver at once (`tokio::net::lookup_host`,
  `zebra-network/src/config.rs`), retries only when all failed, and adds the answers to its
  peer cache. Whether Zakura reads the same key is unconfirmed. Light wallets don't use
  seeders.
- **Not the user's own nodes**: the answers are roughly the peers the crawler holds now (#43),
  and each gives this address one slot, so the herd's nodes can't use them (HERD.md, "One
  connection per address"). There is no "Add to my nodes".
- **Not Zebra's default list**: a hard-coded array whose past additions (Zebra #10228, #11096)
  followed weeks in production and a pledge to keep running. The only written policy is
  zcashd's "Expectations for DNS Seed operators" (`doc/book/src/dev/dnsseed-policy.md`).

### What it costs, and the operator's duties

- **Peers**: up to 75 outbound crawler connections per network, each taking a remote node's one
  slot from the herd (inferred). The panel shows the herd's outbound counts before and after.
- **Private addresses**: zeeder's routable check excludes only loopback, unspecified and
  multicast, so it would hand out an RFC 1918, CGNAT or ULA peer it handshaked on the default
  port. Herd nodes are never pointed at each other by LAN address on 8233 (HERD.md), and Zeal
  asks upstream to drop non-global addresses.
- **Resources**: the Foundation's fleet uses 100 to 200 MB of memory, under 5 % of a core and
  2 to 20 Mbps (`docs/operations.md`); a seeder outside the default list should see far less
  (unconfirmed).
- **Duties**, in the seeder's consent. From zcashd's policy: keep the host, the domain and the
  DNS account secure and in your control (a lapsed or hijacked name lets someone else choose
  new nodes' peers), and never sell or transfer the seed; serve no TTL under 60 s; log only what
  the service needs, keep it no longer and share it with no one; publish an email contact; and
  if you can't, stop and tell the Zcash developers. From Zeal: update before every network
  upgrade, and remove the NS record, and say so, when stopping for good.

## Exposing a service

### Rules the flow keeps

Tests assert each one:

1. **Off by default.** No router query, DNS lookup, dynamic-DNS update or ACME request before
   the user starts a setup or clicks "Check my connection", extending ARCHITECTURE.md's rule
   that the updater's Off makes zero network requests.
2. **Less exposure is one click; more is the guided steps**, and opening re-runs every gate in
   the backend at that moment.
3. **Nothing reaches the light client backend before "Open to the internet"** (zebrad's
   endpoint, or Zaino from PS5): until then the front door answers only Let's Encrypt's
   challenge and this computer. zeeder answers the internet only after its own Open.
4. **No public listener outlives Zeal.** The front door runs inside Zeal's process, so it
   closes when Zeal exits or crashes, on every system. zebrad's endpoint listens on loopback
   only, and on Linux PDEATHSIG ends zebrad with Zeal. zeeder has no stop on stdin close, so
   its public mode starts on Linux, where PDEATHSIG ends it. Router mappings are deleted on
   pause and quit, and recorded so a crash's leftovers go at the next start.
5. **"Reachable from the internet" needs evidence**, shown with its source and age.
6. **Everything can be undone**, and turning off says what stays public.

Every outbound request public services make:

| Request | To | When |
| --- | --- | --- |
| NAT-PMP external address; SSDP, the UPnP device description, `GetExternalIPAddress` | the router | "Check my connection", setup, every 5 min while on |
| a mapping, its renewals, its deletion | the router | only after the user's click |
| A, AAAA, NS, SOA, CAA, TXT lookups | the zone's authoritative servers, the system resolver | setup, every 5 min while on |
| an address update or a TXT record | the user's DNS provider | on an address change; DNS-01 |
| ACME orders, ARI checks | Let's Encrypt staging and production | issuance, about daily, "Test now" |
| one SOA query for a random name in the seed zone | the public resolver the user picked | seeder Open, "Test now", every 6 h |
| port-check and address-echo websites | — | never built in |

### The steps

One setup for both services. Each step lists its checks, a failed check shows its fix first,
and Next stays disabled until the gates pass.

1. **What becomes public.** The consent screen ([UI](#the-consent-screen)) with the real name,
   address and versions. Gate: an explicit tick, stored as `Consent { version, at, hostname }`
   and asked again when `CONSENT_VERSION` or the name changes. For the light client server it
   also accepts Let's Encrypt's Subscriber Agreement (ACME's `termsOfServiceAgreed`).
2. **The service.** Light client server: Zeal takes the network's zebrad (in PS1, the one it
   runs), assigns the endpoint's loopback port and restarts zebrad with it. Gates: the zebrad
   is on Zeal's tested list (in PS1, the bundled 7.0.0); zebrad logged
   `Opened lightwalletd gRPC endpoint at` with the assigned address; `GetLightdInfo` answers on
   loopback with zebrad's chain and branch id. From PS5 the step adds and starts the Zaino node
   instead, with Zaino's gates ([Later: Zaino](#later-zaino-as-the-backend)). The setup goes on
   while zebrad syncs ("Zebra · Mainnet is 82 % synced. Wallets can use your server once it
   reaches the tip, in about 9 hours"). Seeder: Zeal reuses the census node or adds one. Gates:
   `zeal-zeeder` exists here; it knows every upgrade the bundled zebrad schedules; the crawler
   port is free (or the 8233 dialog moved its holder); the DNS port is free.
3. **This network.**
   - The LAN address and gateway come from the default route (`netdev`). The user reserves the
     address in the router's DHCP settings ("a forward targets an address"); Zeal records it
     and re-checks it at every start and every 5 minutes.
   - A VPN or exit node on the default route (`tun`, `utun`, `wg`, `ppp`, Tailscale: a
     heuristic) stops the setup: "Turn it off, or set it to leave Zeal out."
   - The router's WAN address, read-only: NAT-PMP's external-address request (opcode 0,
     `crab_nat`) and UPnP IGD's `GetExternalIPAddress` (`igd-next`). PCP isn't used here: it
     reports the address only for a MAP or PEER request, which creates a mapping. Public:
     continue. In 100.64.0.0/10: carrier-grade NAT, no inbound IPv4. RFC 1918: a second
     router. No answer: unknown, and the outside checks decide.
   - The UPnP device description (`manufacturer`, `modelName`) picks the vendor guide.
   - **macOS 15 and later** ask before an app sends to a LAN address (NAT-PMP, SSDP, the UPnP
     description), not before it accepts connections. The prompt names Zeal even for a helper
     process, and needs `NSLocalNetworkUsageDescription` in Zeal's `Info.plist` (Apple TN3179).
     The first attempt may be refused before the user answers, so Zeal explains first and
     retries; a refusal reads "unknown", with why. How Tauri 2 merges a `src-tauri/Info.plist`
     is to be checked.
4. **The name** (light client server; [choices](#names-and-dynamic-dns)). Gates: the zone's
   authoritative servers and the system resolver return the WAN address; TTL 300 s or less;
   CAA, walked up the name (RFC 8659), absent or allowing `letsencrypt.org`. Warnings: the
   address the provider saw differs from the router's (CGNAT or a second router); an AAAA
   record Zeal didn't verify ("Let's Encrypt tries IPv6 first. [Remove it]").

   **The domain** (seeder; [records](#delegation)): the zone and the NS target, whose A record
   Zeal writes or checks now; the delegation waits for step 9. Gates: the NS target resolves
   to the WAN address at a TTL of 300 s or less, is no CNAME, lies outside the zone; the zone
   has no DS record and isn't delegated elsewhere.
5. **The port.** Zeal assigns 8443 for the front door (`PortRole::PublicTls`) or 1053 for the
   seeder (`PortRole::Dns`); the user forwards TCP 443, or UDP and TCP 53, to it by hand
   ([guides](#ports-the-router-and-nat)). The light client server will also offer "Ask my
   router to do it" and, when 443 is impossible, 9067 with a DNS-01 certificate (both PS6).
   Gates: the port is free; an automatic mapping came back on the exact port. The front door
   starts here, in `ChallengeOnly`.
6. **The firewall** ([per system](#firewalls)). No local gate: the outside checks prove it. A
   firewall that blocks everything is a warning with its fix.
7. **The certificate** (light client server; [details](#certificates)): staging, then
   production. Gate: a production chain that verifies for the name with
   `rustls-platform-verifier`, the trust a phone uses. Errors map to fixes: `connection` ("Let's
   Encrypt couldn't reach this computer: the forward, the firewall, or your provider blocking
   443"), `dns` or `unauthorized` ("the name points elsewhere, or hasn't updated yet"), `caa`,
   `rateLimited` ("Zeal tries again after <time>", from the Retry-After).

   **The warm-up** (seeder): zeeder on `127.0.0.1` with the real zone; the step shows servable
   peers growing. Gates: `/ready`; a local SOA names the NS target; NS and A answer.
8. **Test on this computer** (light client server). The front door goes to `LocalOnly`; Zeal
   connects to `127.0.0.1:8443` with SNI set to the name, verifies the chain, and calls
   `GetLightdInfo` and `GetLatestBlock` through it. Gates: Zodl's checks (chain name; Sapling
   activation 419,200 on Mainnet, 280,000 on Testnet; the branch id for that height); a height
   within 2 of zebrad's. The step shows what wallets will be told: vendor, version, Zebra's
   build and subversion (with Zaino, also its build user and any donation address).
9. **Open to the internet**, the only button that makes a service public. The backend re-runs
   every gate and refuses with the failing one (`OpenError { check }`). Light client server:
   zebrad is at the tip for 10 minutes (with Zaino, Zaino within 2 blocks of its archive too);
   the front door goes to `Open`; then the **hairpin test**, a TLS connection from this
   computer to the WAN address, which can't run earlier because a hairpinned connection arrives
   from a LAN address. Success proves the forward; failure proves nothing (no NAT loopback),
   and Zeal says a phone at home then needs mobile data. Seeder: zeeder restarts on the LAN
   address, and after `/ready` Zeal writes the delegation (or shows it); the parent's servers
   must return it, the [outside probe](#the-self-test-evidence-from-outside) must pass, and an
   A query must return addresses. TCP 53 stays "not checked".
10. **Keep it running.** Keep this computer awake while serving, on by default: a
    `systemd-inhibit` child on Linux, `caffeinate -s -w <pid>` on macOS,
    `SetThreadExecutionState` on Windows (windows-sys is already a dependency), no new crate;
    it can't stop a closed lid. "Start Zeal when I sign in" if the maintainer adds
    tauri-plugin-autostart, with "Zeal serves while you're signed in". Laptops get "A laptop
    makes a poor server: it sleeps, changes networks and runs on battery". Limits: an upload
    preset (Light 5, Standard 10, Unlimited Mbit/s) and a monthly cap (proposal: 300 GB, 50 GB
    for the seeder).
11. **Share.** Light client server: Zodl `zec.example.com:443` (Custom, Manual mode), Zingo and
    Zkool `https://zec.example.com:443`, with the real external port, Copy buttons and a QR
    code; with Zaino, optionally a donation address, a validated unified address every wallet
    sees, with the warning that it links the server to that wallet (zebrad's endpoint has no
    field for one). Seeder: the
    `initial_mainnet_peers` line for nodes elsewhere, the duties, "not for Zebra's default
    list".

### Names and dynamic DNS

The light client server's name is one of:

1. **A free dynamic-DNS name**, `name.duckdns.org` or `name.dedyn.io` (deSEC): Zeal opens the
   sign-up page (a URL compiled in), the user pastes a token, Zeal sets and keeps the record.
2. **A domain the user owns** at Cloudflare (a token scoped to one zone's DNS, and the record
   left DNS-only, or Cloudflare would end TLS and see all traffic) or deSEC. Zeal suggests
   `zec.<domain>`.
3. **A record added by hand**, anywhere, checked every 30 s while the setup waits for it and
   every 5 minutes after, like the others: for a static address, or a router that runs dynamic
   DNS itself. A mismatch for 15 minutes pauses the service.

Zeal suggests a neutral name, and WHOIS privacy for an owned domain. The seeder's zone needs
choice 2 or 3; a choice-1 name can be its NS target.

| Provider | Address update | TXT for DNS-01 | Notes |
| --- | --- | --- | --- |
| DuckDNS | `GET https://www.duckdns.org/update?domains=<n>&token=<t>&ip=<v4>&verbose=true` | `&txt=`, `&clear=true`; it answers for every name under it | `OK`/`KO`; `verbose` echoes the address; one token covers the whole account |
| deSEC | `GET https://update.dedyn.io/?hostname=<h>&myipv4=<a>&myipv6=preserve`, `Authorization: Token` | REST rrsets | always pass the address; 60 s TTL; rate-limited |
| Cloudflare | `PATCH /client/v4/zones/{zone}/dns_records/{id}` | the same API | unproxied |
| Manual | — | — | checked, never written |

No-IP isn't offered: free names need a manual confirmation every 30 days.

- **The public address** comes from the router's NAT-PMP or UPnP answer, else from what the
  provider saw (DuckDNS's `verbose`), never from an address-echo site.
- **`DdnsActor`**, one per name, compares every 5 minutes, updates only on change, then expects
  the new value at the authoritative servers within 2 minutes; it backs off after errors and
  stops on "too many updates". A mismatch for 15 minutes pauses the service. After a change:
  "Address changed at 14:02. DNS updated; resolvers may use the old one for up to 5 minutes."
- **Tokens** go to the OS keychain (`keyring` 4.2.0, service `org.zfnd.zeal`, account
  `dns:<provider>:<zone>`); `launcher.json` holds a `SecretRef`, and the UI only learns
  `has_token`. Without a Secret Service on Linux, a 0600 file, as an explicit choice. No request
  URL is formatted into an error, a log line or the status. `DdnsActor` reports the provider and
  the kind of failure (with reqwest, through `Error::without_url()`). A test makes a DuckDNS
  update fail and finds no token in the ring, `launcher.<date>.log`, the status or Copy
  diagnostics.

### Certificates

- **ACME from PS1, not certificate files the user brings.** Wallets accept only a publicly
  trusted certificate, so the alternative is the user running an ACME client of their own
  (certbot needs port 80 for HTTP-01, or a DNS plugin) and renewing it every 90 days, fewer as
  lifetimes shrink: too much to ask for a service that "can easily be enabled". TLS-ALPN-01
  needs only the 443 forward the service needs anyway, and its validation is the setup's first
  evidence from outside ([the self-test](#the-self-test-evidence-from-outside)), which files
  can't give. It is PS1's largest item: instant-acme, rcgen and x509-parser with their trees,
  `h2` among them ([dependencies](#files-dependencies-locks)), still to pass `cargo deny`; the
  account, order and renewal logic in `AcmeActor`; the front door reading the ClientHello to
  answer `acme-tls/1`; Pebble in CI. If PS1 runs short, ARI (`renewal_info`, `replaces`) can
  follow, with renewal at two thirds of the lifetime meanwhile, and so can "Test now" as a whole
  (without the deactivation, a staging order within the reuse window validates nothing) and the
  attempts bookkeeping beyond the Retry-After stop; issuing, verifying, rotation without a
  restart and renewal can't. Files a user already has may come later as an option, verified the
  same way and watched for expiry.
- **Let's Encrypt, through `instant-acme` 0.8.5** (Apache-2.0), `default-features = false` with
  `ring`, `hyper-rustls`, `rcgen`, `x509-parser` and `time`. ring keeps one crypto library:
  `Cargo.lock` already has ring 0.17.14, rustls 0.23.46, hyper-rustls 0.27.10 and
  rustls-platform-verifier 0.7.1, and no aws-lc. 0.8.5 has `NewOrder::profile`,
  `NewOrder::replaces` for ARI, `Account::renewal_info` and authorization `deactivate`
  (`src/types.rs`, `account.rs`, `order.rs`); it refuses `replaces` when the directory lists no
  `renewalInfo`, so Zeal checks first. Not `rustls-acme`: 0.15.4 has no ARI (it renews at two
  thirds of the lifetime, `src/state.rs`, read 2026-11-03), and its stream orders and renews on
  its own schedule, where `AcmeActor` keeps its own: staging first, the attempts budget, the
  challenge hit as evidence.
- **Accounts**: one per environment and installation, ECDSA P-256, no contact email (Let's
  Encrypt ended expiry emails on 2025-06-04; an address now joins ISRG's mailing list).
- **TLS-ALPN-01 is the default**: it needs external 443, holds no DNS credential, and proves
  the path from Let's Encrypt's several vantage points. Zeal builds the challenge certificate
  with `rcgen` (RFC 8737) and sends it to the front door, which answers only ALPN `acme-tls/1`
  with that SNI. **DNS-01** (PS6) is for when 443 can't be used, and for renewals that fall
  while paused, with a provider token; it proves nothing about reachability. No HTTP-01 (port
  80 too, the port providers block most), no IP-address certificates (160 hours, broken by an
  address change, the address in public logs).
- **Issuing**: staging first for every new name and every diagnosis (its TLS-ALPN-01 is the
  first proof that 443 reaches here), then production, `classic` profile, a fresh P-256 key;
  the chain is verified with the platform verifier before it is pushed to the front door.
- **Renewal**: at a random time in ARI's window ("exempt from all rate limits"), else at two
  thirds of the lifetime; never an order at startup while a certificate is valid. While paused:
  DNS-01 with a token (PS6), otherwise at resume; Zeal never opens the port to renew. Let's
  Encrypt is shortening lifetimes ("Decreasing Certificate Lifetimes to 45 Days", 2025-12-02):
  `classic` gives 90 days now, 64 from 2027-02-10 and 45 from 2028-02-16, and `tlsserver` 45
  since 2026-05-13.
- **Authorizations are reused** for 30 days now, 10 from 2027-02-10 and 7 hours from 2028-02-16
  (the same post). Renewals fall after that window, so every TLS-ALPN-01 renewal is a fresh
  outside check; "Test now" first deactivates the last valid staging authorization.
- **Rate limits** (updated 2026-08-05): 50 certificates per registered domain and 5 per exact
  name set per 7 days, 5 failed authorizations per name per hour, long pauses after repeated
  failures. Dynamic-DNS suffixes are on the Public Suffix List, so each user's name is its own
  registered domain. Zeal's `attempts.json` stays under: 3 production orders per name set per
  7 days, 2 failures an hour (proposal), backoff of 1, 4, 12 and 24 h, and a stop with the
  Retry-After on `rateLimited`.
- **Rotation without restarts**: the front door's resolver holds a
  `watch::Receiver<Arc<CertifiedKey>>` and returns `Some(rx.borrow().clone())`; a new
  certificate is `send_replace`d, new handshakes get it, open connections carry on. Challenges
  travel on a `watch<Option<Arc<Challenge>>>`.
- **The front door's TLS**: rustls defaults (TLS 1.2 and 1.3), ALPN `h2` for the service and
  `acme-tls/1` only for a pending challenge, a 10 s handshake, no client certificates. SNI must
  equal the name; anything else, a missing SNI too, gets `unrecognized_name`, so a scanner on
  the bare address gets no certificate (DNS and the logs still link them).
- **Files**: accounts in `<app data>/public/acme/`, and per name
  `<app data>/public/certs/<name>/` (`cert.pem`, `key.pem`, `ari.json`, `attempts.json`), so
  running the setup again, or moving the service to Zaino, orders nothing. Keys are 0600 on
  Unix and under the per-user AppData ACL on Windows, a key others can read is refused, and
  every write is atomic.
- **Expiry**: the panel shows the days left and the next window; a banner 14 days before expiry
  if renewal fails; `ChallengeOnly` at expiry; a daily check of the served chain.

### Ports, the router and NAT

| What | Router | This computer | Protocol | Listener |
| --- | --- | --- | --- | --- |
| light client server, TLS-ALPN-01 | 443 (9067 with DNS-01, PS6) | 8443 (`PublicTls`) | TCP | the front door, in Zeal, on `[::]`, IPv6-only off through `socket2` (Windows defaults to on) |
| zebrad's light-wallet endpoint | never | loopback `127.0.0.1:9067`, Testnet `:19067` (`Grpc`), only while the service is on | TCP | zebrad |
| Zaino gRPC, metrics (PS5) | never | `127.0.0.1:8137`, `:9998` | TCP | zainod |
| DNS seeder | 53 | 1053 (`Dns`) | UDP, TCP | zeeder on the LAN address |
| zeeder metrics, health | never | assigned loopback ports | TCP | zeeder |
| zeeder crawler | never (#106) | 8233 or 18233, HERD.md's fixed claim | TCP | zeeder |

Ports come from MULTI-NODE.md's allocator once it lands (in PS1, defaults with a free-port
check) and are checked at every start, zebrad's endpoint port when Zeal writes the key. The
router's translation (443 to 8443, 53 to 1053) means nothing needs elevated rights.

**By hand is the main path**: reserve the LAN address, then add the forward with the vendor
guide, chosen from the UPnP model or by the user and filled in ("TCP, external 443, internal
192.168.1.20, port 8443"); [I've done it: check] runs the staging validation or the seeder's
checks. Menu paths from the vendors' pages:

| Router | Where |
| --- | --- |
| TP-Link | Advanced > NAT Forwarding > Virtual Servers |
| NETGEAR | ADVANCED > Advanced Setup > Port Forwarding/Port Triggering |
| ASUS | WAN > Virtual Server/Port Forwarding (no IPv6 forwarding) |
| FRITZ!Box | Internet > Permit Access > Port Sharing |
| eero | app: Settings > Advanced networking > Reservations & port forwarding |
| Google Nest Wifi | Google Home: Wi-Fi > Advanced networking > Port management (both sides equal) |
| Xfinity | app: WiFi > View WiFi equipment > Advanced settings > Port forwarding |

**Automatic mapping is an option** (light client server, PS6), never the default:

- PCP, then NAT-PMP (`crab_nat` 0.8, MIT), then UPnP IGD (`igd-next` 0.18 with `aio_tokio`,
  MIT); not `portmapper`, whose UPnP path silently takes any port and labels mappings
  "iroh-portmap".
- Only after a consent click; the exact external port or nothing; labelled "Zeal light client
  server"; a 2-hour lease (PCP 1 h) renewed at half; remapped when the LAN address changes;
  deleted on pause, off and quit, and recorded in `<app data>/public/state.json`. Permanent
  leases only after asking ("If Zeal crashes, the port stays open until it starts again"); 724
  `SamePortValuesRequired`, `ActionNotAuthorized` and conflicts fall back to the guide.
- Expect a refusal for 443: miniupnpd's shipped config advises "only allow redirection of
  ports >= 1024", ends with `deny 0-65535 0.0.0.0/0 0-65535`, and applies to NAT-PMP and PCP
  too. The seeder's 53 is below 1024 as well, so it has no automatic mapping.
- Zeal never asks anyone to turn UPnP on: it has no authentication, CERT/CC recommends turning
  it off on internet-facing interfaces (VU#339275), and WAN-exposed UPnP has been abused at
  scale (Akamai, UPnProxy).

**Routers that require equal ports** can't translate. The light client server then uses 9067
with DNS-01, and the front door listens on 9067 itself: zebrad's endpoint, which took 9067 at
step 2 on Mainnet, moves to the next free loopback port, and zebrad restarts once more before
the front door starts. The seeder then needs port 53 on this computer. On Linux: an nftables
redirect of UDP and TCP 53 on the LAN address to 1053 (shown, never run), so zeeder keeps
binding 1053. Never `setcap` on `zeal-zeeder`: the kernel clears PR_SET_PDEATHSIG when it
executes a binary with file capabilities (prctl(2)), so a crashed Zeal would leave zeeder
answering on 53, and any account here could run that binary on a low port; if it is ever
offered, then only once zeeder stops when its stdin closes (PS0's upstream ask). On macOS, the
seeder binds 53 itself through `0.0.0.0:53`, since unprivileged low ports work on the wildcard
address only (Apple DTS).

**When no forward helps** (step 3 finds these): carrier-grade NAT leaves no inbound IPv4 (many
providers give a public IPv4 on request; otherwise IPv6, PS6, and a DS-Lite line's IPv6 reaches
only IPv6 clients); a second router needs the forward on both, or bridge mode (AVM: an inner
FRITZ!Box's forwards "will have no effect"); a VPN on the default route sends replies the wrong
way.

### Firewalls

Zeal shows each step before the system asks, and never elevates without a click.

- **Linux**: Ubuntu ships ufw disabled and Fedora Workstation opens 1025 to 65535. Zeal shows
  the command for the tool on `PATH` and never runs it: `sudo ufw allow 8443/tcp`, or
  `sudo firewall-cmd --permanent --add-port=8443/tcp && sudo firewall-cmd --reload` (the seeder:
  1053, UDP and TCP).
- **macOS**: the application firewall allows signed software by default, and Zeal, which holds
  the front door, is signed and notarized; Zeal reads `socketfilterfw --getglobalstate` and
  `--getblockall`.
- **Windows**, once a service runs there: Zeal says to choose Private networks before Defender
  asks, and, since the per-user installer can't add rules, offers one UAC-elevated rule scoped
  to Zeal's executable, which holds the front door, and its port:
  `netsh advfirewall firewall add rule name="Zeal light client server" dir=in action=allow program="<dir>\zeal-app.exe" protocol=TCP localport=8443 profile=private`.
  Turning off offers to remove it.

### The self-test: evidence from outside

| Evidence | What produced it | For |
| --- | --- | --- |
| `TlsAlpn01Validation` | Let's Encrypt connected to the external port and completed the challenge | light client server |
| `OutsideTlsConnection` | a public address, not the router's and not in this computer's /64s, completed TLS with the right SNI; worded exactly, since scanners that read certificate logs do too | light client server |
| `UserPhoneTest` | with Wi-Fi off, a wallet or browser connected and the user confirmed with a click | light client server |
| `PublicResolverProbe { resolver }` | a random name in the zone, asked through a public resolver, came back with zeeder's SOA | seeder |

- **DNS-01 issuance is never evidence**: the type has no variant for it.
- **Ages**: under 24 hours "reachable", up to 7 days "last confirmed 3 days ago", then stale. A
  check that passed and now fails sets "lost", with why.
- **The phone test**: a QR code of `zec.example.com:443` and "turn Wi-Fi off first". When an
  outside connection with the right SNI arrives in the next few minutes, the step asks "Was
  that your phone? [Yes] [No, try again]"; only the click makes it a phone test.
- **The seeder probe** asks for `zeal-<random>.mainnet.seed.example.com` SOA through the
  system resolver and a public one the user picks (1.1.1.1 or 9.9.9.9). zeeder answers deeper
  names with NODATA and its SOA, so the random label reaches this computer: NOERROR with
  zeeder's SOA proves the delegation and UDP 53; SERVFAIL means delegated but unreachable;
  NXDOMAIN with the parent's SOA means no delegation.
- **"Test now"**, at most every 10 minutes: on 443 a staging TLS-ALPN-01 order, the last
  authorization deactivated first, never finalized; on 9067 the local, hairpin and phone
  tests; for the seeder, the probe. No port-check website: each would learn the address,
  the port and the time.

### While serving

| Watch | Every | When it fails | Comes back |
| --- | --- | --- | --- |
| zebrad's branch id by `GetLightdInfo`, every 10 min (with Zaino also: within 2 blocks of its archive and not stuck, by `GetLatestBlock` every 10 s) | 10 min; 10 s with Zaino | `Degraded`: the front door says "unavailable" | by itself, after 5 min caught up |
| zebrad (with Zaino, its archive) running and at the tip | on change | `Degraded` while it restarts or catches up; paused if stopped or halted at end of support | by itself |
| an upgrade the serving software doesn't know, at activation minus one | each block | paused (`BuildTooOld`): zeeder, and Zaino from PS5; on Mainnet zebrad halts at its end of support instead; on Testnet zebrad never halts (`zebrad/src/components/sync/end_of_support.rs:53-60`), so one that doesn't know the upgrade stays at the block before it, and the tip watch keeps the front door `Unavailable` until a Zeal update brings a newer zebrad | after a Zeal update |
| the name equals the WAN address, at the authoritative servers | 5 min | paused after 15 min | by itself |
| the LAN address unchanged | 5 min, at start | paused at once | when the forward or address is right again |
| no VPN on the default route | 5 min | paused | by itself |
| the certificate | hourly | a banner 14 days out if renewal fails; `ChallengeOnly` at expiry | after issuance |
| the monthly cap | each stats line | paused | next month, or a higher cap |
| seeder abuse ([limits](#the-seeders-limits)) | 10 s | paused an hour | by itself; after three in 24 h, by the user |
| the seeder's `/ready` | 10 s | a banner after 10 min (empty answers are cached 10 min, but zebrad asks other seeders too) | by itself |
| Zeal quits or crashes | — | everything public stops; mappings deleted (after a crash, at the next start) | at launch, if "resume when Zeal starts" is on and the gates pass |

Problems that clear themselves keep the front door up in `Unavailable`, so wallets get a clear
error and TLS-ALPN-01 renewals still work: a long stall of the backend, such as Zaino's at an
upgrade, must not let the certificate expire. Problems with the exposure itself (the LAN
address, the name, a VPN, the cap, the user, a too-old build) close it. **Pause**, the switch
off, stops the front door or zeeder and deletes the mappings, and removes zebrad's endpoint
with a restart of zebrad ([How Zeal runs it](#how-zeal-runs-it)); **Resume** re-runs what the
pause may have changed. **Turn off and forget** also offers to delete Zeal's DNS records, the
token and the firewall rule, asks whether to keep a Zaino or zeeder node for local use, and
says: "Your address stays in DNS history
services, and the name in public certificate logs." It also lists what Zeal didn't add and
can't remove. The router forward (TCP 443 → 192.168.1.20:8443, or UDP and TCP 53 → 1053), with
the vendor guide's page for it. On Linux, the command that reverses the firewall step:
`sudo ufw delete allow 8443/tcp`, or
`sudo firewall-cmd --permanent --remove-port=8443/tcp && sudo firewall-cmd --reload` (the
seeder: 1053, UDP and TCP). While they stay, a program that later listens on that port is
reachable from the internet.

## Abuse protection and privacy

### The light client server's limits

zebrad's endpoint sets a few limits of its own ([above](#what-zebrads-endpoint-serves)) and
none per client, Zaino 0.10.2 sets none, and behind the front door both see every client as
127.0.0.1, so the limits live in the front door: **v1** (PS1) forwards decrypted bytes and
limits connections, **v2** (PS2) ends HTTP/2 and limits streams and methods
([The front door](#the-front-door)). Values are proposals, tuned by the wallet runs.

| Limit | Default | How | In |
| --- | --- | --- | --- |
| connections, all clients | 128 | `Arc<Semaphore>`, the permit held by the connection | v1 |
| connections per IPv4 address or IPv6 /64 | 8 | `LimitsActor` (`::ffff:` mapped to IPv4) | v1 |
| new connections per address | 2 a second, burst 16 | token bucket in `LimitsActor` | v1 |
| TLS handshake; idle connection | 10 s; 10 min without a byte | timers | v1 |
| connection age | 2 h, then closed: zebrad's HTTP/2 ping every 30 s keeps a connection from going idle | a timer | v1 |
| upload | 10 Mbit/s (the preset) | a `Semaphore` of KiB permits refilled by a ticker | v1 |
| monthly upload | 300 GB, then paused | `AtomicU64`, totals persisted | v1 |
| streams per connection; header list | 16 (never above the backend's own: zebrad's 20); 16 KiB | hyper `http2` builder | v2 |
| request body | 4 MiB, `SendTransaction` 2.5 MiB | a counting body, then `RESOURCE_EXHAUSTED` | v2 |
| `GetBlockRange`, `GetBlockRangeNullifiers` in flight | 16 in all; a 10 s wait, then `UNAVAILABLE` | per-method `Semaphore` | v2 |
| `GetSubtreeRoots` in flight | 2 (a block per root in Zaino 0.10.2, #1665; a hash lookup per root on zebrad) | `Semaphore` | v2 |
| transparent-address methods in flight | 8 | `Semaphore` | v2 |
| `GetMempoolStream` per address | 2 | `LimitsActor` | v2 |
| `SendTransaction` per address | 1 a second, burst 5, 120 an hour | `LimitsActor` | v2 |
| deadlines; connection age | 60 s unary, 60 min streams; 24 h, then GOAWAY | timers | v2 |

lightwalletd v0.6.1 bounds the same things the same way (GHSA-xq7v-jj38-pcwr). v1 can't tell
calls apart, and for Zaino that rules it out: within the connection limits a client can still
repeat `GetSubtreeRoots` or long `GetBlockRange` calls, each many `getblock`s on the archive
behind the owner's wallet; every client frame reaches Zaino's own HTTP/2 stack, and 0.10.2 pins
h2 0.4.15, affected by RUSTSEC-2026-0258 (unbounded empty DATA frames; low; fixed in 0.4.16);
and everything on Zaino's port is reachable, its `zaino.index.v1.IndexedTipService` extension
and, in the rewrite, `/readyz` included. So Zaino serves only through v2. zebrad's endpoint
answers the first two and shrinks the third, so **PS1 opens Mainnet on v1**, labelled
experimental, once Zeal's wallet runs pass there:

- `GetSubtreeRoots` reads one block hash per root, not a block
  (`zebra-rpc/src/lightwalletd/methods.rs:497-501`), and every read stays inside zebrad, with no
  JSON-RPC hop. `GetBlockRange` still reads every block it sends, one after another, so one
  address can keep 8 connections of 20 streams each reading blocks. The connection limits, the
  upload budget and the monthly cap bound that, and when zebrad falls behind the front door
  goes `Unavailable` ([While serving](#while-serving));
- client frames reach zebrad's h2, which zebrad 7.0.0 pins at 0.4.16 (`Cargo.lock`), the release
  with the RUSTSEC-2026-0258 fix;
- besides `CompactTxStreamer`, the port serves only gRPC reflection, which describes the same
  public protocol;
- a client that keeps answering HTTP/2 pings but stops reading holds in zebrad's memory what
  zebrad prepared for it: up to 64 messages per task-fed stream, and the whole answer of
  `GetAddressUtxos`, `GetAddressUtxosStream` or `GetMempoolTx`, about 20 MB near the
  100,000-output cap (estimate). v1 can't tell these calls apart, so only its connection caps
  (8 per address, 128 in all) × zebrad's 20 streams and the 2 h connection age bound this; the
  upload budget and the monthly cap don't apply to a client that downloads nothing. The Mainnet
  wallet runs also measure zebrad's memory under such clients, and v1's per-address cap is set
  from that until v2's permits and deadlines apply (proposal).

v2 forwards only the `CompactTxStreamer` methods the backend serves (others get
`UNIMPLEMENTED`, zebrad's reflection service and Zaino's extension included), only the
`content-type`, `te`, `grpc-timeout`, `grpc-encoding` and `grpc-accept-encoding` headers (no
`user-agent`), and re-frames everything on one upstream h2c connection per client, so the
backend's h2 never sees a client frame. Its upstream client is h2 0.4.16 or newer, and the Zaino
rewrite's changeset says older Zaino streams small DATA frames that make such clients "that read
late" fail with `GOAWAY too_many_data_frames`; so v2 drains upstream frames at once into a
bounded buffer per stream and slows the backend through the flow-control window. Block spans
aren't capped until the wallet runs measure what Zodl and Zingo ask for (PS6).

### The seeder's limits

zeeder v1.8.0 is authoritative only (REFUSED outside its zones; NODATA for ANY, RFC 8482's
direction), answers at most 25 addresses per family, and limits each address with GCRA at 10
queries a second, burst 20, dropping the rest silently (ADR 0003). A ~50-byte query brings back
~450 bytes (25 A) or ~750 (25 AAAA), 9 to 15 times more (estimate), so one spoofed victim gets
at most about 60 kbit/s. The gaps: no overall cap, so many spoofed sources fill the uplink;
limits per full address, so spoofing across a /24 or /64 multiplies them; no TC=1 slip; EDNS
and TCP limits unreviewed.

Zeal adds a watchdog over `zeeder_dns_queries_total` and `zeeder_dns_rate_limited_total`
every 10 s, through its existing Prometheus parser (thresholds are proposals): a banner above
200 answered queries a second for 5 minutes; a one-hour pause when more than 100 rate-limited
or 500 answered a second (about 2.4 Mbit/s) last 60 s; after three pauses in 24 hours, paused
until the user resumes; a monthly cap counted as answers × 600 bytes, 50 GB by default. The
banner: "Your seeder got about 2,400 queries a second, far more than a seeder outside Zebra's
default list should. That usually means someone is using it to flood another computer. Zeal
paused it for an hour." Zaino and zeeder run at a lower priority (nice 10;
`BELOW_NORMAL_PRIORITY_CLASS`).

### What becomes public, and what Zeal keeps

The consent screen ([UI](#the-consent-screen)) lists what becomes public, for good: the home
address (in DNS, kept by DNS-history services), the name (in Certificate Transparency logs
within minutes, which scanners watch, so probes start at once), the certificate (indexed
against the address), the region and provider (geolocation; a personal name or WHOIS record
ties it to a person, so Zeal suggests a neutral name and WHOIS privacy), and the software
(zebrad's vendor, version and subversion in `GetLightdInfo`; with Zaino, its version, the build
user, the backing Zebra's `zcashdBuild` and `zcashdSubversion` and any donation address;
`zeeder/<version> (<commit>)` to every peer the crawler contacts; the SOA's
`hostmaster.<domain>`).

Wallets show the server their addresses and timing, the blocks they fetch, the transactions
they ask for, the transparent addresses they look up and where each sent transaction came
from. For the owner's phone that beats a stranger's server; about everyone else, the owner
learns it all. Zeal keeps as little as it can:

- the front door logs counters, never addresses, and the backend sees only 127.0.0.1 (zebrad
  logs no client address for a gRPC call in any case, and Zeal never turns on Zaino's PROXY
  protocol);
- the limiter is keyed by SipHash-2-4 of the address or /64 under a random key per run, pruned
  after 2 minutes idle, or an hour after its last `SendTransaction` for an address that sent one
  (the hourly cap's window), and never written down; "a connection from outside" keeps only a
  time;
- zeeder's lines are filtered and redacted ([Configuration](#configuration));
- diagnostics (`copy_diagnostics`, Copy summary) blank the addresses, the name and the zone
  (`z**.example.com`) unless "include addresses" is ticked, and never carry tokens or keys.

Third parties, each named at consent: Let's Encrypt (the name, and the address of Zeal's ACME
client), the DNS provider (the name, the address, the token), the public resolver the user
picks (one query for a public name), and the certificate logs (the name). The screen also
says that residential terms often restrict servers or cap data (Xfinity's AUP, for example;
current text unconfirmed), that some providers block inbound 53, 80 or 443, and to check local
law: in the EU an IP address can be personal data (CJEU *Breyer*, C-582/14). Zeal never says
running a server is allowed.

**Never**: a plaintext public bind (zebrad's endpoint anywhere but loopback, a `no_tls_*`
Zaino); Zeal writing zebrad's endpoint while the service is off; Zaino's JSON-RPC;
`enable_cookie_auth = false`; turning UPnP on, or a mapping that outlives a pause; an open
proxy (one upstream, fixed at start, no routing by SNI); WAN 8233 to zeeder; a client address
in a log.

## Backend

### Crates and modules

```text
crates/launcher-types/src/public.rs  serde types (below), wasm-clean
crates/launcher-herd/src/public/     pure, native and wasm (HERD.md's crate; PS1 starts it if HERD H1
                                     hasn't): plan.rs, pause.rs, light.rs (the backend's health and,
                                     from PS5, pick_backing, the upgrade gate and the stall rule),
                                     wallet.rs (Zodl's check, branch ids pinned to zebra-chain 15),
                                     evidence.rs, share.rs
crates/launcher-core/src/            config.rs: the managed lightwalletd key, which it can now remove;
                                     logs.rs: the endpoint's and the bind failure's lines; grpc.rs: a
                                     small gRPC client, prost messages written by hand (no protoc) over
                                     hyper's HTTP/2 client; kinds/zaino.rs (PS5): writers (V0_10,
                                     V0_11), guard, environment, exit classes, log reader;
                                     kinds/zeeder.rs: HERD.md's adapter plus public mode and redaction;
                                     bin/fake-light-server.rs
crates/launcher-public/              new, no Tauri: service.rs (PublicServices, ServiceActor),
                                     light.rs (LightMonitor), netcheck/ (netdev, router queries,
                                     hickory-resolver, VPN), ddns/ (DuckDNS, deSEC, Cloudflare,
                                     manual), acme.rs, portmap.rs, gateway.rs (runs the front door),
                                     selftest/ (TLS and hairpin tests, DNS probes), secrets.rs,
                                     awake.rs, firewall.rs
crates/launcher-gateway/             new, a library without Tauri: accept, tls, forward (v1), proxy
                                     (v2), limits, bandwidth, stats
src-tauri/src/public.rs              event forwarding and the `EndpointChanged` handler
```

- **The mock runs the real logic**: wire types stay in `launcher-types`, and the logic goes in
  HERD.md's pure crate, for HERD.md's reason (a logic module would make the wire-types crate a
  logic crate). The mock drives the same `plan()` and `pause()` over fake facts.
- **`launcher-public`** keeps the new network dependencies (ACME, a DNS resolver, router
  protocols, the keychain) in one crate, reviewed and cargo-denied as a unit, depending on
  `launcher-core` for the supervisor and, once MULTI-NODE.md's registry lands, its watches.
- **`launcher-gateway`** is its own crate so the internet-facing code depends only on tokio,
  tokio-rustls (rustls with ring), socket2 and, for `Unavailable` and v2, hyper, never on
  `launcher-core` or Tauri: it is reviewed as a unit, and can become a sidecar's `main`
  unchanged ([The front door](#the-front-door)).

### Actors and channels

```text
Tauri commands ──mpsc<PublicCmd>──▶ PublicServices ──watch<Vec<PublicStatus>>──▶ event `public_status`
supervisor and settings watches (later the registry's and the herd's) ──▶ one ServiceActor per service
  ServiceActor { LightServer, zebrad-mainnet }
    ├─ NetWatch (one for the app) ──────────▶ watch<NetFacts>
    ├─ DdnsActor(name) ─────────────────────▶ watch<DnsStatus>
    ├─ PortMapActor (PS6) ──────────────────▶ watch<MapStatus>
    ├─ AcmeActor(name), DNS-01 via DdnsActor ▶ watch<Option<CertStatus>>
    ├─ Gateway, on its own runtime ◀─ mpsc<GatewayCmd> ─▶ watch<GatewayStats>, mpsc<GatewayEvent>
    └─ LightMonitor (zebrad's endpoint; Zaino's adapter from PS5) ▶ watch<Option<LightStats>>
  ServiceActor { Seeder, zeeder }: NetWatch, DdnsActor(NS target), HERD.md's census actor, the probes
  KeepAwake: holds the inhibitor while any service serves
```

- **ServiceActor** owns the spec, the steps and checks, the evidence and the month's totals.
  On every child update and every 10 s it gathers facts, calls `plan()` and `pause()`, commands
  its children and publishes its status; `Open` re-runs every gate inside it. For the light
  client server it sets or clears the spec's `backend_port` and sends `EndpointChanged` (with a
  oneshot reply) to `src-tauri/src/public.rs`; it never writes `zebrad.toml` or restarts zebrad
  itself. That handler works as `apply_config` does around `apply::apply_and_restart`: it takes
  `AppState::try_config_permit`, and `start_permit` when zebrad is running, writes the keys
  from `managed_keys` with `config::ensure`, and restarts zebrad only if it was running
  (`SupervisorHandle::restart` is `stop` then `start` with the file as it is, and would start a
  stopped zebrad). `managed_keys` (`src-tauri/src/app_state.rs`) takes the address from the
  stored spec for every caller (launch, `set_settings`, `choose_data_dir`, `reset_config`,
  `apply_config`), so a Configure save keeps the key. When a permit is busy, turning on fails
  fast like `set_settings` ("a configuration change is being applied; try again") and the ServiceActor clears `backend_port` again, so no later writer adds the key; turning off
  closes the front door at once and removes the key once the permits are free. Today `ensure`
  runs only at launch and on settings or data-folder changes, and a crash restart inside the
  supervisor reuses the file and its port.
- **AcmeActor**: load the account and certificate, schedule from ARI, order, publish the
  challenge (to the front door, or a TXT through `DdnsActor`), wait for the hit or the
  authoritative TXT, finalize with an `rcgen` CSR, verify, store, push, publish `CertStatus`.
- **Gateway** reads the certificate, the challenge and the mode from `watch`es at every
  handshake, so a restarted front door starts from their current values and nothing is resent;
  it takes the limits and `Stop` on an mpsc, and reports `GatewayStats` on a `watch` and
  `ChallengeHit` and `Outside` events, which become evidence, on an mpsc.
- Actors never `unwrap`; `PublicServices` restarts a panicked one ("public services checks
  restarted"), the front door included ("front door restarted"). On quit it shuts down first
  (the front door stopped, mappings deleted, zeeder stopped), then the supervisor stops zebrad
  (with MULTI-NODE.md's registry, the nodes, dependents first).

### The front door

**In Zeal's process, as a library.** `launcher-public` runs `launcher_gateway::serve` on a
tokio runtime of its own, on a thread of its own, so a flood of handshakes can't starve the
app's commands. It listens on `[::]:8443` and has one upstream, fixed at start: the endpoint's
logged address (Zaino's from PS5). Its lifetime follows the switch, not zebrad's process, so it
answers TLS-ALPN-01 through zebrad restarts and long stalls. It logs through `tracing` (target
`zeal::gateway`, never an address), which shows in the serving node's Logs tab as "front
door", and its port shows among the node's ports (`PublicTls`).

**Not a sidecar, for now.** The other choice is a `zeal-gateway` binary. Against it, for PS1:

- **Shipping.** A sidecar is one more binary for all five targets: built in CI, listed per
  platform as an `externalBin`, signed and notarized, copied out of the AppImage, supervised by
  a supervisor that today runs only zebrad (`SupervisorConfig { command: ZebradCommand }`,
  MULTI-NODE.md; its `LaunchSpec` isn't built), and driven over a JSON-lines protocol. In the
  app it is a task and a few channels.
- **Restarts.** The front door restarts as a task, in milliseconds, from the `watch`es it
  reads; there is no process to spawn and no state to resend.
- **Updates.** A sidecar ships in Zeal's bundle and updates with it, so it gains nothing there;
  in the app there is one version to pin and test.
- **No listener outlives Zeal**, by construction and on every system, where a sidecar needs
  stdin close, PDEATHSIG or a Job Object, and on macOS can outlive a crash.
- **Dependencies.** rustls 0.23.46, whose `ring` feature tauri-plugin-updater 2.14.0 turns on,
  ring 0.17.14, tokio-rustls 0.26.6, socket2 0.6.5 and rustls-platform-verifier 0.7.1 are in
  `Cargo.lock` already, through reqwest 0.13.5 for tauri 2.12.5 and the updater
  (`cargo tree -e features -i rustls`). v1 forwards bytes with no HTTP code, and instant-acme
  brings hyper's HTTP/2 and `h2` anyway, so `Unavailable`'s trailers-only answers add only
  hyper's `server` feature.

What it gives up, until a sidecar is needed:

- **crash isolation**: an abort or memory exhaustion in the front door ends Zeal, and on Linux
  zebrad with it, through PDEATHSIG; the limits, the handshake timer and bounded buffers are
  what keep that from happening;
- **a firewall rule of its own**: on Windows the rule names Zeal's executable
  ([Firewalls](#firewalls)), and Windows waits for PS6 anyway;
- **a sandbox**: Landlock applies per process, so denying the front door the filesystem needs
  a process of its own.

Same user either way, so a sidecar would not be a boundary against code execution, and Zeal
doesn't claim one. The crate's API carries what a sidecar's control protocol would, so
`zeal-gateway` can be a thin `main` over it when Windows or a sandbox asks for one (PS6).

| Mode | Accepts | For |
| --- | --- | --- |
| `ChallengeOnly` | only ALPN `acme-tls/1` with the pending name; the rest refused after the ClientHello | before a certificate; after expiry |
| `LocalOnly` | challenges, and service connections from loopback | the test on this computer |
| `Open` | everyone, within the limits | serving |
| `Unavailable` | challenges; service handshakes complete, and every call gets `UNAVAILABLE` from the front door | zebrad restarting or behind; with Zaino, Zaino behind or stuck too |
| stopped | nothing | paused, off, Zeal not running |

- **v1 (PS1)**: `tokio_rustls::LazyConfigAcceptor` reads the ClientHello and picks the
  challenge or the service config (ALPN `h2`, the watch resolver). In `Open`,
  `copy_bidirectional` to the backend with the idle timer, the maximum age, the upload bucket
  and `AtomicU64` counters. In `Unavailable`, hyper's HTTP/2 server answers every request with a
  trailers-only `grpc-status: 14`, so v1 never parses what it forwards. When the mode leaves
  `Open`, v1 closes the connections it opened in `Open`; wallets reconnect and get
  `UNAVAILABLE` from the front door.
- **v2 (PS2)**: hyper `server::conn::http2` in and one `client::conn::http2` h2c connection
  per client out; the allow-list, method permits, deadlines and header stripping; bodies and
  trailers streamed; its own errors trailers-only (`grpc-status` 14, 8 or 12); a short page
  for a browser's `GET /` ("This is a Zcash light client server. In your wallet, use
  zec.example.com:443").
- **Control**, as Rust types: in, the certificate, the challenge and the mode on `watch`es, and
  `Limits(…)` and `Stop` (no new connections, 5 s for open ones) on the mpsc; out,
  `Ready { addr }`, `ChallengeHit { name }`, `Outside { at }`, and `Stats` every 5 s (open
  connections, client networks, handshakes completed, failed or refused for SNI, bytes,
  refusals by reason, calls per method in v2). A bind failure is `PortInUse`, reported to the
  `ServiceActor`; a panic ends only the front door, which `PublicServices` restarts.

### Types

In `crates/launcher-types/src/public.rs`, reusing HERD.md's `NodeKind::Zeeder`,
`NodeRole::Seeder`, `PortRole::Dns` and `Health`, and `NodePort.fixed`, none of which is on
`main` yet: they come with HERD.md's phases, which the seeder needs, and PS1 adds only
`PortRole::PublicTls` and the light client server's types:

```rust
pub enum PublicService { LightServer, Seeder }
// PortRole gains PublicTls; zebrad's endpoint uses the existing Grpc. From PS5, ExitClass gains
// WaitingForDependency (neither a crash nor Failed), ResourceLimit and TooOld (deterministic), and
// NodeKind::Zaino.uses() = [Zebrad, Zakura].
pub struct PublicHostname(String);  // LDH, ≤ 253 bytes, ≥ 2 labels; not an IP; not .local, .home.arpa,
                                    // .internal, .invalid or .onion
pub struct SeedZone { pub zone: PublicHostname, pub nameserver: PublicHostname } // nameserver outside zone
pub enum DnsProvider { DuckDns, Desec, Cloudflare { zone_id: String }, Manual }
pub struct SecretRef { pub provider: DnsProvider, pub account: String }  // a keychain entry, never a token
pub enum Challenge { TlsAlpn01, Dns01 }
pub enum Mapping { ByHand, Automatic { via: MapProtocol, lease_secs: u32, permanent: bool } }
pub struct PublicLimits { pub max_connections: u32, pub per_address: u32, pub upload_kbit: Option<u32>,
    pub monthly_gb: Option<u32>, pub sendtx_per_hour: u32, pub seeder_ceiling_qps: u32 }
pub struct Consent { pub version: u32, pub at: SystemTime, pub hostname: String }

/// launcher.json `public.services`; no secrets. `node` is the serving node: in PS1, Zeal's one
/// zebrad (`zebrad-mainnet` or `zebrad-testnet`); the seeder's zeeder; Zaino's node from PS5.
pub struct PublicServiceSpec {
    pub service: PublicService, pub node: NodeId, pub switch_on: bool, pub resume_on_launch: bool,
    pub name: Option<PublicHostname>, pub seed: Option<SeedZone>, pub provider: DnsProvider,
    pub secret: Option<SecretRef>, pub external_port: u16, pub internal_port: u16,
    pub backend_port: Option<u16>,  // zebrad's endpoint, on loopback, only while the service is on
    pub lan_addr: Option<IpAddr>, pub challenge: Option<Challenge>, pub mapping: Mapping,
    pub limits: PublicLimits, pub consent: Option<Consent>, pub keep_awake: bool,
    pub donation_address: Option<String>,
}
pub enum PublicState {
    Off, NotAvailable { why: NotAvailable }, SettingUp { step: StepId }, Warming, Serving,
    Degraded { why: DegradedReason },                                  // listener up, calls refused
    Paused { why: PauseReason, since: SystemTime, resumes: Resume },   // listener closed
}
pub enum NotAvailable { NotOnThisSystem, UntestedZebrad { version: String },
    CrawlerPortTaken { by: NodeId, port: u16 } }
pub enum DegradedReason { BackingRestarting, BackingBehind { blocks: u32 }, CertificateExpired,
    ZainoBehind { blocks: u32 }, ZainoStuck { at: u32 } }             // the last two from PS5
pub enum PauseReason { ByYou, LanAddressChanged { was: IpAddr }, NameElsewhere { points_to: IpAddr },
    VpnRoute, DataCap, UnderAttack { qps: u32 }, BuildTooOld { upgrade: String }, BackingStopped,
    NetworkChanged }
pub enum Resume { Automatic, NeedsYou, NextMonth, At(SystemTime) }
pub enum GatewayMode { ChallengeOnly, LocalOnly, Open, Unavailable }
pub enum StepId { Consent, Service, Network, Name, Port, Firewall, Certificate, WarmUp, LocalTest,
    Open, KeepRunning, Share }
pub struct Step { pub id: StepId, pub state: StepState, pub checks: Vec<CheckResult> }
// StepState: Waiting, Running, Done, NeedsYou, Failed, Skipped. CheckResult { id: CheckId, outcome,
// gates, at }, with CheckOutcome NotRun | Running | Pass | Warn(Finding) | Fail(Finding), and a Finding's
// typed facts and FixAction (OpenGuide, CopyCommand, RunAgain, OpenSettings, AddArchive), from which
// ui/src/public_text.rs writes every sentence, as update_text.rs does.
pub enum EvidenceSource { TlsAlpn01Validation, OutsideTlsConnection, UserPhoneTest,
    PublicResolverProbe { resolver: String } }                           // no DNS-01 variant, by design
pub struct Evidence { pub source: EvidenceSource, pub at: SystemTime }
pub enum Reachability { NotChecked, NotYet, Proven { latest: Evidence }, Stale { latest: Evidence },
    Lost { since: SystemTime, why: LostReason } }                        // AcmeConnection, ProbeFailed, HairpinFailed
/// The event payload, one per service. NetFacts: LAN address, gateway, WanKind (Public, Cgnat,
/// BehindAnotherRouter, Unknown), router model, VPN. CertStatus: expiry, ARI window, next try, last
/// problem. LightStats and SeederStats: tips, connections, handshakes (calls from PS2) and
/// queries today, bytes this month.
/// ShareInfo: the strings per wallet, or the peers line.
pub struct PublicStatus { pub node: NodeId, pub service: PublicService, pub state: PublicState,
    pub gateway: Option<GatewayMode>, pub reach: Reachability, pub steps: Vec<Step>,
    pub cert: Option<CertStatus>, pub net: Option<NetFacts>, pub light: Option<LightStats>,
    pub seeder: Option<SeederStats>, pub share: Option<ShareInfo>, pub limits: PublicLimits }
```

### Commands and events

Thin, in `src-tauri/src/commands.rs` beside the others (build.rs accepts only `commands::<name>`
entries in `generate_handler!`), each added to `COMMANDS` in `src-tauri/build.rs` and allowed in
`src-tauri/capabilities/main.json`; the actor validates again. Errors are strings, except
`open_public_service`'s `OpenError { check, message }`, which names the failing check like
`apply_config`'s `ApplyError`.

| Command | Does |
| --- | --- |
| `get_public_status()` | the snapshot |
| `check_connection(refresh)` | read-only network facts and a verdict per service (PS1) |
| `begin_public_setup(service, network)` | creates the spec and returns its `NodeId`: the network's zebrad for the light client server (Zaino's node from PS5), the zeeder node for the seeder, added when there is none; the herd page's "Set up…" |
| `public_plan(node, draft)` | pure: steps, disclosures, records and ports to show |
| `set_public_switch(node, on)` | off pauses at once; on resumes the setup or the service |
| `answer_public_step(node, answer)` | the user's input for the current step |
| `run_public_checks(node, step)` | one step's checks again |
| `open_public_service(node)` | the only command that opens; refused unless every gate passes now |
| `test_public_service(node)` | "Test now", at most every 10 minutes |
| `set_public_limits(node, limits)` | to the front door, or into zeeder's config |
| `allow_through_firewall(node)` | Windows (PS6): one UAC prompt; elsewhere `NotNeeded` |
| `forget_public_service(node, remove_records, remove_token, remove_rule, keep_node)` | undoes what it can, returns what stays public |
| `pause_all_public()` | the tray's "Pause public services" |
| `open_signup(provider)` | a sign-up URL compiled into Zeal, never one from the UI |

`StepAnswer` covers every input: `Consent { version }`, `UseBacking { node }`, `Name(…)` (a
free name, an owned domain or by hand), `Token { provider, token }` (into the keychain at once,
never echoed), `Seed(SeedZone)`, `LanReserved`, `AllowAutomaticMapping`, `PermanentMappingOk`,
`UseExternalPort { port }`, `ForwardedByHand`, `FirewallDone`, `PhoneTest { worked }`,
`KeepAwake`, `Limits`, `Donation`.

Events follow ARCHITECTURE.md's snapshot rule (subscribe, fetch, drop the snapshot if an event
came first): `public_status` (one changed `PublicStatus`, counters coalesced to one event per
5 s per service) and `net_facts`. The tray reads the same watch.

### Files, dependencies, locks

- **Files**: `launcher.json` gains a `public` section (services, the resolver, consent), loaded
  with serde defaults under today's schema version and kept by MULTI-NODE.md's v3, as HERD.md's
  `herd` section is, with no secrets, but never through `set_settings`:
  `settings::apply_from_ui` keeps the stored `public` section, as it keeps `first_run_done` and
  `data_dir`; `get_settings` leaves it out (the UI reads `get_public_status`); and only the
  public-services commands change it, through `PublicServices` and the settings actor
  (`SettingsHandle::update`), the only writer of `launcher.json`. So neither a stale copy in a
  page nor the webview can set `switch_on`, `resume_on_launch` or `consent`, and
  `apply_from_ui`'s stale-copy test gains a `public` case. `<app data>/public/` holds `acme/`,
  `certs/<name>/`, `state.json` (mappings, firewall rules added, the latest evidence) and
  `usage.json`, all written atomically.
- **Crates**, all in `deny.toml`'s allow-list (their trees still to pass `cargo deny`):
  instant-acme 0.8.5 and prost (Apache-2.0); rcgen 0.14, x509-parser 0.18, hickory-resolver
  0.26, keyring 4.2.0 and siphasher (MIT OR Apache-2.0); igd-next 0.18, crab_nat 0.8, netdev
  and, in the UI, qrcodegen (MIT). tokio-rustls, socket2, hyper, hyper-rustls,
  rustls-native-certs and rustls-platform-verifier are in `Cargo.lock` already. instant-acme
  0.8.5's `hyper-rustls` feature turns on hyper's `http2` and hyper-rustls's `native-tokio`,
  and its `rcgen` and `x509-parser` features take those two crates (crates.io's dependency list,
  read 2026-11-03), so `h2`, rcgen's `yasna` and `pem`, and x509-parser's `asn1-rs`,
  `data-encoding`, `der-parser`, `nom`, `oid-registry` and `rusticata-macros` are new; rcgen's
  `CustomExtension::new_acme_identifier` builds the RFC 8737 extension
  (`src/certificate.rs:664` at 0.14.10). hyper gains its `server` feature for the front door.
  Sidecars, not linked: zainod (Apache-2.0, PS5), zeeder (MIT OR Apache-2.0); Pebble (MPL-2.0)
  runs only in CI.
- **One crypto provider.** `Cargo.toml` and `.github/dependabot.yml` record that clients built
  without a rustls provider panic ("No rustls crypto provider is configured"). Zeal installs
  ring as the process default at startup when none is installed (first install wins), gives
  its own configs an explicit provider, and tests the updater's, the ACME and the self-test
  clients in one process. `launcher-core`'s reqwest stays at 0.12 without TLS.
- **No locks** (`clippy.toml` bans them): each service's state in its actor, published on
  `watch`; the certificate, challenge and mode on `watch` inside the front door; per-address
  counts in `LimitsActor` (mpsc in, oneshot out, a guard's drop sends `Closed`); connections,
  method concurrency and the upload budget on `Arc<Semaphore>`; counters on `AtomicU64`; the
  keychain through `spawn_blocking`. No DashMap: it locks its shards.

## UI

### Where the switches are

- **On the serving node's page**: zebrad's page (Zaino's from PS5) and zeeder's get a
  **Public** tab between Configure and Node settings, with the switch, the setup and the status
  panel.
- **On the herd page**, per network: a **Public services** section, one line per service with
  its dot, one fact and the same switch ("Light client server · serving · 3 connections",
  "DNS seeder · paused: the name points elsewhere"). Without a node yet it shows "Set up…"
  (`begin_public_setup`). HERD.md's setup goal "Help the network (a server)" leads here.
- **With one node**, where there is no sidebar: Settings → Public services, the same lines.
- Everything follows the Mainnet and Testnet tabs, with Testnet marked as everywhere.

The switch is a `<button role="switch" aria-checked>` with a visible label. Off pauses at
once, with no dialog. On opens the setup the first time; after that it shows "Checking…",
"Opening…", then serving or the failing check with its fix. Disabled, it says why and is
never hidden: "Not on this Mac yet", "Zeal hasn't tested this zebrad as a light client
server", "zeeder needs port 8233, which Zebra · Mainnet uses [What can I do?]" (the 8233
dialog). While zebrad serves, the light client server's label says "experimental".

### The guided setup

A checklist in the add-node dialog's shell:

```text
 Light client server (experimental) · Mainnet · setting up                        [ ● On ]
 ✓ You agreed to what becomes public                                             2 min ago
 ✓ Zebra · Mainnet serves light wallets on this computer, at the tip
 ✓ Your router has a public address, 203.0.113.10
 ✓ zec-alice.duckdns.org points here
 ● Port 443 on your router → this computer (192.168.1.20), port 8443
     Add the forward in your router: it takes about two minutes.
     [Show me how · TP-Link Archer AX55]  [I've done it: check]
 ○ This computer's firewall
 ○ A certificate from Let's Encrypt
 ○ Test on this computer
 ○ Open to the internet
 ○ Keep it running
 ○ Share
```

✓ done, ● current, ○ waiting, ✕ failed with its fix first, – skipped. Done steps fold to one
line with their evidence under "details" ("Your router says your public address is
203.0.113.10"); every sentence comes from typed facts in `ui/src/public_text.rs`, with a
"Why?" in plain words ("Your provider shares one address among many homes (CGNAT), so nobody
outside can connect in"). Next waits for the gates; Back always works; progress survives a
restart; focus follows the current step; layouts use container queries and work at 360 px.
"Ask my router to do it" joins the port step with automatic mapping (PS6).

### The consent screen

```text
Serving wallets from this computer makes these public, for good:
 • Your home internet address, 203.0.113.10, under the name zec-alice.duckdns.org.
   DNS history services keep it after you stop.
 • The name zec-alice.duckdns.org, in public certificate logs, minutes after Zeal gets its
   certificate. Scanners read those logs, so expect connection attempts right away.
 • That this computer runs Zcash software: every wallet that connects is told it is
   Zebra 7.0.0.
What you take on:
 • Zebra's light wallet server is experimental: wallets may not work with it yet, and a
   Zebra update may change it.
 • Wallets that use your server show it their address, the blocks they download, the
   transparent addresses they look up and the transactions they send. Zeal keeps no client
   addresses, but anyone who controls this computer could.
 • While the light client server is switched on, including during setup and automatic pauses,
   programs on this computer can use Zebra's light wallet port without a password. Web pages
   in a browser can't.
 • Your internet plan may not allow servers, and the traffic counts toward any data cap.
   Check its terms, and your local law.
 • This computer also holds Zallet · Mainnet. Keep large amounts elsewhere.
 • Zeal gets the certificate from Let's Encrypt, so you accept its Subscriber Agreement [link].
 • Zeal talks to your router, your DNS provider and Let's Encrypt. No one else.
 [ ] I understand what becomes public
```

With Zaino (PS5) the software line reads "Zaino 0.11.0, built by runner, reading Zebra 7.0.0".
The seeder's version names the zone and the NS target, `zeeder/1.8.0 (<commit>)` as every peer
sees it, taken from the bundled binary (its `--version` prints `1.8.0 (<commit>)`, and zeeder
logs `User-Agent: …` at start), the public resolver it will ask, the operator's duties and "not
a candidate for Zebra's default list".

### The status panel

```text
Zebra · Mainnet   zebrad 7.0.0   [Running]                                 [Stop] [Restart]
Dashboard   Logs   Configure   Public   Node settings
 Light client server (experimental)                                               [ ● On ]
 Serving · reachable from the internet (a connection from outside completed TLS 4 min ago)
 ┌ Reachability ────────────────┐ ┌ Certificate ───────────────┐ ┌ Use ─────────────────────┐
 │ zec-alice.duckdns.org:443    │ │ Let's Encrypt · 61 days    │ │ 3 connections, 2 networks│
 │ DNS → this network (2 min)   │ │ renews about 18 Dec (ARI)  │ │ 1,204 handshakes today   │
 │ 443 → 192.168.1.20:8443      │ │ checked from outside 29 Oct│ │ 4.1 of 300 GB this month │
 │ [Run checks]  [Phone test]   │ │ [Renew now]                │ │ 12 refused by limits     │
 └──────────────────────────────┘ └────────────────────────────┘ └──────────────────────────┘
 Zebra at block 3,544,812 · light wallet endpoint 127.0.0.1:9067 · front door running
 Share   Zodl  zec-alice.duckdns.org:443 [Copy] [QR]   Zingo, Zkool  https://zec-alice.duckdns.org:443 [Copy]
 Checks: 17 passed, 1 warning ▸     What is public ▸     [Limits…]  [Test now]  [⋯ Turn off and forget]
```

Other first lines: "Catching up · wallets are told to try again" (with the lag); "Serving · not
yet confirmed from outside" (with the phone-test card); "Paused · zec-alice.duckdns.org points
at 198.51.100.7, not this network. Zeal resumes when it matches."; "Paused · Zebra 7.0.0
reached its end of support. Wallets that use your server see an error until Zeal updates
Zebra."; "Under heavy load: the limits refused 3,200 connections in 10 min" with [Pause]. With
Zaino (PS5): "Paused · Zaino stopped at block 3,543,404: this Zaino doesn't know NU7. Wallets
that use your server see an error until Zeal updates Zaino." Certificate banners turn amber 14
days before a failing expiry, red at it.

```text
 DNS seeder                                                                       [ ● On ]
 Seeding · answering the internet (checked through 1.1.1.1, 2 h ago) · TCP 53 not checked
 ┌ Delegation ──────────────────┐ ┌ Answers ───────────────────┐ ┌ Peers ───────────────────┐
 │ mainnet.seed.example.com     │ │ 1,920 queries today        │ │ 25 IPv4 · 6 IPv6 servable│
 │ NS ns-seed.example.com       │ │ 14 rate-limited · 0 errors │ │ ready · protocol 170190  │
 │ → 203.0.113.10 (TTL 300)     │ │ peak 3 a second            │ │ your Zebra: 12 outbound  │
 └──────────────────────────────┘ └────────────────────────────┘ └──────────────────────────┘
 For nodes elsewhere   initial_mainnet_peers = [<Zebra's five>, "mainnet.seed.example.com:8233"]  [Copy]
 Not in Zebra's default list. What seeder operators are expected to do ›
```

Its numbers come from `zeeder_dns_queries_total`, `zeeder_dns_rate_limited_total`,
`zeeder_peers_servable` and `zeeder_min_protocol_version` through Zeal's parser, and from the
herd's peer counts (whether the counters split by zone is unconfirmed).

### Elsewhere

- **Sidebar** facts: "serving", "catching up", "paused" for the serving node (zebrad, Zaino
  from PS5); "seeding" for zeeder.
- **Tray**: a disabled "Public: light client server serving" line (Linux's tray has no
  tooltip), "Pause public services", "Resume public services…", and "Quit (stops public
  services)" while one is on.
- **Banners** for automatic pauses say the cause and what wallets or nodes see; "Resume anyway"
  isn't offered for a too-old build, a certificate or an attack.
- **Health verdicts** (`ui/src/health.rs`): a build too old, then paused, then degraded, then
  not confirmed from outside.
- **Add node**: HERD.md's zeeder card. From PS5, Zaino's line becomes "Bundled with Zeal ·
  reads an archive node", with "or your own zainod" optional (so the test pairing
  `program_prompt` with `bundled()` changes); until then it keeps today's caveat (releases up
  to 0.10.2 stop syncing at NU7's activation) and adds that a zainod you pick runs for this
  computer only.
- **Words**: "light client server" (the maintainer's), "front door", "DNS seeder",
  "experimental"; Off, Setting up, Serving, Catching up, Paused, Not available.

## Testing

- **Pure logic**, native and wasm: the plan as tables of facts to steps for both services, with
  a test that no plan step makes a network request; pauses with their hysteresis and resume
  rules (zebrad behind or restarting, the name elsewhere, the cap, a third attack; from PS5 a
  stuck Zaino and the NU7 boundary); evidence ages, and DNS-01 never counting; the endpoint's
  key present only while the service is on; from PS5 `pick_backing`, the upgrade table,
  untested versions kept local and the stall rule; the branch check; Zodl's check over
  `LightdInfo` bytes captured from zebrad's endpoint (and a real Zaino in PS5), with field
  numbers pinned to a vendored `service.proto`; `PublicHostname` and `SeedZone`.
- **Adapters**: zebrad's managed key, written while the service is on, removed when it is off,
  a hand edit overridden, a non-loopback value refused, a hand-set loopback value kept while no
  service is set up; the `Opened lightwalletd gRPC endpoint at` and bind-failure lines from
  captured fixtures; from PS5, each `zainod.toml` writer against its `generate-config` fixture,
  the guards, the environment scrubbing, the exit classifiers on captured lines and the restart
  on the archive's pid change; zeeder's redaction on lines inside and outside the `dns_query`
  span, with no address reaching the ring; an answered query and a dropped one add no line to
  the ring or the log file.
- **The front door against `fake-light-server`**, an h2c gRPC server on hyper that serves
  `GetLightdInfo`, `GetLatestBlock`, a delayed `GetBlockRange`, a long `GetMempoolStream` and
  `SendTransaction`, and can stall at a height, stop reading, send error trailers, or, for
  PS5, log `invalid consensus branch id` and exit with each classified Zaino message. Cases:
  bytes both ways (v1), unary and streaming calls, trailers and cancellation (v2); the challenge
  certificate for `acme-tls/1`; a wrong or missing SNI refused; every mode; a certificate
  rotated during an open stream; every limit, with client addresses injected through a trait; a
  slow handshake; the maximum connection age; v2's allow-list; `Stop` closing the port within
  1 s, and `PortInUse` on a taken one; a change to `Unavailable` closes open v1 connections; no
  address in its log.
- **`launcher-public`**: ACME against Pebble and pebble-challtestsrv in Linux CI (pinned by
  sha256 in `.github/tool-versions.env`): TLS-ALPN-01 through the real front door, DNS-01,
  renewal, the error mapping, deactivation before "Test now", no order while valid (Pebble's
  ARI is unconfirmed; without it, `replaces` is never sent). Dynamic DNS against a local hyper
  server; DNS checks against a `hickory-server` zone (delegation present and missing, NODATA,
  SERVFAIL, NXDOMAIN); fake NAT-PMP, PCP and IGD servers (the exact port, refusals 718, 724 and
  725, renewal, deletion, cleanup after a crash); WAN classification; actors under
  `tokio::time::pause`.
- **Contract tests**, opt-in like `tests/real_zebrad.rs` and required when a pin moves.
  `real_zebrad.rs` gains the light endpoint (PS1): a Regtest zebrad with the managed key and
  the real front door with a Pebble certificate; `GetLightdInfo`, `GetLatestBlock`,
  `GetTreeState`, `GetSubtreeRoots` and a `GetBlockRange` streamed to a slow client through the
  front door; an HTTP/1.1 `POST` to the endpoint gets no gRPC answer; the key removed and the
  port closed once the switch is off; the endpoint and bind-failure lines captured for the
  fixtures. This gates every zebrad pin while zebrad serves. `real_zainod.rs` (PS5): a Regtest
  zebrad with NU7 at height 10, the pinned `zeal-zainod` and the real front door with a Pebble
  certificate; Zaino crosses NU7, the wallet check passes through the front door, Zaino
  restarts with zebrad, and a long `GetBlockRange` streams to a slow client. This gates the
  Zaino pin. `real_zeeder.rs` (Linux): `print-config`, SOA, NS and A from loopback, REFUSED out
  of zone, `/health`, the metrics, SIGTERM.
- **Wallet acceptance**, a RELEASING.md checklist before PS1's release on Testnet and Mainnet,
  at every zebrad pin while zebrad serves and for every Zaino pin, on a real home connection:
  Zingo through a scripted `zingo-cli` (a new wallet, an old birthday, a send with a memo), and
  Zodl Android, Zodl iOS and Zkool by hand (Custom in Manual mode, a new wallet, a restore, a
  send, a transparent balance, the app sent to the background mid-sync). Each run records the
  first-sync time (zebrad's one-block-at-a-time reads; #1665 and #1537 with Zaino) and the bytes
  sent. Testnet needs Testnet wallet builds; that Zodl publishes one is unconfirmed.
- **By hand**, per release touching the flow: a TP-Link or FRITZ!Box with UPnP on and off,
  carrier-grade NAT through a phone's hotspot, macOS's Local Network prompt, ufw on.
- **The mock** (`ui/src/mock/public.rs`) runs the pure plan over fake facts, combines with
  `?herd`, and fixes `ui/src/mock/herd.rs`, which gives Zaino a JSON-RPC port (8237) that Zeal
  keeps off. Flags, listed in `ui/src/mock.rs` and ui/README.md: `?public=setup`, `serving`,
  `unconfirmed`, `catchingup`, `eos`, `untested`, `moved`, `lan`, `cap`, `cgnat`, `doublenat`,
  `noforward`, `ratelimited`, `expiring`, `keychain`, `seeder` (replacing HERD.md's
  `?seeder=public`), `seeder-warmup`, `seeder-attack` and `portclash`, and from PS5 `zaino`,
  `zaino-nu7` and `zaino-stuck`. Screenshots come from the headless-Chromium workflow.

## Phases

Each phase is one pull request, or a short series, with its tests, mock flags and screenshots,
and ARCHITECTURE.md updated for what landed. PS1 ships on its own, on Zeal as it is today: one
supervised zebrad, today's `launcher.json`, no node registry and no HERD.md phase. After PS1 the
seeder (PS4) and the light client server's later phases are independent tracks: whichever
upstream gate opens first ships first.

| Phase | Ships | Needs | Screenshots from |
| --- | --- | --- | --- |
| **PS0 Decide and ask** | this document, the edits below, the upstream issues | — | — |
| **PS1 A light client server from zebrad (Linux)** | zebrad's endpoint as a managed loopback key, with the restart, the log line and the health checks; front door v1 in Zeal's process (`launcher-gateway`), its modes and v1 limits; `AcmeActor` (TLS-ALPN-01, staging then production, renewal) against Pebble in CI; the name step (DuckDNS, deSEC, by hand) with `DdnsActor` and the keychain; the read-only network checks and "Check my connection"; the forward by hand with the vendor guides; the firewall commands; the local, hairpin and phone tests and "Test now"; Open, pause and resume; keep-awake; the types, the pure logic and the screens (consent, setup, panel, Share, tray items, banners); Testnet and Mainnet, labelled experimental | today's single supervisor and settings: the endpoint's port is a default with a free-port check, not MULTI-NODE.md's allocator, and the screens sit in Settings → Public services and on the node's page; Zeal's wallet runs passing on Testnet and Mainnet before the release | `?public=setup`, `serving`, `unconfirmed`, `cgnat`, `expiring` |
| **PS2 Front door v2** | HTTP/2 ended in the front door: the allow-list (no reflection), per-method limits, deadlines, header stripping, connection age with GOAWAY, trailers-only errors, the browser page, calls per method in the status panel | PS1; the first-sync times and bytes from PS1's wallet runs | `?public=serving`, `ratelimited` |
| **PS3 macOS** | the light client server on macOS: the Local Network prompt and `NSLocalNetworkUsageDescription`, `caffeinate`, the firewall checks | PS1; how Tauri 2 merges a `src-tauri/Info.plist`, checked | `?public=setup` on macOS |
| **PS4 The public DNS seeder (Linux)** | the bundled `zeal-zeeder`; the domain step (Cloudflare, deSEC, by hand, a dynamic-DNS NS target), the port 53 guide, firewall help, warm-up, Open, the probe, the watchdog, the panel, Share, redaction, on PS1's shared flow | HERD H5 (the zeeder kind, census, the 8233 dialog, on MULTI-NODE phase 4); PS1; zeeder's crawler setting, or the 8233 dialog | `?public=seeder`, `seeder-warmup`, `seeder-attack`, `portclash` |
| **PS5 Zaino as the backend** | `ZAINOD_VERSION`, `ZAINOD_COMMIT`, `zainod-sidecars.yml`; `zeal-zainod` on Linux (macOS once CI passes); the Zaino adapter's config, gates, restarts and exit classes; the front door's upstream moved from zebrad's endpoint to Zaino, Testnet first; the build user and a donation address in Share | PS2; MULTI-NODE phase 5 (the Zaino kind, dependencies); HERD H3 (storage in the spec, so `dependency_ok` refuses a pruned Zakura), or `Zaino.uses()` kept at `[Zebrad]` until it lands; a Zaino release with NU7 passing `real_zainod.rs` and the Testnet wallet run; two weeks on Testnet without a Zaino-caused pause before Mainnet | `?public=zaino`, `zaino-nu7`, `zaino-stuck` |
| **PS6 Reach and hardening** | Windows (after zebrad's stdin-close flag: the UAC firewall rule, keep-awake, stops); optional automatic mapping; DNS-01 and external port 9067; Cloudflare names for the light client server; IPv6 (pinholes, AAAA after an inbound check, zeeder on IPv6); macOS public seeding once zeeder stops on stdin close; the front door as a sidecar if Windows or a Landlock sandbox needs one; Zaino's own index and the rewrite's schema; a second light client server (Testnet beside Mainnet) on its own port; a block-span cap; more DNS providers | PS1, and PS5 for the Zaino items | per item |

- **Why PS1 is Linux only.** With no sidecar to build or sign, macOS waits on two checks, the
  Local Network prompt (TN3179), which the read-only router queries trigger, and how Tauri 2
  merges `NSLocalNetworkUsageDescription` into `Info.plist`, and has one known gap: a crashed
  Zeal leaves zebrad, its loopback endpoint included, running until the next launch
  (ARCHITECTURE.md).
  Windows waits longer: until zebrad has the stdin-close flag every zebrad stop there is a hard
  one (ARCHITECTURE.md), and the service restarts zebrad each time it is turned on or off; its
  firewall rule needs a UAC prompt.
- **Why PS1 can open Mainnet**: see [the limits](#the-light-client-servers-limits); it does so
  once Zeal's wallet runs pass there.
- **If PS1 runs short**, a point release can bring ARI (renewal at two thirds of the lifetime
  meanwhile), "Test now" (the setup's staging validation is the outside evidence meanwhile),
  deSEC beside DuckDNS, and the router's model for the vendor guide (the user picks the guide
  meanwhile). The endpoint, the front door, issuing and renewing, one name provider and the
  evidence from outside can't wait.

Notes for later, not planned: Tailscale Serve for "just my phone"; an onion service (Zodl
accepts `.onion` hosts without TLS, but Arti's onion hosting is still maturing and its
`hs-pow-full` feature links LGPL code, outside `deny.toml`); IP-address certificates.

**PS0's edits.** MULTI-NODE.md: zebrad's light-wallet endpoint, on `PortRole::Grpc`, in the
zebrad section; the Zaino section, for PS5 (the latest GitHub release is 0.10.1, 0.10.2 a tag,
a crates.io release and a Docker image; `rpc` only; an archive behind it, Zakura included; the
raised timeout; the restart with its archive; `ZAINO_*` stripped; two schemas; refusals in place
of "warns"); a zeeder row in the kinds table, and Zaino using "an archive node"; the binaries
non-goal revised for `zeal-zainod` and `zeal-zeeder` (answering its open question 1);
`PortRole::PublicTls`, and Zaino's metrics on 9998. ARCHITECTURE.md, as PS1 lands:
`rpc.lightwalletd_listen_addr` in the managed keys (loopback, assigned, only while the light
client server is on), and the front door. HERD.md: zeeder bundled on Linux (its open question
4); a public seed on a dynamic address instead of "a static public IP (asked, never inferred)";
the outside check through a chosen resolver instead of "shows the `dig` command"; no peer slices
beside a public seeder; H7's public mode moved to PS4; Zaino on an archive other than Zallet's
in the small-server preset; the small-server preset's "a public seed belongs on another IP or a
VPS" replaced by the 8233 dialog; open question 6 answered (public seeding at home, as the
maintainer asked); the census table's "Public seed (servers only, a later phase)" pointed at
PUBLIC-SERVICES.md.

**PS0's upstream issues.** Zebra: testing the light-wallet endpoint with the SDKs (#11158's
follow-up work); lightwallet-protocol v0.4.0 and v0.6.0 (`poolTypes`, `vin` and `vout`,
`LightdInfo`'s fields 16 to 18, `GetStandardFee`) and the build fields of `GetLightdInfo`;
pipelined `GetBlockRange` reads; an error, not `OK`, when a stream gives up on a slow client;
the HTTP/2-only listener as a documented promise; a listener only the front door can use (a
Unix socket, or authentication). zeeder: a crawler `listen_addr` that takes loopback or port 0
and is never advertised (with #106), and a `cache_dir`; rate-limit drops logged as one line
without addresses; an overall response cap; limits by /24 and /56; TC=1 slip; no RFC 1918,
CGNAT or ULA addresses in answers; a stop on stdin close; macOS and Windows builds. Zaino: a
release with #1655; fixes for #1665 and #1537; a `ZAINO_BUILD_USER` override; an h2 bump in the
0.10 line.

## Open questions for the maintainer

1. **Serve PS1 from zebrad's experimental endpoint**, though the Zebra book says not to depend
   on it for a service run for others yet, and not to run it on a user's machine? → Yes: on
   loopback, only while the service is on, behind the front door, labelled experimental, with
   the local risk said at consent and Zeal's wallet runs as the test; Zaino replaces it in PS5.
2. **Mainnet in PS1, on front door v1?** → Yes, once the wallet runs pass on Mainnet: zebrad's h2
   has the RUSTSEC-2026-0258 fix, its subtree roots cost a hash lookup each, and the front door
   goes `Unavailable` when zebrad falls behind; per-method limits follow in PS2, before Zaino,
   which serves only through v2.
3. **The front door as its own sidecar, or in Zeal's process?** → In Zeal's process, as a
   library crate: no new binary to build, sign, notarize and bundle for five targets, no
   supervisor change and no control protocol, a listener that can't outlive Zeal, and rustls
   with ring and tokio-rustls already in `Cargo.lock`. It gives up crash isolation, a firewall
   rule scoped to its own executable and a per-process sandbox; it becomes a sidecar if Windows
   or Landlock needs one (PS6).
4. **Certificates from Let's Encrypt only in PS1** (TLS-ALPN-01, external 443), with no option
   for files the user brings? → Yes; files may come later as an option.
5. **Bundle `zeal-zainod` and `zeal-zeeder`**, or download them on demand, verified like
   zebrad's tarball? → Bundle: "pull in zeeder" and "easily be enabled" ask for it, upstream has
   no binaries beyond zeeder's Linux ones, and a local build leaks its builder's name.
6. **A name required for the light client server** (a free one works), with no IP-certificate
   or Tailscale path built in? → Yes.
7. **Ports**: a hand-made 443 forward first, automatic mapping as an opt-in, 9067 with DNS-01
   only when 443 is impossible; or automatic mapping first with a fall back to 9067? → The
   first: client networks block 443 least, and only 443 lets Let's Encrypt prove the path.
8. **The seeder on a dynamic home address**, against HERD.md's static-IP rule? → Allow it.
9. **Public seeding beside a default-port archive**: wait for zeeder's crawler setting, or ship
   with the 8233 dialog? → Ship with the dialog and its evidence. Who at the Foundation lands
   the zeeder change, and when?
10. **The light client server's node**: share the herd's archive (on a laptop, the wallet's), or
    recommend a second one once public? → Share, prefer one that isn't the wallet's, revisit
    with measured load; the same for zebrad's endpoint and, in PS5, Zaino's archive.
11. **No index of Zaino's own at first**, never on Windows? → Yes, while releases are in the 0.10
    line; the rewrite always keeps one.
12. **"Test now" through a Let's Encrypt staging order**, the last authorization deactivated and
    the order never finalized, at most every 10 minutes: acceptable use? → Yes, on a click only.
13. **Defaults**: the limits, the 300 GB and 50 GB caps, the seeder's pause thresholds,
    "resume when Zeal starts" on? → As proposed, tuned by the wallet runs.
14. **Keep-awake on by default, and tauri-plugin-autostart** for "Start Zeal when I sign in"
    (licence unconfirmed)? → Keep-awake yes; the plugin is the maintainer's call.
15. **Laptops**: offer the switches with a warning, or hide them? → Offer, with the warning.
16. **Wallet acceptance**: which Zodl and Zingo versions and devices are the reference, and is a
    scripted `zingo-cli` run enough before PS1's release and for Zaino's Testnet gate?
17. **Zaino on front door v1** if its NU7 release is the rewrite, with its own `[grpc]` limits?
    → No, v2, for the allow-list, the header stripping and the re-framing.

## Sources

- **This repository** (`main`): MULTI-NODE.md, HERD.md,
  ARCHITECTURE.md, SECURITY.md, SIDECARS.md; `crates/launcher-core/src/config.rs` and
  `logs.rs`; `crates/launcher-types/src/nodes.rs` and `lib.rs`;
  `ui/src/components/add_node.rs`, `ui/src/mock.rs`, `ui/src/mock/herd.rs`;
  `src-tauri/tauri.conf.json`, `tauri.linux.conf.json`, `src-tauri/Cargo.toml`; `Cargo.toml`,
  `Cargo.lock` (with `cargo tree -i rustls` and `cargo metadata`, 2026-11-03), `deny.toml`,
  `clippy.toml`, `.github/dependabot.yml`.
- **Zaino** at `zainod-0.10.2` (`5ced0ae8`): `packages/zainod/src/config.rs`;
  `zaino-serve/src/server/config.rs`, `server/grpc.rs`; `zaino-state/build.rs`,
  `src/indexer.rs`, `src/chain_index/validator_source.rs`; `zaino-rpc/src/probe.rs`;
  `zaino-common/src/config/`; `Cargo.lock`, `LICENSE`, `rust-toolchain.toml`. PRs #1655, #1678,
  #1684, #1688 to #1709 (the rewrite's changesets), #1267; issues #1731, #1665, #1537, #1193,
  #1299; tags, releases and #1655 read with `gh api` on 2026-11-03.
- **zeeder** v1.8.0: `src/config.rs`, `src/commands.rs`, `src/seeder.rs`,
  `src/dns/request_handler.rs`, `src/crawl/servability.rs`, `docs/operations.md`, ADRs 0002 and
  0003, issues #43 and #106, the release notes.
- **Zebra** v7.0.0 (commit `e8db87eb`, as in `ZEBRAD_COMMIT`): `zebra-network/src/config.rs`,
  `peer_set/initialize.rs`;
  `zebra-rpc/src/config/rpc.rs`, `src/server.rs`, `src/server/cookie.rs`, `src/methods.rs`,
  `src/lightwalletd.rs`, `src/lightwalletd/server.rs`, `methods.rs` and `tracing.rs`,
  `proto/service.proto` and `compact_formats.proto`, `Cargo.toml`;
  `zebrad/src/commands/start.rs`, `src/components/tokio.rs`, `src/components/mempool.rs`;
  `zebra-chain/src/parameters/` (`constants.rs`, `network_upgrade.rs`);
  `book/src/user/lightwalletd.md`; `CHANGELOG.md` (6.4.0, 7.0.0); `Cargo.lock` (tonic 0.14.5,
  hyper-util 0.1.20, h2 0.4.16). PRs #10228, #11096 and #10953; issue #11158, read with the
  GitHub API on 2026-11-03. tonic 0.14.5 (`src/transport/server/mod.rs`, `src/codec/mod.rs`)
  and hyper-util 0.1.20 (`src/server/conn/auto/mod.rs`). Zakura 1.8.0's `start.rs`, a local
  copy. zcashd's `doc/book/src/dev/dnsseed-policy.md`.
- **Wallets and protocol**: lightwallet-protocol v0.6.0 and its `CHANGELOG.md` (v0.3.6 to
  v0.6.0, read 2026-11-03); librustzcash's `zcash_client_backend` 0.25.0 (`src/sync.rs`,
  `CHANGELOG.md`) and its `proto/` on `main` (read 2026-11-03); lightwalletd's README, v0.6.1
  (GHSA-xq7v-jj38-pcwr) and its releases (GitHub API, 2026-11-03); zodl-android
  (`ZashiEndpointTextField.kt`, `ServerSelectionPolicy.kt`) and its SDK (`SdkSynchronizer.kt`);
  zodl-swift-wallet-sdk; zingolib and zingo-mobile; zkool2; edge-currency-accountbased
  (`zcashInfo.ts`); zeckyapp/rock-it (`docs/findings.md`).
- **Crates**: instant-acme 0.8.5 (`src/types.rs`, `account.rs`, `order.rs`, and its dependency
  list on crates.io, read 2026-11-03), rcgen 0.14.10 (`src/certificate.rs`), x509-parser 0.18.0's
  dependency list, rustls-acme 0.15.4, igd-next, crab_nat, portmapper, keyring, hickory-resolver;
  RUSTSEC-2026-0258.
- **Let's Encrypt**: the challenge-types, [rate-limits](https://letsencrypt.org/docs/rate-limits/),
  profiles and IPv6 pages; the posts on [ending expiry emails](https://letsencrypt.org/2025/06/26/expiration-notification-service-has-ended),
  [IP certificates](https://letsencrypt.org/2026/01/15/6day-and-ip-general-availability) and
  ["Decreasing Certificate Lifetimes to 45 Days"](https://letsencrypt.org/2025/12/02/from-90-to-45/)
  (read 2026-11-03).
- **Platforms**: Apple's [TN3179](https://developer.apple.com/documentation/technotes/tn3179-understanding-local-network-privacy)
  (read 2026-11-03) and firewall guide; Microsoft's firewall rules; the kernel's ip-sysctl;
  Tauri's [configuration files](https://v2.tauri.app/develop/configuration-files/).
- **Networks and DNS**: [miniupnpd.conf](https://github.com/miniupnp/miniupnp/blob/master/miniupnpd/miniupnpd.conf);
  CERT/CC VU#339275; Akamai's UPnProxy report; RFCs 2181, 6598, 8482, 8659, 8737; the Public
  Suffix List; the DuckDNS, deSEC and Cloudflare APIs and deSEC's forum; No-IP's help; the
  router vendors' pages; CJEU *Breyer*, C-582/14.
