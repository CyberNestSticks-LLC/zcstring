# ZCString

[![Crates.io](https://img.shields.io/crates/v/zcstring.svg)](https://crates.io/crates/zcstring)
[![Docs.rs](https://docs.rs/zcstring/badge.svg)](https://docs.rs/zcstring)
[![MSRV](https://img.shields.io/badge/MSRV-1.79-informational.svg)](Cargo.toml)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)

`ZCString` is a context-aware string handle wrapping [`arcstr::Substr`](https://docs.rs/arcstr), designed for efficient, zero-copy string manipulation and JSON deserialization.

It uses thread-local storage to track an active "source" string buffer. When substrings are parsed or derived from that buffer, they are created as zero-allocation sub-slices referencing the original buffer rather than duplicating string contents into new heap allocations.

---

## Why ZCString?

When parsing structured text like JSON, standard deserializers allocate a separate heap `String` for every string field. `ZCString` eliminates redundant allocations:

* **Context-Aware Slicing**: Maintains an active `SOURCE` reference in thread-local storage. When creating a `ZCString` from a `&str` (via `ZCString::from` or Serde deserialization), it checks if the pointer resides within the active source buffer. If so, it returns a zero-copy sub-slice; otherwise, it falls back to a standard allocation.
* **Serde JSON Integration**: Using `serde_json_from_zcstring`, JSON documents can be deserialized directly into structs containing `ZCString` fields. Unescaped strings become zero-copy slices pointing directly into the input JSON, while escaped strings (which require unescaping) automatically fall back to independent allocations.
* **RAII Context Guards**: Safely manage active source scopes with `SourceGuard` or `ZCString::with_source`. Guards can be arbitrarily nested and automatically restore outer sources upon scope exit.
* **Raw and Lazy JSON Parsing**:
  * `ZCRawValue`: Preserves unparsed JSON fragments as zero-copy raw string slices.
  * `ZCRawValueAnd<T>`: Holds both the unparsed raw JSON slice and an eagerly decoded value `T`.
  * `ZCLazyValue<T>`: Defers parsing raw JSON into `T` until first access, caching the result in a thread-safe `OnceLock`.
* **Stream and File Reading**: Efficiently load UTF-8 strings from files or byte streams via `ZCString::read_range`, `ZCString::read_exact`, and `ZCString::from_file`.
* **String Iteration & Mapping**: Transform standard `&str` iterators (e.g. `.lines()`, `.split()`) into zero-copy `ZCString` streams using `wrap_iter`, or apply zero-allocation transforms using `map`.
* **Compact Memory**: `ZCString` is identical in size to `&str` (16 bytes on 64-bit platforms: a pointer to the parent `ArcStr` and two 32-bit offset values).

---

## Installation

Add this to your `Cargo.toml`:

```toml
[dependencies]
zcstring = "0.5.0"
arcstr = "1.2"
serde = { version = "1.0", features = ["derive"] }
```

---

## Usage Guide

### 1. Zero-Copy Serde JSON Deserialization

```rust
use arcstr::literal;
use serde::Deserialize;
use std::error::Error;
use zcstring::{serde_json_from_zcstring, ZCString};

#[derive(Debug, Deserialize)]
struct LogRecord {
    level: ZCString,
    message: ZCString,
    timestamp: ZCString,
}

fn main() -> Result<(), Box<dyn Error>> {
    let json = literal!(r#"{"level":"INFO","message":"Service started","timestamp":"2026-11-15T12:00:00Z"}"#);

    // serde_json_from_zcstring sets `json` as the active thread-local source context.
    let record: LogRecord = serde_json_from_zcstring(json.into())?;

    println!("Log: {:?}", record);
    Ok(())
}
```

### 2. Deferred Evaluation with `ZCLazyValue`

`ZCLazyValue` avoids parsing expensive or conditionally needed JSON structures until `.value()` is explicitly invoked:

```rust
use serde::{Deserialize, Serialize};
use zcstring::{serde_json_from_zcstring, ZCLazyValue, ZCString};

#[derive(Deserialize, Serialize, Debug)]
struct Metrics {
    samples: Vec<f64>,
}

#[derive(Deserialize, Serialize, Debug)]
struct Packet {
    id: u64,
    #[serde(default, skip_serializing_if = "ZCLazyValue::is_value_only")]
    metrics: ZCLazyValue<Metrics>,
}

fn process_packet(json: ZCString) -> Result<(), Box<dyn std::error::Error>> {
    let packet: Packet = serde_json_from_zcstring(json)?;

    // Inspect metadata without parsing `metrics`
    if packet.id == 0 {
        return Ok(());
    }

    // Deferred parsing occurs only on .value()
    let metrics = packet.metrics.value()?;
    println!("Processed {} samples", metrics.samples.len());
    Ok(())
}
```

### 3. Preserving Raw JSON with `ZCRawValue` & `ZCRawValueAnd`

```rust
use serde::Deserialize;
use zcstring::{serde_json_from_zcstring, ZCRawValue, ZCRawValueAnd, ZCString};

#[derive(Deserialize, Debug)]
struct Payload {
    count: u32,
    tag: ZCString,
}

#[derive(Deserialize, Debug)]
struct Message {
    id: u64,
    // Holds both raw JSON slice and parsed payload:
    payload: ZCRawValueAnd<Payload>,
    // Holds only raw unparsed JSON:
    extra: ZCRawValue,
}
```

### 4. Zero-Copy String Iteration & Mapping

```rust
use zcstring::ZCString;

let text = ZCString::from("  alpha  \n  beta  \n  gamma  ");

// wrap_iter promotes iterator slices into zero-copy ZCStrings:
let trimmed_lines: Vec<ZCString> = text
    .wrap_iter(|s| s.lines())
    .map(|line| line.map(|s| s.trim()))
    .collect();

assert_eq!(trimmed_lines, vec!["alpha", "beta", "gamma"]);
assert!(text.source_of(&trimmed_lines[0]));
```

### 5. Efficient File & Stream Reading

```rust
use std::io::Cursor;
use zcstring::ZCString;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Read range directly into a reference-counted buffer:
    let mut data = Cursor::new(b"Hello Zero-Copy World");
    let word = ZCString::read_range(&mut data, 6..15)?;
    assert_eq!(word, "Zero-Copy");

    // Read an entire file directly:
    // let file_content = ZCString::from_file("data.json")?;
    Ok(())
}
```

### 6. Decoupling Slices with `detach()`

If a small substring is kept from a massive buffer (e.g. 5 bytes from a 1 GB payload), calling `.detach()` allocates an independent copy so the larger buffer can be reclaimed:

```rust
use zcstring::ZCString;

let huge_doc = ZCString::from_str_without_source("..."); // large buffer
let small_slice = huge_doc.substr(0..5);

// Retain small slice independently and allow `huge_doc` to drop:
let isolated = small_slice.detach();
```

---

## Running Examples

The repository includes runnable examples demonstrating core capabilities:

```bash
cargo run --example trivial_example
cargo run --example lines_example
cargo run --example json_example
cargo run --example lazy_example
cargo run --example file_example
```

---

## Minimum Supported Rust Version (MSRV)

This crate requires **Rust 1.79** or newer.

---

## License

Licensed under either of:

* Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
* MIT license ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.
