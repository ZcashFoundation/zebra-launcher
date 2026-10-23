# Fixtures

Captured from `zebrad 7.0.0-rc.0` (git `ad6528b`, release build with
`default-release-binaries`) on Linux, 2026-10-23:

- `zebrad-7.0.0-regtest-start-shutdown.stdout.log`: `zebrad -c regtest.toml start`
  with RPC on `127.0.0.1:0` and metrics on `127.0.0.1:19999`, stopped with
  `SIGTERM` after 20 s. Exit code 0. Paths are scrubbed to `<DATA_DIR>`.
- `zebrad-7.0.0-config-error.stderr.log`: `[rpc] bogus = 1`. Exit code 1, empty stdout.
- `zebrad-7.0.0-missing-config.stderr.log`: `-c does-not-exist.toml`. Exit code 1.
- `zebrad-7.0.0-db-locked.stderr.log`: second instance on the same `state.cache_dir`.
  Panic, exit code 134 (`SIGABRT`, release builds use `panic = "abort"`).
- `zebrad-7.0.0-port-in-use.stderr.log`: metrics port already bound. Panic, exit 134.
- `zebrad-7.0.0-generate.toml`: `zebrad generate -o` output.

Re-capture when the pinned zebrad changes log wording; the parser tests pin
the exact lines the supervisor relies on.
