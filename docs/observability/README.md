# Observability stack

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

> **Not yet verified on macOS and Windows:** whether Docker Desktop forwards
> `host.docker.internal:9999` to a host service bound to `127.0.0.1`. The stack has only
> been run on Linux so far. If it does not, the fallbacks are Docker Desktop's host
> networking (Settings → Resources → Network → Enable host networking) with the Linux
> override, or a metrics bind on the Docker bridge address. The second conflicts with the
> launcher's loopback-only rule for metrics and needs a decision first. Record the result
> here once checked.

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
