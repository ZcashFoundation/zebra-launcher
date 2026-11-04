#!/usr/bin/env bash
# Pins the tip of Zebra's next branch as the bundled nightly. Reads zebrad's
# Cargo package version at that exact commit, records its commit date, and runs
# all pin checks. This is a manual maintainer tool; it does not commit or push.
#
# Requires: bash, gh (authenticated only if GitHub's anonymous rate limit is
# insufficient), sed.
#
# Usage: scripts/bump-zebrad-nightly.sh [--root DIR]
#   --root  the repository to change (default: this script's repository)

set -euo pipefail

die() {
    echo "bump-zebrad-nightly: error: $*" >&2
    exit 2
}

# shellcheck source-path=SCRIPTDIR source=lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "${script_dir}/.." && pwd)"
while [ $# -gt 0 ]; do
    case "$1" in
        --root) [ $# -ge 2 ] || die "--root needs a value"; root="$2"; shift 2 ;;
        -h | --help) sed -n '2,/^$/{s/^# \{0,1\}//p;}' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) die "unknown argument: $1" ;;
    esac
done
[ -d "${root}" ] || die "${root} is not a directory"
command -v gh >/dev/null 2>&1 || die "gh is required but not on PATH"

readonly ZEBRA_REPO=ZcashFoundation/zebra
readonly ZEBRA_BRANCH=next

resolved="$(gh api "repos/${ZEBRA_REPO}/commits/${ZEBRA_BRANCH}" --jq '.sha + " " + .commit.committer.date')"
read -r commit timestamp <<< "${resolved}"
check_commit "${commit}"
date="${timestamp%%T*}"
is_iso_date "${date}" || die "${ZEBRA_BRANCH} has invalid commit date '${timestamp}'"

manifest="$(gh api -H 'Accept: application/vnd.github.raw+json' \
    "repos/${ZEBRA_REPO}/contents/zebrad/Cargo.toml?ref=${commit}")"
version="$(printf '%s\n' "${manifest}" \
    | sed -n 's/^version[[:space:]]*=[[:space:]]*"\([^"]*\)"[[:space:]]*$/\1/p' | head -n 1)"
check_zebrad_version "${version}" "in zebrad/Cargo.toml at ${commit}"

printf '%s\n' "${version}" >"${root}/ZEBRAD_NIGHTLY_VERSION"
printf '%s\n' "${commit}" >"${root}/ZEBRAD_NIGHTLY_COMMIT"
printf '%s\n' "${date}" >"${root}/ZEBRAD_NIGHTLY_DATE"
echo "Pinned nightly to Zebra ${ZEBRA_BRANCH} at ${commit} (zebrad ${version}, ${date})"
"${script_dir}/check-versions.sh" --root "${root}"
