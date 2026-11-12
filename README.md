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

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked
cargo test --workspace --doc --all-features --locked
```

Code comments are written in Korean; the public documentation of `fleetwire` is in English.
Security reports: see [SECURITY.md](SECURITY.md).

## License

Licensed under either of Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE)) or MIT license
([LICENSE-MIT](LICENSE-MIT)) at your option.
