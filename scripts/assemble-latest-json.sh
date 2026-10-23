#!/usr/bin/env bash
# Writes the Tauri updater manifest (latest.json) for a release from the
# updater artifacts and their .sig files, named as scripts/collect-bundles.sh
# names them, and fails unless all five platform keys are present:
#
#   linux-x86_64    zebra-launcher_<ver>_x86_64.AppImage
#   linux-aarch64   zebra-launcher_<ver>_aarch64.AppImage
#   darwin-aarch64  zebra-launcher_<ver>_aarch64.app.tar.gz
#   darwin-x86_64   zebra-launcher_<ver>_x86_64.app.tar.gz
#   windows-x86_64  zebra-launcher_<ver>_x86_64-setup.exe
#
# Each entry's `url` is the release download URL
# https://github.com/<repo>/releases/download/<tag>/<asset>, which works once
# the release is published (the updater endpoint only ever serves published
# releases), and `signature` is the content of the asset's .sig file.
#
# Requires: jq.
#
# Usage: scripts/assemble-latest-json.sh --dir DIR --version X.Y.Z --tag vX.Y.Z
#                                        --repo OWNER/NAME --out FILE [--notes TEXT]

set -euo pipefail

die() {
    echo "::error::assemble-latest-json: $*"
    exit 1
}

dir=""
version=""
tag=""
repo=""
out=""
notes=""
while [ $# -gt 0 ]; do
    case "$1" in
        --dir) [ $# -ge 2 ] || die "--dir needs a value"; dir="$2"; shift 2 ;;
        --version) [ $# -ge 2 ] || die "--version needs a value"; version="$2"; shift 2 ;;
        --tag) [ $# -ge 2 ] || die "--tag needs a value"; tag="$2"; shift 2 ;;
        --repo) [ $# -ge 2 ] || die "--repo needs a value"; repo="$2"; shift 2 ;;
        --out) [ $# -ge 2 ] || die "--out needs a value"; out="$2"; shift 2 ;;
        --notes) [ $# -ge 2 ] || die "--notes needs a value"; notes="$2"; shift 2 ;;
        *) die "unknown argument: $1" ;;
    esac
done
for arg in dir version tag repo out; do
    [ -n "${!arg}" ] || die "--${arg} is required"
done

prefix="zebra-launcher_${version}"
platforms=(
    "linux-x86_64=${prefix}_x86_64.AppImage"
    "linux-aarch64=${prefix}_aarch64.AppImage"
    "darwin-aarch64=${prefix}_aarch64.app.tar.gz"
    "darwin-x86_64=${prefix}_x86_64.app.tar.gz"
    "windows-x86_64=${prefix}_x86_64-setup.exe"
)

entries='{}'
missing=()
for entry in "${platforms[@]}"; do
    key="${entry%%=*}"
    asset="${entry#*=}"
    if [ ! -s "${dir}/${asset}" ] || [ ! -s "${dir}/${asset}.sig" ]; then
        missing+=("${key} (${asset} and ${asset}.sig)")
        continue
    fi
    entries="$(jq \
        --arg key "${key}" \
        --arg url "https://github.com/${repo}/releases/download/${tag}/${asset}" \
        --rawfile sig "${dir}/${asset}.sig" \
        '. + {($key): {signature: ($sig | rtrimstr("\n")), url: $url}}' <<< "${entries}")"
done

if [ "${#missing[@]}" -gt 0 ]; then
    printf '::error::latest.json is missing platform %s\n' "${missing[@]}"
    exit 1
fi

jq -n \
    --arg version "${version}" \
    --arg notes "${notes}" \
    --arg pub_date "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --argjson platforms "${entries}" \
    '{version: $version, notes: $notes, pub_date: $pub_date, platforms: $platforms}' \
    > "${out}"

# Belt and braces: re-read the file and check the five keys.
count="$(jq '.platforms | keys | length' "${out}")"
[ "${count}" -eq 5 ] || die "${out} has ${count} platform keys, expected 5"
echo "Wrote ${out}:"
jq '{version, pub_date, platforms: (.platforms | map_values(.url))}' "${out}"
