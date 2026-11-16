# Public services: a light client server and a DNS seeder

## Status

> **Proposal, 2026-11-03.** A design for review before any code. It builds on
> [MULTI-NODE.md](MULTI-NODE.md) (the node registry, kinds, ports and phases) and
> [HERD.md](HERD.md) (roles, the zeeder kind and its census), and describes the launcher's
> current shape where needed. [ARCHITECTURE.md](ARCHITECTURE.md) stays the description of
> what is built and is updated phase by phase as this lands. Facts marked "unconfirmed" are
> to be checked before the phase that relies on them; numbers marked "proposal" are starting
> values, to be tuned.

> **PS1 is built** (Linux, experimental): see [PS1 as built](#ps1-as-built) for what landed,
> what moved to a point release and where the code differs from this design.

> **zingod replaces Zaino as the planned backend (proposal, 2026-11-16).** No Zaino release
> follows NU7, and Zingo Labs now maintain zingod, a fork of Zaino that does. Zeal bundles it on
> Linux as the light client server's second backend
> ([zingod as the backend](#zingod-as-the-backend)), in phases Z1 to Z5
> ([Phases for zingod](#phases-for-zingod)): Z1 bundles the pinned, attested binary, which
> nothing runs yet; Z2 supervises it, with a contract test and no UI; Z3 lets a Linux user choose
> it for a Testnet light client server; Z4 opens Mainnet after a measured gate; Z5 brings macOS.
> None is built yet.

The maintainer asked for two optional services that a user can turn on at home. First: "an
optional DNS seeder, pull in zeeder", and "The DNS seeder should let you expose the port and
direct your domain name to your server at home so other nodes can bootstrap". Then: "Add a
light client server too that can easily be enabled like zeeder for users to expose the port
and serve their own light client server from their home server". Asked which server, they
answered: "Zaino specifically".

So the light client server starts as zebrad's own light-wallet endpoint, served from the
zebrad Zeal already runs. [Zaino](https://github.com/zingolabs/zaino) was to replace it once a
Zaino release could follow NU7. None has, and Zingo Labs now maintain
[zingod](https://github.com/zingolabs/zingod), their fork of Zaino, which follows NU7: Zeal
bundles it on Linux as the light client server's second backend
([zingod as the backend](#zingod-as-the-backend)).
The seeder is [zeeder](https://github.com/ZcashFoundation/zeeder), HERD.md's seeder kind, in a
public mode. lightwalletd appears only as a note.

Three upstream facts shape the plan (checked 2026-11-03):

- **Zaino can't follow NU7 yet.** Every release up to 0.10.2 stops at the block before NU7's
  activation (Mainnet height 3,543,405, about 2026-11-05) and retries forever (zaino#1731).
  The fix, zaino#1655 "Feat: NU7", is open with `mergeable_state: blocked`, and there is no
  release candidate. (2026-11-16: still no Zaino release follows NU7; Zingo Labs moved to
  zingod, which does.)
- **zebrad 7.0.0 serves light wallets itself, and follows NU7.** Its
  `[rpc] lightwalletd_listen_addr` starts a gRPC server inside zebrad that implements the
  lightwalletd protocol, in plaintext and without authentication, and that upstream calls
  experimental. So the first phase serves wallets from it, on loopback behind Zeal's TLS front
  door, with no extra binary; zingod joins it as a second backend once it passes Zeal's tests:
  Testnet first, then Mainnet.
- **zeeder can't run beside a zebrad on its network's default port.** Its crawler always
  binds `[::]:8233` (Testnet `[::]:18233`), with no setting, and zebra-network panics when the
  port is taken (`ZcashNetwork::network_config`, zeeder `src/config.rs` at v1.8.0). Until
  zeeder has a setting for that listener, a public seeder goes through HERD.md's 8233 dialog.

Both services share one guided flow for what they have in common: the name, the router, the
firewall and the checks from outside.

## PS1 as built

PS1's light client server landed as this design describes it, on Zeal as it is today (one
supervised zebrad, today's `launcher.json`), Linux only and labelled experimental.
[ARCHITECTURE.md](ARCHITECTURE.md#public-services) describes the code.

**Built:**

- zebrad's endpoint as a managed loopback key (`config::LightwalletdKey`): written while the
  service is on, removed while it is off (`ensure` can now remove a key), the user's while
  nothing is set up; `validate` still refuses any non-loopback value. The app restarts zebrad
  behind its permits; the `Opened lightwalletd gRPC endpoint at` line and the bind failure are
  read from fixtures captured from zebrad 7.0.0; the light monitor runs Zodl's check every 10
  minutes; a network change pauses the service and removes the key for zebrad's next start.
- Front door v1 (`launcher-gateway`) in Zeal's process, on its own runtime and thread: the four
  modes, the SNI and ALPN checks, TLS-ALPN-01 answers, the v1 limits (128 connections, a full
  front door closing the largest holder's oldest one for a newcomer, 8 per IPv4 address or
  IPv6 /64, 2 new a second with a burst of 16, a 10 s handshake, 10 minutes
  idle, a 2 hour age, the upload budget), closing at once on pause and at quit.
- The `AcmeActor`: instant-acme with ring, rcgen and x509-parser, TLS-ALPN-01, staging then
  production, verification with the platform verifier, renewal at two thirds of the lifetime,
  the attempts budget; tested against Pebble in CI.
- The name step with DuckDNS and names kept by hand, the `DdnsActor`, the token in the OS
  keychain (`keyring` 4.2).
- The read-only network checks (default route, VPN, NAT-PMP's external address) and "Check my
  connection"; the forward by hand with generic steps; the firewall commands for ufw and
  firewalld, shown and never run; the test on this computer and the hairpin test.
- Open, pause and resume, "Turn off and forget" with what stays public, resume at launch,
  keep-awake (`systemd-inhibit`), the monthly cap; the spec out of reach of `set_settings`.
- The types (`launcher_types::public`), the pure rules (`launcher-herd`), and the screens:
  consent, setup, the status panel with the Share strings, the banners; in Settings → Public
  services, with a card on the node's dashboard and the banner on the node's pages, all in the
  mock (`?public=setup`, `serving`, `unconfirmed`, `cgnat`, `expiring`, `keychain`, `consent`).

**Moved to a point release:** deSEC; ARI (`renewal_info`, `replaces`); "Test now" and the phone
test (the setup's staging validation is the evidence from outside meanwhile); UPnP IGD's
`GetExternalIPAddress` and the router's model for vendor guides (the guide is generic); the
authoritative-servers, TTL and CAA lookups through hickory-resolver (the name is checked
through the system resolver, and a CAA refusal comes back from Let's Encrypt with its fix);
the 0600-file token store as a user's choice (the store exists, the UI does not offer it);
Configure showing the key read-only (an edit is still overridden); the QR code; the tray
items; the zebrad's Public tab (the dashboard card and Settings stand in); the wallet runs,
which gate the release on each network.

**Where the code differs from this design:**

- `grpc.rs` is in `launcher-public`, not `launcher-core`, and decodes the few protobuf messages
  by hand without prost; its decoder is pinned to `LightdInfo` bytes captured from zebrad.
- The front door's `Unavailable` answers with h2's server directly, not hyper's, with hyper's
  limits set by hand: 16 streams a connection and a 16 KiB header list.
- The network checks read the routes from `/proc/net/route` and send NAT-PMP's two-byte
  external-address request themselves: no netdev or crab_nat in PS1. The router is the
  default route with a gateway and the lowest metric. The VPN check looks up the main table's
  route to an internet address as the kernel does: the longest prefix, so a VPN's 0.0.0.0/1
  and 128.0.0.0/1 win over the default, then the lowest metric, with a gateway or without.
  Policy rules, which the main table does not show (wg-quick's own table, a Tailscale exit
  node), are seen from the address the kernel picks towards that internet address, by a UDP
  `connect` that sends nothing: one on another interface than the router's, named by
  `getifaddrs` (nix), is the VPN.
- Consent given at step 1, before a name exists, binds to the first name chosen; another name
  asks again.
- A network change waits for the user's Resume even after switching back (the pause holds),
  and removes the key without restarting zebrad (`EndpointHost::apply(node, port, restart)`).
- The commands take no node in PS1, which has one light client server; the UI reads the steps
  from the status instead of a `public_plan` command, and the upload preset is a step answer
  instead of `set_public_limits`.
- The bind failure's text is confirmed: zebrad logs `shutting down Zebra due to an error
  error=Address already in use (os error 98) report=` at ERROR on stdout and exits 1.

## Goals and non-goals

### Goals

- Two optional services, each off until the user turns it on, each with one switch and a
  guided setup that checks every step and shows the fix first when a step fails.
- Only the service is reachable. zebrad's light-wallet endpoint, zingod, zebrad's RPC and
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

zingod, bundled on Linux, can take zebrad's place behind the same front door once it passes
Zeal's tests ([zingod as the backend](#zingod-as-the-backend)); wallets keep the name, the port
and the certificate. PS1 ships the first part on Linux; zingod, front door v2, macOS and Windows
follow ([Phases](#phases)).

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

With zingod as the backend, its port also answers HTTP/1.1, for `/status` and `/readyz` only
([The front door for zingod](#the-front-door-for-zingod)).

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
  The off also holds in the service actor's memory, so if `launcher.json` cannot be written the
  front door stays closed, the endpoint key is removed all the same (the actor gives the app
  the endpoint it decided on, not the stored port) and the reply says the off was not saved;
  the actor clears the stored port once it can, and a launch writes the key from the settings
  it prepared, saved or not.
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
  (`zebrad/src/components/tokio.rs:101-106`). Zeal's classifier found `PortInUse` in panics
  only (`logs.rs:285-295`), so it gained a rule for this line, from a captured fixture
  (`error=Address already in use (os error 98)`, exit code 1). Zeal also checks the port when it writes the key.
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
- **the methods wallets call**, all of which zebrad 7.0.0's endpoint serves, as do Zaino
  0.10.2 and zingod 1.1.0: `GetLightdInfo`, `GetLatestBlock`, `GetBlockRange`,
  `GetTreeState`, `GetSubtreeRoots`, `GetTransaction`, `SendTransaction`, `GetMempoolStream`,
  `GetAddressUtxosStream`, `GetTaddressTxids` or `GetTaddressTransactions`.

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

### Zaino

Zaino was the backend this design planned (PS5). No Zaino release follows NU7: every release up
to 0.10.2 stops at the block before its activation (zaino#1731). Zingo Labs, who made Zaino, no
longer work on it; they forked it as zingod, which follows NU7, so Zeal bundles zingod instead
([below](#zingod-as-the-backend)). Much of the Zaino plan carries over: the upgrade gate, the
restart with the backing zebrad, the exit classes, the stall rule and the loopback-only rules. A
zainod the user picks stays possible as a program of MULTI-NODE.md's Zaino kind, for this
computer only. The Zaino facts used here were checked at tag `zainod-0.10.2` (`5ced0ae8`) on
2026-11-03.

### zingod as the backend

> **Proposal, 2026-11-16.** Facts checked at zingod tag `v1.1.0` (commit
> `bfce8b2b433578054dec82928d2707e523ec75b7`), Zebra `v7.0.0` and Zeal `main` at `95a0b5b`.
> Paths under `packages/` and `docs/` in a zingod citation are zingod's. A "research run" is a
> run of zingod 1.1.0, built from that tag, during this design. "Unconfirmed" marks what the
> phase that relies on it checks first; "proposal" marks a starting value.

Zingo Labs forked Zaino as [zingod](https://github.com/zingolabs/zingod), "the Zingo Labs
lightclient indexer" (its README; Apache-2.0), announced on the Zcash forum on 2026-11-06.
zingod 1.1.0 (2026-11-10) follows NU7. Zeal bundles it on Linux as `zeal-zingod`
([SIDECARS.md](SIDECARS.md#the-bundled-zingod-linux)) and offers it as the light client
server's second backend, beside zebrad's endpoint. The user picks one on the consent screen; the
front door, the name, the port and the certificate are the same for both.

```text
phone wallet ──TLS, ALPN h2──▶ router :443 ──▶ [::]:8443      the front door, unchanged
                                                 ▼ the same HTTP/2 frames, plaintext, on loopback
                                       zingod 127.0.0.1:18137     zeal-zingod, run by the light client server
                                                 │ JSON-RPC every second, zebrad's cookie, loopback
                                                 ▼
                                       zebrad 127.0.0.1:18232     the registry's first node, the whole chain
  zingod's index: <that node's data folder>/zingod/testnet/     about 71 GB on Mainnet
```

**Not a node.** zingod isn't in the registry, the sidebar or `MAX_NODES`, and has no Configure
page. The light client server's actor owns it, as it owns the front door, and runs it only while
the service is on with zingod chosen. One owner keeps the order local: the front door goes
`Unavailable` before zingod stops, and opens only once zingod serves. MULTI-NODE.md's kind
adapter doesn't exist yet; zingod can become a node kind when it does (phase 5), reusing the
pieces below.

#### Choosing the backend

| | Zebra's built-in server (PS1) | zingod 1.1.0 |
| --- | --- | --- |
| Serves | once zebrad is at the tip | once its index reaches zebrad's tip: "about an hour on NVMe" for a first build (its README), unmeasured on a desktop; its Mainnet live test allows 48 h |
| Disk beside zebrad's | none | 71.3 GB on Mainnet at height 3,544,662 (Zingo Labs' snapshot manifest, read 2026-11-16); Testnet unmeasured |
| Protocol | v0.3.6 with Ironwood's fields: no transparent compact data, no `GetStandardFee`, an empty `lightwalletProtocolVersion` | v0.6.0: transparent `poolTypes`, `vin` and `vout`, `CompactTx.fee`, `GetStandardFee`, `upgradeName` and `upgradeHeight` |
| Zingo wallets | zingolib's pepper-sync refuses a server whose `lightwalletProtocolVersion` is empty or below v0.5.0 (`crates/pepper-sync/src/client.rs` at `zingolib_v8.0.0` and at `dev`, `0a39640`); which zingolib Zingo mobile 3.0.0 and Zingo PC 2.0.27 ship is unconfirmed | pass that check |
| Not served | `Ping` | `Ping`, `GetBlockNullifiers` (removed from its proto), gRPC reflection |
| Limits of its own | 20 streams a connection | connections, streams, read lanes, request sizes, timeouts, connection age ([below](#the-front-door-for-zingod)) |
| Upstream says | experimental | "In development, not for production mainnet use", logged at every start (`packages/zingod/src/daemon.rs:80`) |
| Platforms | Linux (PS1) | Linux; macOS never built; Windows doesn't compile at 1.1.0 |

**The default stays Zebra's built-in server**, one constant in launcher-herd
(`DEFAULT_BACKEND`): it serves as soon as zebrad is at the tip, needs no extra disk and has
PS1's tests behind it. Zingo wallets are the reason to pick zingod, and its card says so once the
wallet runs confirm it. Which backend new setups get once Mainnet opens is the maintainer's call
(question 19). A light client server set up before this change keeps zebrad's endpoint: a spec
without `backend` reads as `ZebradEndpoint`.

#### What can go wrong, and what the user sees

This table is the design; the sections after it say how each line works. Each row has a mock
flag (`?public=<flag>`).

| What goes wrong | How Zeal knows | What Zeal does | The status panel says | Comes back | Flag |
| --- | --- | --- | --- | --- | --- |
| The index is being built: the first start, or after Rebuild | `/status` is `syncing` | `IndexBuilding`; the test on this computer and Open wait | "Building zingod's index: block 1,204,331 of 4,612,000 (26 %), about 5 h left" | by itself, once zingod serves within 2 blocks of Zebra | `zingod-building` |
| zebrad restarts: an update, a configuration change, a crash | zebrad's pid or start time changes | stops zingod; starts it once the new run's RPC is open, its cookie written and zebrad at the tip | "Catching up · zingod restarts with Zebra" | by itself | `zingod-restarting` |
| zebrad isn't at the tip yet in this run | zebrad's height and its estimate of the tip | doesn't start zingod | "Waiting for Zebra · Testnet to reach the tip: zingod starts then" | by itself | `zingod-waiting` |
| zebrad stops, fails or halts | zebrad's phase | stops zingod; PS1's `BackingStopped` pause | PS1's line | when zebrad runs again | `zingod-nozebra` |
| zingod can't reach zebrad | `/status` `chain.validators.answering` is 0 for 30 s | `IndexerDown`; restarts zingod once per zebrad run | "Catching up · zingod can't reach Zebra · Testnet" | by itself | `zingod-unreachable` |
| zingod reports `down`, or doesn't answer | `/status`, or three reads without an answer | `IndexerDown`, with zingod's reason | "zingod is down: <zingod's message>" | by itself, or as a crash | `zingod-down` |
| zingod falls behind | its served height against zebrad's | `IndexBehind` | "Catching up · zingod is 12 blocks behind Zebra" | by itself, within 2 | `zingod-behind` |
| zingod stalls | the stall rule | restarts it once, then `IndexerStuck` | "zingod is stuck at block 4,611,002; Zebra is at 4,611,019. [Open the log]" | by itself, when it moves | `zingod-stuck` |
| zingod crashes | `Crash` | restarts with backoff; `IndexerFailed` after 5 crashes in 10 minutes | "zingod stopped unexpectedly 5 times in 10 minutes. [Open the log] [Try again]" | the user's Try again | `zingod-crashloop` |
| zingod refuses Zeal's configuration | `ConfigError` | no restart | "zingod refused the settings Zeal wrote. This is a bug in Zeal: [Copy diagnostics]" | a Zeal update | `zingod-config` |
| zingod's port is taken | the check before the spawn, or `PortInUse` | moves to the next free port, once | "Other programs use ports 18137 and 18138. [Try again]" | the user | `zingod-port` |
| another program holds the index | `DbLocked`, after the pid file ended Zeal's own leftover | waits | "Another program holds zingod's index in /mnt/zebra/zingod/testnet. [Try again]" | the user | `zingod-locked` |
| the disk fills | free space every 60 s; `DiskFull` | stops zingod below 10 GB free, starts it above 20 GB | "Waiting for disk space: 6 GB free on /mnt/zebra. zingod starts again above 20 GB." | by itself | `zingod-disk` |
| the index is damaged, from another chain or of an older format | `IndexBroken` | no restart | "zingod's index is damaged. Rebuilding it takes hours, and wallets are told to try again meanwhile. [Rebuild]" | the user's Rebuild | `zingod-broken` |
| zingod serves a block zebrad doesn't have | the tip cross-check | stops zingod | "zingod's chain doesn't match Zebra's at block 4,611,980. [Rebuild]" | the user's Rebuild | `zingod-mismatch` |
| an upgrade zingod doesn't know is active | zebrad's upgrades against `ZINGOD_KNOWS` | doesn't start zingod; `BuildTooOld` | "zingod 1.1.0 doesn't know NU8, which is active. Wallets that use your server see an error until Zeal updates zingod." | a Zeal update | `zingod-tooold` |
| an upgrade zingod doesn't know is scheduled (Z4) | the same | no opening within 7 days of it; a countdown from 14 days; `BuildTooOld` at activation minus one | "zingod 1.1.0 doesn't know NU8, which activates at block 4,700,000 in about 5 days. From then until Zeal updates zingod, wallets that use your server see an error." | a Zeal update | `zingod-upgrade` |
| an untested pair, or Mainnet before Z4 | `TESTED_ZINGOD` | the choice is disabled; a spec that has it is `NotAvailable` | "zingod is being tested on Testnet first" | a Zeal update | `zingod-mainnet` |
| no `zeal-zingod` on this system | `Options.zingod_binary` is `None` | the choice is disabled | "zingod runs on Linux only for now" | — | `zingod-absent` |

A failed wallet check stays PS1's `WalletCheckFailed`. Two lines close the front door: PS1's
`BackingStopped` and the pause for an upgrade zingod doesn't know (`gateway_mode` closes it for
every `Paused`). Every other zingod problem is `Degraded`, so the front door stays up in
`Unavailable` and TLS-ALPN-01 renewals go on through a long build, stall or rebuild.

#### Which zebrad zingod reads

PS1's node: the registry's first node (`RegistryHandle::follow_first`), on the service's
network. zingod fetches every block from genesis from its validator and answers `GetTransaction`
with the validator's `getrawtransaction` (`docs/running.md:112-120`;
`packages/zingod-grpc/src/routes/transactions.rs:64-88`), so the validator must keep the whole
chain; every zebrad 7 does (HERD.md). zingod reads zebrad's managed RPC: the address zebrad
logged for this run (`Status.rpc_addr`) and the cookie at `NodePaths::rpc_cookie_file()`. Zeal
always writes `cookie_path`, so zingod's own lookup (`<cache dir>/zebra/.cookie`) never runs.

- **Tested pairs.** `TESTED_ZINGOD` in launcher-herd lists each zingod version with the zebrad
  versions and networks it passed with: in Z3, 1.1.0 with zebrad 7.0.0 on Testnet; Z4 adds
  Mainnet once its gate passes. Any other pair is `NotAvailable::UntestedZingod`. A
  launcher-herd test checks that the bundled `ZINGOD_VERSION` is on the list.
- **One network, one source.** zingod doesn't check its validator's network: in a research run,
  set to `network = "mainnet"` against a Regtest zebrad, it anchored on the Regtest genesis and
  reported `serving` with `chain.network` `mainnet`. So Zeal writes zingod's `network`, the
  validator's address and its cookie from the same `NodeSpec`, starts zingod only after zebrad's
  `getblockchaininfo` chain matches the service's network, and cross-checks the served tip while
  it runs ([Health of zingod](#health-of-zingod)).

#### When zingod runs

A pure rule, `public::zingod::want(facts)` in launcher-herd, says whether zingod should run;
the runner brings the process in line at every change and every 10 s. zingod runs when all of
these hold:

1. The service is on, its consent covers zingod, zingod is the chosen backend, the pair is
   tested, `zeal-zingod` is bundled, and no pause waits for the user.
2. zebrad runs on the service's network, and this run has logged its RPC endpoint
   (`Status.rpc_addr`).
3. zebrad's cookie exists, isn't empty, and was written at or after this run's `started_at`.
4. zebrad has been at the tip, within 2 blocks, once in this run.
5. No zingod failure waits for the user: a refused configuration, a damaged index, a chain
   mismatch, a crash loop, a taken port or a held lock.
6. The index's disk has more than the low mark free, and the upgrade gate allows it.

**One zingod run per zebrad run.** zebrad writes a new cookie at every start. zingod reads its
cookie at the first request and again only after a 401 or 403
(`packages/zingod-source/src/endpoint.rs:98-160`). zebrad 7.0.0, and Zeal's Beta (`9889683`)
and Nightly (`7ae1a82`) pins, close the connection on a wrong cookie without an HTTP answer
(`zebra-rpc/src/server/http_request_compatibility.rs:225-227` at `v7.0.0`). In a research run
against 7.0.0, zingod's polls failed for the rest of the run once zebrad came back; against a
zebrad that answers 401, zingod recovered in 7 s. The 401 is Zebra commit `a1bf8a7e89`, first
released in 7.1.0. So when zebrad's pid or start time changes, Zeal stops zingod and starts it
again under the rule above. The rule stays after a 7.1.0 pin: zebrad's restart already made the
server unavailable, so the extra restart costs little.

**Why the RPC line.** zebrad 7.0.0 logs `RPC auth cookie written to disk` just before
`Opened RPC endpoint at` (fixtures `zebrad-7.0.0-lightwalletd-start-shutdown.stdout.log`, lines
56-57, and `zebrad-7.0.0-regtest-start-shutdown.stdout.log`, lines 57-58), and the supervisor
clears `Status.rpc_addr` at every spawn (`crates/launcher-core/src/supervisor/actor.rs:532,
779`). So once `rpc_addr` is set, this run's cookie is on disk; the time check is the backstop. A
zingod started on the previous run's cookie would never recover.

**Why the tip.** zingod can index only what zebrad has, and a first build beside a syncing zebrad
doubles the heavy work. The wait applies after every zebrad restart too, where it is usually
short; the panel and zebrad's Stop and Restart say so.

| When | zingod |
| --- | --- |
| zebrad's run changes | stopped; started again under the rule |
| zebrad stops, fails or halts | stopped; PS1's `BackingStopped` pause |
| switched off, a pause only the user ends, a network change, "Turn off and forget", the other backend chosen | stopped; the index stays |
| any other automatic pause (the name, the LAN address, a VPN, the cap) | keeps running, so a resume serves at once, as PS1 keeps zebrad's endpoint |
| less than 10 GB free on the index's disk | stopped with `SIGTERM`, so the indexes commit; started again above 20 GB |
| zingod exits on its own | [zingod's exits](#zingods-exits) |
| Zeal quits | the front door closes first, within its 5 s cap (`src-tauri/src/exit.rs:108-111`); then zingod's stop runs beside the nodes', under the larger of the registry's cap and zingod's |

#### How Zeal runs zingod

- **The supervisor.** launcher-core's supervisor runs zingod as it runs zebrad: the spawner
  thread with the process group and `PR_SET_PDEATHSIG(SIGTERM)`, the pid file with its executable
  check (`kill_stale` ends a `zeal-zingod` an earlier Zeal left), the log ring and the rotating
  file, the restart policy, the check of the port before the spawn and the process-tree probe.
  `SupervisorConfig.command` becomes `Launch { Zebrad(ZebradCommand), Zingod(ZingodCommand) }`,
  with `From<ZebradCommand>`, so today's callers don't change. The four places in
  `supervisor/actor.rs` that know only zebrad (the detector, the line parser, the exit
  classifier, and `Running` at `Starting zebrad`) go through a reader that `Launch` picks, and
  the actor's notes name the program ("zingod started (pid N)"). The change is made in place: no
  code moves, and zebrad's tests stay as they are.
- **The command.** `zeal-zingod -c <config> start` (`-c` is global,
  `packages/zingod/src/cli.rs:15-54`), with stdin null and nice 10, as this document planned for
  Zaino: zebrad gets the CPU first during the build, when zingod otherwise uses every core (rayon,
  merges paced at 200 MiB/s).
- **The environment is cleared**, an exception to ARCHITECTURE.md's "never `env_clear`", as
  WALLET.md makes for Zallet. That rule keeps `PATH`, `HOME`, `SystemRoot` and `LOCALAPPDATA`
  for zebrad on Windows; zingod runs on Linux only, and with `[snapshot]` off it starts no
  program, so it needs no `PATH`. Zeal sets only `HOME`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME` and
  `XDG_CACHE_HOME`, under `<index dir>/home`, and `RUST_LOG=zingod=info`. That keeps out:
  - `ZINGOD_CONFIG_*`, which zingod matches without regard to case, which win over the file, and
    of which an unknown one fails the load (`packages/zingod/src/config.rs:732-752`);
  - a malformed `RUST_LOG`, which makes zingod exit 2;
  - `NOTIFY_SOCKET`, so zingod never tells a service manager Zeal runs under that it is ready;
  - allocator settings.

  With every path set, zingod wrote nothing under `HOME` in the research runs.
- **Running** starts at zingod's `Listening` line with `component=Grpc`
  (`packages/zingod/src/daemon.rs:161`), never `component=Admin`. By then the configuration has
  loaded, the open-file limit was raised and the port is bound; boot comes after. The line's
  `endpoint` becomes the supervisor's `Status.lightwalletd_addr`, the field the front door and the
  light monitor already follow.
- **Stop.** On `SIGTERM` zingod closes its listener, sends GOAWAY, waits up to `[grpc]
  shutdown_grace_secs` (10 s) for open connections, commits its indexes and exits 0. Measured:
  0.05 s with no connection, 10 s with one idle h2c connection. A second `SIGTERM` doesn't
  shorten the drain (measured, despite the comment at `daemon.rs:526`), so Zeal sends one. The
  stop grace is 30 s (proposal), then `SIGKILL` to the process group. A killed zingod reopens its
  indexes by checking each file's length, tail page and `.crc` (`docs/running.md:291-295`).
- **Restarts** after a `Crash` use the supervisor's backoff: 1 s doubling to 60 s, and failed
  after 5 crashes in 10 minutes. zingod never restarts inside its process, and expects a
  supervisor to (`packages/zingod/src/main.rs:90-93`).

| What | Where |
| --- | --- |
| Configuration | `<app data>/public/zingod-<network>.toml`, written whole before every start |
| Pid file | `<app data>/public/zingod-<network>.pid` (zingod writes none) |
| Log | `<log dir>/zingod-<network>.log`, rotated like `zebrad.log`, never mixed into zebrad's |
| Index | `<index dir>/indexes/`: `compact_block`, `block_hash`, `tree_state`, `transparent_address` and `txid_location`, with `utxo_set` beside `compact_block` |
| State | `<index dir>/state`, for zingod's hold and overload markers, which Zeal doesn't use |
| Home | `<index dir>/home`, empty |
| Binary | `zeal-zingod` beside Zeal; in an AppImage, copied to `<app data>/sidecar/zingod/<Zeal version>/` |

`<index dir>` is `<the first node's data folder>/zingod/<network>`: the disk the user picked for
chain data, and zingod asks for local storage beside its validator. The spec records it when
zingod is first chosen (`ZingodSpec.dir`), so a later data-folder change doesn't orphan the index,
and the panel names it. The network is in the path because an index built for one network refuses
to open on another.

#### The managed zingod.toml

Every table of zingod's configuration refuses unknown keys (`deny_unknown_fields`), and one
unknown key fails the load. So Zeal writes only keys the pinned version has, sets every path,
and rewrites the file before every start. A test checks that every key Zeal writes is in
`generate-config` output captured from the pin, or on a short list of keys that the pin reads
but doesn't print, because they default to unset. At 1.1.0 that list holds only
`[[trusted_validators]] cookie_path` (`packages/zingod/src/config.rs:89`, default `None` at
`:114`); each bump checks the list again. Another test checks that the Mainnet and Testnet files
always set `cookie_path` to `NodePaths::rpc_cookie_file()`. Without it, a loopback validator gets
zingod's default lookup (`config.rs:145-148`), `<cache dir>/zebra/.cookie`, which under Zeal's
`XDG_CACHE_HOME` doesn't exist, so zingod would send no credentials and log no error that names
the cause. Two keys whose values are today's defaults, `validator_client` and
`[p2p] enabled`, are written anyway, so a changed default can't turn their features on. In
research runs this file without those two, `max_streams_per_connection` and `[grpc.overload]`
loaded, booted and stopped with exit 0, with Mainnet and with Testnet values; all four are keys
of 1.1.0's `generate-config` output, and the contract test boots the whole file. For Testnet:

```toml
# Managed by Zeal: rewritten before every start.
network = "testnet"                       # from the serving node's spec; lowercase ("Testnet" is refused)
validator_client = "legacy_rpc"           # JSON-RPC; "grpc" needs Zingo's zebra fork

[[trusted_validators]]
jsonrpc_address = "127.0.0.1:18232"       # the RPC address zebrad logged for this run
cookie_path = "<node data>/rpc/.cookie"   # NodePaths::rpc_cookie_file()

[serve]
grpc_listen_address = "127.0.0.1:18137"   # ZingodSpec.port; loopback with no tls key: plaintext h2c
state_dir = "<index dir>/state"

[grpc]
max_connections = 256                     # every front-door client is 127.0.0.1 to zingod,
max_connections_per_ip = 256              # so the per-address cap is the total
max_streams_per_connection = 16           # zingod's default is 100; front door v2's planned 16

[grpc.overload]
cpu_pressure_percent = 0                  # off: zingod reads its cgroup's PSI, which it likely
io_pressure_percent = 0                   # shares with zebrad and Zeal (unconfirmed)

[p2p]
enabled = false

[index.compact_block]
path = "<index dir>/indexes/compact_block"
# and the same for block_hash, tree_state, transparent_address and txid_location
```

- **Mainnet**: `network = "mainnet"`, the node's RPC port (8232 for the first node) and port
  8137.
- **Ports**: 8137 on Mainnet and 18137 on Testnet (proposal; zingod's own default is 8137), or the
  next free one above, checked when written and kept in `ZingodSpec.port`. They differ from
  zebrad's endpoint ports (9067, 19067), so both can listen during Z4's switch.
- **Open files**: zingod raises its soft limit to the hard one, and refuses to start unless
  `max_connections` plus 1,024 fits: 1,280 here, 5,120 with its default
  (`packages/zingod/src/daemon/fd_limit.rs`).
- **All five indexes on**: between them, librustzcash and pepper-sync call every indexed method
  (`docs/client-requirements.md`); with an index off, its methods answer `UNIMPLEMENTED`.
- **Left out, and so off:**
  - `[serve] tls`: unset keeps zingod's loopback-only rule for plaintext. `"off"` would allow
    plaintext on any address, and `self_signed` or certificate files would break forwarding,
    since the front door speaks plaintext to its upstream.
  - `trusted_proxies`: front door v1 sends no PROXY header.
  - `[metrics]`: the admin listener has no authentication, its metrics name the validator, and
    zingod's example puts it on 127.0.0.1:9999, zebrad's metrics port.
  - `[snapshot]`: it downloads indexes from Zingo Labs' server through `aria2c` from `PATH`.
  - `indexer_address`: zebrad's push streams. The bundled zebrad 7.0.0 has the indexer gRPC
    server, which stays off until `rpc.indexer_listen_addr` is set: it logs "configure an
    indexer_listen_addr to start the indexer RPC server" (fixture
    `zebrad-7.0.0-lightwalletd-start-shutdown.stdout.log`, line 58), and
    `zebrad/src/commands/start.rs:557-573` starts it with no feature gate (WALLET.md, "What
    `indexer` does"). MULTI-NODE.md's `indexer` feature is zebra-state's spending index, which
    Zallet needs, not this server. Leaving the key out is a choice, not an unknown: it would
    cost one more loopback port and managed key, and zingod polls every second without it.
  - the validator's user and password.
- **Regtest**, for the contract test only: `network = "regtest"`. There zingod's boot waits for
  the validator's first answer, and only there may `[sync] finalised_depth` go below 1,000
  (`packages/zingod/src/config.rs:436-440`). A test checks that the Mainnet and Testnet files
  never carry a Regtest-only value.

#### zingod's exits

zingod prints `Error: <message>` on stderr and exits 1 for a start failure, exits 2 for a
malformed `RUST_LOG` or a runtime that couldn't start, and aborts (SIGABRT) on any panic
(`packages/zingod/src/main.rs:33-56`). Its reader (`launcher-core/src/zingod/logs.rs`)
classifies the last lines:

| zingod's output | Class | Then |
| --- | --- | --- |
| `Error: configuration error: …` (exit 1); `Error: RUST_LOG: …` or `Error: tokio runtime: …` (exit 2) | `ConfigError` | no restart: zingod refused what Zeal wrote, a bug in Zeal |
| `… grpc.max_connections = 256 needs 1280 open files … and the limit is …` | `ConfigError` | no restart; the panel names the limit to raise |
| `Error: binding the gRPC listener: Address already in use (os error 98)` | `PortInUse` | the next free port, once; then the user |
| `… <dir>/LOCK is held by another process` | `DbLocked` | the user: the pid file already ended Zeal's own leftover |
| `<index> index commit failed: disk <dir> full`, a panic, then an abort | `DiskFull` (new, deterministic) | waits for space ([Disk](#disk-memory-and-the-first-build)) |
| an index that refuses to open (another network or format, a bad tail page or `.crc`), a page checksum mismatch, or `… (resync required)` (`FollowError::Diverged`, `packages/zingod-sync/src/follower.rs:33-36`) | `IndexBroken` (new, deterministic) | the user's Rebuild |
| `Error: cannot read validator cookie …` | `Crash` | backoff: the cookie may be mid-write |
| anything else: exit 1 or 2, a signal, another panic | `Crash` | backoff |

- The configuration, port, lock and cookie texts were captured from the binary in research runs.
- The disk-full text comes from the code (`packages/zingod-persistence/src/error.rs:51`;
  `packages/zingod-sync/usage.md:117`) and stays unconfirmed until captured.
- How `Diverged` reaches stderr wasn't reproduced, so the rule matches `(resync required)`
  anywhere in the last lines.
- The contract test captures the corruption texts by flipping a byte in a tail page.
- zebrad's classifier doesn't change.

#### Health of zingod

zingod answers `GET /status` and `GET /readyz` on its gRPC port, over HTTP/1.1 or h2, from
process start, 10 at once per connection and then one a second
(`packages/zingod-grpc/src/health.rs`). While zingod runs, the runner reads `/status` every 5 s,
one request per new HTTP/1.1 connection with a 2 s timeout, through hyper's client, already a
launcher-public dependency. It keeps the answer as an `IndexerStatus` (the state, the detail,
`chain.network`, `chain.tip`, `chain.served`, the validator counts, the alarms and
`build.version`), parsed with unknown fields ignored and pinned to captured answers. `/readyz`
answers 503 through a whole first sync, so Zeal doesn't read it.

| zingod says | Zeal |
| --- | --- |
| `provisioning` | `IndexerStarting` |
| `syncing`, `detail {height, target, blocks_per_sec}` | `IndexBuilding { served, target, eta_secs }`; the panel says "Catching up" once fewer than 1,000 blocks remain (proposal) |
| `serving`, `degraded` | ready, under the rules below; zingod's `degraded` (a fresh tip no validator vouches for) keeps serving, as zingod intends |
| `down`, `detail {reason, message}` | `IndexerDown`, with zingod's reason and message |
| `held` | `IndexerDown`: Zeal never runs `zingod hold` |
| no answer, three reads in a row | `IndexerDown`, "zingod isn't answering" |

- **Can't reach zebrad**: `chain.validators.answering` is 0 for 30 s. zingod's own state can't
  say this in time: a booted zingod stays `serving` until its tip is stale, 24 target spacings,
  about 10 minutes at NU7's 25 s, and only then reports `tip_unvouched`
  (`docs/architecture.md:327-336`). `no_validator_reachable` appears only while zingod starts and
  waits for a validator (`readiness` in `packages/zingod/src/health.rs`), which it doesn't do on
  Mainnet or Testnet. The documented reason `tip_stale` doesn't exist in the code, so the panel
  shows zingod's own message for any reason it doesn't know. Zeal restarts zingod once per zebrad
  run on this, since a missed cookie change looks the same.
- **Behind**: once ready, zingod is behind under `BackingBehind`'s constants. `served` 10 or more
  blocks below zebrad's height at once, or more than 2 for 10 minutes, is `IndexBehind`. zingod
  polls every second, so at the tip it should trail by about a block (unmeasured; the wallet runs
  record it).
- **The stall rule** (pure): `served` unchanged for 5 minutes while zebrad is 4 or more blocks
  ahead, or 15 minutes during a build, since the indexes publish in steps. Zeal restarts zingod
  once; if it stays stuck, `IndexerStuck` holds until it moves. An upgrade zingod doesn't know
  would look like this ([Upgrades of zingod](#upgrades-of-zingod)).
- **The tip cross-check**: at most once a minute while zingod is ready, the light monitor calls
  `GetLatestBlock` on loopback and compares the hash at the served height (sent in internal byte
  order, so reversed) with zebrad's `getblockhash` at that height, asked through launcher-core's
  RPC client with zebrad's cookie. Three different answers in a
  row, spanning at least 3 minutes, are `ChainMismatch`: zingod stops and Rebuild is offered. A
  reorg clears sooner, and a height zebrad doesn't have yet is skipped. `/status` `chain.tip`
  can't do this job: it is the tip the validator vouches for, so it always equals zebrad's. The
  same-spec rule prevents the known cause, and zingod's own `Diverged` exit catches an index that
  no longer extends the chain; the cross-check is the cheap backstop.
- **The wallet check** is unchanged: Zodl's chain name, Sapling height and branch id at zingod's
  `blockHeight`, and `blockHeight` within 2 of zebrad's height. zingod takes its schedule from its
  `zcash_protocol` fork (`605917a3`; `packages/zingod-primitives/src/protocol.rs:26-38`), whose
  heights and branch ids equal launcher-herd's `BRANCH_IDS`, `MAINNET_HEIGHTS` and
  `TESTNET_HEIGHTS`. The light monitor follows zingod's endpoint only once `/status` says ready;
  before that `IndexReady` is the failing check, and the test on this computer and Open wait.
- **What opens the front door**: `serving` or `degraded`, within 2 blocks of zebrad, with the
  wallet check and the cross-check clean. Everything else is `Unavailable`.

#### The front door for zingod

Unchanged. Its upstream is `127.0.0.1:<ZingodSpec.port>`. zingod reads the first bytes
(hyper-util's `auto` builder), sees the HTTP/2 preface the front door forwards and serves h2c; in
plaintext it checks neither `:authority` nor ALPN. Its pings every 30 s keep a live connection
from tripping the front door's 10 minute idle timer, and its GOAWAYs after 30 minutes of age or
idleness come before the front door's 2 hour cut. This is read from the code; the contract test
confirms it through the front door.

**v1 is enough for zingod** (open question 17, answered again). The reasons Zaino 0.10.2 needed
v2 don't hold for zingod 1.1.0:

- it pins h2 0.4.20 (`Cargo.lock:1554-1555`), past 0.4.16, which fixed RUSTSEC-2026-0258;
- its port serves only `CompactTxStreamer`, `GET /status` and `GET /readyz`: no reflection, no
  extension service, no gRPC-web;
- it sets limits of its own (`packages/zingod-grpc/src/limits.rs`):

| | zebrad 7.0.0's endpoint | zingod 1.1.0 as Zeal writes it |
| --- | --- | --- |
| Connections | none | 256 |
| Streams per connection | 20 | 16 |
| Request bodies | 4 MiB | 64 KiB; `SendTransaction` 2 MB + 1 KiB; all within 30 s |
| Reads | nothing per method | 64 point, 32 range and 4 scan reads at once; adaptive work streams |
| Stalled clients | 60 s, then the stream ends `OK` | 300 s unread closes the connection; 300 s silent ends the stream `UNAVAILABLE` |
| Connection age | none (the front door's 2 h) | GOAWAY at 30 minutes ±10 %, with 10 minutes' grace; the same after 30 minutes idle |
| Deadlines | none | `grpc-timeout` honoured |

What v1 still can't do:

- **Tell one client from another.** Every client reaches zingod as 127.0.0.1, so the front door's
  per-address caps (8 connections, 2 new a second, a burst of 16) are the only per-client limits.
  One address can take the 32 range lanes with long `GetBlockRange` calls and slow everyone else:
  lanes queue, they don't refuse. Front door v2's per-address method limits (PS2) close that
  gap. zingod's PROXY protocol isn't used: trusting 127.0.0.1 would make every loopback caller,
  Zeal's own checks included, send a header within 5 s or be dropped
  (`packages/zingod-grpc/src/proxy_protocol.rs`).
- **Hide `/status` and `/readyz`.** Both reach the internet over h2. zingod's docs call `/status`
  safe for the open internet (`docs/running.md:196-202`): it gives the state, the network, the
  tips, the validator counts, the alarms, the version and the indexes, and no validator address,
  peer, disk or mempool.

**The loopback port.** Unlike zebrad's endpoint, zingod's port answers HTTP/1.1, but only for
`GET /status` and `/readyz`; any other path gets a 404
(`packages/zingod-grpc/src/health.rs:120-131`), so no gRPC method can be reached over HTTP/1.1,
and browsers don't speak h2c. zingod sends no CORS headers and checks no `Host`, so a page on
this computer that rebinds a name of its own to 127.0.0.1 could read `/status`. As with zebrad's
endpoint, any local program can call every method without a password, and the consent says so.
The contract test sends an HTTP/1.1 `POST` to a gRPC path and expects a 404.

**zebrad's endpoint while zingod serves: removed.** `LightwalletdKey::for_public` gives `Absent`
while zingod is the backend, so only one plaintext loopback port exists (two during Z4's switch).
`config::ensure` reports whether it changed the file, and `EndpointHost::apply` restarts zebrad
only when it did: choosing zingod restarts zebrad once, to drop its endpoint.

#### What wallets see from zingod

`GetLightdInfo` makes no validator call (`packages/zingod-grpc/src/routes/info.rs:53-68`). It
reports vendor `zingod`, version `1.1.0`, `lightwalletProtocolVersion` `v0.6.0`, the chain
(`main` or `test`), the served tip as `blockHeight` and the verified best as `estimatedHeight`.
`gitCommit`, `branch`, `buildDate`, `buildUser`, `zcashdBuild`, `zcashdSubversion` and
`donationAddress` stay empty, so Zaino's builder-login leak doesn't exist here, and Share offers
no donation address.

zingod implements 19 methods (`packages/zingod-grpc/src/service.rs:48-68`), every one in
[What wallets need](#what-wallets-need) among them. Besides the protocol rows above, a wallet may
hit:

- `OUT_OF_RANGE` at the end of a `GetBlockRange` that reaches past zingod's tip;
- `UNAVAILABLE` for mempool calls while zebrad's mempool is off (zebrad's endpoint says
  `INTERNAL`);
- height 0 for an unmined or orphaned transaction, and `NOT_FOUND` from `GetTransaction` when
  zebrad lacks the transaction;
- `RESOURCE_EXHAUSTED` for an address walk past 100,000 rows;
- a 64 KiB request cap, so `GetTaddressBalance` with more than about 1,500 addresses fails, where
  zebrad allows 10,000.

Whether Zodl's `validateServerEndpoint` or Zkool react to vendor `zingod`, to an empty
`zcashdBuild`, or to the `Nu7` spelling of `upgradeName` is unconfirmed. The wallet runs check
it before Share names a wallet for zingod.

#### Disk, memory and the first build

- **Size.** Zingo Labs' Mainnet index snapshot at height 3,544,662 is 71.3 GB installed
  (`snapshots.zingolabs.dev/zingod-snapshot-1.0.0/manifest.json`, read 2026-11-16):
  `compact_block` 31.2 GB, `transparent_address` 25.5, `tree_state` 6.8, `utxo_set` 6.2,
  `txid_location` 1.4 and `block_hash` 0.13. It grows with the chain. Testnet's size is neither
  documented nor measured.
- **Before the first build** the Service step needs 100 GB free on Mainnet and 50 GB on Testnet
  on the index's disk (proposals, set from Z3's and Z4's measurements). It warns, without
  refusing, on a network filesystem: zingod reads its indexes through mmap and asks for local
  storage.
- **While it runs** the runner reads the free space every 60 s. Below 10 GB it stops zingod with
  `SIGTERM`, before a commit can fail, since a failed commit panics; above 20 GB it starts zingod
  again (proposals). zebrad usually shares the disk, so the stop leaves it room too. The index's
  size comes from a walk every 10 minutes, capped at 50,000 entries.
- **Memory.** No RSS figure exists. From the code: about 674 MiB of index write buffers, up to
  256 MiB of queue per index that lags, and the non-finalized blocks in RAM; 1 to 2 GiB is
  likely. The Service step warns below 8 GB of RAM (proposal), and the wallet runs measure the
  peak.
- **Time.** The README's "about an hour on NVMe" assumes the validator on the same host; the
  Mainnet live test allows 48 hours and uses Zingo's own zebra fork. Nothing is measured for
  `legacy_rpc` against a stock zebrad on a desktop, so the UI estimates from the observed rate,
  and says "an hour or more" until it has one.
- **Load on zebrad.** zingod polls every second (`getblockchaininfo`, `getrawmempool true`,
  `getblockhash`), and fetches the first build's blocks over up to 32 connections of the 100 that
  zebrad's RPC admits for every client; Zeal's monitor and the explorer share the rest.

#### Privacy with zingod

With `[p2p]` and `[snapshot]` off, zingod talks only to the zebrad Zeal names, over loopback
JSON-RPC. It makes no DNS lookup (the address is an IP literal), sends no telemetry and checks
for no updates; its daemon code has no remote URL. Every wallet transaction goes to that zebrad
as `sendrawtransaction`, after a `checkadmission` that stock zebrad answers with -32601, so the
network sees it come from Zeal's node, as with zebrad's endpoint.

- **Logs.** At `zingod=info` zingod logs no client address
  (`packages/zingod-grpc/src/transport.rs:252-349`). It logs a `Serving` summary with counts once
  a minute, and each minute's first failed and first unavailable request with its method. It
  logs the txid of a transaction that wasn't accepted, at WARN
  (`packages/zingod-chainview/src/submit/driver.rs`), so Zeal's reader replaces every `txid=`
  value with `[txid]` before the line reaches the ring or the file.
- **Diagnostics** carry zingod's `/status`, its configuration (paths only; the cookie is a path)
  and the last 200 of its redacted lines.
- **Never turned on**:
  - `[p2p]`, which would add about 8 peers from DNS seeders with a `/Zingod:1.1.0/` user agent
    from the home address, push wallet transactions to them, and compete with zebrad for peers
    that allow one connection per address;
  - `[snapshot]`;
  - `[metrics]`;
  - a debug `RUST_LOG`.

#### Resources and logs of zingod

- **Resources.** The supervisor's process-tree probe feeds a `ResourceSampler`, as for nodes, so
  the panel shows zingod's CPU, memory and disk I/O on Linux.
- **Logs.** zingod logs logfmt on stdout, without colour off a terminal: `ts=… level=… message=…
  component=…`, with no target. Zeal's reader takes `ts`, `level` and `message`, uses `component`
  as the target, and keeps the other keys. The supervisor keeps its 10,000-line ring and always
  drains stdout, which zingod writes synchronously. The panel shows the last error line and
  "Open the log"; a live log view comes later.

#### Upgrades of zingod

- **zingod.** Each Zeal release bundles one pinned zingod; the next start runs the new one on the
  existing index. An index in a format it can't open is `IndexBroken`, "from an older zingod",
  with Rebuild, and the release notes say so. A pin moves with the checklist in SIDECARS.md.
- **The network.** zingod compiles in the Mainnet and Testnet schedules from its `zcash_protocol`
  fork: NU7 at 3,543,405 on Mainnet and 4,465,026 on Testnet, and nothing after. A transaction
  with a branch id the build doesn't know fails to parse
  (`packages/zingod-source/src/decode.rs:128-139`), so indexing would stop at the activation
  height (inferred from the code, not exercised), and each upgrade after NU7 needs a new zingod
  and a Zeal release. So, as this document planned for Zaino:
  - `ZINGOD_KNOWS` in launcher-herd lists the upgrades each tested zingod knows (Overwinter to
    NU7 for 1.1.0), compared with zebrad's `NodeDetails.upgrades`, which `follow_first` then also
    hands out;
  - an active upgrade zingod doesn't know: zingod isn't started, and the service pauses
    (`BuildTooOld`) until a Zeal update (Z3);
  - a scheduled one: no opening within 7 days of it, a countdown from 14 days, and a pause at
    activation minus one, so wallets get an error instead of a frozen chain (Z4). Days come from
    zebrad's heights and the target spacing.
- **zebrad.** zingod needs Zebra 7.0.0 or later (its CHANGELOG). When zebrad reaches its end of
  support it stops, which stops zingod: PS1's `BackingStopped`.

#### Removing zingod's index

- **"Turn off and forget"** gains "Delete zingod's index (41 GB in /mnt/zebra/zingod/testnet)",
  ticked: the index holds nothing of the user's and rebuilds from zebrad's chain.
- **Switching back to Zebra's built-in server** stops zingod and keeps the index, so switching
  again is quick; the panel shows its size with [Delete].
- **Rebuild and Delete** stop zingod, then remove `indexes/` and `state/` inside the recorded
  folder, in a blocking task, naming anything they can't remove. They have their own path rules:
  `delete_node_data` refuses any folder not named `state`
  (`crates/launcher-core/src/firstrun.rs:420`). Deletion waits for zingod's stop, since its
  `LOCK` files hold the folders.
- **The first node** can't be removed while a light client server is set up
  (`NodeError::ServesPublic`), so the index never goes with its data folder unasked.
- **UNINSTALL.md** names the folder.

#### zingod in the UI

The consent screen gains the choice before the tick, because what becomes public depends on it:

```text
Which server answers wallets?
 (•) Zebra's built-in server · experimental
     Serves as soon as Zebra · Testnet is at the tip, with no extra disk.
     Sends no transparent data; newer Zingo releases refuse it.
 ( ) zingod 1.1.0, by Zingo Labs · experimental · bundled with Zeal
     The current light wallet protocol, with transparent data and fees.
     Builds its own index first, which can take hours, and needs 50 GB free on
     /mnt/zebra (412 GB free).
```

On Mainnet before Z4 the second choice is disabled, with "zingod is being tested on Testnet
first"; without `zeal-zingod`, "zingod runs on Linux only for now". The Zingo sentence ships only
once the wallet runs confirm it. With zingod chosen, the consent list reads:

- "every wallet that connects is told it is zingod 1.1.0";
- "zingod is experimental: Zingo Labs say it is in development and not for production Mainnet
  use. They publish no binaries, so Zeal builds it from their source";
- "zingod keeps its own index of the chain on /mnt/zebra, about 71 GB on Mainnet";
- "while the light client server is switched on, programs on this computer can use zingod's
  port without a password. Web pages in a browser can't call it".

The consent records the backend (`Consent.backend`), so choosing the other one later asks again;
"Server software…" in the panel's ⋯ menu does that. `CONSENT_VERSION` doesn't change, so no open
service pauses for this release.

Step 2 with zingod:

```text
 ✓ Zebra · Testnet 7.0.0 with zingod 1.1.0 is a pair Zeal tested
 ✓ zingod knows every upgrade Zebra · Testnet lists
 ✓ 412 GB free on /mnt/zebra: zingod's index needs 50 GB here
 ✓ Zebra · Testnet is at the tip
 ✓ zingod 1.1.0 is running, reading Zebra · Testnet
 ● Building zingod's index: block 1,204,331 of 4,612,000 (26 %), about 5 h left
     Wallets can't use your server until this is done. Quitting Zeal pauses it, and it
     goes on where it stopped.
 ○ zingod's tip matches Zebra's
 ○ zingod answers the wallet check
```

The setup goes on while the index builds, as it does while zebrad syncs; the test on this
computer and Open wait for it. The status panel's first line follows
[the table above](#what-can-go-wrong-and-what-the-user-sees), and a **Server** card joins
Reachability, Certificate and Use:

```text
 ┌ Server ───────────────────────────┐
 │ zingod 1.1.0 · serving            │
 │ block 4,612,019 · Zebra 4,612,019 │
 │ index 18.4 GB in /mnt/zebra       │
 │ 1.4 GB memory · 3 % CPU           │
 │ [Open the log]  [Rebuild…]        │
 └───────────────────────────────────┘
 Zebra at block 4,612,019 · zingod 127.0.0.1:18137 · front door running
```

- The dashboard card and the banner on the node's pages show the same first line; the sidebar's
  facts don't change.
- The serving zebrad's header says "zingod reads this node", and its Stop and Restart say "zingod
  restarts with Zebra and waits for it to reach the tip: wallets are told to try again
  meanwhile".
- Every sentence comes from typed facts in `ui/src/public_text.rs`; the choice uses
  `RadioChoice`.
- **Mock flags** (`ui/src/mock/public.rs`, listed in `ui/src/mock.rs` and ui/README.md):
  `?public=zingod-choose` (the consent screen with both choices), `zingod` (serving) and the
  flag of each row in [the table above](#what-can-go-wrong-and-what-the-user-sees). Each builds
  facts from captured `/status` answers and runs launcher-herd's real rules. Screenshots come
  from the headless-Chromium workflow.

#### Types and commands for zingod

```rust
// launcher_types::public
pub enum LightBackend { #[default] ZebradEndpoint, Zingod }      // snake_case on the wire
PublicServiceSpec {
    #[serde(default)] pub backend: LightBackend,
    #[serde(default)] pub zingod: Option<ZingodSpec>,            // kept after switching back
    // backend_port stays zebrad's endpoint port
}
pub struct ZingodSpec { pub port: u16, pub dir: PathBuf }          // recorded when first chosen
Consent { #[serde(default)] pub backend: LightBackend }           // a consent covers one backend
StepAnswer::Backend { backend: LightBackend }
NotAvailable::UntestedZingod { version: String, network: Network }
CheckId: ZingodTested, UpgradesKnown, IndexSpace, IndexReady, TipMatches
                                // beside ZebradTested, AtTip, EndpointOpened and WalletCheck
DegradedReason: IndexerStarting { waiting: IndexerWait },         // ZebradTip, Boot, Backoff
    IndexBuilding { served: u64, target: u64, eta_secs: Option<u64> }, IndexBehind { blocks: u64 },
    IndexerDown { why: DownReason },                              // Zingod { reason, message }, NoAnswer, NoValidator
    IndexerStuck { at: u64, zebrad_at: u64 }, IndexerFailed { class: ExitClass }, IndexBroken,
    ChainMismatch { at: u64 }, DiskLow { free_bytes: u64 }
PauseReason::BuildTooOld { upgrade: String, height: u64 }
PublicStatus { pub indexer: Option<IndexerStats> }
pub struct IndexerStats { version, phase: Phase, status: Option<IndexerStatus>, dir, index_bytes,
    free_bytes, resources: Option<NodeResources>, last_exit: Option<ExitInfo> }
pub struct IndexerStatus { state, served, tip, target, rate, network, trusted, answering, alarms,
    version }                                                     // zingod's /status, parsed
// launcher_types: ExitClass gains DiskFull and IndexBroken, both deterministic
```

The names say "indexer" where the code doesn't depend on zingod, so a node kind can take them
over.

- **The runner.** `ZingodRunner` (`launcher-public/src/zingod.rs`), a child of the service actor,
  owns zingod's supervisor (stop grace 30 s, the pid file, the log). It takes a
  `watch<Option<ZingodPlan>>` (the configuration, the port and zebrad's run) and acts on every
  change, so nothing is lost or replayed. Rebuild and Delete come on an mpsc, each with a oneshot
  reply. It publishes a `watch<ZingodFacts>`: the supervisor's status, the `IndexerStatus`, the
  resources, the free space, the index's size and since when it stalled. No locks; zingod's own
  `std::sync::Mutex` stays inside its process.
- **The service actor** adds the runner's facts, runs `want()` and the new rules, picks the front
  door's upstream and the light monitor's endpoint by backend, and stops zingod after the front
  door goes `Unavailable`.
- **Options** gains the launcher's `Paths` and `zingod_binary: Option<PathBuf>`, `None` where no
  `zeal-zingod` is bundled. `src-tauri/src/sidecar.rs`'s `resolve` takes a `Sidecar` key,
  `Zebrad(channel)` or `Zingod`, changed in place, with the AppImage copy-out per key.
- **Commands**: `rebuild_public_index` and `delete_public_index` are new;
  `forget_public_service` gains `remove_index`; `open_path` opens zingod's log. Each goes in
  `COMMANDS` in `src-tauri/build.rs` and in `capabilities/main.json`, and the actor checks it
  again. "Try again" is `run_public_checks(Service)`, which clears a failure that waits for the
  user.
- **The exit flow** (`src-tauri/src/exit.rs`): the front door closes first, as today; then
  zingod's stop and the registry's run side by side under the larger cap.
- **Diagnostics**: `copy_diagnostics` adds zingod's status, configuration and log tail.

#### Tests for zingod

- **Pure rules** (launcher-herd, native and wasm): `want()` over tables of facts, each condition
  turned off alone; one zingod run per zebrad run; the stall rule's two windows and its single
  restart; the disk hysteresis; the cross-check's three in a row; every `/status` state mapped;
  the Service step's checks for both backends; a consent for one backend not covering the other;
  `DEFAULT_BACKEND`; `TESTED_ZINGOD` holding the bundled `ZINGOD_VERSION`; the upgrade gate;
  `gateway_mode` keeping the front door up for every zingod problem but `BackingStopped` and
  `BuildTooOld`.
- **The adapter** (launcher-core):
  - the configuration for both networks against a golden file, every key present in
    `tests/fixtures/zingod-1.1.0-generate.toml` or on the list of unprinted keys
    (`cookie_path`), `cookie_path` always set to the node's cookie, every path absolute, and no
    Regtest-only value on Mainnet or Testnet;
  - the environment: a fake that prints its own sees only the five variables, so
    `ZINGOD_CONFIG_SYNC__CONCURRENCY`, `zingod_config_x`, `NOTIFY_SOCKET` and a bad `RUST_LOG`
    never reach the child;
  - the logfmt reader; `Running` at `Listening` with `component=Grpc`, not `Admin`;
  - each exit class, from captured lines;
  - the txid redaction, before the ring and the file.
- **The runner against fakes** (launcher-public, under `tokio::time::pause`). `fake-zingod`, a
  launcher-core bin beside `fake-zebrad`:
  - reads its `-c` file and refuses an unknown key with zingod's `unknown field` message;
  - binds `grpc_listen_address`, prints zingod's lines, answers `/status` and `/readyz` over
    HTTP/1.1 from captured JSON and `GetLightdInfo` and `GetLatestBlock` over h2c, and stops on
    `SIGTERM`;
  - plays, through `FAKE_ZINGOD_MODE`: syncing then serving, stuck, a wrong tip hash, no
    validator answering, each classified exit, an abort, and a `SIGTERM` it ignores;
  - reads the cookie once and again only after a 401, as 1.1.0 does. `fake-zebrad` gains a mode
    that closes the connection on a wrong cookie, as 7.0.0 does, so the fast tests prove that the
    restart on a new zebrad run is what brings zingod back;
  - reports `ZINGOD_VERSION`, read with `include_str!`, which a cargo test checks
    (`check-versions.sh` runs before anything compiles, so it can't).
- **Contract** (`crates/launcher-public/tests/real_zingod.rs`, ignored by default like
  `real_light_server.rs`; `ZEBRAD_BIN`, `ZINGOD_BIN`). A Regtest zebrad through the supervisor,
  the real `zeal-zingod` through the runner with Zeal's configuration, and the real front door with
  a test certificate. It checks that:
  - `/status` reaches `serving`;
  - `GetLightdInfo` says `zingod`, `v0.6.0` and `regtest` (zingod's name for Regtest; zebrad says
    `test`);
  - `GetLatestBlock` equals zebrad's height and hash;
  - a `GetBlockRange` streams to a slow client, on loopback and through the front door;
  - after a zebrad restart zingod's polls fail with the stale cookie, and the rule brings it back
    within 60 s;
  - `SIGTERM` exits 0 within the grace, with and without an open connection;
  - a second zingod on the index gets `DbLocked`, a taken port `PortInUse`, an unknown key
    `ConfigError`, and a flipped tail-page byte `IndexBroken`;
  - a `SIGKILL` mid-build reopens and serves;
  - an HTTP/1.1 `POST` to a gRPC path gets a 404;
  - a `ZINGOD_CONFIG_NETWORK` in the test's environment doesn't reach zingod, and nothing is
    written outside the configured folders.

  Its capture mode writes the fixtures. CI runs it on pushes to `main` and on pull requests that
  change `ZINGOD_*`, `ZEBRAD_VERSION`, `ZEBRAD_COMMIT`, zingod's adapter or runner, or the front
  door, with zingod built by `scripts/build-zingod.sh` on the runner and cached by commit, as a
  test input only. To settle when it is written (unconfirmed): whether a Regtest chain reaches
  `serving`, and whether the pinned zebrad's `generate` mines the blocks the test needs.
- **Wallet runs** (RELEASING.md), on a real home connection, before zingod is offered on a
  network: Zingo through a scripted `zingo-cli` (a new wallet, an old birthday, a send with a
  memo), which zebrad's endpoint can't serve; Zodl and Zkool by hand, where builds for the network
  exist. Each run records the index build time, its size, zingod's peak memory, zebrad's CPU under
  zingod's polls, the first wallet sync, the tip lag and the bytes sent.

#### Phases for zingod

These replace PS5. Each is one pull request or a short series, with its tests, mock flags and
screenshots, and ARCHITECTURE.md updated for what landed. Z1 and Z2 change nothing users see and
can land beside the explorer, herd and resources work; only Z2's `Launch` touches shared code.
None of them needs front door v2 (PS2) or MULTI-NODE phase 5.

| Phase | Ships | Needs |
| --- | --- | --- |
| **Z1 Bundle** | the `ZINGOD_*` pins; `build-zingod.sh` and `bump-zingod.sh`; `zingod-sidecars.yml` with its checks and attestation; the source archive on each release; `zeal-zingod` and its licences in the Linux bundles; a minimal `fake-zingod` for `dev-sidecar.sh`. Nothing runs it yet | — |
| **Z2 Supervise** | `Launch` and the zingod reader in the supervisor; the configuration writer; `DiskFull` and `IndexBroken`; the pure rules; the runner; `fake-zingod`'s modes and fake-zebrad's stale-cookie mode; `real_zingod.rs` in CI. No UI | Z1 |
| **Z3 Choose (Linux, Testnet)** | the types; the backend on the consent screen and in the setup; the service actor's and the app's wiring; the disk, stall, cross-check and active-upgrade rules; Rebuild, Delete and Forget's box; the Server card, banners and mock flags; `TESTED_ZINGOD` = 1.1.0 with zebrad 7.0.0 on Testnet. Switching an open service means "unavailable" until the index is built | Z2; the Testnet wallet run before the release |
| **Z4 Mainnet** | the gate below; `TESTED_ZINGOD` gains Mainnet; the disk and memory numbers from measurements; the scheduled-upgrade countdown and pause; the consent's Mainnet line; Share naming the wallets the runs confirmed; switching while Zebra's endpoint keeps serving; the default for new setups, as the maintainer decides | Z3 and the gate |
| **Z5 macOS** | `zingod-sidecars.yml` on `macos-15` and `macos-15-intel` (Xcode command line tools for jemalloc's `sh` and `make` and libproc's bindgen, protoc); a `tauri.macos.conf.json` with four `externalBin` entries; the contract test there; `kern.maxfilesperproc` checked against zingod's 1,280 | PS3; the build passing |

**The Mainnet gate (Z4):**

1. `real_zingod.rs` passes on both Linux targets at the pins.
2. A Testnet light client server ran on zingod for 14 days on a home server, with no pause, crash
   loop or stall caused by zingod, and the Testnet wallet runs passed through the front door.
3. A Mainnet first build was measured on a desktop (the time, the peak memory, the index's size
   and zebrad's CPU), and the disk and memory numbers above were set from it.
4. The Mainnet wallet runs pass.
5. The maintainer confirms v1 for Mainnet with zingod.

**Switching while serving (Z4).** Zebra's endpoint keeps serving on its port while zingod builds
on its own. Once zingod has served within 2 blocks for 10 minutes with every check clean, the
front door's upstream moves to zingod (a mode change, so open connections close and wallets
reconnect). Then zebrad's key goes, with one zebrad restart and zingod's after it: about a minute
of "unavailable", measured in Z4. Back to Zebra reverses it; the index stays until deleted.

**Later**, each its own pull request:

- Windows, after upstream's portability changes.
- zingod as a registry node kind, with MULTI-NODE phase 5: the `Launch` reader becomes its
  adapter, `IndexerStatus` its status, and `RegistryHandle::follow(id)` its link to the zebrad it
  reads.
- A live log view (`subscribe_public_logs` over `bridge::forward_logs`).
- PROXY v2 from the front door, with `trusted_proxies = ["127.0.0.1/32"]` and v2 `LOCAL` headers
  on Zeal's own probes, so zingod's per-address limits see each client.
- zebrad's push streams (`indexer_address`), with `rpc.indexer_listen_addr` as one more managed
  loopback key, once the contract test covers them.
- A per-index rebuild from `zingod verify`, and choosing the index's folder.
- A weekly fetch-and-advisory run, and a watcher for zingod releases.
- No restart with zebrad for zebrad 7.1.0 and later.

#### Upstream asks for zingod

- Release binaries with signed checksums or attestations, and annotated tags. The image's
  revision label names the release commit's parent.
- Read the cookie again when a request fails with no HTTP answer: zebrad before 7.1.0 closes the
  connection instead of answering 401.
- Windows portability: `cfg` gates for `std::os::unix` (`FileExt`, `MetadataExt`), `statvfs`,
  `tokio::signal::unix` and sd-notify; jemalloc off MSVC; directory fsync.
- macOS in CI.
- A second signal that ends the drain, as the comment at `daemon.rs:526` says.
- Docs: `tip_stale` in `docs/running.md`, which the code reports as `tip_unvouched`, and the index
  default in `config.rs:322`'s comment.
- A `NOTICE` file and the full licence text.
- A Testnet index size.
- Fork dependencies on refs that can't disappear, or mirrors of them.

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
   endpoint, or zingod): until then the front door answers only Let's Encrypt's
   challenge and this computer. zeeder answers the internet only after its own Open.
4. **No public listener outlives Zeal.** The front door runs inside Zeal's process, so it
   closes when Zeal exits or crashes, on every system. zebrad's endpoint and zingod listen on
   loopback only, and on Linux PDEATHSIG ends both with Zeal. zeeder has no stop on stdin close, so
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
   and asked again when `CONSENT_VERSION` or the name changes. An open service whose consent
   a Zeal release no longer covers pauses (`ConsentNeeded`, until the user agrees again on the
   consent screen, which the panel then shows): the front door, the name's updates and the
   endpoint stop, and at launch the endpoint goes before zebrad's first start. For the light
   client server it also accepts Let's Encrypt's Subscriber Agreement (ACME's
   `termsOfServiceAgreed`).
2. **The service.** Light client server: Zeal takes the network's zebrad (in PS1, the one it
   runs), assigns the endpoint's loopback port and restarts zebrad with it. Gates: the zebrad
   is on Zeal's tested list (in PS1, the bundled 7.0.0); zebrad logged
   `Opened lightwalletd gRPC endpoint at` with the assigned address; `GetLightdInfo` answers on
   loopback with zebrad's chain and branch id. With zingod chosen the step starts zingod instead,
   with its own checks ([zingod in the UI](#zingod-in-the-ui)). The setup goes on
   while zebrad syncs ("Zebra · Mainnet is 82 % synced. Wallets can use your server once it
   reaches the tip, in about 9 hours"). Seeder: Zeal reuses the census node or adds one. Gates:
   `zeal-zeeder` exists here; it knows every upgrade the bundled zebrad schedules; the crawler
   port is free (or the 8233 dialog moved its holder); the DNS port is free.
3. **This network.**
   - The LAN address and gateway come from the default route (`netdev`). The user reserves the
     address in the router's DHCP settings ("a forward targets an address"); Zeal records it
     and re-checks it at every start and every 5 minutes.
   - A VPN or exit node on the route to the internet stops the setup: a route on a VPN's
     interface (`tun`, `utun`, `wg`, `ppp`, Tailscale: a heuristic), or on another interface
     than the router's, such as a tunnel's default without a gateway at a lower metric, or the
     0.0.0.0/1 and 128.0.0.0/1 of OpenVPN's `redirect-gateway def1`; and policy rules that
     send internet traffic out of another interface (wg-quick, NetworkManager's WireGuard, a
     Tailscale exit node), seen from the address the kernel picks towards the internet. "Turn
     it off, or set it to leave Zeal out."
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
   build and subversion (with zingod, its vendor and version, and no build user).
9. **Open to the internet**, the only button that makes a service public. The backend re-runs
   every gate and refuses with the failing one (`OpenError { check }`). Light client server:
   zebrad is at the tip for 10 minutes (with zingod, zingod within 2 blocks of zebrad too);
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
    code; no donation address, since neither zebrad's endpoint nor zingod sends one. Seeder: the
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
  stops on "too many updates". Without the router's address (no NAT-PMP answer, carrier-grade
  NAT, a second router), DuckDNS takes the address it sees, which may change unseen, so that
  update goes again every 5 minutes (DuckDNS answers `NOCHANGE`). No update goes out until a
  network check made since the service started, or resumed from a pause only the user ends,
  finds the router's default route and this computer's LAN address, no VPN on the route to
  the internet and, once open, the LAN address the forward targets: a laptop that starts Zeal
  or resumes on a café's network never points the name there, and neither does one without a
  default route through a router, as when a VPN replaced it by its own without a gateway
  (that VPN is found too). A mismatch for 15 minutes, against the router's address or else
  the one DuckDNS saw, pauses the service. After a change:
  "Address changed at 14:02. DNS updated; resolvers may use the old one for up to 5 minutes."
- **Tokens** go to the OS keychain (`keyring` 4.2.0, service `org.zfnd.zeal`, account
  `dns:<provider>:<zone>`: a DuckDNS token covers every name on the user's DuckDNS account,
  so it is kept once as `dns:duckdns:duckdns.org` and a new name or provider leaves no other
  entry); `launcher.json` holds a `SecretRef`, and the UI only learns `has_token`. A token an
  older Zeal kept under the name (`dns:duckdns:<name>`) is read as a fallback and moves to
  the zone's entry when the name changes; until the keychain has written the zone's entry and
  deleted the old one, the name does not change, so no entry is left under a name Zeal no
  longer looks at. "Turn off and forget" with the token box ticked
  deletes every entry the token may be under and says when the keychain refused, so the token
  may still be there. Without a Secret Service on Linux, a 0600 file, as an explicit choice. No request
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
  the chain is verified with the platform verifier before it is pushed to the front door. A
  certificate that does not verify or cannot be stored counts as one failed production order,
  with the backoff, so a broken verifier or a read-only folder never orders back to back.
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
  running the setup again, or switching the server software, orders nothing. Keys are 0600 on
  Unix and under the per-user AppData ACL on Windows, a key others can read is refused, and
  every write is atomic.
- **Expiry**: the panel shows the days left and the next window; a banner 14 days before expiry
  if renewal fails; `ChallengeOnly` at expiry; a daily check of the served chain. An expired
  certificate is ordered again under the budget without being asked, and the panel offers
  "Try again"; one found expired on disk when Zeal starts is never served, but its dates are
  kept, so it counts as lapsed whatever step of the setup is current.

### Ports, the router and NAT

| What | Router | This computer | Protocol | Listener |
| --- | --- | --- | --- | --- |
| light client server, TLS-ALPN-01 | 443 (9067 with DNS-01, PS6) | 8443 (`PublicTls`) | TCP | the front door, in Zeal, on `[::]`, IPv6-only off through `socket2` (Windows defaults to on) |
| zebrad's light-wallet endpoint | never | loopback `127.0.0.1:9067`, Testnet `:19067` (`Grpc`), only while the service is on | TCP | zebrad |
| zingod gRPC, `/status`, `/readyz` | never | loopback `127.0.0.1:8137`, Testnet `:18137` (`ZingodSpec.port`), only while zingod runs for the service | TCP | zingod |
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
FRITZ!Box's forwards "will have no effect"); a VPN on the route to the internet sends replies
the wrong way.

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
| zebrad's branch id by `GetLightdInfo`, every 10 min (with zingod also: `/status`, within 2 blocks of zebrad, not stuck, the tip cross-check; [Health of zingod](#health-of-zingod)) | 10 min; 5 s with zingod | `Degraded`: the front door says "unavailable" | by itself, after 5 min caught up |
| zebrad running and at the tip | on change | `Degraded` while it restarts or catches up: 10 or more blocks behind at once, more than 2 only after 10 min, since zebrad's estimate of the tip grows by one every 75 s after the tip block's time and an ordinary gap between blocks puts it 3 ahead; paused if stopped or halted at end of support | by itself, as soon as it is within 2 |
| an upgrade the serving software doesn't know, at activation minus one | each block | paused (`BuildTooOld`): zeeder, and zingod; on Mainnet zebrad halts at its end of support instead; on Testnet zebrad never halts (`zebrad/src/components/sync/end_of_support.rs:53-60`), so one that doesn't know the upgrade stays at the block before it, and the tip watch keeps the front door `Unavailable` until a Zeal update brings a newer zebrad | after a Zeal update |
| the name equals the WAN address, at the authoritative servers | 5 min | paused after 15 min | by itself |
| the LAN address unchanged | 5 min, at start | paused at once | when the forward or address is right again |
| no VPN on the route to the internet | 5 min | paused | by itself |
| the certificate | hourly | a banner 14 days out if renewal fails, and "Try again" on the panel; `ChallengeOnly` at expiry, with orders going on under the budget | after issuance |
| the monthly cap | each stats line | paused | next month, or a higher cap |
| seeder abuse ([limits](#the-seeders-limits)) | 10 s | paused an hour | by itself; after three in 24 h, by the user |
| the seeder's `/ready` | 10 s | a banner after 10 min (empty answers are cached 10 min, but zebrad asks other seeders too) | by itself |
| Zeal quits or crashes | — | everything public stops; mappings deleted (after a crash, at the next start) | at launch, if "resume when Zeal starts" is on and the gates pass |

Problems that clear themselves keep the front door up in `Unavailable`, so wallets get a clear
error and TLS-ALPN-01 renewals still work: a long stall of the backend, such as zingod's first
index build, must not let the certificate expire. Problems with the exposure itself (the LAN
address, the name, a VPN, the cap, the user, a too-old build) close it. The network checks and
the name's resolution keep running through a pause that ends by itself, so it sees its cause
clear: a VPN going down, the name pointing here again (until a resolution says so the pause
for the name holds). A pause only the user ends stops them. **Pause**, the switch
off, stops the front door or zeeder and deletes the mappings, and removes zebrad's endpoint
with a restart of zebrad ([How Zeal runs it](#how-zeal-runs-it)); **Resume** re-runs what the
pause may have changed. **Turn off and forget** also offers to delete Zeal's DNS records, the
token, the firewall rule and zingod's index, asks whether to keep a zeeder node for local use,
and says: "Your address stays in DNS history
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
none per client, zingod sets its own ([The front door for zingod](#the-front-door-for-zingod)),
and behind the front door both see every client as 127.0.0.1, so the per-client limits live in
the front door: **v1** (PS1) forwards decrypted bytes and
limits connections, **v2** (PS2) ends HTTP/2 and limits streams and methods
([The front door](#the-front-door)). Values are proposals, tuned by the wallet runs.

| Limit | Default | How | In |
| --- | --- | --- | --- |
| connections, all clients | 128; when full, a newcomer from a network holding fewer than the largest holder closes that holder's oldest connection | `LimitsActor`, a oneshot per connection to close it | v1 |
| connections per IPv4 address or IPv6 /64 | 8 | `LimitsActor` (`::ffff:` mapped to IPv4) | v1 |
| new connections per address | 2 a second, burst 16 | token bucket in `LimitsActor` | v1 |
| TLS handshake; idle connection | 10 s; 10 min without a byte | timers | v1 |
| connection age | 2 h, then closed: zebrad's HTTP/2 ping every 30 s keeps a connection from going idle | a timer | v1 |
| upload | 10 Mbit/s (the preset) | a `Semaphore` of KiB permits refilled by a ticker | v1 |
| monthly upload | 300 GB, then paused | `AtomicU64`, totals persisted | v1 |
| header list in `Unavailable` | 16 KiB (hyper's and zebrad's), then the call or the connection ends | h2 server builder | v1 |
| streams per connection; header list | 16 (never above the backend's own: zebrad's 20); 16 KiB | hyper `http2` builder | v2 |
| request body | 4 MiB, `SendTransaction` 2.5 MiB | a counting body, then `RESOURCE_EXHAUSTED` | v2 |
| `GetBlockRange`, `GetBlockRangeNullifiers` in flight | 16 in all; a 10 s wait, then `UNAVAILABLE` | per-method `Semaphore` | v2 |
| `GetSubtreeRoots` in flight | 2 (a hash lookup per root on zebrad; zingod reads its own index) | `Semaphore` | v2 |
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
and, in the rewrite, `/readyz` included. So Zaino would have served only through v2; zingod,
which answers all three, serves through v1
([The front door for zingod](#the-front-door-for-zingod)). zebrad's endpoint answers the first
two and shrinks the third, so **PS1 opens Mainnet on v1**, labelled experimental, once Zeal's
wallet runs pass there:

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
`UNIMPLEMENTED`, zebrad's reflection service and zingod's `/status` and `/readyz` included),
only the `content-type`, `te`, `grpc-timeout`, `grpc-encoding` and `grpc-accept-encoding`
headers (no `user-agent`), and re-frames everything on one upstream h2c connection per client,
so the backend's h2 never sees a client frame. Its upstream client is h2 0.4.16 or newer, and the Zaino
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
paused it for an hour." zingod and zeeder run at a lower priority (nice 10;
`BELOW_NORMAL_PRIORITY_CLASS`).

### What becomes public, and what Zeal keeps

The consent screen ([UI](#the-consent-screen)) lists what becomes public, for good: the home
address (in DNS, kept by DNS-history services), the name (in Certificate Transparency logs
within minutes, which scanners watch, so probes start at once), the certificate (indexed
against the address), the region and provider (geolocation; a personal name or WHOIS record
ties it to a person, so Zeal suggests a neutral name and WHOIS privacy), and the software
(zebrad's vendor, version and subversion in `GetLightdInfo`; with zingod, its vendor and
version only;
`zeeder/<version> (<commit>)` to every peer the crawler contacts; the SOA's
`hostmaster.<domain>`).

Wallets show the server their addresses and timing, the blocks they fetch, the transactions
they ask for, the transparent addresses they look up and where each sent transaction came
from. For the owner's phone that beats a stranger's server; about everyone else, the owner
learns it all. Zeal keeps as little as it can:

- the front door logs counters, never addresses, and the backend sees only 127.0.0.1 (zebrad
  logs no client address for a gRPC call in any case, zingod none at the level Zeal sets, and
  Zeal never turns on zingod's PROXY protocol);
- the limiter is keyed by SipHash-2-4 of the address or /64 under a random key per run, pruned
  once an address without a connection has its whole burst back, after 2 minutes idle at most,
  or an hour after its last `SendTransaction` for an address that sent one (the hourly cap's
  window), and never written down; it keeps at most 16 idle addresses per connection slot,
  forgetting the ones idle longest, so its memory and the work of an admission do not grow with
  the number of networks that try; "a connection from outside" keeps only a time;
- zeeder's lines are filtered and redacted ([Configuration](#configuration));
- diagnostics (`copy_diagnostics`, Copy summary) blank the addresses and the name unless
  "include addresses" is ticked, and never carry tokens or keys. A DuckDNS name keeps its zone,
  which thousands of names share (`z**.duckdns.org`); any other name keeps only its last label
  (`z**.a**.net`), since its zone is the user's own domain and certificate logs are searched by
  domain. The launcher log's tail goes into the diagnostics as it is, so its lines that quote
  Let's Encrypt or the certificate verifier mask the name the same way.

Third parties, each named at consent: Let's Encrypt (the name, and the address of Zeal's ACME
client), the DNS provider (the name, the address, the token), the public resolver the user
picks (one query for a public name), and the certificate logs (the name). The screen also
says that residential terms often restrict servers or cap data (Xfinity's AUP, for example;
current text unconfirmed), that some providers block inbound 53, 80 or 443, and to check local
law: in the EU an IP address can be personal data (CJEU *Breyer*, C-582/14). Zeal never says
running a server is allowed.

**Never**: a plaintext public bind (zebrad's endpoint or zingod anywhere but loopback, a `tls`
key in zingod's file); Zeal writing zebrad's endpoint while the service is off; zingod's
`[p2p]`, `[snapshot]` or `[metrics]`;
`enable_cookie_auth = false`; turning UPnP on, or a mapping that outlives a pause; an open
proxy (one upstream, fixed at start, no routing by SNI); WAN 8233 to zeeder; a client address
in a log.

## Backend

### Crates and modules

```text
crates/launcher-types/src/public.rs  serde types (below), wasm-clean
crates/launcher-herd/src/public/     pure, native and wasm (HERD.md's crate; PS1 starts it if HERD H1
                                     hasn't): plan.rs, pause.rs, light.rs (the backend's health),
                                     zingod.rs (Z2: when zingod runs, the stall rule, the
                                     cross-check, the upgrade gate),
                                     wallet.rs (Zodl's check, branch ids pinned to zebra-chain 15),
                                     evidence.rs, share.rs
crates/launcher-core/src/            config.rs: the managed lightwalletd key, which it can now remove;
                                     logs.rs: the endpoint's and the bind failure's lines; grpc.rs: a
                                     small gRPC client, prost messages written by hand (no protoc) over
                                     hyper's HTTP/2 client; zingod/ (Z2): the configuration writer,
                                     the command and its environment, the log reader, exit classes;
                                     kinds/zeeder.rs: HERD.md's adapter plus public mode and redaction;
                                     bin/fake-light-server.rs
crates/launcher-public/              new, no Tauri: service.rs (PublicServices, ServiceActor),
                                     light.rs (LightMonitor), zingod.rs (ZingodRunner, Z2),
                                     netcheck/ (netdev, router queries,
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
    ├─ ZingodRunner (with zingod, Z2) ──────▶ watch<ZingodFacts>
    └─ LightMonitor (the backend's endpoint) ▶ watch<Option<LightStats>>
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
  `apply_config`), so a Configure save keeps the key; the ServiceActor's own applies write the
  endpoint it decided on, so an off it could not save still removes the key. When a permit is busy, turning on fails
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
logged address (zingod's, with zingod). Its lifetime follows the switch, not zebrad's process, so it
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
| `Unavailable` | challenges; service handshakes complete, and every call gets `UNAVAILABLE` from the front door | zebrad restarting or behind; with zingod, every zingod problem but `BackingStopped` and `BuildTooOld` |
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
// PortRole gains PublicTls; zebrad's endpoint uses the existing Grpc. With zingod (Z2), ExitClass
// gains DiskFull and IndexBroken (deterministic); zingod's other types are in "Types and commands
// for zingod".
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
/// zebrad (`zebrad-mainnet` or `zebrad-testnet`), also with zingod, which reads it; the seeder's
/// zeeder.
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
pub enum DegradedReason { BackingRestarting, BackingBehind { blocks: u32 }, CertificateExpired }
                                    // zingod's reasons: "Types and commands for zingod"
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
| `begin_public_setup(service, network)` | creates the spec and returns its `NodeId`: the network's zebrad for the light client server, the zeeder node for the seeder, added when there is none; the herd page's "Set up…" |
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
  `certs/<name>/`, `state.json` (mappings, firewall rules added, the latest evidence and
  staging validation per name) and
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
  Sidecars, not linked: zingod (Apache-2.0, Z1), zeeder (MIT OR Apache-2.0); Pebble (MPL-2.0)
  runs only in CI.
- **One crypto provider.** `Cargo.toml` and `.github/dependabot.yml` record that clients built
  without a rustls provider panic ("No rustls crypto provider is configured"). Zeal installs
  ring as the process default at startup when none is installed (first install wins), gives
  its own configs an explicit provider, and tests the updater's, the ACME and the self-test
  clients in one process. `launcher-core`'s reqwest stays at 0.12 without TLS.
- **No locks** (`clippy.toml` bans them): each service's state in its actor, published on
  `watch`; the certificate, challenge and mode on `watch` inside the front door; per-address
  and total connection counts in `LimitsActor` (mpsc in, oneshot out, a oneshot per
  connection to close it, a guard's drop sends `Closed`); method concurrency and the upload
  budget on `Arc<Semaphore>`; counters on `AtomicU64`; the
  keychain through `spawn_blocking`. No DashMap: it locks its shards.

## UI

### Where the switches are

- **On the serving node's page**: zebrad's page, with either backend, and zeeder's get a
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

With zingod the software line reads "zingod 1.1.0, reading Zebra 7.0.0", and the lines in
[zingod in the UI](#zingod-in-the-ui) join the list.
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
zingod, the lines in [What can go wrong](#what-can-go-wrong-and-what-the-user-sees). Certificate
banners turn amber 14 days before a failing expiry, red at it.

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

- **Sidebar** facts: "serving", "catching up", "paused" for the serving node (zebrad, with
  either backend); "seeding" for zeeder.
- **Tray**: a disabled "Public: light client server serving" line (Linux's tray has no
  tooltip), "Pause public services", "Resume public services…", and "Quit (stops public
  services)" while one is on.
- **Banners** for automatic pauses say the cause and what wallets or nodes see; "Resume anyway"
  isn't offered for a too-old build, a certificate or an attack.
- **Health verdicts** (`ui/src/health.rs`): a build too old, then paused, then degraded, then
  not confirmed from outside.
- **Add node**: HERD.md's zeeder card. Zaino's card keeps today's caveat, now that no Zaino
  release follows NU7, and adds that a zainod you pick runs for this computer only. zingod gets
  no card: it is the light client server's, not a node.
- **Words**: "light client server" (the maintainer's), "front door", "DNS seeder",
  "experimental"; Off, Setting up, Serving, Catching up, Paused, Not available.

## Testing

- **Pure logic**, native and wasm: the plan as tables of facts to steps for both services, with
  a test that no plan step makes a network request; pauses with their hysteresis and resume
  rules (zebrad behind or restarting, the name elsewhere, the cap, a third attack); evidence
  ages, and DNS-01 never counting; the endpoint's key present only while the service is on; the
  branch check; Zodl's check over `LightdInfo` bytes captured from zebrad's endpoint (and from
  zingod in Z2), with field numbers pinned to a vendored `service.proto`; `PublicHostname` and
  `SeedZone`. zingod's are in [Tests for zingod](#tests-for-zingod).
- **Adapters**: zebrad's managed key, written while the service is on, removed when it is off,
  a hand edit overridden, a non-loopback value refused, a hand-set loopback value kept while no
  service is set up; the `Opened lightwalletd gRPC endpoint at` and bind-failure lines from
  captured fixtures; from Z2, zingod's adapter ([Tests for zingod](#tests-for-zingod)); zeeder's
  redaction on lines inside and outside the `dns_query`
  span, with no address reaching the ring; an answered query and a dropped one add no line to
  the ring or the log file.
- **The front door against `fake-light-server`**, an h2c gRPC server on hyper that serves
  `GetLightdInfo`, `GetLatestBlock`, a delayed `GetBlockRange`, a long `GetMempoolStream` and
  `SendTransaction`, and can stall at a height, stop reading or send error trailers. Cases:
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
  fixtures. This gates every zebrad pin while zebrad serves. `real_zingod.rs` (Z2,
  [Tests for zingod](#tests-for-zingod)) gates the zingod pin, and every zebrad pin with it.
  `real_zeeder.rs` (Linux): `print-config`, SOA, NS and A from loopback, REFUSED out
  of zone, `/health`, the metrics, SIGTERM.
- **Wallet acceptance**, a RELEASING.md checklist before PS1's release on Testnet and Mainnet,
  at every zebrad pin while zebrad serves and for every zingod pin, on a real home connection:
  Zingo through a scripted `zingo-cli` (a new wallet, an old birthday, a send with a memo), and
  Zodl Android, Zodl iOS and Zkool by hand (Custom in Manual mode, a new wallet, a restore, a
  send, a transparent balance, the app sent to the background mid-sync). Each run records the
  first-sync time (zebrad's one-block-at-a-time reads; with zingod, also its index build) and
  the bytes sent. Testnet needs Testnet wallet builds; that Zodl publishes one is unconfirmed.
- **By hand**, per release touching the flow: a TP-Link or FRITZ!Box with UPnP on and off,
  carrier-grade NAT through a phone's hotspot, macOS's Local Network prompt, ufw on.
- **The mock** (`ui/src/mock/public.rs`) runs the pure plan over fake facts, combines with
  `?herd`, and fixes `ui/src/mock/herd.rs`, which gives Zaino a JSON-RPC port (8237) that Zeal
  keeps off. Flags, listed in `ui/src/mock.rs` and ui/README.md: `?public=setup`, `serving`,
  `unconfirmed`, `catchingup`, `eos`, `untested`, `moved`, `lan`, `cap`, `cgnat`, `doublenat`,
  `noforward`, `ratelimited`, `expiring`, `keychain`, `seeder` (replacing HERD.md's
  `?seeder=public`), `seeder-warmup`, `seeder-attack` and `portclash`, and from Z3 the `zingod*`
  flags ([zingod in the UI](#zingod-in-the-ui)). Screenshots come from the headless-Chromium
  workflow.

## Phases

Each phase is one pull request, or a short series, with its tests, mock flags and screenshots,
and ARCHITECTURE.md updated for what landed. PS1 ships on its own, on Zeal as it is today: one
supervised zebrad, today's `launcher.json`, no node registry and no HERD.md phase. After PS1 the
seeder (PS4) and the light client server's later phases are independent tracks: whichever
upstream gate opens first ships first.

| Phase | Ships | Needs | Screenshots from |
| --- | --- | --- | --- |
| **PS0 Decide and ask** | this document, the edits below, the upstream issues | — | — |
| **PS1 A light client server from zebrad (Linux)** | zebrad's endpoint as a managed loopback key, with the restart, the log line and the health checks; front door v1 in Zeal's process (`launcher-gateway`), its modes and v1 limits; `AcmeActor` (TLS-ALPN-01, staging then production, renewal) against Pebble in CI; the name step (DuckDNS, deSEC, by hand) with `DdnsActor` and the keychain; the read-only network checks and "Check my connection"; the forward by hand with the vendor guides; the firewall commands; the local, hairpin and phone tests and "Test now"; Open, pause and resume; keep-awake; the types, the pure logic and the screens (consent, setup, panel, Share, tray items, banners); Testnet and Mainnet, labelled experimental | today's single supervisor and settings: the endpoint's port is a default with a free-port check, not MULTI-NODE.md's allocator, and the screens sit in Settings → Public services and on the node's page; Zeal's wallet runs passing on Testnet and Mainnet before the release | `?public=setup`, `serving`, `unconfirmed`, `cgnat`, `expiring`, `keychain`, `consent` |
| **PS2 Front door v2** | HTTP/2 ended in the front door: the allow-list (no reflection), per-method limits, deadlines, header stripping, connection age with GOAWAY, trailers-only errors, the browser page, calls per method in the status panel | PS1; the first-sync times and bytes from PS1's wallet runs | `?public=serving`, `ratelimited` |
| **PS3 macOS** | the light client server on macOS: the Local Network prompt and `NSLocalNetworkUsageDescription`, `caffeinate`, the firewall checks | PS1; how Tauri 2 merges a `src-tauri/Info.plist`, checked | `?public=setup` on macOS |
| **PS4 The public DNS seeder (Linux)** | the bundled `zeal-zeeder`; the domain step (Cloudflare, deSEC, by hand, a dynamic-DNS NS target), the port 53 guide, firewall help, warm-up, Open, the probe, the watchdog, the panel, Share, redaction, on PS1's shared flow | HERD H5 (the zeeder kind, census, the 8233 dialog, on MULTI-NODE phase 4); PS1; zeeder's crawler setting, or the 8233 dialog | `?public=seeder`, `seeder-warmup`, `seeder-attack`, `portclash` |
| **PS5 zingod as the backend** | Z1 to Z5 ([Phases for zingod](#phases-for-zingod)), in place of the Zaino plan: `zeal-zingod` bundled on Linux and run by the light client server, chosen at consent; Testnet first, then Mainnet after a measured gate | PS1; neither PS2 nor MULTI-NODE phase 5 | `?public=zingod-choose`, `zingod-building`, `zingod` and one flag per row of [What can go wrong](#what-can-go-wrong-and-what-the-user-sees) |
| **PS6 Reach and hardening** | Windows (after zebrad's stdin-close flag: the UAC firewall rule, keep-awake, stops); optional automatic mapping; DNS-01 and external port 9067; Cloudflare names for the light client server; IPv6 (pinholes, AAAA after an inbound check, zeeder on IPv6); macOS public seeding once zeeder stops on stdin close; the front door as a sidecar if Windows or a Landlock sandbox needs one; a second light client server (Testnet beside Mainnet) on its own port; a block-span cap; more DNS providers | PS1 | per item |

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
PUBLIC-SERVICES.md. On 2026-11-16 zingod took the Zaino items' place: MULTI-NODE.md's binaries
non-goal and decision 1 name `zeal-zingod`, and its Zaino section notes the move.

**PS0's upstream issues.** Zebra: testing the light-wallet endpoint with the SDKs (#11158's
follow-up work); lightwallet-protocol v0.4.0 and v0.6.0 (`poolTypes`, `vin` and `vout`,
`LightdInfo`'s fields 16 to 18, `GetStandardFee`) and the build fields of `GetLightdInfo`;
pipelined `GetBlockRange` reads; an error, not `OK`, when a stream gives up on a slow client;
the HTTP/2-only listener as a documented promise; a listener only the front door can use (a
Unix socket, or authentication). zeeder: a crawler `listen_addr` that takes loopback or port 0
and is never advertised (with #106), and a `cache_dir`; rate-limit drops logged as one line
without addresses; an overall response cap; limits by /24 and /56; TC=1 slip; no RFC 1918,
CGNAT or ULA addresses in answers; a stop on stdin close; macOS and Windows builds. Zaino,
before the move to zingod: a release with #1655; fixes for #1665 and #1537; a
`ZAINO_BUILD_USER` override; an h2 bump in the 0.10 line. zingod:
[Upstream asks for zingod](#upstream-asks-for-zingod).

## Open questions for the maintainer

1. **Serve PS1 from zebrad's experimental endpoint**, though the Zebra book says not to depend
   on it for a service run for others yet, and not to run it on a user's machine? → Yes: on
   loopback, only while the service is on, behind the front door, labelled experimental, with
   the local risk said at consent and Zeal's wallet runs as the test; zingod joins it as a
   second backend (Z1 to Z5).
2. **Mainnet in PS1, on front door v1?** → Yes, once the wallet runs pass on Mainnet: zebrad's h2
   has the RUSTSEC-2026-0258 fix, its subtree roots cost a hash lookup each, and the front door
   goes `Unavailable` when zebrad falls behind; per-method limits follow in PS2.
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
   no binaries beyond zeeder's Linux ones, and a local build leaks its builder's name. zingod
   takes zainod's place (question 18).
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
    with measured load; the same for zebrad's endpoint and for the zebrad zingod reads.
11. **No index of Zaino's own at first**, never on Windows? → Yes, while releases are in the 0.10
    line; the rewrite always keeps one, as zingod does.
12. **"Test now" through a Let's Encrypt staging order**, the last authorization deactivated and
    the order never finalized, at most every 10 minutes: acceptable use? → Yes, on a click only.
13. **Defaults**: the limits, the 300 GB and 50 GB caps, the seeder's pause thresholds,
    "resume when Zeal starts" on? → As proposed, tuned by the wallet runs.
14. **Keep-awake on by default, and tauri-plugin-autostart** for "Start Zeal when I sign in"
    (licence unconfirmed)? → Keep-awake yes; the plugin is the maintainer's call.
15. **Laptops**: offer the switches with a warning, or hide them? → Offer, with the warning.
16. **Wallet acceptance**: which Zodl and Zingo versions and devices are the reference, and is a
    scripted `zingo-cli` run enough before PS1's release and for zingod's Testnet gate?
17. **Zaino on front door v1** if its NU7 release is the rewrite, with its own `[grpc]` limits?
    → No, v2, for the allow-list, the header stripping and the re-framing. For zingod 1.1.0
    (2026-11-16): → v1, for the reasons in [The front door for zingod](#the-front-door-for-zingod);
    v2 follows for both backends.
18. **Bundle `zeal-zingod` on Linux from Z1**, revising MULTI-NODE.md's "Zeal bundles only
    zebrad"? → Yes, as asked: upstream publishes no binaries, and a non-expert can't build
    zingod. Linux installers grow by about 25 MB, and nothing runs zingod until Z3; if Z3 slips,
    Z1 can leave out only the `tauri.linux.conf.json` line and still build and attest.
19. **The default backend for new setups** once Mainnet passes Z4's gate? → Zebra's built-in
    server until then; after it, zingod, for Zingo wallets and its own limits, with Zebra's
    offered for small disks. The maintainer's call (`DEFAULT_BACKEND`).
20. **Mainnet with zingod**, which says it is not for production Mainnet use? → After Z4's
    gate, labelled experimental, as PS1 did with zebrad's endpoint.
21. **zingod owned by the light client server now, a node kind later?** → Yes: the kind adapter
    doesn't exist yet, and one owner keeps the front door and zingod in order.
22. **The five git dependencies on one person's forks**: the source archive on each Zeal
    release, or mirrors under the Foundation too? → The archive from Z1; a mirror is cheap
    insurance.
23. **zingod's environment cleared**, an exception to ARCHITECTURE.md's "never `env_clear`"?
    → Yes, as for Zallet: Linux only, and zingod starts no program.
24. **Advisories in zingod's `Cargo.lock`**: report them in the build's summary and triage them
    at each bump, or fail the build? → Report: Zeal can't patch zingod's dependencies, and a
    failing build would block unrelated releases.
25. **Licence notices**: zingod's, lightwallet-protocol's and the four C libraries' texts,
    without a list of every Rust crate, the gap zebrad's bundle already has? → Yes for Z1; a
    Zeal-wide inventory is a follow-up.

## Sources

- **This repository** (`main`): MULTI-NODE.md, HERD.md,
  ARCHITECTURE.md, SECURITY.md, SIDECARS.md; `crates/launcher-core/src/config.rs` and
  `logs.rs`; `crates/launcher-types/src/nodes.rs` and `lib.rs`;
  `ui/src/components/add_node.rs`, `ui/src/mock.rs`, `ui/src/mock/herd.rs`;
  `src-tauri/tauri.conf.json`, `tauri.linux.conf.json`, `src-tauri/Cargo.toml`; `Cargo.toml`,
  `Cargo.lock` (with `cargo tree -i rustls` and `cargo metadata`, 2026-11-03), `deny.toml`,
  `clippy.toml`, `.github/dependabot.yml`.
- **zingod** at tag `v1.1.0` (`bfce8b2b`), read and built in research runs on 2026-11-16:
  `packages/zingod/src/` (`main.rs`, `cli.rs`, `daemon.rs`, `daemon/fd_limit.rs`, `config.rs`,
  `health.rs`); `packages/zingod-grpc/src/` (`service.rs`, `limits.rs`, `health.rs`,
  `transport.rs`, `proxy_protocol.rs`, `routes/info.rs`, `routes/transactions.rs`);
  `packages/zingod-source/src/` (`endpoint.rs`, `decode.rs`);
  `packages/zingod-sync/src/follower.rs` and `usage.md`; `packages/zingod-persistence/src/error.rs`;
  `packages/zingod-primitives/src/protocol.rs`; `packages/zingod-chainview/src/submit/driver.rs`;
  `docs/running.md`, `docs/architecture.md`, `docs/rpc_api.md` and `docs/client-requirements.md`;
  `README.md`, `CHANGELOG.md`, `Cargo.lock`, `LICENSE`, `rust-toolchain.toml`; the release and
  the tag through the GitHub API; Zingo Labs' Mainnet index snapshot manifest
  (`snapshots.zingolabs.dev`, read 2026-11-16). Zebra `v7.0.0`'s
  `zebra-rpc/src/server/http_request_compatibility.rs`, and commit `a1bf8a7e89` (in `v7.1.0`).
  zingolib's `crates/pepper-sync/src/client.rs` at `zingolib_v8.0.0` and at `dev` (`0a39640`).
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
