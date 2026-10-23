# shellcheck shell=bash
# Helpers shared by the sidecar scripts (fetch-zebrad-linux.sh,
# build-zebrad.sh, dev-sidecar.sh). Source it after defining
# `die MESSAGE...`, which must print the message and exit non-zero.
#
# The functions that print a value are meant for `x="$(fn ...)"` under
# `set -e`: when they call die inside the command substitution, the assignment
# fails and the caller exits too.

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

# check_zebrad_version VERSION [HINT]: dies unless VERSION is plain semver
# (optionally with a pre-release suffix) without a leading "v", so it is safe
# in URLs and patterns. HINT is appended to the leading-"v" error.
check_zebrad_version() {
    case "$1" in
        v* | V*) die "version '$1' must not start with 'v'${2:+ $2}" ;;
    esac
    printf '%s\n' "$1" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$' ||
        die "version '$1' is not of the form X.Y.Z"
}

# check_commit SHA: dies unless SHA is a full 40-character lowercase hex SHA.
check_commit() {
    printf '%s\n' "$1" | grep -Eq '^[0-9a-f]{40}$' ||
        die "commit '$1' is not a full 40-character lowercase hex SHA"
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
