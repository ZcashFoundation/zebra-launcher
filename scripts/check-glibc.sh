#!/usr/bin/env bash
# Fails if any of the given ELF binaries needs a glibc symbol version newer
# than the floor (default 2.35, the glibc of Ubuntu 22.04; Debian 12 has 2.36). Bundles
# built in the ubuntu:22.04 container meet it; a binary linked on a newer
# system would not start on Ubuntu 22.04.
#
# Requires objdump (binutils).
#
# Usage: scripts/check-glibc.sh [--floor X.Y] BINARY...

set -euo pipefail

floor="2.35"
if [ "${1:-}" = "--floor" ]; then
    [ $# -ge 2 ] || { echo "check-glibc: --floor needs a value" >&2; exit 1; }
    floor="$2"
    shift 2
fi
[ $# -gt 0 ] || { echo "usage: check-glibc.sh [--floor X.Y] BINARY..." >&2; exit 1; }

status=0
for binary in "$@"; do
    if [ ! -f "${binary}" ]; then
        echo "::error::${binary} does not exist"
        status=1
        continue
    fi
    highest="$(objdump -T "${binary}" \
        | grep -oE 'GLIBC_[0-9]+(\.[0-9]+)+' \
        | sed 's/^GLIBC_//' \
        | sort -uV \
        | tail -n 1 || true)"
    if [ -z "${highest}" ]; then
        echo "${binary}: no versioned glibc symbols (static or not dynamically linked)"
        continue
    fi
    newest="$(printf '%s\n%s\n' "${highest}" "${floor}" | sort -V | tail -n 1)"
    if [ "${newest}" != "${floor}" ]; then
        echo "::error::${binary} needs GLIBC_${highest}, above the ${floor} floor"
        objdump -T "${binary}" | grep -E "GLIBC_${highest//./\\.}\b" | head -n 20
        status=1
    else
        echo "${binary}: highest glibc symbol version ${highest} (floor ${floor})"
    fi
done
exit "${status}"
