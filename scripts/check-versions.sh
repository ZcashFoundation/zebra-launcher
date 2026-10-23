#!/usr/bin/env bash
# Checks that the version pins in the repository agree with each other:
#
#   1. ZEBRAD_VERSION is MAJOR.MINOR.PATCH, with no leading "v".
#   2. ZEBRAD_COMMIT is a full 40-character lowercase hex commit SHA.
#   3. VERSION in crates/launcher-core/src/bin/fake-zebrad.rs equals
#      ZEBRAD_VERSION, so the fake reports the version the launcher bundles.
#   4. If a workflow pins tauri-cli with a `TAURI_CLI_VERSION:` env entry, its
#      MAJOR.MINOR equals that of the `tauri` crate in Cargo.lock (or, while
#      the app is outside the workspace, src-tauri/Cargo.lock): the CLI that
#      bundles the app must match the runtime it was built against. No
#      workflow pins tauri-cli yet, so this check is skipped until one does.
#   5. src-tauri/tauri.conf.json sets plugins.updater.requireSignedVersion,
#      and its updater pubkey is not the placeholder, unless the environment
#      variable UPDATER_ENABLED is "false". Skipped while UPDATER_ENABLED is
#      unset (CI on pull requests); release.yml sets it.
#
# Runs every check and reports each one, then exits 1 if any failed.
# Requires: bash, grep, sed.
#
# Usage: scripts/check-versions.sh [--root DIR]
#   --root  the repository to check (default: this script's repository)

set -euo pipefail

die() {
    echo "check-versions: error: $*" >&2
    exit 2
}

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
while [ $# -gt 0 ]; do
    case "$1" in
        --root) [ $# -ge 2 ] || die "--root needs a value"; root="$2"; shift 2 ;;
        -h | --help) sed -n '2,/^$/{s/^# \{0,1\}//p;}' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) die "unknown argument: $1" ;;
    esac
done
[ -d "${root}" ] || die "${root} is not a directory"

failures=0
ok() { echo "ok:   $*"; }
fail() {
    echo "FAIL: $*"
    failures=$((failures + 1))
}
skip() { echo "skip: $*"; }

# Prints a file with all whitespace removed, or nothing if it is missing.
read_pin() {
    [ -f "$1" ] && tr -d '[:space:]' <"$1"
    return 0
}

# 1. ZEBRAD_VERSION
version="$(read_pin "${root}/ZEBRAD_VERSION")"
if [ ! -f "${root}/ZEBRAD_VERSION" ]; then
    fail "ZEBRAD_VERSION is missing"
elif printf '%s\n' "${version}" | grep -Eq '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$'; then
    ok "ZEBRAD_VERSION is ${version}"
else
    fail "ZEBRAD_VERSION is '${version}', expected MAJOR.MINOR.PATCH without a leading v"
fi

# 2. ZEBRAD_COMMIT
commit="$(read_pin "${root}/ZEBRAD_COMMIT")"
if [ ! -f "${root}/ZEBRAD_COMMIT" ]; then
    fail "ZEBRAD_COMMIT is missing"
elif printf '%s\n' "${commit}" | grep -Eq '^[0-9a-f]{40}$'; then
    ok "ZEBRAD_COMMIT is ${commit}"
else
    fail "ZEBRAD_COMMIT is '${commit}', expected a full 40-character lowercase hex SHA"
fi

# 3. fake-zebrad's VERSION
fake="crates/launcher-core/src/bin/fake-zebrad.rs"
if [ ! -f "${root}/${fake}" ]; then
    fail "${fake} is missing"
else
    fake_versions="$(sed -n 's/^const VERSION: &str = "\([^"]*\)";$/\1/p' "${root}/${fake}")"
    if [ -z "${fake_versions}" ] || [ "$(printf '%s\n' "${fake_versions}" | wc -l)" -ne 1 ]; then
        fail "${fake} must have exactly one line 'const VERSION: &str = \"X.Y.Z\";'"
    elif [ "${fake_versions}" = "${version}" ]; then
        ok "${fake} VERSION is ${fake_versions}"
    else
        fail "${fake} VERSION is ${fake_versions}, but ZEBRAD_VERSION is ${version}"
    fi
fi

# 4. tauri-cli pin vs. the tauri crate
# Matches `TAURI_CLI_VERSION: 2.1.0`, quoted or not, in any workflow's env.
cli_pins="$(grep -HnE '^[[:space:]]*TAURI_CLI_VERSION:' "${root}"/.github/workflows/*.yml "${root}"/.github/workflows/*.yaml 2>/dev/null || true)"
if [ -z "${cli_pins}" ]; then
    skip "no workflow sets TAURI_CLI_VERSION, so the tauri-cli/tauri check does not apply"
else
    tauri_version=""
    for lock in Cargo.lock src-tauri/Cargo.lock; do
        [ -f "${root}/${lock}" ] || continue
        tauri_version="$(sed -n '/^name = "tauri"$/{n;s/^version = "\([^"]*\)"$/\1/p;}' "${root}/${lock}" | head -n 1)"
        if [ -n "${tauri_version}" ]; then
            tauri_lock="${lock}"
            break
        fi
    done
    if [ -z "${tauri_version}" ]; then
        fail "a workflow sets TAURI_CLI_VERSION, but no Cargo.lock has the tauri crate"
    else
        tauri_minor="$(printf '%s\n' "${tauri_version}" | sed -n 's/^\([0-9]*\.[0-9]*\)\..*/\1/p')"
        while IFS= read -r pin; do
            location="${pin%%:*}:$(printf '%s\n' "${pin#*:}" | cut -d: -f1)"
            location="${location#"${root}"/}"
            cli_version="$(printf '%s\n' "${pin}" | sed -E 's/.*TAURI_CLI_VERSION:[[:space:]]*["'\'']?([^"'\''[:space:]#]*).*/\1/')"
            cli_minor="$(printf '%s\n' "${cli_version}" | sed -n 's/^\([0-9]*\.[0-9]*\)\..*/\1/p')"
            if [ -n "${cli_minor}" ] && [ "${cli_minor}" = "${tauri_minor}" ]; then
                ok "${location} tauri-cli ${cli_version} matches tauri ${tauri_version} (${tauri_lock})"
            else
                fail "${location} tauri-cli '${cli_version}' is not tauri ${tauri_minor}.x (tauri ${tauri_version} in ${tauri_lock})"
            fi
        done <<<"${cli_pins}"
    fi
fi

# 5. updater public key
# The app is built with the minisign public key in tauri.conf.json. A release
# with the updater enabled must not ship the placeholder: every update would
# fail to verify. Checked only when UPDATER_ENABLED is set (release.yml sets
# it); any value but "false" enforces it.
conf="src-tauri/tauri.conf.json"
placeholder="REPLACE_WITH_MINISIGN_PUBLIC_KEY"
if [ ! -f "${root}/${conf}" ]; then
    fail "${conf} is missing"
elif ! grep -Eq '"requireSignedVersion":[[:space:]]*true' "${root}/${conf}"; then
    fail "${conf} must set plugins.updater.requireSignedVersion to true (camelCase)"
elif ! grep -q "\"pubkey\":[[:space:]]*\"${placeholder}\"" "${root}/${conf}"; then
    ok "${conf} has an updater public key"
elif [ -z "${UPDATER_ENABLED+set}" ]; then
    skip "${conf} has the placeholder updater public key; fine unless releasing (UPDATER_ENABLED is unset)"
elif [ "${UPDATER_ENABLED}" = "false" ]; then
    ok "${conf} has the placeholder updater public key, and UPDATER_ENABLED=false"
else
    fail "${conf} has the placeholder updater public key, but UPDATER_ENABLED is '${UPDATER_ENABLED}': replace it (docs/SECURITY.md) or set the repository variable UPDATER_ENABLED=false"
fi

if [ "${failures}" -gt 0 ]; then
    echo "check-versions: ${failures} check(s) failed" >&2
    exit 1
fi
echo "check-versions: all checks passed"
