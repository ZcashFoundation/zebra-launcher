# Uninstall

Removing the app does not remove the chain state unless you ask for it. Mainnet state is
hundreds of GB.

## Where things live

The app identifier is `org.zfnd.zeal`.

| What | Location |
| --- | --- |
| Chain state (default) | `<app_local_data_dir>/state`, or the data dir you chose at first run |
| RPC cookie | `<app_local_data_dir>/rpc` |
| `zebrad.toml`, `launcher.json` | `<app_config_dir>` |
| Logs (`launcher.<date>.log`, `zebrad.log`) | `<app_log_dir>` |
| Each node added later: its `zebrad.toml`, cookie, `zebrad.log` and default chain state | `<app_local_data_dir>/nodes/<id>` |

| OS | `app_local_data_dir` / `app_config_dir` |
| --- | --- |
| Linux | `~/.local/share/org.zfnd.zeal` / `~/.config/org.zfnd.zeal` |
| macOS | `~/Library/Application Support/org.zfnd.zeal` (both) |
| Windows | `%LOCALAPPDATA%\org.zfnd.zeal` / `%APPDATA%\org.zfnd.zeal` |

`<app_log_dir>` is `~/.local/share/org.zfnd.zeal/logs` on Linux,
`~/Library/Logs/org.zfnd.zeal` on macOS and
`%LOCALAPPDATA%\org.zfnd.zeal\logs` on Windows.

These are Tauri's `app_config_dir`, `app_local_data_dir` and `app_log_dir` for the
identifier (`src-tauri/src/paths.rs`), so they follow Tauri's path resolver on each OS.

### Where the chain state lives

| Data dir | Chain state (`state.cache_dir`) |
| --- | --- |
| Default | `<app_local_data_dir>/state`, for example `~/.local/share/org.zfnd.zeal/state` (Linux), `~/Library/Application Support/org.zfnd.zeal/state` (macOS), `%LOCALAPPDATA%\org.zfnd.zeal\state` (Windows) |
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

With several nodes, a node's **Node settings → Remove** asks whether to keep its data or
delete it. Deleting removes its chain state with the same rules and then, for a node added
later, the rest of its `nodes/<id>` folder; the first node's `zebrad.toml` and logs stay.
Kept data stays where it is; delete it by hand if you no longer want it.

## Linux

- **AppImage:** delete the `.AppImage` file, and the copy of zebrad the launcher keeps
  outside the AppImage mount:
  `~/.local/share/org.zfnd.zeal/sidecar/<launcher version>/zeal-zebrad`
  (the launcher deletes other versions' copies, never the current one).
- **deb:** `sudo apt remove zeal-app` (the package installs `/usr/bin/zeal-app`
  and `/usr/bin/zeal-zebrad`).

Then remove the directories above to delete state, config and logs.

## macOS

Quit the launcher (tray → Quit), then move "Zeal" from Applications to the
Trash. Remove `~/Library/Application Support/org.zfnd.zeal` (state and config)
and `~/Library/Logs/org.zfnd.zeal` (logs). Whether macOS keeps a webview cache
for the identifier elsewhere has not been checked on an installed bundle.

## Windows

Settings → Apps → Installed apps → Zeal → Uninstall. The NSIS uninstaller
offers to delete app data, which covers the default data dir. A data dir you chose
yourself stays. The uninstaller's exact wording has not been checked on an installed
bundle.

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
and moving it into the launcher takes it away from that zebrad. These alpha.4 paths are
what the old prototype's code used; they have not been checked against a real alpha.4
install on each OS.
