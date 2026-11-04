# shellcheck shell=bash
# Helpers shared by the sidecar and version scripts (fetch-zebrad-linux.sh,
# build-zebrad.sh, dev-sidecar.sh, check-versions.sh, bump-zebrad.sh) and by
# the `run:` steps of zebrad-sidecars.yml and zebrad-watch.yml. Source it after
# defining `die MESSAGE...`, which must print the message and exit non-zero.
#
# The functions that print a value are meant for `x="$(fn ...)"` under
# `set -e`: when they call die inside the command substitution, the assignment
# fails and the caller exits too.
#
# Version grammars, decided once here:
#   - the ZEBRAD_VERSION pin is a stable release, MAJOR.MINOR.PATCH and nothing
#     else (is_stable_version): zebrad-watch.yml only ever bumps to one, and
#     check-versions.sh and bump-zebrad.sh enforce it;
#   - a one-off `--version` given to the build and fetch scripts, the `version`
#     input of zebrad-sidecars.yml, or a beta/nightly pin may carry a
#     pre-release suffix such as 7.0.0-rc.0 (is_zebrad_version). Beta pins must
#     have that suffix (is_prerelease_version).

# read_pin FILE: prints FILE with all whitespace removed, or nothing if it is
# missing.
read_pin() {
    [ -f "$1" ] && tr -d '[:space:]' <"$1"
    return 0
}

# read_zebrad_version ROOT: prints the version pinned in ROOT/ZEBRAD_VERSION.
read_zebrad_version() {
    [ -f "$1/ZEBRAD_VERSION" ] || die "no --version given and $1/ZEBRAD_VERSION is missing"
    read_pin "$1/ZEBRAD_VERSION"
}

# read_zebrad_commit ROOT VERSION: prints the commit pinned in
# ROOT/ZEBRAD_COMMIT when VERSION is the one in ROOT/ZEBRAD_VERSION (the
# commit pins that version only), and nothing otherwise.
read_zebrad_commit() {
    if [ -f "$1/ZEBRAD_COMMIT" ] && [ "$2" = "$(read_pin "$1/ZEBRAD_VERSION")" ]; then
        read_pin "$1/ZEBRAD_COMMIT"
    fi
}

# is_zebrad_version STRING: succeeds for semantic X.Y.Z or X.Y.Z-PRERELEASE,
# without a leading "v" or build metadata. Numeric identifiers have no leading
# zeros; pre-release identifiers may contain letters and additional hyphens.
is_zebrad_version() {
    printf '%s\n' "$1" | grep -Eq '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-((0|[1-9][0-9]*)|([0-9A-Za-z-]*[A-Za-z-][0-9A-Za-z-]*))(\.((0|[1-9][0-9]*)|([0-9A-Za-z-]*[A-Za-z-][0-9A-Za-z-]*)))*)?$'
}

# is_stable_version STRING: succeeds when STRING is exactly MAJOR.MINOR.PATCH,
# with no leading zeros, suffix or leading "v": the grammar of ZEBRAD_VERSION.
is_stable_version() {
    printf '%s\n' "$1" | grep -Eq '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$'
}

# is_prerelease_version STRING: succeeds for the beta pin grammar, X.Y.Z with
# a pre-release suffix and no leading "v".
is_prerelease_version() {
    is_zebrad_version "$1" && [[ "$1" == *-* ]]
}

# is_iso_date STRING: succeeds for a real YYYY-MM-DD calendar date, including
# leap-year rules, without relying on GNU date.
is_iso_date() {
    [[ "$1" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]] || return 1
    local year=$((10#${1:0:4}))
    local month=$((10#${1:5:2}))
    local day=$((10#${1:8:2}))
    ((year >= 1 && month >= 1 && month <= 12 && day >= 1)) || return 1
    local days=(31 28 31 30 31 30 31 31 30 31 30 31)
    if ((year % 4 == 0 && (year % 100 != 0 || year % 400 == 0))); then
        days[1]=29
    fi
    ((day <= days[month - 1]))
}

# semver_newer LEFT RIGHT: succeeds only when LEFT is semver-newer than RIGHT.
# Both arguments must already use is_zebrad_version's grammar. Returns 2 for an
# invalid argument, 1 for an equal or older version, and 0 for a newer one.
semver_newer() {
    is_zebrad_version "$1" && is_zebrad_version "$2" || return 2
    local LC_ALL=C
    local left_core="${1%%-*}" right_core="${2%%-*}"
    local left_pre="" right_pre=""
    [ "${1}" = "${left_core}" ] || left_pre="${1#*-}"
    [ "${2}" = "${right_core}" ] || right_pre="${2#*-}"

    local left_parts=() right_parts=()
    IFS=. read -r -a left_parts <<<"${left_core}"
    IFS=. read -r -a right_parts <<<"${right_core}"
    local i
    for i in 0 1 2; do
        if ((10#${left_parts[i]} > 10#${right_parts[i]})); then
            return 0
        elif ((10#${left_parts[i]} < 10#${right_parts[i]})); then
            return 1
        fi
    done

    # A release is newer than a pre-release of the same MAJOR.MINOR.PATCH.
    if [ -z "${left_pre}" ]; then
        [ -n "${right_pre}" ] && return 0
        return 1
    fi
    [ -n "${right_pre}" ] || return 1

    local left_ids=() right_ids=()
    IFS=. read -r -a left_ids <<<"${left_pre}"
    IFS=. read -r -a right_ids <<<"${right_pre}"
    local count="${#left_ids[@]}"
    [ "${#right_ids[@]}" -le "${count}" ] || count="${#right_ids[@]}"
    local left_id right_id
    for ((i = 0; i < count; i++)); do
        if [ "${i}" -ge "${#left_ids[@]}" ]; then
            return 1
        elif [ "${i}" -ge "${#right_ids[@]}" ]; then
            return 0
        fi
        left_id="${left_ids[i]}"
        right_id="${right_ids[i]}"
        if [[ "${left_id}" =~ ^[0-9]+$ ]] && [[ "${right_id}" =~ ^[0-9]+$ ]]; then
            if ((10#${left_id} > 10#${right_id})); then
                return 0
            elif ((10#${left_id} < 10#${right_id})); then
                return 1
            fi
        elif [[ "${left_id}" =~ ^[0-9]+$ ]]; then
            return 1
        elif [[ "${right_id}" =~ ^[0-9]+$ ]]; then
            return 0
        elif [[ "${left_id}" != "${right_id}" ]]; then
            [[ "${left_id}" > "${right_id}" ]] && return 0
            return 1
        fi
    done
    return 1
}

# check_zebrad_version VERSION [HINT]: dies unless is_zebrad_version VERSION.
# HINT is appended to the leading-"v" error.
check_zebrad_version() {
    case "$1" in
        v* | V*) die "version '$1' must not start with 'v'${2:+ $2}" ;;
    esac
    is_zebrad_version "$1" || die "version '$1' is not of the form X.Y.Z"
}

# check_stable_version VERSION: dies unless is_stable_version VERSION, as the
# ZEBRAD_VERSION pin requires.
check_stable_version() {
    is_stable_version "$1" || die "'$1' is not MAJOR.MINOR.PATCH without a leading v"
}

# check_commit SHA: dies unless is_full_sha SHA.
check_commit() {
    is_full_sha "$1" || die "commit '$1' is not a full 40-character lowercase hex SHA"
}

# check_git_ref REF: dies unless REF looks like a branch, tag or commit name
# that is safe to pass to git as an argument: no leading "-", no whitespace,
# no "..", nothing but letters, digits and ._/+-.
check_git_ref() {
    case "$1" in
        '' | -* | *..* | */ | *.lock) die "'$1' is not a usable git ref" ;;
    esac
    printf '%s\n' "$1" | grep -Eq '^[A-Za-z0-9][A-Za-z0-9._/+-]*$' ||
        die "'$1' is not a usable git ref (allowed: letters, digits and ._/+-)"
}

# is_full_sha STRING: succeeds when STRING is a full 40-hex lowercase SHA.
is_full_sha() {
    printf '%s\n' "$1" | grep -Eq '^[0-9a-f]{40}$'
}

# host_triple: prints rustc's host target triple.
host_triple() {
    command -v rustc >/dev/null 2>&1 || die "rustup/rustc is required"
    local triple
    triple="$(rustc -vV | sed -n 's/^host: //p')"
    [ -n "${triple}" ] || die "rustc -vV did not report a host triple"
    printf '%s\n' "${triple}"
}

# check_reported_version BINARY VERSION: dies, deleting BINARY, unless
# `BINARY --version` prints `zebrad VERSION`, optionally followed by a space or
# by `+` build metadata.
check_reported_version() {
    local reported
    reported="$("$1" --version 2>/dev/null | head -n 1)" || reported=""
    case "${reported}" in
        "zebrad $2" | "zebrad $2 "* | "zebrad $2+"*) ;;
        *)
            rm -f "$1"
            die "binary reports '${reported}', expected 'zebrad $2'"
            ;;
    esac
}

# zebrad_version_line BINARY: prints the first line of `BINARY --version`, and
# dies, deleting BINARY, unless it is `zebrad <something>`. For development
# builds from a git ref, whose version is not known in advance.
zebrad_version_line() {
    local reported
    reported="$("$1" --version 2>/dev/null | head -n 1)" || reported=""
    case "${reported}" in
        "zebrad "?*) printf '%s\n' "${reported}" ;;
        *)
            rm -f "$1"
            die "binary reports '${reported}', expected 'zebrad <version>'"
            ;;
    esac
}

# install_atomic SOURCE OUT [VERSION]: installs SOURCE as the executable OUT
# through OUT.tmp, so a failed copy or check never leaves a bad file at OUT.
# With VERSION, OUT.tmp must pass check_reported_version first.
install_atomic() {
    mkdir -p "$(dirname "$2")"
    cp "$1" "$2.tmp"
    chmod +x "$2.tmp"
    if [ -n "${3:-}" ]; then
        check_reported_version "$2.tmp" "$3"
    fi
    mv -f "$2.tmp" "$2"
}
