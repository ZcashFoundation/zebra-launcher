#!/usr/bin/env bash
# Installs the pinned build tools of .github/tool-versions.env from their
# GitHub release binaries, each checked against its sha256 pin before it is
# unpacked: tauri-cli for a job that bundles the app (--bundler), and trunk,
# wasm-bindgen and wasm-opt (binaryen) for a job that compiles the UI
# (--compile). This is the tool-install step of the setup-tauri composite
# action (.github/actions/setup-tauri), kept as a script so shellcheck covers
# it and it can be dry-run locally. Nothing is compiled from crates.io, and
# nothing is installed without a recorded hash: a missing or wrong hash fails.
#
# tauri-cli and trunk go into $CARGO_HOME/bin, which is on PATH on the
# runners, so `cargo tauri` finds cargo-tauri. wasm-bindgen and the binaryen
# tree go into $RUNNER_TEMP/frontend-tools, put first on PATH for the rest of
# the job through GITHUB_PATH. trunk then runs offline (TRUNK_OFFLINE=true and
# TRUNK_TOOLS_WASM_OPT, written to GITHUB_ENV), so it cannot download its own
# wasm-bindgen or wasm-opt: it uses the pinned ones at exactly the versions it
# requires, or fails. WASM_BINDGEN_VERSION must equal the wasm-bindgen crate
# in Cargo.lock; that is checked before anything is downloaded.
#
# The pins are read from the file itself (KEY=value lines, as the action loads
# them into GITHUB_ENV), so the script does not depend on an earlier step.
#
# Usage: scripts/install-pinned-tools.sh [--compile BOOL] [--bundler BOOL]
#                                        [--host TRIPLE] [--pins FILE] [--dry-run]
#   --compile BOOL  install trunk, wasm-bindgen and wasm-opt (default: $COMPILE, else true)
#   --bundler BOOL  install tauri-cli (default: $BUNDLER, else true)
#   --host TRIPLE   the target triple to install for (default: rustc's host)
#   --pins FILE     the pins to read (default: this repository's .github/tool-versions.env)
#   --dry-run       print each download, its expected hash and where it would be
#                   installed, and the PATH and environment lines the job would
#                   get; fetch and write nothing
#
# Environment: COMPILE and BUNDLER, the composite action's inputs; RUNNER_TEMP,
# the work and tool directories (default: $TMPDIR, else /tmp); CARGO_HOME
# (default: ~/.cargo); GITHUB_PATH and GITHUB_ENV, appended to when set and
# printed otherwise, so a local run shows what the job would export.
#
# Requires: bash, curl, tar (and unzip or 7z for the Windows archives),
# sha256sum or shasum; rustc unless --host is given; cargo for --bundler.

set -euo pipefail

die() {
    if [ -n "${GITHUB_ACTIONS:-}" ]; then
        echo "::error::$*"
    fi
    echo "install-pinned-tools: error: $*" >&2
    exit 1
}

# shellcheck source-path=SCRIPTDIR source=lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
compile="${COMPILE:-true}"
bundler="${BUNDLER:-true}"
host=""
pins="${repo_root}/.github/tool-versions.env"
dry_run=false
while [ $# -gt 0 ]; do
    case "$1" in
        --compile) [ $# -ge 2 ] || die "--compile needs true or false"; compile="$2"; shift 2 ;;
        --bundler) [ $# -ge 2 ] || die "--bundler needs true or false"; bundler="$2"; shift 2 ;;
        --host) [ $# -ge 2 ] || die "--host needs a target triple"; host="$2"; shift 2 ;;
        --pins) [ $# -ge 2 ] || die "--pins needs a file"; pins="$2"; shift 2 ;;
        --dry-run) dry_run=true; shift ;;
        -h | --help) sed -n '2,/^$/{s/^# \{0,1\}//p;}' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) die "unknown argument: $1 (see --help)" ;;
    esac
done
for flag in compile bundler; do
    case "${!flag}" in
        true | false) ;;
        *) die "--${flag} must be true or false, not '${!flag}'" ;;
    esac
done

# The pins: KEY=value lines only (no multi-line values), `#` comments and
# blank lines skipped, CRs dropped in case a Windows checkout converted line
# endings. Each becomes a variable of this script.
[ -f "${pins}" ] || die "${pins} does not exist"
while IFS= read -r line || [ -n "${line}" ]; do
    line="${line%$'\r'}"
    case "${line}" in
        '' | '#'*) continue ;;
    esac
    [[ "${line}" =~ ^[A-Z][A-Z0-9_]*= ]] || die "malformed line in ${pins}: ${line}"
    export "${line?}"
done < "${pins}"

# need_pins NAME...: dies unless every NAME is set, naming the pins file.
need_pins() {
    local name
    for name in "$@"; do
        [ -n "${!name:-}" ] || die "${name} is not in ${pins}"
    done
}

if [ -z "${host}" ]; then
    host="$(host_triple)"
fi
case "${host}" in
    *-windows-*) exe=.exe ;;
    *) exe="" ;;
esac

temp="${RUNNER_TEMP:-${TMPDIR:-/tmp}}"
bin="${CARGO_HOME:-${HOME}/.cargo}/bin"
work="${temp}/tauri-tools"
tools="${temp}/frontend-tools"

to_posix() {
    if command -v cygpath > /dev/null 2>&1; then
        cygpath -u "$1"
    else
        printf '%s\n' "$1"
    fi
}

sha256_of() {
    # Read from stdin: with a file operand GNU sha256sum prefixes the hash with
    # a backslash when the path contains one (Windows paths do).
    if command -v sha256sum > /dev/null 2>&1; then
        sha256sum < "$1" | cut -d' ' -f1
    else
        shasum -a 256 < "$1" | cut -d' ' -f1
    fi
}

# add_path DIR: puts DIR first on PATH for the later steps of the job.
add_path() {
    local dir="$1"
    if command -v cygpath > /dev/null 2>&1; then
        dir="$(cygpath -w "${dir}")"
    fi
    if [ "${dry_run}" = true ] || [ -z "${GITHUB_PATH:-}" ]; then
        echo "PATH: would add ${dir}"
    else
        printf '%s\n' "${dir}" >> "${GITHUB_PATH}"
    fi
}

# set_env LINE: a KEY=value for the later steps of the job.
set_env() {
    if [ "${dry_run}" = true ] || [ -z "${GITHUB_ENV:-}" ]; then
        echo "env: would set $1"
    else
        printf '%s\n' "$1" >> "${GITHUB_ENV}"
    fi
}

# unpack NAME URL PLATFORM DIR: downloads URL, checks it against the
# <NAME>_SHA256_<PLATFORM> pin and unpacks it into DIR. A dry run only says
# what it would fetch.
unpack() {
    local name="$1" url="$2" platform="$3" dir="$4"
    local var want got archive
    var="${name}_SHA256_$(printf '%s' "${platform}" | tr 'a-z-' 'A-Z_')"
    want="${!var:-}"
    [ -n "${want}" ] || die "${var} is not in ${pins}"
    archive="${work}/${url##*/}"
    if [ "${dry_run}" = true ]; then
        echo "would fetch ${url}"
        echo "    sha256 ${want} (${var}); unpack into ${dir}"
        return 0
    fi
    curl --proto '=https' --tlsv1.2 -sSfL --retry 3 -o "${archive}" "${url}"
    got="$(sha256_of "${archive}")"
    [ "${got}" = "${want}" ] || die "${url##*/} has sha256 ${got}, expected ${want} (${var})"
    echo "${url##*/}: sha256 ${got} OK"
    mkdir -p "${dir}"
    case "${archive}" in
        *.zip)
            if command -v 7z > /dev/null 2>&1; then
                7z x -y -o"${dir}" "${archive}" > /dev/null
            else
                unzip -q -o "${archive}" -d "${dir}"
            fi
            ;;
        # Git Bash's tar reads "D:\..." as a remote host; hand it POSIX paths.
        *) tar -xzf "$(to_posix "${archive}")" -C "$(to_posix "${dir}")" ;;
    esac
}

# fetch NAME URL PLATFORM BINARY [DEST]: unpack, then install BINARY (a path
# inside the archive) into DEST (default: the cargo bin directory).
fetch() {
    local name="$1" url="$2" platform="$3" binary="$4" dest="${5:-${bin}}"
    unpack "${name}" "${url}" "${platform}" "${work}/${name}"
    if [ "${dry_run}" = true ]; then
        echo "    install ${binary##*/} into ${dest}"
        return 0
    fi
    cp "${work}/${name}/${binary}" "${dest}/${binary##*/}"
    chmod +x "${dest}/${binary##*/}"
}

echo "install-pinned-tools: host ${host}, compile ${compile}, bundler ${bundler}, pins ${pins}"
if [ "${dry_run}" != true ]; then
    rm -rf "${work}"
    mkdir -p "${work}" "${bin}"
fi

if [ "${bundler}" = true ]; then
    need_pins TAURI_CLI_VERSION
    case "${host}" in
        *-linux-*) cli_ext=tgz ;;
        *) cli_ext=zip ;;
    esac
    fetch TAURI_CLI \
        "https://github.com/tauri-apps/tauri/releases/download/tauri-cli-v${TAURI_CLI_VERSION}/cargo-tauri-${host}.${cli_ext}" \
        "${host}" "cargo-tauri${exe}"
    if [ "${dry_run}" != true ]; then
        # Through cargo, as the bundle steps run it: $CARGO_HOME/bin must be on PATH.
        reported="$(cargo tauri --version)"
        [ "${reported}" = "tauri-cli ${TAURI_CLI_VERSION}" ] \
            || die "cargo tauri --version is '${reported}', expected tauri-cli ${TAURI_CLI_VERSION}"
        echo "${reported}"
    fi
fi

if [ "${compile}" = true ]; then
    need_pins TRUNK_VERSION WASM_BINDGEN_VERSION BINARYEN_VERSION

    # wasm-bindgen: trunk requires the version of the wasm-bindgen crate in
    # Cargo.lock, which must be the pinned one.
    lock_version="$("${repo_root}/scripts/cargo-lock-version.sh" wasm-bindgen)" \
        || die "cannot read the wasm-bindgen version from Cargo.lock"
    [ "${lock_version}" = "${WASM_BINDGEN_VERSION}" ] \
        || die "Cargo.lock has wasm-bindgen ${lock_version}, but WASM_BINDGEN_VERSION is ${WASM_BINDGEN_VERSION} (${pins})"

    # On Linux the static musl builds of trunk and wasm-bindgen: the glibc
    # builds need symbols above the ubuntu:22.04 container's 2.35.
    case "${host}" in
        *-windows-*) trunk_platform="${host}"; trunk_ext=zip ;;
        *-linux-gnu) trunk_platform="${host%-gnu}-musl"; trunk_ext=tar.gz ;;
        *) trunk_platform="${host}"; trunk_ext=tar.gz ;;
    esac
    fetch TRUNK \
        "https://github.com/trunk-rs/trunk/releases/download/v${TRUNK_VERSION}/trunk-${trunk_platform}.${trunk_ext}" \
        "${trunk_platform}" "trunk${exe}"
    if [ "${dry_run}" != true ]; then
        reported="$("${bin}/trunk${exe}" --version)"
        [ "${reported}" = "trunk ${TRUNK_VERSION}" ] \
            || die "trunk --version is '${reported}', expected trunk ${TRUNK_VERSION}"
        echo "${reported}"
    fi

    # wasm-bindgen and wasm-opt for trunk, in a directory of their own put
    # first on PATH. Offline, trunk takes a tool from PATH only at exactly the
    # version it requires, else it fails.
    if command -v cygpath > /dev/null 2>&1; then
        tools="$(cygpath -u "${tools}")"
    fi
    if [ "${dry_run}" != true ]; then
        rm -rf "${tools}"
        mkdir -p "${tools}/bin"
    fi

    case "${host}" in
        *-linux-gnu) wb_platform="${host%-gnu}-musl" ;;
        *) wb_platform="${host}" ;;
    esac
    fetch WASM_BINDGEN \
        "https://github.com/wasm-bindgen/wasm-bindgen/releases/download/${WASM_BINDGEN_VERSION}/wasm-bindgen-${WASM_BINDGEN_VERSION}-${wb_platform}.tar.gz" \
        "${wb_platform}" "wasm-bindgen-${WASM_BINDGEN_VERSION}-${wb_platform}/wasm-bindgen${exe}" "${tools}/bin"

    # wasm-opt: the whole binaryen tree, since on macOS bin/wasm-opt loads
    # lib/libbinaryen.dylib.
    case "${host}" in
        x86_64-unknown-linux-gnu) by_platform=x86_64-linux ;;
        aarch64-unknown-linux-gnu) by_platform=aarch64-linux ;;
        aarch64-apple-darwin) by_platform=arm64-macos ;;
        x86_64-apple-darwin) by_platform=x86_64-macos ;;
        x86_64-pc-windows-msvc) by_platform=x86_64-windows ;;
        *) die "no pinned binaryen for ${host}" ;;
    esac
    unpack BINARYEN \
        "https://github.com/WebAssembly/binaryen/releases/download/${BINARYEN_VERSION}/binaryen-${BINARYEN_VERSION}-${by_platform}.tar.gz" \
        "${by_platform}" "${tools}"
    binaryen_bin="${tools}/binaryen-${BINARYEN_VERSION}/bin"

    if [ "${dry_run}" != true ]; then
        export PATH="${tools}/bin:${binaryen_bin}:${PATH}"
        # GITHUB_PATH entries come first in later steps, the last one added
        # first of all; check the order trunk will see here too. (Git Bash may
        # report a Windows .exe with or without its suffix.)
        case "$(command -v wasm-bindgen)" in
            "${tools}/bin/wasm-bindgen" | "${tools}/bin/wasm-bindgen${exe}") ;;
            *) die "wasm-bindgen on PATH is $(command -v wasm-bindgen), not the pinned one" ;;
        esac
        case "$(command -v wasm-opt)" in
            "${binaryen_bin}/wasm-opt" | "${binaryen_bin}/wasm-opt${exe}") ;;
            *) die "wasm-opt on PATH is $(command -v wasm-opt), not the pinned one" ;;
        esac
        # The version strings exactly as trunk parses them (src/tools.rs).
        wb_version="$(wasm-bindgen --version | tr -d '\r' | cut -d' ' -f2)"
        [ "${wb_version}" = "${WASM_BINDGEN_VERSION}" ] \
            || die "wasm-bindgen --version reports ${wb_version}, expected ${WASM_BINDGEN_VERSION}"
        wo_version="version_$(wasm-opt --version | tr -d '\r' | cut -d' ' -f3)"
        [ "${wo_version}" = "${BINARYEN_VERSION}" ] \
            || die "wasm-opt --version reports ${wo_version}, expected ${BINARYEN_VERSION}"
        wasm-bindgen --version
        wasm-opt --version
    fi
    add_path "${tools}/bin"
    add_path "${binaryen_bin}"
    set_env "TRUNK_OFFLINE=true"
    set_env "TRUNK_TOOLS_WASM_OPT=${BINARYEN_VERSION}"
fi

if [ "${dry_run}" != true ]; then
    rm -rf "${work}"
fi
