#!/usr/bin/env bash
# Installs fake-zebrad as all three channel sidecars for the host, so
# `cargo tauri dev` and `cargo tauri build` work without downloading or building
# the real zebrad. Never ship a bundle built this way. Windows:
# scripts/dev-sidecar.ps1.
set -euo pipefail

die() {
    echo "dev-sidecar: error: $*" >&2
    exit 1
}

# shellcheck source-path=SCRIPTDIR source=lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
host="$(host_triple)"

# Take the binary's path from cargo, which knows about build.target-dir,
# CARGO_TARGET_DIR and CARGO_BUILD_TARGET. Diagnostics still go to stderr.
messages="$(cargo build --manifest-path "${root}/Cargo.toml" -p launcher-core --bin fake-zebrad \
    --message-format=json-render-diagnostics)"
built="$(
    { printf '%s\n' "${messages}" | grep '"reason":"compiler-artifact"' | grep '"name":"fake-zebrad"' |
        grep -o '"executable":"[^"]*"' | tail -n 1; } || true
)"
built="${built#\"executable\":\"}"
built="${built%\"}"
[ -n "${built}" ] || die "cargo did not report where it built fake-zebrad"
for channel in stable beta nightly; do
    suffix=""
    [ "${channel}" = stable ] || suffix="-${channel}"
    out="${root}/src-tauri/binaries/zeal-zebrad${suffix}-${host}"
    install_atomic "${built}" "${out}"
    echo "installed ${channel} fake-zebrad ($("${out}" --version)) at ${out}"
done
