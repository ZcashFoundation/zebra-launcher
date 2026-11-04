#!/usr/bin/env bash
# Pins Zebra's newest published GitHub prerelease as the bundled beta. Resolves
# annotated tags to their commit, records the commit date, and runs all pin
# checks. This is a manual maintainer tool; it does not commit or push anything.
#
# Requires: bash, gh (authenticated only if GitHub's anonymous rate limit is
# insufficient), sed.
#
# Usage: scripts/bump-zebrad-beta.sh [--root DIR]
#   --root  the repository to change (default: this script's repository)

set -euo pipefail

die() {
    echo "bump-zebrad-beta: error: $*" >&2
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

tags="$(gh api --paginate "repos/${ZEBRA_REPO}/releases?per_page=100" --jq \
    '.[] | select((.draft | not) and .prerelease) | .tag_name')"
version=""
while IFS= read -r tag; do
    case "${tag}" in
        v*) candidate="${tag#v}" ;;
        *) continue ;;
    esac
    is_prerelease_version "${candidate}" || continue
    if [ -z "${version}" ] || semver_newer "${candidate}" "${version}"; then
        version="${candidate}"
        newest_tag="${tag}"
    fi
done <<< "${tags}"
[ -n "${version}" ] || die "${ZEBRA_REPO} has no published prerelease with a vX.Y.Z-PRERELEASE tag"

# A release tag can point to an annotated tag object, including a nested one.
object="$(gh api "repos/${ZEBRA_REPO}/git/ref/tags/${newest_tag}" --jq '.object.type + " " + .object.sha')"
read -r object_type commit <<< "${object}"
for _ in 1 2 3 4 5; do
    [ "${object_type}" = tag ] || break
    object="$(gh api "repos/${ZEBRA_REPO}/git/tags/${commit}" --jq '.object.type + " " + .object.sha')"
    read -r object_type commit <<< "${object}"
done
[ "${object_type}" = commit ] || die "could not resolve ${newest_tag} to a commit (got ${object_type} ${commit})"
check_commit "${commit}"

timestamp="$(gh api "repos/${ZEBRA_REPO}/commits/${commit}" --jq '.commit.committer.date')"
date="${timestamp%%T*}"
is_iso_date "${date}" || die "${newest_tag} has invalid commit date '${timestamp}'"
stable="$(read_zebrad_version "${root}")"
semver_newer "${version}" "${stable}" \
    || die "newest prerelease ${version} is not semver-newer than stable ${stable}; leaving the beta pin unchanged"

printf '%s\n' "${version}" >"${root}/ZEBRAD_BETA_VERSION"
printf '%s\n' "${commit}" >"${root}/ZEBRAD_BETA_COMMIT"
printf '%s\n' "${date}" >"${root}/ZEBRAD_BETA_DATE"
echo "Pinned beta ${newest_tag} at ${commit} (${date})"
"${script_dir}/check-versions.sh" --root "${root}"
