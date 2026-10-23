#!/usr/bin/env bash
# Installs fake-zebrad as the sidecar for the host, so `cargo tauri dev` and
# `cargo tauri build` work without downloading or building the real zebrad.
# Never ship a bundle built this way. Windows: scripts/dev-sidecar.ps1.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
host="$(rustc -vV | sed -n 's/^host: //p')"
out="${root}/src-tauri/binaries/zebra-launcher-zebrad-${host}"

cargo build --manifest-path "${root}/Cargo.toml" -p launcher-core --bin fake-zebrad
mkdir -p "$(dirname "${out}")"
cp "${CARGO_TARGET_DIR:-${root}/target}/debug/fake-zebrad" "${out}"
chmod +x "${out}"
echo "installed fake-zebrad ($("${out}" --version)) at ${out}"
