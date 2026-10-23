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
| Logs (`launcher.log`, `zebrad.log`) | TODO(M5) |

| OS | `app_local_data_dir` / `app_config_dir` |
| --- | --- |
| Linux | `~/.local/share/org.zfnd.zebra-launcher` / `~/.config/org.zfnd.zebra-launcher` |
| macOS | `~/Library/Application Support/org.zfnd.zebra-launcher` (both) |
| Windows | `%LOCALAPPDATA%\org.zfnd.zebra-launcher` / `%APPDATA%\org.zfnd.zebra-launcher` |

TODO(M5): confirm these against Tauri's path resolver on each OS.

To delete only the chain state, use **Settings → Delete node data** (M8). A data dir you
chose yourself is never removed automatically.

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

The new launcher's first run (M8) offers to move that state into its data dir, delete it,
or leave it. To remove it by hand, delete the old app and that cache directory, but only
if no terminal zebrad uses it: a zebrad you run yourself uses the same default directory.
TODO(M8): confirm alpha.4 paths on each OS.
