#!/usr/bin/env bash
# Copies the Tauri bundles for one target out of target/<triple>/release/bundle
# into one directory, under fixed release asset names without spaces:
#
#   zebra-launcher_<ver>_<arch>.AppImage      Linux (updater artifact)
#   zebra-launcher_<ver>_<debarch>.deb        Linux (Notify-only)
#   zebra-launcher_<ver>_<arch>.dmg           macOS installer
#   zebra-launcher_<ver>_<arch>.app.tar.gz    macOS updater artifact
#   zebra-launcher_<ver>_<arch>-setup.exe     Windows NSIS (installer and updater artifact)
#
# plus `<name>.sig` next to each file that has an updater signature. <arch> is
# the first component of the target triple (x86_64, aarch64); <debarch> is
# Debian's name (amd64, arm64).
#
# Tauri's own names contain the product name with a space ("Zebra Launcher"),
# which GitHub rewrites to a dot on upload, and the two macOS architectures
# would both produce "Zebra Launcher.app.tar.gz". Fixed names keep the release
# assets and the URLs in latest.json predictable.
#
# Every bundle type expected for the target must be present exactly once.
#
# Usage: scripts/collect-bundles.sh --target TRIPLE --out DIR [--version X.Y.Z]
#                                   [--require-updater-sigs]

set -euo pipefail

die() {
    echo "collect-bundles: error: $*" >&2
    exit 1
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

target=""
out=""
version=""
require_sigs=false

while [ $# -gt 0 ]; do
    case "$1" in
        --target) [ $# -ge 2 ] || die "--target needs a value"; target="$2"; shift 2 ;;
        --out) [ $# -ge 2 ] || die "--out needs a value"; out="$2"; shift 2 ;;
        --version) [ $# -ge 2 ] || die "--version needs a value"; version="$2"; shift 2 ;;
        --require-updater-sigs) require_sigs=true; shift ;;
        *) die "unknown argument: $1" ;;
    esac
done

[ -n "${target}" ] || die "--target is required"
[ -n "${out}" ] || die "--out is required"
[ -n "${version}" ] || version="$("${repo_root}/scripts/workspace-version.sh")"

bundle_dir="${repo_root}/target/${target}/release/bundle"
[ -d "${bundle_dir}" ] || die "${bundle_dir} does not exist; run cargo tauri build/bundle first"
mkdir -p "${out}"

arch="${target%%-*}"
prefix="zebra-launcher_${version}"

# copy_one GLOB DEST UPDATER: copies the single file matching GLOB (relative
# to the bundle directory) to DEST in the output directory, with its .sig.
# UPDATER=true marks an updater artifact, whose .sig is required with
# --require-updater-sigs.
copy_one() {
    local pattern="$1" dest="$2" updater="$3"
    local matches=()
    shopt -s nullglob
    # shellcheck disable=SC2206 # the glob is meant to expand here
    matches=("${bundle_dir}"/${pattern})
    shopt -u nullglob
    [ "${#matches[@]}" -eq 1 ] \
        || die "expected exactly one ${pattern} in ${bundle_dir}, found ${#matches[@]}"

    cp "${matches[0]}" "${out}/${dest}"
    echo "${matches[0]#"${repo_root}/"} -> ${out}/${dest}"
    if [ -f "${matches[0]}.sig" ]; then
        cp "${matches[0]}.sig" "${out}/${dest}.sig"
        echo "${matches[0]#"${repo_root}/"}.sig -> ${out}/${dest}.sig"
    elif [ "${updater}" = true ] && [ "${require_sigs}" = true ]; then
        die "${matches[0]} has no updater signature (.sig)"
    fi
}

case "${target}" in
    x86_64-unknown-linux-gnu | aarch64-unknown-linux-gnu)
        if [ "${arch}" = x86_64 ]; then debarch=amd64; else debarch=arm64; fi
        copy_one 'appimage/*.AppImage' "${prefix}_${arch}.AppImage" true
        copy_one 'deb/*.deb' "${prefix}_${debarch}.deb" false
        ;;
    aarch64-apple-darwin | x86_64-apple-darwin)
        copy_one 'dmg/*.dmg' "${prefix}_${arch}.dmg" false
        # Exists only when the updater artifacts are enabled.
        if compgen -G "${bundle_dir}/macos/*.app.tar.gz" > /dev/null || [ "${require_sigs}" = true ]; then
            copy_one 'macos/*.app.tar.gz' "${prefix}_${arch}.app.tar.gz" true
        fi
        ;;
    x86_64-pc-windows-msvc)
        copy_one 'nsis/*-setup.exe' "${prefix}_${arch}-setup.exe" true
        ;;
    *)
        die "unsupported target ${target}"
        ;;
esac
