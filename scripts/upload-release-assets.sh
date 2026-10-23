#!/usr/bin/env bash
# Uploads files to a GitHub release, draft or not, by release id, replacing
# assets of the same name (so a re-run job can upload again).
#
# Every existing asset with the file's name is deleted first (a partial run
# can leave more than one), from the full, paginated asset list. The upload
# itself is never retried blindly: a retried POST after a server-side success
# gets 422 `already_exists`. Instead, when the POST fails, the asset list is
# read again: an uploaded asset with the file's name, size and (when GitHub
# reports one) sha256 digest counts as success; otherwise the leftovers are
# deleted and the file is uploaded once more.
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
assets_url="${api}/repos/${GITHUB_REPOSITORY}/releases/${release_id}/assets"

# curl_api ARGS...: an idempotent API request (GET, DELETE), retried on
# transient errors.
curl_api() {
    curl --proto "=https" --tlsv1.2 -sS --fail-with-body --retry 3 \
        -H "Authorization: Bearer ${GH_TOKEN}" \
        -H "Accept: application/vnd.github+json" \
        -H "X-GitHub-Api-Version: 2022-11-28" \
        "$@"
}

# list_assets: prints every asset of the release as one JSON array, reading
# pages of 100 until one comes back short.
list_assets() {
    local page=1 all='[]' batch count
    while :; do
        batch="$(curl_api "${assets_url}?per_page=100&page=${page}")"
        count="$(jq 'length' <<< "${batch}")"
        all="$(jq -s 'add' <<< "${all}${batch}")"
        [ "${count}" -eq 100 ] || break
        page=$((page + 1))
    done
    printf '%s\n' "${all}"
}

# asset_ids NAME: prints the id of every asset in ${existing} named NAME, one
# per line.
asset_ids() {
    jq -r --arg name "$1" '.[] | select(.name == $name) | .id' <<< "${existing}"
}

# delete_assets NAME: deletes every asset in ${existing} named NAME. A 404
# (already gone, e.g. a retried DELETE that had succeeded) is fine.
delete_assets() {
    local id code
    while IFS= read -r id; do
        [ -n "${id}" ] || continue
        [[ "${id}" =~ ^[0-9]+$ ]] || die "unexpected asset id '${id}' for $1"
        echo "Deleting existing asset $1 (${id})"
        code="$(curl --proto "=https" --tlsv1.2 -sS --retry 3 -o /dev/null -w '%{http_code}' -X DELETE \
            -H "Authorization: Bearer ${GH_TOKEN}" \
            -H "Accept: application/vnd.github+json" \
            -H "X-GitHub-Api-Version: 2022-11-28" \
            "${api}/repos/${GITHUB_REPOSITORY}/releases/assets/${id}")" || code=000
        case "${code}" in
            204 | 404) ;;
            *) die "deleting asset ${id} ($1) failed with HTTP ${code}" ;;
        esac
    done < <(asset_ids "$1")
}

sha256_of() {
    if command -v sha256sum > /dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    else
        shasum -a 256 "$1" | cut -d' ' -f1
    fi
}

# upload FILE NAME: one POST, no retries. Prints the HTTP status (000 when
# curl failed without one); the response body goes to ${response}.
upload() {
    local code
    : > "${response}"
    code="$(curl --proto "=https" --tlsv1.2 -sS -o "${response}" -w '%{http_code}' -X POST \
        -H "Authorization: Bearer ${GH_TOKEN}" \
        -H "Accept: application/vnd.github+json" \
        -H "X-GitHub-Api-Version: 2022-11-28" \
        -H "Content-Type: application/octet-stream" \
        --data-binary "@$1" \
        "${upload_url}?name=$2")" || true
    printf '%s\n' "${code:-000}"
}

# uploaded_matches NAME SIZE SHA256: true if ${existing} has exactly one asset
# named NAME, fully uploaded, with this size and, when GitHub reports a
# digest, this sha256.
uploaded_matches() {
    jq -e --arg name "$1" --argjson size "$2" --arg digest "sha256:$3" '
        [.[] | select(.name == $name)]
        | length == 1
          and .[0].state == "uploaded"
          and .[0].size == $size
          and (.[0].digest == null or .[0].digest == $digest)
    ' <<< "${existing}" > /dev/null
}

response="$(mktemp)"
trap 'rm -f "${response}"' EXIT

existing="$(list_assets)"

for file in "$@"; do
    [ -f "${file}" ] || die "${file} does not exist"
    name="$(basename "${file}")"
    case "${name}" in
        *[!A-Za-z0-9._+-]*) die "asset name '${name}' has characters GitHub would rewrite" ;;
    esac
    size="$(wc -c < "${file}" | tr -d ' ')"
    sha256="$(sha256_of "${file}")"

    delete_assets "${name}"

    echo "Uploading ${name} (${size} bytes)"
    code="$(upload "${file}" "${name}")"
    [ "${code}" = 201 ] && continue

    # The POST may have succeeded server-side (422 already_exists on a
    # connection that dropped and was retried by a proxy, or a 5xx/timeout
    # after the asset was stored). Look before uploading again.
    echo "Upload of ${name} returned HTTP ${code}: $(head -c 500 "${response}" 2> /dev/null)"
    case "${code}" in
        000 | 422 | 5??) ;;
        *) die "uploading ${name} failed with HTTP ${code}" ;;
    esac
    existing="$(list_assets)"
    if uploaded_matches "${name}" "${size}" "${sha256}"; then
        echo "${name} is on the release with the expected size and digest; treating the upload as done"
        continue
    fi
    delete_assets "${name}"
    echo "Uploading ${name} again"
    code="$(upload "${file}" "${name}")"
    [ "${code}" = 201 ] \
        || die "uploading ${name} failed twice, the last time with HTTP ${code}: $(head -c 500 "${response}" 2> /dev/null)"
done
