# Troubleshooting

> Skeleton. Entries marked TODO are completed as the features land (M5–M12).

Start with **Settings → Copy diagnostics** (M8): it bundles the launcher log, the tail of
the zebrad log, versions and the config, ready to attach to an issue.
TODO(M8): list where `launcher.log` and `zebrad.log` live on each OS.

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

zebrad listens for peers on `[::]:8233` (P2P). macOS and Windows ask whether to allow
incoming connections the first time. Allowing it helps the network; denying it still lets
your node sync through outbound connections. RPC and metrics are loopback-only and never
need a firewall rule. TODO(M8): match the first-run note's wording.

## Node stopped: release too old

On Mainnet, zebrad warns about 42 days and refuses to run about 56 days after its release.
The launcher shows a countdown, then a blocking card. Install the latest launcher release
(or let the updater do it, if enabled). TODO(M11): describe the in-app update path.

## Config change rolled back

If zebrad rejects an edited config, the launcher restores the previous file, restarts
zebrad, and shows zebrad's error. Fix the key named in the error and save again.

## Grafana panel blank or "No data"

See [GRAFANA.md](GRAFANA.md#troubleshooting). The status strip works without Grafana.
