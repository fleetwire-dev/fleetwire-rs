# Fuzzing

These targets hand arbitrary bytes to the places in `fleetwire` that read input the crate did not
write. A panic or a failed assertion in any of them is a bug.

| Target | What it feeds | What must hold |
|---|---|---|
| `redact` | Text into the redaction layer | No panic. A preview stays within its byte bound. A bearer token planted in the middle of the text never appears in the output. |
| `decode` | A status, a content type and a body into the response decoder | No panic, and the error it builds prints without panicking. |
| `token` | A string into the unverified scope check | No panic. Every input gets a verdict. |
| `region` | A string into the region URL check and the `Retry-After` parser | No panic. An accepted URL is https, on the default port, without userinfo, and on a host from the default allow-list. |

The entry points live in `fleetwire::fuzz`, a module that exists only under `--cfg fuzzing` (which
`cargo fuzz` sets). It is not part of the public API and is absent from a normal build.

## Running

`cargo fuzz` needs a nightly toolchain: libFuzzer's instrumentation uses unstable flags.

```sh
rustup toolchain install nightly --profile minimal   # once
cargo install cargo-fuzz --locked                    # once
cargo +nightly fuzz run redact -- -max_total_time=300
cargo +nightly fuzz run decode -- -max_total_time=300
cargo +nightly fuzz run token  -- -max_total_time=300
cargo +nightly fuzz run region -- -max_total_time=300
```

Run them from the repository root. `fuzz/` is a separate package outside the root workspace, so the
normal build, the tests and the dependency budget are not affected. `corpus/`, `artifacts/` and
`target/` are not tracked.

## When something is found

1. A file appears under `fuzz/artifacts/<target>/`. `cargo +nightly fuzz fmt <target> <file>` prints
   the input.
2. Move that input into a regression test in the module's `tests.rs` and see it fail before fixing
   the code.
3. Run the target again for the same length of time.

## Record

| Date | Target | Time | Executions | Result |
|---|---|---|---|---|
| 2026-11-20 | redact | 60 s | 1.22 M | no panic, no failed assertion |
| 2026-11-20 | decode | 60 s | 0.84 M | no panic |
| 2026-11-20 | token | 60 s | 4.20 M | no panic |
| 2026-11-20 | region | 60 s | 0.63 M | no panic, no failed assertion |
