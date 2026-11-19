# Contributing to fleetwire

Thanks for looking. This is a small, solo-maintained library, so the scope is kept narrow on purpose.

The crate sources are exported from a private workspace (see "Where this comes from" in the README).
A change you send here is re-applied there by the maintainer, so smaller pull requests land faster.
Coding agents: read [AGENTS.md](AGENTS.md) first.

## What fits, and what does not

Welcome:

- bug reports and fixes, with a test that fails without the fix;
- corrections where the crate disagrees with what the Tesla Fleet API actually returns (say how you
  observed it — redact tokens, VINs and coordinates);
- documentation and example improvements.

Not accepted:

- vehicle commands or any other write request — the crate is read-only by design;
- an async API or an async runtime dependency;
- retries on billed endpoints, or anything that can send a billed request twice;
- new dependencies without a strong reason.

If you are unsure, open an issue before writing code.

## Development

The commands in the README's "Development" section are what CI runs; run them before opening a
pull request. `cargo deny check` and gitleaks also run in CI.

`scripts/check_invariants.py` holds the invariant gates: short checks that the properties the README
promises (GET only, two billed endpoints, no default retry, one HTTP agent, no `Debug` on secrets)
still hold in the source. If one fails, `--list` prints why the rule exists. A gate is changed only
together with its self-test.

- Changes land through pull requests; `main` is protected and all CI jobs must pass.
- Tests should prove an invariant and fail when the guard they cover is removed.
- Never put a real token, VIN, coordinate or raw API response in code, tests or an issue.

## Language

Comments and documentation are in English, without emoji. The `comment-language` gate checks it.
Text inside a string literal is not a comment: a test may feed any input it needs.

## Commits

Use `type: summary` (`feat`, `fix`, `docs`, `test`, `refactor`, `ci`, `chore`), written in English in
the imperative. A template is in `.gitmessage`.

## Security

Report vulnerabilities privately — see [SECURITY.md](SECURITY.md).

## Licence

By contributing you agree that your contribution is dual licensed under MIT and Apache-2.0, as the
project is.
