# rust-swig but Worse

this crate is a work-in-progress and currently only contains README files

automatically generate an [ABI](https://github.com/kazimuth/transgress-rs/tree/master/transgress-abi) /
FFI API for rust crates

ideally:

- magically create ABI over top of ABI-less rust
- use from multiple languages
- not require hand-annotation

use-cases:

- stable rust plugins
- python bindings
- java bindings
- nim / c / c++ bindings
- wasm bindings

impl strategies:

- get API surface

  - rustdoc path

    - handles macro-expansion, resolve, ... for us
    - just parse rustdoc's output, lol
    - https://github.com/servo/html5ever

  - other paths: rust-analyzer, rustc nightly, hand implementation, ...

compare: cffi, abi-vs-api bindings

1. lower to ABI

- generate rust shim implementing ABI
  - mask ABI-less things
- generate [target-lang] shim using ABI

2. OR, generate rust code that generates API

- wasm-bindgen
- rust-swig
