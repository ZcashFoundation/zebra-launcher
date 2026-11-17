#!/usr/bin/env bash
# Builds zingod from its pinned commit and installs it as the Linux sidecar
# zeal-zingod, with the licence files and build information Zeal ships beside
# it. zingod publishes no binaries, so Zeal builds every zingod it bundles
# (docs/SIDECARS.md, "The bundled zingod"):
#
#   1. The tag v<ZINGOD_VERSION> must resolve, through `git ls-remote` (the
#      peeled line of an annotated tag, else the tag's own), to ZINGOD_COMMIT
#      or --commit. The source is fetched at that commit and HEAD is checked.
#   2. The toolchain is the one in the checkout's rust-toolchain.toml, through
#      RUSTUP_TOOLCHAIN.
#   3. `cargo fetch --locked` runs first, so a git source that has gone fails
#      early, by name. The committed licence folder must then equal what
#      scripts/zingod-licenses.sh gathers from the checkout and its sources.
#   4. `cargo build --locked --release -p zingod --bin zingod`, with
#      SOURCE_DATE_EPOCH from the commit, symbols stripped, AWS-LC never taken
#      from the host (AWS_LC_SYS_USE_SYSTEM=0), and the checkout, CARGO_HOME,
#      RUSTUP_HOME and CARGO_TARGET_DIR remapped out of the binary.
#   5. Checks, and nothing is installed unless all pass: `--version` prints
#      `zingod <ZINGOD_VERSION>`; `generate-config` writes a file, offline;
#      scripts/check-glibc.sh passes at GLIBC_FLOOR (.github/tool-versions.env);
#      ldd names only libc, libm, libgcc_s, the loader and the vdso; and no
#      string in the binary names /home/, /root/, /runner/, /github/ or /__w/.
#      The build never runs `start`, which would contact a validator.
#   6. Installs the binary at --out and, in the same folder, licenses/ with
#      the two licence files that aren't committed (jemalloc-COPYING and
#      aws-lc-LICENSE) and zingod-build-info.txt, which records their sha256.
#
# With --vendor-archive FILE it also runs `cargo vendor --locked` in a copy of
# the checkout and packs it as FILE, which must be named
# zingod-src-<ZINGOD_VERSION>.tar.zst, and writes FILE.sha256: what a rebuild
# needs if a git source disappears.
#
# The checks run the binary, so the target is the host's own Linux triple.
# Build in ubuntu:22.04, as CI does: on a newer system the binary needs a newer
# glibc and fails the floor, so nothing is installed.
#
# Build dependencies (AWS-LC, jemalloc, zstd and libsecp256k1 are compiled and
# linked statically): a C compiler, make and sh (build-essential); protoc with
# the well-known types (protobuf-compiler and libprotobuf-dev); git; rustup;
# binutils (objdump, strings); zstd for --vendor-archive. No cmake, libclang or
# OpenSSL. A clean build takes about a minute on 32 threads.
#
# Usage: scripts/build-zingod.sh [--commit SHA] [--src-dir DIR] [--target TRIPLE]
#                                [--out PATH] [--vendor-archive FILE] [--dry-run]

set -euo pipefail

readonly ZINGOD_GIT="https://github.com/zingolabs/zingod"

die() {
    echo "build-zingod: error: $*" >&2
    exit 1
}

# shellcheck source-path=SCRIPTDIR source=lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

usage() {
    cat <<'EOF'
Usage: build-zingod.sh [--commit SHA] [--src-dir DIR] [--target TRIPLE] [--out PATH]
                       [--vendor-archive FILE] [--dry-run]

  --commit          full 40-hex zingod commit that tag v<ZINGOD_VERSION> must
                    point to (default: ZINGOD_COMMIT)
  --src-dir         where zingod is checked out; fetched at the commit if
                    missing, kept afterwards (default: a temporary directory,
                    removed on exit). An existing checkout must be at the
                    commit, with no changes to tracked files.
  --target          the host's Linux target triple (default: the host)
  --out             where to install the binary
                    (default: src-tauri/binaries/zeal-zingod-<target>)
  --vendor-archive  also write the vendored source to this
                    zingod-src-<version>.tar.zst, with a .sha256 beside it
  --dry-run         print the commands instead of running them (nothing is
                    fetched or built)
EOF
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

commit=""
target=""
out=""
src=""
archive=""
dry_run=false

while [ $# -gt 0 ]; do
    case "$1" in
        --commit) [ $# -ge 2 ] || die "--commit needs a value"; commit="$2"; shift 2 ;;
        --target) [ $# -ge 2 ] || die "--target needs a value"; target="$2"; shift 2 ;;
        --out) [ $# -ge 2 ] || die "--out needs a value"; out="$2"; shift 2 ;;
        --src-dir) [ $# -ge 2 ] || die "--src-dir needs a value"; src="$2"; shift 2 ;;
        --vendor-archive) [ $# -ge 2 ] || die "--vendor-archive needs a value"; archive="$2"; shift 2 ;;
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

# absolute PATH: prints PATH made absolute against the current directory.
absolute() {
    case "$1" in
        /*) printf '%s\n' "$1" ;;
        *) printf '%s\n' "${PWD}/$1" ;;
    esac
}

version="$(read_pin "${repo_root}/ZINGOD_VERSION")"
[ -n "${version}" ] || die "${repo_root}/ZINGOD_VERSION is missing or empty"
check_stable_version "${version}"
tag="v${version}"
[ -n "${commit}" ] || commit="$(read_pin "${repo_root}/ZINGOD_COMMIT")"
[ -n "${commit}" ] || die "${repo_root}/ZINGOD_COMMIT is missing or empty; pass --commit"
check_commit "${commit}"

# From uname, not rustc: no toolchain need be installed before zingod's own.
case "$(uname -s)-$(uname -m)" in
    Linux-x86_64) host=x86_64-unknown-linux-gnu ;;
    Linux-aarch64) host=aarch64-unknown-linux-gnu ;;
    *) die "zingod builds on x86_64 and aarch64 Linux hosts only, not on $(uname -s) $(uname -m)" ;;
esac
[ -n "${target}" ] || target="${host}"
case "${target}" in
    *-unknown-linux-gnu) ;;
    *) die "zingod is bundled on Linux only, not for ${target}" ;;
esac
[ "${target}" = "${host}" ] ||
    die "the checks run the binary, so build ${target} on a ${target} host, not on ${host}"
[ -n "${out}" ] || out="${repo_root}/src-tauri/binaries/zeal-zingod-${target}"
out="$(absolute "${out}")"
out_dir="$(dirname "${out}")"
if [ -n "${archive}" ]; then
    [ "$(basename "${archive}")" = "zingod-src-${version}.tar.zst" ] ||
        die "--vendor-archive must be named zingod-src-${version}.tar.zst"
    archive="$(absolute "${archive}")"
fi
floor="$(sed -n 's/^GLIBC_FLOOR=//p' "${repo_root}/.github/tool-versions.env" | tr -d '\r')"
[ -n "${floor}" ] || die "no GLIBC_FLOOR in ${repo_root}/.github/tool-versions.env"

if [ "${dry_run}" = false ]; then
    tools=(git rustup cc make protoc objdump strings ldd)
    [ -z "${archive}" ] || tools+=(tar zstd)
    for tool in "${tools[@]}"; do
        command -v "${tool}" >/dev/null 2>&1 || die "${tool} is required (see the top of this script)"
    done
fi

# One scratch folder for everything that isn't installed; the checkout too,
# unless --src-dir names one.
work="$(mktemp -d)"
trap 'rm -rf "${work}"' EXIT
if [ -z "${src}" ]; then
    src="${work}/zingod"
else
    # Absolute, so --remap-path-prefix matches the paths rustc sees.
    src="$(absolute "${src}")"
fi

# 1. The tag must name the pinned commit; then fetch exactly that commit.
echo "+ git ls-remote ${ZINGOD_GIT} refs/tags/${tag} refs/tags/${tag}^{}"
if [ "${dry_run}" = true ]; then
    echo "+ check that ${tag} is the pinned commit ${commit}"
else
    refs="$(git ls-remote "${ZINGOD_GIT}" "refs/tags/${tag}" "refs/tags/${tag}^{}")" ||
        die "cannot list the tags of ${ZINGOD_GIT}"
    tag_commit=""
    peeled=""
    while read -r sha ref; do
        case "${ref}" in
            "refs/tags/${tag}^{}") peeled="${sha}" ;;
            "refs/tags/${tag}") tag_commit="${sha}" ;;
        esac
    done <<<"${refs}"
    # An annotated tag's own line names the tag object; its peeled line names
    # the commit.
    [ -z "${peeled}" ] || tag_commit="${peeled}"
    [ -n "${tag_commit}" ] || die "${ZINGOD_GIT} has no tag ${tag}"
    [ "${tag_commit}" = "${commit}" ] ||
        die "${tag} is commit ${tag_commit}, but the pinned commit is ${commit}"
    echo "${tag} is the pinned commit ${commit}"
fi
if [ -d "${src}/.git" ]; then
    echo "Using the existing checkout in ${src}"
else
    run git init -q "${src}"
    run git -C "${src}" fetch -q --depth 1 "${ZINGOD_GIT}" "${commit}"
    run git -C "${src}" -c advice.detachedHead=false checkout -q --detach FETCH_HEAD
fi
if [ "${dry_run}" = false ]; then
    head="$(git -C "${src}" rev-parse HEAD)"
    [ "${head}" = "${commit}" ] || die "${src} is at ${head}, not at the pinned commit ${commit}"
    [ -z "$(git -C "${src}" status --porcelain --untracked-files=no)" ] ||
        die "${src} has changes to tracked files"
fi

# 2. The toolchain zingod pins.
if [ -f "${src}/rust-toolchain.toml" ]; then
    toolchain="$(sed -n 's/^[[:space:]]*channel[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "${src}/rust-toolchain.toml")"
    [ -n "${toolchain}" ] || die "no channel in ${src}/rust-toolchain.toml"
    source_date_epoch="$(git -C "${src}" log -1 --pretty=%ct)"
elif [ "${dry_run}" = true ]; then
    toolchain="<channel from ${src}/rust-toolchain.toml>"
    source_date_epoch="<commit time of ${commit}>"
else
    die "${src}/rust-toolchain.toml is missing"
fi
export RUSTUP_TOOLCHAIN="${toolchain}"
echo "+ export RUSTUP_TOOLCHAIN=${toolchain}"
if [ "${dry_run}" = true ] || ! rustup run "${toolchain}" rustc --version >/dev/null 2>&1; then
    run rustup toolchain install "${toolchain}" --profile minimal
fi

# 3. Fetch every dependency, then check the committed licence folder.
cargo_home="$(absolute "${CARGO_HOME:-${HOME}/.cargo}")"
rustup_home="$(absolute "${RUSTUP_HOME:-${HOME}/.rustup}")"
target_dir="$(absolute "${CARGO_TARGET_DIR:-${src}/target}")"
export CARGO_TARGET_DIR="${target_dir}"
echo "+ export CARGO_TARGET_DIR=${target_dir}"
run cargo fetch --manifest-path "${src}/Cargo.toml" --locked

licenses="${work}/licenses"
committed="${repo_root}/src-tauri/resources/licenses/zingod"
run "${repo_root}/scripts/zingod-licenses.sh" --src-dir "${src}" --out "${licenses}"
if [ "${dry_run}" = true ]; then
    echo "+ compare ${committed} with ${licenses}/committed"
else
    for file in "${licenses}/committed/"*; do
        name="$(basename "${file}")"
        cmp -s "${file}" "${committed}/${name}" ||
            die "${committed}/${name} is not what zingod ${version} gives; refresh the folder with scripts/bump-zingod.sh"
    done
    for file in "${committed}/"*; do
        name="$(basename "${file}")"
        [ -f "${licenses}/committed/${name}" ] || [ -f "${licenses}/copied/${name}" ] ||
            die "${committed}/${name} is not one of zingod's licence files"
    done
    echo "${committed} matches the checkout and its sources"
fi

# 4. Build. The flags are separated by 0x1f, not spaces, so paths with spaces
#    survive. rustc applies the last prefix that matches, so the target folder,
#    by default inside the checkout, comes last.
export SOURCE_DATE_EPOCH="${source_date_epoch}"
export CARGO_ENCODED_RUSTFLAGS="--remap-path-prefix=${src}=/zingod"$'\x1f'"--remap-path-prefix=${cargo_home}=/cargo"$'\x1f'"--remap-path-prefix=${rustup_home}=/rustup"$'\x1f'"--remap-path-prefix=${target_dir}=/target"
export CARGO_PROFILE_RELEASE_STRIP=symbols
export AWS_LC_SYS_USE_SYSTEM=0
echo "+ export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH}"
printf '+ export CARGO_ENCODED_RUSTFLAGS=%q\n' "${CARGO_ENCODED_RUSTFLAGS}"
echo "+ export CARGO_PROFILE_RELEASE_STRIP=symbols AWS_LC_SYS_USE_SYSTEM=0"
run cargo build --manifest-path "${src}/Cargo.toml" --locked --release \
    -p zingod --bin zingod --target "${target}"
built="${target_dir}/${target}/release/zingod"

if [ "${dry_run}" = true ]; then
    echo "+ check ${built}: --version, generate-config, the glibc floor ${floor}, ldd, strings"
    [ -z "${archive}" ] || echo "+ cargo vendor --locked in a copy of ${src}, packed as ${archive}"
    echo "dry run: zingod ${version} for ${target} would be installed at ${out}"
    exit 0
fi

# 5. Check a copy of the binary; nothing is installed unless every check passes.
staged="${work}/zeal-zingod-${target}"
cp "${built}" "${staged}"
check_zingod_version "${staged}" "${version}"
echo "${staged}: $("${staged}" --version)"

mkdir -p "${work}/home"
HOME="${work}/home" XDG_CONFIG_HOME="${work}/home" \
    "${staged}" generate-config -o "${work}/zingod.toml" >/dev/null ||
    die "zingod generate-config failed"
[ -s "${work}/zingod.toml" ] || die "zingod generate-config wrote nothing"
echo "zingod generate-config: $(wc -l <"${work}/zingod.toml") lines"

"${repo_root}/scripts/check-glibc.sh" --floor "${floor}" "${staged}" ||
    die "zeal-zingod needs a glibc above ${floor}: build it in ubuntu:22.04"

libraries="$(ldd "${staged}")" || die "ldd cannot read ${staged}"
unexpected=()
while read -r library _; do
    case "${library##*/}" in
        linux-vdso.so.1 | libc.so.6 | libm.so.6 | libgcc_s.so.1 | ld-linux-x86-64.so.2 | ld-linux-aarch64.so.1) ;;
        *) unexpected+=("${library}") ;;
    esac
done <<<"${libraries}"
[ "${#unexpected[@]}" -eq 0 ] ||
    die "zeal-zingod links more than libc, libm, libgcc_s and the loader: ${unexpected[*]}"
echo "ldd: only libc, libm, libgcc_s, the loader and the vdso"

leaks="$(strings -a "${staged}" | grep -E '/home/|/root/|/runner/|/github/|/__w/' | head -n 20 || true)"
[ -z "${leaks}" ] || die "zeal-zingod holds paths of the build machine:
${leaks}"
echo "strings: no path of the build machine"

# The vendored source, before anything is installed.
if [ -n "${archive}" ]; then
    name="zingod-src-${version}"
    stage="${work}/vendor/${name}"
    mkdir -p "${stage}/.cargo"
    git -C "${src}" archive --format=tar HEAD | tar -x -C "${stage}"
    # Relative, so the printed source replacement names no folder of this machine.
    echo "+ cargo vendor --locked --offline vendor (in ${stage})"
    (cd "${stage}" && cargo vendor --locked --offline vendor) >>"${stage}/.cargo/config.toml"
    mkdir -p "$(dirname "${archive}")"
    (cd "${work}/vendor" && tar --sort=name --mtime="@${SOURCE_DATE_EPOCH}" --owner=0 --group=0 \
        --numeric-owner --format=gnu -cf - "${name}") | zstd -q -19 -T0 -f -o "${archive}.tmp"
    chmod 0644 "${archive}.tmp"
    mv -f "${archive}.tmp" "${archive}"
    printf '%s  %s\n' "$(sha256_of "${archive}")" "$(basename "${archive}")" >"${archive}.sha256"
    echo "${archive}: $(du -h "${archive}" | cut -f1), sha256 $(cut -d' ' -f1 <"${archive}.sha256")"
fi

# 6. Install the binary, the copied licence files and the build information.
install_atomic "${staged}" "${out}"
mkdir -p "${out_dir}/licenses"
for file in "${licenses}/copied/"*; do
    cp "${file}" "${out_dir}/licenses/"
done
os="$(sed -n 's/^PRETTY_NAME="\(.*\)"$/\1/p' /etc/os-release 2>/dev/null || true)"
{
    echo "zingod built by Zeal from its pinned commit; Zingo Labs publishes no binaries."
    echo
    echo "zingod_version=${version}"
    echo "zingod_commit=${commit}"
    echo "zingod_source=${ZINGOD_GIT}"
    echo "target=${target}"
    echo "toolchain=${toolchain}"
    echo "rustc=$(rustc --version)"
    echo "protoc=$(protoc --version)"
    echo "os=${os:-unknown}"
    echo "source_date_epoch=${SOURCE_DATE_EPOCH}"
    echo "binary_sha256=$(sha256_of "${out}")"
    for file in "${licenses}/copied/"*; do
        echo "license_sha256_$(basename "${file}")=$(sha256_of "${file}")"
    done
    if [ -n "${archive}" ]; then
        echo "source_archive=$(basename "${archive}")"
        echo "source_archive_sha256=$(cut -d' ' -f1 <"${archive}.sha256")"
    fi
    if [ -n "${GITHUB_RUN_ID:-}" ]; then
        echo "workflow_run=${GITHUB_SERVER_URL:-https://github.com}/${GITHUB_REPOSITORY:-}/actions/runs/${GITHUB_RUN_ID}"
        echo "launcher_commit=${GITHUB_SHA:-}"
    fi
} >"${out_dir}/zingod-build-info.txt"

echo "zingod ${version} (${target}): built from ${tag} (${commit}) with ${toolchain}, installed at ${out}"
echo "licences: ${out_dir}/licenses/; build information: ${out_dir}/zingod-build-info.txt"
