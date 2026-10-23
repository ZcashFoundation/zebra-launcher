# Development

How to build, run and test Zebra Launcher. For how the pieces fit, read
[ARCHITECTURE.md](ARCHITECTURE.md) first.

## Prerequisites

All platforms:

- [rustup](https://rustup.rs). The toolchain, `rustfmt`, `clippy` and the
  `wasm32-unknown-unknown` target come from [`rust-toolchain.toml`](../rust-toolchain.toml)
  and are installed on first use; do not override them.
- Trunk and the Tauri CLI (needed from M5, when the `ui` crate and the new `src-tauri` land):

  ```sh
  cargo install --locked trunk tauri-cli
  ```

  CI installs pinned versions with `cargo binstall`; `tauri-cli` must match the minor
  version of the `tauri` crate.
- `cargo install --locked cargo-deny` to run the license and advisory checks locally.

No Node, npm or pnpm is needed once M5 lands.

### Linux (Debian, Ubuntu)

```sh
sudo apt-get install libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev libssl-dev
```

Plus a C toolchain (`build-essential`). CI builds in an `ubuntu:22.04` container, which is
the oldest supported base.

### macOS

Xcode Command Line Tools (`xcode-select --install`).

### Windows

Visual Studio Build Tools with the "Desktop development with C++" workload, and the
WebView2 runtime (preinstalled on Windows 11). See Tauri's
[prerequisites](https://v2.tauri.app/start/prerequisites/) for details.

## Run the app

The Tauri build needs a sidecar file at
`src-tauri/binaries/zebra-launcher-zebrad-<host target triple>` (`.exe` on Windows).
Binaries are never committed. The prefixed name takes effect with the rewritten app
crate (milestone M5); until then the old `src-tauri/tauri.conf.json` still expects
`binaries/zebrad-<triple>`.

- **Fake sidecar:** `scripts/dev-sidecar.sh` builds `fake-zebrad` and copies it into
  place. It starts fast, needs no chain state, and can be scripted into failure modes
  (`ok`, `crash`, `config-error`, `eos-warn`, `eos-halt`, `port-in-use`, `db-locked`,
  `hang-on-signal`, `stderr-flood`, `non-utf8`, `stdin-close`).
  <!-- TODO(M5): document dev-sidecar.sh's arguments once it lands. -->
- **Real zebrad:** the version in [`ZEBRAD_VERSION`](../ZEBRAD_VERSION), fetched or built by
  the M9 scripts (`scripts/fetch-zebrad-linux.sh`, `scripts/build-zebrad.sh`).
  <!-- TODO(M9): document fetching or building the real sidecar. -->

Then:

```sh
cargo tauri dev
```

This runs `trunk serve` for the `ui` crate and starts the app with hot reload.
<!-- TODO(M5): confirm the command and any environment variables once src-tauri is rewritten. -->

On Linux, if the window is blank, see [TROUBLESHOOTING.md](TROUBLESHOOTING.md).

To see the Grafana embed, start the stack in [GRAFANA.md](GRAFANA.md).

## Quality gate

CI ([`.github/workflows/ci.yml`](../.github/workflows/ci.yml)) runs these on every PR;
run them before pushing:

```sh
cargo fmt --all --check
RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets --locked
cargo test --workspace --locked
cargo deny check
```

From M5, also `trunk build` in `ui/`. Windows and macOS CI run clippy and tests too, so
`cfg(windows)` and `cfg(target_os = "macos")` code is checked; when you change it, run
clippy for that target or rely on CI.

Tests in `launcher-core` drive the `fake-zebrad` binary (`CARGO_BIN_EXE_fake-zebrad`)
and put a timeout on every await, so a hang fails the test instead of stalling CI.

## No mutexes

Rust code in this repository uses no locks. [`clippy.toml`](../clippy.toml) bans
`std::sync::Mutex`, `std::sync::RwLock`, `tokio::sync::Mutex`, `tokio::sync::RwLock`,
`futures::lock::Mutex` and the `parking_lot` equivalents, and the workspace denies
`clippy::disallowed_types`.

Use instead:

- an actor task that owns the state and receives commands on an `mpsc` channel,
  replying through `oneshot`;
- `tokio::sync::watch` to publish the latest value;
- `tokio::sync::broadcast` for streams with several subscribers;
- atomics for flags and counters;
- `tokio::sync::Semaphore` to reserve a slot.

## Log fixtures

`crates/launcher-core/tests/fixtures/` holds stdout and stderr captured from a real
zebrad. The parser and classifier tests pin the exact lines the supervisor depends on.
[`crates/launcher-core/tests/fixtures/README.md`](../crates/launcher-core/tests/fixtures/README.md)
lists how each file was produced.

Re-capture them when the pinned zebrad changes its log wording, normally as part of a
`ZEBRAD_VERSION` bump:

1. Get the new zebrad (release build with `default-release-binaries`).
2. Repeat each capture listed in the fixtures README (regtest start and `SIGTERM` after
   20 s, a bad config key, a missing config, a second instance on the same state, a
   metrics port already bound, `zebrad generate -o`).
3. Replace absolute paths with `<DATA_DIR>`, and rename the files for the new version.
4. Update the README (version, git commit, date, exit codes) and the test paths in
   `crates/launcher-core/tests/fixtures.rs`.
5. Run `cargo test -p launcher-core`. A failing classifier test means zebrad changed a
   line the supervisor relies on: update the classifier and `fake-zebrad` together.

## Real-zebrad contract tests

`crates/launcher-core/tests/real_zebrad.rs` runs a real zebrad through the supervisor
on Regtest, with every listener on loopback and all state in a temporary directory. It
checks the startup events in order, the reported RPC and metrics addresses, the RPC
cookie file, a clean `SIGTERM` stop with zebrad's shutdown lines, and that a second
zebrad on the same state fails as `DbLocked` and is not retried.

The tests are skipped (they pass after printing a note) unless `ZEBRAD_BIN` names a
zebrad binary, so `cargo test` needs no zebrad and no network. Run them locally against
the zebrad you are about to pin:

```sh
ZEBRAD_BIN=/path/to/zebrad cargo test -p launcher-core --test real_zebrad -- --nocapture
```

They take a few seconds. On failure they print zebrad's whole output. Run them on every
`ZEBRAD_VERSION` bump, next to re-capturing the [log fixtures](#log-fixtures).

## Observability stack

`docs/observability/` is a Prometheus and Grafana compose stack for manual testing and
for users. When `ZEBRAD_VERSION` changes, re-vendor Zebra's dashboards with
`docs/observability/sync-dashboards.sh`; see [GRAFANA.md](GRAFANA.md).
