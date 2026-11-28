#!/usr/bin/env python3
"""Invariant gates for the fleetwire crate.

Each gate states something that must stay true about the source tree and fails with the reason
when it is not. Gates are written as absence claims ("nothing outside X does Y") wherever
possible, because a gate that counts the things it knows about is blind to the rest.

Every gate has a self-test: a copy of the tree with the violation planted must fail the gate,
and the real tree must pass it. A gate without that control proves nothing (see
docs/how-this-is-built.md).

Most gates read source text, so a refactor can break them. That is intended: a person then
decides whether the code or the rule should change. They are friction against mistakes, not a
defence against someone determined to get around them.

Usage:
  check_invariants.py                 run every gate on the repository this script lives in
  check_invariants.py --self-test     run the planted-violation controls
  check_invariants.py --list          print the gates
  check_invariants.py --no-cargo      skip the one gate that needs `cargo tree`
  check_invariants.py --root DIR      check another checkout

Exit codes: 0 all gates pass, 1 a gate found a violation, 2 a gate could not run (missing file,
cargo failure). 2 is kept apart from 1 so that a broken tool is never read as a verdict.

Standard library only.
"""
import argparse
import json
import pathlib
import re
import subprocess
import sys

CRATE = "crates/fleetwire/src/"
# Transitive normal dependencies of `fleetwire` over all targets, itself included, as counted by
# `cargo tree -p fleetwire -e normal --prefix none --format {p} --locked --target all | sort -u`.
# Lowering this is always fine. Raising it needs a reason in the commit message.
DEP_BUDGET = 78
FORBIDDEN_DEPS = ("tokio", "async-std", "smol", "reqwest", "hyper", "openssl-sys", "native-tls")
# The types known to hold a token, a VIN or coordinates. None of them may derive Debug.
# The list is by name: a new type that holds a secret has to be added here by hand.
SECRET_TYPES = [
    ("crates/fleetwire/src/auth/mod.rs", "Refreshed"),
    ("crates/fleetwire/src/auth/store.rs", "TokenSet"),
    ("crates/fleetwire/src/model.rs", "VehicleSummary"),
    ("crates/fleetwire/src/model.rs", "VehicleData"),
]

HANGUL = re.compile("[ᄀ-ᇿ㄰-㆏가-힣]")
EMOJI = re.compile("[\U0001f000-\U0001faff☀-➿⬀-⯿️‼⁉]")
IDENT = re.compile(r"[A-Za-z0-9_]")


class CannotRun(Exception):
    """A gate could not reach a verdict (exit code 2)."""


# ── Rust source helpers ──────────────────────────────────────────────────────────────────────


def segments(src):
    """Splits Rust source into (kind, text) with kind in code | comment | str | char."""
    out, i, n, start = [], 0, len(src), 0

    def flush(upto):
        if upto > start:
            out.append(("code", src[start:upto]))

    while i < n:
        c, two = src[i], src[i : i + 2]
        if two == "//":
            flush(i)
            j = src.find("\n", i)
            j = n if j == -1 else j
            out.append(("comment", src[i:j]))
            i = start = j
            continue
        if two == "/*":
            flush(i)
            depth, j = 1, i + 2
            while j < n and depth:
                if src[j : j + 2] == "/*":
                    depth, j = depth + 1, j + 2
                elif src[j : j + 2] == "*/":
                    depth, j = depth - 1, j + 2
                else:
                    j += 1
            out.append(("comment", src[i:j]))
            i = start = j
            continue
        if c in "rbc" and (i == 0 or not IDENT.match(src[i - 1])):
            m = re.match(r'(?:b|c)?r(#*)"', src[i:])
            if m:
                flush(i)
                close = '"' + m.group(1)
                j = src.find(close, i + m.end())
                j = n if j == -1 else j + len(close)
                out.append(("str", src[i:j]))
                i = start = j
                continue
        if c == '"':
            flush(i)
            j = i + 1
            while j < n and src[j] != '"':
                j += 2 if src[j] == "\\" else 1
            j = min(j + 1, n)
            out.append(("str", src[i:j]))
            i = start = j
            continue
        if c == "'":
            # A char literal closes within a few characters; a lifetime does not.
            if src[i + 1 : i + 2] == "\\":
                flush(i)
                j = i + 2
                while j < n and src[j] != "'":
                    j += 2 if src[j] == "\\" else 1
                j = min(j + 1, n)
                out.append(("char", src[i:j]))
                i = start = j
                continue
            if src[i + 2 : i + 3] == "'":
                flush(i)
                out.append(("char", src[i : i + 3]))
                i = start = i + 3
                continue
        i += 1
    flush(n)
    return out


def code_only(src):
    """The source with comments removed and every string or char literal blanked."""
    return "".join(t if k == "code" else ('""' if k == "str" else ("''" if k == "char" else "")) for k, t in segments(src))


def product(src):
    """The source without its test-only items.

    An item is test-only when a column-0 `#[cfg(test)]` precedes it. A one-line item (`mod tests;`,
    `use ...;`) is dropped alone; a block is dropped up to its closing brace at column 0.
    Indented `#[cfg(test)]` items inside an impl stay; no gate here depends on them.
    """
    lines, out, i = src.split("\n"), [], 0
    while i < len(lines):
        if lines[i].startswith("#[cfg(test)]"):
            i += 1
            while i < len(lines) and lines[i].startswith("#["):
                i += 1
            if i < len(lines) and not lines[i].rstrip().endswith(";"):
                while i < len(lines) and lines[i] != "}":
                    i += 1
            i += 1
            continue
        out.append(lines[i])
        i += 1
    return "\n".join(out)


def is_test_file(path):
    return path.endswith("/tests.rs") or "/tests/" in path or path.endswith("test_support.rs")


class Tree:
    """The files the gates read, as {repository-relative path: text}."""

    def __init__(self, files):
        self.files = files

    @classmethod
    def load(cls, root):
        root = pathlib.Path(root)
        files = {}
        patterns = ("crates/**/*.rs", "crates/**/Cargo.toml", "crates/**/clippy.toml", "Cargo.toml")
        patterns += (".claude/settings.json", ".claude/agents/*.md", ".claude/hooks/*")
        patterns += (".github/ISSUE_TEMPLATE/*.yml", ".github/ISSUE_TEMPLATE/*.yaml", "SECURITY.md")
        for pattern in patterns:
            for p in root.glob(pattern):
                if "target" not in p.parts and p.is_file():
                    files[p.relative_to(root).as_posix()] = p.read_text(encoding="utf-8")
        if not any(k.startswith(CRATE) for k in files):
            raise CannotRun(f"no Rust sources under {root / CRATE}")
        return cls(files)

    def text(self, path):
        if path not in self.files:
            raise CannotRun(f"expected file is missing: {path}")
        return self.files[path]

    def crate_product(self):
        """(path, product code with comments and literals removed) for the crate's non-test files."""
        for path in sorted(self.files):
            if path.startswith(CRATE) and path.endswith(".rs") and not is_test_file(path):
                yield path, code_only(product(self.files[path]))

    def edited(self, path, old, new):
        """A copy with one replacement, which must apply exactly once (self-tests)."""
        text = self.text(path)
        if text.count(old) != 1:
            raise CannotRun(f"self-test edit of {path} matched {text.count(old)} times: {old!r}")
        files = dict(self.files)
        files[path] = text.replace(old, new)
        return Tree(files)


# ── Gates ────────────────────────────────────────────────────────────────────────────────────

AGENT_BUILDERS = re.compile(r"\b(config_builder|new_with_config|new_with_defaults)\s*\(|\bureq::agent\s*\(")
AGENT_SETTINGS = (".max_redirects(0)", ".proxy(None)", ".http_status_as_error(false)")


def gate_one_agent(tree):
    """The HTTP agent is built in one function, with redirects and environment proxies off.

    Why: a redirect or a proxy is a way for the bearer token to reach a host nobody chose.
    """
    bad = []
    for path, code in tree.crate_product():
        if path != CRATE + "net.rs" and AGENT_BUILDERS.search(code):
            bad.append(f"{path}: builds a ureq agent; only net.rs::agent may")
    net = "".join(product(tree.text(CRATE + "net.rs")).split())
    for setting in AGENT_SETTINGS:
        if setting not in net:
            bad.append(f"{CRATE}net.rs: the agent configuration no longer has `{setting}`")
    return bad


WRITE_CALL = re.compile(r"\.\s*(post|put|patch|delete)\s*\(|\bMethod::(POST|PUT|PATCH|DELETE)\b")
LINT_ESCAPE = re.compile(r"#!?\[(allow|expect)\([^\]]*disallowed_methods")


def gate_get_only(tree):
    """No write request anywhere but the single token-refresh POST.

    Why: the crate is read-only. The clippy deny-list catches the call; this catches the escape
    hatch (`allow`) appearing a second time, a write method named some other way, and a second
    write call slipping into the one file that is allowed to make one.
    """
    bad, escapes = [], []
    refresh = CRATE + "auth/refresh.rs"
    for path, code in tree.crate_product():
        escapes += [path] * len(LINT_ESCAPE.findall(code))
        writes = len(WRITE_CALL.findall(code))
        if path != refresh and writes:
            bad.append(f"{path}: an HTTP write method outside the token refresh")
        if path == refresh and writes != 1:
            bad.append(f"{path}: {writes} HTTP write calls; the token refresh makes exactly one")
    if escapes != [refresh]:
        bad.append(f"disallowed_methods is lifted {len(escapes)} time(s) in {sorted(set(escapes))}; exactly one, in {refresh}, is allowed")
    return bad


def gate_billed_set(tree):
    """Exactly two request types are billed, and both are in api/vehicles.rs.

    Why: adding a billed endpoint changes what a caller can be charged for. It must be a
    deliberate change that also updates the README, the changelog and this gate.
    How: it counts the places under api/ that construct `Pricing::data()`. It does not prove the
    two are the right two; the tests that name each endpoint's pricing do that.
    """
    found = []
    for path, code in tree.crate_product():
        if path.startswith(CRATE + "api/"):
            found += [path] * len(re.findall(r"\bPricing::data\s*\(", code))
    if found != [CRATE + "api/vehicles.rs"] * 2:
        return [f"billed endpoints: expected 2 in api/vehicles.rs, found {len(found)} in {sorted(set(found))}"]
    return []


def gate_no_default_retry(tree):
    """Retries stay off by default.

    Why: a retry of a billed request is a second charge. Opting in is the caller's decision.
    """
    code = code_only(product(tree.text(CRATE + "client.rs")))
    if not re.search(r"\bconst\s+DEFAULT_MAX_ATTEMPTS\s*:\s*u32\s*=\s*1\s*;", code):
        return [f"{CRATE}client.rs: DEFAULT_MAX_ATTEMPTS is not `1`"]
    return []


def gate_secret_debug(tree):
    """The named types that hold a token, a VIN or coordinates do not derive Debug.

    Why: a derived Debug prints every field. One `{:?}` in a log line would disclose a live
    refresh token. This was found in review once; the gate keeps it found.
    Limit: it checks the types listed in SECRET_TYPES, not every type that could hold a secret.
    """
    bad = []
    for path, name in SECRET_TYPES:
        code = code_only(product(tree.text(path)))
        m = re.search(r"((?:#\[[^\]]*\]\s*)*)pub(?:\([a-z]+\))?\s+struct\s+" + name + r"\b", code)
        if not m:
            raise CannotRun(f"{path}: struct {name} not found; update SECRET_TYPES if it moved")
        for derive in re.findall(r"#\[derive\(([^)]*)\)\]", m.group(1)):
            if re.search(r"\bDebug\b", derive):
                bad.append(f"{path}: {name} derives Debug")
    return bad


def gate_sealed(tree):
    """The endpoint trait stays sealed.

    Why: an endpoint defined outside the crate could carry any path and any pricing, and the
    read-only and billing guarantees would no longer cover what the client sends.
    """
    bad = []
    lib = code_only(product(tree.text(CRATE + "lib.rs")))
    if re.search(r"\bpub(\([a-z]+\))?\s+mod\s+sealed\b", lib):
        bad.append(f"{CRATE}lib.rs: `mod sealed` is public")
    if not re.search(r"(?m)^mod\s+sealed\s*;", lib):
        bad.append(f"{CRATE}lib.rs: `mod sealed;` is missing")
    endpoint = code_only(product(tree.text(CRATE + "endpoint.rs")))
    if not re.search(r"pub\s+trait\s+FleetGet\s*:\s*crate::sealed::Sealed\b", endpoint):
        bad.append(f"{CRATE}endpoint.rs: FleetGet no longer has the Sealed supertrait")
    if re.search(r"\bpub\s+use\s+[^;]*\bsealed\b", lib):
        bad.append(f"{CRATE}lib.rs: the sealed module is re-exported")
    return bad


def gate_comment_language(tree):
    """Comments are in English and carry no emoji.

    Why: the source is read by people outside the team that wrote it. Text inside a string
    literal is not a comment and is left alone (tests feed non-ASCII input on purpose).
    A line may opt out with `noqa: comment-lang` when it must quote other text verbatim.
    """
    bad = []
    for path in sorted(tree.files):
        if not path.endswith(".rs"):
            continue
        line = 1
        for kind, text in segments(tree.files[path]):
            if kind == "comment":
                for off, part in enumerate(text.split("\n")):
                    if "noqa: comment-lang" in part:
                        continue
                    if HANGUL.search(part):
                        bad.append(f"{path}:{line + off}: comment is not in English")
                    elif EMOJI.search(part):
                        bad.append(f"{path}:{line + off}: comment carries an emoji or dingbat")
            line += text.count("\n")
    return bad


ERROR_ATTR = re.compile(r'#\[error\(\s*"((?:[^"\\]|\\.)*)"')
ACRONYM = re.compile(r"^[A-Z][A-Z0-9_]+\b")


def error_message_faults(msg):
    faults = []
    text = msg.strip()
    if any(ord(ch) > 127 for ch in text):
        faults.append("is not ASCII")
    if text[:1].isalpha() and text[:1].isupper() and not ACRONYM.match(text):
        faults.append("starts with a capital letter")
    if text.endswith((".", "!", "?")):
        faults.append("ends with punctuation")
    if re.search(r"\billegal\b", text, re.IGNORECASE):
        faults.append("says \"illegal\"; say \"invalid\" or something more specific")
    return faults


def gate_error_style(tree):
    """Error messages are lowercase, unpunctuated English.

    Why: they are concatenated into longer messages and searched in logs (Rust API Guidelines
    C-GOOD-ERR). One style means one place to look.
    """
    bad, seen = [], 0
    for path in sorted(tree.files):
        if not (path.startswith(CRATE) and path.endswith(".rs")) or is_test_file(path):
            continue
        for m in ERROR_ATTR.finditer(product(tree.files[path])):
            seen += 1
            for fault in error_message_faults(m.group(1)):
                bad.append(f"{path}: error message {m.group(1)!r} {fault}")
    if seen == 0:
        raise CannotRun("no #[error(\"...\")] attribute found; the scan is looking in the wrong place")
    return bad


def gate_no_unsafe(tree):
    """`unsafe` stays forbidden workspace-wide, with no local escape.

    Why: `forbid` cannot be overridden by an inner `allow`, but it can be deleted.
    """
    bad = []
    root = tree.text("Cargo.toml")
    if not re.search(r'(?m)^unsafe_code\s*=\s*"forbid"', root):
        bad.append('Cargo.toml: [workspace.lints.rust] no longer has unsafe_code = "forbid"')
    for path in sorted(tree.files):
        if path.endswith(".rs") and re.search(r"(allow|expect)\(\s*unsafe_code", code_only(tree.files[path])):
            bad.append(f"{path}: lifts the unsafe_code lint")
        if path.endswith("Cargo.toml") and path != "Cargo.toml":
            # A member without a [lints] table silently opts out of every workspace lint.
            if not re.search(r"(?m)^\[lints\]\s*\nworkspace\s*=\s*true", tree.files[path]):
                bad.append(f"{path}: does not inherit the workspace lints ([lints] workspace = true)")
    return bad


WRITE_TOOLS = {"Edit", "Write", "MultiEdit", "NotebookEdit"}


def gate_harness(tree):
    """The agent harness is intact: configured hooks exist, and no reviewer is given an edit tool.

    Why: the hooks and the reviewer are part of how changes are checked. A hook whose file is
    gone fails silently, and a reviewer that fixes what it finds is no longer an independent check.
    Limit: it reads the `tools:` line of each agent definition. A reviewer keeps its shell (it
    runs tests), so this stops an accident, not a determined agent.
    """
    bad = []
    try:
        settings = json.loads(tree.text(".claude/settings.json"))
    except ValueError as e:
        raise CannotRun(f".claude/settings.json is not valid JSON: {e}") from e
    commands = [h.get("command", "") for groups in settings.get("hooks", {}).values() for g in groups for h in g.get("hooks", [])]
    named = sorted({m for c in commands for m in re.findall(r"\.claude/hooks/[A-Za-z0-9_.-]+", c)})
    if not named:
        bad.append(".claude/settings.json: no hook is configured")
    bad += [f".claude/settings.json: hook file {n} does not exist" for n in named if n not in tree.files]
    orphans = sorted(k for k in tree.files if k.startswith(".claude/hooks/") and k not in named)
    bad += [f"{k}: hook file is not named in .claude/settings.json" for k in orphans]
    agents = sorted(k for k in tree.files if k.startswith(".claude/agents/"))
    if not agents:
        bad.append(".claude/agents/: the reviewer agent definition is missing")
    for path in agents:
        m = re.search(r"(?m)^tools:\s*(.+)$", tree.files[path])
        if not m:
            bad.append(f"{path}: no `tools:` line, so the agent inherits every tool")
            continue
        tools = {t.strip() for t in m.group(1).split(",")}
        if tools & WRITE_TOOLS:
            bad.append(f"{path}: reviewer has a write tool ({', '.join(sorted(tools & WRITE_TOOLS))})")
    return bad


FORMS = ".github/ISSUE_TEMPLATE/"
ADVISORIES = "https://github.com/fleetwire-dev/fleetwire-rs/security/advisories/new"
REDACT_WORDS = ("do not paste", "token", "vin", "coordinate")
FORM_TEXT_LINE = re.compile(r"^\s*(?:-\s+)?(name|description|label|labels|title)\s*:\s*(.*)$")


def gate_issue_forms(tree):
    """Every issue form opens with the redaction warning, and the forms and the security policy agree.

    Why: an issue is public the moment it is filed. The first thing a reporter reads must tell them
    not to paste a token, a VIN, a coordinate or a raw response, and a security report must have a
    private route that works. Form names and labels are English without emoji, like the docs.
    Limit: it reads the YAML as text (standard library only). It checks that the first body item
    is a markdown block carrying the warning, not how GitHub renders the form.
    """
    bad = []
    forms = sorted(k for k in tree.files if k.startswith(FORMS) and k.rsplit("/", 1)[-1] not in ("config.yml", "config.yaml"))
    if not forms:
        bad.append(f"{FORMS}: no issue form found")
    for path in forms:
        text = tree.files[path]
        body = re.split(r"(?m)^body\s*:\s*$", text, maxsplit=1)
        items = re.split(r"(?m)^\s*-\s+type\s*:\s*", body[1]) if len(body) == 2 else []
        first = items[1] if len(items) > 1 else ""
        if not first.startswith("markdown"):
            bad.append(f"{path}: the first body item is not the markdown warning")
        elif not all(w in first.lower() for w in REDACT_WORDS):
            bad.append(f"{path}: the opening markdown does not warn against pasting tokens, VINs and coordinates")
        if not re.search(r"(?m)^labels\s*:", text):
            bad.append(f"{path}: no `labels:` line")
        for n, line in enumerate(text.split("\n"), 1):
            m = FORM_TEXT_LINE.match(line)
            if m and (HANGUL.search(m.group(2)) or EMOJI.search(m.group(2))):
                bad.append(f"{path}:{n}: `{m.group(1)}` is not plain English (Hangul or emoji)")
    config = tree.files.get(FORMS + "config.yml")
    if config is None:
        bad.append(f"{FORMS}config.yml: missing, so blank issues are allowed")
    else:
        if not re.search(r"(?m)^blank_issues_enabled\s*:\s*false\s*$", config):
            bad.append(f"{FORMS}config.yml: blank issues are not turned off")
        if ADVISORIES not in config:
            bad.append(f"{FORMS}config.yml: no private vulnerability reporting link")
    policy = tree.files.get("SECURITY.md", "")
    if ADVISORIES not in policy:
        bad.append("SECURITY.md: no private vulnerability reporting link")
    return bad


def dependency_faults(packages):
    """Faults in a list of `name vX.Y.Z` lines from cargo tree."""
    names = sorted({line.split()[0] for line in packages if line.strip()})
    bad = [f"dependency tree holds `{n}`" for n in names if n in FORBIDDEN_DEPS]
    if len(names) > DEP_BUDGET:
        bad.append(f"{len(names)} transitive dependencies, over the budget of {DEP_BUDGET}")
    return bad


def gate_dependencies(root):
    """No async runtime, no second HTTP or TLS stack, and the tree stays within its budget.

    Why: every dependency is code that runs with the token in memory. The client is synchronous
    on purpose.
    """
    cmd = ["cargo", "tree", "-p", "fleetwire", "-e", "normal", "--prefix", "none", "--format", "{p}", "--locked", "--target", "all"]
    try:
        got = subprocess.run(cmd, cwd=root, capture_output=True, text=True, check=False)
    except OSError as e:
        raise CannotRun(f"cargo could not be started: {e}") from e
    if got.returncode != 0:
        raise CannotRun(f"cargo tree failed: {got.stderr.strip()[:300]}")
    lines = [line.strip() for line in got.stdout.splitlines() if line.strip()]
    if not any(line.startswith("fleetwire ") for line in lines):
        raise CannotRun("cargo tree output does not list fleetwire itself")
    return dependency_faults(lines)


TREE_GATES = [
    ("one-agent", gate_one_agent),
    ("get-only", gate_get_only),
    ("billed-set", gate_billed_set),
    ("no-default-retry", gate_no_default_retry),
    ("secret-debug", gate_secret_debug),
    ("sealed", gate_sealed),
    ("comment-language", gate_comment_language),
    ("error-style", gate_error_style),
    ("no-unsafe", gate_no_unsafe),
    ("harness", gate_harness),
    ("issue-forms", gate_issue_forms),
]


# ── Self-tests ───────────────────────────────────────────────────────────────────────────────

# (gate, file, text to find once, replacement, must the gate fail?)
# `True` rows plant a violation. `False` rows are specificity controls: an edit that looks like
# a violation and is not one, so that a gate which fails on everything is caught too.
PLANTED = [
    ("one-agent", "net.rs", ".max_redirects(0)", ".max_redirects(3)", True),
    ("one-agent", "net.rs", ".proxy(None)", "", True),
    ("one-agent", "client.rs", "impl ClientBuilder {", "fn rogue() { let _ = ureq::Agent::config_builder(); }\nimpl ClientBuilder {", True),
    ("one-agent", "client.rs", "impl ClientBuilder {", "// ureq::Agent::config_builder() is only for net.rs\nimpl ClientBuilder {", False),
    ("get-only", "client.rs", "impl ClientBuilder {", "fn rogue(a: &ureq::Agent) { let _ = a.post(\"https://x\"); }\nimpl ClientBuilder {", True),
    ("get-only", "transport.rs", "use std::time::{Duration, Instant};", "use std::time::{Duration, Instant};\nconst M: http::Method = http::Method::DELETE;", True),
    ("get-only", "client.rs", "impl ClientBuilder {", "#[allow(clippy::disallowed_methods)]\nimpl ClientBuilder {", True),
    ("get-only", "auth/refresh.rs", "#[allow(clippy::disallowed_methods)]", "", True),
    ("get-only", "client.rs", "impl ClientBuilder {", "const NOTE: &str = \"never call .post( here\";\nimpl ClientBuilder {", False),
    ("billed-set", "api/users.rs", "pub struct GetRegion;", "fn rogue() -> Pricing { Pricing::data() }\npub struct GetRegion;", True),
    ("billed-set", "api/users.rs", "pub struct GetRegion;", "// Pricing::data() would make this billed\npub struct GetRegion;", False),
    ("no-default-retry", "client.rs", "const DEFAULT_MAX_ATTEMPTS: u32 = 1;", "const DEFAULT_MAX_ATTEMPTS: u32 = 3;", True),
    ("secret-debug", "auth/mod.rs", "#[derive(Clone)]\n#[non_exhaustive]\npub struct Refreshed {", "#[derive(Clone, Debug)]\n#[non_exhaustive]\npub struct Refreshed {", True),
    ("secret-debug", "model.rs", "#[derive(Deserialize)]\n#[non_exhaustive]\npub struct VehicleSummary {", "#[derive(Debug, Deserialize)]\n#[non_exhaustive]\npub struct VehicleSummary {", True),
    ("sealed", "lib.rs", "\nmod sealed;", "\npub mod sealed;", True),
    ("sealed", "endpoint.rs", "pub trait FleetGet: crate::sealed::Sealed + Send + Sync {", "pub trait FleetGet: Send + Sync {", True),
    ("comment-language", "pricing.rs", "impl Pricing {", "// 요금표\nimpl Pricing {", True),
    ("comment-language", "pricing.rs", "impl Pricing {", "// careful ⚠️\nimpl Pricing {", True),
    ("comment-language", "pricing.rs", "impl Pricing {", "const SAMPLE: &str = \"요금표\";\nimpl Pricing {", False),
    ("error-style", "error.rs", '#[error("request not sent: {message}")]', '#[error("Request not sent: {message}.")]', True),
    ("error-style", "error.rs", '#[error("HTTP {status}: {message}")]', '#[error("HTTP {status} is illegal: {message}")]', True),
    ("no-unsafe", "pricing.rs", "impl Pricing {", "#[allow(unsafe_code)]\nimpl Pricing {", True),
    ("secret-debug", "model.rs", "#[derive(Deserialize)]\n#[non_exhaustive]\npub struct VehicleData {", "#[derive(Debug, Deserialize)]\n#[non_exhaustive]\npub struct VehicleData {", True),
    ("get-only", "auth/refresh.rs", "impl HttpRefresher {", "fn rogue(a: &ureq::Agent) { let _ = a.put(\"https://x\"); }\nimpl HttpRefresher {", True),
]

DEP_CASES = [
    (["fleetwire v0.2.0", "ureq v3.3.0", "rustls v0.23.46"], False),
    (["fleetwire v0.2.0", "ureq v3.3.0", "tokio v1.54.1"], True),
    (["fleetwire v0.2.0"] + [f"crate{i} v1.0.0" for i in range(DEP_BUDGET)], True),
]


def self_test(tree):
    gates = dict(TREE_GATES)
    fails = []
    for name, gate in TREE_GATES:
        got = gate(tree)
        if got:
            fails.append(f"{name}: the unedited tree does not pass ({got[0]})")
    for name, rel, old, new, must_fail in PLANTED:
        got = gates[name](tree.edited(CRATE + rel, old, new))
        if must_fail and not got:
            fails.append(f"{name}: planted violation in {rel} was not caught: {new[:60]!r}")
        if not must_fail and got:
            fails.append(f"{name}: harmless edit in {rel} was flagged: {got[0]}")
    for packages, must_fail in DEP_CASES:
        if bool(dependency_faults(packages)) != must_fail:
            fails.append(f"dependencies: wrong verdict for a tree of {len(packages)} with {packages[-1]}")
    lifted = tree.edited("Cargo.toml", 'unsafe_code = "forbid"', 'unsafe_code = "warn"')
    if not gate_no_unsafe(lifted):
        fails.append("no-unsafe: weakening the workspace lint was not caught")
    opted_out = tree.edited("crates/fleetwire/Cargo.toml", "[lints]\nworkspace = true\n", "")
    if not gate_no_unsafe(opted_out):
        fails.append("no-unsafe: a member that dropped its [lints] table was not caught")
    reviewer = ".claude/agents/code-reviewer.md"
    if not gate_harness(tree.edited(reviewer, "tools: Read, Grep, Glob, Bash", "tools: Read, Grep, Glob, Bash, Edit")):
        fails.append("harness: a reviewer with a write tool was not caught")
    if not gate_harness(tree.edited(reviewer, "tools: Read, Grep, Glob, Bash\n", "")):
        fails.append("harness: a reviewer with no tools line was not caught")
    if not gate_harness(Tree({k: v for k, v in tree.files.items() if k != ".claude/hooks/block_bulk_add.py"})):
        fails.append("harness: a missing hook file was not caught")
    if gate_harness(tree.edited(reviewer, "You are the final reviewer", "You may suggest an Edit or a Write; you are the final reviewer")):
        fails.append("harness: prose that mentions a write tool was flagged")
    bug, cfg = FORMS + "bug_report.yml", FORMS + "config.yml"
    form_cases = [
        (bug, "**Do not paste tokens, VINs, coordinates or raw API responses.**", "Please be careful.", True),
        (bug, "name: Bug report", "name: Bug report \U0001F41B", True),
        (bug, "label: What happened, and what did you expect?", "label: 무슨 일이 있었나요?", True),
        (bug, "  - type: markdown", "  - type: input\n    id: early\n    attributes: { label: Early }\n  - type: markdown", True),
        (bug, "labels: [bug]", "", True),
        (cfg, "blank_issues_enabled: false", "blank_issues_enabled: true", True),
        (cfg, ADVISORIES, "https://example.invalid/report", True),
        ("SECURITY.md", ADVISORIES, "https://example.invalid/report", True),
        (bug, "label: What happened, and what did you expect?", "label: What happened, and what did you expect instead?", False),
        (bug, "Redact them first.", "Redact them first. Example: VIN 5YJ3EXAMPLE (made up).", False),
    ]
    for path, old, new, must_fail in form_cases:
        got = gate_issue_forms(tree.edited(path, old, new))
        if must_fail and not got:
            fails.append(f"issue-forms: planted violation in {path} was not caught: {new[:60]!r}")
        if not must_fail and got:
            fails.append(f"issue-forms: harmless edit in {path} was flagged: {got[0]}")
    if not gate_issue_forms(Tree({k: v for k, v in tree.files.items() if k != cfg})):
        fails.append("issue-forms: a missing config.yml was not caught")
    if fails:
        sys.stderr.write("check_invariants self-test FAILED:\n  " + "\n  ".join(fails) + "\n")
        return 1
    planted = sum(1 for row in PLANTED if row[4]) + 2 + 2 + 3 + sum(1 for c in form_cases if c[3]) + 1
    controls = sum(1 for row in PLANTED if not row[4]) + 1 + 1 + sum(1 for c in form_cases if not c[3])
    print(f"check_invariants self-test: ok ({planted} planted violations caught, {controls} harmless edits passed)")
    return 0


# ── Entry point ──────────────────────────────────────────────────────────────────────────────


def main(argv):
    ap = argparse.ArgumentParser(description="Invariant gates for the fleetwire crate")
    ap.add_argument("--root", default=str(pathlib.Path(__file__).resolve().parent.parent))
    ap.add_argument("--self-test", action="store_true")
    ap.add_argument("--list", action="store_true")
    ap.add_argument("--no-cargo", action="store_true")
    args = ap.parse_args(argv)

    if args.list:
        for name, gate in TREE_GATES + [("dependencies", gate_dependencies)]:
            print(f"{name:18s} {gate.__doc__.strip().splitlines()[0]}")
        return 0
    try:
        tree = Tree.load(args.root)
        if args.self_test:
            return self_test(tree)
        failed = 0
        gates = [(name, lambda g=gate: g(tree)) for name, gate in TREE_GATES]
        if not args.no_cargo:
            gates.append(("dependencies", lambda: gate_dependencies(args.root)))
        for name, run in gates:
            got = run()
            print(f"{'FAIL' if got else 'ok  '} {name}")
            for line in got:
                print(f"       {line}")
            failed += bool(got)
    except CannotRun as e:
        sys.stderr.write(f"check_invariants could not run: {e}\n")
        return 2
    if failed:
        sys.stderr.write(f"{failed} gate(s) failed. Each gate's docstring says why the rule exists (--list).\n")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
