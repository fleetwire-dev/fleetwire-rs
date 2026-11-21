# Roadmap

Each release has one goal, written as one line, and an outside check: something anyone can verify
without trusting this repository. Dates are targets, not promises. Smaller chores are tracked with
labels, not milestones.

Progress and the issues in each release are on the
[milestones page](https://github.com/fleetwire-dev/fleetwire-rs/milestones).

| Release | Goal | Target | How you can check it |
|---|---|---|---|
| 0.2.1 | Trusted publishing and a semver promise | 2026-11-30 | The release on crates.io is published from CI by trusted publishing, with no long-lived token. CI runs `cargo-semver-checks`. docs.rs builds 0.2.1. |
| — | A write-up on Tesla's billing limit | around 2026-12-07 | Published and linked from the README, after 0.2.1 and before 0.3.0. |
| 0.3.0 | Stop the call before Tesla's limit stops you | 2026-12-14 | A cost ledger crate is on crates.io. An example that needs no Tesla account runs in CI and shows a paid call refused at the limit. |
| — | First outside user | 2026-12-31 | Someone outside the project opens an issue or a pull request, or depends on the crate. Their first issue gets an answer within 72 hours. |
| 1.0 | Safe to depend on | 2027 Q1 | Written semver and MSRV policies, and three public repositories that depend on the crate. |

## 0.2.1: Trusted publishing and a semver promise

- Publish from CI with crates.io trusted publishing (OIDC), then delete the registry token.
- Run `cargo-semver-checks` in CI, so a breaking change cannot ship in a patch release.
- Make the `invariants` check required on `main`.
- Ship what landed after 0.2.0: the rewritten README, the `offline_demo` example and the fix for a
  read timeout just before the request deadline.
- Fix the comment on `GetRegion::required_scopes`, which does not match the scope expression.

## 0.3.0: Stop the call before Tesla's limit stops you

Every Fleet API account has a billing limit. When usage goes over it, Tesla suspends API access
until the limit is raised or the next monthly cycle starts. It also removes the Fleet Telemetry
streaming configuration from the vehicles, and does not restore it when access comes back
([Tesla: billing and limits](https://developer.tesla.com/docs/fleet-api/billing-and-limits)).
Finding out from Tesla is finding out too late.

The client already knows which endpoints are billed and refuses some paid calls it can tell are
wasted. 0.3.0 adds the other half: counting what this program has spent, and refusing the next
paid call before it is sent once a limit you set is reached.

- Compute the cost of a call from the published price table before it is sent.
- A daily limit that refuses a paid call before it goes out, not after.
- A ledger file written atomically, so a crash never leaves a half-written record.
- One example that runs without a Tesla account.

It is a library you call from your own Rust program, not an app to run, and the ledger is a plain
file you can read and back up. It counts only the calls made through it, so it does not promise to
keep an account that other apps also use under Tesla's limit.

The write-up comes first, around 2026-12-07: what the billing limit does when it is reached, and
how to stay under it.

Later in 0.3.x: monthly reports, several vehicles in one ledger, and a hash-chained ledger that
shows when a line was changed or removed.

## Labels

Work that does not belong to one release:

- `scorecard`: raise the OpenSSF Scorecard (best-practices badge, a second reviewer).
- `testkit`: publish the std-only mock HTTP server used by the tests, and a replayable synthetic
  stream for local tests ([#13](https://github.com/fleetwire-dev/fleetwire-rs/issues/13)).
- `hash-chain`: a hash-chained append-only log as its own crate.
- `housekeeping`: stale branches and similar clean-up.

## Not on this roadmap

This crate stays read-only. Commands that change a vehicle's state are not planned for it;
[docs/safety-boundaries.md](docs/safety-boundaries.md) describes how they are kept apart.
