#!/usr/bin/env bash
# Signs the Tauri updater artifacts of one target with the minisign updater key,
# in a process of its own, after `cargo tauri bundle` has finished.
#
# release.yml bundles with a throwaway key generated in the job, because
# tauri-cli signs every updater artifact itself when the updater is enabled
# and fails without a key, and because the bundler downloads and runs
# unverified AppImage tools (docs/SECURITY.md#release-pipeline-downloads):
# the real key must never be in its environment. The bundler's throwaway
# `.sig` files say which files it would have signed. This script, the only
# place the real key is used:
#
#   1. finds every `<artifact>.sig` under DIR, checks that each names its
#      artifact and the release version in its trusted comment (tauri-cli
#      writes `timestamp:<t>\tfile:<name>\tversion:<version>`), and deletes it;
#   2. writes TAURI_SIGNING_PRIVATE_KEY to a mode 0600 temporary file (deleted
#      on exit) and runs `cargo tauri signer sign --private-key-path <file>
#      --app-version VERSION <artifact>` for each artifact, which writes the
#      same trusted comment, so the updater's requireSignedVersion check
#      accepts it;
#   3. checks each new signature: the key ID of the public key PUBKEY, the
#      artifact's file name and `version:VERSION` in the trusted comment.
#
# The password goes to the signer in TAURI_SIGNING_PRIVATE_KEY_PASSWORD (what
# `--password` reads), not on the command line, where other processes could
# read it.
#
# Usage: scripts/sign-updater-artifacts.sh --dir DIR --version X.Y.Z --pubkey KEY
#   --dir      the bundle directory, target/<triple>/release/bundle
#   --version  the release version (tauri-cli's version for the bundle)
#   --pubkey   plugins.updater.pubkey from tauri.conf.json: the base64 minisign
#              public key, or a path to a file holding it
# Environment:
#   TAURI_SIGNING_PRIVATE_KEY           the private key (base64 file contents)
#   TAURI_SIGNING_PRIVATE_KEY_PASSWORD  its password (optional)
#   CARGO_TAURI                         a cargo-tauri binary to run instead of
#                                       `cargo tauri` (for testing)
# Requires: bash, find, base64, od, sed, mktemp.

set -euo pipefail

die() {
    echo "sign-updater-artifacts: error: $*" >&2
    exit 1
}

dir=""
version=""
pubkey=""
while [ $# -gt 0 ]; do
    case "$1" in
        --dir) [ $# -ge 2 ] || die "--dir needs a value"; dir="$2"; shift 2 ;;
        --version) [ $# -ge 2 ] || die "--version needs a value"; version="$2"; shift 2 ;;
        --pubkey) [ $# -ge 2 ] || die "--pubkey needs a value"; pubkey="$2"; shift 2 ;;
        -h | --help) sed -n '2,/^$/{s/^# \{0,1\}//p;}' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) die "unknown argument: $1" ;;
    esac
done
[ -n "${dir}" ] || die "--dir is required"
[ -n "${version}" ] || die "--version is required"
[ -n "${pubkey}" ] || die "--pubkey is required"
[ -d "${dir}" ] || die "${dir} is not a directory"
[ -n "${TAURI_SIGNING_PRIVATE_KEY:-}" ] || die "TAURI_SIGNING_PRIVATE_KEY is not set"
if [ -f "${pubkey}" ]; then
    pubkey="$(cat "${pubkey}")"
fi

# key_id_of_signature FILE: the 8-byte key ID, in hex, of a .sig file (base64
# of a minisign signature box; line 2 is base64 of "ED" + key ID + signature).
key_id_of_signature() {
    base64 -d < "$1" | sed -n 2p | base64 -d | od -An -tx1 -j2 -N8 | tr -d ' \n'
}

# trusted_comment FILE: the trusted comment of a .sig file.
trusted_comment() {
    base64 -d < "$1" | sed -n 's/^trusted comment: //p'
}

# check_comment SIG ARTIFACT: dies unless SIG's trusted comment names
# ARTIFACT's file name and, as its last field, `version:${version}`.
check_comment() {
    local sig="$1" artifact="$2" comment
    comment="$(trusted_comment "${sig}")" || die "${sig} is not a minisign signature"
    case "${comment}" in
        *$'\t'"file:${artifact##*/}"$'\t'"version:${version}") ;;
        *) die "${sig}: trusted comment '${comment}' does not end with file:${artifact##*/}<TAB>version:${version}" ;;
    esac
}

want_key_id="$(printf '%s' "${pubkey}" | base64 -d | sed -n 2p | base64 -d | od -An -tx1 -j2 -N8 | tr -d ' \n')" \
    || die "--pubkey is not a base64 minisign public key"
[ "${#want_key_id}" -eq 16 ] || die "--pubkey is not a base64 minisign public key"

# 1. The bundler's throwaway signatures: which files to sign.
artifacts=()
while IFS= read -r -d '' sig; do
    artifact="${sig%.sig}"
    [ -f "${artifact}" ] || die "${sig} has no artifact ${artifact}"
    check_comment "${sig}" "${artifact}"
    rm -f "${sig}"
    artifacts+=("${artifact}")
done < <(find "${dir}" -type f -name '*.sig' -print0 | sort -z)
[ "${#artifacts[@]}" -gt 0 ] \
    || die "no .sig files under ${dir}: did cargo tauri bundle run with bundle.createUpdaterArtifacts and a key?"

# 2. Sign with the real key, from a file only this process can read.
tmp="${RUNNER_TEMP:-${TMPDIR:-/tmp}}"
if command -v cygpath > /dev/null 2>&1; then
    tmp="$(cygpath -u "${tmp}")"
fi
key_file="$(umask 077 && mktemp "${tmp}/updater-key.XXXXXX")"
trap 'rm -f "${key_file}"' EXIT
(umask 077 && printf '%s' "${TAURI_SIGNING_PRIVATE_KEY}" > "${key_file}")

if [ -n "${CARGO_TAURI:-}" ]; then
    signer=("${CARGO_TAURI}" tauri signer sign)
else
    signer=(cargo tauri signer sign)
fi
for artifact in "${artifacts[@]}"; do
    echo "Signing ${artifact}"
    # The key must not also be in the environment: `--private-key-path`
    # conflicts with TAURI_SIGNING_PRIVATE_KEY (clap reads it as --private-key).
    env -u TAURI_SIGNING_PRIVATE_KEY -u TAURI_SIGNING_PRIVATE_KEY_PATH \
        "${signer[@]}" --private-key-path "${key_file}" --app-version "${version}" "${artifact}" \
        < /dev/null > /dev/null \
        || die "cargo tauri signer sign failed for ${artifact}"

    # 3. Check the new signature.
    sig="${artifact}.sig"
    [ -s "${sig}" ] || die "no signature was written for ${artifact}"
    got_key_id="$(key_id_of_signature "${sig}")" || die "${sig} is not a minisign signature"
    [ "${got_key_id}" = "${want_key_id}" ] \
        || die "${sig} has key ID ${got_key_id}, but the updater public key is ${want_key_id}"
    check_comment "${sig}" "${artifact}"
    echo "  ${sig##*/}: key ${want_key_id}, $(trusted_comment "${sig}" | tr '\t' ' ')"
done
rm -f "${key_file}"
echo "sign-updater-artifacts: signed ${#artifacts[@]} artifact(s) for ${version}"
