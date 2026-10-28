#!/usr/bin/env bash
# Prints the version of one package in a Cargo.lock, e.g. the `wasm-bindgen`
# version that trunk requires its wasm-bindgen CLI to match exactly. Fails if
# the package is missing or locked at more than one version.
#
# Plain sed, so it runs in a bare ubuntu:22.04 container, on macOS and in Git
# Bash on Windows.
#
# Usage: scripts/cargo-lock-version.sh NAME [CARGO_LOCK]
#   CARGO_LOCK defaults to this repository's Cargo.lock.

set -euo pipefail

die() {
    echo "cargo-lock-version: error: $*" >&2
    exit 1
}

[ $# -ge 1 ] && [ $# -le 2 ] || die "usage: cargo-lock-version.sh NAME [CARGO_LOCK]"
name="$1"
lock="${2:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/Cargo.lock}"
[ -f "${lock}" ] || die "${lock} does not exist"

# A [[package]] entry is `name = "..."` followed directly by `version = "..."`.
# CRs are dropped in case a Windows checkout converted line endings.
versions="$(tr -d '\r' < "${lock}" | sed -n "/^name = \"${name}\"\$/{n;s/^version = \"\\([^\"]*\\)\"\$/\\1/p;}" | sort -u)"
[ -n "${versions}" ] || die "${lock} has no package ${name}"
[ "$(printf '%s\n' "${versions}" | wc -l)" -eq 1 ] \
    || die "${lock} has more than one version of ${name}: $(printf '%s\n' "${versions}" | tr '\n' ' ')"
printf '%s\n' "${versions}"
