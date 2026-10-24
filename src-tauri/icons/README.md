# Application icons

Generated files; do not edit them by hand. The source is `source/zebra-icon.png`, the Zebra
icon from <https://zfnd.org/wp-content/uploads/2022/03/zebra-icon.png> (Zcash Foundation).

`scripts/make-icons.py` composes the 1024 px icon (the gear centred with a transparent
margin), runs `cargo tauri icon` for the PNG set, and writes `icon.ico` and `icon.icns` with
a simplified variant (no gear teeth, larger head) for the tiny renderings, where the full
gear would smear. Needs Pillow and the pinned tauri-cli:

```sh
python3 -P scripts/make-icons.py
```

`32x32.png` is also copied into the UI bundle for the header (see `ui/index.html`).
