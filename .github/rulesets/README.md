# Repository rulesets

These files are the source of truth for the GitHub rulesets on this repository. They are applied by a
maintainer with `scripts/apply-rulesets.sh` (GitHub does not read them from the tree).

- `main.json` — the default branch: changes arrive by pull request only, all six CI jobs must pass on an
  up-to-date branch, history stays linear, no force-push, no deletion. No bypass actors, so the rules bind
  administrators too.
- `tags.json` — `v*` release tags cannot be moved or deleted once pushed.
