#!/usr/bin/env python3
"""PreToolUse(Bash) hook: refuse bulk staging (`git add -A`, `git add .` and their spellings).

Why: a working tree can hold a token file or a raw API response with a VIN and coordinates in it, and one bulk
add can put them in a commit. AGENTS.md: stage explicit paths and check `git diff --cached`.

The first version matched one regular expression against the whole command and missed these (measured
2026-11-15): `git -C <dir> add -A`, `git -c k=v add .`, `git add ./`, `git add :/`, `git add *`, `git add -Av`,
`git add -fA`. This version splits the command into shell words, finds each `git ... add` (or `stage`) call past
git's global options, and looks at its flags and pathspecs.

Not covered, by design: `git add -u` and `git commit -a` (tracked files only, so an untracked capture output is not
picked up), and paths fed from another command (`... | xargs git add`).

Usage: block_bulk_add.py            reads the hook JSON on stdin; exit 2 with a reason when the command is refused
       block_bulk_add.py --self-test
"""
import json
import re
import shlex
import sys

# Git global options that take a separate value word (`git -C dir add`), so the value is not the subcommand.
GLOBAL_WITH_VALUE = {"-C", "-c", "--git-dir", "--work-tree", "--namespace", "--exec-path", "--super-prefix", "--config-env"}
SEPARATORS = {"&&", "||", ";", "|", "&", "(", ")", "{", "}", "!", "then", "do", "else"}
WHOLE_TREE = {".", "*", "**", ":/", ":/*", ":/.", ":(top)", ":(top)*", ":(top).", ":!", ""}
SPLIT = re.compile(r"(&&|\|\||;|\||&|\n)")


def words(command):
    """Shell words of the command with operators as their own words; None when the text cannot be split."""
    spaced = SPLIT.sub(lambda m: " ; " if m.group(1) == "\n" else f" {m.group(1)} ", command)
    try:
        raw = shlex.split(spaced, comments=False, posix=True)
    except ValueError:
        return None
    # Subshell parentheses stick to the neighbouring word: `(git add .)`. A pathspec magic word keeps its own: `:(top)`.
    return [w.lstrip("(") if ":(" in w else w.lstrip("(").rstrip(")") for w in raw]


def is_whole_tree(pathspec):
    p = pathspec
    while p.endswith("/") and p not in (":/",):
        p = p[:-1]
    if p.startswith("./"):
        p = p[2:] or "."
    return p in WHOLE_TREE


def bulk_add_reason(command):
    """Reason string when the command stages the whole tree, else None."""
    ws = words(command)
    if ws is None:
        # Unbalanced quotes (heredoc bodies and the like): fall back to the original pattern.
        if re.search(r"git\s+(.*\s)?(add|stage)\s+(.*\s)?(-A|--all|\.)(\s|$)", command):
            return "git add -A/./--all"
        return None
    i, n = 0, len(ws)
    while i < n:
        if ws[i] != "git" and not ws[i].endswith("/git"):
            i += 1
            continue
        i += 1
        while i < n and ws[i].startswith("-") and ws[i] not in SEPARATORS:
            i += 2 if ws[i] in GLOBAL_WITH_VALUE else 1
        if i >= n or ws[i] not in ("add", "stage"):
            continue
        i += 1
        paths_only = False
        while i < n and ws[i] not in SEPARATORS:
            w = ws[i]
            i += 1
            if not paths_only and w == "--":
                paths_only = True
            elif not paths_only and w.startswith("--"):
                if w in ("--all", "--no-ignore-removal"):
                    return f"git add {w}"
            elif not paths_only and w.startswith("-") and len(w) > 1:
                if "A" in w[1:]:
                    return f"git add {w}"
            elif is_whole_tree(w):
                return f"git add {w or '(empty pathspec)'}"
    return None


def self_test():
    blocked = ["git add -A", "git add .", "git add --all", "git add -A crates", "cd x && git add .", "git -C /repo add -A",
               "git -c core.x=y add .", "git add ./", "git add -- .", "git add :/", 'git add "*"', "git add *",
               "git add -Av", "git add -fA", "git stage -A", "git add crates/a.rs .", "true; git add .\ngit commit -m x",
               "/usr/bin/git add -A", "git --no-pager add .", "git add --no-ignore-removal x", "(git add .)",
               "git add ':(top)'", "git -C a -c b=c add --all"]
    allowed = ["git add -u", "git commit -am msg", "git add ./crates/a.rs", "git add .gitignore", "git add -p",
               "git stash -u", 'echo "git add -A"', "git add crates/ scripts/x.py", "git add -f target/out.txt",
               "git log --all", "git diff -A", "git add -- -A.txt", "git add .claude/hooks/x.sh", "ls -A . && git status",
               "git commit -m 'git add .'", "git add ..hidden", "grep -A 3 add .", "git add -N crates/a.rs"]
    fails = [f"should block: {c}" for c in blocked if bulk_add_reason(c) is None]
    fails += [f"should allow: {c}" for c in allowed if bulk_add_reason(c) is not None]
    # Negative control: the old single-regex check must miss the spellings this version exists for.
    old = re.compile(r"git[\s]+add[\s]+(.*[\s])?(-A|--all|\.)([\s]|$)")
    if old.search("git -C /repo add -A") or old.search("git add -Av") or old.search("git add ./"):
        fails.append("control: the old pattern unexpectedly catches the bypass cases")
    if fails:
        sys.stderr.write("block_bulk_add self-test FAILED:\n  " + "\n  ".join(fails) + "\n")
        return 1
    print(f"block_bulk_add self-test: ok ({len(blocked)} blocked, {len(allowed)} allowed)")
    return 0


def main(argv):
    if argv == ["--self-test"]:
        return self_test()
    try:
        command = json.load(sys.stdin).get("tool_input", {}).get("command", "")
    except ValueError:
        return 0
    why = bulk_add_reason(command) if isinstance(command, str) else None
    if why:
        sys.stderr.write(f"blocked: '{why}' stages the whole tree; stage explicit paths instead (see AGENTS.md)\n")
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
