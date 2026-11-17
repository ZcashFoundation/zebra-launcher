#!/usr/bin/env bash
# Installs fake-zebrad as all three channel sidecars for the host, and on a
# Linux host fake-zingod as zeal-zingod, so `cargo tauri dev` and
# `cargo tauri build` work without downloading or building the real zebrad
# and zingod. Never ship a bundle built this way. Windows:
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

# zingod is bundled on Linux only (tauri.linux.conf.json).
zingod=false
case "${host}" in *-linux-*) zingod=true ;; esac
bin_args=(--bin fake-zebrad)
[ "${zingod}" = false ] || bin_args+=(--bin fake-zingod)

# Take the binaries' paths from cargo, which knows about build.target-dir,
# CARGO_TARGET_DIR and CARGO_BUILD_TARGET. Diagnostics still go to stderr.
messages="$(cargo build --manifest-path "${root}/Cargo.toml" -p launcher-core "${bin_args[@]}" \
    --message-format=json-render-diagnostics)"

# built NAME: prints where cargo built the binary NAME.
built() {
    local path
    path="$(
        { printf '%s\n' "${messages}" | grep '"reason":"compiler-artifact"' | grep "\"name\":\"$1\"" |
            grep -o '"executable":"[^"]*"' | tail -n 1; } || true
    )"
    path="${path#\"executable\":\"}"
    path="${path%\"}"
    [ -n "${path}" ] || die "cargo did not report where it built $1"
    printf '%s\n' "${path}"
}

fake_zebrad="$(built fake-zebrad)"
for channel in stable beta nightly; do
    suffix=""
    [ "${channel}" = stable ] || suffix="-${channel}"
    out="${root}/src-tauri/binaries/zeal-zebrad${suffix}-${host}"
    install_atomic "${fake_zebrad}" "${out}"
    echo "installed ${channel} fake-zebrad ($("${out}" --version)) at ${out}"
done

if [ "${zingod}" = true ]; then
    out="${root}/src-tauri/binaries/zeal-zingod-${host}"
    install_atomic "$(built fake-zingod)" "${out}"
    echo "installed fake-zingod ($("${out}" --version)) at ${out}"
fi
