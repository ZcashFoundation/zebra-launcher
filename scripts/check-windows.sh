#!/usr/bin/env bash
# Type-checks the app crate for Windows from a Linux or macOS host:
#
#   cargo clippy -p zebra-launcher --target x86_64-pc-windows-msvc --all-targets
#
# so `cfg(windows)` code is checked without a Windows machine, and without the
# WebKitGTK packages the Linux build of the app needs. Three things the bare
# command lacks on a non-Windows host are provided here:
#
#   1. The target: `rustup target add x86_64-pc-windows-msvc` when it is not
#      installed (rust-toolchain.toml lists only wasm32).
#   2. The sidecar file tauri-build requires: an empty
#      src-tauri/binaries/zebra-launcher-zebrad-x86_64-pc-windows-msvc.exe,
#      created when missing and removed again afterwards (the directory is
#      git-ignored).
#   3. A C compiler and archiver for the target: the build scripts of `ring`,
#      `embed-resource` and `vswhom-sys` ask the `cc` crate for one and fail
#      without MSVC. A type-check never links, so stubs that write empty
#      objects and archives are enough; they are written to a temporary
#      directory and passed through CC_x86_64_pc_windows_msvc and
#      AR_x86_64_pc_windows_msvc.
#
# RUSTFLAGS defaults to "-D warnings" when it is unset. Extra arguments are
# passed on to cargo clippy (for example --locked).
#
# Requires: bash, rustup, cargo with clippy.
#
# Usage: scripts/check-windows.sh [cargo clippy arguments...]

set -euo pipefail

die() {
    echo "check-windows: error: $*" >&2
    exit 1
}

readonly TARGET=x86_64-pc-windows-msvc

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

for tool in rustup cargo; do
    command -v "${tool}" >/dev/null 2>&1 || die "${tool} is required but not on PATH"
done

if ! rustup target list --installed | grep -qx "${TARGET}"; then
    echo "Installing the ${TARGET} target"
    rustup target add "${TARGET}"
fi

sidecar="${repo_root}/src-tauri/binaries/zebra-launcher-zebrad-${TARGET}.exe"
created_sidecar=false
if [ ! -e "${sidecar}" ]; then
    mkdir -p "$(dirname "${sidecar}")"
    : >"${sidecar}"
    created_sidecar=true
    echo "Created the placeholder sidecar ${sidecar#"${repo_root}/"}"
fi

stubs="$(mktemp -d)"
cleanup() {
    rm -rf "${stubs}"
    if [ "${created_sidecar}" = true ]; then
        rm -f "${sidecar}"
    fi
}
trap cleanup EXIT

cat >"${stubs}/cc" <<'EOF'
#!/usr/bin/env bash
# Type-check stub for the MSVC C compiler: writes an empty object for every
# -Fo<file>, /Fo<file> or -o <file> output and succeeds.
prev=""
for a in "$@"; do
    case "$a" in
        -Fo* | /Fo*) : >"${a:3}" ;;
    esac
    if [ "$prev" = "-o" ]; then : >"$a"; fi
    prev="$a"
done
exit 0
EOF

cat >"${stubs}/ar" <<'EOF'
#!/usr/bin/env bash
# Type-check stub for the MSVC librarian: writes an empty archive to the
# -out:<file> / /OUT:<file> argument, or to the first .lib/.a argument, and
# succeeds.
for a in "$@"; do
    case "$a" in
        -out:* | /OUT:* | -OUT:* | /out:*) : >"${a#*:}"; exit 0 ;;
    esac
done
for a in "$@"; do
    case "$a" in
        *.lib | *.a) printf '!<arch>\n' >"$a"; exit 0 ;;
    esac
done
exit 0
EOF

chmod +x "${stubs}/cc" "${stubs}/ar"
export CC_x86_64_pc_windows_msvc="${stubs}/cc"
export AR_x86_64_pc_windows_msvc="${stubs}/ar"
export RUSTFLAGS="${RUSTFLAGS-"-D warnings"}"

cd "${repo_root}"
echo "RUSTFLAGS=${RUSTFLAGS} cargo clippy -p zebra-launcher --target ${TARGET} --all-targets $*"
cargo clippy -p zebra-launcher --target "${TARGET}" --all-targets "$@"
