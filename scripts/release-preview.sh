#!/usr/bin/env bash
# 공개 전환·crates.io 게시 직전에 「밖에서 보이게 되는 것」을 한 화면으로 찍습니다. 아무것도 바꾸지 않습니다.
# 공개와 게시는 되돌릴 수 없는 일로 다룹니다 — 이 화면을 본 뒤에만 실행합니다.
# 사용: scripts/release-preview.sh [owner/repo]
set -euo pipefail
cd "$(dirname "$0")/.."
repo=${1:-}

echo "== 누가 썼다고 보이는가 =="
echo "커밋 작성자(이름 <메일>, 커밋 수):"
git log --format='%an <%ae>' | sort | uniq -c | sed 's/^/  /'
echo "커밋 메시지에 적힌 공동 작성자:"
git log --format='%b' | grep -i '^co-authored-by:' | sort | uniq -c | sed 's/^/  /' || true
echo "LICENSE-MIT 의 저작권 줄:  $(grep -m1 -i '^copyright' LICENSE-MIT || echo '(없음)')"
echo "Cargo.toml 의 authors:      $(grep -m1 '^authors' Cargo.toml || echo '(없음)')"
echo "Cargo.toml 의 repository:   $(grep -m1 '^repository' Cargo.toml || echo '(없음)')"

echo
echo "== 무엇이 보이는가 =="
echo "커밋 $(git rev-list --count HEAD)개 · 추적 파일 $(git ls-files | wc -l | tr -d ' ')개 · 태그 $(git tag | wc -l | tr -d ' ')개 · 로컬 브랜치 $(git branch --format='%(refname:short)' | wc -l | tr -d ' ')개"
echo "맨 위 폴더·파일:"
git ls-files | sed 's#/.*##' | sort | uniq -c | sed 's/^/  /'
echo "커밋 안 된 변경: $(git status --porcelain | wc -l | tr -d ' ')개 (0 이 아니면 지금 보이는 트리와 올라갈 트리가 다릅니다)"
echo "첫 커밋: $(git log --reverse --format='%h %ad %s' --date=short | head -1)"
echo "끝 커밋: $(git log -1 --format='%h %ad %s' --date=short)"

echo
echo "== crates.io 에 올라갈 묶음(fleetwire) =="
if grep -q '^publish = false' crates/fleetwire/Cargo.toml; then
  echo "publish = false 가 켜져 있습니다 — 지금은 게시되지 않습니다."
fi
if command -v cargo >/dev/null 2>&1; then
  cargo package -p fleetwire --allow-dirty --list 2>/dev/null | sed 's#/.*##' | sort | uniq -c | sed 's/^/  /' || echo "  (cargo package 목록을 얻지 못했습니다)"
else
  echo "  (이 셸에 cargo 가 없습니다 — VM 에서 cargo package -p fleetwire --list)"
fi

if [ -n "$repo" ] && command -v gh >/dev/null 2>&1; then
  echo
  echo "== GitHub 의 지금 상태($repo) =="
  gh repo view "$repo" --json visibility,isEmpty,forkCount,stargazerCount,description \
    --jq '"공개 상태 \(.visibility) · 비어 있음 \(.isEmpty) · 포크 \(.forkCount) · 별 \(.stargazerCount)\n설명: \(.description)"' || echo "  (조회하지 못했습니다)"
  echo "원격 브랜치: $(gh api "repos/$repo/branches" --jq '[.[].name] | join(", ")' 2>/dev/null || echo '(없음·조회 실패)')"
fi

echo
echo "되돌릴 수 없는 것: 공개된 동안 복제된 내용, crates.io 에 게시한 판(지울 수 없고 회수만 됩니다)."
