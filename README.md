# H2

A Tokio aware, HTTP/2.0 client & server implementation for Rust.

[![Build Status](https://travis-ci.org/carllerche/h2.svg?branch=master)](https://travis-ci.org/carllerche/h2)
[![Codecov](https://img.shields.io/codecov/c/github/carllerche/h2.svg)](https://codecov.io/gh/carllerche/h2)
<!-- [![Crates.io](https://img.shields.io/crates/v/h2.svg?maxAge=2592000)](https://crates.io/crates/h2) -->
<!-- [![Documentation](https://docs.rs/h2/badge.svg)][dox] -->

**This library is not production ready. Do not try to use it in a production
environment or you will regret it!** This crate is still under active
development and there has not yet been any focus on documentation (because you
shouldn't be using it yet!).

More information about this crate can be found in the [crate documentation][dox]

[dox]: https://carllerche.github.io/h2/h2

## Features

* Client and server HTTP/2.0 implementation.
* Implements the full HTTP/2.0 specification.
* Passes [h2spec](https://github.com/summerwind/h2spec).
* Focus on performance and correctness.
* Built on [Tokio](https://tokio.rs).

## Non goals

This crate is intended to only be an implementation of the HTTP/2.0
specification. It does not handle:

* Managing TCP connections
* HTTP 1.0 upgrade
* TLS
* Any feature not described by the HTTP/2.0 specification.

The intent is that this crate will eventually be used by
[hyper](https://github.com/hyperium/hyper), which will provide all of these features.

## Usage

To use `h2`, first add this to your `Cargo.toml`:

```toml
[dependencies]
h2 = { git = 'https://github.com/carllerche/h2' } # soon to be on crates.io!
```

Next, add this to your crate:

```rust
extern crate h2;

use h2::server::Server;

fn main() {
    // ...
}
```

## License

`h2` is primarily distributed under the terms of both the MIT license and the
Apache License (Version 2.0), with portions covered by various BSD-like
licenses.

See LICENSE-APACHE, and LICENSE-MIT for details.
