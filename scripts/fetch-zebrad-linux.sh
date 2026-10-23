#!/usr/bin/env bash
# Downloads a released Linux zebrad, verifies it, and installs it as the Tauri
# sidecar.
#
# Zebra publishes, per release (see Zebra's ADR 0008 and its
# zfnd-release-binaries.yml workflow):
#   zebrad-<ver>-<target>.tar.gz   the binary plus licenses and README
#   SHA256SUMS                     checksums of every tarball
#   SHA256SUMS.sigstore.json       keyless cosign signature over SHA256SUMS
#   and a GitHub build-provenance attestation per tarball.
#
# This script checks, in order:
#   1. the cosign signature on SHA256SUMS, pinned to Zebra's release workflow
#      at the release tag and, when the commit is known, at that commit;
#   2. the tarball's checksum against the signed SHA256SUMS;
#   3. the GitHub attestation of the tarball, when gh is on PATH, pinned to
#      the tag and, when the commit is known, to that commit. This needs
#      gh >= 2.68.0, the first release with `gh attestation verify
#      --source-ref/--source-digest`, and `gh auth login` or GH_TOKEN. An older
#      gh is an error, not a reason to skip the check;
#   4. that the extracted binary reports the requested version.
#
# Requires: bash, curl, cosign, tar, gzip, sha256sum; optionally gh.
#
# Usage: scripts/fetch-zebrad-linux.sh [--target TRIPLE] [--version X.Y.Z] [--commit SHA]
#                                      [--out PATH] [--from-dir DIR]

set -euo pipefail

readonly REPO="ZcashFoundation/zebra"
readonly SIGNER_WORKFLOW="${REPO}/.github/workflows/zfnd-release-binaries.yml"
readonly OIDC_ISSUER="https://token.actions.githubusercontent.com"
# The first gh with `gh attestation verify --source-ref/--source-digest`.
readonly MIN_GH_VERSION="2.68.0"

die() {
    echo "fetch-zebrad-linux: error: $*" >&2
    exit 1
}

# shellcheck source-path=SCRIPTDIR source=lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

usage() {
    cat <<'EOF'
Usage: fetch-zebrad-linux.sh [--target TRIPLE] [--version X.Y.Z] [--commit SHA] [--out PATH]
                             [--from-dir DIR]

  --target    x86_64-unknown-linux-gnu or aarch64-unknown-linux-gnu
              (default: the host)
  --version   zebrad version without a leading "v" (default: ZEBRAD_VERSION)
  --commit    full 40-hex Zebra commit that tag v<version> points to; the
              signature and attestation must come from it (default:
              ZEBRAD_COMMIT when the version is ZEBRAD_VERSION; for any
              other version the commit is not pinned unless given)
  --out       where to install the binary
              (default: src-tauri/binaries/zebra-launcher-zebrad-<target>)
  --from-dir  verify and install the three release files from DIR instead of
              downloading them (the same checks apply)
EOF
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

target=""
version=""
commit=""
out=""
from_dir=""

while [ $# -gt 0 ]; do
    case "$1" in
        --target) [ $# -ge 2 ] || die "--target needs a value"; target="$2"; shift 2 ;;
        --version) [ $# -ge 2 ] || die "--version needs a value"; version="$2"; shift 2 ;;
        --commit) [ $# -ge 2 ] || die "--commit needs a value"; commit="$2"; shift 2 ;;
        --out) [ $# -ge 2 ] || die "--out needs a value"; out="$2"; shift 2 ;;
        --from-dir) [ $# -ge 2 ] || die "--from-dir needs a value"; from_dir="$2"; shift 2 ;;
        -h | --help) usage; exit 0 ;;
        *) usage >&2; die "unknown argument: $1" ;;
    esac
done

if [ -z "${target}" ]; then
    case "$(uname -m)" in
        x86_64 | amd64) target="x86_64-unknown-linux-gnu" ;;
        aarch64 | arm64) target="aarch64-unknown-linux-gnu" ;;
        *) die "unsupported host architecture $(uname -m); pass --target" ;;
    esac
fi
case "${target}" in
    x86_64-unknown-linux-gnu | aarch64-unknown-linux-gnu) ;;
    *) die "unsupported target ${target}; Zebra only publishes Linux x86_64 and aarch64 binaries" ;;
esac

[ -n "${version}" ] || version="$(read_zebrad_version "${repo_root}")"
# ZEBRAD_COMMIT pins the commit of ZEBRAD_VERSION, so it only applies to it.
[ -n "${commit}" ] || commit="$(read_zebrad_commit "${repo_root}" "${version}")"
[ -z "${commit}" ] || check_commit "${commit}"
check_zebrad_version "${version}" "(asset names are zebrad-<X.Y.Z>-<target>.tar.gz)"

[ -n "${out}" ] || out="${repo_root}/src-tauri/binaries/zebra-launcher-zebrad-${target}"

for tool in curl cosign tar gzip sha256sum; do
    command -v "${tool}" >/dev/null 2>&1 || die "${tool} is required but not on PATH"
done

# A gh on PATH must be able to pin the attestation to the tag and commit;
# checked before downloading anything. The help goes through a variable, as
# `gh ... | grep -q` could fail with SIGPIPE under pipefail.
use_gh=false
if command -v gh >/dev/null 2>&1; then
    gh_help="$(gh attestation verify --help 2>&1)" || gh_help=""
    if ! grep -q -- '--source-digest' <<<"${gh_help}"; then
        gh_version="$(gh --version 2>/dev/null)" || gh_version=""
        gh_version="${gh_version%%$'\n'*}"
        die "${gh_version:-gh} on PATH lacks 'gh attestation verify --source-digest'; the attestation check needs gh >= ${MIN_GH_VERSION}. Upgrade gh, or take it off PATH to skip the check"
    fi
    use_gh=true
fi

tag="v${version}"
asset="zebrad-${version}-${target}.tar.gz"
base_url="https://github.com/${REPO}/releases/download/${tag}"
cert_identity="https://github.com/${SIGNER_WORKFLOW}@refs/tags/${tag}"

work="$(mktemp -d)"
trap 'rm -rf "${work}"' EXIT

files=("${asset}" SHA256SUMS SHA256SUMS.sigstore.json)
if [ -n "${from_dir}" ]; then
    echo "Using ${files[*]} from ${from_dir}"
    for file in "${files[@]}"; do
        cp "${from_dir}/${file}" "${work}/${file}" || die "${from_dir}/${file} is missing"
    done
else
    echo "Downloading ${files[*]} from ${base_url}"
    for file in "${files[@]}"; do
        curl --proto '=https' --tlsv1.2 -fsSL --retry 3 -o "${work}/${file}" "${base_url}/${file}" ||
            die "download of ${base_url}/${file} failed"
    done
fi

echo "Verifying the cosign signature on SHA256SUMS"
echo "  certificate identity: ${cert_identity}"
cosign_args=(
    --bundle "${work}/SHA256SUMS.sigstore.json"
    --certificate-identity "${cert_identity}"
    --certificate-oidc-issuer "${OIDC_ISSUER}"
)
if [ -n "${commit}" ]; then
    echo "  workflow commit: ${commit}"
    cosign_args+=(--certificate-github-workflow-sha "${commit}")
fi
cosign verify-blob "${work}/SHA256SUMS" "${cosign_args[@]}" ||
    die "cosign could not verify SHA256SUMS; refusing to use this release"

echo "Checking ${asset} against SHA256SUMS"
# --ignore-missing passes when no line matches, so require the line first.
awk -v a="${asset}" '$2 == a || $2 == "*" a { found = 1 } END { exit !found }' \
    "${work}/SHA256SUMS" || die "SHA256SUMS has no entry for ${asset}"
(cd "${work}" && sha256sum -c --ignore-missing --strict SHA256SUMS) ||
    die "checksum mismatch for ${asset}"

attestation="skipped"
if [ "${use_gh}" = true ]; then
    echo "Verifying the GitHub build-provenance attestation of ${asset}"
    gh_args=(
        --repo "${REPO}"
        --signer-workflow "${SIGNER_WORKFLOW}"
        --source-ref "refs/tags/${tag}"
    )
    [ -z "${commit}" ] || gh_args+=(--source-digest "${commit}")
    gh attestation verify "${work}/${asset}" "${gh_args[@]}" ||
        die "gh attestation verify failed for ${asset}"
    attestation="verified"
else
    echo "Skipping the GitHub attestation check: gh is not on PATH (the check needs gh >= ${MIN_GH_VERSION})"
fi

echo "Extracting zebrad"
mkdir "${work}/extract"
tar -xzf "${work}/${asset}" -C "${work}/extract" --no-same-owner zebrad ||
    die "${asset} does not contain zebrad at its top level"
if [ ! -f "${work}/extract/zebrad" ] || [ -L "${work}/extract/zebrad" ]; then
    die "zebrad in ${asset} is not a regular file"
fi

install_atomic "${work}/extract/zebrad" "${out}" "${version}"

echo "zebrad ${version} (${target}, commit ${commit:-not pinned}): cosign verified, sha256 ok, attestation ${attestation}, installed at ${out}"
