# Uninstall

> Skeleton. Paths are confirmed per OS once the app identifier and bundles ship
> (M5, M9); entries marked TODO depend on that.

Removing the app does not remove the chain state unless you ask for it. Mainnet state is
hundreds of GB.

## Where things live

The app identifier is `org.zfnd.zebra-launcher`.

| What | Location |
| --- | --- |
| Chain state (default) | `<app_local_data_dir>/state`, or the data dir you chose at first run |
| RPC cookie | `<app_local_data_dir>/rpc` |
| `zebrad.toml`, `launcher.json` | `<app_config_dir>` |
| Logs (`launcher.<date>.log`, `zebrad.log`) | `<app_log_dir>` |

| OS | `app_local_data_dir` / `app_config_dir` |
| --- | --- |
| Linux | `~/.local/share/org.zfnd.zebra-launcher` / `~/.config/org.zfnd.zebra-launcher` |
| macOS | `~/Library/Application Support/org.zfnd.zebra-launcher` (both) |
| Windows | `%LOCALAPPDATA%\org.zfnd.zebra-launcher` / `%APPDATA%\org.zfnd.zebra-launcher` |

`<app_log_dir>` is `~/.local/share/org.zfnd.zebra-launcher/logs` on Linux,
`~/Library/Logs/org.zfnd.zebra-launcher` on macOS and
`%LOCALAPPDATA%\org.zfnd.zebra-launcher\logs` on Windows.

TODO(M9): confirm these against Tauri's path resolver on each OS with installed bundles.

### Where the chain state lives

| Data dir | Chain state (`state.cache_dir`) |
| --- | --- |
| Default | `<app_local_data_dir>/state`, for example `~/.local/share/org.zfnd.zebra-launcher/state` (Linux), `~/Library/Application Support/org.zfnd.zebra-launcher/state` (macOS), `%LOCALAPPDATA%\org.zfnd.zebra-launcher\state` (Windows) |
| Chosen at first run or in Settings | `<chosen dir>/state` |

zebrad creates `state/v<N>/<network>` below that. Changing the data dir does not move
existing state; the old directory stays until you delete it.

### Delete node data

**Settings → Delete node data…** (only while zebrad is stopped or failed, after you type
`delete` to confirm) removes the current chain state directory, `<data dir>/state`, and
nothing else: `zebrad.toml`, `launcher.json` (the launcher settings), the logs and the RPC
cookie stay, and the next start syncs from scratch. It shows how much space was freed. It refuses to delete anything that is not a `state` directory inside
the launcher's data dir or the data dir chosen in the settings. A data dir you chose
yourself is never removed by an uninstaller; delete it by hand if you no longer want it.

## Linux

- **AppImage:** delete the `.AppImage` file. TODO(M5): the extracted zebrad sidecar in the
  app data dir.
- **deb:** `sudo apt remove zebra-launcher`. TODO(M9): confirm the package name.

Then remove the directories above to delete state, config and logs.

## macOS

Quit the launcher (tray → Quit), then move "Zebra Launcher" from Applications to the
Trash. Remove `~/Library/Application Support/org.zfnd.zebra-launcher` to delete state,
config and logs. TODO(M9): any caches or saved state.

## Windows

Settings → Apps → Installed apps → Zebra Launcher → Uninstall. The NSIS uninstaller
offers to delete app data, which covers the default data dir. A data dir you chose
yourself stays. TODO(M9): confirm the uninstaller's wording.

## Leftovers from v0.0.0-alpha.4

The old prototype kept its files in different places:

- `zebrad.toml` next to the app (inside the `zebra.app` bundle or install folder).
- Chain state in zebrad's default cache dir: `~/.cache/zebra` (Linux),
  `~/Library/Caches/zebra` (macOS), `%LOCALAPPDATA%\zebra` (Windows).

The new launcher's first run detects a `zebrad.toml` next to its own executable and a
`state` directory in that cache dir, shows the state's size, and offers to move the
state into its own state directory ("Move it here", after you confirm that the old
launcher is not running) or to leave it ("Leave it"). The move is a rename, so it only works
when both are on the same drive; otherwise the launcher asks you to move the folder by
hand and never copies it itself. It runs only while the launcher's zebrad is stopped.

To remove the leftovers by hand, delete the old app and that cache directory, but only
if no terminal zebrad uses it: a zebrad you run yourself uses the same default directory,
and moving it into the launcher takes it away from that zebrad.
TODO(M9): confirm alpha.4 paths on each OS against a real alpha.4 install.
