#!/usr/bin/env bash
# Uploads files to a GitHub release, draft or not, by release id, replacing
# assets of the same name (so a re-run job can upload again).
#
# Uses the REST API through curl and jq rather than `gh release upload`, so it
# runs the same way in the ubuntu:22.04 container (no gh), on macOS and in Git
# Bash on Windows, and does not depend on looking a draft up by its tag.
#
# Requires: curl, jq; GH_TOKEN with contents: write; GITHUB_REPOSITORY;
# GITHUB_API_URL (set by Actions; defaults to https://api.github.com).
#
# Usage: scripts/upload-release-assets.sh --release-id ID --upload-url URL FILE...
#   URL is the release's `upload_url` (with or without its {?name,label} suffix).

set -euo pipefail

die() {
    echo "upload-release-assets: error: $*" >&2
    exit 1
}

release_id=""
upload_url=""
while [ $# -gt 0 ]; do
    case "$1" in
        --release-id) [ $# -ge 2 ] || die "--release-id needs a value"; release_id="$2"; shift 2 ;;
        --upload-url) [ $# -ge 2 ] || die "--upload-url needs a value"; upload_url="$2"; shift 2 ;;
        --) shift; break ;;
        -*) die "unknown option: $1" ;;
        *) break ;;
    esac
done
[ -n "${release_id}" ] || die "--release-id is required"
[ -n "${upload_url}" ] || die "--upload-url is required"
[ $# -gt 0 ] || die "no files to upload"
[ -n "${GH_TOKEN:-}" ] || die "GH_TOKEN is not set"
[ -n "${GITHUB_REPOSITORY:-}" ] || die "GITHUB_REPOSITORY is not set"

api="${GITHUB_API_URL:-https://api.github.com}"
upload_url="${upload_url%%\{*}"

curl_api() {
    curl --proto "=https" --tlsv1.2 -sS --fail-with-body --retry 3 \
        -H "Authorization: Bearer ${GH_TOKEN}" \
        -H "Accept: application/vnd.github+json" \
        -H "X-GitHub-Api-Version: 2022-11-28" \
        "$@"
}

existing="$(curl_api "${api}/repos/${GITHUB_REPOSITORY}/releases/${release_id}/assets?per_page=100")"

for file in "$@"; do
    [ -f "${file}" ] || die "${file} does not exist"
    name="$(basename "${file}")"
    case "${name}" in
        *[!A-Za-z0-9._+-]*) die "asset name '${name}' has characters GitHub would rewrite" ;;
    esac

    old_id="$(jq -r --arg name "${name}" '.[] | select(.name == $name) | .id' <<< "${existing}")"
    if [ -n "${old_id}" ]; then
        echo "Replacing existing asset ${name} (${old_id})"
        curl_api -X DELETE "${api}/repos/${GITHUB_REPOSITORY}/releases/assets/${old_id}"
    fi

    echo "Uploading ${name} ($(wc -c < "${file}" | tr -d ' ') bytes)"
    curl_api -X POST \
        -H "Content-Type: application/octet-stream" \
        --data-binary "@${file}" \
        "${upload_url}?name=${name}" > /dev/null
done
