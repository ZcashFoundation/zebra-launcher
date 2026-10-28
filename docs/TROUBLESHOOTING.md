# Troubleshooting

Start with **Settings → Copy diagnostics**: it copies the launcher and bundled zebrad
versions, the OS, the node status, the settings, the managed `zebrad.toml` and the last
200 lines of the launcher and zebrad logs to the clipboard, ready to paste into an issue.
It never includes the RPC cookie, and credentials in the Grafana URL are redacted. If the
clipboard cannot be written, the text is shown below the button to copy by hand.

The logs live in the app log directory: `~/.local/share/org.zfnd.zebra-launcher/logs`
(Linux), `~/Library/Logs/org.zfnd.zebra-launcher` (macOS),
`%LOCALAPPDATA%\org.zfnd.zebra-launcher\logs` (Windows). The launcher writes one
`launcher.<date>.log` per day (14 kept); zebrad's output goes to `zebrad.log`, rotated at
10 MB. If the launcher cannot start at all, an error dialog names this directory.

## No tray icon (Linux)

On Linux the tray icon needs `libayatana-appindicator3` (a dependency of the deb, bundled
in the AppImage) and a desktop that shows AppIndicators. Stock GNOME does not: install
the "AppIndicator and KStatusNotifierItem Support" extension (Ubuntu ships it enabled).
On Linux the icon only has a menu (Show, Start zebrad, Stop zebrad, Quit); clicking it
does nothing else, and it shows no tooltip.

If the library is missing, the launcher logs that it could not create the tray icon,
and closing the window quits (stopping zebrad) instead of hiding it. If the library is
present but the desktop does not show the icon, closing the window hides it while zebrad
keeps running: start the launcher again to bring the window back, or turn off
**Settings → Close to tray**.

## Not enough disk space

The first run shows the free space where the chain state will go. Mainnet state needs
hundreds of GB and keeps growing; Testnet needs much less. If the drive is too small,
choose a data directory on a larger drive before starting the node; existing state is
not moved, so delete it afterwards if you no longer need it (see
[UNINSTALL.md](UNINSTALL.md#delete-node-data)). zebrad fails when the disk fills up.

## A port is already in use

zebrad stops with "already in use" when another program holds one of its ports. The
launcher shows the node as failed ("a port zebrad needs is already in use") and does not
retry. It cannot move the port for you: the RPC and metrics addresses are managed by the
launcher and are read-only in Configure. Free the port, then press Start.

| Port | Used for |
| --- | --- |
| 8233 (18233 Testnet) | P2P |
| 8232 (18232 Testnet) | RPC, loopback |
| 9999 | metrics, loopback |

The usual cause is a second zebrad, for example one started from a terminal. Find it with
`lsof -i :8233` (Linux, macOS) or `Get-NetTCPConnection -LocalPort 8233` (Windows).

## Database likely already open

Another zebrad is using the same state directory, which is locked. The launcher does not
retry. Close the other zebrad.

If it is the launcher's own zebrad from an earlier run (the launcher crashed or was
killed), the launcher finds it through its pid file at the next start and shows a banner
instead of starting a second one: "A zebrad from an earlier launcher run is still running
(pid …), so the launcher cannot start its own." Its **Stop it and start** button
terminates that zebrad and starts a fresh one; or wait for it to exit by itself.

The launcher never reuses a terminal zebrad's state directory, so otherwise this means two
launchers or a manual zebrad pointed at the launcher's data dir.

## Blank window on Linux

Some GPU drivers (often NVIDIA, or VMs) leave the WebKitGTK window blank. Start the
launcher with the DMA-BUF renderer disabled:

```sh
WEBKIT_DISABLE_DMABUF_RENDERER=1 zebra-launcher
```

For the AppImage: `WEBKIT_DISABLE_DMABUF_RENDERER=1 ./zebra-launcher_<version>_x86_64.AppImage`.
The deb installs the binary as `/usr/bin/zebra-launcher`. To make the setting permanent
for the desktop entry, copy the `.desktop` file the package installs under
`/usr/share/applications/` to `~/.local/share/applications/` and prefix its `Exec=` line
with `env WEBKIT_DISABLE_DMABUF_RENDERER=1`.

## macOS: "cannot be opened" (Gatekeeper)

Release builds are signed and notarized and should open normally. If macOS still blocks
the app, check the download as described in [SECURITY.md](SECURITY.md#verifying-a-download).
Unsigned development or nightly builds open through System Settings → Privacy & Security
→ Open Anyway (the exact wording differs between macOS versions).

## Windows: SmartScreen warning

A new signing identity may show "Windows protected your PC" until it builds reputation.
Choose More info → Run anyway after checking the signature
(`Get-AuthenticodeSignature`).

## Firewall prompt for port 8233

zebrad listens for peers on `[::]:8233` (P2P; `18233` on Testnet). The first time the
node starts, macOS and Windows ask whether to allow incoming connections for
`zebra-launcher-zebrad`; the first-run screen warns about this prompt. Allowing it helps
the network; denying it still lets your node sync through outbound connections. RPC and
metrics are loopback-only and never need a firewall rule. To change your answer later:
Windows Security → Firewall & network protection → Allow an app through firewall; macOS
System Settings → Network → Firewall → Options.

## Node stopped: release too old

On Mainnet, zebrad warns about 42 days and refuses to run about 56 days after its release.
The launcher shows a countdown, then a blocking card. Install the latest launcher release,
or let the updater do it: with updates set to Notify or Auto (Settings → Updates), a
banner at the top offers **Install** when a new launcher is available; in Auto mode the
update is downloaded in the background and, once the halt is under 14 days away or zebrad
has halted, installed after a 60 s countdown that **Cancel** stops. The halt card itself
has **Check for updates**. The `.deb` cannot update itself and shows **Open releases
page** instead.

## Config change rolled back

If zebrad rejects an edited config, the launcher restores the previous file, restarts
zebrad, and shows zebrad's error. Fix the key named in the error and save again.

## Observability stack (Prometheus and Grafana)

The optional Prometheus and Grafana stack in [`observability/`](observability/README.md)
is behind most blank Grafana tabs and empty panels. The launcher's Dashboard and Logs
tabs work without it. The commands below use the Docker Desktop form; on Linux add
`-f docs/observability/docker-compose.linux.yml` after the first `-f`.

### The stack does not start

- `docker compose up` ends with "dependency failed to start" and names the Prometheus
  container as unhealthy: Prometheus did not pass its health check, usually because
  `prometheus.yml` (or `prometheus.linux.yml`) does not parse after an edit. Read
  `docker compose -f docs/observability/docker-compose.yml logs prometheus`; the first
  error line names the file and line.
- "Bind for 127.0.0.1:3000 failed: port is already allocated" (or 9090), or on Linux a
  service that exits at once with "address already in use": see
  [Port 9090 or 3000 is already in use](#port-9090-or-3000-is-already-in-use).
- A pull error: the first start downloads the two images and needs the network.

### The Grafana tab is blank or says the page refused to connect

Work down the list:

1. Is the stack running? `docker compose -f docs/observability/docker-compose.yml ps`
   should list `grafana` as `Up … (healthy)`. Nothing listed: start it (the README has
   the commands). `(unhealthy)` or restarting: `… logs grafana`.
2. Is the URL right? The hint under the toolbar names the URL the tab uses, and "Open in
   browser" shows whether that Grafana answers outside the launcher. The URL must start
   with `http://127.0.0.1:` or `http://localhost:`; the stack's port is 3000 unless you
   moved it. Change it in Settings → Metrics.
3. Does Grafana allow framing?

   ```sh
   curl -sI http://127.0.0.1:3000/login | grep -i x-frame-options   # should print nothing
   ```

   If a header is printed, `GF_SECURITY_ALLOW_EMBEDDING` did not reach the container:
   `docker compose -f docs/observability/docker-compose.yml up -d --force-recreate grafana`.
   A Grafana that is not this stack needs the settings in
   [observability/README.md](observability/README.md#how-the-launcher-embeds-grafana).
4. On macOS, whether WKWebView loads `http://127.0.0.1:3000` inside the launcher's
   `tauri://localhost` page (mixed content) has not been verified yet; if it is blocked,
   use "Open in browser".

### The frame shows a login page

Anonymous access is off. This should succeed without credentials:

```sh
curl -sf http://127.0.0.1:3000/api/dashboards/uid/zebra-overview > /dev/null && echo ok
```

If it does not, `GF_AUTH_ANONYMOUS_ENABLED` did not reach the container; recreate it as
above. Whether anonymous Viewer works inside the sandboxed iframe has not been verified
yet on an installed bundle on any OS. Logging in inside the iframe may not persist,
because Grafana's session cookie is `SameSite=Lax` and the launcher page is a different
site; log in from a browser instead.

### Prometheus target DOWN, dashboards show "No data"

Open <http://127.0.0.1:9090/targets> and look at the `zebrad` job.

**UP, but a panel is empty.** The panel may query a metric zebrad 7.0.0 does not export
([known gaps](observability/README.md#known-gaps-with-zebrad-700)), or one zebrad has
not recorded yet: checkpoint metrics exist only during the checkpoint phase of the
initial sync, RPC metrics after the first request. If the dashboard's job picker (top
left) shows a job other than `zebrad`, pick `All` or `zebrad`. Prometheus keeps 15 days
of data, so a node that was off shows a gap, not an error.

**DOWN.** The target's "Error" column says why:

- "connection refused" and zebrad is not running: metrics are served only while zebrad
  runs. Start it in the launcher, then check `curl -s http://127.0.0.1:9999/metrics | head`.
  The target recovers at the next scrape (10 s).
- "connection refused" on Linux while zebrad runs: the stack was started without
  `docker-compose.linux.yml`. On the bridge network Prometheus scrapes
  `host.docker.internal`, which is the host's bridge address, and zebrad listens on
  `127.0.0.1` only. `docker compose -f docs/observability/docker-compose.yml down`, then
  `up -d` with both `-f` files.
- DOWN on Docker Desktop (macOS, Windows): Prometheus scrapes `host.docker.internal:9999`,
  Docker Desktop's name for the host. Whether that reaches a service bound to `127.0.0.1`,
  as the launcher binds zebrad's metrics, has not been verified yet; the stack has only
  been run on Linux. If the target stays DOWN with zebrad running, try Docker Desktop's
  host networking (Settings → Resources → Network → Enable host networking) with the Linux
  override, and please report the result in an issue so the README can record it.

### Port 9090 or 3000 is already in use

`docker compose up` fails with "port is already allocated" (Docker Desktop), or on Linux
Grafana or Prometheus exits at once with "address already in use" (see `… logs grafana`).
Find the holder with `lsof -i :3000` (Linux, macOS) or
`Get-NetTCPConnection -LocalPort 3000` (Windows); 3000 is a common development-server
port, 9090 another Prometheus. Stop it, or move the stack's port:

- Docker Desktop: change the host side of the `ports:` mapping in `docker-compose.yml`,
  for example `"127.0.0.1:3001:3000"`. The container ports and the health checks stay as
  they are.
- Linux: in `docker-compose.linux.yml`, change `GF_SERVER_HTTP_PORT` for Grafana, or
  `--web.listen-address` and `PROMETHEUS_URL` for Prometheus. The health checks in
  `docker-compose.yml` probe `127.0.0.1:9090` and `127.0.0.1:3000`, which on the host
  network are the moved ports, so add the same `healthcheck:` block with the new port to
  that service in the override, or it stays unhealthy and Grafana does not start.

Then `up -d` and, for Grafana, set the launcher's Grafana URL to the new port.

### The raw metrics page

zebrad serves its metrics as plain text at <http://127.0.0.1:9999/metrics> while it runs
(the launcher sets `metrics.endpoint_addr`; loopback only, so only this machine can read
it). Open it in a browser or run `curl -s http://127.0.0.1:9999/metrics | less`. Each
metric has a `# TYPE name counter|gauge|histogram` line followed by its samples; the
names are zebrad's dotted names with underscores. Use it to see whether a metric a panel
queries exists at all, or to read a value without Prometheus and Grafana.
