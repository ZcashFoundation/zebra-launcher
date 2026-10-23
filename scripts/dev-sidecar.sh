#!/usr/bin/env bash
# Installs fake-zebrad as the sidecar for the host, so `cargo tauri dev` and
# `cargo tauri build` work without downloading or building the real zebrad.
# Never ship a bundle built this way. Windows: scripts/dev-sidecar.ps1.
set -euo pipefail

die() {
    echo "dev-sidecar: error: $*" >&2
    exit 1
}

# shellcheck source-path=SCRIPTDIR source=lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
host="$(host_triple)"
out="${root}/src-tauri/binaries/zebra-launcher-zebrad-${host}"

cargo build --manifest-path "${root}/Cargo.toml" -p launcher-core --bin fake-zebrad
install_atomic "${CARGO_TARGET_DIR:-${root}/target}/debug/fake-zebrad" "${out}"
echo "installed fake-zebrad ($("${out}" --version)) at ${out}"
