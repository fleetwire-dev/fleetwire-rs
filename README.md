# fleetwire

An **unofficial, read-only** Rust client for the Tesla Fleet API that knows which calls cost money.

> **Not affiliated with, endorsed by, or sponsored by Tesla, Inc.**
> Tesla names and trademarks belong to their respective owners.

The crate lives in [`crates/fleetwire`](crates/fleetwire) — start with its
[README](crates/fleetwire/README.md) for the quick start, the list of billed calls and the current status.

| Path | What it is |
|---|---|
| `crates/fleetwire` | The client library (published to crates.io as `fleetwire`). |
| `crates/fleetwire-testkit` | A std-only mock HTTP server used by the tests. Not published. |

## Development

Builds on Linux and macOS. `rust-toolchain.toml` pins the toolchain used for development (rustup installs
it on first use); the minimum supported Rust version is 1.89.0.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked
cargo test --workspace --doc --all-features --locked
cargo +1.89.0 test --workspace --all-targets --all-features --locked   # MSRV
```

Public documentation is in English; many internal comments are in Korean. See
[CONTRIBUTING.md](CONTRIBUTING.md) for what fits the project, and [SECURITY.md](SECURITY.md) to report a
vulnerability.

## License

Licensed under either of Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE)) or MIT license
([LICENSE-MIT](LICENSE-MIT)) at your option.
