# Changelog

All notable changes to `fleetwire` are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.1] - 2026-11-26

No change to the library's public API. One error is classified differently (see Fixed).

### Security

- The repository's lock file moves `url` to 2.5.9 for RUSTSEC-2026-0592 (stack overflow in
  `Url::origin()` on nested `blob:` URLs). This crate does not call `Url::origin()`. A project
  that depends on fleetwire resolves `url` in its own lock file.

### Release process

- Published from CI by crates.io trusted publishing: the release job exchanges its OIDC token for a
  short-lived publish token and no longer reads a stored registry token. The job that can request
  that token runs only the exchange and `cargo publish`; the tests and the semver check run in a
  job without it.
- `cargo-semver-checks` compares the public API with the newest release on crates.io, in CI and
  again in the release job before publishing.

### Added

- `examples/offline_demo`: drives the real client against a mock Fleet API on a loopback port and
  checks 23 claims against what the server received. It shows a sleeping car's billed call being
  skipped, a billed request not being sent again after a 401 or a 408, a token with a command
  scope being refused before anything is sent, a region answer that names another server not
  being adopted, and an error body being scrubbed. It needs no token and exits non-zero if a
  claim stops holding.
- `examples/guarded_vehicle_data` says what a call spent, and `examples/pricing_table` says that
  its prices are the request types' own values and not a live tariff.
- `scripts/check_invariants.py`: invariant gates (GET only, exactly two billed endpoints, no default
  retry, one HTTP agent with redirects and proxies off, no `Debug` on secret-holding types, sealed
  endpoint trait, English comments, error-message style, no `unsafe`, dependency budget), each with
  a self-test that plants the violation. CI runs them in a new `invariants` job, together with the
  two examples that need no account.
- `fuzz/`: four `cargo fuzz` targets for the code that reads untrusted bytes (redaction, response
  decoding, the unverified scope check, the region URL check and `Retry-After`), with a weekly
  smoke run in CI. Their entry points are in `fleetwire::fuzz`, which exists only under
  `--cfg fuzzing` and is not part of the public API.
- `docs/architecture.md`, `docs/safety-boundaries.md` and `docs/how-this-is-built.md`.
- `AGENTS.md`, `REVIEW.md`, `scripts/risk_tier.py` and `.claude/`: the instructions, review standard
  and hooks coding agents work under in this repository.
- `README.ko.md`: a Korean summary.

### Fixed

- A body read that timed out a moment before the request deadline was reported as a generic I/O
  error. It is now reported as a timeout. Only the message changes; the retry and billing judgments
  were already the same for both.

### Changed

- Source comments and test failure messages are now in English throughout, and comments that
  still described the earlier HTTP stack were corrected.
- `examples/list_vehicles` and `examples/guarded_vehicle_data` name both missing environment
  variables in one message. The "If it fails" table quotes the full error text.
- The crate README no longer says that all sixteen endpoints were verified against live
  responses: it says which were exercised, and that a response is strongly typed only where a
  real one was observed.
- The repository README explains what the crate refuses to do and where each refusal is tested.
- The comment on `GetRegion::required_scopes` now says which accepted scopes were measured and
  which were not. The scope expression itself is unchanged.

## [0.2.0] - 2026-11-15

No change to the library's API or behaviour since 0.1.0; this release adds examples and documentation.

### Added

- `examples/pricing_table`: prints which of the sixteen endpoints are billed, with each path and its
  required scopes, read from the request types. Needs no token and makes no request.
- `examples/guarded_vehicle_data`: reads the battery level through `Client::send_guarded`, which skips
  the billed `vehicle_data` call when the car is not online.

### Documentation

- The README lists more of what the code guarantees (redaction, VIN checks, the region host allowlist,
  no `unsafe`), shows the `pricing_table` output, and adds a "Free calls" section with an example.
- A CI badge in the repository README.

## [0.1.0] - 2026-11-12

First public version of the read-only client.

### Added

- `Client` with four entry points: `send`, `send_guarded`, `send_metered` and `send_raw`.
- Sixteen GET endpoints verified against live responses (12 vehicle, 4 user), each tagged with its
  `Pricing`; the two billed ones are never retried or re-sent by the recovery paths.
- `TokenManager`: owner-only token storage, refresh-token rotation persisted before returning, and
  refreshes collapsed within and across processes.
- `examples/list_vehicles`, and a quick start in the README.
