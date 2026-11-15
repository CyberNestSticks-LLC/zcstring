# Changelog
All notable changes to this project will be documented in this file.

-- TBD

## [0.3.0] - 2026-01-13
### Added

- Support for efficient direct to ZCString file and Read trait reads
-  ZCString::from_file([file path])
-  ZCString::read_range([Read + Seek], [start], [end])
-  ZCString::read_exact([impl Read], [bytes])
-  ZCString::read_upto([impl Read], [bytes])

## [0.3.1] - 2026-01-13
### Added

- Support for PartialEq to String

## [0.3.2] - 2026-09-29
### Added

- Re-export Arcstr literal macro

## [0.3.3] - 2026-09-29
### Added

- Re-export Arcstr literal_substr macro
- const constructors of empty and const substr derived ZCString

## [0.3.4] - 2026-11-03
### Added

- ZCRawValue type that behaves like an easier to use RawValue
- capturing raw JSON values.

## [0.3.5] - 2026-11-03
### Added

- ZCRawValue really should support clone

## [0.3.6] - 2026-11-03
### Added

- ZCRawValueAnd really should support clone

## [0.4.0] - 2026-11-13
### Changed 
- Changed "data" to "value" in ZCRawValueAnd

### Added
- ZCLazyValue
Similar to ZCRawValueAnd except the raw ZCString is parsed in a Lazy
manner

