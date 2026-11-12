#!/usr/bin/env bash
# .github/rulesets/*.json 을 저장소 규칙 묶음으로 올립니다. 같은 이름이 있으면 고치고 없으면 만듭니다.
# 사용: scripts/apply-rulesets.sh <owner/repo> [--dry-run]
# 규칙 묶음은 공개 저장소(또는 유료 요금제)에서만 걸립니다. 관리자 권한의 gh 로그인이 필요합니다.
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
