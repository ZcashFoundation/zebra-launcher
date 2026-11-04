#!/usr/bin/env bash
# Checks that the version pins in the repository agree with each other:
#
#   1. ZEBRAD_VERSION is MAJOR.MINOR.PATCH, with no leading "v".
#   2. ZEBRAD_COMMIT is a full 40-character lowercase hex commit SHA.
#   3. ZEBRAD_DATE and all beta/nightly pins use their channel grammars;
#      commits are full lowercase SHAs and dates are real ISO calendar dates.
#   4. ZEBRAD_BETA_VERSION newer than ZEBRAD_VERSION makes Beta available;
#      an equal-or-older beta is unavailable, not invalid.
#   5. The tauri-cli pin, `TAURI_CLI_VERSION=` in .github/tool-versions.env,
#      has the MAJOR.MINOR of the `tauri` crate in Cargo.lock: the CLI that
#      bundles the app must match the runtime it was built against. Skipped if
#      nothing pins tauri-cli.
#   6. src-tauri/tauri.conf.json sets plugins.updater.requireSignedVersion
#      to true, always. Its updater pubkey must also not be the placeholder,
#      unless the environment variable UPDATER_ENABLED is "false"; only this
#      pubkey part is skipped while UPDATER_ENABLED is unset (CI on pull
#      requests). release.yml sets it.
#   7. The wasm-bindgen CLI pin, `WASM_BINDGEN_VERSION=` in
#      .github/tool-versions.env, equals the `wasm-bindgen` crate in Cargo.lock
#      (read by scripts/cargo-lock-version.sh): trunk runs the CLI only at
#      exactly that version. Skipped if nothing pins it.
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

# shellcheck source-path=SCRIPTDIR source=lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

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

# 1. ZEBRAD_VERSION (the pin grammar is decided in lib.sh)
version="$(read_pin "${root}/ZEBRAD_VERSION")"
if [ ! -f "${root}/ZEBRAD_VERSION" ]; then
    fail "ZEBRAD_VERSION is missing"
elif is_stable_version "${version}"; then
    ok "ZEBRAD_VERSION is ${version}"
else
    fail "ZEBRAD_VERSION is '${version}', expected MAJOR.MINOR.PATCH without a leading v"
fi

# 2. ZEBRAD_COMMIT
commit="$(read_pin "${root}/ZEBRAD_COMMIT")"
if [ ! -f "${root}/ZEBRAD_COMMIT" ]; then
    fail "ZEBRAD_COMMIT is missing"
elif is_full_sha "${commit}"; then
    ok "ZEBRAD_COMMIT is ${commit}"
else
    fail "ZEBRAD_COMMIT is '${commit}', expected a full 40-character lowercase hex SHA"
fi

# 3. Stable release date
stable_date="$(read_pin "${root}/ZEBRAD_DATE")"
if [ ! -f "${root}/ZEBRAD_DATE" ]; then
    fail "ZEBRAD_DATE is missing"
elif is_iso_date "${stable_date}"; then
    ok "ZEBRAD_DATE is ${stable_date}"
else
    fail "ZEBRAD_DATE is '${stable_date}', expected a real ISO calendar date (YYYY-MM-DD)"
fi

# 4. Beta and nightly channel pins
for channel in beta nightly; do
    prefix="ZEBRAD_$(printf '%s' "${channel}" | tr '[:lower:]' '[:upper:]')"
    channel_version="$(read_pin "${root}/${prefix}_VERSION")"
    channel_commit="$(read_pin "${root}/${prefix}_COMMIT")"
    channel_date="$(read_pin "${root}/${prefix}_DATE")"

    if [ ! -f "${root}/${prefix}_VERSION" ]; then
        fail "${prefix}_VERSION is missing"
    elif [ "${channel}" = beta ] && ! is_prerelease_version "${channel_version}"; then
        fail "${prefix}_VERSION is '${channel_version}', expected X.Y.Z-PRERELEASE without a leading v"
    elif [ "${channel}" = nightly ] && ! is_zebrad_version "${channel_version}"; then
        fail "${prefix}_VERSION is '${channel_version}', expected X.Y.Z with an optional pre-release suffix"
    else
        ok "${prefix}_VERSION is ${channel_version}"
    fi

    if [ ! -f "${root}/${prefix}_COMMIT" ]; then
        fail "${prefix}_COMMIT is missing"
    elif is_full_sha "${channel_commit}"; then
        ok "${prefix}_COMMIT is ${channel_commit}"
    else
        fail "${prefix}_COMMIT is '${channel_commit}', expected a full 40-character lowercase hex SHA"
    fi

    if [ ! -f "${root}/${prefix}_DATE" ]; then
        fail "${prefix}_DATE is missing"
    elif is_iso_date "${channel_date}"; then
        ok "${prefix}_DATE is ${channel_date}"
    else
        fail "${prefix}_DATE is '${channel_date}', expected a real ISO calendar date (YYYY-MM-DD)"
    fi

    if [ "${channel}" = beta ]; then
        beta_version="${channel_version}"
    fi
done

# A beta from the same release as stable is older, not newer: a stable version
# takes precedence over every pre-release with the same MAJOR.MINOR.PATCH.
# Equal-or-older beta pins are valid but make Beta unavailable.
if is_stable_version "${version}" && is_prerelease_version "${beta_version}"; then
    if semver_newer "${beta_version}" "${version}"; then
        ok "ZEBRAD_BETA_VERSION ${beta_version} is semver-newer than stable ${version}"
    else
        ok "Beta is unavailable: ZEBRAD_BETA_VERSION ${beta_version} is equal to or older than ZEBRAD_VERSION ${version}"
    fi
else
    skip "beta/stable ordering (a version pin is invalid)"
fi

# 5. tauri-cli pin vs. the tauri crate
cli_pin_line="$(grep -nE '^TAURI_CLI_VERSION=' "${root}/.github/tool-versions.env" 2>/dev/null || true)"
if [ -z "${cli_pin_line}" ]; then
    skip "nothing sets TAURI_CLI_VERSION, so the tauri-cli/tauri check does not apply"
elif [ "$(printf '%s\n' "${cli_pin_line}" | wc -l)" -ne 1 ]; then
    fail ".github/tool-versions.env sets TAURI_CLI_VERSION more than once"
else
    location=".github/tool-versions.env:${cli_pin_line%%:*}"
    cli_version="${cli_pin_line#*=}"
    cli_version="${cli_version%$'\r'}"
    tauri_version="$(sed -n '/^name = "tauri"$/{n;s/^version = "\([^"]*\)"$/\1/p;}' "${root}/Cargo.lock" 2>/dev/null | head -n 1)"
    if [ -z "${tauri_version}" ]; then
        fail "TAURI_CLI_VERSION is set, but Cargo.lock has no tauri crate"
    else
        tauri_minor="$(printf '%s\n' "${tauri_version}" | sed -n 's/^\([0-9]*\.[0-9]*\)\..*/\1/p')"
        cli_minor="$(printf '%s\n' "${cli_version}" | sed -n 's/^\([0-9]*\.[0-9]*\)\..*/\1/p')"
        if [ -n "${cli_minor}" ] && [ "${cli_minor}" = "${tauri_minor}" ]; then
            ok "${location} tauri-cli ${cli_version} matches tauri ${tauri_version} (Cargo.lock)"
        else
            fail "${location} tauri-cli '${cli_version}' is not tauri ${tauri_minor}.x (tauri ${tauri_version} in Cargo.lock)"
        fi
    fi
fi

# 6. updater signature settings
# requireSignedVersion is always checked. The app is built with the minisign
# public key in tauri.conf.json; a release with the updater enabled must not
# ship the placeholder, or every update would fail to verify. The placeholder
# is only checked when UPDATER_ENABLED is set (release.yml sets it); any value
# but "false" enforces it.
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

# 7. wasm-bindgen CLI pin vs. the wasm-bindgen crate
wb_pin_line="$(grep -nE '^WASM_BINDGEN_VERSION=' "${root}/.github/tool-versions.env" 2>/dev/null || true)"
if [ -z "${wb_pin_line}" ]; then
    skip "nothing sets WASM_BINDGEN_VERSION, so the wasm-bindgen check does not apply"
elif [ "$(printf '%s\n' "${wb_pin_line}" | wc -l)" -ne 1 ]; then
    fail ".github/tool-versions.env sets WASM_BINDGEN_VERSION more than once"
else
    wb_pin="${wb_pin_line#*=}"
    wb_pin="${wb_pin%$'\r'}"
    if ! wb_lock="$("$(dirname "${BASH_SOURCE[0]}")/cargo-lock-version.sh" wasm-bindgen "${root}/Cargo.lock" 2>&1)"; then
        fail "WASM_BINDGEN_VERSION is ${wb_pin}, but: ${wb_lock}"
    elif [ "${wb_pin}" = "${wb_lock}" ]; then
        ok ".github/tool-versions.env:${wb_pin_line%%:*} wasm-bindgen CLI ${wb_pin} matches the wasm-bindgen crate (Cargo.lock)"
    else
        fail ".github/tool-versions.env:${wb_pin_line%%:*} wasm-bindgen CLI ${wb_pin} is not the wasm-bindgen crate's ${wb_lock} (Cargo.lock); trunk needs them equal"
    fi
fi

if [ "${failures}" -gt 0 ]; then
    echo "check-versions: ${failures} check(s) failed" >&2
    exit 1
fi
echo "check-versions: all checks passed"
