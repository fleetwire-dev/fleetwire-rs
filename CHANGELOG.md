# Changelog

All notable changes to `fleetwire` are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
