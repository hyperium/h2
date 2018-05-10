# h2 integration tests

This crate includes the h2 integration tests. These tests exist in a separate
crate because they transitively depend on the `unstable` feature flag via
`h2-support`. Due to a cargo limitation, if these tests existed as part of the
`h2` crate, it would require that `h2-support` be published to crates.io and
force the `unstable` feature flag to always be on.

## Setup

Install honggfuzz for cargo:

```rust
cargo install honggfuzz
```

## Running

From within this directory, run the following command:

```
HFUZZ_RUN_ARGS="-t 1" cargo hfuzz run h2-fuzz
```
