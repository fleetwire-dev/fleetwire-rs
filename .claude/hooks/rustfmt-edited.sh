#!/usr/bin/env bash
# PostToolUse(Edit|Write): format the one .rs file that was just edited.
# The agent's shell may not have ~/.cargo/bin on PATH, so it is added here.
# A formatting failure is reported to the agent and never blocks the edit.
set -u
export PATH="$HOME/.cargo/bin:$PATH"
f=$(python3 -c 'import sys,json; print(json.load(sys.stdin).get("tool_input",{}).get("file_path",""))' 2>/dev/null)
case "$f" in
  *.rs)
    [ -f "$f" ] || exit 0
    rustfmt --edition 2024 "$f" 2>&1 | head -20 >&2
    ;;
esac
exit 0
