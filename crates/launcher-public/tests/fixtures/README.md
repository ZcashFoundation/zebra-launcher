# Fixtures

- `zebrad-7.0.0-regtest-lightdinfo.hex`: the `LightdInfo` message zebrad 7.0.0
  (`e8db87e`, the release tarball) returned to `GetLightdInfo` on Regtest at
  height 0, as hex, captured by `tests/real_light_server.rs` on 2026-11-03. The
  version fields carry zebrad's build metadata (`v7.0.0+git.e8db87e`), Regtest
  reports the `test` chain, and a chain before any upgrade has branch id
  `00000000`. `taddrSupport` (field 3) is set and the build fields are absent.

Re-capture when the pinned zebrad changes: the contract test prints the bytes.
