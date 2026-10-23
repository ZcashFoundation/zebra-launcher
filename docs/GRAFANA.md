# Grafana

The launcher's status strip (phase, height, sync %, peers, mempool, versions, EOS
countdown) comes from zebrad's RPC and works without Grafana. Grafana is optional: if
you run it, the Dashboard page embeds Zebra's overview dashboard in an iframe.

The stack in [`observability/`](observability/) runs Prometheus and Grafana in Docker,
scrapes the zebrad the launcher started, and provisions Zebra's own dashboards.

## What the launcher configures

The launcher writes these keys into its managed `zebrad.toml`:

| Key | Value |
| --- | --- |
| `metrics.endpoint_addr` | `127.0.0.1:9999` |
| `rpc.listen_addr` | `127.0.0.1:8232` (`18232` on Testnet) |
| `rpc.enable_cookie_auth` | `true` |

Prometheus only needs the metrics endpoint. If the launcher moved a port because it was
in use, change the target in `observability/prometheus.yml` and
`observability/prometheus.linux.yml` to the port shown in the launcher's config.

## Start the stack

From the repository root:

```sh
# Docker Desktop (macOS, Windows)
docker compose -f docs/observability/docker-compose.yml up -d

# Linux (Docker Engine)
docker compose -f docs/observability/docker-compose.yml -f docs/observability/docker-compose.linux.yml up -d
```

| Service | URL | Notes |
| --- | --- | --- |
| Grafana | <http://127.0.0.1:3000> | anonymous Viewer; `admin`/`admin` to edit |
| Prometheus | <http://127.0.0.1:9090> | targets at `/targets` |

Both bind to `127.0.0.1` only. To set the admin password, put
`GF_SECURITY_ADMIN_PASSWORD=...` in `docs/observability/.env` before the first start.

Stop with `docker compose -f docs/observability/docker-compose.yml down` (add `-v` to
delete the Prometheus and Grafana volumes).

### Why two compose files

zebrad's metrics are bound to `127.0.0.1` on the host, and a container on the default
Docker bridge cannot reach a host port bound to `127.0.0.1`.

- **Linux:** `docker-compose.linux.yml` puts Prometheus and Grafana on the host network
  (`network_mode: host`). Prometheus scrapes `127.0.0.1:9999` with
  `prometheus.linux.yml`; Prometheus listens on `127.0.0.1:9090` and Grafana on
  `127.0.0.1:3000`, since published ports do not apply to host networking.
- **Docker Desktop (macOS, Windows):** the default file scrapes `host.docker.internal:9999`,
  with `extra_hosts: ["host.docker.internal:host-gateway"]`.

> **To verify in M7:** whether Docker Desktop forwards `host.docker.internal:9999` to a
> host service bound to `127.0.0.1` on macOS and Windows. If it does not, the fallbacks
> are Docker Desktop's host networking (Settings → Resources → Network → Enable host
> networking) with the Linux override, or a metrics bind on the Docker bridge address.
> The second conflicts with the launcher's loopback-only rule for metrics and needs a
> decision first. Record the result here.

## Embed it in the launcher

Grafana URL to enter in the launcher's settings (the Settings page lands in M8):

```text
http://127.0.0.1:3000/d/zebra-overview?kiosk&theme=dark
```

`zebra-overview` is the `uid` of `zebra_overview.json`, which is also Grafana's home
dashboard in this stack.

- The URL must be loopback (`http://127.0.0.1:<port>` or `http://localhost:<port>`). The
  launcher refuses other hosts, and its build-time CSP only allows
  `frame-src http://127.0.0.1:* http://localhost:*`.
- The iframe is sandboxed with `allow-scripts allow-same-origin` (Grafana keeps its own
  origin, cookies and API calls), with no top navigation, popups or referrer, and it gets
  no access to the launcher's IPC.
- With no Grafana URL set, the Dashboard page shows only the status strip.

Grafana settings this relies on, all set in `docker-compose.yml`:

| Setting | Why |
| --- | --- |
| `GF_SECURITY_ALLOW_EMBEDDING=true` | Grafana sends `X-Frame-Options: deny` otherwise |
| `GF_AUTH_ANONYMOUS_ENABLED=true`, `GF_AUTH_ANONYMOUS_ORG_ROLE=Viewer` | the iframe shows dashboards without a login |
| `GF_AUTH_DISABLE_LOGIN_FORM=false` | admins can still log in from a browser |
| `GF_DASHBOARDS_DEFAULT_HOME_DASHBOARD_PATH` | `/` opens the Zebra overview |

## Dashboards

`observability/grafana/dashboards/` holds Zebra's 14 dashboards, copied unchanged from
`docker/observability/grafana/dashboards` at the tag `v<ZEBRAD_VERSION>` of
[ZcashFoundation/zebra](https://github.com/ZcashFoundation/zebra). They query the
Prometheus datasource with uid `zebra-prometheus`, which
`observability/grafana/provisioning/datasources/datasources.yml` provides.

When `ZEBRAD_VERSION` changes, re-vendor them in the same PR:

```sh
docs/observability/sync-dashboards.sh               # download from GitHub at v$(cat ZEBRAD_VERSION)
docs/observability/sync-dashboards.sh ../zebra      # or read the tag from a local Zebra checkout
```

The script reports dashboards Zebra added that it does not know about. Dashboards are
provisioned read-only; to change one, use "Save as" in Grafana, or edit Zebra's copy
upstream.

`rpc_tracing.json` stays empty: it reads Jaeger span metrics, and this stack has no
Jaeger. Zebra's full stack (`docker/docker-compose.observability.yml` in the Zebra repo)
includes it.

## Troubleshooting

**The iframe is blank or says the page refused to connect.**
Check that Grafana allows framing:

```sh
curl -sI http://127.0.0.1:3000/login | grep -i x-frame-options   # should print nothing
```

If a header is printed, `GF_SECURITY_ALLOW_EMBEDDING` did not reach the container:
recreate it with `docker compose ... up -d --force-recreate grafana`. Also check that
the launcher's Grafana URL starts with `http://127.0.0.1:` or `http://localhost:`.
On macOS, whether WKWebView loads `http://127.0.0.1:3000` inside the launcher's
`tauri://localhost` page (mixed content) is **to verify in M7**; if it is blocked, use
"Open in browser".

**The iframe shows a login page.**
Anonymous access is off. Check that this succeeds without credentials:

```sh
curl -sf http://127.0.0.1:3000/api/dashboards/uid/zebra-overview > /dev/null && echo ok
```

Whether anonymous Viewer works inside the sandboxed iframe on every OS is **to verify in
M7**. Logging in inside the iframe may not persist, because Grafana's session cookie is
`SameSite=Lax` and the launcher page is a different site; log in from a browser instead.

**Dashboards show "No data".**

1. Is zebrad running in the launcher, with metrics on? `curl -s http://127.0.0.1:9999/metrics | head`
2. Is the target up? Open <http://127.0.0.1:9090/targets>.
3. Target down on Linux: you started without `docker-compose.linux.yml`.
4. Target down on Docker Desktop: see "Why two compose files" above.
5. Wrong port: the launcher moved the metrics port; update the Prometheus target.

**Port 3000 or 9090 already in use.**
Change the host side of the `ports:` mapping in `docker-compose.yml`; on Linux change
`GF_SERVER_HTTP_PORT`, `--web.listen-address` and `PROMETHEUS_URL` in the override
instead. Then update the launcher's Grafana URL.
