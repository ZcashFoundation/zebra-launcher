#!/usr/bin/env bash
# Builds zebrad from a Zebra release tag and installs it as the Tauri sidecar.
#
# Zebra only publishes Linux binaries (its ADR 0008), so the macOS sidecars are
# built from source with this script (Windows: scripts/build-zebrad.ps1). The
# build mirrors Zebra's own zfnd-release-binaries.yml: Zebra's pinned toolchain,
# `--locked`, the `default-release-binaries` features, SOURCE_DATE_EPOCH from
# the tag's commit, build paths remapped out of the binary, symbols stripped.
#
# Build dependencies (RocksDB and the zcash FFI are compiled and linked
# statically, and some crates generate protobuf code):
#   macOS:  Xcode command line tools (`xcode-select --install`: clang, libclang,
#           strip) and `brew install protobuf cmake`.
#   Linux:  `apt-get install clang libclang-dev cmake protobuf-compiler`.
#   Both:   rustup (the toolchain named in Zebra's rust-toolchain.toml is
#           installed on demand), git.
# A release build takes 15-40 minutes and several GB of disk.
#
# The tag must resolve to the pinned commit (ZEBRAD_COMMIT, or --commit), so a
# moved tag cannot change what gets built.
#
# Usage: scripts/build-zebrad.sh [--version X.Y.Z] [--commit SHA] [--target TRIPLE]
#                                [--out PATH] [--src-dir DIR] [--dry-run]

set -euo pipefail

readonly ZEBRA_GIT="https://github.com/ZcashFoundation/zebra"

die() {
    echo "build-zebrad: error: $*" >&2
    exit 1
}

usage() {
    cat <<'EOF'
Usage: build-zebrad.sh [--version X.Y.Z] [--commit SHA] [--target TRIPLE] [--out PATH]
                       [--src-dir DIR] [--dry-run]

  --version  zebrad version without a leading "v" (default: ZEBRAD_VERSION)
  --commit   full 40-hex Zebra commit that tag v<version> must point to
             (default: ZEBRAD_COMMIT when the version is ZEBRAD_VERSION;
             required for any other version)
  --target   Rust target triple (default: the host)
  --out      where to install the binary
             (default: src-tauri/binaries/zebra-launcher-zebrad-<target>)
  --src-dir  where Zebra is checked out; cloned at tag v<version> if missing,
             kept afterwards (default: a temporary directory, removed on exit)
  --dry-run  print the commands instead of running them (nothing is cloned)
EOF
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

version=""
commit=""
target=""
out=""
src=""
dry_run=false

while [ $# -gt 0 ]; do
    case "$1" in
        --version) [ $# -ge 2 ] || die "--version needs a value"; version="$2"; shift 2 ;;
        --commit) [ $# -ge 2 ] || die "--commit needs a value"; commit="$2"; shift 2 ;;
        --target) [ $# -ge 2 ] || die "--target needs a value"; target="$2"; shift 2 ;;
        --out) [ $# -ge 2 ] || die "--out needs a value"; out="$2"; shift 2 ;;
        --src-dir) [ $# -ge 2 ] || die "--src-dir needs a value"; src="$2"; shift 2 ;;
        --dry-run) dry_run=true; shift ;;
        -h | --help) usage; exit 0 ;;
        *) usage >&2; die "unknown argument: $1" ;;
    esac
done

# Prints a command, then runs it unless this is a dry run.
run() {
    printf '+'
    printf ' %q' "$@"
    printf '\n'
    if [ "${dry_run}" = false ]; then
        "$@"
    fi
}

if [ -z "${version}" ]; then
    [ -f "${repo_root}/ZEBRAD_VERSION" ] || die "no --version given and ${repo_root}/ZEBRAD_VERSION is missing"
    version="$(tr -d '[:space:]' <"${repo_root}/ZEBRAD_VERSION")"
fi
case "${version}" in
    v* | V*) die "version '${version}' must not start with 'v'" ;;
esac
if ! printf '%s\n' "${version}" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$'; then
    die "version '${version}' is not of the form X.Y.Z"
fi
tag="v${version}"

# ZEBRAD_COMMIT pins the commit of ZEBRAD_VERSION, so it only applies to it.
if [ -z "${commit}" ]; then
    pinned_version="$(tr -d '[:space:]' <"${repo_root}/ZEBRAD_VERSION" 2>/dev/null)" || pinned_version=""
    [ "${version}" = "${pinned_version}" ] ||
        die "version ${version} is not ZEBRAD_VERSION (${pinned_version:-missing}); pass --commit"
    [ -f "${repo_root}/ZEBRAD_COMMIT" ] || die "${repo_root}/ZEBRAD_COMMIT is missing; pass --commit"
    commit="$(tr -d '[:space:]' <"${repo_root}/ZEBRAD_COMMIT")"
fi
if ! printf '%s\n' "${commit}" | grep -Eq '^[0-9a-f]{40}$'; then
    die "commit '${commit}' is not a full 40-character lowercase hex SHA"
fi

command -v rustc >/dev/null 2>&1 || die "rustup/rustc is required"
host="$(rustc -vV | sed -n 's/^host: //p')"
[ -n "${target}" ] || target="${host}"

exe=""
case "${target}" in *-windows-*) exe=".exe" ;; esac
[ -n "${out}" ] || out="${repo_root}/src-tauri/binaries/zebra-launcher-zebrad-${target}${exe}"

if [ -z "${src}" ]; then
    src="$(mktemp -d)/zebra"
    trap 'rm -rf "$(dirname "${src}")"' EXIT
else
    # Absolute, so --remap-path-prefix matches the paths rustc sees.
    mkdir -p "$(dirname "${src}")"
    src="$(cd "$(dirname "${src}")" && pwd)/$(basename "${src}")"
fi

# 1. Check out the release tag, and check that it is the pinned commit.
if [ -d "${src}/.git" ]; then
    echo "Using the existing checkout in ${src}"
else
    run git -c advice.detachedHead=false clone --depth 1 --branch "${tag}" "${ZEBRA_GIT}" "${src}"
fi
if [ -d "${src}/.git" ]; then
    head="$(git -C "${src}" rev-parse HEAD)"
    tag_commit="$(git -C "${src}" rev-parse "refs/tags/${tag}^{commit}" 2>/dev/null)" ||
        die "${src} has no tag ${tag}"
    [ "${head}" = "${tag_commit}" ] || die "${src} is at ${head}, not at ${tag} (${tag_commit})"
    [ "${tag_commit}" = "${commit}" ] ||
        die "${tag} is commit ${tag_commit}, but the pinned commit is ${commit}"
    echo "${tag} is the pinned commit ${commit}"
else
    echo "+ check that ${tag} and HEAD are the pinned commit ${commit}"
fi

# 2. Use the toolchain Zebra pins for this release.
if [ -f "${src}/rust-toolchain.toml" ]; then
    toolchain="$(sed -n 's/^[[:space:]]*channel[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "${src}/rust-toolchain.toml")"
    [ -n "${toolchain}" ] || die "no channel in ${src}/rust-toolchain.toml"
    source_date_epoch="$(git -C "${src}" log -1 --pretty=%ct)"
elif [ "${dry_run}" = true ]; then
    toolchain="<channel from ${src}/rust-toolchain.toml>"
    source_date_epoch="<commit time of ${tag}>"
else
    die "${src}/rust-toolchain.toml is missing"
fi
export RUSTUP_TOOLCHAIN="${toolchain}"
echo "+ export RUSTUP_TOOLCHAIN=${toolchain}"
if [ "${dry_run}" = true ] || ! rustup run "${toolchain}" rustc --version >/dev/null 2>&1; then
    run rustup toolchain install "${toolchain}" --profile minimal
fi
run rustup target add --toolchain "${toolchain}" "${target}"

# 3. Build like Zebra's release workflow does.
export SOURCE_DATE_EPOCH="${source_date_epoch}"
export RUSTFLAGS="--remap-path-prefix=${src}=/zebra --remap-path-prefix=${CARGO_HOME:-${HOME}/.cargo}=/cargo"
export CARGO_PROFILE_RELEASE_STRIP=symbols
echo "+ export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH}"
echo "+ export RUSTFLAGS=\"${RUSTFLAGS}\""
echo "+ export CARGO_PROFILE_RELEASE_STRIP=symbols"
run cargo build --manifest-path "${src}/Cargo.toml" --target-dir "${src}/target" \
    --locked --release -p zebrad --bin zebrad \
    --features default-release-binaries --target "${target}"

# 4. Install. Cargo already stripped symbols (CARGO_PROFILE_RELEASE_STRIP).
built="${src}/target/${target}/release/zebrad${exe}"
run mkdir -p "$(dirname "${out}")"
run cp "${built}" "${out}.tmp"
run chmod +x "${out}.tmp"

# 5. Smoke test, when the host can run the target.
if [ "${dry_run}" = true ]; then
    echo "+ ${out}.tmp --version  # must print 'zebrad ${version}'"
    run mv -f "${out}.tmp" "${out}"
    echo "dry run: zebrad ${version} for ${target} would be installed at ${out}"
    exit 0
fi
if [ "${target}" = "${host}" ]; then
    reported="$("${out}.tmp" --version 2>/dev/null | head -n 1)" || reported=""
    case "${reported}" in
        "zebrad ${version}" | "zebrad ${version} "* | "zebrad ${version}+"*) ;;
        *)
            rm -f "${out}.tmp"
            die "binary reports '${reported}', expected 'zebrad ${version}'"
            ;;
    esac
else
    echo "Skipping the --version smoke test: ${target} does not run on ${host}"
fi
mv -f "${out}.tmp" "${out}"

echo "zebrad ${version} (${target}): built from ${tag} (${commit}) with ${toolchain}, installed at ${out}"
