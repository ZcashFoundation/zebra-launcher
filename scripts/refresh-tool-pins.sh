#!/usr/bin/env bash
# Recomputes the sha256 pins of one build tool in .github/tool-versions.env
# for a given version: downloads that release's asset for every platform the
# file already pins (its <TOOL>_SHA256_<PLATFORM> lines), hashes each one, and
# cross-checks the hash against the `digest` GitHub reports for the asset
# (through `gh api`, when gh is installed and the release has digests).
#
# Without --write it only prints what it would set (a dry run). With --write it
# rewrites <TOOL>_VERSION and the <TOOL>_SHA256_* lines in place; the comments
# (such as the date the hashes were computed) are left for you to update.
# Nothing is written unless every download succeeded and every digest GitHub
# reports matched.
#
# Tools, their release pages and the asset each platform key names:
#   wasm-bindgen  wasm-bindgen/wasm-bindgen, tag <version>,
#                 wasm-bindgen-<version>-<triple>.tar.gz
#   binaryen      WebAssembly/binaryen, tag <version> (such as version_131),
#                 binaryen-<version>-<arch>-<os>.tar.gz
#   tauri-cli     tauri-apps/tauri, tag tauri-cli-v<version>,
#                 cargo-tauri-<triple>.tgz (Linux) or .zip (macOS, Windows)
#   trunk         trunk-rs/trunk, tag v<version>,
#                 trunk-<triple>.tar.gz, or .zip on Windows
# These are the URLs .github/actions/setup-tauri downloads; keep them in step.
#
# Exits 1 if a download fails or a hash differs from GitHub's digest.
# Requires: bash, curl, sed, awk, sha256sum or shasum; gh for the digest check.
#
# Usage: scripts/refresh-tool-pins.sh [--root DIR] [--write] TOOL VERSION
#   TOOL     wasm-bindgen, binaryen, tauri-cli or trunk
#   VERSION  the release version, as the *_VERSION line takes it
#   --write  update .github/tool-versions.env (default: dry run)
#   --root   the repository to change (default: this script's repository)

set -euo pipefail

die() {
    echo "refresh-tool-pins: error: $*" >&2
    exit 2
}

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
write=false
args=()
while [ $# -gt 0 ]; do
    case "$1" in
        --root) [ $# -ge 2 ] || die "--root needs a value"; root="$2"; shift 2 ;;
        --write) write=true; shift ;;
        -h | --help) sed -n '2,/^$/{s/^# \{0,1\}//p;}' "${BASH_SOURCE[0]}"; exit 0 ;;
        -*) die "unknown option: $1" ;;
        *) args+=("$1"); shift ;;
    esac
done
[ "${#args[@]}" -eq 2 ] || die "expected TOOL and VERSION (see --help)"
tool="${args[0]}"
version="${args[1]}"
env_file="${root}/.github/tool-versions.env"
[ -f "${env_file}" ] || die "${env_file} is missing"

# The version goes into URLs and a sed replacement: keep it to safe characters.
printf '%s\n' "${version}" | grep -Eq '^[0-9A-Za-z][0-9A-Za-z._-]*$' ||
    die "'${version}' is not a usable version (allowed: letters, digits and ._-)"

case "${tool}" in
    wasm-bindgen) prefix=WASM_BINDGEN; repo=wasm-bindgen/wasm-bindgen; tag="${version}" ;;
    binaryen) prefix=BINARYEN; repo=WebAssembly/binaryen; tag="${version}" ;;
    tauri-cli) prefix=TAURI_CLI; repo=tauri-apps/tauri; tag="tauri-cli-v${version}" ;;
    trunk) prefix=TRUNK; repo=trunk-rs/trunk; tag="v${version}" ;;
    *) die "unknown tool '${tool}': use wasm-bindgen, binaryen, tauri-cli or trunk" ;;
esac

# asset_name PLATFORM: the release asset for a platform as tool-versions.env
# spells it (the part after <PREFIX>_SHA256_, such as X86_64_PC_WINDOWS_MSVC).
asset_name() {
    local platform
    # Lowercase, `_` to `-`, then put back the underscore of x86_64.
    platform="$(printf '%s' "$1" | tr 'A-Z_' 'a-z-' | sed 's/x86-64/x86_64/g')"
    case "${tool}" in
        wasm-bindgen) printf 'wasm-bindgen-%s-%s.tar.gz\n' "${version}" "${platform}" ;;
        binaryen) printf 'binaryen-%s-%s.tar.gz\n' "${version}" "${platform}" ;;
        tauri-cli)
            case "${platform}" in
                *-linux-*) printf 'cargo-tauri-%s.tgz\n' "${platform}" ;;
                *) printf 'cargo-tauri-%s.zip\n' "${platform}" ;;
            esac
            ;;
        trunk)
            case "${platform}" in
                *-windows-*) printf 'trunk-%s.zip\n' "${platform}" ;;
                *) printf 'trunk-%s.tar.gz\n' "${platform}" ;;
            esac
            ;;
    esac
}

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    else
        shasum -a 256 "$1" | cut -d' ' -f1
    fi
}

platforms="$(sed -n "s/^${prefix}_SHA256_\([A-Z0-9_]*\)=.*/\1/p" "${env_file}")"
[ -n "${platforms}" ] || die "${env_file} has no ${prefix}_SHA256_* lines to refresh"
current_version="$(sed -n "s/^${prefix}_VERSION=//p" "${env_file}")"
# A checkout with CRLF line ends leaves a CR on each value, as check-versions.sh notes.
current_version="${current_version%$'\r'}"
[ -n "${current_version}" ] || die "${env_file} has no ${prefix}_VERSION line"

# GitHub's per-asset digests, as "name<TAB>sha256:hex" lines, when gh can get them.
digests=""
if command -v gh >/dev/null 2>&1; then
    if ! digests="$(gh api "repos/${repo}/releases/tags/${tag}" --jq '.assets[] | [.name, (.digest // "")] | @tsv' 2>&1)"; then
        echo "note: gh api could not read ${repo} release ${tag}, so GitHub's digests are not checked: ${digests}" >&2
        digests=""
    fi
else
    echo "note: gh is not installed, so GitHub's asset digests are not checked" >&2
fi

work="$(mktemp -d)"
trap 'rm -rf "${work}"' EXIT

echo "${tool} ${version} (${prefix}_VERSION is ${current_version} now), from https://github.com/${repo}/releases/tag/${tag}"
failures=0
new_lines=()
while IFS= read -r platform; do
    key="${prefix}_SHA256_${platform}"
    asset="$(asset_name "${platform}")"
    url="https://github.com/${repo}/releases/download/${tag}/${asset}"
    if ! curl --proto '=https' --tlsv1.2 -sSfL --retry 3 -o "${work}/${asset}" "${url}"; then
        echo "FAIL: ${key}: could not download ${url}"
        failures=$((failures + 1))
        continue
    fi
    hash="$(sha256_of "${work}/${asset}")"
    rm -f "${work}/${asset}"
    github="$(printf '%s\n' "${digests}" | awk -F '\t' -v name="${asset}" '$1 == name { print $2 }')"
    case "${github}" in
        "sha256:${hash}") check="matches GitHub's digest" ;;
        sha256:*)
            echo "FAIL: ${key}: ${asset} hashes to ${hash}, but GitHub reports ${github}"
            failures=$((failures + 1))
            continue
            ;;
        *) check="no GitHub digest to compare" ;;
    esac
    old="$(sed -n "s/^${key}=//p" "${env_file}")"
    old="${old%$'\r'}"
    if [ "${old}" = "${hash}" ]; then
        state="unchanged"
    else
        state="was ${old:-unset}"
    fi
    echo "${key}=${hash}"
    echo "    ${asset}: ${check}; ${state}"
    new_lines+=("${key}=${hash}")
done <<<"${platforms}"

if [ "${failures}" -gt 0 ]; then
    echo "refresh-tool-pins: ${failures} platform(s) failed; nothing written" >&2
    exit 1
fi
if [ "${write}" != true ]; then
    echo "Dry run: pass --write to set ${prefix}_VERSION=${version} and these hashes in .github/tool-versions.env."
    exit 0
fi

# Rewrite through a temporary file: the version line, then each hash line.
tmp="${env_file}.refresh-tmp"
sed "s/^${prefix}_VERSION=.*/${prefix}_VERSION=${version}/" "${env_file}" >"${tmp}"
for line in "${new_lines[@]}"; do
    key="${line%%=*}"
    sed "s/^${key}=.*/${line}/" "${tmp}" >"${tmp}.next"
    mv "${tmp}.next" "${tmp}"
done
mv "${tmp}" "${env_file}"
echo "Updated .github/tool-versions.env: ${prefix}_VERSION=${version} and ${#new_lines[@]} hash(es). Update the comments above them (dates, notes) by hand, and run scripts/check-versions.sh."
