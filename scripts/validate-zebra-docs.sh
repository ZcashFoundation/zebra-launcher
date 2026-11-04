#!/usr/bin/env bash
# Validates generated offline Zebra documentation. With no --channel, requires
# stable, beta and nightly. Development fallback markers are errors unless
# --allow-fallback is given for checking the local development resources.
# External hyperlinks are fine; remotely loaded assets are not. Every HTML file
# must carry the same restrictive per-document CSP as defense in depth.
#
# Usage: scripts/validate-zebra-docs.sh --root ZEBRA_DOCS_DIRECTORY \
#          [--channel stable|beta|nightly] [--allow-fallback]

set -euo pipefail

readonly DOCS_CSP="default-src 'none'; base-uri 'none'; connect-src 'self'; font-src 'self' data:; form-action 'none'; frame-src 'none'; img-src 'self' data:; media-src 'none'; object-src 'none'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; worker-src 'none'"
readonly DOCS_CSP_META="<meta http-equiv=\"Content-Security-Policy\" content=\"${DOCS_CSP}\">"

root=""
channel=""
allow_fallback=false

die() {
    echo "validate-zebra-docs: error: $*" >&2
    exit 1
}

while [ $# -gt 0 ]; do
    case "$1" in
        --root) [ $# -ge 2 ] || die "--root needs a directory"; root="$2"; shift 2 ;;
        --channel) [ $# -ge 2 ] || die "--channel needs a value"; channel="$2"; shift 2 ;;
        --allow-fallback) allow_fallback=true; shift ;;
        -h | --help) sed -n '2,/^$/{s/^# \{0,1\}//p;}' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) die "unknown argument: $1" ;;
    esac
done

[ -n "${root}" ] || die "--root is required"
[ -d "${root}" ] || die "${root} is not a directory"
if [ -n "${channel}" ]; then
    case "${channel}" in
        stable | beta | nightly) channels=("${channel}") ;;
        *) die "--channel must be stable, beta or nightly" ;;
    esac
else
    channels=(stable beta nightly)
fi

for name in "${channels[@]}"; do
    for relative in book/index.html config.html changes.html; do
        file="${root}/${name}/${relative}"
        if [ ! -f "${file}" ] || [ -L "${file}" ] || [ ! -s "${file}" ]; then
            die "${file} is missing, empty, or not a regular file"
        fi
    done

    # Config and changes deliberately embed their CSS and theme script instead
    # of depending on book assets or any other file.
    for file in "${root}/${name}/config.html" "${root}/${name}/changes.html"; do
        if grep -n -i -E '<(script|link|img|iframe|source)([[:space:]]|>).*([[:space:]])(src|href)[[:space:]]*=' "${file}"; then
            die "${file} loads an external or relative asset instead of being self-contained"
        fi
    done
done

while IFS= read -r -d '' file; do
    if ! grep -F -q -- "${DOCS_CSP_META}" "${file}"; then
        die "${file} is missing the required Content-Security-Policy meta tag"
    fi
done < <(find "${root}" -type f -name '*.html' -print0)

if [ "${allow_fallback}" = false ] && grep -R -l -F 'ZEAL DEVELOPMENT FALLBACK' "${root}"; then
    die "generated documentation still contains development fallback resources"
fi

# Match actual asset-bearing elements, not ordinary <a href> links to upstream
# references. Protocol-relative URLs are remote too. The CSP remains the
# defense-in-depth guarantee when obfuscation or an encoding evades these scans.
if grep -R -n -i -E --include='*.html' "<(script|img|iframe|source|link|object|embed|video|audio|track)([[:space:]]|>)[^>]*(src|href|data|poster)[[:space:]]*=[[:space:]]*[\"']?(https?:)?//" "${root}"; then
    die "documentation HTML loads a remote or protocol-relative asset"
fi
if grep -R -n -i -E --include='*.html' '<(frame|iframe|object|embed|video|audio)([[:space:]]|>)' "${root}"; then
    die "documentation HTML contains a prohibited frame, object, or media element"
fi
if grep -R -n -i -E --include='*.html' --include='*.css' "(@import|url\()[^;]*[\"']?(https?:)?//" "${root}"; then
    die "documentation CSS loads a remote or protocol-relative style, font, or image"
fi

remote_request_pattern="(fetch|import|importScripts|WebSocket|EventSource|sendBeacon|Worker|SharedWorker)[[:space:]]*\([^)]*[\"']((https?|wss?):)?//|\.open[[:space:]]*\([^)]*,[^)]*[\"'](https?:)?//|\.(src|poster|data)[[:space:]]*=[[:space:]]*[\"'](https?:)?//"
if grep -R -n -i -E --include='*.html' --include='*.js' "${remote_request_pattern}" "${root}"; then
    die "documentation script initiates a remote request"
fi
if grep -R -n -i -E --include='*.html' --include='*.js' 'cdn\.knowledgelens\.ai|api\.knowledgelens\.ai|lens-init\.js' "${root}"; then
    die "documentation contains the remote documentation assistant"
fi

printf 'Validated offline Zebra documentation in %s (%s)\n' "${root}" "${channels[*]}"
