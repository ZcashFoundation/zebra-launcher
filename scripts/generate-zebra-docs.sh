#!/usr/bin/env bash
# Renders one channel of Zebra's documentation for offline use. The Linux
# sidecar supplies the commented default config and proves the version/commit;
# the book and changelog are read from git objects at that exact commit.
# Stable and beta use their version's CHANGELOG section when present. A beta tag
# without one falls back to Zebra's official tagged release notes through gh.
# Nightly and custom-ref stable builds list commit subjects since the nearest
# reachable v*.*.* tag, without author metadata.
#
# The output is OUT/CHANNEL/{book/index.html,config.html,changes.html}, plus a
# manifest.txt. The book's optional third-party assistant is removed. Every HTML
# document has a CSP permitting only local mdBook assets and local search requests.
# All three entry points support ?theme=light and ?theme=dark without remote resources.
#
# Usage: scripts/generate-zebra-docs.sh --channel stable|beta|nightly \
#          --version X.Y.Z[-PRERELEASE] --commit FULL_SHA --binary ZEBRAD \
#          --out DIRECTORY [--source-dir GIT_CHECKOUT] [--development-ref REF]
#
# An existing --source-dir must have the requested commit checked out. Without
# it, the script fetches Zebra into a temporary directory. --development-ref is
# stable-only and skips the release-tag identity check for custom-ref builds.
# mdbook and mdbook-mermaid must be on PATH (see install-zebra-docs-tools.sh).

set -euo pipefail

readonly ZEBRA_GIT="https://github.com/ZcashFoundation/zebra"
readonly DOCS_CSP="default-src 'none'; base-uri 'none'; connect-src 'self'; font-src 'self' data:; form-action 'none'; frame-src 'none'; img-src 'self' data:; media-src 'none'; object-src 'none'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; worker-src 'none'"

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
channel=""
version=""
commit=""
binary=""
out_root=""
source_dir=""
development_ref=""

die() {
    echo "generate-zebra-docs: error: $*" >&2
    exit 1
}

# shellcheck source-path=SCRIPTDIR source=lib.sh
# shellcheck disable=SC1091 # lib.sh is a separate input to the repository lint
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

usage() {
    sed -n '2,/^$/{s/^# \{0,1\}//p;}' "${BASH_SOURCE[0]}"
}

while [ $# -gt 0 ]; do
    case "$1" in
        --channel) [ $# -ge 2 ] || die "--channel needs a value"; channel="$2"; shift 2 ;;
        --version) [ $# -ge 2 ] || die "--version needs a value"; version="$2"; shift 2 ;;
        --commit) [ $# -ge 2 ] || die "--commit needs a value"; commit="$2"; shift 2 ;;
        --binary) [ $# -ge 2 ] || die "--binary needs a path"; binary="$2"; shift 2 ;;
        --out) [ $# -ge 2 ] || die "--out needs a directory"; out_root="$2"; shift 2 ;;
        --source-dir) [ $# -ge 2 ] || die "--source-dir needs a directory"; source_dir="$2"; shift 2 ;;
        --development-ref) [ $# -ge 2 ] || die "--development-ref needs a value"; development_ref="$2"; shift 2 ;;
        -h | --help) usage; exit 0 ;;
        *) usage >&2; die "unknown argument: $1" ;;
    esac
done

case "${channel}" in
    stable | beta | nightly) ;;
    *) die "--channel must be stable, beta or nightly" ;;
esac
if [ -n "${development_ref}" ]; then
    [ "${channel}" = stable ] || die "--development-ref is supported only for custom stable builds"
    check_git_ref "${development_ref}"
fi
check_zebrad_version "${version}"
check_commit "${commit}"
[ -f "${binary}" ] || die "sidecar ${binary} does not exist"
[ -n "${out_root}" ] || die "--out is required"
for tool in git tar mdbook mdbook-mermaid; do
    command -v "${tool}" > /dev/null 2>&1 || die "${tool} is required but not on PATH"
done

# A base-version match is not enough: Zebra's build metadata must also contain
# a hexadecimal prefix of the expected commit, either as +git.<prefix> or as
# git-describe's +<count>.g<prefix>. Other build metadata is preserved.
check_sidecar() {
    local output reported base metadata part candidate found=false
    local -a metadata_parts
    output="$("${binary}" --version 2>&1)" || die "${binary} --version failed: ${output}"
    reported="${output%%$'\n'*}"
    reported="${reported%$'\r'}"
    base="zebrad ${version}"
    case "${reported}" in
        "${base}"+*) metadata="${reported#"${base}"+}" ;;
        *) die "sidecar reports '${reported}', expected '${base}+<build metadata>'" ;;
    esac

    IFS='.' read -r -a metadata_parts <<< "${metadata}"
    for part in "${metadata_parts[@]}"; do
        candidate="${part#g}"
        if [[ "${candidate}" =~ ^[0-9a-f]{7,40}$ ]] && [[ "${commit}" == "${candidate}"* ]]; then
            found=true
            break
        fi
    done
    [ "${found}" = true ] ||
        die "sidecar version '${reported}' has no commit prefix matching ${commit}"
    printf '%s\n' "${reported}"
}

work="$(mktemp -d)"
trap 'rm -rf "${work}"' EXIT
chmod +x "${binary}"
reported_version="$(check_sidecar)"

if [ -z "${source_dir}" ]; then
    source_dir="${work}/zebra"
    git init -q "${source_dir}"
    git -C "${source_dir}" remote add origin "${ZEBRA_GIT}"
    if [ "${channel}" = nightly ] || [ -n "${development_ref}" ]; then
        # Tags plus recent history usually avoid a full clone. Deepen only when
        # the nearest version tag is still behind the shallow boundary.
        git -C "${source_dir}" fetch -q --depth 128 origin \
            "${commit}" '+refs/tags/v*:refs/tags/v*' ||
            die "could not fetch Zebra commit ${commit} and its tags"
    else
        git -C "${source_dir}" fetch -q --depth 1 origin \
            "${commit}" "+refs/tags/v${version}:refs/tags/v${version}" ||
            die "could not fetch Zebra ${version} at ${commit}"
    fi
    git -C "${source_dir}" -c advice.detachedHead=false checkout -q --detach "${commit}"
fi

[ -d "${source_dir}/.git" ] || die "${source_dir} is not a git checkout"
head="$(git -C "${source_dir}" rev-parse HEAD)" || die "cannot read ${source_dir} HEAD"
[ "${head}" = "${commit}" ] || die "${source_dir} is at ${head}, expected ${commit}"

if [ "${channel}" != nightly ] && [ -z "${development_ref}" ]; then
    tag="v${version}"
    tag_commit="$(git -C "${source_dir}" rev-parse "refs/tags/${tag}^{commit}" 2> /dev/null)" ||
        die "${source_dir} has no tag ${tag}"
    [ "${tag_commit}" = "${commit}" ] ||
        die "${tag} resolves to ${tag_commit}, expected ${commit}"
fi

changes_source=""
changes=""
if [ "${channel}" = nightly ] || [ -n "${development_ref}" ]; then
    nearest_tag() {
        git -C "${source_dir}" describe --tags --match 'v*.*.*' --abbrev=0 "${commit}" 2> /dev/null
    }
    tag="$(nearest_tag)" || tag=""
    if [ -z "${tag}" ] && [ "$(git -C "${source_dir}" rev-parse --is-shallow-repository)" = true ]; then
        git -C "${source_dir}" fetch -q --unshallow origin \
            "${commit}" '+refs/tags/v*:refs/tags/v*' ||
            die "could not deepen Zebra history to find a tag reachable from ${commit}"
        tag="$(nearest_tag)" || tag=""
    fi
    [ -n "${tag}" ] || die "no v*.*.* tag is reachable from ${channel} commit ${commit}"
    changes="$(git -C "${source_dir}" log --no-decorate --format='%h %s' "${tag}..${commit}")"
    if [ -n "${development_ref}" ]; then
        changes_source="Development ref ${development_ref}; commits since ${tag}"
        if ! grep -q '[^[:space:]]' <<< "${changes}"; then
            changes="${development_ref} resolves to ${commit} at ${tag}; no commits since that tag."
        fi
    else
        changes_source="Commits since ${tag}"
    fi
else
    # Zebra headings are '## [Zebra X.Y.Z](release URL) - DATE'. Normalize only
    # the product name, then use the launcher's exact-version section parser.
    normalized_changelog="${work}/CHANGELOG.md"
    git -C "${source_dir}" show "${commit}:CHANGELOG.md" |
        sed -E '/^## / s/(^## \[?)Zebra /\1/' > "${normalized_changelog}" ||
        die "Zebra commit ${commit} has no CHANGELOG.md"
    changes_status=0
    changes="$("${script_dir}/changelog-section.sh" "${version}" "${normalized_changelog}")" ||
        changes_status=$?
    if [ "${changes_status}" -eq 0 ]; then
        changes_source="Zebra ${version} changelog"
    elif [ "${channel}" = beta ] && command -v gh > /dev/null 2>&1; then
        # The 7.1.0-alpha.0 tag predates a beta CHANGELOG section. Its official
        # release notes are the only version-specific prose at that tag. Keep
        # preferring the changelog so a later beta pin uses it automatically.
        echo "generate-zebra-docs: warning: CHANGELOG.md has no ${version} section; using Zebra's tagged release notes" >&2
        changes="$(gh release view "v${version}" --repo ZcashFoundation/zebra --json body --jq '.body // empty')" ||
            die "CHANGELOG.md has no ${version} section and the tagged release notes are unavailable"
        changes_source="Zebra ${version} tagged release notes (no CHANGELOG.md section at ${commit})"
    else
        die "CHANGELOG.md has no non-empty section for Zebra ${version}"
    fi
fi
grep -q '[^[:space:]]' <<< "${changes}" || die "empty changes for ${channel}"

# Archive committed files instead of trusting potentially modified worktree
# files in a caller-supplied checkout. The book includes repository-root README
# and CONTRIBUTING files, so archive the whole tree rather than only book/.
mkdir -p "${work}/source"
git -C "${source_dir}" archive "${commit}" | tar -x -C "${work}/source" ||
    die "could not archive Zebra source at ${commit}"
book="${work}/source/book"
[ -f "${book}/book.toml" ] || die "Zebra commit ${commit} has no book/book.toml"

mkdir -p "${out_root}"
out_root="$(cd "${out_root}" && pwd)"
[ "${out_root}" != / ] || die "--out must not be the filesystem root"
channel_out="${out_root}/${channel}"
rm -rf "${channel_out}"
mkdir -p "${channel_out}"

# The generated default contains XDG_CACHE_HOME. Use an isolated home, then
# replace that documentation-only prefix with ~ so no runner account or path is
# published. The remaining TOML is exactly what this sidecar generated.
docs_home="${work}/home"
config_toml="${work}/zebrad.toml"
HOME="${docs_home}" XDG_CACHE_HOME="${docs_home}/.cache" XDG_CONFIG_HOME="${docs_home}/.config" \
    "${binary}" generate -o "${config_toml}" || die "${binary} generate failed"
[ -s "${config_toml}" ] || die "${binary} generated an empty config"
config_contents="$(< "${config_toml}")"
config_contents="${config_contents//"${docs_home}"/'~'}"
printf '%s\n' "${config_contents}" > "${config_toml}"
if grep -Fq "${work}" "${config_toml}"; then
    die "generated config still contains a temporary build path"
fi

# Remove the opt-in remote KnowledgeLens loader entirely. Mermaid and every
# other book asset stay local. The additional script applies the launcher's
# explicit iframe theme after mdBook has initialized its own theme.
rm -f "${book}/lens-init.js"
sed -i -E '
    /^additional-js[[:space:]]*=/ {
        s/"lens-init\.js"//g
        s/\[[[:space:]]*,[[:space:]]*/[/g
        s/,[[:space:]]*,/,/g
        s/,[[:space:]]*\]/]/g
        s/\[/["zeal-theme.js", /
        s/,[[:space:]]*\]/]/g
    }
' "${book}/book.toml"
grep -Eq '^additional-js[[:space:]]*=.*"zeal-theme\.js"' "${book}/book.toml" ||
    die "could not add the offline theme script to book.toml"
if grep -Fq 'lens-init.js' "${book}/book.toml"; then
    die "could not remove the remote documentation assistant from book.toml"
fi
cat > "${book}/zeal-theme.js" <<'EOF'
(() => {
  const theme = new URLSearchParams(window.location.search).get("theme");
  if (theme !== "light" && theme !== "dark") return;

  const root = document.documentElement;
  root.classList.remove("light", "dark", "coal", "navy", "ayu", "rust");
  root.classList.add(theme);
  root.dataset.theme = theme;
  try {
    localStorage.setItem("mdbook-theme", theme);
  } catch {
    // A sandboxed iframe can have opaque-origin storage disabled.
  }
})();
EOF

mdbook_log="${work}/mdbook.log"
mdbook build "${book}" --dest-dir "${channel_out}/book" 2>&1 | tee "${mdbook_log}" ||
    die "mdbook failed for ${channel} ${version}"
if grep -q '^ERROR' "${mdbook_log}"; then
    die "mdbook reported errors for ${channel} ${version}; refusing an incomplete book"
fi

# Put the policy before any mdBook resource or script. Replace the no-JavaScript
# TOC iframe with a link to the same local page so navigation works without
# frames. The repository README embeds a remote logo and live CI badges. Preserve
# their alternative text instead of shipping stale images or network requests.
# The validator requires this CSP and rejects remote assets or unfamiliar forms.
find "${channel_out}/book" -type f -name '*.html' -exec sed -i \
    "/<head>/a\\  <meta http-equiv=\"Content-Security-Policy\" content=\"${DOCS_CSP}\">" {} +
find "${channel_out}/book" -type f -name '*.html' -exec sed -i -E '
    s#<iframe class="sidebar-iframe-outer" src="([^"]*)"></iframe>#<a class="sidebar-iframe-outer" href="\1">Table of contents</a>#g
    s#<img src="https?://[^"]*" alt="([^"]*)"[^>]*>#\1#g
    s#<img alt="([^"]*)" src="https?://[^"]*"[^>]*>#\1#g
    s#<img src="https?://[^"]*"[^>]*>#[Offline image omitted]#g
' {} +

escape_html() {
    sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g' \
        -e 's/"/\&quot;/g' -e "s/'/\&#39;/g"
}

write_html() {
    local title="$1" meta="$2" body_file="$3" destination="$4"
    local escaped_title escaped_meta
    escaped_title="$(printf '%s' "${title}" | escape_html)"
    escaped_meta="$(printf '%s' "${meta}" | escape_html)"
    {
        cat <<EOF
<!doctype html>
<html lang="en">
<head>
  <meta http-equiv="Content-Security-Policy" content="${DOCS_CSP}">
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <meta name="color-scheme" content="light dark">
  <title>${escaped_title}</title>
  <style>
    :root { color-scheme: light; --bg: #ffffff; --fg: #202124; --muted: #5f6368; --code: #f3f4f6; --border: #d8dce1; }
    :root[data-theme="dark"] { color-scheme: dark; --bg: #151719; --fg: #e8eaed; --muted: #aeb4bc; --code: #202428; --border: #3c4249; }
    @media (prefers-color-scheme: dark) {
      :root:not([data-theme]) { color-scheme: dark; --bg: #151719; --fg: #e8eaed; --muted: #aeb4bc; --code: #202428; --border: #3c4249; }
    }
    * { box-sizing: border-box; }
    body { margin: 0 auto; padding: 2rem; max-width: 76rem; background: var(--bg); color: var(--fg); font: 16px/1.55 system-ui, sans-serif; }
    h1 { margin: 0 0 .35rem; font-size: 1.65rem; }
    .meta { margin: 0 0 1.5rem; color: var(--muted); }
    pre { margin: 0; padding: 1.25rem; overflow-x: auto; white-space: pre-wrap; overflow-wrap: anywhere; border: 1px solid var(--border); border-radius: .45rem; background: var(--code); font: 13px/1.55 ui-monospace, SFMono-Regular, Consolas, monospace; }
    code { font-family: ui-monospace, SFMono-Regular, Consolas, monospace; }
  </style>
  <script>
    (() => {
      const theme = new URLSearchParams(location.search).get("theme");
      if (theme === "light" || theme === "dark") document.documentElement.dataset.theme = theme;
    })();
  </script>
</head>
<body>
  <main>
    <h1>${escaped_title}</h1>
    <p class="meta">${escaped_meta}</p>
    <pre>
EOF
        escape_html < "${body_file}"
        cat <<'EOF'
</pre>
  </main>
</body>
</html>
EOF
    } > "${destination}"
}

changes_file="${work}/changes.txt"
printf '%s\n' "${changes}" > "${changes_file}"
write_html "Zebra ${channel} changes" \
    "${changes_source} · zebrad ${version} · commit ${commit}" \
    "${changes_file}" "${channel_out}/changes.html"
write_html "Zebra ${channel} default configuration" \
    "${reported_version} · Linux x86_64 · documentation home paths use ~" \
    "${config_toml}" "${channel_out}/config.html"

cat > "${channel_out}/manifest.txt" <<EOF
channel=${channel}
zebrad_version=${version}
zebrad_commit=${commit}
zebrad_reported_version=${reported_version}
changes_source=${changes_source}
EOF

"${script_dir}/validate-zebra-docs.sh" --root "${out_root}" --channel "${channel}"
printf 'Generated %s documentation for zebrad %s (%s) in %s\n' \
    "${channel}" "${version}" "${commit}" "${channel_out}"
