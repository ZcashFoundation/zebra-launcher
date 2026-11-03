#!/usr/bin/env bash
# Sets the bundled zebrad to a new release: writes ZEBRAD_VERSION and
# ZEBRAD_COMMIT, then runs scripts/check-versions.sh against the result. The
# two must change together (docs/SIDECARS.md, "Bumping ZEBRAD_VERSION"); the
# fake zebrad reads ZEBRAD_VERSION at build time. zebrad-watch.yml runs this
# script; it does not fetch, build or verify zebrad.
#
# Requires: bash.
#
# Usage: scripts/bump-zebrad.sh [--root DIR] VERSION COMMIT
#   VERSION  MAJOR.MINOR.PATCH, without a leading "v"
#   COMMIT   the full 40-character lowercase SHA of the commit tag v<VERSION>
#            points to (not the annotated tag object's SHA)
#   --root   the repository to change (default: this script's repository)

set -euo pipefail

die() {
    echo "bump-zebrad: error: $*" >&2
    exit 2
}

# shellcheck source-path=SCRIPTDIR source=lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "${script_dir}/.." && pwd)"
args=()
while [ $# -gt 0 ]; do
    case "$1" in
        --root) [ $# -ge 2 ] || die "--root needs a value"; root="$2"; shift 2 ;;
        -h | --help) sed -n '2,/^$/{s/^# \{0,1\}//p;}' "${BASH_SOURCE[0]}"; exit 0 ;;
        -*) die "unknown option: $1" ;;
        *) args+=("$1"); shift ;;
    esac
done
[ "${#args[@]}" -eq 2 ] || die "expected VERSION and COMMIT (see --help)"
version="${args[0]}"
commit="${args[1]}"
[ -d "${root}" ] || die "${root} is not a directory"

# The pin grammar is decided in lib.sh: a stable release only.
check_stable_version "${version}"
check_commit "${commit}"

printf '%s\n' "${version}" >"${root}/ZEBRAD_VERSION"
printf '%s\n' "${commit}" >"${root}/ZEBRAD_COMMIT"

"${script_dir}/check-versions.sh" --root "${root}"
