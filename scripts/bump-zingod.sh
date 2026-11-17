#!/usr/bin/env bash
# Sets the bundled zingod to a new release: writes ZINGOD_VERSION and
# ZINGOD_COMMIT, refreshes src-tauri/resources/licenses/zingod/ from a checkout
# at the commit (through scripts/zingod-licenses.sh), then runs
# scripts/check-versions.sh against the result. Set ZINGOD_DATE, the commit's
# date in UTC, in the same change: the script prints it. See docs/SIDECARS.md,
# "Bumping ZINGOD_VERSION".
#
# It refuses, and names, a licence file that would be committed with an email
# address in it: Zeal keeps addresses out of its repository, so such a file
# moves to zingod-licenses.sh's copied list. Nothing is written until every
# file passes. It does not build or verify zingod; build-zingod.sh and the
# zingod sidecars workflow do.
#
# Requires: bash, git, cargo (to fetch the checkout's dependencies), grep, sed.
#
# Usage: scripts/bump-zingod.sh [--root DIR] [--src-dir DIR] VERSION COMMIT
#   VERSION    MAJOR.MINOR.PATCH, without a leading "v"
#   COMMIT     the full 40-character lowercase SHA of the commit that tag
#              v<VERSION> names
#   --src-dir  a zingod checkout at COMMIT (default: fetched into a temporary
#              directory)
#   --root     the repository to change (default: this script's repository)

set -euo pipefail

readonly ZINGOD_GIT="https://github.com/zingolabs/zingod"

die() {
    echo "bump-zingod: error: $*" >&2
    exit 2
}

# shellcheck source-path=SCRIPTDIR source=lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "${script_dir}/.." && pwd)"
src=""
args=()
while [ $# -gt 0 ]; do
    case "$1" in
        --root) [ $# -ge 2 ] || die "--root needs a value"; root="$2"; shift 2 ;;
        --src-dir) [ $# -ge 2 ] || die "--src-dir needs a value"; src="$2"; shift 2 ;;
        -h | --help) sed -n '2,/^$/{s/^# \{0,1\}//p;}' "${BASH_SOURCE[0]}"; exit 0 ;;
        -*) die "unknown option: $1" ;;
        *) args+=("$1"); shift ;;
    esac
done
[ "${#args[@]}" -eq 2 ] || die "expected VERSION and COMMIT (see --help)"
version="${args[0]}"
commit="${args[1]}"
[ -d "${root}" ] || die "${root} is not a directory"

# The pin grammar is decided in lib.sh: a release only.
check_stable_version "${version}"
check_commit "${commit}"

work="$(mktemp -d)"
trap 'rm -rf "${work}"' EXIT
if [ -z "${src}" ]; then
    src="${work}/zingod"
    git init -q "${src}"
    git -C "${src}" fetch -q --depth 1 "${ZINGOD_GIT}" "${commit}" || die "cannot fetch ${commit} from ${ZINGOD_GIT}"
    git -C "${src}" -c advice.detachedHead=false checkout -q --detach FETCH_HEAD
fi
head="$(git -C "${src}" rev-parse HEAD)"
[ "${head}" = "${commit}" ] || die "${src} is at ${head}, not at ${commit}"
cargo fetch --manifest-path "${src}/Cargo.toml" --locked

"${script_dir}/zingod-licenses.sh" --src-dir "${src}" --out "${work}/licenses"

# An address anywhere in a file that would be committed stops the bump.
with_address=()
for file in "${work}/licenses/committed/"*; do
    if grep -Eq '[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}' "${file}"; then
        with_address+=("$(basename "${file}")")
    fi
done
[ "${#with_address[@]}" -eq 0 ] ||
    die "these licence files contain an email address, so they can't be committed: ${with_address[*]}. Move them to the copied list in scripts/zingod-licenses.sh"

# Replace the committed folder's files, keeping the copied ones a build
# installed there.
folder="${root}/src-tauri/resources/licenses/zingod"
mkdir -p "${folder}"
for file in "${folder}/"*; do
    [ -e "${file}" ] || continue
    name="$(basename "${file}")"
    [ -f "${work}/licenses/copied/${name}" ] || rm -f "${file}"
done
cp "${work}/licenses/committed/"* "${folder}/"

printf '%s\n' "${version}" >"${root}/ZINGOD_VERSION"
printf '%s\n' "${commit}" >"${root}/ZINGOD_COMMIT"
date="$(TZ=UTC git -C "${src}" log -1 --format=%cd --date=format-local:%Y-%m-%d)"
echo "Wrote ZINGOD_VERSION ${version}, ZINGOD_COMMIT ${commit} and ${folder}."
echo "Set ZINGOD_DATE in the same change: the commit's date in UTC is ${date}."

"${script_dir}/check-versions.sh" --root "${root}"
