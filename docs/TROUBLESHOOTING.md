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

## Grafana panel blank or "No data"

See [GRAFANA.md](GRAFANA.md#troubleshooting). The status strip works without Grafana.
