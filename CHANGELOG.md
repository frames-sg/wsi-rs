<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# Changelog

## [Unreleased]

### Removed

- Removed internal release/stability/architecture Markdown files and stale
  benchmark-tooling documentation from public repo docs.

## [0.4.0] - 2026-05-27

- Added `cargo xtask rc-preflight`, API snapshot, fuzz, package, and supply chain gates.
- Hardened public constructors and request builders for the 0.4 API cleanup
  line.
- Documented and tested Metal/CUDA feature public API surfaces.

## [0.3.1] - 2026-05-26

- Raised the j2k crate family dependency floor to 0.4.4.

## [0.3.0] - 2026-05-12

- Moved the public dependency surface to the pre-1.0 `j2k` 0.4 crate
  family and refreshed repository metadata for `frames-sg/wsi-rs`.

## [0.1.5] - 2026-05-06

- Raised the Metal JPEG adapter dependency to `j2k-jpeg-metal` 0.2.2.

## [0.1.4] - 2026-05-06

- Added a required compressed-device tile output preference.

## [0.1.3] - 2026-05-05

- Improved malformed NDPI error reporting.

## [0.1.2] - 2026-05-05

- Added raw JPEG tile passthrough and NDPI Metal tile batch decode.
- Moved JPEG 2000 decode through the `j2k` facade.
- Updated `lru` to avoid `RUSTSEC-2026-0002`.

## [0.1.1]

- Initial public release.

[Unreleased]: https://github.com/frames-sg/wsi-rs/compare/v0.4.0...HEAD
[0.4.0]: https://github.com/frames-sg/wsi-rs/compare/v0.3.1...v0.4.0
[0.3.1]: https://github.com/frames-sg/wsi-rs/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/frames-sg/wsi-rs/compare/v0.1.5...v0.3.0
[0.1.5]: https://github.com/frames-sg/wsi-rs/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/frames-sg/wsi-rs/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/frames-sg/wsi-rs/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/frames-sg/wsi-rs/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/frames-sg/wsi-rs/releases/tag/v0.1.1
