# Troubleshooting

> Skeleton. Entries marked TODO are completed as the features land (M5–M12).

Start with **Settings → Copy diagnostics**: it copies the launcher and bundled zebrad
versions, the OS, the node status, the settings, the managed `zebrad.toml` and the last
200 lines of the launcher and zebrad logs to the clipboard, ready to paste into an issue.
It never includes the RPC cookie, and credentials in the Grafana URL are redacted.

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
launcher does not retry; it offers alternative ports for RPC and metrics and writes them
into its `zebrad.toml`.

| Port | Used for |
| --- | --- |
| 8233 (18233 Testnet) | P2P |
| 8232 (18232 Testnet) | RPC, loopback |
| 9999 | metrics, loopback |

The usual cause is a second zebrad, for example one started from a terminal. Find it with
`lsof -i :8233` (Linux, macOS) or `Get-NetTCPConnection -LocalPort 8233` (Windows).
If you moved the metrics port and use Grafana, update the Prometheus target (see
[GRAFANA.md](GRAFANA.md)). TODO(M4): screenshots of the remediation dialog.

## Database likely already open

Another zebrad is using the same state directory, which is locked. The launcher does not
retry. Close the other zebrad. If it is a zebrad left over from a launcher crash, the
launcher offers to wait for it or kill it on startup.

The launcher never reuses a terminal zebrad's state directory, so this normally means two
launchers or a manual zebrad pointed at the launcher's data dir. TODO(M3): describe the
stale-process dialog.

## Blank window on Linux

Some GPU drivers (often NVIDIA, or VMs) leave the WebKitGTK window blank. Start the
launcher with the DMA-BUF renderer disabled:

```sh
WEBKIT_DISABLE_DMABUF_RENDERER=1 zebra-launcher
```

For the AppImage: `WEBKIT_DISABLE_DMABUF_RENDERER=1 ./Zebra_Launcher_*.AppImage`.
TODO(M5): confirm the binary name of the deb and how to persist this in the desktop
entry.

## macOS: "cannot be opened" (Gatekeeper)

Release builds are signed and notarized and should open normally. If macOS still blocks
the app, check the download as described in [SECURITY.md](SECURITY.md#verifying-a-download).
Unsigned development or nightly builds open through System Settings → Privacy & Security
→ Open Anyway. TODO(M10): exact wording on macOS 15.

## Windows: SmartScreen warning

A new signing identity may show "Windows protected your PC" until it builds reputation.
Choose More info → Run anyway after checking the signature
(`Get-AuthenticodeSignature`). TODO(M10): confirm behaviour with Azure Artifact Signing.

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
The launcher shows a countdown, then a blocking card. Install the latest launcher release
(or let the updater do it, if enabled). TODO(M11): describe the in-app update path.

## Config change rolled back

If zebrad rejects an edited config, the launcher restores the previous file, restarts
zebrad, and shows zebrad's error. Fix the key named in the error and save again.

## Grafana panel blank or "No data"

See [GRAFANA.md](GRAFANA.md#troubleshooting). The status strip works without Grafana.
