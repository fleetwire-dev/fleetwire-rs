# Changelog

All notable changes to `fleetwire` are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-11-12

First public version of the read-only client.

### Added

- `Client` with four entry points: `send`, `send_guarded`, `send_metered` and `send_raw`.
- Sixteen GET endpoints verified against live responses (12 vehicle, 4 user), each tagged with its
  `Pricing`; the two billed ones are never retried or re-sent by the recovery paths.
- `TokenManager`: owner-only token storage, refresh-token rotation persisted before returning, and
  refreshes collapsed within and across processes.
- `examples/list_vehicles`, and a quick start in the README.
