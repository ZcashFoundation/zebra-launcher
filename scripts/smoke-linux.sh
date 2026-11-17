#!/usr/bin/env bash
# Install smoke test for a Linux bundle, run by bundle.yml in the ubuntu:22.04
# container: checks all three channel sidecars, zeal-zingod, the offline
# documentation and zingod's licence files, then starts the launcher under Xvfb
# with a fresh home directory whose first run is complete. After letting it
# run, checks that it was still running when stopped, that its log says the
# bundled zebrad started, and that no zebrad or zingod is left over.
#
#   --deb      checks the installed package, then runs /usr/bin/zeal-app
#   --appimage PATH extracts a scratch copy to check its contents, then runs it
#              (set APPIMAGE_EXTRACT_AND_RUN=1 where there is no FUSE)
#
# The launcher's log is expected in Tauri's app_log_dir, which on Linux is
# $XDG_DATA_HOME/org.zfnd.zeal/logs (~/.local/share/... by default).
# The script searches the whole fresh home for the line, so a different log
# location still passes, and prints where it found it.
#
# Requires: xvfb-run, dbus-run-session, timeout, pgrep.
#
# Usage: scripts/smoke-linux.sh (--deb | --appimage PATH) [--seconds N]

set -euo pipefail

readonly IDENTIFIER="org.zfnd.zeal"
readonly EXPECTED="zebrad started"
# The licence files a build copies into the folder, beside the committed ones
# (scripts/zingod-licenses.sh).
readonly ZINGOD_COPIED_LICENSES=(jemalloc-COPYING aws-lc-LICENSE)

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "${script_dir}/.." && pwd)"
zingod_version="$(tr -d '[:space:]' < "${repo_root}/ZINGOD_VERSION")"

die() {
    echo "::error::smoke-linux: $*"
    exit 1
}

check_sidecars() {
    local bin_dir="$1" channel suffix sidecar reported
    for channel in stable beta nightly; do
        suffix=""
        [ "${channel}" = stable ] || suffix="-${channel}"
        sidecar="${bin_dir}/zeal-zebrad${suffix}"
        [ -x "${sidecar}" ] || die "the bundle does not contain executable ${sidecar}"
        echo "Installed sidecar: ${sidecar} ($("${sidecar}" --version | head -n 1))"
    done
    sidecar="${bin_dir}/zeal-zingod"
    [ -x "${sidecar}" ] || die "the bundle does not contain executable ${sidecar}"
    reported="$("${sidecar}" --version | sed -n 1p)"
    [ "${reported}" = "zingod ${zingod_version}" ] ||
        die "${sidecar} reports '${reported}', not 'zingod ${zingod_version}'"
    echo "Installed sidecar: ${sidecar} (${reported})"
}

# check_zingod_licenses DIR: every licence file of the repository's folder, and
# the copied ones, is in DIR with the same bytes.
check_zingod_licenses() {
    local dir="$1" committed="${repo_root}/src-tauri/resources/licenses/zingod" file name
    [ -d "${dir}" ] || die "the bundle does not contain zingod's licences at ${dir}"
    for name in "${ZINGOD_COPIED_LICENSES[@]}"; do
        [ -f "${committed}/${name}" ] || die "${committed}/${name} was not installed before the bundle was built"
    done
    for file in "${committed}/"*; do
        name="$(basename "${file}")"
        cmp -s "${file}" "${dir}/${name}" || die "the bundle's ${dir}/${name} is missing or differs"
    done
    echo "zingod's licences: $(find "${dir}" -type f | wc -l) files in ${dir}"
}

check_docs() {
    local root="$1"
    [ -d "${root}" ] || die "the bundle does not contain offline documentation at ${root}"
    "${script_dir}/validate-zebra-docs.sh" --root "${root}"
}

cmd=()
seconds=15
while [ $# -gt 0 ]; do
    case "$1" in
        --deb)
            cmd=(/usr/bin/zeal-app)
            check_sidecars /usr/bin
            docs_index="$(dpkg -L zeal-app | grep -E '/zebra-docs/stable/book/index\.html$' | head -n 1 || true)"
            [ -n "${docs_index}" ] || die "the .deb did not install the offline documentation"
            check_docs "${docs_index%/stable/book/index.html}"
            readme="$(dpkg -L zeal-app | grep -E '/licenses/zingod/README\.txt$' | head -n 1 || true)"
            [ -n "${readme}" ] || die "the .deb did not install zingod's licences"
            check_zingod_licenses "$(dirname "${readme}")"
            shift
            ;;
        --appimage)
            [ $# -ge 2 ] || die "--appimage needs a path"
            chmod +x "$2"
            appimage="$(cd "$(dirname "$2")" && pwd)/$(basename "$2")"
            extract="$(mktemp -d)"
            trap 'rm -rf "${extract}"' EXIT
            if ! (cd "${extract}" && "${appimage}" --appimage-extract > /dev/null); then
                die "could not extract ${appimage} to check its sidecars and documentation"
            fi
            app_root="${extract}/squashfs-root"
            check_sidecars "${app_root}/usr/bin"
            docs_index="$(find "${app_root}" -type f -path '*/zebra-docs/stable/book/index.html' -print -quit)"
            [ -n "${docs_index}" ] || die "the AppImage does not contain the offline documentation"
            check_docs "${docs_index%/stable/book/index.html}"
            readme="$(find "${app_root}" -type f -path '*/licenses/zingod/README.txt' -print -quit)"
            [ -n "${readme}" ] || die "the AppImage does not contain zingod's licences"
            check_zingod_licenses "$(dirname "${readme}")"
            cmd=("${appimage}")
            shift 2
            ;;
        --seconds) [ $# -ge 2 ] || die "--seconds needs a value"; seconds="$2"; shift 2 ;;
        *) die "unknown argument: $1" ;;
    esac
done
[ "${#cmd[@]}" -gt 0 ] || die "pass --deb or --appimage PATH"
[ -x "${cmd[0]}" ] || die "${cmd[0]} is not an executable file"

# A fresh home, so the run starts from first-launch state and the log search
# cannot pick up an earlier run.
home="$(mktemp -d)"
export HOME="${home}"
export XDG_DATA_HOME="${home}/.local/share"
export XDG_CONFIG_HOME="${home}/.config"
export XDG_CACHE_HOME="${home}/.cache"
export XDG_STATE_HOME="${home}/.local/state"
export XDG_RUNTIME_DIR="${home}/runtime"
mkdir -p "${XDG_DATA_HOME}" "${XDG_CONFIG_HOME}" "${XDG_CACHE_HOME}" "${XDG_STATE_HOME}"
mkdir -p "${XDG_RUNTIME_DIR}"
chmod 0700 "${XDG_RUNTIME_DIR}"

# The state after the first-run wizard. A fresh install starts no zebrad until
# the wizard is finished; from then on it starts the bundled zebrad at launch,
# which is what this test checks. The other settings take their defaults.
mkdir -p "${XDG_CONFIG_HOME}/${IDENTIFIER}"
printf '{"schema_version": 2, "first_run_done": true}\n' \
    > "${XDG_CONFIG_HOME}/${IDENTIFIER}/launcher.json"

# WebKitGTK in a container: no GPU, and its bubblewrap sandbox needs user
# namespaces the container does not grant. Smoke test only; never set for users.
export WEBKIT_DISABLE_DMABUF_RENDERER=1
export WEBKIT_DISABLE_COMPOSITING_MODE=1
export WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1
export LIBGL_ALWAYS_SOFTWARE=1

echo "Running ${cmd[*]} for ${seconds}s with HOME=${home}"
set +e
xvfb-run --auto-servernum --server-args='-screen 0 1280x800x24' \
    dbus-run-session -- \
    timeout --signal=TERM --kill-after=10 "${seconds}" "${cmd[@]}"
rc=$?
set -e

# 124: timeout stopped the launcher, so it was still running after N seconds.
if [ "${rc}" -ne 124 ]; then
    echo "::error::the launcher exited on its own with status ${rc} before ${seconds}s"
    failed=true
else
    echo "The launcher was still running after ${seconds}s (stopped by timeout)."
    failed=false
fi

log_dir="${XDG_DATA_HOME}/${IDENTIFIER}/logs"
elsewhere="$(grep -rlsF "${EXPECTED}" "${home}" || true)"
if grep -rqsF "${EXPECTED}" "${log_dir}"; then
    echo "Found \"${EXPECTED}\" in ${log_dir}:"
    grep -rsF "${EXPECTED}" "${log_dir}" || true
elif [ -n "${elsewhere}" ]; then
    echo "::warning::\"${EXPECTED}\" found outside ${log_dir}, in: ${elsewhere//$'\n'/ }. Update the expected log dir."
else
    echo "::error::no \"${EXPECTED}\" line in any file under ${home}"
    failed=true
fi

# The launcher stops zebrad, and later zingod, when it exits (PDEATHSIG /
# graceful stop). A sidecar that exited but was orphaned stays a zombie in a
# container whose PID 1 does not reap children; it no longer runs, so only live
# processes count.
live_sidecars() {
    local pid state
    for pid in $(pgrep -f 'zeal-zebrad|zeal-zingod' || true); do
        state="$(ps -o stat= -p "${pid}" 2>/dev/null || true)"
        case "${state}" in
            "" | Z*) ;;
            *) echo "${pid}" ;;
        esac
    done
}
for _ in $(seq 1 15); do
    [ -z "$(live_sidecars)" ] && break
    sleep 1
done
mapfile -t leftover < <(live_sidecars)
if [ "${#leftover[@]}" -gt 0 ]; then
    echo "::error::a zeal-zebrad or zeal-zingod process outlived the launcher by 15 s"
    for pid in "${leftover[@]}"; do ps -o pid=,stat=,args= -p "${pid}" || true; done
    pkill -KILL -f 'zeal-zebrad|zeal-zingod' || true
    failed=true
fi

if [ "${failed}" = true ]; then
    echo "--- files under ${home}"
    find "${home}" -type f -not -path '*/runtime/*' | head -n 100
    echo "--- last lines of each log file"
    find "${home}" -type f -name '*.log*' -print0 \
        | while IFS= read -r -d '' f; do echo "== ${f}"; tail -n 40 "${f}"; done
    exit 1
fi
echo "Smoke test passed."
