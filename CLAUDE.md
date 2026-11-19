@AGENTS.md

# Claude Code specifics

- **Hooks** (`.claude/settings.json`) are enforcement, not advice: `block_bulk_add.py` refuses
  `git add -A` and its spellings before the command runs, and `rustfmt-edited.sh` formats each
  `.rs` file after an edit. A refused Bash call refuses the **whole** command, so do not chain an
  edit and a check behind one; rerun the parts and look at `git status`.
- **Reviewer** (`.claude/agents/code-reviewer.md`): spawn it before a pull request with the diff
  range. It does not edit. `scripts/check_invariants.py` fails if its definition ever lists an
  edit tool (it keeps a shell, to run tests in a temporary worktree).
- **Scripted multi-file edits**: check every match count before writing anything. A script that
  asserts half-way leaves the tree half-edited.
- This file is context. The gates and hooks are what actually hold.
