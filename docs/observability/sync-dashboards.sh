#!/usr/bin/env bash
# Re-vendor Zebra's Grafana dashboards for the zebrad the launcher pins.
#
# Usage:
#   docs/observability/sync-dashboards.sh                  # download from GitHub
#   docs/observability/sync-dashboards.sh /path/to/zebra   # copy from a Zebra checkout
#
# The files come from the commit in ZEBRAD_COMMIT, the commit the tag v<ZEBRAD_VERSION>
# points to, so the vendored dashboards are the ones the bundled zebrad was built with
# even if the tag were ever moved. Without a ZEBRAD_COMMIT file the tag is used instead.
# With a checkout, the files are read from that commit when the checkout has it,
# otherwise from the working tree (with a warning). The files are copied unchanged.
#
# Afterwards it prints the file, uid and title of every dashboard. When they differ from
# the table in README.md, update the table, and re-run the "Known gaps" comparison the
# README describes.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$here/../.." && pwd)"
version="$(tr -d '[:space:]' < "$repo_root/ZEBRAD_VERSION")"
tag="v$version"
if [[ -f "$repo_root/ZEBRAD_COMMIT" ]]; then
  ref="$(tr -d '[:space:]' < "$repo_root/ZEBRAD_COMMIT")"
  [[ "$ref" =~ ^[0-9a-f]{40}$ ]] || { echo "error: ZEBRAD_COMMIT is not a full lowercase SHA: $ref" >&2; exit 1; }
  ref_label="commit ${ref:0:12} ($tag)"
else
  ref="$tag"
  ref_label="$tag"
fi
src_path="docker/observability/grafana/dashboards"
dest="$here/grafana/dashboards"

dashboards=(
  block_verification
  checkpoint_verification
  errors
  mempool
  network_health
  network_messages
  peers
  rocksdb
  rpc_metrics
  rpc_tracing
  syncer
  transaction-verification
  value_pools
  zebra_overview
)

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

zebra_dir="${1:-}"
for name in "${dashboards[@]}"; do
  file="$name.json"
  if [[ -z "$zebra_dir" ]]; then
    url="https://raw.githubusercontent.com/ZcashFoundation/zebra/$ref/$src_path/$file"
    curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 -o "$tmp/$file" "$url"
  elif git -C "$zebra_dir" cat-file -e "$ref^{commit}" 2> /dev/null; then
    git -C "$zebra_dir" show "$ref:$src_path/$file" > "$tmp/$file"
  else
    echo "warning: $zebra_dir has no $ref_label; copying $file from its working tree" >&2
    cp "$zebra_dir/$src_path/$file" "$tmp/$file"
  fi
done

python3 -I -c 'import json, sys; [json.load(open(f)) for f in sys.argv[1:]]' "$tmp"/*.json

# Report dashboards Zebra added that are not in the list above.
if [[ -n "$zebra_dir" && -d "$zebra_dir/$src_path" ]]; then
  for f in "$zebra_dir/$src_path"/*.json; do
    n="$(basename "$f" .json)"
    if [[ ! " ${dashboards[*]} " == *" $n "* ]]; then
      echo "note: $n.json exists in $zebra_dir but is not synced; add it to this script" >&2
    fi
  done
fi

cp "$tmp"/*.json "$dest/"
echo "Synced ${#dashboards[@]} dashboards from zebra $ref_label into $dest"
echo
echo "file, uid and title (compare with the table in README.md):"
python3 -I -c '
import json, os, sys
for f in sys.argv[1:]:
    with open(f) as fh:
        d = json.load(fh)
    print("  " + os.path.basename(f) + "\t" + d.get("uid", "") + "\t" + d.get("title", ""))
' "$dest"/*.json
