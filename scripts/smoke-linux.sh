#!/usr/bin/env bash
# Install smoke test for a Linux bundle, run by bundle.yml in the ubuntu:22.04
# container: starts the launcher under Xvfb with a fresh home directory, lets
# it run for a while, then checks that it was still running when stopped, that
# its log says the bundled zebrad started, and that no zebrad is left over.
#
#   --deb      runs the installed /usr/bin/zebra-launcher (install the .deb first)
#   --appimage PATH runs the AppImage (set APPIMAGE_EXTRACT_AND_RUN=1 where
#              there is no FUSE, as in containers)
#
# The launcher's log is expected in Tauri's app_log_dir, which on Linux is
# $XDG_DATA_HOME/org.zfnd.zebra-launcher/logs (~/.local/share/... by default).
# The script searches the whole fresh home for the line, so a different log
# location still passes, and prints where it found it.
#
# Requires: xvfb-run, dbus-run-session, timeout, pgrep.
#
# Usage: scripts/smoke-linux.sh (--deb | --appimage PATH) [--seconds N]

set -euo pipefail

readonly IDENTIFIER="org.zfnd.zebra-launcher"
readonly EXPECTED="zebrad started"

die() {
    echo "::error::smoke-linux: $*"
    exit 1
}

cmd=()
seconds=15
while [ $# -gt 0 ]; do
    case "$1" in
        --deb) cmd=(/usr/bin/zebra-launcher); shift ;;
        --appimage)
            [ $# -ge 2 ] || die "--appimage needs a path"
            chmod +x "$2"
            cmd=("$(cd "$(dirname "$2")" && pwd)/$(basename "$2")")
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

# The launcher stops zebrad when it exits (PDEATHSIG / graceful stop); give it
# a moment, then report and clean up any leftover.
sleep 5
if pgrep -f zebra-launcher-zebrad > /dev/null; then
    echo "::error::a zebra-launcher-zebrad process outlived the launcher"
    pgrep -af zebra-launcher-zebrad || true
    pkill -KILL -f zebra-launcher-zebrad || true
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
