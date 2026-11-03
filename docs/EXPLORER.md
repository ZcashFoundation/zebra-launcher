# Block explorer

> **Status: P0 and P1 built, 2026-11-03; P2 onward is a proposal.** It builds on
> [MULTI-NODE.md](MULTI-NODE.md). [ARCHITECTURE.md](ARCHITECTURE.md) stays the description of
> what is built and is updated phase by phase as this lands. [Built so far](#built-so-far)
> says what P0 and P1 built and where the code differs from this design.

The maintainer asked for "a block explorer too, a highly performant one that offers highly
functional, flexible, and intuitive visualizations, search features, and UX". This document
proposes an Explorer tab on each node that reads only that node's own JSON-RPC.

[Decisions](#decisions) records each choice, what it replaces and why.

**Sources.** Zebra paths are at tag `v7.0.0` (commit e8db87e, as in `ZEBRAD_COMMIT`):
`methods.rs` = zebra-rpc/src/methods.rs, `types/` = zebra-rpc/src/methods/types/, `check.rs` =
zebra-consensus/src/transaction/check.rs, `zip317.rs` =
zebra-chain/src/transaction/unmined/zip317.rs, `subsidy.rs` =
zebra-chain/src/parameters/network/subsidy.rs. jsonrpsee 0.24.10, zcash_protocol 0.11.0,
zcash_address 0.14.0, zcash_primitives 0.31.0 and orchard 0.16.0 are the versions in Zebra's
Cargo.lock; Tauri 2.12.5 and reqwest 0.12.28 those in Zeal's. Zeal paths are on main at
2f8375c, which includes PR #88 (the Mainnet and Testnet tabs). "Unconfirmed" marks what the
sources do not settle.

## Goals and non-goals

**Goals**

- An Explorer for every node with a zebrad-compatible RPC, first the bundled zebrad 7.0.0:
  search, the chain, blocks, transactions, transparent addresses, the mempool, charts.
- Read only the selected node, over its cookie-authenticated JSON-RPC on 127.0.0.1.
- Show Zcash as it is built: six value pools and the flows between them, ZIP 317 fees and ZIP
  218 budgets as zebrad applies them, rules keyed to the node's own upgrade heights. A hidden
  amount is never shown as zero.
- Paint every navigation within one frame, use the cheapest call that answers, bound the load
  on the node.
- Search that never dead-ends.
- Keep the project's rules: Rust only, no mutexes (clippy.toml bans them), a thin Tauri layer,
  native tests, every page demoable with `trunk serve`.

**Non-goals**

- Decrypting anything, taking keys, showing shielded balances or memos: Zeal is not a wallet.
- Prices, peer maps or any other third-party data.
- Writing to the node: write methods cannot be expressed.
- A full chain index by default (an opt-in later phase).
- Linking transactions or guessing wallets. Wallets switch fee levels together so that the fee
  does not reveal their software (types/get_standard_fee.rs:13-18).

## Decisions

| Question | Decision | Instead of | Why |
| --- | --- | --- | --- |
| Fee rules | zebrad's: 1,000 zat marginal fee, ratio cap 10, no unpaid actions | the ZIP text's cap 4 and 50 unpaid | zip317.rs:23, :36, :50; the mempool rejects unpaid actions (:177-190) |
| Fees in lists | the exact ledger from v1 plus the coinbase, fetched once scrolling settles | v3 per row; a subsidy estimate; eager calls | v3 is the heaviest call; the identity is exact in every era |
| Missing `BlockInfo` | pool deltas only when the block and its parent have a row | zebrad's delta as is | zebrad diffs against zero when only the parent lacks one (methods.rs:1797-1806) |
| Big blocks | visible rows via `getrawtransaction(txid, 1, blockhash)` first | v2 first | v2 saves prevout reads, not JSON size |
| Mempool | `getrawmempool false` diff every 2 s, verbose on change | verbose every 3 s | txid lists are small |
| IPC | one request command, one job command, one event, JSON | 17 commands, view sessions, a binary codec | raw bodies over 1 KiB take the same fetch path as JSON over 8 KiB (tauri src/ipc/channel.rs:36-38); raw frames only if P0 needs them |
| Decoding | typed `Envelope<T>`, typed `ChainSource` | a source returning `Value` | rpc.rs:409-432 allocates every hex string |
| Scheduling | three lanes, heavy calls weigh 3, one 6-permit `Semaphore` only the actor takes from | separate pools | one budget on zebrad; permits free themselves on panic, lane counts settle through a drop guard |
| Pure code | a serde-only crate `explorer-model`; JSON parsing and TEX decoding in launcher-core | serde_json, bech32, sha2 in the wasm | `ui` has no serde_json (ui/Cargo.toml) |
| Store | per node; fixed grid and background sweep; final heights only | per network; writes on visits | reads only the selected node; must not mirror browsing |
| Home name | "Chain" | "Overview" | `Tab::Overview` is the several-node overview (ui/src/app.rs:36-46) |
| Sections | Chain · Blocks · Mempool · Charts | seven: Chain · Blocks · Mempool · Pools · Supply · Upgrades · This node | pools, supply and upgrades open from Charts, search and links; This node comes later |
| Strip | older left, newer right, next block last | next block first | matches ← → and time axes |
| Motion, texture | none; texture opt-in | slide-ins, default hatching | ui/styles/charts.css:4-5 |
| Logging | no ids at any level | ids at DEBUG | `RUST_LOG=debug` writes 14 days of files (src-tauri/src/logging.rs:5, :33) |
| Orchard from NU6.3 | "same receiver" for every Orchard action, by height | per transaction version | "regardless of transaction version" (zebra-consensus/src/primitives/halo2.rs:59-80); the RPC has no such flag (types/transaction.rs:791-797) |
| ZIP 318 | aggregates only | per-transaction "standard amount", `kind:migration` in the mempool | partitioning migrations per wallet is ZIP 318's threat |
| Phase order | blocks, transactions, live, addresses, charts, nodes | pools before addresses | addresses need only zebrad's address index, which it always keeps; pool pages are V12 charts over P5's samples |
| Provenance | zebrad's start log, bundled list as fallback | bundled list only | `checkpoint_sync` is configurable (zebra-consensus/src/router.rs:436-443) |
| Previews | complete queries after 150 ms; prefixes never reach the node | no lookup before Enter, or lookups after every 250 ms pause | a complete query gets one coalesced probe after 150 ms, which bounds the load (Resolution, step 2); zebrad looks up only complete ids, so prefixes match only what Zeal has seen (Classification, row 7) |
| Links, lookups | Copy only, no URL opened; spender results in memory | opening zips.z.cash; `spenders.bin` | one rule to audit; no record of lookups |

## What zebrad 7.0.0 answers

The bundled zebrad is a release build without the `indexer` feature (zebrad/Cargo.toml:65-74).
It always keeps a txid index and a transparent address index; it has no spent-by or statistics
RPC. "v1" and "v3" below are `getblock` verbosity 1 and 3; "cb" is `getrawtransaction(tx[0], 1)`.

| View | RPC | Cost | Caveats | Source |
| --- | --- | --- | --- | --- |
| Tip | `getbestblockheightandhash` | in memory | Zebra only | methods.rs:1980 |
| Chain facts | `getblockchaininfo` | 3 reads | polled by the monitor every 5 s (crates/launcher-core/src/monitor.rs:22); no chainwork, no NSM | methods.rs:1351 |
| Block summary | v1 | 8 reads, no decoding; ≥ 3.5 KB JSON (2,688-hex solution) + 66 B per txid | best chain only; `size`, `valuePools`, `chainSupply` need a `BlockInfo` row; zero-diff deltas when only the parent lacks one; empty trees omitted | methods.rs:1572-1838, :1797-1807, :5049-5057 |
| Coinbase | cb | cheap | per-pool coinbase amounts | methods.rs:2100 |
| Input values, fees | v3 | heaviest: re-reads every distinct parent; ~4× the block in JSON (unconfirmed) | no `fee` if a prevout is missing | zebra-state/src/service/read/block.rs:162-262 |
| Header | `getblockheader(x, true)` | 3 reads | `verbose` defaults to true in code | methods.rs:1841-1846 |
| Reward split | `getblocksubsidy(h)` | a calculation | needs the parent's pools once ZIP 237 reissuance starts; founders'-reward heights unconfirmed at runtime | methods.rs:3299-3396; subsidy.rs:515-518 |
| Transaction | `getrawtransaction(txid, 1, blockhash?)` | 2 reads + mempool | mempool, best chain, side chains (height −1); no fee; input `value` and `address` never set | methods.rs:2100-2279; types/transaction.rs:973-980 |
| Spent? | `gettxout(txid, n, false)` | 2 reads | best chain; no spender; `include_mempool=true` errors during sync | methods.rs:3641; zebrad/src/components/mempool.rs:1146 |
| Address | `getaddressbalance`, `getaddresstxids(start, end)`, `getaddressutxos(startHeight, maxEntries)` | grows with matches | transparent only, no mempool, TEX rejected; ≤ 10,000 addresses; > 100,000 matches → −8; txids oldest first | methods.rs:1483, :2500-2660, :4138-4142 |
| Address kinds | `validateaddress`, `z_validateaddress`, `z_listunifiedreceivers`, `z_converttex` | trivial | `z_converttex` is P2PKH → TEX only | methods.rs:3238-3250, :3403 |
| Mempool | `getrawmempool` false, true; `getmempoolinfo` | grows with size | verbose errors during sync; `fee` in ZEC (doc says zat) | methods.rs:1987-2091; types/get_raw_mempool.rs:35-60 |
| Wallet fee | `getstandardfee` | trivial | 5,000 zat per action on Mainnet below 3,590,000, then 1,000 (draft ZIP, zcash/zips PR 1352) | types/get_standard_fee.rs:8-34 |
| Note history | `z_getsubtreesbyindex(pool, 0)` | cheap | one entry per 2^16 notes | methods.rs:2402 |
| Difficulty, hash rate | v1 `difficulty`; `getnetworksolps(n, h)` | cheap per point | window 17 → 102 blocks at NU7 | methods.rs:3131 |
| By date, forks, spent-by, statistics | none | — | bisection; Zeal's fork log; `indexer` has no RPC; sampling | zebra-state/src/request.rs:1555-1560 |

**Server.** Batches run one call after another and one oversized element fails them all
(jsonrpsee server/src/server.rs:1316-1320); each element needs `"jsonrpc":"2.0"`
(zebra-rpc/src/server/http_request_compatibility.rs:128-156). At most 100 requests in flight,
shared by every client, then HTTP 429 (server.rs:71, :1040). Responses up to 50 MiB
(zebra-rpc/src/config/rpc.rs:129). No rate limit, timeout or backpressure: reads run on the
blocking pool and the read service is always ready (zebra-state/src/service.rs:1426, :1956), so
Zeal limits itself. Each call is its own snapshot. zebrad traces only method names
(zebra-rpc/src/server/rpc_tracing.rs:36). Zeal's client has one 10 s timeout (rpc.rs:25).

**Rules the explorer shows.** Heights come from the node's `getblockchaininfo.upgrades`;
constants are a fallback.

| Fact | Value | Source |
| --- | --- | --- |
| Mainnet upgrades | NU5 1,687,104 · NU6 2,726,400 · NU6.1 3,146,400 · NU6.2 3,364,600 · NU6.3 (Ironwood) 3,428,143 · NU7 3,543,405 | zebra-chain/src/parameters/constants.rs:80-107 |
| Reorg window | 1,000 blocks, a Zebra policy, not consensus | constants.rs:20-33 |
| Logical actions | max(⌈input bytes / 150⌉, ⌈output bytes / 34⌉) + 2 × JoinSplits + max(Sapling spends, outputs) + Orchard + Ironwood actions; conventional fee 1,000 × max(2, actions) | zip317.rs:23-32, :140-174 |
| Unpaid actions | max(0, max(2, actions) − ⌊fee / 1,000⌋); the mempool rejects any | zip317.rs:90-104, :140-174, :177-190 |
| Template | weighted random by min(max(1, fee) / conventional, 10), within bytes, 20,000 sigops, 0 unpaid, ZIP 218 from NU7 | zip317.rs:113-130; types/get_block_template/zip317.rs:56-180, :366-460 |
| ZIP 218, NU7 on | Orchard + Ironwood + Sapling spends and outputs + 2 × JoinSplits ≤ 330; Orchard, Ironwood ≤ 330 each; Sapling ≤ 300; JoinSplits 0 | zebra-consensus/src/block.rs:184-260 |
| NSM | burns ⌊6F / 10⌋ of block fees F from NU7; seed 368.58445520 ZEC (Mainnet), 557.68414957 (Testnet) | subsidy.rs:521-536, :723-748 |
| Coinbase | from NU6 equals subsidy plus claimable fees (ZIP 236) | zebra-chain/src/parameters/network/subsidy/check.rs:262-279 |
| Orchard, NU6.3 on | `valueBalanceOrchard` ≥ 0; no Orchard in a coinbase; each action's output goes to the receiver of the note it spends, in v5 and v6 | check.rs:184-260; zebra-consensus/src/primitives/halo2.rs:59-80; orchard src/bundle.rs:327-333; zcash_primitives src/transaction/components/orchard.rs:156-185 |
| Sprout | no new value from Canopy; no JoinSplits from NU7 | check.rs:333-358; ZIP 218's Sprout limit 0: zebra-consensus/src/block.rs:197, enforced in zebra-consensus/src/block/check.rs:301-330 and, for the mempool, zebra-consensus/src/transaction.rs:488-497 |
| Shielded coinbase | Sapling outputs still allowed; outputs must decrypt with an all-zero outgoing viewing key, so amounts are public by consensus | check.rs:277-315, :450-498 |
| Lockbox | funded from NU6; 78,750 ZEC disbursed at NU6.1 | zebra-chain/src/parameters/network/subsidy/constants/mainnet.rs:19-34 |

**Derived without heavy calls.** A block's pool change is the negated sum of its transactions'
value balances, with the lockbox and the NSM set apart (zebra-chain/src/block.rs:303-372), and
a coinbase's value balance is minus its outputs. So from v1 of a block and its parent plus cb:

- **Fees** F = Σ coinbase outputs (transparent outputs plus −`valueBalanceZat` per shielded
  pool) − Σ `valueDeltaZat` of the five pools other than the lockbox. Exact in every era, the
  NU6.1 disbursement included. **NSM share** ⌊6F / 10⌋ from NU7.
- **Notes created** per pool: the change in `trees.*.size` (missing = 0), one per Sapling
  output and Orchard or Ironwood action (protocol fact, not re-checked in zebra-state).
- **Users' flow** per pool: delta minus the coinbase amount. **Budget:** "≥ x of 330" until v3
  adds Sapling spends.
- **Checks:** NU6 to NU7, Σ six deltas = `totalblocksubsidy`; from NU7, subsidy − Σ = NSM
  share; before NU6 the gap is unclaimed value. A failed check shows "unverified".
- **NSM reserve** = seed + (Σ subsidy − Δ `chainSupply` since NU7) − reissued, which is zero
  until after the third halving (subsidy.rs:554-600, :806-828). zebrad tracks it
  (block.rs:358-367) but does not report it (types/get_blockchain_info.rs:129-130): "computed by
  Zeal".

## Information architecture and pages

### Placement

- **Tab:** `Tab::Explorer` after Logs: `ALL = [Dashboard, Logs, Explorer, Grafana, Configure,
  Settings]`, `NODE = [Dashboard, Logs, Explorer, Configure]` (today app.rs:77-86). Ctrl/Cmd+3;
  Grafana, Configure, Settings move to 4–6. Appending as Ctrl/Cmd+6 is the alternative (open
  question 1). `from_shortcut` already maps any digit (app.rs:140-148); `Tab::number`'s doc and
  the tests at app.rs:777-817 and ui/src/components/nav.rs:163 change.
- **Which nodes:** `NodeKind::has_explorer()` (crates/launcher-types/src/nodes.rs): zebrad and
  Zakura; the zcashd compatibility build once probed; not Zaino, Zallet, Zebend, Generic.
  `node_tabs` and its tab-leaving effect (ui/src/components/herd_header.rs:16-32) apply it.
- **Networks:** the Explorer belongs to the selected node, which is always on the network the
  window shows (the Mainnet and Testnet tabs, MULTI-NODE.md:312-316), so the Testnet tab has a
  Testnet explorer: amounts read TAZ, and the Testnet band stays over the page. "Open in Zebra ·
  Testnet" from another network's search moves the window to Testnet with that node.
- **Ctrl/Cmd+K** anywhere opens the selected node's Explorer with search focused; on Zaino,
  that of the node it reads.
- **Links in:** `AppState.explorer_request: RwSignal<Option<(NodeId, ExplorerRoute)>>`, like
  `log_filter_request` (app.rs:206-208), set by the dashboard's best block, the status strip's
  height, and log lines with `Height(N)` (ui/src/insights/fields.rs:56-65) or a 64-hex hash.

### Shell

- Back and forward, the search box (`/` and `Ctrl K` keycaps), Chain · Blocks · Mempool ·
  Charts.
- A source line: "Zebra · Mainnet · zebrad 7.0.0 · 127.0.0.1:8232 · tip 3,544,210 · 12 s ago",
  chips `final ≤ 3,543,210` and `ZEC` (`TAZ` on test networks), an ⓘ ("Everything here comes
  from your own node. Nothing you search leaves this computer."), "Show RPC", and unit and time
  toggles (ZEC or zat; local, UTC, Unix) kept in localStorage as ui/src/pages/grafana.rs does.
- One banner, most important first: stopped (Start, via `NodeState::start`), RPC not answering,
  syncing ("Your node has blocks up to 2,951,020 of about 3,120,007"), mempool inactive, reorg
  ("2 blocks replaced at 14:02"), pruned.
- Only the current route's page is mounted; other routes live in the route cache.

### Routes

| Route | Page | Phase |
| --- | --- | --- |
| (empty) | Chain | P1 |
| `blocks?below=H&f=…` | Blocks | P3 |
| `block/<height or hash>?tab=txs\|map\|raw` | Block; a future height counts down, then becomes the block | P1 |
| `tx/<txid>?out=n` | Transaction | P1 |
| `address/<addr>?tab=history\|utxos\|pending&below=H` | Address | P4 |
| `mempool?f=…&sort=…` | Mempool | P3 |
| `charts/<id>?range=…&x=height\|time`, `pool/<p>`, `upgrade/<name>` | Charts, Pool, Upgrade | P5 |
| `search?q=…` | Search results | P1 |
| `node` | This node | later |

`ExplorerRoute` and its text form live in launcher-types, so the backend's resolver returns
routes. Filters, sorts and ranges live in the route, so back and forward return the same rows.

### Pages

**Chain (home).** The block strip (V1). Tiles that are links: tip, with the last 100 blocks'
average interval against the target (75 s, 25 s from NU7); mempool; supply (issued; burned since
NU7, Σ subsidy − Δ `chainSupply` from block NU7 − 1; the computed NSM reserve); the next
upgrade's countdown; difficulty and Sol/s. "Where all ZEC is" (V9). 25 recent blocks: height,
age, interval, transactions, size, notes by pool, users' net shielding, fees, burn, budget. From
P3, 10 waiting transactions. Once the reader scrolls, a "3 new blocks" pill replaces moving
rows; while the node syncs, nothing follows the tip.

**Blocks.** A virtual table over the whole chain, following the tip or pinned at a height;
"jump to height or date"; chips (`has:shielded`, `budget>=80%`, `notes:ironwood>0`, `burn>0`,
`replaced`, `since:nu7`) that filter the recent 10,000 blocks (the loaded rows until P5) and say
so; the heat strip (V5); an era label where an upgrade activates. New rows at the top keep the
reader's place.

**Block.** Height with ◀ ▶; hash with Copy; time and "x ago"; live confirmations; a finality
chip (Final from 1,000 deep, or Provisional, n of 1,000 deep, with n = tip − height); a
side-chain or "replaced" banner. A sentence: "37 transactions · 212 note commitments · users
moved 13.4 ZEC into shielded pools · fees 0.0021 ZEC (0.00126 burned)", plus counts by kind once
v3 lands. Then the turnstile (V2), the reward split (V3), the budget meter (V4; size before
NU7), the transactions table beside the block map (V8), folded details (version, roots,
commitments, tree sizes, bits, nonce; the solution in the raw JSON on request), raw JSON on
request (≤ 4 MB), Show RPC. The table: index, txid, kind badge, flow glyph (t→z…), pools, net
per pool, fee, fee per action, multiple, logical actions, size; 200 a page, sorted and filtered
in the backend. Edge cases: a big block ("Reading 4,812 transactions from your node…" over the
summary), a replaced block, the tip (▶ waits for the next one), no `BlockInfo` (said so), a
future height (countdown summed over eras, the upgrade there, and the node's end of support).

**Transaction.** A status line with the confirmations meter (V7): confirmed; waiting ("3 min ·
pays 5× the conventional fee"); side chain; or "left your mempool at 14:05 without being mined",
when seen this session. A sentence: "Moved 100 ZEC from Orchard to Ironwood. 2 Orchard and 2
Ironwood actions; amounts per note hidden. Fee 0.00004 ZEC = 4 logical actions × 1,000 zat." The
flow diagram (V6). A card per shielded pool: counts, net value as a movement, "amount per note:
hidden", rule notes (Orchard since NU6.3). Transparent inputs (previous output, address, value,
coin age, coinbase or not) and outputs (address, value, script, spent status, "Find the spending
transaction"). A fee card: fee, actions broken down, conventional fee, per action, multiple,
ratio capped at 10, and for waiting ones "zebrad picks transactions at random, weighted by this
ratio" and "wallets pay 5,000 zat per action until Mainnet 3,590,000". Details: version, expiry
with countdown, lock time, size, authdigest, branch, raw hex. A waiting transaction is followed
until a polite live region announces its block.

**Address.** Transparent and TEX: the address in groups, Copy, a kind chip, "Looked up on your
node only"; balance, received, sent (received − balance), "≥ N" transactions, first and last
seen; the balance chart (V10); tabs History (newest first, signed change and balance after;
`in`, `out`, date filters), Unspent (200 a page), Pending (from the mempool tracker). TEX shows
its P2PKH form under a ZIP 320 banner. Unified: its receivers; transparent ones link with their
balance; shielded ones say "no public history, by design", with what a viewing key would reveal
and that Zeal never asks for one. Sapling: an explanation and the checksum. Sprout: "closed to
new value since Canopy; frozen since NU7, when blocks may hold no JoinSplits".

**Mempool.** Tiles: count, bytes, fees, oldest, budget units waiting ("≈ N blocks"), median fee
per action, and "Every transaction here pays at least zebrad's conventional fee", true since
unpaid actions are rejected. V11's histogram and scatter. A live virtual list: age since first
seen, txid, kind, pools, fee, multiple, actions, size, expiry in blocks, `depends` chips;
filters `kind:`, `pool:`, `v:`, `expires<10`, `age>10m`; while pointer or focus is in the list,
updates wait ("8 changes · Resume"). In mempool lists (this page and the Chain page's waiting
transactions), every transaction with no transparent parts reads kind "shielded" with no pool
breakdown, whether it stays in one pool or moves between pools; `kind:shielded` matches all of
them, and `pool:` and `sort=` do not use their shielded pools. While syncing: "Your node's
mempool starts when it reaches the tip."

**Charts, pools, upgrades.** A catalog in four groups (Shielded pools, Supply, Chain, Network);
each card is badged "exact from samples", "sampled every s blocks" or "recent 10,000 blocks" and
opens a V12 chart. A pool page: balance, share, change over 1, 7 and 30 days split into coinbase
and users, a lifecycle chip (Sprout frozen since NU7; Sapling ≤ 300 spends and outputs a block;
Orchard drain-only since NU6.3; Lockbox funded since NU6; Ironwood open since NU6.3), subtree
milestones, its largest days but never its largest transactions. An upgrade page: branch id,
height or countdown, status, "what changed" from Zebra's CHANGELOG at v7.0.0, before-and-after
panels.

**Search results.** Only for an ambiguous query or one that cannot be resolved: candidates with
previews, or the reason and its action.

**This node (later).** Provenance (V13), the fork log with replaced blocks, block arrival
latency, mempool overlap, the methods the node answers.

## Search

### The box

An ARIA combobox in the shell. Ctrl/Cmd+K anywhere; `/` in the Explorer outside text fields.
Both are free: the shell binds Ctrl/Cmd+digits (app.rs:536-547), Configure binds Ctrl/Cmd+S.
Before classifying: trim; drop surrounding quotes, angle brackets or backticks; take the address
of a `zcash:` URI (ZIP 321) and drop its amount, memo and label; take a pasted URL's last
segment, `?q=` or fragment when that classifies; take the id from a JSON fragment; accept Zeal
routes.

### Classification

`explorer_model::classify_query(input, network) -> Classified` is pure, under 1 ms and
table-tested; the UI runs it on every keystroke, the backend first. First match wins. `B58` is
`[1-9A-HJ-NP-Za-km-z]`, `B32` is `[02-9ac-hj-np-z]`.

| # | Class | Pattern | Leads to | Behind it |
| --- | --- | --- | --- | --- |
| 1 | Secret | the list below | a refusal; the field clears | nothing |
| 2 | Height | `^[0-9]{1,9}$` after removing `,` `_` `'` and spaces; no leading zero | `block/<h>`; past the tip, "past your node's tip" or a countdown | the chain index, else `getblock("<h>", 1)` |
| 3 | Unix time | `^[0-9]{10}$` | first block at or after it | as row 14 |
| 4 | Relative | `^tip$`, `^latest$`, `^tip-[0-9]+$`, `^-[0-9]+$` | `block/<h>`; `-1` is the tip, as in `getblockhash` (methods.rs:543) | the live tip |
| 5 | Hash | `^[0-9a-fA-F]{64}$` | the block or transaction that exists; a choice if both | caches and fork log, then `getblock(x, 1)` and `getrawtransaction(x, 1)` at once |
| 6 | Outpoint | `^[0-9a-fA-F]{64}[:-][0-9]{1,5}$` | `tx/<txid>?out=n` | as row 5 |
| 7 | Prefix | `^[0-9a-fA-F]{6,63}$` with a letter a–f or a leading zero | suggestions only | memory, never the node |
| 8 | Transparent | Mainnet `^t1B58{33}$` (P2PKH), `^t3B58{33}$` (P2SH); Testnet and Regtest `^tmB58{33}$`, `^t2B58{33}$` | `address/<a>` | `validateaddress`, then the address calls |
| 9 | TEX | `^tex1B32{38}$`, `^textest1B32{38}$`, `^texregtest1B32{38}$` | `address/<p2pkh>` with a TEX banner | converted in launcher-core; `z_converttex(p2pkh)` must return the input (methods.rs:3250) |
| 10 | Sapling | `^zs1B32{75}$`, `^ztestsapling1B32{75}$`, `^zregtestsapling1B32{75}$` | the Sapling explanation | `z_validateaddress` |
| 11 | Unified | `^(u\|zu\|tu)1B32+$`, `^(u\|zu\|tu)test1B32+$`, `^(u\|zu\|tu)regtest1B32+$` (ZIP 316 Revision 0 `u`; Revision 2 `zu` shielded-only, `tu` with transparent receivers) | the unified page | `z_listunifiedreceivers` (methods.rs:3403) |
| 12 | Sprout | `^zcB58{93}$`, `^ztB58{93}$` | the Sprout explanation | nothing |
| 13 | Payment request | `^zcash:` | the address it names | as that address |
| 14 | Date | `^\d{4}-\d{2}-\d{2}([ T]\d{2}:\d{2}(:\d{2})?)?(Z\|[+-]\d{2}:?\d{2})?$`, local without a zone | first block at or after it, with neighbours | interpolate over stored samples, then bisect `getblockheader`: 3–6 calls, ≤ 23 without the store |
| 15 | Pool | `transparent`, `sprout`, `sapling`, `orchard`, `ironwood`, `lockbox` | `pool/<pool>`; a same-named upgrade second | nothing |
| 16 | Upgrade | `overwinter`, `blossom`, `heartwood`, `canopy`, `nu5` … `nu7` | `upgrade/<name>` | the upgrades map |
| 17 | ZIP | `^zip[ -]?[0-9]{1,4}$` | the view it drives (317 fees, 218 budget, 235 burn, 318 migration…) | nothing |
| 18 | Command | `>mempool`, `>latest`, `>copy link`, `>utc`, `>node <id>`, `>shortcuts` | runs it | nothing |

Heights above 999,999,999 (zebrad accepts up to 2^31 − 1) read as Unix time: no such height
exists for centuries at 25 s blocks. Prefixes are zcash_protocol 0.11.0's
src/constants/{mainnet,testnet,regtest}.rs and, for ZIP 316 Revision 2 (`zu`, `tu`, `uvf`,
`uvi`), zcash_address 0.14.0's src/constants.rs; lengths follow from the payloads (26 bytes
base58check, 20 for TEX per ZIP 320, 43 for Sapling, 70 for Sprout; unified has no fixed length,
ZIP 316). zebrad checks checksums. Another network's prefix gives "this is a Testnet address".

### Refused input

```text
zxviews1…  zxviewtestsapling1…  zxviewregtestsapling1…   Sapling viewing keys
uview1…    uviewtest1…          uviewregtest1…           unified full viewing keys
uivk1…     uivktest1…           uivkregtest1…            unified incoming viewing keys
uvf1…      uvftest1…            uvfregtest1…             unified full viewing keys (ZIP 316 Revision 2)
uvi1…      uvitest1…            uviregtest1…             unified incoming viewing keys (Revision 2)
secret-extended-key-main1…  -test1…  -regtest1…          Sapling spending keys
```

From zcash_protocol 0.11.0's constants and, for ZIP 316 Revision 2, zcash_address 0.14.0's
src/constants.rs. By heuristic too: Sprout keys `SK…`, `ST…`, `ZiVK…`, `ZiVt…`
(zcashd's encodings, unconfirmed); WIF keys, base58 of 51 characters from `5` or `9` or 52 from
`K`, `L` or `c` (derived from version bytes 0x80 and 0xef, unconfirmed); 12, 15, 18, 21 or 24
lowercase words. The field clears; nothing is stored, logged or sent: "This looks like a viewing
key (or spending key, or recovery phrase). Zeal never needs it, and it was not saved. Anyone who
has a spending key or recovery phrase can spend your funds."

### Resolution

1. Every keystroke: classify and show a hint (`aria-describedby`, polite): "Height · your node
   is at 3,601,234", "Unified address · lists its receivers", "Viewing key · Zeal never takes
   keys".
2. After 150 ms without typing, a complete query (rows 2–6, 8–17) sends `Resolve { preview:
   true }`: at most one cheap probe, coalesced, stale replies dropped by a generation counter.
   The hint becomes a preview: "Transaction · Shielding · mined in 3,120,000 · 4 min ago".
   Prefixes never reach the node.
3. Enter navigates at once for an unambiguous class, so a skeleton paints in the same frame. An
   unanswered hash opens `search?q=…`, which replaces itself with the right page, so back skips
   it. A spinner shows only after 300 ms.

### Not found, with a reason

| Reason | Says and offers |
| --- | --- |
| `NotMinedYet` | a countdown; past the node's end of support, "your zebrad stops at block X" |
| `PastNodeTip` | "Your node has blocks up to H", progress, a link to the Dashboard |
| `OtherNetwork` | "This is a Testnet address; this node is on Mainnet", "Open in Zebra · Testnet" per node |
| `ShieldedByDesign` | Sapling and Sprout addresses, shielded receivers |
| `BadChecksum`, `WrongLength` | "A character may be wrong"; "t1 addresses are 35 characters; this one has 34" |
| `Pruned` | "Your Zakura node keeps transactions for the last 10,000 blocks" |
| `SideChainUnknown` | "zebrad looks up blocks by hash in its best chain only; Zeal's fork log has the replaced blocks it saw" |
| `UnknownTxid` | "Not in your node's chain or mempool; it only knows transactions it has received" |
| `Unsupported` | names the missing method (other node kinds) |

### Suggestions and history

- **Suggestions:** Go to (the result); Recent; Seen by Zeal (hash prefixes over L2, the mempool
  snapshot, the fork log and the recent window: a sorted `Vec<(u64, u32)>` of first 8 bytes,
  binary-searched, ≤ 8 results within 5 ms, labelled "only what Zeal has seen"); Commands; "Jump
  to date…". An empty field offers recent searches, "Latest block", "Mempool", "Charts".
- **History:** a `NavHistory` per node, at most 100 entries, push drops forward entries, back
  restores scroll and focus. Not the webview's history: tabs and nodes are not browser history.
- **Recent searches:** 10 per node, in memory, cleared on quit, removable; secrets never enter.
  Keeping them on disk is open question 5; off by default, since a search history reveals which
  addresses are yours.

### Keyboard shortcuts

Single keys work outside text fields only, and a setting turns them off (WCAG 2.1.4). A test
checks for collisions with Ctrl/Cmd+digits and Ctrl/Cmd+S.

| Keys | Action |
| --- | --- |
| Ctrl/Cmd+K, `/` | search |
| ↑ ↓, Enter, Tab, Esc | suggestions, go, complete a command, close (a second Esc leaves) |
| Alt+← Alt+→ (⌘[ ⌘]), mouse back and forward (`MouseEvent.button` 3, 4) | back, forward |
| `[` `]`; ← → when the page has focus | previous, next block; past the tip, the next-block tile |
| `.` | latest block |
| `g` then `h`, `b`, `m`, `c` | Chain (home), Blocks, Mempool, Charts |
| j k, PgUp PgDn, Home End, Enter | move in a list, open |
| `c`, `l`, `t` | copy the id, copy the link, cycle the time format |
| `?` | all shortcuts and ten one-line concepts (confirmation, coinbase, mempool, pool, turnstile, note commitment, logical action, conventional fee, shielded budget, finality) |

**Filters in lists.** One grammar, `ListFilter::parse` in explorer-model, shared with the
backend; chips are buttons whose text lives in the route: `kind:` (`shielding`, `deshielding`,
`shielded`, `migration`, `transparent`, `mixed`, `coinbase`), `pool:orchard`, `v:6`, `fee>=2x`,
`fee<5x`, `actions>10`, `value>1.5`, `size>100k`, `height:3.1M..3.2M`, `since:nu7`,
`has:shielded`, `budget>=80%`, `notes:<pool>>0`, `burn>0`, `replaced`, `after:2026-10-01`, `in`,
`out`, `expires<10`, `age>10m`. The mempool has no `kind:migration`.

## Visualizations

### Rules

- **Pool colours are identities.** Tokens in ui/styles.css, dark under `:root`, light under
  `:root[data-theme="light"]` (styles.css:3-8, :74-106):

  | Pool | Token | Light | Dark |
  | --- | --- | --- | --- |
  | Transparent | `--pool-transparent` | #eb6834 | #d95926 |
  | Sprout | `--pool-sprout` | #6250d6 | #9085e9 |
  | Sapling | `--pool-sapling` | #1baf7a | #199e70 |
  | Orchard | `--pool-orchard` | #2a78d6 | #3987e5 |
  | Ironwood | `--pool-ironwood` | #008300 | #008300 |
  | Lockbox | `--pool-lockbox` | #e87ba4 | #d55181 |

  This is the stack order: public first, Ironwood beside Orchard so migrations read as
  neighbours, the lockbox last (the `Pool` enum keeps zebrad's order). It passes the dataviz
  palette validator against `--panel` in both themes (worst adjacent colour-vision ΔE 17.6
  light, 13.0 dark). In light mode Sapling (2.82:1) and Lockbox (2.69:1) are under 3:1, so pool
  marks always carry labels or a legend and a table view. Orange, blue and aqua are the existing
  `--series-2`, `--series-1`, `--series-3` (styles.css:51-53, :102-104).
- **Two hues where any marks can touch** (block map, projection): public orange, shielded blue;
  a transaction crossing between them is split diagonally.
- **Sign:** blue into shielded pools, orange out, around grey zero; never red against green.
- **Hidden values** get no width, zero or empty cell: a lock and "hidden".
- **No motion:** a refetch dims the previous render; new items get a static dot for 10 s.
  Texture only with "Patterns in charts", `forced-colors` or print.
- **Every chart:** a title naming its subject, a legend (direct labels up to four series), a
  table view, `role="img"` with a one-sentence summary, focus that shows what hover shows, hit
  targets ≥ 24 px.
- **Rendering:** SVG up to about 1,000 marks, reusing ui/src/charts.rs and `LineChart`'s
  crosshair, keyboard and table (ui/src/charts/line_chart.rs); above, Canvas 2D for data under an
  SVG overlay, so hover never redraws data
  (https://echarts.apache.org/handbook/en/best-practices/canvas-vs-svg/). Device pixel ratio
  capped at 2; colours read once per theme change, redrawn on `AppState.resolved_dark`. No WebGL:
  support in the system webviews is unconfirmed. Long series use M4 (first, minimum, maximum,
  last per pixel column; https://www.vldb.org/pvldb/vol7/p797-jugel.pdf) instead of
  `downsample` (charts.rs:273), which drops spikes.

### Catalog

**V1. Block strip** (Chain; compact on Block). *Purpose:* what just happened, what waits.
*Data:* v1 of 12 blocks and a parent; cb per tile; the mempool tracker (P3). *Encoding:* tiles
older to newer, then a dashed next-block tile; each tile shows height, age, transactions, a bar
of notes created (Sapling, Orchard, Ironwood), six turnstile ticks, fees and "60 % burned" from
NU7, "≥ x/330", a "replaced" badge; under them, one bar per gap as tall as its seconds (capped
at 4× target) with a target line. *Interaction:* links; roving tabindex (← → Home End Enter);
hover cards; holds still under the pointer with "+1 new block". *Rendering:* DOM, 13 tiles keyed
by hash. *Budget:* ≤ 2 ms per new block; 1 v1 + 1 cb.

**V2. Block turnstile** (Block). *Purpose:* how value moved between pools, and that it balances.
*Data:* the block ledger. *Encoding:* six rows of bars above or below zero, each split into
"coinbase outputs (subsidy and claimed fees)" (light shade) and "users' transactions" (solid);
the lockbox row shows deferrals and the NU6.1 disbursement; rows for fees, miner share, NSM
share; the era's check as caption ("Pool changes + fees = coinbase outputs ✓"). *Interaction:*
exact zatoshis per row, a pool opens its page, table view. *Rendering:* SVG, ≤ 30 marks.
*Budget:* < 1 ms, no extra calls.

**V3. Reward split** (Block). *Purpose:* where the reward went. *Data:* `getblocksubsidy` and
the coinbase. *Encoding:* one stacked bar of miner subsidy, funding streams (named as
`getblocksubsidy` names them), lockbox, fees to the miner, NSM share; rows with payout
addresses; the coinbase's printable bytes, sanitised, "text chosen by the miner".
*Interaction:* hover, address links, table view. *Rendering:* SVG. *Budget:* one trivial call.

**V4. Budget meter** (Block; next-block tile). *Purpose:* fullness in Zcash's own unit since
NU7. *Data:* ZIP 218 counts from v3, or tree deltas as "≥". *Encoding:* a 0–330 bar split into
Orchard, Ironwood, Sapling spends and outputs, 2 × JoinSplits; ticks at 300 and 330; before
NU7, size against 2,000,000 bytes. *Interaction:* hover counts. *Rendering:* SVG. *Budget:*
none.

**V5. Budget heat strip** (Blocks). *Purpose:* fullness across many blocks. *Data:* loaded rows
and the recent window. *Encoding:* one column per block, one sequential hue. *Interaction:*
hover card, click, ← → cursor. *Rendering:* Canvas 2D, SVG cursor. *Budget:* ≤ 2 ms for 2,000
columns.

**V6. Transaction flow** (Transaction). *Purpose:* value is conserved; what is public; where
amounts hide. *Data:* inputs with prevouts, outputs, net per pool, fee and NSM split.
*Encoding:* left, transparent inputs grouped by address (top 12, then "N more"), pools that
lose value, and for a coinbase "subsidy" and "this block's fees"; right, transparent outputs,
pools that gain, and the fee, from NU7 about 40 % to the miner and 60 % burned (the split
applies to the block's total). Ribbons linear in zatoshis, ≥ 2 px. Pool nodes hold an outlined
box ("Orchard · 2 actions · amounts hidden"); value that stays in a pool gets no ribbon. Below,
"1.5001 ZEC in = 1.5 into Orchard + 0.0001 fee ✓" or "1 input's value is unknown".
*Interaction:* ribbon and table row highlight each other; inputs open the previous
transaction, outputs start the spender search; table view. *Rendering:* SVG from a pure, tested
`flow_layout`, ≤ 24 strands a side plus "N more", as mempool.space's bowtie (`maxStrands =
24`). *Budget:* layout ≤ 2 ms, ≤ 100 marks.

**V7. Confirmations meter** (Transaction; chip on Block). *Purpose:* how settled, in Zebra's
terms. *Data:* tip − height + 1, live. *Encoding:* log scale 1, 10, 100, 1,000; "final in Zebra"
from 1,001 confirmations (1,000 deep: height ≤ tip − 1,000), captioned "Zebra treats blocks
1,000 deep as final; this is Zebra's policy, not a consensus rule". *Interaction:* tooltip.
*Rendering:* SVG. *Budget:* none.

**V8. Block map** (Block, P5). *Purpose:* what fills a block; floods of similar transactions.
*Data:* from the v3 summary: size, logical actions, kind, pools. *Encoding:* squarified treemap,
area by actions or bytes, two hues with split tiles, coinbase outlined. *Interaction:* cards,
click, arrow keys between tiles, the selected tile highlights its row. *Rendering:* SVG to 500
tiles, then Canvas 2D with binary-search hit-testing; a placeholder grid from `nTx` first.
*Budget:* layout ≤ 5 ms, draw ≤ 3 ms for 5,000 tiles.

**V9. Where all ZEC is** (Chain; pool pages). *Purpose:* each pool's share and its last day.
*Data:* the monitor's pools; v1 at tip and tip − N, N the blocks of ≈ 24 h. *Encoding:* one
segmented bar with a legend; "Shielded: 34.2 % of issued supply"; under it a bar per pool for
the change, "includes mining rewards; last 3,456 blocks (≈ 24 h)". *Interaction:* hover, click
for the pool page, table view. *Rendering:* SVG. *Budget:* one cheap call, at most once a
minute.

**V10. Address balance** (Address). *Purpose:* the balance over time. *Data:* loaded rows; each
balance after = balance − newer changes. *Encoding:* a step line by height or date, a marker per
transaction, "since 12 Oct (loaded so far)". *Interaction:* hover, click; the history list is
the table. *Rendering:* SVG to 1,000 steps, then Canvas. *Budget:* ≤ 4 ms a redraw.

**V11. Mempool** (Mempool). *Purpose:* what waits, what it pays, later what zebrad would mine.
*Data:* the snapshot and tracker shapes. *Encoding:* fee multiples in four bins on one ordinal
hue, 1× to < 2×, 2× to < 5×, 5× to < 10× (wallets still paying 5,000 zat per action), ≥ 10×
(the ratio's cap); fee per action (log) against age; later, projected blocks as budget bars,
"as zebrad builds blocks; other miners may differ". *Interaction:* brushing bins filters the
list; hover; click. *Rendering:* SVG histogram; Canvas scatter to 2,000 points. *Budget:*
histogram < 1 ms, scatter ≤ 4 ms, 200 simulation runs over ≤ 5,000 transactions ≤ 10 ms native,
in `spawn_blocking`.

**V12. Charts** (Charts, pool and upgrade pages). *Interaction:* presets anchored to upgrades
(since NU7, NU6.3, NU5; 1 year; all), x by height or date, absolute or share, drag and wheel
zoom, pan, reset, crosshair, click for the nearest block, upgrade markers, table view, CSV from
P5. *Rendering:* `DenseChart` as in the rules, M4 frames of ≤ 4 values per pixel column; a pan
first shifts a cached frame 3× as wide. *Budget:* ≤ 8 ms a frame; a first frame at once,
refined ≤ 4 times a second; ≤ 150 ms from stored samples.

| Chart | Purpose | Data | Encoding |
| --- | --- | --- | --- |
| Pool balances | where value lives | `chainValueZat` at samples | stacked area; lines in share mode |
| Pool lifecycle | rules over time | samples, `pool_rules` | six small multiples, own y-scales, bands for open, drain-only, frozen |
| Turnstile flows | net shielding per period | balance differences, exact | diverging columns, "includes mining rewards"; users only in the recent window |
| Notes created | shielded activity | tree-size differences; `z_getsubtreesbyindex` (3 calls) | stacked columns, a step line of 65,536-note subtrees, "note commitments, not payments" |
| Issued supply | against the schedule | `chainSupply` | line, schedule dashed |
| NSM | what NU7 removes | Σ subsidy − Δ supply; the reserve | cumulative line and columns, "computed by Zeal" |
| Lockbox | deferred funds | its `chainValueZat` | step area |
| Block interval | timing | per block in the recent window; Δtime / Δheight beyond | dots over a target band; a 5 s histogram |
| Transactions, size, budget | load | the recent window | columns; budget against 330 |
| Difficulty, hash rate | mining | v1 `difficulty`; `getnetworksolps(120, h)` at ≤ 500 samples | lines, "sampled every s blocks" |
| Orchard → Ironwood (later) | ZIP 318 progress | users' flows, recent window | daily totals, a denomination histogram; aggregates only |

**V13. Provenance** (This node, later). *Purpose:* which blocks this zebrad accepts by
checkpoint and which it validates fully. *Data:* `max_checkpoint_height` from zebrad's INFO line
"initializing block verifier router" (zebra-consensus/src/router.rs:400-404); fallback, the
bundled lists' last heights, 3,522,460 (Mainnet) and 4,518,360 (Testnet). *Encoding:* one bar:
checkpointed (proof of work, Merkle root and height only: zebra-consensus/src/checkpoint.rs:627-664),
fully validated, provisional top 1,000; it states this binary's rule, since older blocks were
verified by whichever zebrad wrote them. *Interaction, rendering, budget:* hover; SVG; none.

## Privacy

| Shielded data | Shown | Never shown |
| --- | --- | --- |
| Sapling spends and outputs, Orchard and Ironwood actions, JoinSplits | counts | a note's amount, sender, recipient, memo; which spend pays which output |
| A pool's net value per transaction or block | as a movement: "1.2 ZEC entered the Ironwood pool" | zero or blank for a hidden amount |
| Note commitments | counts, "including padding wallets add" | the word "payments" |
| Shielded coinbase outputs | the per-pool amount from the value balance, public by consensus (check.rs:450-498) | decrypted notes |
| Unified addresses | receiver kinds; transparent receivers link | any shielded history ("no public history, by design") |
| Fees, transparent inputs and outputs | in full | — |

1. **Hidden is never zero:** values arrive as `Shown::{Public, Hidden, Unknown}`; `Hidden`
   carries no number and the formatter cannot print one (tested).
2. **Captions say what a total does not reveal:** "Aggregate pool flows do not identify users
   or prove that coins were sold."
3. **No keys:** refused before anything stores them.
4. **ZIP 318:** migrations only as daily totals and a denomination histogram; no list, sort,
   filter or search of individual migrations by amount, denomination, time or anchor, across
   blocks or in the mempool; no per-transaction "standard amount" flag. A block's table still
   shows each transaction's public kind. Mempool lists do not tell a migration from any other
   fully shielded transaction.
5. **No linking:** no "related transactions", no matching of shielding and deshielding amounts,
   no privacy scores, no wallet guesses.

**The app's own rules.**

- **Network:** the client reaches only the node's loopback RPC (reqwest without TLS, `no_proxy`:
  rpc.rs:275); the CSP keeps the webview on IPC (src-tauri/tauri.conf.json:35); the explorer
  opens no URL, so ids and ZIP references get Copy buttons, never "open in another explorer".
- **Logs:** no query, txid, address, hash or height at any level: the launcher's log file
  follows `RUST_LOG` and keeps 14 days (src-tauri/src/logging.rs:5, :33), and INFO and louder
  reach the Logs tab and `zebrad.log` (docs/ARCHITECTURE.md, "Launcher lines in the log
  stream"). DEBUG lines carry a request kind and a duration; errors drop their parameters.
- **Disk:** the store holds public block records at fixed heights and the recent window,
  filled independently of what the user opens; never addresses, txids, searches or spender
  results.
- **Diagnostics:** nothing from the explorer in `copy_diagnostics`. **Show RPC** shows request
  bodies on screen only, never the cookie.

## Backend

### Crates and modules

**`crates/explorer-model`** (new): pure, no I/O, no tokio, depends on `serde` and launcher-types,
builds for native and wasm. `Deserialize` derives and a counting visitor over `IgnoredAny` are
pure serde, so zebrad's shapes live here while byte parsing (serde_json) stays in launcher-core.
Modules: `shapes` (only the fields used; `hex`, `solution`, proofs and ciphertexts skipped;
shielded arrays counted without allocating; script lengths kept for ZIP 317; `BlockInfo` fields
optional), `ledger`, `classify`, `zip317`, `zip218`, `pools`, `upgrades` (`UpgradeHeights`,
`spacing_between`, explainers), `query`, `filter`, `flow`, `m4`, `template`, `glossary`, `views`
(shapes to launcher-types views) and `synth` (the synthetic chain for the mock, actor tests and
fake-zebrad).

**launcher-core** `src/explorer.rs` (handle, `spawn`, config) and `src/explorer/`: `actor.rs`
(modelled on updates/actor.rs), `sched.rs` (pure, tested without tokio), `source.rs`
(`ChainSource`, and `RpcSource` over a closed `ReadCall` enum so nothing from IPC names a
method), `fetch.rs` and `assemble.rs` (one async function per request), `cache.rs`,
`chain_index.rs`, `mempool.rs`, `address.rs`, `spender.rs`, `sampler.rs`, `store.rs`, and
`tex.rs` (a small bech32m decoder tested on ZIP 320's vectors, base58check through `bs58` 0.5.1
and `sha2` 0.10.9, both in Cargo.lock).

### The actor

One per node. It owns, with no locks: the source (rebuilt when `Status.rpc_addr` changes, as
`monitor::run` does), the `ChainIndex`, L2, the in-flight map, the scheduler and semaphore, the
mempool tracker, the store handle, jobs, interest, and a `watch::Sender<ExplorerLive>`. Its loop
selects over commands, fetch results, the status, stats and details watches
(`best_block_hash` every 5 s), the settings watch, a 2 s tip tick while visible and at the tip, a 2 s mempool tick
while watched, and an idle timer. Each request runs as a task that asks the actor for each key;
the actor answers from L2, joins a fetch in flight, or schedules one. Bodies over 1 MiB decode
in `spawn_blocking`, as does the simulation.

- Lazy: nothing until the first command. `NodeNotRunning` unless Running with an `rpc_addr`.
- A new `rpc_addr` or `started_at`: new source, fresh cookie, caches kept. A new chain or
  genesis hash: everything cleared.
- Syncing uses the UI's at-tip rule (within one block, or progress ≥ 99.99 %:
  ui/src/stats_history.rs:54-57), moved to launcher-types so both share it.
- Hidden: no own polling, no background work; L2 shrinks to 8 MiB after 10 minutes. Stopped:
  fully cached replies served with `stale: true`.

### RPC client (rpc.rs), one commit each

1. Typed decode, `Envelope<T> { result, error }` in one pass instead of the `Value` step
   (rpc.rs:409-432); the monitor gains too.
2. `OwnedSession` (client, URL and authorization in `Arc`s) for spawned tasks.
3. Per-call timeouts with `RequestBuilder::timeout`, which overrides the client's (reqwest
   src/async_impl/request.rs:286-291): 10 s cheap, 60 s heavy.
4. One cookie re-read on an HTTP error or empty body (a failed login is not a clean JSON 401:
   http_request_compatibility.rs:225-227).
5. Keep-alive, at most 8 idle connections; no HTTP/2 on loopback.
6. `call_batch` only if P0 shows a gain, only for tiny calls (`getblockhash`, `gettxout`), ids
   matched, a fallback to single calls, never heavy calls.

### Scheduling, coalescing and limits

In-flight work holds permits of one `tokio::sync::Semaphore` of 6. Only the actor calls
`try_acquire_many_owned`, so it sets the order; the permit moves into the fetch task and frees
itself when the task ends, even on panic. Lane counts live in the actor's scheduler and settle
when the task's drop guard reports `Done`.

| Lane | Carries | Cap |
| --- | --- | --- |
| Interactive | visible rows, headers, search probes | 6 |
| Prefetch | overscan, neighbours, hover cards, lazy coinbase calls | 2 |
| Background | samples, the recent-window sweep, tracker enrichment | 2, only at the tip, while visible, with nothing interactive waiting |

- Cheap calls weigh 1; heavy calls (v2, v3, wide `getaddresstxids`, transactions over 100 KB)
  weigh 3; one heavy call while interactive work waits, two otherwise. The newest interactive
  request goes first.
- Syncing: 4 permits, prefetch 1, background 0, no polling or tip following of its own. HTTP
  429: caps halve for 30 s, retries back off from 100 ms to 2 s.
- **Coalescing:** `inflight: HashMap<FetchKey, Vec<Waiter>>`; a queued key moves up a lane when
  an interactive request wants it; replies share `Arc`s.
- **Cancellation:** paged requests carry `Scope { view, generation }`; a newer generation drops
  older queued jobs (answered `Superseded`); a prefetch with no waiter left is skipped; calls
  already sent are never aborted, and their results fill the cache.
- **Limits:** pages of 50 blocks, 200 block transactions, 25 address rows, 200 UTXOs or mempool
  rows; 64 outputs per status call; parents: 16 at once, 1,000 in the prefetch lane, 200 for a
  waiting transaction; 1,000 spender candidates; 2,000 series columns; 20,000 tracked mempool
  transactions. Replies under 64 KB in normal use and 1 MB always; raw dumps ≤ 4 MB on request.

### Caches

L2 is an LRU weighted by bytes, 64 MiB (8 MiB idle); an entry over an eighth of the budget is
not cached.

| Entry | Key | Size | Invalidation |
| --- | --- | --- | --- |
| Block ledger | block hash | ~260 B | never: keyed by hash, only evicted |
| Block transactions (v3, reduced) | block hash | ~120 B per transaction | never |
| Transaction parts | txid | 1–4 KB | mined never; waiting when it leaves |
| Prevout | outpoint | ~80 B | never |
| Subsidy; final header time | height | 300 B; 8 B | never |
| Mempool snapshot | — | ~150 B per transaction | each poll, each tip |
| Address windows | address, window | ~70 B per txid | top window each tip; the view after 10 s |
| Output status; negative probes | outpoint; query | small | each tip |
| Spender results | outpoint | 32 B | memory only |

Never cached: confirmations, the tip's `nextblockhash`, best-chain membership, finality.

### Tip and reorgs

`ChainIndex` keeps final hashes (≤ tip − 1,000) in a sparse `BTreeMap` and the top 1,000 in a
`VecDeque<(height, hash, prev)>`.

- A new tip (the monitor's hash, or `getbestblockheightandhash` every 2 s): fetch its v1; if its
  `previousblockhash` is our tip, push it.
- Otherwise walk back up to 3 parents (each a useful v1); failing that, binary-search the
  volatile window with `getblockhash`, at most 10 calls (the chains agree up to the fork and
  nowhere above), then re-read the tip.
- Replace the entries above the fork, bump `epoch` once, move the replaced hashes and their
  ledgers into the fork log (last 100 events; persisted from P5), and publish a `ReorgNotice`.
  A gap over 1,000 (normal in sync) drops the volatile part without a walk.
- The UI drops height-keyed rows and routes at or above the fork; hash-keyed pages keep their
  content under "no longer in your node's best chain"; a transaction in a replaced block reads
  its status again.

### Derived data

- **Ledger** as above; without `BlockInfo` for the block or its parent, deltas and fees are
  `Unknown` and the Block page takes exact fees from v3.
- **Classification** by value flow: `net_out` is `valueBalanceZat` (Sapling, Orchard, Ironwood)
  or Σ(vpub_new − vpub_old) (Sprout); fee = T_in − T_out + Σ net_out.

  | Kind | Rule |
  | --- | --- |
  | Coinbase | a coinbase input; records the shielded pools it paid |
  | Transparent | no shielded parts |
  | Fully shielded | no transparent parts, one pool; `same_receiver` for Orchard from NU6.3 |
  | Migration | no transparent parts; one pool loses, another gains; the amount is public |
  | Shielding | value enters shielded pools; "with transparent change" if it has transparent outputs |
  | Deshielding | value leaves shielded pools for transparent outputs |
  | Mixed | anything else |

- **ZIP 317 figures** from script lengths; mined transactions "pay" or "pay less than zebrad's
  conventional fee", never "invalid".
- **Prevouts:** a mined transaction uses its block's cached v3; else up to 16 parents at once;
  more, the block's v3 if ≤ 512 KB, else parents in the prefetch lane to 1,000 with "Resolve
  all". A waiting one takes its fee from the snapshot and fetches up to 200 parents.
- **Address history:** `getaddresstxids` windows below the cursor, 10,000 blocks, ×4 while short
  of a page, halved on −8; 25 rows a page with their transactions and parents. Once the txid
  set is complete, only parents in it are fetched: no other input can spend from the address.
  Balance after row i = balance − Σ newer changes, read against one tip (re-read once if it
  moved).
- **Spender search,** a job: `gettxout(txid, n, false)`; non-null is unspent in the chain, then
  `gettxout(txid, n, true)` returns null if a waiting transaction spends it (not while syncing,
  when it errors), and the tracker names that transaction. If the first call returns null and
  the output has an address: `getaddresstxids` in windows upward from the output height (`start` = output height, then the
  previous `end` + 1; `end` = `start` + 10,000 blocks, ×4 while empty, halved on −8, as for the
  address history), inputs oldest first, stop at the match or the tip, ≤ 1,000 candidate
  transactions, progress events. Nulldata is unspendable; P2PK, multisig and nonstandard outputs
  are explained without a search.
- **Mempool tracker,** only while the Mempool page, the next-block tile, a waiting transaction or
  an Address page's Pending tab is shown, or while a spender job runs: the non-verbose diff every
  2 s, verbose on change (≤ every 2 s) and every 30 s, one `getrawtransaction` per new txid (≤ 50
  per tick), maps from output address and spent outpoint to txid, first-seen times, departures
  (mined; expired when `expiryheight` is non-zero and ≤ tip; else evicted). Until it has read
  every waiting transaction, Pending says "partial". Above 2,000 transactions the verbose call is
  skipped; transactions that arrive after that have no fee, so the histogram and the scatter
  count only those with a fee and say "partial", and the projection waits until every waiting
  transaction has a fee.
- **Projection (later):** 200 seeded runs of zebrad's selection (weighted random, parents first,
  bytes, 0 unpaid, ZIP 218 from NU7); sigops are not modelled, the RPC does not report them;
  skipped when everything fits one block.
- **Date search:** interpolate from stored (height, time) samples, bisect with `getblockheader`;
  times are not monotonic, so the first block with time ≥ t, shown with its neighbours.

### The local store and scanning

No full-chain scan by default. The store is small, public and rebuildable.

- **Where:** `<app data>/nodes/<node-id>/explorer/` via a new `Paths::explorer_dir(&NodeId)`,
  per node as MULTI-NODE.md lays out data: two nodes on one network can disagree.
- **Files:** `store.json` (`{format, network, genesis_hash}`; a mismatch wipes the folder);
  `s4096.bin`, `s256.bin` (records at those multiples); `recent.bin` (final blocks of the last
  10,000); `forks.json` (written with `fsutil::write_atomic`).
- **Record,** 192 B little-endian: height, flags (present, has `BlockInfo`, parent has it, has
  coinbase, verified), hash, time, size, transaction count, 3 tree sizes, 6 pool balances, 4
  coinbase amounts, difficulty, fees, NSM share, FNV-1a checksum (bad reads as missing).
- **Sizes on Mainnet:** `s4096` ~870 records (167 KB), `s256` ~13,900 (2.7 MB), `recent.bin`
  1.9 MB; cap 32 MiB.
- **Filling:** background lane only, paused while syncing or hidden: `s4096` (~870 v1), the
  recent window (20,000 calls once, then 2 a block), `s256` (~13,900 v1), each coarse to fine in
  bit-reversed order. Grid samples are v1 only, so their fees read unknown; finer samples for a
  zoom stay in L2.
- **Never:** a record because the user opened a block; a height above tip − 1,000; a record
  served above the node's tip. After a restart the top 1,000 are read again (~1,000 cheap
  calls), so nothing provisional is kept on disk.
- **At open:** the top hash is checked with `getblockhash`; a mismatch (a minority chain)
  binary-searches the last match and drops what is above.
- **Writes:** a store actor in `spawn_blocking`, merged every 2 s or 256 records; no new
  dependencies; "Clear explorer cache" in Settings; it moves to Node settings when MULTI-NODE
  phase 2 adds them.
- **Opt-in history index (later):** a resumable sweep of final heights into delta and varint
  columns, ~20 B a block (~70 MB, unmeasured), after P0: 3.6 M v1 calls carry ≥ 12 GB of
  mostly discarded hex.

### Several nodes

One explorer per node runtime. Until MULTI-NODE phase 2, `explorer_for` serves the primary under
the id of the network zebrad runs on, `NodeId::suggest(Zebrad, n, …)`, the form
`node_state::primary_info` uses with the settings' network. n is `NodeStats.chain` ("main" is Mainnet, "test"
Testnet); while zebrad is stopped it is the last chain zebrad reported, and before zebrad first
reports it is the settings' network (`SettingsStore::watch()`,
crates/launcher-core/src/settings.rs:132). The UI's id follows the settings, which the first-run
wizard and Settings change while Zeal runs, but zebrad changes network only at its next start.
In between, a call for the other id gets `NodeNotRunning` with a message saying zebrad changes
network at its next start, not `UnknownNode`, so a Testnet page never shows Mainnet data; other ids get
`UnknownNode`. When n changes, the actor drops its caches and reopens its store at
`paths.explorer_dir` of the new id, leaving the other network's store in place, so a switch
opens the other network's store instead of wiping this one. Prefixes, unit and upgrade heights
come from the chain the node reports. Then `NodeRuntime.explorer`, each with its own limits and
store. Other kinds are probed once per run (`rpc.discover`; −32601 remembered, as monitor.rs
does for `getdeprecationinfo`). Pruned Zakura (≥ 10,000 blocks kept) answers older lookups with
`Pruned`.

## Types and Tauri commands

**Wire types**, `crates/launcher-types/src/explorer.rs`, serde only, re-exported like `nodes`.
Amounts are integer zatoshis; f64 ZEC converts once with `zec_to_zat` (round(x · 1e8), exact for
amounts zebrad reports: types/zec.rs:47-88). Views not spelled out (`TxView`, `BlockView`,
`AddressView`, `MempoolView`, `ExplorerLive`, `ExplorerRoute`) follow the fields listed in Pages.

```rust
pub struct Hash32([u8; 32]);        // 64 lowercase hex on the wire, like NodeId's try_from/into
pub type BlockHash = Hash32;
pub type TxId = Hash32;
#[serde(tag = "by", content = "value", rename_all = "snake_case")]
pub enum BlockId { Height(u32), Hash(BlockHash) }
pub enum Pool { Transparent, Sprout, Sapling, Orchard, Lockbox, Ironwood }   // zebrad's order
pub struct PerPool<T>(pub [T; 6]);
#[serde(tag = "shown", rename_all = "snake_case")]
pub enum Shown { Public { zat: i64 }, Hidden, Unknown { why: UnknownWhy } }   // Hidden: no number
pub struct ChainView { pub height: u32, pub hash: BlockHash, pub epoch: u64, pub stale: bool }

pub struct BlockLedger {
    pub height: u32, pub hash: BlockHash, pub prev: BlockHash, pub time: u32, pub size: Option<u32>,
    pub tx_count: u32, pub trees: [u64; 3], pub notes: Option<[u32; 3]>,
    pub pools: Option<PerPool<u64>>, pub deltas: Option<PerPool<i64>>,  // None without both rows
    pub coinbase_in: Option<PerPool<u64>>, pub fees: Option<u64>, pub nsm: Option<u64>,
    pub budget: Budget, pub check: LedgerCheck,
}
pub struct TxShape {
    pub txid: TxId, pub index: u32, pub version: u8, pub size: u32,
    pub t_in: u16, pub t_out: u16, pub sapling: [u16; 2], pub orchard_actions: u16,
    pub ironwood_actions: u16, pub joinsplits: u16,
    pub net_out: [i64; 4],          // Sprout, Sapling, Orchard, Ironwood; + leaves the pool
    pub fee: Option<FeeFacts>, pub kind: TxKind,
    pub expiry_height: Option<u32>, // RPC expiryheight 0 (no expiry, ZIP 203) maps to None
}
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TxKind {
    Coinbase { shielded_to: Vec<Pool> }, Transparent, Shielded { pool: Pool, same_receiver: bool },
    Migration { from: Pool, to: Pool, zat: u64 }, Shielding { into: Vec<Pool>, transparent_change: bool },
    Deshielding { from: Vec<Pool> }, Mixed,
}
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TxStatus { Mempool { since: u64, depends: Vec<TxId> }, Mined { block: BlockHash, height: u32 },
    SideChain { block: BlockHash }, Left { at: u64, expired: bool } }

pub struct ExplorerCall { pub request: ExplorerRequest, pub scope: Option<Scope>, pub trace: bool }
pub struct ExplorerReply { pub chain: Option<ChainView>, pub response: ExplorerResponse,
    pub sources: Option<Vec<SourceCall>> }   // with `trace`: method, params, ms, bytes, cached
#[serde(tag = "q", rename_all = "snake_case")]
pub enum ExplorerRequest {
    Status, Interest { visible: bool, page: PageKind }, Chain,
    Blocks { below: Option<u32>, count: u16, filter: String }, Block { id: BlockId },
    BlockTxs { hash: BlockHash, offset: u32, limit: u16, filter: String, sort: TxSort },
    Tx { txid: TxId, resolve: bool }, Outputs { txid: TxId, vouts: Vec<u32> },
    Address { address: String, tab: AddressTab, cursor: Option<AddressCursor> },
    Mempool { filter: String, sort: MempoolSort, offset: u32, limit: u16 },
    Resolve { query: String, preview: bool }, Suggest { prefix: String },
    Series { chart: ChartId, range: RangeSpec, columns: u16 }, Pools, Supply, Upgrades,
    Prefetch { targets: Vec<PrefetchTarget> }, Raw { what: RawWhat }, CancelJob { job: JobId },
}
// ExplorerResponse: one variant per request (Status, Chain, Block, Tx, Resolve(Resolution), ...).
pub enum Resolution { Go(ExplorerRoute), Choose(Vec<Candidate>), Explain(NotFoundHint) }
pub enum ExplorerJob { FindSpender { out: OutPoint }, AddressHistory { address: String },
    FillSeries { chart: ChartId, range: RangeSpec }, Export { what: ExportWhat } }
pub enum JobEvent { Progress { done: u64, total: Option<u64> }, Spender(Option<TxId>), Done, Failed(ExplorerError) }
pub struct ExplorerError { pub kind: ExplorerErrorKind, pub message: String }   // like ApplyError
pub enum ExplorerErrorKind { UnknownNode, NodeNotRunning, Syncing { height: u32, estimated: u32 },
    PastTip { height: u32, node_tip: u32 }, NotFound { hint: NotFoundHint }, Refused { what: SecretKind },
    InvalidQuery, Unsupported { what: String }, Pruned { below: u32 }, Busy, Timeout, Superseded,
    Rpc { code: i64 }, Internal }
```

**explorer-model** exposes, among others: `classify_query(&str, Network) -> Classified`,
`block_ledger(&BlockV1, Option<&BlockV1>, Option<&Tx>, &UpgradeHeights) -> BlockLedger`,
`ledger_check`, `tx_shape`, `logical_actions(&Tx) -> u32` (the sum zebrad's
`conventional_actions` computes before its max(2, ·), zip317.rs:140-174),
`conventional_fee(u32) -> u64`, `weight_ratio(u64, u64) -> f32`, `nsm_share(u64) -> u64`
(`fees / 10 * 6 + fees % 10 * 6 / 10`), `pool_rules`, `spacing_between`, `m4`,
`simulate_template` and `zec_to_zat`.

**launcher-core:**

```rust
pub struct ExplorerInputs { pub status: watch::Receiver<Status>,
    pub stats: watch::Receiver<Option<NodeStats>>, pub details: watch::Receiver<Option<NodeDetails>>,
    pub settings: watch::Receiver<Settings> }
pub fn spawn(inputs: ExplorerInputs, config: ExplorerConfig) -> ExplorerHandle;
// ExplorerConfig: cookie file, `Paths` (the store follows the node's network), 6 slots,
// caps 2 and 2, heavy weight 3, 64 MiB cache, 8 MiB idle after 10 min, 32 MiB store,
// timeouts 10 s and 60 s, polls 2 s.
pub(crate) fn spawn_with<S: ChainSource>(inputs: ExplorerInputs, config: ExplorerConfig,
    new_source: impl Fn(SocketAddr, PathBuf) -> Result<S, RpcError> + Send + 'static) -> ExplorerHandle;

#[derive(Clone, Debug)]
pub struct ExplorerHandle { cmd: mpsc::Sender<Cmd>, live: watch::Receiver<ExplorerLive> }
impl ExplorerHandle {
    pub async fn call(&self, call: ExplorerCall) -> Result<ExplorerReply, ExplorerError>;
    pub async fn job(&self, job: ExplorerJob, sink: Box<dyn JobSink>) -> Result<JobId, ExplorerError>;
    pub async fn export(&self, what: ExportWhat, path: PathBuf, sink: Box<dyn JobSink>) -> Result<JobId, ExplorerError>;
    pub fn live(&self) -> watch::Receiver<ExplorerLive>;
}
pub trait JobSink: Send + 'static { fn send(&self, event: JobEvent) -> bool; }   // false: gone

/// Typed reads into explorer-model's shapes; RPITIT like UpdateBackend.
pub trait ChainSource: Clone + Send + Sync + 'static {
    fn tip(&self) -> impl Future<Output = Result<(u32, BlockHash), RpcError>> + Send;
    fn block_v1(&self, id: BlockId) -> impl Future<Output = Result<shapes::BlockV1, RpcError>> + Send;
    fn block_v3(&self, hash: BlockHash) -> impl Future<Output = Result<shapes::BlockV3, RpcError>> + Send;
    fn tx(&self, txid: TxId, block: Option<BlockHash>) -> impl Future<Output = Result<shapes::Tx, RpcError>> + Send;
    // block_hash, header, mempool_txids, mempool_verbose, tx_out, address_balance, address_txids,
    // address_utxos, validate_address, unified_receivers, convert_tex, block_subsidy, subtrees,
    // network_sol_ps, standard_fee, discover: the same pattern.
}
```

**Tauri** (src-tauri), one line of logic each:

```rust
#[tauri::command]
pub async fn explorer(state: State<'_, AppState>, node: NodeId, call: ExplorerCall)
    -> Result<ExplorerReply, ExplorerError> {
    state.explorer_for(&node)?.call(call).await
}

#[tauri::command]
pub async fn explorer_job(app: AppHandle, state: State<'_, AppState>, node: NodeId,
    job: ExplorerJob, events: Channel<JobEvent>) -> Result<JobId, ExplorerError> {
    let (explorer, sink) = (state.explorer_for(&node)?, Box::new(ChannelSink(events)));
    match job {   // an export's path comes from the save dialog, never from the webview
        ExplorerJob::Export { what } => explorer.export(what, save_path(&app, &what).await?, sink).await,
        job => explorer.job(job, sink).await,
    }
}
```

- **Event `explorer_live`** `{ node, live }`, from the actor's `watch` by a new
  `bridge::forward_explorer_live` task, modelled on `forward_watch` (src-tauri/src/bridge.rs:56),
  which emits every change of a watch value as it is. The new task wraps the value as
  `{ node, live }`, sends the latest value, then waits 250 ms before reading again, so at most 4
  a second reach the UI; the UI subscribes with `subscribe_then_fetch` (app.rs:358).
- **Wiring:** both commands in `COMMANDS` (src-tauri/build.rs), `generate_handler!`
  (src-tauri/src/lib.rs) and `allow-explorer`, `allow-explorer-job`
  (src-tauri/capabilities/main.json); build.rs fails when they disagree. `AppState` gains
  `explorer` and `explorer_for(&NodeId)`, built in `lib.rs::start` from
  `supervisor.watch_status()`, the monitor's watches, `paths.rpc_cookie_file()`,
  `settings.watch()` and `paths` (the actor opens `explorer_dir` for the network the node
  reports).
- **ui/src/api.rs:** `explorer(node, call)` with typed helpers; in the browser it calls
  `mock::explorer::handle` through `wire()` (api.rs:58); `rename_all = "camelCase"` argument
  structs; errors decode like `apply_error` (api.rs:286).

## UI architecture

- **Modules:** `ui/src/explorer/` (shell `mod.rs`, `history.rs`, `state.rs`, `hash_mirror.rs`;
  `pages/` chain, blocks, block, tx, address, unified, mempool, charts, pool, upgrade, search,
  node; `components/` omnibox, source_line, state_banner, hash_link, amount, pool_chip,
  kind_badge, block_strip, turnstile, reward_bar, budget_meter, flow_diagram,
  confirmations_meter, block_map, virtual_list, filter_chips, glossary, show_rpc,
  shortcut_sheet); `ui/src/charts/` dense_chart, canvas, height_axis, histogram, small_multiples;
  `ui/src/mock/explorer/`; `ui/styles/explorer.css` with `@container page` breakpoints, as the
  other pages use (ui/styles.css; MULTI-NODE.md:326-331).
- **Components:** `HashLink` serves every hash, height and address: monospace, middle-shortened
  in groups of 4, Copy, a hover card (data after 120 ms, card at 300 ms), keyboard-reachable.
  `short_hash`, `zec_text` and `pools_text` first move from
  ui/src/pages/dashboard/details.rs:108-160 to ui/src/format.rs in their own commit. `StatTile`
  (ui/src/charts/widgets.rs), `components/rows.rs`, `tooltip.rs`, `icons.rs` are reused. "x ago"
  cells read `AppState.now` themselves, so the 1 s tick never rebuilds a list.
- **State per node:** `NodeState.explorer: ExplorerState`, a `Copy` struct of signals: `route`,
  `history` (100 entries), `live`, `status`, `routes` (a `StoredValue<RouteCache>` of 50
  payloads stamped with the epoch), `blocks` (a row store of 20,000), `recent` (memory only).
  Each node keeps its place; switching returns to it.
- **Routing and deep links:** each navigation pushes onto `NavHistory`, and routes stay in
  memory. Only the browser mock (`!is_tauri()`, ui/src/tauri.rs:37) mirrors the route into
  `#/explorer/<node-id>/<route>` with `history.replaceState` and follows typed hashes with a
  `hashchange` listener, for deep links while developing
  (`http://localhost:1420/?herd#/explorer/zebrad-testnet/block/4471000`). In the app no route
  reaches the webview's URL or storage. "Copy link" copies the route, which the search box
  accepts. New web-sys features: `History`, `HashChangeEvent`, `HtmlCanvasElement`,
  `CanvasRenderingContext2d`, `CssStyleDeclaration`, `Performance`,
  `VisibilityState`.
- **Per node:** the page stays mounted and hidden like every page and sends `Interest { visible:
  false }` when its tab hides, the node changes or the document is hidden. The Tauri layer also
  tells the explorer about the main window. When exit.rs hides it to the tray, every explorer is
  treated as `Interest { visible: false }`, and `show_main_window` (src-tauri/src/lib.rs:116)
  restores the last interest. Minimizing counts as hiding unless the spike shows that
  `visibilitychange` fires then. `visibilitychange` is an extra signal, not the only one. Each
  Explorer uses its node's prefixes, unit, upgrade heights and tip, from the chain the node
  reports. "View on Zakura · Mainnet" (P6) keeps the route and switches node. In the mock, every
  `?herd` node has its own chain.
- **VirtualList:** fixed 32 px rows; `ceil(viewport / row) + 2 × overscan` slots keyed by `index
  mod slots`, so DOM nodes are reused and scrolling only updates text; `translateY` inside a
  `contain: strict` scroller; `scrollTop` read once a frame; `ResizeObserver` for the viewport;
  no fetches above 2 viewports a second (same-height skeletons hold the place); prepends keep the
  reader's place; at most 100,000 rows (3.2 M px) at once, re-anchored by 50,000 within 10,000 of
  an edge, so the whole chain stays reachable under webview element-height limits (WebKitGTK's
  is unconfirmed); an ARIA grid with `aria-rowcount` and `aria-rowindex`; a pure, tested
  `visible_window()`. Nothing virtualizes today (Logs renders ≤ 1,000 lines with `<For>`).
- **Charts:** `ChartData`'s x values become `enum XValues { Times(..), Heights(Vec<u32>) }` in
  place (ui/src/charts.rs:547), with `height_ticks` beside `time_ticks` (:323). M4 replaces
  `downsample` for the dashboard's sparklines (ui/src/pages/dashboard/tiles.rs:231) in its own
  commit. Table views keep `downsample_indices`, and the dashboard's line charts already draw
  every sample.

### Mock data generation

- `ui/src/mock/explorer/` answers every request through `wire()` with 20–80 ms of `sleep()`,
  running explorer-model's real builders over `synth`, so kinds, fees, ledgers and sentences
  follow the backend's rules.
- **Deterministic and O(1):** a block is a pure function of (network, height, branch); `?reorg`
  and `fakereorg D` move the top D blocks to a new branch, and on the first branch `hash(h) =
  "00000000" + mock_hash(h)[8..]`, the formula ui/src/mock/details.rs uses, so the dashboard's
  best block opens the right page (details.rs, reading synth, follows the branch); cumulative
  values follow per-era trends plus bounded seeded noise, so every pool stays ≥ 0 (ZIP 209) at
  any height.
- **Era rules as tested invariants:** no Sprout inflows after Canopy, no JoinSplits from NU7;
  from NU6.3 `valueBalanceOrchard` ≥ 0 and shielded coinbase to Ironwood or Sapling; ZIP 218 from
  NU7; lockbox from NU6 and the NU6.1 disbursement; NSM seed and 60 % burn; fees at 1,000 or
  5,000 zat per action; exact coinbase claims, so every block passes `ledger_check`.
- **Content:** transactions seeded by (height, index), full within ~2,000 blocks of the tip and
  summaries beyond; 2,000 fake t-addresses on a Zipf curve with wrong checksums, never real; a
  mempool sized from `stats.mempool_transactions`, partly mined each block; times anchored at
  page load, 150, 75 or 25 s apart by era.
- **Heights:** the default mock syncs from 2,950,000 towards 3,120,000 (ui/src/mock.rs:95-98),
  before NU6.1, so it cannot show Ironwood, the burn or the budget; `?attip` and `?tip=` can
  (re-basing is open question 7); `?herd`'s Testnet node is past NU7 (ui/src/mock/herd.rs:40-41).
- **The dashboard agrees:** mock/details.rs lists five pools and mints every coin into Orchard
  (details.rs:72-125), which NU6.3 forbids; it reads synth instead, in its own commit.
- The mock ships in release builds (chosen at runtime, ui/src/tauri.rs:37), so it is procedural:
  no fixture JSON in the wasm.

| Flag | Effect |
| --- | --- |
| `?attip`, `?tip=<h>` | the main node synced near 3,551,000 (past NU7), a block every 25 s; or any height. The mock's end of support (ui/src/mock.rs:627-630, now block 3,160,000) moves with the tip and keeps its distance: 41 days of blocks above it at the era's spacing, 10 with `?eos=soon`, so the dashboard and "your zebrad stops at block X" agree with the height |
| `?explorer=slow`, `=big`, `=empty` | 300–800 ms calls; 5,000-transaction and 2 MB blocks and a 200,000-transaction address; a 10-block chain |
| `?explorer=nobi` | blocks without `BlockInfo`, including a parent without (the zero-diff case) |
| `?explorer=pruned`, `=stopped`, `=perf` | Zakura pruned (with `?herd`); a stopped node with cached pages "from memory"; the performance overlay |
| `?reorg` | a 2-block reorg about every 90 s |
| `?mempool=busy`, `=off` | three blocks' budget waiting at 1× to 10× (none unpaid: zebrad cannot hold them); the mempool inactive |

They join mock.rs's table and ui/README.md and compose with `?herd`, `?stalled`, `?nopeers`,
`?firstrun`.

## Performance budgets and how they are met

Targets until P0 measures them; thresholds are configuration, not constants.

| What | Budget | How |
| --- | --- | --- |
| Keystroke to hint | ≤ 16 ms | pure classifier in wasm |
| Navigation to first paint; back or forward | ≤ 16 ms | cached data, a skeleton, or the previous page dimmed; the route cache |
| Reply from the actor's cache | ≤ 50 ms with IPC | L2, small JSON |
| Cold block header | ≤ 150 ms | 2 v1, cb, subsidy in parallel |
| Cold Chain to the strip | ≤ 400 ms | 13 v1, six at a time, coinbase calls after |
| Small block's transactions | ≤ 1 s | one v3, heavy lane |
| Big block's visible rows | ≤ 300 ms | ~20 `getrawtransaction` on interactive permits |
| Scrolling | 60 fps, ≤ 4 ms script a frame, no fetch in flings | VirtualList |
| Chart redraw; refined frame | ≤ 8 ms; ≤ 150 ms from stored samples | M4 frames |
| Suggestions | ≤ 5 ms in the backend | sorted prefix index |
| IPC | ≤ 64 KB typical, 1 MB cap; events and job messages < 8 KiB, delivered directly (tauri src/ipc/channel.rs:36) | paging, slim views |
| Load on zebrad | ≤ 6 weight units of 100 shared slots; 0 hidden; no background while syncing | the scheduler |
| Idle at the tip, visible | ~1 request a second + 2 per block | 2 s tip poll and mempool diff |
| Memory | L2 64 MiB (8 idle); store ≤ 32 MiB; UI routes ≤ 8 MiB, rows ≤ 4 MiB | byte-weighted caches |
| Wasm size | tracked each phase | procedural mock, no serde_json in the UI |

| View | Calls, cold | Warm |
| --- | --- | --- |
| Chain | 1 tip + 26 v1 (25 rows and a parent; the strip's 13 among them) + 25 cb (lazy) + 1 `getblockheader` at tip − 100 + 1 v1 at tip − N + 1 v1 at NU7 − 1 + 1–2 subsidy + 1 solps | per block: 1 v1 + 1 cb |
| Blocks, 30 rows | 31 v1 + 30 cb once settled; recent-window rows free from P5 | 0 |
| Block | 2 v1 + cb + subsidy, then 1 v3 after 150 ms (blocks ≤ 256 transactions and 512 KB, P0 to confirm) | 0 |
| Transaction | 1 + ≤ 16 parents, or none with the block's v3 cached | 0 |
| Address | 1 balance + 1–4 windows + 25 transactions and parents | 10 s, each tip |
| Mempool | non-verbose every 2 s; verbose on change; 1 per new txid | — |
| Date search; whole-chain chart | ≤ 23 headers (3–6 with the store); ~870 v1, then `s256` | 0 |

Every v1 carries the 2,688-hex-character solution (methods.rs:1818), so 100 rows are ~350 KB
from zebrad for ~15 KB of rows in the UI: hence `IgnoredAny` and slim replies. A 2,000,000-byte
block (zebra-chain/src/block/serialize.rs:24) may give ~8 MB of v3 JSON (unconfirmed).

**Measurement:** (1) a P0 bench, `crates/launcher-core/tests/explorer_bench.rs`, `#[ignore]`,
with `ZEAL_BENCH_RPC_ADDR` and `ZEAL_BENCH_COOKIE`, on a synced and a syncing Mainnet zebrad:
p50/p95/p99 latency and bytes per method (v1 new and old; v3 small, median and the 10 largest
of 10,000; transactions; a busy address; both mempool calls), concurrency 1–16, batches 1–64
with a failing element, a bad cookie, `BlockInfo` at sampled heights; its table goes into this
document. (2) A Tauri spike: JSON, `Channel` and raw replies, 1 KB to 4 MB, on WebKitGTK,
WebView2 and WKWebView; on each, `document.visibilityState` after `window.hide()` (hiding to the
tray) and after minimizing; on WebView2, whether the user data folder's History records
`history.replaceState` fragment changes. (3) `?explorer=perf`: frame times over a scripted
10,000-row scroll, navigation to paint, IPC timings. (4) `#[ignore]` micro-benchmarks: the
largest v3 fixture, M4 over 1 M points, 5,000 treemap tiles, the simulation. (5) Call budgets in
CI through fake-zebrad's `fakegetcallcounts`.

## Testing

- **explorer-model and launcher-types:** the `classify_query` table (every prefix per network,
  separators, relative heights, outpoints, URIs, URLs, JSON, dates, every refused class, `uvf`
  and `uvi` keys on each network included); wire shapes like `apply_payload_shapes`; route round
  trips; `ListFilter`; `zec_to_zat`; the ledger on fixtures (coinbase-and-delta fees equal v3's
  `fee` sum; the era identities; a parent without `BlockInfo` gives `deltas: None`); every kind
  (migration, shielding with change, coinbase into Ironwood and Sapling, Orchard at NU6.3);
  `logical_actions` against zebrad's rule with Ironwood; the ZIP 218 limits; `nsm_share`
  rounding; `pool_rules` and `spacing_between` across eras; TEX against ZIP 320's vectors; the
  simulation never exceeds a limit; `Shown::Hidden` never formats as a number.
- **Fixtures,** captured in P0 by an opt-in script from a synced Mainnet zebrad 7.0.0 into
  `crates/launcher-core/tests/fixtures/zebrad-7.0.0-rpc/` with a README: v1 and v3 of height 1,
  a Sapling-era block, the Canopy, NU5, NU6, NU6.1, NU6.3 and NU7 activation blocks, a migration,
  an Ironwood coinbase, an NU7 block with fees, a large block, a block without `BlockInfo` if one
  exists; each transaction kind and a waiting one; `getrawmempool true`; the address methods and
  the 100,000 error; `gettxout` both ways; `z_listunifiedreceivers`; `z_getsubtreesbyindex`;
  `getblocksubsidy` around NU7.
- **Property test:** for 10,000 seeded heights, the reducers over synth's zebrad-shaped JSON give
  synth's own views.
- **launcher-core pure:** the LRU; `ChainIndex` (reorgs of 1, 2 and 999; gaps and deep reorgs
  reset; one epoch bump; the binary search); the scheduler (lanes, weights, promotion,
  cancellation, dead waiters, 429, sync caps); the store (round trip, checksums, header mismatch,
  final heights only, nothing above the tip, the check at open); mempool diffs; address windows;
  the running balance.
- **The actor,** with an in-memory synth `ChainSource` that counts calls and holds them on
  oneshot gates: merging, no call for cached content, interactive first, pauses while syncing
  or hidden, `NodeNotRunning`, caches kept on a new `rpc_addr` and cleared on a new chain, one
  reorg notice; a network change: the other id gets `NodeNotRunning` with the next-start message
  until zebrad restarts,
  then the Explorer answers under the new id and the other network's store stays in place; TTLs
  under tokio's paused clock (`test-util` is a dev-dependency).
- **Privacy:** the actor tests run under a capturing `tracing` subscriber at TRACE and fail if
  any event or span field holds a requested address, txid, hash, height or query (synth values
  chosen so they cannot appear by chance). Two actors on the same synth chain, one that opens
  100 blocks, transactions and addresses and one that opens none, end with byte-identical store
  files once their sweeps finish. `diagnostics::bundle` after such a session contains none of the
  requested values.
- **fake-zebrad,** after a move-only split into a folder: `FAKE_ZEBRAD_CHAIN=synthetic` with
  `FAKE_ZEBRAD_CHAIN_TIP` serves every `ReadCall` from synth; `fakemine N`, `fakereorg D`; JSON
  arrays one by one; `FAKE_ZEBRAD_LATENCY_MS`; `FAKE_ZEBRAD_MAX_INFLIGHT` answers 429;
  keep-alive (today `Connection: close`, fake-zebrad.rs:619). The canned mode stays default:
  tests/rpc_monitor.rs asserts 2,500,000 (:99).
- **End to end,** `crates/launcher-core/tests/explorer.rs`, after `spawn_fake_with` moves from
  rpc_monitor.rs:38 to tests/support/helpers.rs: Chain ≤ 28 v1, 1 header, 25 cb, no v3; a
  revisit 0; a Block page 2 v1, 1 cb, 1 subsidy, then 1 v3; a 3-parent transaction 1 + 3; 30
  Blocks rows ≤ 31 v1; after `fakereorg 2`, one epoch bump, ≤ 3 walk-back calls or ≤ 10
  `getblockhash`, the old block `Replaced`.
- **Real zebrad, opt-in:** a Regtest contract test in the tests/real_zebrad.rs style (`[mining]
  miner_address`, `generate`, every request decodes; the harness, never the explorer, forces a
  reorg with `invalidateblock`). Early NU6.3 and NU7 on Regtest are unconfirmed config keys.
- **UI, native:** `NavHistory`, `visible_window`, re-anchoring, M4 keeps a spike, histogram bins,
  `flow_layout`, the treemap, sentences, formatting, the keyboard map against Ctrl+digits, a
  token per pool in both themes; the mock is deterministic, links, matches details.rs's best
  hash and its own blocks, and survives `wire()`. CI's wasm clippy and `trunk build --release`
  cover the views.
- **Before each merge:** a keyboard-only walkthrough, an Orca pass (VoiceOver and NVDA before
  the release), forced colours, 200 % zoom, screenshots light and dark at 1,000 and 1,360 px.

## Phases

Each phase is one PR that leaves the app working, with screenshots from the mock.

1. **P0, groundwork. Built, without the measurements.** Move-only commits: `short_hash`,
   `zec_text`, `pools_text` to format.rs; the at-tip rule's body to launcher-types. Then
   rpc.rs's typed decode, `OwnedSession`, timeouts and cookie re-read; `NodeDetails` gains
   `upgrades` and `consensus`. Not yet: `spawn_fake_with`'s move and fake-zebrad's folder (other
   branches change fake-zebrad; the move comes with the end-to-end tests that need it), and the
   bench, the Tauri spike and the fixture script, which need a synced Mainnet zebrad (open
   question 11). Until they run, the thresholds below are the design's targets.
2. **P1, chain, blocks and a first transaction page. Built.** explorer-model (shapes, ledger,
   budget, upgrades, views, the classifier, synth); the actor (scheduler, L2, merging,
   `ChainIndex`, tip follower, `Interest`); `explorer` and `explorer_live`; the tab, gate and
   shell; Chain (V1 without the next-block tile, tiles, recent blocks); Block (V2, V3, V4,
   details, txids 200 a page); future heights; Transaction without input values (status, V7,
   inputs and outputs, shielded cards, kind); pool tokens; `?attip`, `?tip=`,
   `?explorer=slow|empty|nobi`, `?reorg`; details.rs reads synth; call budgets as tests. Not
   yet: V9 and the Sol/s tile, and fake-zebrad's synthetic mode. Screenshots: Chain, Block,
   Transaction, light and dark, with `?attip`, `?attip&reorg`, `?explorer=nobi`, `?herd`.
3. **P2, transaction detail.** The Block page's table on `VirtualList` (v3, visible rows first
   for big blocks); prevouts; V6 with its conservation line; the fee card; outpoints; hover
   cards; Show RPC; prefetching. Screenshots: fully shielded, shielding with change, Orchard →
   Ironwood, a coinbase, `?explorer=big`, Show RPC.
4. **P3, live views.** Blocks over the whole chain, with filters and V5 over the loaded rows
   until P5's sweep fills the recent 10,000 blocks, and jump to height (to a date with P5's date
   search); the mempool tracker and page (V11 without the projection); the next-block tile;
   following a waiting transaction; `Left`; reorg banners and the fork log in memory; the URL
   mirror in the mock; links from the dashboard and Logs; the `?` sheet and single-key setting.
   Screenshots: Blocks at an old height, `?mempool=busy`, `?mempool=off`, a reorg banner, a
   waiting transaction.
5. **P4, addresses.** Transparent and TEX pages (windows, running balance, UTXOs, pending, V10);
   unified, Sapling and Sprout pages; TEX checked by `z_converttex`; output status; the spender
   job. Screenshots: a long-history address, a unified address, a TEX address, the spender
   search running.
6. **P5, charts and the store.** Sampler, store, persisted fork log; `Series`, `DenseChart` and
   V12; pool and upgrade pages; V8; date search; CSV export; `XValues`; M4 for the dashboard's
   sparklines (own commit). May split in two. Screenshots: pool balances since NU5, the NSM
   since NU7, notes with subtrees, a block map, `?explorer=perf`.
7. **P6, several nodes.** `NodeRuntime.explorer` from MULTI-NODE phase 2; a store per node;
   probing; tip agreement across nodes; Zakura pruning and "View on Zakura · Mainnet" once
   MULTI-NODE phase 3 adds Zakura; the zcashd compatibility build once probed and Ctrl/Cmd+K
   from Zaino once its phase 5 runs them, in the mock only until then. Screenshots: `?herd` with
   two Explorers, `?herd&explorer=pruned`.
8. **Later,** each its own PR: the projection, once the store shows whether blocks fill; ZIP 318
   aggregates; This node (V13, arrival latency, mempool overlap); opt-in labels and a watchlist;
   the opt-in history index; push updates over the indexer gRPC; `zeal://` links.

## Built so far

P0 and P1, for the one node the backend runs (see [Several nodes](#several-nodes)). Where the
code differs from the design above:

- **RPC client.** The typed decode, `OwnedSession`, per-call timeouts (`call_timeout`, with
  `HEAVY_REQUEST_TIMEOUT` at 60 s) and the cookie re-read are in rpc.rs. A call reads the cookie
  again after an HTTP status, an empty body or an HTTP error other than a timeout, and repeats
  itself only if the file changed, so a stopped zebrad costs a file read, not a second request.
  Keep-alive settings and `call_batch` wait for the bench.
- **`NodeDetails`** carries `upgrades` as a list ordered by height, each with its branch id, and
  the `consensus` branches of the tip and the next block.
- **Moves.** `short_hash`, `zec_text`, `pools_text` and the at-tip rule moved as planned.
  `spawn_fake_with` stays in rpc_monitor.rs and fake-zebrad in one file: nothing in this phase
  needs them elsewhere, and two other branches change fake-zebrad.
- **Wire types** in `launcher_types::explorer` carry P1's requests only: `Status`, `Interest`,
  `Chain`, `Block`, `BlockTxs`, `Tx` and `Resolve`. `ExplorerCall` has no `scope` (cancelling
  paged requests) or `trace` (Show RPC, P2) yet, and there is no `explorer_job` command until
  the first job (P4).
- **explorer-model's modules** are `shapes`, `upgrades`, `ledger` (with the ZIP 218 budget from
  the note trees), `views`, `classify` and `synth`. The pool rules the pages show sit with the
  pages; `zip317`, `pools`, `query`, `filter`, `flow`, `m4`, `template` and `glossary` come with
  the phases that use them. Testnet's NU6.2, NU6.3 and NU7 heights are zebra-chain 7.0.0's:
  4,052,000, 4,134,000 and 4,465,026.
- **The classifier** looks up heights (with `tip`, `latest`, `tip-N` and `-N`), hashes and
  outpoints, and refuses keys and phrases, a search route's query included. It takes a pasted
  URL's last segment, `?q=` or fragment, but not an id inside JSON, and it names a `zcash:`
  request instead of opening its address. The other classes are recognised for the hint and
  answered "a later version".
- **Tauri.** The explorer's handle is Tauri state of its own, as the updater's is, not an
  `AppState` field, and the node-id rule is the actor's, behind `ExplorerHandle::call(node,
  call)`: it answers the id of the network zebrad runs on, and calls the other network's id not
  running until zebrad's next start. Hiding the window to the tray does not send `Interest`
  yet: the tab and the document's visibility do.
- **The actor.** Two lanes, interactive and prefetch (the follower); every P1 read weighs one,
  since P1 makes no heavy call, and the background lane comes with the store (P5). Six permits,
  four while the node syncs; no HTTP 429 back-off yet, since six reads never come near zebrad's
  100. A new network clears everything, where the design says a new genesis hash. Hashes above
  the final 1,000 are trusted for 10 s after a tip check, else a block asked for by height is
  read again; headers above them are cached per epoch.
- **The follower** polls `getbestblockheightandhash` every 2 s while the page is shown and the
  node is at the tip, and a page that needs the tip checks it when the last check is older than
  it can use; the monitor's `best_block_hash` is not used. It reads the new tip's block: one on
  top is a push; otherwise a `getblockhash` at the old tip tells a jump from a reorganisation,
  whose fork a binary search of the known heights finds in at most ten calls. It does not walk
  back three parents first. `ChainIndex` is one `BTreeMap` of the heights it knows, and there is
  no fork log yet: `ExplorerLive` carries the last reorganisation.
- **After a reorganisation** the Chain page, Block pages opened by height and Transaction pages
  ask again. A Block page opened by hash keeps its block, without the "no longer in your node's
  best chain" line.
- **Not yet:** the store, stale replies while the node is stopped (the explorer answers "not
  running"), the search's 150 ms previews, suggestions and recent searches, the "3 new blocks"
  pill, V9, the Sol/s tile, the route cache (back and forward ask the explorer again, which
  answers from its cache) and hover cards.
- **Call budgets** are pinned by launcher-core's actor tests over an in-memory synthetic source
  that counts its calls: the Chain page costs 1 tip, 25 coinbases, 1 header, at most 27 blocks
  (26, and the one before NU7 once past it) and at most 2 subsidies; a revisit costs nothing; a
  Block page 2 blocks, its coinbase and its subsidy; a Transaction page one read; two pages
  asking for one block share its reads; a reorganisation takes at most ten `getblockhash`. A
  test also checks that no log field carries a requested hash, txid or height. fake-zebrad's
  synthetic mode and the end-to-end tests through `fakegetcallcounts` wait with the move above.
- **The UI** is `ui/src/explorer.rs` (the shell) and `ui/src/explorer/` (`chain`, `block`, `tx`,
  `search`, `parts`, `state`, `history`, `text`), with `ui/styles/explorer.css`. Each node's
  place lives in the Explorer tab (`Explorers`), not in `NodeState`. Nothing is read before the
  tab is first opened. The Transaction page lists transparent inputs as links to the outputs
  they spend, without values, and shows the fee only for a transaction without transparent
  inputs.
- **The mock** is one file, `ui/src/mock/explorer.rs`, with the flags `?attip`, `?tip=`,
  `?explorer=slow|empty|nobi` and `?reorg`; the other flags come with their phases. Every
  `?herd` zebrad and Zakura node has its own chain (Zakura's pruning is P6). The synthetic
  chain's subsidy leaves out the slow start; the explorer reads the real one from
  `getblocksubsidy`.

## Open questions for the maintainer

1. Explorer after Logs as Ctrl/Cmd+3 (moving Grafana, Configure and Settings by one), or
   appended as Ctrl/Cmd+6? P1 places it after Logs, as Ctrl/Cmd+3.
2. A new workspace crate `explorer-model` (pure, serde only), or modules in launcher-types? P1
   builds the crate.
3. TEX: a hand-written bech32m decoder with `bs58` and `sha2` (in Cargo.lock), or the `bech32`
   or `zcash_address` crate (new to `cargo deny`)?
4. ZIP references: Copy only, or open zips.z.cash after a confirmation?
5. Recent searches: memory only, or an opt-in "remember searches on this computer"?
6. Labels and a watchlist: wanted? Per node or per network?
7. Re-base the mock's `FIRST_HEIGHT` and `FIRST_TIP` to straddle NU6.3 and NU7, or rely on
   `?attip`?
8. The store: on by default while the Explorer is visible (≤ 32 MiB of public data), or opt-in?
9. `zeal://` links other apps can open: later, or never?
10. Push updates through the indexer gRPC, which has no authentication and opens a loopback
    port: later, or never?
11. Who runs the P0 bench on a synced Mainnet zebrad, the IPC spike per platform, and the
    fixture script? All three are still to run.
12. Build the projection only if the store shows that blocks fill up?

## Not confirmed

- Real latencies and JSON sizes of v1 and v3; whether 60 s covers heavy calls.
- Batches at runtime and how a failing element is reported; HTTP/2 on the RPC port; what a
  wrong cookie returns.
- Whether an upgraded 7.0.0 database can still lack `BlockInfo` rows.
- Whether `getblockheader` finds side-chain blocks.
- Whether NU7 has activated on Mainnet at 3,543,405.
- `getblocksubsidy` below the first halving at runtime, and whether its total includes ZIP 237's
  additional subsidy once reissuance starts.
- Sprout key prefixes and WIF leading characters.
- How often Zcash blocks are full.
- WebKitGTK's element-height limit and canvas speed, WebGL in the system webviews, Tauri's IPC
  timings near 1 MB.
- Whether the webviews report the page hidden (`document.visibilityState`) when Zeal hides its
  window to the tray or is minimized; the Tauri spike checks each.
- Whether WebView2 records fragment changes made with `history.replaceState` in its History
  database; the Tauri spike checks the user data folder.
- How closely Zakura's and the zcashd compatibility build's RPCs match zebrad's.
- Whether the indexer gRPC starts without the `indexer` feature (zebra-rpc/src/config/rpc.rs:36
  says it needs it; zebrad/src/commands/start.rs:557-575 starts it whenever the address is set).
- The 5,000 to 1,000 zat wallet fee switch at Mainnet 3,590,000 (a draft ZIP); ZIP 229 and ZIP
  318 are drafts.
- That tree-size changes equal Sapling outputs plus Orchard and Ironwood actions.
