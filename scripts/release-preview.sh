#!/usr/bin/env bash
# Prints, on one screen, what the outside world will see before a visibility change or a crates.io
# publish. It changes nothing. Publishing is treated as irreversible: read this output first.
# Usage: scripts/release-preview.sh [owner/repo]
set -euo pipefail
cd "$(dirname "$0")/.."
repo=${1:-}

echo "== Who appears as the author =="
echo "Commit authors (name <email>, commits):"
git log --format='%an <%ae>' | sort | uniq -c | sed 's/^/  /'
echo "Co-authors named in commit messages:"
git log --format='%b' | grep -i '^co-authored-by:' | sort | uniq -c | sed 's/^/  /' || true
echo "LICENSE-MIT copyright line:  $(grep -m1 -i '^copyright' LICENSE-MIT || echo '(none)')"
echo "Cargo.toml authors:          $(grep -m1 '^authors' Cargo.toml || echo '(none)')"
echo "Cargo.toml repository:       $(grep -m1 '^repository' Cargo.toml || echo '(none)')"

echo
echo "== What is visible =="
echo "$(git rev-list --count HEAD) commits · $(git ls-files | wc -l | tr -d ' ') tracked files · $(git tag | wc -l | tr -d ' ') tags · $(git branch --format='%(refname:short)' | wc -l | tr -d ' ') local branches"
echo "Top-level entries:"
git ls-files | sed 's#/.*##' | sort | uniq -c | sed 's/^/  /'
echo "Uncommitted changes: $(git status --porcelain | wc -l | tr -d ' ') (if not 0, the tree you see differs from the tree that ships)"
echo "First commit: $(git log --reverse --format='%h %ad %s' --date=short | head -1)"
echo "Last commit:  $(git log -1 --format='%h %ad %s' --date=short)"

echo
echo "== What crates.io would receive (fleetwire) =="
if grep -q '^publish = false' crates/fleetwire/Cargo.toml; then
  echo "publish = false is set — nothing would be published right now."
fi
if command -v cargo >/dev/null 2>&1; then
  cargo package -p fleetwire --allow-dirty --list 2>/dev/null | sed 's#/.*##' | sort | uniq -c | sed 's/^/  /' || echo "  (could not list the package)"
else
  echo "  (cargo is not on PATH — run: cargo package -p fleetwire --list)"
fi

if [ -n "$repo" ] && command -v gh >/dev/null 2>&1; then
  echo
  echo "== GitHub right now ($repo) =="
  gh repo view "$repo" --json visibility,isEmpty,forkCount,stargazerCount,description \
    --jq '"visibility \(.visibility) · empty \(.isEmpty) · forks \(.forkCount) · stars \(.stargazerCount)\ndescription: \(.description)"' || echo "  (lookup failed)"
  echo "Remote branches: $(gh api "repos/$repo/branches" --jq '[.[].name] | join(", ")' 2>/dev/null || echo '(none or lookup failed)')"
fi

echo
echo "Cannot be undone: anything cloned while public, and a version published to crates.io (it can be yanked, not deleted)."
