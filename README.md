# Zebra Launcher

A Desktop app for installing and running Zebra.

> **Status:** the launcher is being rebuilt on Tauri 2 stable. The alpha releases
> below are from the previous prototype and are no longer maintained.

## Installation

To install the Zebra Launcher:

1. Download the installer for your operating system from the Github releases [here](https://github.com/ZcashFoundation/zebra-launcher/releases).
2. Run the installer

Note: It currently requires a 64-bit processor, and on macOS, an ARM-based processor such as the Apple M1.

## Screenshots

<img src="https://github.com/ZcashFoundation/zebra-app/assets/5491350/4fb8b310-15e7-47be-8650-18a24b733ebc" width="400">
&nbsp;
<img src="https://github.com/ZcashFoundation/zebra-app/assets/5491350/564bef27-5b82-4f1e-8e9f-af709bf48528" width="400">

## Documentation

- [Architecture](docs/ARCHITECTURE.md)
- [Development](docs/DEVELOPMENT.md)
- [Grafana dashboards](docs/GRAFANA.md) and the [observability stack](docs/observability/)
- [Releasing](docs/RELEASING.md)
- [Security](docs/SECURITY.md)
- [Troubleshooting](docs/TROUBLESHOOTING.md)
- [Uninstall](docs/UNINSTALL.md)

## Developer Setup

1. Make sure all of the [Tauri prerequisites](https://beta.tauri.app/guides/prerequisites/) are installed.
2. Install `pnpm`
3. Run `pnpm i` to install npm packages
4. Run `pnpm tauri dev` to start the Tauri app.
5. Add an empty `dist` folder to the root of this project to fix the Rust warning in `src-tauri/src/main.rs`
6. Install `Prettier` for formatting .ts/.tsx/.css files

#### Recommended IDE Setup

- [VS Code](https://code.visualstudio.com/) + [Tauri](https://marketplace.visualstudio.com/items?itemName=tauri-apps.tauri-vscode) + [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer)

## Release

The bundled `zebrad` version is pinned in `ZEBRAD_VERSION`. Binaries are no
longer committed: the release pipeline being built will fetch the Linux
`zebrad` from Zebra's signed releases and build the macOS and Windows ones
from that tag in CI. Until it lands, the prototype under `src-tauri` only
builds with a `zebrad-<target-triple>` sidecar placed in `src-tauri/binaries/`
by hand.
