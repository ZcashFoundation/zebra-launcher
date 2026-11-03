#!/usr/bin/env bash
# Prints zebrad-build-info.txt for a development build of zebrad (one built
# from a Zebra git ref by build-zebrad.sh --ref / build-zebrad.ps1 -Ref): a
# warning that it is unverified, then KEY=value lines with the ref, the
# resolved commit and what the binary reports for --version. In CI the GitHub
# Actions run is recorded too. zebrad-sidecars.yml puts the file in each
# sidecar artifact, and bundle.yml next to the bundles.
#
# Usage: scripts/dev-build-info.sh --ref REF --commit SHA --target TRIPLE --binary PATH

set -euo pipefail

die() {
    echo "dev-build-info: error: $*" >&2
    exit 1
}

ref=""
commit=""
target=""
binary=""

while [ $# -gt 0 ]; do
    case "$1" in
        --ref) [ $# -ge 2 ] || die "--ref needs a value"; ref="$2"; shift 2 ;;
        --commit) [ $# -ge 2 ] || die "--commit needs a value"; commit="$2"; shift 2 ;;
        --target) [ $# -ge 2 ] || die "--target needs a value"; target="$2"; shift 2 ;;
        --binary) [ $# -ge 2 ] || die "--binary needs a value"; binary="$2"; shift 2 ;;
        *) die "unknown argument: $1" ;;
    esac
done

[ -n "${ref}" ] || die "--ref is required"
[ -n "${commit}" ] || die "--commit is required"
[ -n "${target}" ] || die "--target is required"
[ -x "${binary}" ] || die "--binary '${binary}' is not an executable file"

version_string="$("${binary}" --version 2>/dev/null | head -n 1 | tr -d '\r')" || version_string=""
case "${version_string}" in
    "zebrad "?*) ;;
    *) die "${binary} reports '${version_string}' for --version, not 'zebrad <version>'" ;;
esac

cat <<EOF
UNVERIFIED DEVELOPMENT BUILD of zebrad: built from a Zebra git ref, not from
a verified Zebra release. Its commit is not pinned or checked, and it is not
signed. Never release or distribute it.

zebrad_ref=${ref}
zebrad_commit=${commit}
zebrad_version=${version_string}
target=${target}
EOF
if [ -n "${GITHUB_RUN_ID:-}" ]; then
    echo "workflow_run=${GITHUB_SERVER_URL:-https://github.com}/${GITHUB_REPOSITORY:-}/actions/runs/${GITHUB_RUN_ID}"
    echo "launcher_commit=${GITHUB_SHA:-}"
fi
