# Contributing to fleetwire

Thanks for looking. This is a small, solo-maintained library, so the scope is kept narrow on purpose.

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

The four commands in the README's "Development" section are what CI runs; run them before opening a
pull request. `cargo deny check` and gitleaks also run in CI.

- Changes land through pull requests; `main` is protected and all CI jobs must pass.
- Tests should prove an invariant and fail when the guard they cover is removed.
- Never put a real token, VIN, coordinate or raw API response in code, tests or an issue.

## Language

Public documentation (`///`, `//!`, READMEs) is in English. Many internal `//` comments are in Korean,
the maintainer's working language; you do not need to translate them, and new comments may be in
English.

## Commits

Use `type: summary` (`feat`, `fix`, `docs`, `test`, `refactor`, `ci`, `chore`), written in English in
the imperative. A template is in `.gitmessage`.

## Security

Report vulnerabilities privately — see [SECURITY.md](SECURITY.md).

## Licence

By contributing you agree that your contribution is dual licensed under MIT and Apache-2.0, as the
project is.
