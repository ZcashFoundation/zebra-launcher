# Zeal UI

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
backend that makes every page demoable. Its node-scoped API is the backend's (`mock/nodes.rs`):
every node command and event carries a node's id, and the mock's zebrad below is the first node.

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
  text below the button when the browser denies clipboard access, as headless browsers do.
  Settings → Nodes → "Add a node…" opens the add-node dialog, and once a second node is added
  the window switches to the sidebar;
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

- the dashboard's Resources card: zebrad's process tree is sampled every 5 s while it has a pid
  (Starting, Running and Stopping). It opens its database for the first 10 s of every run (1.1
  cores, 180 MB/s read), then syncs at about 5.6 cores with 35 MB/s read and 95 MB/s written
  while its memory climbs from 2.4 to 6.4 GiB over 30 minutes; once the node reaches the tip
  (after about 47 minutes in the mock) it settles at 0.25 cores and 3.2 GiB. `?stalled` and
  `?nopeers` keep it at 0.1 cores, and `?crashloop` restarts the counters with every run. Stop
  the node to see the card's last-run line. The computer has 8 CPUs and 16 GiB, of which other
  programs use 0.6 to 0.9 cores and 5.2 GiB. `?os=macos` and `?os=windows` show those
  platforms' figures and labels (memory pressure on macOS; commit, handles and no thread
  counts on Windows); Linux is the default. `?children` gives zebrad a `block_notify_command`
  that runs in cycles of three samples: an `sh` with `notify-block` under it, indented under
  their parents; then 40 `curl`s as well, more than a sample lists, so the table ends with
  "and 11 more"; then `notify-block` alone, orphaned, on a line of its own. `?unreadable`
  reports CPU and I/O as refused by the OS: the CPU tile reads "not available" and the Disk
  tile "—" with "the system does not report them". `?readerpanic` makes the resource reader
  panic 30 s into the first run, as the backend's can: the samples stop while zebrad keeps
  running, and the card says it is not measured until zebrad's next run, which Restart
  starts;
- several nodes: `?herd` adds five nodes beside the mock's zebrad: on Mainnet, Zakura catching
  up from a snapshot, Zaino reading the Mainnet Zebra, and the zcashd compatibility build,
  stopped; on Testnet, a Zebra and a pruned Zakura at the tip. The window shows the sidebar
  with its Mainnet and Testnet tabs, the overview with a card per node of the chosen network,
  each node's pages and the add-node dialog. The dialog offers Zebra on either network (the
  other kinds show as coming later), and its Add creates one more simulated zebrad node, with
  its own folder, ports and `zebrad.toml`, under the backend's rules: at most eight nodes, and
  a name, ports and data folder no other node has. Its data folder shows the free space there
  (212.4 GB in the launcher's data folder, with the low-space warning on Mainnet), and its
  Change… picks `/mnt/storage/zebra` (1,834.5 GB free). A node's Node settings tab renames it,
  turns its start at launch on or off and removes it, keeping or deleting its data (the last
  node cannot be removed). The mock's own zebrad, the first node, can be removed too, after
  which a Testnet node may be first while the window stays on the network shown; with
  `?stale` its removal is refused while the zebrad from an earlier run is still there.
  `?herd&leftover` makes a removal that deletes the data leave the node's folder (or the first
  node's chain state) behind, as when the backend cannot delete a file: the node leaves the
  list and the error banner names what stayed. Every node has a Resources card of its own,
  sampled every 5 s while it has a pid: Zebra · Testnet uses 0.15 cores and 1.9 GiB, the
  pruned Zakura · Testnet 0.1 cores and 1.2 GiB, Zakura · Mainnet 3.2 cores and 4.4 GiB while
  it catches up, Zaino 0.3 cores and 650 MiB, and zcashd, stopped, has no card until it is
  started (0.2 cores, 2.4 GiB); a zebrad the dialog adds uses what the mock's zebrad does at
  the tip. With `?herd` the computer has 16 CPUs and 32 GiB, and its figures count every
  node. The real backend has the same API and rules; there every node is a zebrad for now.

The flags are read from the query string once, when the page loads; the table at the top of
[`src/mock.rs`](src/mock.rs) lists every one.

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
| `src/api.rs` | One typed function per backend command or event, Tauri or mock; the node commands and events carry the node's id |
| `src/mock.rs`, `src/mock/` | The fake backend for `trunk serve`; its values go through the same JSON round trip as Tauri payloads. `nodes.rs` is the node-scoped API and the node list's rules, `herd.rs` the light simulation of every node but the first, and `resources.rs` the resource samples, pure and tested natively |
| `src/app.rs` | Shared state (`AppState`: the node list from `list_nodes` and the `nodes` event, matched by id, and the selected node), backend subscriptions, the shell: header, the tab bar for one node or the sidebar for several, and the first-run wizard |
| `src/node_state.rs` | One node's state: what the backend reports about it and the histories and log analysis the UI keeps for it |
| `src/node_summary.rs` | A node's state in a few words, for the sidebar and the overview. Pure, tested natively |
| `src/theme.rs` | Light and dark appearance: resolves the setting against the system preference and applies it to the document |
| `src/clock.rs` | The current time from JavaScript's `Date` (`SystemTime::now()` panics on wasm) |
| `src/clipboard.rs` | Copying text to the system clipboard |
| `src/format.rs` | Display helpers: numbers, durations, relative times, exit classes. Pure, tested natively |
| `src/log_view.rs` | The Logs page's data model: a capped, de-duplicated buffer in `seq` order, dropped-line markers, level and text filters. Pure, tested natively |
| `src/insights.rs`, `src/insights/fields.rs` | Log analysis: sync stage, problems, startup milestones and activity counts folded from the log stream; tolerant readers for the fields of a zebrad line. Pure, tested natively |
| `src/stats_history.rs` | The `NodeStats` samples since the launcher opened, for the dashboard's charts and rates. Pure, tested natively |
| `src/resource_history.rs` | The samples of the node's process tree since the launcher opened, for the Resources card's one-minute rates, sparklines and last-run line. Pure, tested natively |
| `src/charts.rs`, `src/charts/line_chart.rs`, `src/charts/widgets.rs` | Inline SVG line charts (crosshair, keyboard navigation, table view), stat tiles with sparklines, the sync meter and the range control; the geometry is pure and tested natively |
| `src/grafana_url.rs` | Builds the URL the Grafana tab embeds from the saved Grafana URL, the picked dashboard, time range, refresh and theme. Pure, tested natively |
| `src/config_form.rs`, `src/config_form/schema.rs` | zebrad's configuration as a form: the schema of every key the bundled zebrad accepts, values read from `zebrad.toml` with `toml_edit`, validation, and edits written back into the same document. Pure, tested natively; a test pins the schema to the `zebrad generate` fixture |
| `src/config_view.rs` | Helpers for the Configure page's file pane: changed lines, where a key is written, how far to scroll. Pure, tested natively |
| `src/components/` | `nav.rs` (header and tabs), `sidebar.rs` and `herd_header.rs` (the shell for several nodes), `add_node.rs` (the add-node dialog), `status_strip.rs` (phase, progress, Start/Stop/Restart), `stale_banner.rs`, `updates.rs` (update banner, buttons, "Check for updates", recovery panel), `update_mode_choice.rs` (radio buttons shared by the wizard and Settings), `config_form.rs` (the two-view configuration editor), `log_insights.rs` (analysis cards), `reset_config.rs`, `tooltip.rs` |
| `src/pages/` | The pages (`overview.rs` with several nodes, `dashboard.rs`, `logs.rs`, `grafana.rs`, `configure.rs`, `node_settings.rs` with several nodes, `settings.rs`) and the first-run wizard (`first_run.rs`) |

The crate also compiles natively so `cargo clippy --workspace --all-targets` and
`cargo test --workspace` cover it; the native binary only prints a hint. Check the wasm build with
`cargo clippy -p ui --target wasm32-unknown-unknown`. CI runs that and `trunk build --release` on
every pull request (`.github/workflows/ci.yml`), so a dependency or an API that does not exist on
wasm fails there, not in the bundle build after the merge.
