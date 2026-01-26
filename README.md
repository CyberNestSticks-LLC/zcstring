# ZCString

[![Crates.io](https://img.shields.io/crates/v/zcstring.svg)](https://crates.io/crates/zcstring)
[![Docs.rs](https://docs.rs/zcstring/badge.svg)](https://docs.rs/zcstring)

`ZCString` is a context-aware string wrapper for
[`arcstr::Substr`](https://docs.rs/arcstr) designed for efficient, zero-copy
string management.  It uses thread-local storage to track a "source"
string, allowing substrings to be derived from existing memory buffers
without triggering new allocations. The currently supported use
case is parsing of json strings with serde_json.

## Why ZCString?

When parsing or processing large strings, you often want to derive
substrings that point back to the original memory.  `ZCString` simplifies
this by:

* **Tracking Source Memory**: It maintains a thread-local `SOURCE` reference that acts as the active memory context.
* **Identity-Based Slicing**: When creating a string via
  `from_str_with_source`, it checks if the pointer resides within the
  current `SOURCE`.  If it does, it returns a sub-slice; otherwise, it falls
  back to a standard allocation.
* **Recursive Safety**: It uses RAII guards to allow nesting of different
  source contexts, automatically restoring the previous source when a scope
  ends.



## Features

* **Zero-allocation**: Sub-slices share the same reference-counted buffer as
  the parent `ArcStr`.
* **RAII Contexts**: Safely manage the active "source" using `with_source`
  or `get_source_guard`.
* **Serde Support**: Optional integration for zero-copy deserialization of
  JSON and other formats.

## Usage

Add this to your `Cargo.toml`:

```toml
[dependencies]
# by default the std and serde features are enable
zcstring = "0.1.0"

## Example code
cargo run --example simple_example
