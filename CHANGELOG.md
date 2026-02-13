# Changelog
All notable changes to this project will be documented in this file.

-- TBD

## [0.3.0] - 2026-01-29
### Added

- Support for efficient direct to ZCString file and Read trait reads
-  ZCString::from_file([file path])
-  ZCString::read_range([Read + Seek], [start], [end])
-  ZCString::read_exact([impl Read], [bytes])
-  ZCString::read_upto([impl Read], [bytes])

## [0.3.1] - 2026-01-13
### Added

- Support for PartialEq to String
