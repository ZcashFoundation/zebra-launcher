# Grafana

The launcher's status strip (phase, height, sync %, peers, mempool, versions, EOS
countdown) comes from zebrad's RPC and works without Grafana. Grafana is optional: if
you run it, the launcher's Grafana tab embeds Zebra's dashboards in an iframe, with a
dashboard picker, a time range and a refresh interval (see [The Grafana tab](#the-grafana-tab)).

The Prometheus and Grafana stack in [`observability/`](observability/README.md) scrapes
the zebrad the launcher started and provisions Zebra's own dashboards, set up so the
tab can embed them. Its README covers starting, checking and updating it, where its
data lives, the dashboards and their known gaps. This page covers the launcher's side.

## What the launcher configures

The launcher writes these keys into its managed `zebrad.toml`:

| Key | Value |
| --- | --- |
| `metrics.endpoint_addr` | `127.0.0.1:9999` |
| `rpc.listen_addr` | `127.0.0.1:8232` (`18232` on Testnet) |
| `rpc.enable_cookie_auth` | `true` |

Prometheus only needs the metrics endpoint. The launcher never moves these ports (they are
read-only in Configure), so the targets in `observability/prometheus.yml` and
`observability/prometheus.linux.yml` always match.

## The Grafana tab

The launcher's **Grafana** tab embeds Grafana in an iframe once a Grafana URL is set.

Without a URL, the tab shows a setup card: what the stack in this repository provides and
where to get it (an "Open the repository" button opens
<https://github.com/ZcashFoundation/zebra-launcher> in the browser, since the installed app
does not carry the compose files), the compose command from the stack's README with a
Copy button (and the Linux override), the embedding requirement, and a URL field
prefilled with `http://127.0.0.1:3000`. "Use this Grafana" checks the URL against the
rule below and saves it into the launcher settings; the dashboards appear as soon as it
is stored.

With a URL set, a toolbar sits above the dashboard:

| Control | What it does |
| --- | --- |
| Dashboard | Picks one of the 14 shipped dashboards (see [Dashboards](#dashboards)). The default, `zebra-overview`, is also Grafana's home dashboard in this stack. When the saved URL names a dashboard that is not one of the 14, the picker lists it first as "From your URL". |
| Time range | Last 15 minutes, last hour (default), 6 hours, 24 hours or 7 days: `from=now-…&to=now`. |
| Refresh | Off, every 10 s (default), 30 s or minute: `refresh=…`, left out when off. |
| Reload | Re-creates the iframe with the same URL. |
| Open in browser | Opens the saved Grafana URL with the system browser (the backend's `open_path("grafana")`, which fails when no URL is set). |
| Change | Opens Settings, where the URL lives. |

The dashboard, time range and refresh choices are remembered in the webview's local
storage. The embedded dashboard runs in kiosk mode (`kiosk`: no Grafana menus), and its
theme follows the launcher's appearance (`theme=dark` or `theme=light`). Under the toolbar
a hint names the URL to check when the frame stays blank.

The dashboard loads when the Grafana tab is first opened, not when the launcher starts,
and stays loaded while other tabs are shown, so Grafana does not run (or refresh) for a
user who never opens the tab.

### The URL

The URL is the "Grafana dashboard URL" field in Settings → Metrics. The setup card writes
the same field, and the Settings page still accepts, validates and clears it. Three forms
work:

| Form | Example | The tab embeds |
| --- | --- | --- |
| An origin | `http://127.0.0.1:3000` | `http://127.0.0.1:3000/d/<uid>?kiosk&from=…` |
| An origin with a path prefix | `http://localhost:3000/grafana` | `http://localhost:3000/grafana/d/<uid>?kiosk&from=…` |
| A full dashboard URL | `http://127.0.0.1:3000/d/zebra-overview/zebra-overview?orgId=1&kiosk` | `http://127.0.0.1:3000/d/<uid>?orgId=1&kiosk&from=…` |

The `/d/<uid>` segment is replaced by the picked dashboard and any slug after it is
dropped (a bare `/d` or `/d/` counts as the segment with no uid); the query keys `kiosk`,
`from`, `to`, `refresh` and `theme` are replaced by the
toolbar's values; other keys (`orgId`, `var-*`) and a path prefix are kept; a fragment is
dropped; trailing slashes do not matter. The rules and their tests live in
`ui/src/grafana_url.rs`.

- The URL must match the launcher's one Grafana URL rule, which mirrors its CSP
  (`frame-src http://127.0.0.1:* http://localhost:*`): scheme `http`, host exactly
  `127.0.0.1` or `localhost`, an optional port from 1 to 65535, and any path, query or
  fragment. `https`, `[::1]`, other loopback addresses such as `127.0.0.2`, user info
  (`user@`), backslashes and spaces are refused. The Grafana tab, the Settings page and
  the backend all check it with the same function, `launcher_types::is_loopback_http_url`.
- The iframe is sandboxed with `allow-scripts allow-same-origin allow-forms` (Grafana
  keeps its own origin, cookies and API calls, and its own forms such as the dashboard
  search work), with no top navigation, popups or referrer, and it gets no access to the
  launcher's IPC.

Grafana must allow being framed and show dashboards without a login. The stack's
`docker-compose.yml` sets the options for that; they are listed, with what another
Grafana needs, in
[observability/README.md](observability/README.md#how-the-launcher-embeds-grafana).

## Dashboards

The picker lists the 14 dashboards the stack provisions, by uid, with its own
sentence-case labels (`ui/src/grafana_url.rs`). Their files, uids, Grafana titles and
contents are in [observability/README.md](observability/README.md#dashboards), and the
panels that stay empty with the bundled zebrad in its
[known gaps](observability/README.md#known-gaps-with-zebrad-700). When the vendored
dashboards change, the picker's list changes with them.

## Troubleshooting

See [TROUBLESHOOTING.md](TROUBLESHOOTING.md#observability-stack-prometheus-and-grafana).
The status strip works without Grafana.
