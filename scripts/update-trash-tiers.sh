#!/usr/bin/env bash
# Refresh the release-group rankings in assets/trash from TRaSH Guides
# (https://github.com/TRaSH-Guides/Guides, MIT license). Rebuild afterwards.
set -euo pipefail

dest="$(dirname "$0")/../assets/trash"
base="https://raw.githubusercontent.com/TRaSH-Guides/Guides/master/docs/json/sonarr/cf"
files=(
  anime-bd-tier-0{1..8}
  anime-web-tier-0{1..6}
  anime-lq-groups
  anime-raws
  anime-dual-audio
)

for f in "${files[@]}"; do
  curl -fsSL "$base/$f.json" -o "$dest/$f.json.tmp"
  mv "$dest/$f.json.tmp" "$dest/$f.json"
  echo "updated $f.json"
done
