#!/usr/bin/env bash
# Re-vendor Zebra's Grafana dashboards for the zebrad version pinned in ZEBRAD_VERSION.
#
# Usage:
#   docs/observability/sync-dashboards.sh                  # download from GitHub
#   docs/observability/sync-dashboards.sh /path/to/zebra   # copy from a Zebra checkout
#
# With a checkout, the files are read from the `v<ZEBRAD_VERSION>` tag when it exists
# there, otherwise from the working tree (with a warning). The files are copied unchanged.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$here/../.." && pwd)"
version="$(tr -d '[:space:]' < "$repo_root/ZEBRAD_VERSION")"
tag="v$version"
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
    url="https://raw.githubusercontent.com/ZcashFoundation/zebra/$tag/$src_path/$file"
    curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 -o "$tmp/$file" "$url"
  elif git -C "$zebra_dir" rev-parse --verify --quiet "refs/tags/$tag" > /dev/null; then
    git -C "$zebra_dir" show "$tag:$src_path/$file" > "$tmp/$file"
  else
    echo "warning: $zebra_dir has no tag $tag; copying $file from its working tree" >&2
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
echo "Synced ${#dashboards[@]} dashboards from zebra $tag into $dest"
