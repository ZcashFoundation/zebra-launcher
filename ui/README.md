# Zebra Launcher UI

The launcher's user interface: a [Leptos](https://leptos.dev) 0.8 app, client-side rendered only
(no SSR, no server functions), compiled to WebAssembly by [trunk](https://trunkrs.dev). There is no
Node, npm or pnpm anywhere in the build.

## Prerequisites

```sh
cargo install --locked trunk   # 0.22.x
```

The `wasm32-unknown-unknown` target comes from `rust-toolchain.toml`. trunk downloads the
`wasm-bindgen` CLI matching the `wasm-bindgen` version in `Cargo.lock` (`[tools] wasm_bindgen` in
`Trunk.toml` is deliberately unpinned; the crate and the CLI must match exactly) and `wasm-opt` for
release builds.

## Develop in a browser (mock mode)

```sh
cd ui
trunk serve        # http://localhost:1420, rebuilds and reloads on save
```

Outside Tauri there is no `window.__TAURI__`, so `api.rs` routes every command to `mock.rs`, a fake
backend that makes every page demoable:

- zebrad "starts" on load (Starting, then Running) and Start / Stop / Restart change the phase
  after short delays;
- the Logs page receives batches of real zebrad 7.0.0 lines (from
  `crates/launcher-core/tests/fixtures`), and now and then a `Launcher` line "7 lines dropped"
  stands in for skipped lines, as the Tauri bridge sends when the UI falls behind; the page shows
  it as the "… N lines dropped" marker;
- node stats tick while Running, with an end-of-support date 41 days away so the EOS banner
  shows; `?eos=soon` moves it to 10 days (the banner then offers "Check for updates"), and
  `?eos=halt` makes zebrad halt at its end of support (Failed, with the halt banner);
- Configure: a config containing `bogus` is "rejected" and rolled back with zebrad's real error
  text; a config containing the line `# simulate-conflict` gets the "file changed on disk" error;
  one containing `# simulate-read-failure` makes the reload after Save fail (add `bogus` too to
  see a rollback followed by a failed reload). "Reset to managed defaults…" works once the node is
  stopped and replaces the text with the launcher's minimal file for the network and data folder in
  the settings. `?configfail` makes the first read of the config fail, as for an unreadable file; Reset stays available then;
- open `http://localhost:1420/?stale` to simulate a zebrad left behind by an earlier launcher run:
  the stale-process banner shows, Start fails, and "Stop it and start" clears it;
- open `http://localhost:1420/?firstrun` for the first-run wizard: zebrad stays stopped until
  Finish, the default data folder has 212.4 GB free (so the low-space warning shows), "Choose
  another folder…" always picks `/mnt/storage/zebra` (1,834.5 GB free), and alpha.4 leftovers
  (a `zebrad.toml` and 96.7 GB of state, counted as a lower bound) are offered for moving or
  deleting (type `delete` to confirm).
  Combine both flags (`?firstrun&stale`) to see Finish report that zebrad did not start.
  `?firstrun&storeddir` starts with a data folder chosen in an earlier, unfinished first run
  (`/media/small-disk/zebra`, 120.0 GB free, so the low-space warning shows; the free space is
  read with the read-only `free_space`, which answers after 1.5 s while Next stays enabled), and
  `?firstrun&running` has zebrad already running with Start failing, which Finish treats as
  success;
- Settings are kept in memory; set the Grafana URL to see the iframe (it stays blank unless Grafana
  runs on that address). Without `?firstrun` the first run counts as done. "Change…" for the data
  folder and "Delete node data…" work once the node is stopped (they refuse with "stop zebrad
  first" otherwise, like the backend); the mock state is 248.3 GB. "Copy diagnostics" shows the
  text below the button when the browser denies clipboard access, as headless browsers do;
- launcher updates: the mock runs version 0.1.0 with updates Off, and "Check now" (Settings) or
  "Check for updates" (dashboard) finds nothing. `?update=…` starts in a state, with the mode
  that goes with it, and a check then finds 0.2.0 (84 MB) unless noted:
  - `available`: Notify mode found 0.2.0; Install downloads it, installs, and "restarts" as
    0.2.0;
  - `downloading`: Auto mode is downloading it in the background, then waits, ready;
  - `ready`: downloaded and verified, waiting for Install;
  - `countdown`: Auto mode installs it after a 60 s countdown; Cancel stops it;
  - `error`: the last check failed, and every check fails;
  - `health`: the last update did not come back healthy (`UpdateInfo.health_failure`, and an
    `update_health_failed` event 1.5 s after load); a check finds nothing;

  add `&deb` for an installation that cannot update itself (the `.deb`: "Open releases page"
  instead of Install) and `&stalecheck` for the stale-check warning (no successful check for
  20 days). Saving the Off mode returns the updater to Idle, like the backend.

The flags are read from the query string once, when the page loads.

## Build

```sh
cd ui
trunk build --release   # writes ui/dist
```

Release builds use the `release-wasm` profile from the root `Cargo.toml` (size over speed),
selected by `data-cargo-profile-release` in `index.html`, then `wasm-opt -Oz`.

## How Tauri embeds it

The Tauri app (`src-tauri`) points at this crate:

```json
"build": {
  "beforeDevCommand": { "script": "trunk serve", "cwd": "../ui" },
  "devUrl": "http://localhost:1420",
  "beforeBuildCommand": { "script": "trunk build --release", "cwd": "../ui" },
  "frontendDist": "../ui/dist"
},
"app": { "withGlobalTauri": true }
```

`withGlobalTauri` is required: `src/tauri.rs` binds by hand (no `tauri-sys`) to
`window.__TAURI__.core.invoke`, `window.__TAURI__.event.listen` and `window.__TAURI__.core.Channel`.
The CSP must allow `'wasm-unsafe-eval'` in `script-src`, and `frame-src` must list the loopback
origins the shared Grafana URL rule accepts (`http://127.0.0.1:* http://localhost:*`, see
`launcher_types::is_loopback_http_url` and `docs/GRAFANA.md`). trunk puts a
small inline module script in `index.html` to load the wasm; Tauri adds hashes for inline scripts
to the CSP when it serves the bundled assets.

## Layout

| File | What |
| --- | --- |
| `src/main.rs` | Entry point: mounts the app in the browser; the native build only prints a hint |
| `src/tauri.rs` | `#[wasm_bindgen]` externs to `window.__TAURI__`, JSON-compatible (de)serialization |
| `src/api.rs` | One typed function per backend command or event, Tauri or mock |
| `src/mock.rs` | The fake backend for `trunk serve`; its values go through the same JSON round trip as Tauri payloads |
| `src/app.rs` | Shared state (`AppState`), backend subscriptions, the shell: header, the five tabs and the first-run wizard |
| `src/theme.rs` | Light and dark appearance: resolves the setting against the system preference and applies it to the document |
| `src/clock.rs` | The current time from JavaScript's `Date` (`SystemTime::now()` panics on wasm) |
| `src/clipboard.rs` | Copying text to the system clipboard |
| `src/format.rs` | Display helpers: numbers, durations, relative times, exit classes. Pure, tested natively |
| `src/log_view.rs` | The Logs page's data model: a capped, de-duplicated buffer in `seq` order, dropped-line markers, level and text filters. Pure, tested natively |
| `src/insights.rs`, `src/insights/fields.rs` | Log analysis: sync stage, problems, startup milestones and activity counts folded from the log stream; tolerant readers for the fields of a zebrad line. Pure, tested natively |
| `src/stats_history.rs` | The `NodeStats` samples since the launcher opened, for the dashboard's charts and rates. Pure, tested natively |
| `src/charts.rs`, `src/charts/line_chart.rs`, `src/charts/widgets.rs` | Inline SVG line charts (crosshair, keyboard navigation, table view), stat tiles with sparklines, the sync meter and the range control; the geometry is pure and tested natively |
| `src/grafana_url.rs` | Builds the URL the Grafana tab embeds from the saved Grafana URL, the picked dashboard, time range, refresh and theme. Pure, tested natively |
| `src/config_form.rs`, `src/config_form/schema.rs` | zebrad's configuration as a form: the schema of every key the bundled zebrad accepts, values read from `zebrad.toml` with `toml_edit`, validation, and edits written back into the same document. Pure, tested natively; a test pins the schema to the `zebrad generate` fixture |
| `src/config_view.rs` | Helpers for the Configure page's file pane: changed lines, where a key is written, how far to scroll. Pure, tested natively |
| `src/components/` | `nav.rs` (header and tabs), `status_strip.rs` (phase, progress, Start/Stop/Restart), `stale_banner.rs`, `updates.rs` (update banner, buttons, "Check for updates", recovery panel), `update_mode_choice.rs` (radio buttons shared by the wizard and Settings), `config_form.rs` (the two-view configuration editor), `log_insights.rs` (analysis cards), `reset_config.rs`, `tooltip.rs` |
| `src/pages/` | The five tabs (`dashboard.rs`, `logs.rs`, `grafana.rs`, `configure.rs`, `settings.rs`) and the first-run wizard (`first_run.rs`) |

The crate also compiles natively so `cargo clippy --workspace --all-targets` and
`cargo test --workspace` cover it; the native binary only prints a hint. Check the wasm build with
`cargo clippy -p ui --target wasm32-unknown-unknown`.
