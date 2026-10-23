#!/usr/bin/env bash
# Prints the target matrix of .github/targets.json as one line of JSON
# ({"include":[...]}, for `fromJSON` in a workflow), narrowed to the selected
# targets. bundle.yml and zebrad-sidecars.yml both use it, so a `targets`
# input selects the same targets in both.
#
#   --targets LIST   comma-separated target triples (spaces around the commas
#                    are ignored); empty selects every target. A triple that is
#                    not in the file is an error.
#   --family NAME    keep only linux, macos or windows targets (may leave the
#                    matrix empty: {"include":[]})
#   --file PATH      the matrix file (default: .github/targets.json)
#
# The entries keep their order and fields from the file.
#
# Usage: scripts/select-targets.sh [--targets LIST] [--family linux|macos|windows]
#                                  [--file PATH]

set -euo pipefail

die() {
    echo "select-targets: error: $*" >&2
    exit 1
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

targets=""
family=""
file="${repo_root}/.github/targets.json"

while [ $# -gt 0 ]; do
    case "$1" in
        --targets) [ $# -ge 2 ] || die "--targets needs a value"; targets="$2"; shift 2 ;;
        --family) [ $# -ge 2 ] || die "--family needs a value"; family="$2"; shift 2 ;;
        --file) [ $# -ge 2 ] || die "--file needs a value"; file="$2"; shift 2 ;;
        *) die "unknown argument: $1" ;;
    esac
done

case "${family}" in
    '') pattern="" ;;
    linux) pattern="-linux-" ;;
    macos) pattern="-apple-darwin" ;;
    windows) pattern="-windows-" ;;
    *) die "--family must be linux, macos or windows, not '${family}'" ;;
esac
[ -f "${file}" ] || die "${file} does not exist"
command -v jq >/dev/null 2>&1 || die "jq is required"

# jq exits with status 5 and prints the message of error(...).
jq -c --arg targets "${targets}" --arg pattern "${pattern}" '
    ($targets | split(",") | map(gsub("^\\s+|\\s+$"; "")) | map(select(. != ""))) as $want
    | ($want - [.include[].target]) as $unknown
    | if ($unknown | length) > 0 then
        error("not in the target matrix: \($unknown | join(", ")) (known: \([.include[].target] | join(", ")))")
      else . end
    | if ($want | length) > 0 then .include |= map(select(.target | IN($want[]))) else . end
    | if $pattern != "" then .include |= map(select(.target | contains($pattern))) else . end
' "${file}" || die "cannot select '${targets}' from ${file}"
