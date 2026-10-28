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
# Development builds: --ref builds from any branch, tag or full commit SHA of
# the Zebra repository instead (for example `--ref main`), the same way. The
# result is NOT a verified release: nothing pins the commit, and the
# `--version` smoke test only checks that the binary is zebrad. See
# docs/SIDECARS.md, "Development builds from a Zebra git ref".
#
# Usage: scripts/build-zebrad.sh [--version X.Y.Z] [--commit SHA] [--target TRIPLE]
#                                [--out PATH] [--src-dir DIR] [--fetch-only] [--dry-run]
#        scripts/build-zebrad.sh --ref REF [--target TRIPLE] [--out PATH]
#                                [--src-dir DIR] [--fetch-only] [--dry-run]

set -euo pipefail

readonly ZEBRA_GIT="https://github.com/ZcashFoundation/zebra"

die() {
    echo "build-zebrad: error: $*" >&2
    exit 1
}

# shellcheck source-path=SCRIPTDIR source=lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

usage() {
    cat <<'EOF'
Usage: build-zebrad.sh [--version X.Y.Z] [--commit SHA] [--target TRIPLE] [--out PATH]
                       [--src-dir DIR] [--fetch-only] [--dry-run]
       build-zebrad.sh --ref REF [--target TRIPLE] [--out PATH]
                       [--src-dir DIR] [--fetch-only] [--dry-run]

  --version     zebrad version without a leading "v" (default: ZEBRAD_VERSION)
  --commit      full 40-hex Zebra commit that tag v<version> must point to
                (default: ZEBRAD_COMMIT when the version is ZEBRAD_VERSION;
                required for any other version)
  --ref         DEVELOPMENT BUILD: build from this Zebra branch, tag or full
                40-hex commit instead of a verified release; not combinable
                with --version or --commit
  --target      Rust target triple (default: the host)
  --out         where to install the binary
                (default: src-tauri/binaries/zebra-launcher-zebrad-<target>)
  --src-dir     where Zebra is checked out; cloned at tag v<version> (or
                fetched at --ref) if missing, kept afterwards (default: a
                temporary directory, removed on exit). An existing checkout
                must be at the tag (or at what --ref names in it).
  --fetch-only  stop after checking out and checking the source (needs
                --src-dir)
  --dry-run     print the commands instead of running them (nothing is cloned)
EOF
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

version=""
commit=""
ref=""
target=""
out=""
src=""
fetch_only=false
dry_run=false

while [ $# -gt 0 ]; do
    case "$1" in
        --version) [ $# -ge 2 ] || die "--version needs a value"; version="$2"; shift 2 ;;
        --commit) [ $# -ge 2 ] || die "--commit needs a value"; commit="$2"; shift 2 ;;
        --ref) [ $# -ge 2 ] || die "--ref needs a value"; ref="$2"; shift 2 ;;
        --fetch-only) fetch_only=true; shift ;;
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

if [ -n "${ref}" ]; then
    # A development build: no version, and the commit is whatever the ref
    # names when it is fetched.
    [ -z "${version}" ] && [ -z "${commit}" ] ||
        die "--ref cannot be combined with --version or --commit"
    check_git_ref "${ref}"
    label="${ref}"
else
    [ -n "${version}" ] || version="$(read_zebrad_version "${repo_root}")"
    check_zebrad_version "${version}"
    tag="v${version}"

    # ZEBRAD_COMMIT pins the commit of ZEBRAD_VERSION, so it only applies to it.
    if [ -z "${commit}" ]; then
        pinned_version="$(read_pin "${repo_root}/ZEBRAD_VERSION")"
        [ "${version}" = "${pinned_version}" ] ||
            die "version ${version} is not ZEBRAD_VERSION (${pinned_version:-missing}); pass --commit"
        [ -f "${repo_root}/ZEBRAD_COMMIT" ] || die "${repo_root}/ZEBRAD_COMMIT is missing; pass --commit"
        commit="$(read_zebrad_commit "${repo_root}" "${version}")"
    fi
    check_commit "${commit}"
    label="${tag}"
fi
# Without --src-dir the checkout is a temporary directory, removed on exit.
[ "${fetch_only}" = false ] || [ -n "${src}" ] || die "--fetch-only needs --src-dir"

# Fetching needs no Rust toolchain.
host=""
[ "${fetch_only}" = true ] || host="$(host_triple)"
[ -n "${target}" ] || target="${host}"

exe=""
case "${target}" in *-windows-*) exe=".exe" ;; esac
[ -n "${out}" ] || out="${repo_root}/src-tauri/binaries/zebra-launcher-zebrad-${target}${exe}"

if [ -z "${src}" ]; then
    src="$(mktemp -d)/zebra"
    trap 'rm -rf "$(dirname "${src}")"' EXIT
else
    # Absolute, so --remap-path-prefix matches the paths rustc sees.
    src_parent="$(dirname "${src}")"
    [ -d "${src_parent}" ] || run mkdir -p "${src_parent}"
    if [ -d "${src_parent}" ]; then
        src="$(cd "${src_parent}" && pwd)/$(basename "${src}")"
    else
        # Dry run: the parent was not created, so only make the path absolute.
        case "${src}" in /*) ;; *) src="${PWD}/${src}" ;; esac
    fi
fi

if [ -n "${ref}" ]; then
    # 1. Fetch exactly the ref. There is no pinned commit to check against:
    #    the commit is recorded instead.
    if [ -d "${src}/.git" ]; then
        echo "Using the existing checkout in ${src}"
        named="$(git -C "${src}" rev-parse --verify -q "${ref}^{commit}")" ||
            die "${ref} does not name a commit in ${src}"
        head="$(git -C "${src}" rev-parse HEAD)"
        [ "${head}" = "${named}" ] || die "${src} is at ${head}, not at ${ref} (${named})"
    else
        run git init -q "${src}"
        run git -C "${src}" remote add origin "${ZEBRA_GIT}"
        # GitHub serves branches, tags and full commit SHAs to a shallow fetch.
        checkout_rev=FETCH_HEAD
        if run git -C "${src}" fetch --depth 1 origin "${ref}"; then
            :
        elif is_full_sha "${ref}"; then
            # A server may refuse to send a commit by SHA; a full fetch of the
            # branches and tags has it if any of them reaches it.
            echo "The shallow fetch of ${ref} was refused; fetching all branches and tags"
            run git -C "${src}" fetch --tags origin '+refs/heads/*:refs/remotes/origin/*'
            checkout_rev="${ref}"
        else
            die "cannot fetch ${ref} from ${ZEBRA_GIT}"
        fi
        run git -C "${src}" -c advice.detachedHead=false checkout --detach "${checkout_rev}"
    fi
    if [ -d "${src}/.git" ]; then
        commit="$(git -C "${src}" rev-parse HEAD)"
        if is_full_sha "${ref}" && [ "${commit}" != "${ref}" ]; then
            die "${src} is at ${commit}, not at ${ref}"
        fi
    else
        commit="<commit of ${ref}>"
    fi
    dev_build="DEV BUILD: zebrad from ${ref} @ ${commit}, not a verified release"
    echo "################################################################################"
    echo "${dev_build}"
    echo "################################################################################"
else
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
fi

if [ "${fetch_only}" = true ]; then
    echo "${label} is checked out in ${src} (--fetch-only: not building)"
    exit 0
fi

# 2. Use the toolchain Zebra pins for this release.
if [ -f "${src}/rust-toolchain.toml" ]; then
    toolchain="$(sed -n 's/^[[:space:]]*channel[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "${src}/rust-toolchain.toml")"
    [ -n "${toolchain}" ] || die "no channel in ${src}/rust-toolchain.toml"
    source_date_epoch="$(git -C "${src}" log -1 --pretty=%ct)"
elif [ "${dry_run}" = true ]; then
    toolchain="<channel from ${src}/rust-toolchain.toml>"
    source_date_epoch="<commit time of ${label}>"
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
# The flags are separated by 0x1f, not spaces, so paths with spaces survive.
export CARGO_ENCODED_RUSTFLAGS="--remap-path-prefix=${src}=/zebra"$'\x1f'"--remap-path-prefix=${CARGO_HOME:-${HOME}/.cargo}=/cargo"
export CARGO_PROFILE_RELEASE_STRIP=symbols
echo "+ export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH}"
printf '+ export CARGO_ENCODED_RUSTFLAGS=%q\n' "${CARGO_ENCODED_RUSTFLAGS}"
echo "+ export CARGO_PROFILE_RELEASE_STRIP=symbols"
run cargo build --manifest-path "${src}/Cargo.toml" --target-dir "${src}/target" \
    --locked --release -p zebrad --bin zebrad \
    --features default-release-binaries --target "${target}"

# 4. Install through ${out}.tmp, smoke-testing it first when the host can run
#    the target. Cargo already stripped symbols (CARGO_PROFILE_RELEASE_STRIP).
built="${src}/target/${target}/release/zebrad${exe}"
if [ -n "${ref}" ]; then
    # A development build has no expected version: the smoke test only checks
    # that the binary runs and is zebrad, and records what it reports.
    if [ "${dry_run}" = true ]; then
        echo "+ install ${built} at ${out} through ${out}.tmp"
        echo "+ ${out} --version  # must print 'zebrad <anything>' (not compared with ZEBRAD_VERSION)"
        echo "dry run: zebrad from ${ref} for ${target} would be installed at ${out}"
        echo "${dev_build}"
        exit 0
    fi
    echo "+ install ${built} at ${out}"
    install_atomic "${built}" "${out}"
    if [ "${target}" = "${host}" ]; then
        version_string="$(zebrad_version_line "${out}")"
    else
        echo "Skipping the --version smoke test: ${target} does not run on ${host}"
        version_string="unknown (not run: ${target} does not run on ${host})"
    fi
    echo "zebrad (${target}): built from ${ref} (${commit}) with ${toolchain}, installed at ${out}"
    echo "zebrad --version: ${version_string}"
    echo "################################################################################"
    echo "${dev_build}"
    echo "################################################################################"
    exit 0
fi
if [ "${dry_run}" = true ]; then
    echo "+ install ${built} at ${out} through ${out}.tmp"
    echo "+ ${out}.tmp --version  # must print 'zebrad ${version}'"
    echo "dry run: zebrad ${version} for ${target} would be installed at ${out}"
    exit 0
fi
check_version="${version}"
if [ "${target}" != "${host}" ]; then
    echo "Skipping the --version smoke test: ${target} does not run on ${host}"
    check_version=""
fi
echo "+ install ${built} at ${out}"
install_atomic "${built}" "${out}" "${check_version}"

echo "zebrad ${version} (${target}): built from ${tag} (${commit}) with ${toolchain}, installed at ${out}"
