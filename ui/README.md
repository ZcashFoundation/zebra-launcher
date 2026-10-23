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
- node stats tick while Running, with an end-of-support date so the EOS banner shows;
- Configure: a config containing `bogus` is "rejected" and rolled back with zebrad's real error
  text; a config containing the line `# simulate-conflict` gets the "file changed on disk" error;
  one containing `# simulate-read-failure` makes the reload after Save fail (add `bogus` too to
  see a rollback followed by a failed reload);
- open `http://localhost:1420/?stale` to simulate a zebrad left behind by an earlier launcher run:
  the stale-process banner shows, Start fails, and "Stop it and start" clears it;
- open `http://localhost:1420/?firstrun` for the first-run wizard: zebrad stays stopped until
  Finish, the default data folder has 212.4 GB free (so the low-space warning shows), "Choose
  another folder…" always picks `/mnt/storage/zebra` (1,834.5 GB free), and alpha.4 leftovers
  (a `zebrad.toml` and 96.7 GB of state, counted as a lower bound) are offered for moving.
  Combine both flags (`?firstrun&stale`) to see Finish report that zebrad did not start;
- Settings are kept in memory; set the Grafana URL to see the iframe (it stays blank unless Grafana
  runs on that address). Without `?firstrun` the first run counts as done. "Change…" for the data
  folder and "Delete node data…" work once the node is stopped (they refuse with "stop zebrad
  first" otherwise, like the backend); the mock state is 248.3 GB. "Copy diagnostics" shows the
  text below the button when the browser denies clipboard access, as headless browsers do.

The flags are read from the query string once, when the page loads.

## Build

```sh
cd ui
trunk build --release   # writes ui/dist
```

Release builds use the `release-wasm` profile from the root `Cargo.toml` (size over speed),
selected by `data-cargo-profile-release` in `index.html`, then `wasm-opt -Oz`.

## How Tauri embeds it

The Tauri app (`src-tauri`, rewritten in M5) points at this crate:

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
| `src/tauri.rs` | `#[wasm_bindgen]` externs to `window.__TAURI__`, JSON-compatible (de)serialization |
| `src/api.rs` | One typed function per backend command or event, Tauri or mock |
| `src/mock.rs` | The fake backend for `trunk serve`; its values go through the same JSON round trip as Tauri payloads |
| `src/log_view.rs`, `src/format.rs` | Pure logic (log buffer, gaps, filters, formatting), unit-tested natively |
| `src/app.rs` | Shared state, subscriptions, the shell |
| `src/components/`, `src/pages/` | The status strip, tabs, the four pages and the first-run wizard (`pages/first_run.rs`) |

The crate also compiles natively so `cargo clippy --workspace --all-targets` and
`cargo test --workspace` cover it; the native binary only prints a hint. Check the wasm build with
`cargo clippy -p ui --target wasm32-unknown-unknown`.
