#!/usr/bin/env bash
# Prints the launcher version: `[workspace.package] version` in the root
# Cargo.toml, the only place it lives.
#
# Plain awk, so it runs in a bare ubuntu:22.04 container, on macOS and in Git
# Bash on Windows without Python or jq.
#
# Usage: scripts/workspace-version.sh

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

version="$(awk '
    /^\[/ { in_section = ($0 == "[workspace.package]") }
    in_section && /^version[[:space:]]*=/ {
        sub(/^version[[:space:]]*=[[:space:]]*"/, "")
        sub(/".*$/, "")
        print
        exit
    }
' "${repo_root}/Cargo.toml")"

if [ -z "${version}" ]; then
    echo "workspace-version: no [workspace.package] version in Cargo.toml" >&2
    exit 1
fi
printf '%s\n' "${version}"
