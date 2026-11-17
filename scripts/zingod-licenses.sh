#!/usr/bin/env bash
# Gathers the licence files Zeal ships with zingod from a zingod checkout and
# the sources cargo fetched for it, into two folders:
#
#   OUT/committed/  what src-tauri/resources/licenses/zingod/ holds in git:
#                   zingod-LICENSE (zingod's notice), Apache-2.0.txt (the
#                   licence's full text), lightwallet-protocol-LICENSE,
#                   zebra-LICENSE-MIT, zstd-LICENSE, secp256k1-COPYING, and
#                   README.txt, which this script writes
#   OUT/copied/     jemalloc-COPYING and aws-lc-LICENSE, whose copyright lines
#                   carry personal addresses: they stay out of the repository
#                   and are copied into each shipped bundle instead
#
# build-zingod.sh checks the committed folder against OUT/committed and ships
# OUT/copied with the binary; bump-zingod.sh refreshes the folder from
# OUT/committed. zingod's checkout has only the licence's notice, so
# Apache-2.0.txt is the committed copy of
# https://www.apache.org/licenses/LICENSE-2.0.txt, checked against that file's
# sha256 rather than against a copy some dependency happens to ship. See
# docs/SIDECARS.md, "Licences of zingod".
#
# Requires: bash, git, cargo (with the checkout's dependencies fetched: this
# runs `cargo metadata --offline`), grep, sed, sha256sum or shasum.
#
# Usage: scripts/zingod-licenses.sh --src-dir DIR --out DIR

set -euo pipefail

die() {
    echo "zingod-licenses: error: $*" >&2
    exit 1
}

# shellcheck source-path=SCRIPTDIR source=lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "${script_dir}/.." && pwd)"
readonly COMMITTED_DIR="${repo_root}/src-tauri/resources/licenses/zingod"
readonly APACHE_SHA256=cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30

# NAME FROM PATH. FROM is `checkout` (the zingod checkout), a crate (its folder
# as cargo fetched it) or `repo:<crate>` (the git repository the crate comes
# from: the folder above the crate's).
readonly COMMITTED=(
    "zingod-LICENSE checkout LICENSE"
    "lightwallet-protocol-LICENSE checkout packages/zingod-proto/lightwallet-protocol/LICENSE"
    "zebra-LICENSE-MIT repo:zebra-chain LICENSE-MIT"
    "zstd-LICENSE zstd-sys zstd/LICENSE"
    "secp256k1-COPYING secp256k1-sys depend/secp256k1/COPYING"
)
readonly COPIED=(
    "jemalloc-COPYING tikv-jemalloc-sys jemalloc/COPYING"
    "aws-lc-LICENSE aws-lc-sys aws-lc/LICENSE"
)

src=""
out=""
while [ $# -gt 0 ]; do
    case "$1" in
        --src-dir) [ $# -ge 2 ] || die "--src-dir needs a value"; src="$2"; shift 2 ;;
        --out) [ $# -ge 2 ] || die "--out needs a value"; out="$2"; shift 2 ;;
        -h | --help) sed -n '2,/^$/{s/^# \{0,1\}//p;}' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) die "unknown argument: $1" ;;
    esac
done
[ -n "${src}" ] || die "--src-dir is required"
[ -n "${out}" ] || die "--out is required"
[ -f "${src}/Cargo.lock" ] || die "${src} is not a zingod checkout (no Cargo.lock)"
lock="${src}/Cargo.lock"

metadata="$(cargo metadata --manifest-path "${src}/Cargo.toml" --format-version 1 --locked --offline)" ||
    die "cargo metadata failed in ${src}; run \`cargo fetch --locked\` there first"
manifests="$(printf '%s\n' "${metadata}" | grep -o '"manifest_path":"[^"]*"' | sed 's/^"manifest_path":"//; s/"$//' | sort -u)"

# crate_dir NAME: prints the folder of the one crate NAME in the checkout's
# Cargo.lock, as cargo fetched it: <NAME>-<version> from a registry, <NAME>
# from a git repository.
crate_dir() {
    local version found
    version="$("${script_dir}/cargo-lock-version.sh" "$1" "${lock}")" || die "no single $1 in ${lock}"
    found="$(printf '%s\n' "${manifests}" | grep -F -e "/$1-${version}/Cargo.toml" -e "/$1/Cargo.toml" || true)"
    [ -n "${found}" ] || die "cargo metadata names no folder for $1 ${version}"
    [ "$(printf '%s\n' "${found}" | wc -l)" -eq 1 ] || die "cargo metadata names more than one folder for $1: ${found}"
    dirname "${found}"
}

# source_of FROM PATH: prints the file that FROM and PATH name.
source_of() {
    local dir
    case "$1" in
        checkout) dir="${src}" ;;
        repo:*) dir="$(crate_dir "${1#repo:}")" && dir="$(dirname "${dir}")" ;;
        *) dir="$(crate_dir "$1")" ;;
    esac
    printf '%s\n' "${dir}/$2"
}

rm -rf "${out}/committed" "${out}/copied"
mkdir -p "${out}/committed" "${out}/copied"

# gather KIND NAME FROM PATH
gather() {
    local from
    from="$(source_of "$3" "$4")"
    [ -f "${from}" ] || die "${from} is missing"
    cp "${from}" "${out}/$1/$2"
    echo "$1/$2 <- ${from}"
}
for entry in "${COMMITTED[@]}"; do
    # shellcheck disable=SC2086 # each entry is three words
    gather committed ${entry}
done
for entry in "${COPIED[@]}"; do
    # shellcheck disable=SC2086 # each entry is three words
    gather copied ${entry}
done

apache="${COMMITTED_DIR}/Apache-2.0.txt"
[ -f "${apache}" ] || die "${apache} is missing"
[ "$(sha256_of "${apache}")" = "${APACHE_SHA256}" ] ||
    die "${apache} is not the text of https://www.apache.org/licenses/LICENSE-2.0.txt (sha256 ${APACHE_SHA256})"
cp "${apache}" "${out}/committed/Apache-2.0.txt"
echo "committed/Apache-2.0.txt <- ${apache} (sha256 checked)"

# The facts README.txt names, read from the checkout and its sources.
version="$(sed -n '/^\[workspace\.package\]$/,/^\[/{s/^version = "\([^"]*\)"$/\1/p;}' "${src}/Cargo.toml")"
[ -n "${version}" ] || die "no [workspace.package] version in ${src}/Cargo.toml"
commit="$(git -C "${src}" rev-parse HEAD)"
copyright="$(sed -n 's/^[[:space:]]*\(Copyright .*[^[:space:]]\)[[:space:]]*$/\1/p' "${src}/LICENSE" | head -n 1)"
[ -n "${copyright}" ] || die "no copyright line in ${src}/LICENSE"
zebra="$(tr -d '\r' <"${lock}" | sed -n '/^name = "zebra-chain"$/{n;n;s/^source = "git+\([^?#]*\)[^#]*#\([0-9a-f]*\)"$/\1 \2/p;}')"
zebra_repo="${zebra% *}"
zebra_repo="${zebra_repo%.git}"
zebra_rev="${zebra#* }"
if [ -z "${zebra}" ] || ! is_full_sha "${zebra_rev}"; then
    die "zebra-chain is not a git dependency at a full commit in ${lock}"
fi
lock_version() { "${script_dir}/cargo-lock-version.sh" "$1" "${lock}"; }
aws_lc_sys="$(lock_version aws-lc-sys)"
aws_lc_dir="$(crate_dir aws-lc-sys)"
aws_lc="$(sed -n 's/^#define AWSLC_VERSION_NUMBER_STRING "\([^"]*\)"$/\1/p' "${aws_lc_dir}/aws-lc/include/openssl/base.h")"
jemalloc_sys="$(lock_version tikv-jemalloc-sys)"
jemalloc="${jemalloc_sys#*+}"
jemalloc="${jemalloc%%-*}"
zstd_sys="$(lock_version zstd-sys)"
zstd="${zstd_sys#*+zstd.}"
secp256k1_sys="$(lock_version secp256k1-sys)"
for fact in aws_lc jemalloc zstd; do
    [ -n "${!fact}" ] || die "cannot tell the ${fact//_/-} version from the sources"
done

cat >"${out}/committed/README.txt" <<EOF
zingod, bundled with Zeal as zeal-zingod

zingod ${version} is Zingo Labs' Zcash indexer. ${copyright}.
It is licensed under the Apache License, Version 2.0: zingod-LICENSE is
zingod's notice and Apache-2.0.txt the licence's full text.

Source: https://github.com/zingolabs/zingod, tag v${version}, commit
${commit}.
Each Zeal release that bundles zingod also has the asset
zingod-src-${version}.tar.zst: that source with every dependency vendored,
which Zeal built zeal-zingod from.

The .proto files compiled into zingod, in packages/zingod-proto/proto/:
- compact_formats.proto, from lightwallet-protocol, which zingod vendors in
  packages/zingod-proto/lightwallet-protocol/: MIT,
  lightwallet-protocol-LICENSE.
- service.proto, zingod-proto's modified copy of lightwallet-protocol's,
  under the Zcash developers' MIT licence named in its header.
- zebra_indexer.proto, Zebra's zebra-rpc/proto/indexer.proto through
  ${zebra_repo} at commit
  ${zebra_rev}: MIT OR Apache-2.0,
  zebra-LICENSE-MIT. The zebra-chain and zebra-network crates zingod links
  come from the same commit, under the same licences.

The C libraries linked in statically:
- AWS-LC ${aws_lc} (aws-lc-sys ${aws_lc_sys}): aws-lc-LICENSE.
- jemalloc ${jemalloc} (tikv-jemalloc-sys ${jemalloc_sys%%+*}): jemalloc-COPYING.
- zstd ${zstd} (zstd-sys ${zstd_sys%%+*}): zstd-LICENSE. zstd is
  BSD-3-Clause OR GPL-2.0, and this is its BSD licence.
- libsecp256k1 (secp256k1-sys ${secp256k1_sys}): secp256k1-COPYING.
  The crate's own CC0-1.0 licence covers only its Rust wrapper.

aws-lc-LICENSE and jemalloc-COPYING are copied from the build's own sources
when a bundle is built; a development build doesn't have them.
EOF
echo "committed/README.txt written (zingod ${version} at ${commit})"
