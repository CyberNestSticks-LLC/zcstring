# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.5.0] - 2026-11-15

### Added

- **`ZCLazyValue` Redesign & Capabilities**:
  - Direct constructors: `ZCLazyValue::from_raw(raw: ZCString)`, `ZCLazyValue::from_value(t: Arc<T>)`, and `ZCLazyValue::from_error(e: Arc<serde_json::Error>)`.
  - State inspection: `is_decoded()` to check initialization status, and `is_value_only()` to identify default/in-memory values.
  - Integration with `#[serde(default, skip_serializing_if = "ZCLazyValue::is_value_only")]` for omitting absent fields during re-serialization.
  - Const-generic error logging: `ZCLazyValue<T, const L: bool = false>` logs parse failures to `log::error!` when `L = true`.
  - Verified deferred decode containment: lazily decoded `ZCString`s retain zero-copy provenance back to the original source `ZCString`.
- **`ZCRawValueAnd<T>` Enhancements**:
  - Added constructor `ZCRawValueAnd::new(raw: ZCString) -> Result<Self, serde_json::Error>`.
  - Added `into_parts(self) -> (ZCString, T)` to decompose into raw slice and decoded value.
  - Added `Default` implementation when `T: Default`.
- **`ZCRawValue` Enhancements**:
  - Added `ZCRawValue::new(v: ZCString)` and `ZCRawValue::into_value(self) -> ZCString`.
  - Added validation during serialization via `serde_json::value::RawValue`.
- **`ZCString` Enhancements**:
  - Added `ZCString::detach(&self) -> Self` to decouple a small sub-slice from a large parent allocation.
  - Added `ZCString::wrap_iter` to convert standard `&str` iterators (e.g. `.lines()`, `.split()`) into zero-copy `ZCString` streams.
  - Added `ZCString::map` for zero-allocation transformations (e.g. `.trim()`).
  - Added explicit constructors `from_str_without_source` and `from_str_with_source`.
- **Documentation & Testing**:
  - Complete rustdoc comments across all types and methods with runnable doctests.
  - Expanded unit test suite from 28 to 47 unit tests (+ 29 doc-tests = 76 total tests), covering high-concurrency evaluation, boundary conditions, and unwind safety.

### Changed

- **Breaking**: `ZCLazyValue::value(&self)` now returns `Result<&Arc<T>, &Arc<serde_json::Error>>` (replacing the previous `&Option<T>` / `try_value` pattern).
- **Breaking**: `serde`, `serde_json` (with `raw_value` feature), and `log` are now direct core dependencies rather than optional cargo features.
- **Breaking**: Raised Minimum Supported Rust Version (MSRV) to Rust 1.79.
- `ZCLazyValue` comparison traits (`PartialEq`, `Eq`, `PartialOrd`, `Ord`) and `Hash` now consistently compare/hash inner values when `Ok`, treat all deserialization errors as equal when `Err`, and order `Ok` > `Err`.
- `ZCRawValueAnd` comparison and hashing traits now operate directly on `decoded`, ignoring raw whitespace variations.

### Fixed

- Hardened `source_of` pointer arithmetic using `checked_sub` and `checked_add` to prevent integer overflow.
- Improved error handling in `ZCString::read_range` and `read_exact` to prioritize I/O errors over subsequent UTF-8 validation errors.

---

## [0.4.2] - 2026-11-13 (Deprecated)

### Fixed

- Removed `T: Default` trait bound requirement on `Default` implementation for `ZCLazyValue<T>`.

## [0.4.1] - 2026-11-13 (Deprecated)

### Fixed

- Adjusted internal error caching within `ZCLazyValue`.

## [0.4.0] - 2026-11-13 (Deprecated)

### Added

- Added `ZCLazyValue<T>` for deferred zero-copy JSON parsing backed by `OnceLock`.

### Changed

- Renamed accessor methods on `ZCRawValueAnd<T>` from `get_raw()` to `raw()` and `get_data()` to `value()`.
- Renamed `ZCRawValue::get()` to `ZCRawValue::value()`.

---

## [0.3.6] - 2026-11-03

### Added

- Implemented `Clone` for `ZCRawValueAnd<T>`.

---

## [0.3.5] - 2026-11-03

### Added

- Implemented `Clone` for `ZCRawValue`.

---

## [0.3.4] - 2026-11-03

### Added

- Introduced `ZCRawValue` to capture raw, unparsed JSON string slices without allocation.

---

## [0.3.3] - 2026-09-29

### Added

- Re-exported `arcstr::literal_substr` macro.
- Added `const fn` constructors `ZCString::new()` and `ZCString::l()` for static memory initialization.

---

## [0.3.2] - 2026-09-29

### Added

- Re-exported `arcstr::literal` macro.

---

## [0.3.1] - 2026-02-13

### Added

- Implemented bidirectional `PartialEq` between `ZCString` and `String`.

---

## [0.3.0] - 2026-01-29

### Added

- Added streaming and file reading methods:
  - `ZCString::from_file` to read entire files into a `ZCString`.
  - `ZCString::read_range` to read arbitrary byte ranges from `Read + Seek` streams.
  - `ZCString::read_exact` to read exact byte counts from `Read` streams.

---

## [0.2.0] - 2026-01-27

### Changed

- Switched error handling to `serde_json::Error` for improved thread safety (`Send + Sync`).

---

## [0.1.0] - 2026-01-26

### Added

- Initial release of `ZCString`: context-aware zero-copy string handle wrapping `arcstr::Substr` with thread-local source context and `serde_json` support.
