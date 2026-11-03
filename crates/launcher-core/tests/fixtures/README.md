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

Captured from the same `zebrad 7.0.0-rc.0` (git `ad6528b`) on Linux, 2026-10-28:

- `zebrad-7.0.0-metrics.txt`: `GET /metrics` on the configured
  `metrics.endpoint_addr`, 20 s after `Opened RPC endpoint` on Regtest with every
  listener on loopback and the state in a temporary directory; exit code 0 on
  `SIGTERM` afterwards. 57 families: `# TYPE` lines only (no `# HELP`), gauges
  with labels (`zcash_net_peers{network="regtest"}`), a `NaN` gauge, and one
  `summary` (zebrad's exporter renders its histograms as summaries, so there
  are no `_bucket` samples). `fake-zebrad` serves this file on its metrics
  endpoint.

Captured from `zebrad 7.0.0` (git `e8db87e`, the release tarball
`zebrad-7.0.0-x86_64-unknown-linux-gnu.tar.gz`, its sha256 checked against the
release's `SHA256SUMS` by hand, without the cosign signature) on Linux,
2026-11-03, on Regtest with every listener on loopback and
`ZEBRA_TRACING__USE_COLOR=false`:

- `zebrad-7.0.0-lightwalletd-start-shutdown.stdout.log`: with
  `rpc.lightwalletd_listen_addr = "127.0.0.1:39067"`, stopped with `SIGTERM`
  after 10 s. Exit code 0, empty stderr.
- `zebrad-7.0.0-lightwalletd-port-in-use.stdout.log`: the same, with
  `127.0.0.1:39067` already bound by another process. zebrad logs `shutting
  down Zebra due to an error error=Address already in use (os error 98)` at
  ERROR on stdout and exits 1; no panic, empty stderr.

Re-capture when the pinned zebrad changes log wording; the parser tests pin
the exact lines the supervisor relies on.
