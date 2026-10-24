# Zebra Launcher

A Desktop app for installing and running Zebra.

> **Status:** the launcher is being rebuilt on Tauri 2 stable. The alpha releases
> below are from the previous prototype and are no longer maintained.

## Installation

To install the Zebra Launcher:

1. Download the installer for your operating system from the Github releases [here](https://github.com/ZcashFoundation/zebra-launcher/releases).
2. Run the installer

Note: It currently requires a 64-bit processor, and on macOS, an ARM-based processor such as the Apple M1.

## What it does

- Runs the bundled `zebrad` as a supervised child process: start, stop and restart from
  the window or the tray, graceful shutdown, crash backoff, and a clear reason when
  zebrad cannot run (bad config, port in use, database locked, end of support).
- **Dashboard:** sync progress with the block rate and the time left to the tip, stat
  tiles and charts of height, peers and mempool since the launcher opened, the sync
  stage and recent problems read from zebrad's own log, and the node's addresses.
- **Logs:** zebrad's output with level, target and text filters, match highlighting,
  and an analysis panel (sync stage, deduplicated warnings and errors, lines per
  minute, the startup timeline of the current run).
- **Grafana:** your own Grafana embedded, with a picker for the 14 Zebra dashboards the
  repository ships, a time range and a refresh interval ([GRAFANA.md](docs/GRAFANA.md)).
- **Configure:** edit the managed `zebrad.toml` with validation and automatic rollback
  when zebrad rejects a change.
- **Settings:** opt-in launcher updates (off, notify, automatic), close to tray, light or
  dark appearance, the chain data folder with its free space, diagnostics for bug
  reports, and deleting the node data.

## Documentation

- [Architecture](docs/ARCHITECTURE.md)
- [Development](docs/DEVELOPMENT.md)
- [Grafana dashboards](docs/GRAFANA.md) and the [observability stack](docs/observability/)
- [Releasing](docs/RELEASING.md)
- [Security](docs/SECURITY.md)
- [Troubleshooting](docs/TROUBLESHOOTING.md)
- [Uninstall](docs/UNINSTALL.md)

## Developer setup

Rust only: no Node, npm or pnpm. See [DEVELOPMENT.md](docs/DEVELOPMENT.md) for the
prerequisites per platform, the quality gate and the test setup.

```sh
scripts/dev-sidecar.sh                    # builds a fake zebrad for development
cargo install --locked trunk tauri-cli    # once
cargo tauri dev
```

`trunk serve` in `ui/` shows the UI alone against a mock backend at
http://localhost:1420.

## Release

The bundled `zebrad` version is pinned in [`ZEBRAD_VERSION`](ZEBRAD_VERSION). Binaries
are never committed: CI fetches the Linux `zebrad` from Zebra's signed releases and
builds the macOS and Windows ones from the same tag ([SIDECARS.md](docs/SIDECARS.md)).
Releases are built, signed and published by the workflow described in
[RELEASING.md](docs/RELEASING.md).
