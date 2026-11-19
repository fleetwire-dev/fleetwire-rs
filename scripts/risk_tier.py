#!/usr/bin/env python3
"""Sorts a change into a review tier by the paths it touches (REVIEW.md, "Depth").

The depth of a review is decided by what changed, not by who wrote it or how confident they
are. The author cannot lower a tier; a reviewer may raise one.

  quick     documentation, tests and agent instructions only: machine checks are enough
  standard  any other code: one reviewer with a fresh context
  deep      a path listed in DEEP below: two reviewers with split perspectives and a security
            pass

Usage:
  risk_tier.py <base> [head]        reads `git diff --name-only` and `--numstat` for base..head
  risk_tier.py --paths a.rs b.md    classifies the given paths
  risk_tier.py --self-test

Exit codes: 0 classified, 2 could not run. Standard library only.
"""
import fnmatch
import subprocess
import sys

# (glob, reason). One match makes the change deep. A test file never does: it changes no
# behaviour a user can reach.
DEEP = [
    ("crates/fleetwire/src/client.rs", "paid: the send loop, recovery and the guard decide what is billed"),
    ("crates/fleetwire/src/pricing.rs", "paid: what each request costs"),
    ("crates/fleetwire/src/error.rs", "paid: the billing and retry judgments"),
    ("crates/fleetwire/src/api/*", "paid: endpoint definitions carry pricing and scopes"),
    ("crates/fleetwire/src/transport.rs", "transport: the one place a request is sent"),
    ("crates/fleetwire/src/net.rs", "transport: agent configuration (redirects, proxy), body cap, error stage"),
    ("crates/fleetwire/src/auth/*", "auth: token storage, rotation and refresh"),
    ("crates/fleetwire/src/scope.rs", "auth: scope requirements"),
    ("crates/fleetwire/src/redaction.rs", "privacy: the single redaction layer"),
    ("crates/fleetwire/src/model.rs", "privacy: types that hold a VIN or coordinates"),
    ("crates/fleetwire/src/endpoint.rs", "read-only: the sealed endpoint trait"),
    ("crates/fleetwire/src/sealed.rs", "read-only: the sealed endpoint trait"),
    ("crates/fleetwire/clippy.toml", "read-only: the deny-list of write methods"),
    ("clippy.toml", "read-only: the deny-list of write methods"),
    ("scripts/check_invariants.py", "gate: the invariant gates"),
    (".github/workflows/*", "gate: what CI enforces"),
    (".github/rulesets/*", "gate: branch and tag protection"),
    (".claude/settings.json", "gate: agent hooks"),
    (".claude/hooks/*", "gate: agent hooks"),
    ("Cargo.lock", "dependency: what ships to consumers"),
    ("*Cargo.toml", "dependency: what ships to consumers"),
    ("deny.toml", "dependency: advisory, licence and source policy"),
]
TESTS = ["*/tests.rs", "*/tests/*", "*/test_support.rs"]
# A change made only of these is quick.
QUICK = ["*.md", "docs/*", ".claude/agents/*", ".github/ISSUE_TEMPLATE/*", ".gitmessage", "LICENSE-*"] + TESTS

LARGE_LINES = 2000
REVIEWERS = {"quick": 0, "standard": 1, "deep": 2}
ORDER = ["quick", "standard", "deep"]


def _match(path, patterns):
    return any(fnmatch.fnmatch(path, p) for p in patterns)


def classify(paths, lines=0):
    """(tier, reviewers, security pass needed, reasons) for a list of changed paths."""
    tier, reasons = "quick", []
    for p in paths:
        hits = [] if _match(p, TESTS) else [why for pat, why in DEEP if fnmatch.fnmatch(p, pat)]
        if hits:
            tier = "deep"
            reasons += [f"{p}: {why}" for why in hits]
        elif not _match(p, QUICK) and tier == "quick":
            tier = "standard"
    if lines > LARGE_LINES and tier == "quick":
        tier = "standard"
        reasons.append(f"{lines} changed lines is over {LARGE_LINES}")
    reviewers = max(REVIEWERS[tier], 3) if lines > LARGE_LINES else REVIEWERS[tier]
    return tier, reviewers, tier == "deep", list(dict.fromkeys(reasons))


def report(result):
    tier, reviewers, security, reasons = result
    head = f"tier={tier} reviewers={reviewers} security={'yes' if security else 'no'}"
    return "\n".join([head] + [f"  {r}" for r in reasons])


def self_test():
    cases = [
        (["README.md", "docs/architecture.md"], 0, "quick"),
        (["crates/fleetwire/src/client/tests.rs"], 0, "quick"),
        (["crates/fleetwire/src/auth/tests.rs", "crates/fleetwire/src/auth/lock/tests.rs"], 0, "quick"),
        (["crates/fleetwire/examples/offline_demo.rs"], 0, "standard"),
        (["crates/fleetwire/src/decode.rs"], 0, "standard"),
        (["crates/fleetwire-testkit/src/server.rs"], 0, "standard"),
        (["crates/fleetwire/src/client.rs"], 0, "deep"),
        (["crates/fleetwire/src/auth/store.rs"], 0, "deep"),
        (["crates/fleetwire/src/redaction.rs", "README.md"], 0, "deep"),
        (["crates/fleetwire/Cargo.toml"], 0, "deep"),
        ([".github/workflows/ci.yml"], 0, "deep"),
        (["scripts/check_invariants.py"], 0, "deep"),
        (["README.md"], LARGE_LINES + 1, "standard"),
    ]
    fails = [f"{paths} ({lines} lines): expected {want}, got {classify(paths, lines)[0]}" for paths, lines, want in cases if classify(paths, lines)[0] != want]
    # A test file under a deep path must not raise the tier, and a deep path must outrank it.
    if classify(["crates/fleetwire/src/auth/tests.rs", "crates/fleetwire/src/auth/mod.rs"])[0] != "deep":
        fails.append("a product file next to a test file must still be deep")
    if classify(["crates/fleetwire/src/client.rs"], LARGE_LINES + 1)[1] != 3:
        fails.append("a large change must get three reviewers")
    if fails:
        sys.stderr.write("risk_tier self-test FAILED:\n  " + "\n  ".join(fails) + "\n")
        return 1
    print(f"risk_tier self-test: ok ({len(cases) + 2} cases)")
    return 0


def _git(args):
    got = subprocess.run(["git", *args], capture_output=True, text=True, check=False)
    if got.returncode != 0:
        first = got.stderr.strip().splitlines()[0] if got.stderr.strip() else "failed"
        raise RuntimeError(f"git {' '.join(args)}: {first}")
    return got.stdout


def main(argv):
    if argv[1:] == ["--self-test"]:
        return self_test()
    if len(argv) >= 2 and argv[1] == "--paths":
        print(report(classify(argv[2:])))
        return 0
    if len(argv) < 2:
        sys.stderr.write("usage: risk_tier.py <base> [head] | --paths <path>... | --self-test\n")
        return 2
    rng = f"{argv[1]}..{argv[2] if len(argv) > 2 else 'HEAD'}"
    try:
        names = _git(["diff", "--name-only", rng])
        stats = _git(["diff", "--numstat", rng])
    except (RuntimeError, OSError) as e:
        sys.stderr.write(f"risk_tier: {e}\n")
        return 2
    paths = [p for p in names.splitlines() if p]
    lines = 0
    for row in stats.splitlines():
        added, deleted = (row.split("\t") + ["", ""])[:2]
        lines += (int(added) if added.isdigit() else 0) + (int(deleted) if deleted.isdigit() else 0)
    print(report(classify(paths, lines)))
    print(f"  ({len(paths)} files, {lines} changed lines)")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
