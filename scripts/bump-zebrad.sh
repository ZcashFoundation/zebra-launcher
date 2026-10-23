#!/usr/bin/env bash
# Sets the bundled zebrad to a new release: writes ZEBRAD_VERSION and
# ZEBRAD_COMMIT and the VERSION constant in
# crates/launcher-core/src/bin/fake-zebrad.rs, then runs
# scripts/check-versions.sh against the result. The three must change
# together (docs/SIDECARS.md, "Bumping ZEBRAD_VERSION"). zebrad-watch.yml runs
# this script; it does not fetch, build or verify zebrad.
#
# Requires: bash, grep, sed.
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

printf '%s\n' "${version}" | grep -Eq '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$' ||
    die "'${version}' is not MAJOR.MINOR.PATCH without a leading v"
printf '%s\n' "${commit}" | grep -Eq '^[0-9a-f]{40}$' ||
    die "'${commit}' is not a full 40-character lowercase hex SHA"

fake="${root}/crates/launcher-core/src/bin/fake-zebrad.rs"
[ -f "${fake}" ] || die "${fake} is missing"
# Only the exact constant line is rewritten, so a VERSION string elsewhere in
# the file (tests, format strings) is never touched.
pattern='^const VERSION: &str = "[^"]*";$'
matches="$(grep -c -- "${pattern}" "${fake}" || true)"
[ "${matches}" -eq 1 ] || die "${fake} must have exactly one line 'const VERSION: &str = \"X.Y.Z\";' (found ${matches})"

printf '%s\n' "${version}" >"${root}/ZEBRAD_VERSION"
printf '%s\n' "${commit}" >"${root}/ZEBRAD_COMMIT"
# Write through a temporary file so this works with BSD and GNU sed alike.
sed "s/${pattern}/const VERSION: \\&str = \"${version}\";/" "${fake}" >"${fake}.bump-tmp"
mv "${fake}.bump-tmp" "${fake}"

"${script_dir}/check-versions.sh" --root "${root}"
