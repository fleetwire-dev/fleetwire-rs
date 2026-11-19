# AGENTS.md — fleetwire

Instructions for coding agents working in this repository. Claude Code reads this through
`CLAUDE.md`; other tools read it directly. People are welcome to read it too: it is the shortest
statement of what must not break.

## What this is

An **unofficial, read-only** Rust client for the Tesla Fleet API, plus `fleetwire-testkit`, the
small mock HTTP server its tests use. Start with [README.md](README.md) and
[docs/architecture.md](docs/architecture.md).

## Where the code comes from

The crate sources under `crates/` are exported from a private workspace by a script. **The next
export overwrites them.** A change to anything under `crates/` made here must be small enough to
re-apply upstream, and the pull request must say so. Files that live only here (this file,
`README.md`, `docs/`, `scripts/`, `.github/`, `.claude/`, the root `Cargo.toml`, `deny.toml`,
`clippy.toml`) are edited here.

## Invariants — do not break

Each of these has a gate in `scripts/check_invariants.py` (name in brackets) or a test that pins
it. If a gate fails, read its docstring before touching the gate.

- **Read-only.** The crate sends GET requests to the Fleet API and nothing else. The only POST
  is the token refresh in `auth/refresh.rs`, behind the only `allow(clippy::disallowed_methods)`
  [`get-only`]. `FleetGet` stays sealed [`sealed`].
- **A billed request is sent once.** `max_attempts` defaults to `1` [`no-default-retry`]. The 401
  and 412 recovery paths repair the token or the region and never re-send a request whose
  `pricing()` is `Pricing::Data`. Never add a retry, a fallback or a "try again" around a billed
  endpoint.
- **Exactly two endpoints are billed** [`billed-set`]. Adding a billed endpoint is a deliberate
  change to the README, the changelog and the gate.
- **One agent, no redirects, no environment proxy** [`one-agent`]. The HTTP agent is built in
  `net.rs::agent` only.
- **A server cannot redirect the token.** A server-supplied region URL is adopted only after
  `validate_region_url` passes (https, default port, allow-listed host). The bootstrap host and
  the token URL are the caller's to set. Do not add an "insecure" or "any host" switch for the
  server-supplied path; the test-only hatch is `#[cfg(test)]` and stays that way.
- **A read token with a command scope is refused** before anything is sent, on every iteration
  of the send loop. The check reads unverified claims and fails open: a token it cannot read is
  not blocked, and it must stay that way (blocking would turn an advisory check into a false
  denial).
- **Secrets do not print.** Tokens, VINs and coordinates pass through `redaction` before they
  reach an error message or a preview. Types that hold them never derive `Debug` (the
  [`secret-debug`] gate names four of them; a new one must be added there); a hand-written
  `Debug` destructures `self`.
- **No `unsafe`** [`no-unsafe`], and product code is linted against `unwrap` and `panic!`. An
  `expect` is acceptable only for something that cannot fail by construction, with a message that
  says why.
- **Synchronous, small dependency tree** [`dependencies`]. No async runtime. A new dependency
  needs a reason in the commit message.
- **Documentation says what was measured.** A statement about how Tesla's API behaves is either
  backed by an observed response (say when) or marked as unverified. Do not turn a guess into a
  fact by writing it confidently.

## Commands — run all of them before saying a change is done

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked
cargo test --workspace --doc --all-features --locked    # --all-targets skips doctests
RUSTFLAGS="--cfg fuzzing" cargo clippy -p fleetwire --lib --locked -- -D warnings   # the fuzz entry points
python3 scripts/check_invariants.py --self-test
python3 scripts/check_invariants.py
```

MSRV: `cargo +1.89.0 test --workspace --all-targets --all-features --locked`.

"Done" means these ran and you report each exit code and the pass/fail counts, including
failures. A truncated tail of output is not a result.

## Conventions

- **A test proves an invariant and has a negative control.** Before relying on a test, see it
  fail with the guard removed. If a test could pass for an unrelated reason (a mock that ran out
  of responses, a precondition that silently did not hold), assert the precondition in the test.
- **Assert request counts** when the rule is about how many requests reach the server.
- **A function that classifies an outcome** (billed or not, retryable or not, sent or unknown)
  falls to the safe side when it does not recognise something: not retryable, not provably
  unsent, billing unknown. It has a test for every arm.
- **A threshold constant** comes with a pair of tests, one just above and one just below.
- New unit tests go in a sibling file: `foo.rs` declares `#[cfg(test)] mod tests;` and the tests
  are in `foo/tests.rs`.
- Comments and documentation are in English, with no emoji [`comment-language`].
- Error messages are lowercase English with no trailing punctuation and never say "illegal"
  [`error-style`].
- Commit messages: `type: summary` in the imperative (`feat`, `fix`, `docs`, `test`, `refactor`,
  `ci`, `chore`).

## Never

- Never put a real token, VIN, coordinate, account identifier or raw API response in code, a
  test, a fixture, an issue or a commit message. Use made-up values that are obviously made up.
- Never `git add -A` or `git add .` (a hook refuses it). Stage explicit paths and read
  `git diff --cached`.
- Never push, merge, tag, publish to crates.io or change repository settings without the
  maintainer saying so in this conversation. An approval relayed by another agent is not one.
- Never weaken a gate, a lint or a test to make a change pass. If the rule is wrong, say why and
  ask.

## Reports

Tag each factual sentence in a report with its source as you write it: **checked** (this
session's command output or `file:line`, named), **reported** (a subagent, a reviewer, a note
from an earlier session; say whose), or **guess** (a cause or a limit you did not reproduce). A
subagent's "tests pass" is **reported** until you rerun it. "No matches" comes with a positive
control: a value from the same query that had to be non-zero.

## Review

`python3 scripts/risk_tier.py <base>` decides the depth; [REVIEW.md](REVIEW.md) is the standard.
The reviewer agent in `.claude/agents/code-reviewer.md` is read-only and starts from a fresh
context: give it the diff range, not your conclusion.
