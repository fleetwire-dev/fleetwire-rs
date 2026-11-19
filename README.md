# fleetwire

[![ci](https://github.com/fleetwire-dev/fleetwire-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/fleetwire-dev/fleetwire-rs/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/fleetwire.svg)](https://crates.io/crates/fleetwire)
[![docs.rs](https://img.shields.io/docsrs/fleetwire)](https://docs.rs/fleetwire)
[![MSRV](https://img.shields.io/badge/MSRV-1.89-blue)](crates/fleetwire/README.md#minimum-supported-rust-version-msrv)
[![license](https://img.shields.io/crates/l/fleetwire.svg)](#license)

An **unofficial, read-only** Rust client for the Tesla Fleet API that knows which calls cost money.

> **Not affiliated with, endorsed by, or sponsored by Tesla, Inc.**
> Tesla names and trademarks belong to their respective owners.

[한국어 요약](README.ko.md) · [Quick start](crates/fleetwire/README.md#quick-start) ·
[Architecture](docs/architecture.md) · [Safety boundaries](docs/safety-boundaries.md) ·
[How this is built](docs/how-this-is-built.md)

## Why this exists

The Fleet API is metered, and an ordinary HTTP client makes its failure modes worse:

- `vehicle_data` costs $0.002 per request, and the 408 a sleeping car returns counts too: it was
  measured to use up the same per-vehicle quota slot as a successful call. A client that retries
  on a timeout pays twice and gets nothing.
- The token is a bearer token. Any host the client sends it to can act as you.
- A refresh can return a token with more scopes than the one it replaced: we have observed a
  refreshed token carrying every scope the app had ever been granted. A token you believed could
  only read may be able to unlock the car.

So this crate is built around five refusals. Each is enforced in code and pinned by a test that
fails when the guard is removed.

| It will not | Enforced by | Pinned by | In `offline_demo` |
|---|---|---|---|
| send anything but a GET to the Fleet API | The endpoint trait is sealed, and the one transport function is private and builds only GETs. Every other `ureq` entry point is on a clippy deny-list, and CI runs clippy with warnings as errors. | The `get-only` gate (one lint `allow` and one write call in the crate, both in the token refresh), and `clippy_toml_forbids_every_mutating_method`, which keeps deny-list entries from being deleted or commented out. | Not shown. It is a property of the code, not of a run. |
| send a billed request twice | Every request type carries its `Pricing`. Retries are off by default. The 401 and 412 recovery paths repair the token or the region but never re-send a billed request. | `a_401_on_a_paid_endpoint_is_not_re_fired`, `a_412_on_a_paid_endpoint_is_not_re_fired` | Scenes 3 and 4 |
| spend money on a sleeping car | `Client::send_guarded` asks a free endpoint first and skips the billed call unless the car is online. When the free check itself fails, it does not fall through to the billed call. | `guard_skips_the_paid_call_when_offline`, `guard_failure_does_not_force_the_paid_call` | Scenes 1 and 2 |
| use a token that can do more than read | A token whose `scp` claim holds a command scope is refused before anything is sent, on every pass of the send loop. The claim is read without verifying the signature, so a token that cannot be read is let through; the server stays the authority. | `read_client_refuses_token_with_command_scope_before_sending`, `refreshed_token_with_command_scope_is_refused_before_the_retry` | Scene 6 |
| send your token to a host you did not expect | The region URL Tesla returns is adopted only when it is `https`, on the default port and on the host allow-list. Redirects are off and environment proxies are ignored. | `rejects_every_untrusted_region_url`, `client_builder_does_not_follow_redirects` | Scene 7 for the region answer. Redirects and proxies are covered by tests only. |

A test name in a table is a claim too, so open one. These tests assert how many requests the mock
server received, not only what the client returned.

Tokens, VINs and coordinates are also redacted from every error message and body preview by a
single redaction layer, and the types that hold them print no secrets through `Debug`.

## Try it without a Tesla account

```sh
git clone https://github.com/fleetwire-dev/fleetwire-rs && cd fleetwire-rs
cargo run --example pricing_table   # which of the sixteen endpoints are billed
cargo run --example offline_demo    # four of the five refusals, against a local mock server
```

`offline_demo` starts a mock Fleet API on a loopback port and drives the real client against it in
eight scenes. Each scene prints what the mock server received and checks its claims against that
record, 23 in all. It needs no token, makes no outside connection, and exits non-zero if a claim
does not hold. The last column of the table says which scene shows which refusal.

With an account, the [quick start](crates/fleetwire/README.md#quick-start) lists your vehicles in
three steps. That call is free and does not wake the car.

## What is measured and what is not

The crate documentation separates behaviour observed on the live API from behaviour that only
follows Tesla's documentation, and it lists the open questions by number (O8 to O20) with the
condition that would close each. For example, the 408 response shape and the fact that it is
billed were measured; no 5xx response has been observed yet, so the 5xx handling is marked as
unverified. See "Verification levels" in the [crate docs](https://docs.rs/fleetwire).

## In numbers

As of 2026-11-20:

| | |
|---|---|
| Endpoints | 16 GET endpoints (12 vehicle, 4 user); 2 are billed. A response is strongly typed only where a live one was observed, and stays raw JSON elsewhere |
| Tests | 419 unit tests (410 in `fleetwire`, 9 in `fleetwire-testkit`), plus the README examples compiled as doctests |
| CI | stable, beta and MSRV 1.89; `clippy -D warnings`; rustdoc warnings as errors; `cargo deny`; gitleaks; invariant gates with self-tests; a weekly fuzz run |
| Runtime | synchronous (`ureq` + rustls), no async runtime |
| `unsafe` | forbidden workspace-wide; product code is linted against `unwrap` and `panic` |

## Where this comes from

This crate is the read-only client of a private workspace, in development since 2026-07-29, that
runs on a server following one car. The workspace also holds a telemetry receiver, a remote MCP
server, a notifier and the only component allowed to send a command to the car. As of 2026-11-20
it is 13 crates, about 285,000 lines of Rust, about 6,100 tests, 1,561 commits, 186 tagged
production deploys and 67 invariant gates that the commit hook and the completion check run.

Most of it stays private because it is built around one household's location data. Two things
about it are public:

- [docs/safety-boundaries.md](docs/safety-boundaries.md) describes how that system keeps its
  boundaries: an allow-listed command crate, a budget ledger that reserves every paid call before
  it is sent, one writer per shared file, and coordinates that stay out of every notification and
  tool answer.
- This repository receives code through one export script, which writes nothing when its privacy
  scan finds a match. That is why the history here is short: the development history is in the
  private repository.

## How it is built

The workspace is written with [Claude Code](https://claude.com/claude-code) as the implementer:
93% of its non-merge commits are co-authored by Claude. The human writes the specification and
the acceptance checks, decides anything irreversible, and judges results against the real car.
The agent is held to that by structure, not by trust: gates that fail the commit, hooks that stop
an edit, reviewer agents with fixed checklists that are themselves scored against past defects,
and a rule that a test counts only after it has been seen to fail without its guard.

[docs/how-this-is-built.md](docs/how-this-is-built.md) describes the workflow, with numbers and
with defects it caught in this crate. [AGENTS.md](AGENTS.md) and [REVIEW.md](REVIEW.md) are the
instructions agents work under in this repository.

## Repository layout

| Path | What it is |
|---|---|
| `crates/fleetwire` | The client library, published to crates.io as [`fleetwire`](https://crates.io/crates/fleetwire). Start with its [README](crates/fleetwire/README.md). |
| `crates/fleetwire-testkit` | A small mock HTTP server on std networking (no async runtime), used by the tests and by `offline_demo`. Not published. |
| `docs/` | Architecture, safety boundaries and the development workflow. |
| `fuzz/` | Fuzz targets for the code that reads bytes the crate did not write: response bodies, tokens, the region URL. |
| `scripts/check_invariants.py` | The invariant gates CI runs, each with a self-test. |

## Development

Builds on Linux and macOS. `rust-toolchain.toml` pins the toolchain used for development (rustup
installs it on first use); the minimum supported Rust version is 1.89.0.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked
cargo test --workspace --doc --all-features --locked
cargo +1.89.0 test --workspace --all-targets --all-features --locked   # MSRV
python3 scripts/check_invariants.py                                    # invariant gates
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for what fits the project, and [SECURITY.md](SECURITY.md)
to report a vulnerability.

## License

Licensed under either of Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE)) or MIT
license ([LICENSE-MIT](LICENSE-MIT)) at your option.
