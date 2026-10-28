# Observability stack

Prometheus and Grafana in Docker, scraping the zebrad that Zebra Launcher runs and
serving Zebra's own Grafana dashboards. It is optional and the advanced option: the
launcher's Dashboard and Logs tabs show the node's state without it, from zebrad's RPC
and log. Run this stack when you want every metric zebrad exports, history that outlives
the launcher session, or Grafana's own tools. The launcher's Grafana tab then embeds the
dashboards ([GRAFANA.md](../GRAFANA.md)).

## What is in this directory

| File | Role |
| --- | --- |
| `docker-compose.yml` | The stack: Prometheus and Grafana with health checks, ports on `127.0.0.1` only, Grafana set up for embedding. The Docker Desktop layout. |
| `docker-compose.linux.yml` | Override for Docker Engine on Linux: both services on the host network. |
| `prometheus.yml`, `prometheus.linux.yml` | The scrape config for each layout; they differ only in the target host (see [Why two compose files](#why-two-compose-files)). |
| `grafana/provisioning/` | The Prometheus datasource (uid `zebra-prometheus`, URL from `PROMETHEUS_URL`) and the dashboard provider (folder "Zebra", read-only, re-read every 30 s). |
| `grafana/dashboards/` | Zebra's 14 dashboards, vendored unchanged from the Zebra repository. |
| `sync-dashboards.sh` | Re-vendors the dashboards from the pinned Zebra commit. |

## Requirements

- Docker with Compose v2 (`docker compose`): Docker Desktop on macOS and Windows,
  Docker Engine with the compose plugin on Linux. The Linux override uses the `!reset`
  tag (checked with Docker Compose 2.23.3).
- The repository: the installed launcher does not carry these files. Clone it or
  download it from <https://github.com/ZcashFoundation/zebra-launcher> (the Grafana
  tab's setup card has a button for that).
- zebrad running in the launcher. The launcher sets `metrics.endpoint_addr =
  127.0.0.1:9999` in its managed `zebrad.toml` and never moves it
  ([what the launcher configures](../GRAFANA.md#what-the-launcher-configures)), so the
  scrape target always matches. Metrics are served only while zebrad runs.
- Ports 9090 (Prometheus) and 3000 (Grafana) free on `127.0.0.1`.

## Start, stop, update

From the repository root. Docker Desktop (macOS, Windows):

```sh
docker compose -f docs/observability/docker-compose.yml up -d
```

Linux (Docker Engine), with the host-network override:

```sh
docker compose -f docs/observability/docker-compose.yml -f docs/observability/docker-compose.linux.yml up -d
```

The first start pulls the two images. Grafana waits for Prometheus to pass its health
check before it starts. The other commands take the same `-f` files:

| Do | Command, after the `-f` files | Notes |
| --- | --- | --- |
| See the state | `ps` | both services show `(healthy)` within about a minute of the first start |
| Follow the logs | `logs -f` | `logs prometheus` or `logs grafana` for one service |
| Stop | `down` | the data stays in the volumes |
| Start again | `up -d` | |
| Update | `pull`, then `up -d` | after a `git pull` that bumped the images (Dependabot PRs) or changed the compose files |
| Apply a changed `prometheus*.yml` | `restart prometheus` | Prometheus reads its config at start; the file is bind-mounted, so no `up -d` is needed |
| Reset | `down -v` | deletes both volumes; see [Where the data lives](#where-the-data-lives) |

Changed dashboards need nothing: Grafana re-reads the bind-mounted directory every 30 s.

Both services listen on `127.0.0.1` only. Grafana lets anyone on this machine view
without a login (the launcher relies on that) and asks for `admin`/`admin` to edit. To
set another admin password, create `docs/observability/.env` (git-ignored) with
`GF_SECURITY_ADMIN_PASSWORD=...` before the first start: Compose reads the `.env` next
to the compose file for the `${GF_SECURITY_ADMIN_PASSWORD:-admin}` in
`docker-compose.yml`. Grafana applies it when it creates its database, so after the
first start change the password in Grafana (profile menu) or with
`docker compose -f docs/observability/docker-compose.yml exec grafana grafana cli admin reset-admin-password <new>`.

### Why two compose files

zebrad's metrics are bound to `127.0.0.1` on the host, and a container on the default
Docker bridge cannot reach a host port bound to `127.0.0.1`: its own `127.0.0.1` is the
container.

- **Linux:** `docker-compose.linux.yml` puts Prometheus and Grafana on the host network
  (`network_mode: host`), so `127.0.0.1` is the host and Prometheus scrapes
  `127.0.0.1:9999` with `prometheus.linux.yml`. Prometheus listens on `127.0.0.1:9090`
  and Grafana on `127.0.0.1:3000`, since published ports do not apply to host
  networking.
- **Docker Desktop (macOS, Windows):** the base file scrapes `host.docker.internal:9999`
  with `prometheus.yml`; `extra_hosts: ["host.docker.internal:host-gateway"]` maps that
  name to the host.

> **Not yet verified on macOS and Windows:** whether Docker Desktop forwards
> `host.docker.internal:9999` to a host service bound to `127.0.0.1`. The stack has only
> been run on Linux so far. If it does not, the fallbacks are Docker Desktop's host
> networking (Settings → Resources → Network → Enable host networking) with the Linux
> override, or a metrics bind on the Docker bridge address. The second conflicts with the
> launcher's loopback-only rule for metrics and needs a decision first. Record the result
> here once checked.

### Scrape timing

Prometheus scrapes the `zebrad` job every 10 s with a 5 s timeout, the cadence the
launcher itself polls zebrad at. Zebra's dashboards use `rate(...[1m])` windows, which
need at least two samples per window, so the interval must stay at 30 s or less. The job
name is the `job` label on every series; the dashboards fill their "job" picker from
Prometheus, so a rename needs no dashboard change.

## Check that it is working

1. `docker compose -f docs/observability/docker-compose.yml ps` (Linux: both `-f`
   files): both services `(healthy)`. If `up` ended with "dependency failed to start",
   Prometheus did not become healthy; read `logs prometheus`.
2. Prometheus targets, <http://127.0.0.1:9090/targets>: the `zebrad` job is **UP** and
   its last scrape a few seconds old. **DOWN**: see
   [TROUBLESHOOTING.md](../TROUBLESHOOTING.md#prometheus-target-down-dashboards-show-no-data).
3. Grafana, <http://127.0.0.1:3000>: the Zebra overview opens without a login and shows
   the block height. The Graph tab of Prometheus answers a query such as
   `zcash_net_peers` as well.
4. In the launcher, open the Grafana tab, keep `http://127.0.0.1:3000` and choose "Use
   this Grafana": the overview appears in the tab.

The same from a terminal:

```sh
curl -s http://127.0.0.1:9999/metrics | head -n 3      # zebrad's metrics page, while zebrad runs
curl -s http://127.0.0.1:9090/api/v1/targets | grep -o '"health":"[a-z]*"'   # "health":"up"
curl -sf http://127.0.0.1:3000/api/health             # {"database": "ok", ...}
```

## How the launcher embeds Grafana

The launcher's Grafana tab loads Grafana in an iframe at the URL you give it (the
default is `http://127.0.0.1:3000`), picks a dashboard by its uid and adds the kiosk,
time range, refresh and theme parameters itself; the tab's controls and URL rules are in
[GRAFANA.md](../GRAFANA.md#the-grafana-tab). For that, Grafana must allow being framed
and show dashboards without a login. `docker-compose.yml` sets:

| Setting | Why |
| --- | --- |
| `GF_SECURITY_ALLOW_EMBEDDING=true` | Grafana sends `X-Frame-Options: deny` otherwise, and the tab stays blank |
| `GF_AUTH_ANONYMOUS_ENABLED=true`, `GF_AUTH_ANONYMOUS_ORG_ROLE=Viewer` | the iframe shows dashboards without a login, which the launcher cannot do for you |
| `GF_AUTH_DISABLE_LOGIN_FORM=false` | admins can still log in from a browser |
| `GF_SECURITY_ADMIN_PASSWORD` | from `.env`, `admin` otherwise (see above) |
| `GF_DASHBOARDS_DEFAULT_HOME_DASHBOARD_PATH` | `/` opens the Zebra overview |
| `GF_ANALYTICS_REPORTING_ENABLED=false`, `GF_ANALYTICS_CHECK_FOR_UPDATES=false` | a local desktop stack has no reason to phone home |

Another Grafana works in the tab if it has the first two settings, Zebra's dashboards
(import the JSON files from `grafana/dashboards/`; they expect a Prometheus datasource
with uid `zebra-prometheus`) and a loopback URL, `http://127.0.0.1:<port>` or
`http://localhost:<port>`, the only form the launcher accepts.

## Where the data lives

Two named volumes, listed by `docker volume ls`:

| Volume | Holds |
| --- | --- |
| `zebra-launcher-observability_prometheus-data` | the Prometheus time series, kept 15 days (Prometheus's default retention); a few hundred megabytes at most for one zebrad |
| `zebra-launcher-observability_grafana-data` | Grafana's database: the admin password, dashboards saved with "Save as", preferences |

The dashboards, the provisioning and the Prometheus config are read from this directory
(read-only bind mounts) and are not in the volumes. `down -v` deletes both volumes; the
next `up -d` starts from scratch, with `admin`/`admin` again. To drop only the
measurements, `down`, then `docker volume rm zebra-launcher-observability_prometheus-data`,
then `up -d`.

## Dashboards

`grafana/dashboards/` holds Zebra's 14 dashboards, copied unchanged from
`docker/observability/grafana/dashboards` at the commit in
[`ZEBRAD_COMMIT`](../../ZEBRAD_COMMIT), the one the tag `v<ZEBRAD_VERSION>` points to, in
[ZcashFoundation/zebra](https://github.com/ZcashFoundation/zebra). They are provisioned
read-only into the folder "Zebra" and query the datasource with uid `zebra-prometheus`.
To change one, use "Save as" in Grafana, or change Zebra's copy upstream. The launcher's
picker lists them by uid with its own sentence-case labels (`ui/src/grafana_url.rs`), in
this order:

| Grafana title | uid | File | What it shows |
| --- | --- | --- | --- |
| Zebra Overview | `zebra-overview` | `zebra_overview.json` | Node identity and version, block height and rate, sync distance, peers and handshakes, network traffic and message rates, mempool size, proof verification rate, RPC health and p99 latency, value pools and total supply, database size, process CPU, memory and file descriptors (empty, see below), links to the other dashboards. Grafana's home page in this stack. |
| syncer | `Sl3h19Gnk` | `syncer.json` | Block height, downloaded and verified blocks, in-flight downloads, sync status and queued downloads, sync peer responses, sync stage and block download/verify durations (the last three stay empty, see below). |
| checkpoint verification | `o4LmN_OMk` | `checkpoint_verification.json` | The checkpoint phase of the initial sync: checkpoint height, pipeline throughput, queues. |
| block verification | `rO_Cl5tGz` | `block_verification.json` | Verified block height, block sync and gossip counts. |
| (a magnifying-glass symbol) | `UXVRR1v7z` | `transaction-verification.json` | Transactions finalized, signatures validated, proofs verified, nullifiers revealed, transparent outpoints, batch verification throughput and duration by verifier. |
| peers | `S29TgUH7k` | `peers.json` | Compatible and obsolete peers, latest connected and obsolete protocol versions, successful handshakes by implementation. |
| network health | `320aS_dMk` | `network_health.json` | Bytes in and out, the candidate set, connection states, peer readiness. |
| network messages | `YQ3yxiVnk` | `network_messages.json` | Inbound and outbound message totals and types, requests and responses, cancelled outbound requests. |
| mempool | `wVXGE6v7z` | `mempool.json` | Mempool storage and rejected storage, transaction downloader, verifier and gossiper rates, downloaded transactions by version, block rates. |
| errors | `IhbO11wGk` | `errors.json` | Sighash errors (empty with zebrad 7.0.0, see below). |
| Zebra Value Pools | `zebra-value-pools` | `value_pools.json` | Sprout, Sapling, Orchard and transparent pool balances, total chain supply, pool health, current block height. |
| Zebra RPC Metrics | `zebra-rpc-metrics` | `rpc_metrics.json` | Request and error rates, active requests, latency percentiles by method, errors by method and code, method statistics. |
| RPC Tracing | `zebra-rpc-tracing` | `rpc_tracing.json` | RPC rates and latencies from Jaeger span metrics; empty in this stack (no Jaeger). |
| RocksDB Database | `zebra-rocksdb` | `rocksdb.json` | Database disk and memory size, live data size, largest column families, compaction efficiency and status, SST files per level, batch commit latency (empty, see below). |

### Re-vendoring

When `ZEBRAD_VERSION` and `ZEBRAD_COMMIT` change, re-vendor the dashboards in the same
pull request:

```sh
docs/observability/sync-dashboards.sh               # download from GitHub at the commit in ZEBRAD_COMMIT
docs/observability/sync-dashboards.sh ../zebra      # or read that commit from a local Zebra checkout
```

The script falls back to the tag `v<ZEBRAD_VERSION>` only when `ZEBRAD_COMMIT` is
missing, warns when a checkout lacks the commit and copies from its working tree, reports
dashboards Zebra added that it does not know about, and ends with the file, uid and title
of every dashboard. Update the table above and the launcher's picker when they change,
and redo the [known gaps](#known-gaps-with-zebrad-700) check.

## Known gaps with zebrad 7.0.0

The dashboards are written for Zebra's full stack and for metrics that have changed over
time; some panels query names the bundled zebrad does not export. The check: every
metric name in the dashboards' PromQL (`expr` in the JSON files) against the names zebrad
7.0.0 registers, as its Prometheus exporter writes them (dots become underscores,
histograms get `_bucket`, `_sum` and `_count`, counters get no `_total`). Of the 120 names
the dashboards use, 95 exist; the 25 others leave these panels or series empty:

**Metrics zebrad does not have**

- Zebra Overview: Process CPU Usage, Process Memory and Open File Descriptors
  (`process_*`: zebrad's exporter has no process collector); the `proofs_groth16_verified`
  series of Proof Verification Rate (zebrad counts `proofs_sapling_*` and
  `proofs_halo2_*`); Successful Handshakes by Implementation
  (`zcash_net_peer_version_outcomes_total`), also in peers.
- checkpoint verification: Checkpoint Height (`state_checkpoint_committed_block_height`)
  and the `state_checkpoint_committed_block_count` series of Checkpoint Pipeline
  Throughput (zebrad exports `state_checkpoint_finalized_block_height` and `_count`);
  the `sync_pending_blocks_len` series of Checkpoint Queues.
- block verification: the `state_checkpoint_queued_min_height` series of Verified Block
  Height.
- errors: all three Sighash Errors panels (`zebra_error_sapling_binding`,
  `zebra_sighash_error_sapling_spend`, `zebra_sighash_error_sprout_joinsplit`), so the
  dashboard is empty.
- transaction verification: the `proofs_groth16_verified` series of Proofs verified.

**Histograms queried as summaries**: these panels select a `quantile` label, which only
summaries have; zebrad exports these metrics as histograms (`_bucket`, `_sum`, `_count`),
so the panels stay empty. The RPC latency panels, which use `histogram_quantile` over
`rpc_request_duration_seconds_bucket`, show how it is done right.

- syncer: Sync Stage Duration Latency (`sync_stage_duration_seconds`), Block
  Download/Verify Duration (p95) (`sync_block_download_duration_seconds`,
  `sync_block_verify_duration_seconds`), and Sync Peer Responses, which reads
  `sync_extend_response_hash_count` and `sync_obtain_response_hash_count` without a
  histogram suffix.
- transaction verification: Batch Verification Duration by Verifier
  (`zebra_consensus_batch_duration_seconds`; the throughput panel next to it uses
  `_count` and works).
- RocksDB Database: Batch Commit Latency
  (`zebra_state_rocksdb_batch_commit_duration_seconds`).

**No Jaeger**: RPC Tracing reads `traces_spanmetrics_*` from Jaeger's span-metrics
connector in all eight panels. This stack has no Jaeger; Zebra's full stack
(`docker/docker-compose.observability.yml` in the Zebra repository) has one.

Two more reasons for an empty panel that are not gaps: zebrad registers a metric when it
first records it, so checkpoint metrics appear only during the checkpoint phase and RPC
metrics after the first request; and metrics are served only while zebrad runs. The raw
page at <http://127.0.0.1:9999/metrics> shows what exists right now.

The dashboards are vendored unchanged, so the fix for a gap is a change in the Zebra
repository followed by a re-vendor.

## Problems

[TROUBLESHOOTING.md](../TROUBLESHOOTING.md#observability-stack-prometheus-and-grafana):
blank Grafana tab, login page in the frame, target DOWN, ports in use, the raw metrics
page.
