#!/usr/bin/env bash
# Prints the body of the CHANGELOG.md section for one version, without its
# heading and without leading or trailing blank lines, and exits 1 if there is
# no such section or it is empty.
#
# A section starts at a `## ` heading that begins with the exact version,
# optionally after `v` and/or `[`: `## 0.2.0`, `## v0.2.0 - 2026-11-02` and
# `## [0.2.0] - 2026-11-02` all match 0.2.0, but `## 0.2.0-rc.1`, `## 0.2.01`
# and `## 0.2.0.1` do not, because the character after the version must not be
# one that can continue a version (`[0-9A-Za-z.+-]`). It ends at the next
# `## ` heading.
#
# Plain string comparison, not a regex built from the version, so dots and
# `+` in the version are literal.
#
# Usage: scripts/changelog-section.sh VERSION [FILE]   (FILE defaults to CHANGELOG.md)

set -euo pipefail

[ $# -ge 1 ] && [ $# -le 2 ] || {
    echo "usage: changelog-section.sh VERSION [FILE]" >&2
    exit 2
}
version="$1"
file="${2:-CHANGELOG.md}"
[ -n "${version}" ] || { echo "changelog-section: empty version" >&2; exit 2; }
[ -f "${file}" ] || exit 1

awk -v v="${version}" '
    /^## / {
        if (found) exit
        h = substr($0, 4)
        sub(/^\[/, "", h)
        sub(/^v/, "", h)
        if (substr(h, 1, length(v)) == v) {
            rest = substr(h, length(v) + 1)
            if (rest == "" || rest !~ /^[0-9A-Za-z.+-]/) found = 1
        }
        next
    }
    found {
        n++
        line[n] = $0
        if ($0 ~ /[^[:space:]]/) {
            if (!first) first = n
            last = n
        }
    }
    END {
        if (!first) exit 1
        for (i = first; i <= last; i++) print line[i]
    }
' "${file}"
