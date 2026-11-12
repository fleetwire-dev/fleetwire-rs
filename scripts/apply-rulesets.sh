#!/usr/bin/env bash
# Applies .github/rulesets/*.json as repository rulesets: updates one with the same name, creates it otherwise.
# Usage: scripts/apply-rulesets.sh <owner/repo> [--dry-run]
# Rulesets only bind on public repositories (or paid plans). Needs a gh login with admin rights.
set -euo pipefail
cd "$(dirname "$0")/.."
repo=${1:?usage: apply-rulesets.sh <owner/repo> [--dry-run]}
dry=${2:-}
existing=$(gh api "repos/$repo/rulesets" --jq '.[] | "\(.id)\t\(.name)"')
for f in .github/rulesets/*.json; do
  name=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["name"])' "$f")
  id=$(printf '%s\n' "$existing" | awk -F'\t' -v n="$name" '$2 == n { print $1 }')
  if [ -n "$id" ]; then verb=PUT; path="repos/$repo/rulesets/$id"; else verb=POST; path="repos/$repo/rulesets"; fi
  echo "$verb $path  <- $f ($name)"
  [ "$dry" = "--dry-run" ] || gh api -X "$verb" "$path" --input "$f" --jq '"  -> id \(.id) · \(.enforcement)"'
done
