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
#   3. the GitHub attestation of the tarball, when `gh attestation` exists
#      (gh >= 2.49; it needs `gh auth login` or GH_TOKEN), pinned to the tag
#      and, when the commit is known, to that commit;
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

die() {
    echo "fetch-zebrad-linux: error: $*" >&2
    exit 1
}

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

if [ -z "${version}" ]; then
    [ -f "${repo_root}/ZEBRAD_VERSION" ] || die "no --version given and ${repo_root}/ZEBRAD_VERSION is missing"
    version="$(tr -d '[:space:]' <"${repo_root}/ZEBRAD_VERSION")"
fi
# ZEBRAD_COMMIT pins the commit of ZEBRAD_VERSION, so it only applies to it.
if [ -z "${commit}" ] && [ -f "${repo_root}/ZEBRAD_COMMIT" ] &&
    [ "${version}" = "$(tr -d '[:space:]' <"${repo_root}/ZEBRAD_VERSION" 2>/dev/null)" ]; then
    commit="$(tr -d '[:space:]' <"${repo_root}/ZEBRAD_COMMIT")"
fi
if [ -n "${commit}" ] && ! printf '%s\n' "${commit}" | grep -Eq '^[0-9a-f]{40}$'; then
    die "commit '${commit}' is not a full 40-character lowercase hex SHA"
fi
case "${version}" in
    v* | V*) die "version '${version}' must not start with 'v' (asset names are zebrad-<X.Y.Z>-<target>.tar.gz)" ;;
esac
# Plain semver, optionally with a pre-release suffix, so it is safe in URLs and patterns.
if ! printf '%s\n' "${version}" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$'; then
    die "version '${version}' is not of the form X.Y.Z"
fi

[ -n "${out}" ] || out="${repo_root}/src-tauri/binaries/zebra-launcher-zebrad-${target}"

for tool in curl cosign tar gzip sha256sum; do
    command -v "${tool}" >/dev/null 2>&1 || die "${tool} is required but not on PATH"
done

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
if command -v gh >/dev/null 2>&1 && gh attestation verify --help >/dev/null 2>&1; then
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
    echo "Skipping the GitHub attestation check: gh >= 2.49 with 'gh attestation' is not available"
fi

echo "Extracting zebrad"
mkdir "${work}/extract"
tar -xzf "${work}/${asset}" -C "${work}/extract" --no-same-owner zebrad ||
    die "${asset} does not contain zebrad at its top level"
if [ ! -f "${work}/extract/zebrad" ] || [ -L "${work}/extract/zebrad" ]; then
    die "zebrad in ${asset} is not a regular file"
fi

mkdir -p "$(dirname "${out}")"
# Install through a temporary file so a failed check never leaves a bad sidecar.
cp "${work}/extract/zebrad" "${out}.tmp"
chmod +x "${out}.tmp"

reported="$("${out}.tmp" --version 2>/dev/null | head -n 1)" || reported=""
case "${reported}" in
    "zebrad ${version}" | "zebrad ${version} "* | "zebrad ${version}+"*) ;;
    *)
        rm -f "${out}.tmp"
        die "binary reports '${reported}', expected 'zebrad ${version}'"
        ;;
esac
mv -f "${out}.tmp" "${out}"

echo "zebrad ${version} (${target}, commit ${commit:-not pinned}): cosign verified, sha256 ok, attestation ${attestation}, installed at ${out}"
