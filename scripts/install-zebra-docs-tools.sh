#!/usr/bin/env bash
# Installs the mdBook tools used to render the offline Zebra documentation.
# Both release archives are pinned by version and SHA-256; no Rust or Node
# toolchain is needed. The versions follow Zebra's mdBook 0.5 book workflow.
#
# Usage: scripts/install-zebra-docs-tools.sh [--bin-dir DIR]
# Default DIR: $RUNNER_TEMP/zebra-docs-tools, or /tmp/zebra-docs-tools.

set -euo pipefail

readonly MDBOOK_VERSION="0.5.2"
readonly MDBOOK_SHA256="8e9d802bbecea34380aabfc65bec4fdba23c07453ef2c7e06fa34c282082e2c4"
readonly MDBOOK_MERMAID_VERSION="0.17.0"
readonly MDBOOK_MERMAID_SHA256="c3a924da1c1895d694f0544fb8ce71ae01508138d8e9b02374e9bf608ae765d7"

bin_dir="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/zebra-docs-tools"

die() {
    echo "install-zebra-docs-tools: error: $*" >&2
    exit 1
}

while [ $# -gt 0 ]; do
    case "$1" in
        --bin-dir) [ $# -ge 2 ] || die "--bin-dir needs a directory"; bin_dir="$2"; shift 2 ;;
        -h | --help) sed -n '2,/^$/{s/^# \{0,1\}//p;}' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) die "unknown argument: $1" ;;
    esac
done

for tool in curl tar sha256sum install; do
    command -v "${tool}" > /dev/null 2>&1 || die "${tool} is required but not on PATH"
done

work="$(mktemp -d)"
trap 'rm -rf "${work}"' EXIT
mkdir -p "${bin_dir}"

fetch_tool() {
    local package="$1" version="$2" expected="$3" executable="$4"
    local archive="${work}/${executable}.tar.gz"
    local url="https://github.com/${package}/releases/download/v${version}"
    local extracted="${work}/${executable}-extract"

    case "${package}" in
        rust-lang/mdBook) url="${url}/mdbook-v${version}-x86_64-unknown-linux-musl.tar.gz" ;;
        badboy/mdbook-mermaid) url="${url}/mdbook-mermaid-v${version}-x86_64-unknown-linux-musl.tar.gz" ;;
        *) die "unknown tool package ${package}" ;;
    esac

    curl --proto '=https' --tlsv1.2 -fsSL --retry 3 -o "${archive}" "${url}" ||
        die "could not download ${url}"
    echo "${expected}  ${archive}" | sha256sum -c - || die "${package} checksum mismatch"
    mkdir -p "${extracted}"
    tar -xzf "${archive}" -C "${extracted}" "${executable}" ||
        die "${package} archive does not contain ${executable}"
    install -m 0755 "${extracted}/${executable}" "${bin_dir}/${executable}"
}

fetch_tool rust-lang/mdBook "${MDBOOK_VERSION}" "${MDBOOK_SHA256}" mdbook
fetch_tool badboy/mdbook-mermaid "${MDBOOK_MERMAID_VERSION}" "${MDBOOK_MERMAID_SHA256}" mdbook-mermaid

reported="$("${bin_dir}/mdbook" --version)"
[ "${reported}" = "mdbook v${MDBOOK_VERSION}" ] ||
    die "mdbook reports '${reported}', expected mdbook v${MDBOOK_VERSION}"
reported="$("${bin_dir}/mdbook-mermaid" --version)"
[ "${reported}" = "mdbook-mermaid ${MDBOOK_MERMAID_VERSION}" ] ||
    die "mdbook-mermaid reports '${reported}', expected mdbook-mermaid ${MDBOOK_MERMAID_VERSION}"

printf '%s\n' "Installed mdbook v${MDBOOK_VERSION} and mdbook-mermaid ${MDBOOK_MERMAID_VERSION} in ${bin_dir}"
