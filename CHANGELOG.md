<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# Changelog

## [Unreleased]

## [0.7.0] - 2026-09-05

### Added

- Added ARGOS and Huron TIFF readers backed by real public-corpus fixtures,
  including ARGOS sparse tiles and Z planes and associated images for both vendors.
- Added single-plane brightfield CZI reading with uncompressed, JPEG and JPEG XR
  subblocks, plus JPEG XR tiled-TIFF decoding through the external JXR crate.
  BGR48 embedded CZI preview images preserve 16-bit samples.
- Added exact raw JPEG access for eligible CZI, MIRAX, full-resolution VMS and
  ordinary tiled-TIFF native/display tiles.
- Added DICOM JPEG Extended, progressive Huffman and lossless transfer-syntax
  routing through the external JPEG decoder, with SOF-process and predictor checks.
- Added per-slide resource limits, associated-image ICC metadata and OpenSlide
  ABI access, and strict JP2K/HTJ2K Metal and CUDA resident tile APIs.

### Changed

- Normal tile, batch, controlled, region, display and associated reads return
  `CpuTile`. `SlideReader` requires one CPU tile method and preserves batch order
  and cardinality by default.
- Replaced public output-routing and sampling controls with
  `DecodeAcceleration::{Auto, CpuOnly}`. Automatic acceleration measures device
  decode plus readback, requires a 15% device win and retains a CPU fallback.
- Consolidated JP2K CPU work onto one process-wide pool and removed per-slide
  thread-pool configuration.
- Built-in probes and bundle parsers share the configured metadata/index budget.
  Custom registry readers remain trusted during open and are conservatively
  admitted for reads afterward.
- Default retained caches use 64 MiB for source tiles, 32 MiB for display tiles
  and a byte-weighted 32 MiB aggregate private budget. Legacy per-cache requests
  are proportionally clamped within that total.
- Reused decoded CZI source blocks across output tiles, composed RGB directly
  and reused preflight file handles. Embedded associated-image metadata probing
  preserves source decoding/validation while avoiding unused canvas composition.
- Borrowed cached NDPI MCU indexes, bounded NDPI region batches and coalesced
  overlapping shared-cache misses across concurrent region reads.
- Buffered MIRAX and Olympus ETS index I/O and enabled the external SHA-256
  hardware backend on macOS/aarch64 with its software fallback.
- Delegated JPEG 2000 header/coding validation to J2K, retaining WSI limits and
  pixel contracts for multi-tile and multi-part codestreams.

### Fixed

- Rejected short TIFF decoded payloads, malformed CZI raw payload lengths and
  invalid CZI segment/directory geometry before dependency processing.
- Preserved per-IFD JPEG tables for generic TIFF and Philips associated-image
  strips, and decoded compressed associated images one strip at a time.
- Honored DICOM RGB/YBR color-transform metadata and retained the grayscale
  lossless JPEG batch path.
- Preserved CZI plane separation, resource limits and mosaic overlap ordering;
  requests for missing native levels return errors.
- Hardened Aperio, Ventana, DICOM, NDPI and MIRAX geometry, sparse-data and
  large-offset handling, and rejected non-finite physical metadata.
- Matched OpenSlide edge semantics for invalid levels, missing associated images,
  sticky-error output clearing, zero-length ICC reads, Leica barcodes and bounds.
- Kept recognizable corrupt MIRAX bundles detectable so opening reports the
  error, and stopped installing incorrect OpenSlide `.4` library aliases.

### Removed

- Removed `TileOutputPreference`, `DeviceOutputContext`, `OutputBackendRequest`,
  `TilePixels`, `DeviceTile`, public route-decision types, the route-sample knob
  and ordinary-JPEG GPU routing. No deprecated forwarding API is retained.
- Removed `SlideReader::recommended_shared_cache_bytes`; cache sizing is a
  per-slide policy rather than a backend-specific hint.
- Removed QuPath-specific integration guidance. Sakura remains unsupported
  pending a redistributable real sample.

## [0.6.0] - 2026-08-25

### Added

- Added reproducible OpenSlide comparison tooling, changed-line and component
  coverage gates, deterministic workload checksums, and CPU/host metadata for
  performance captures.
- Added focused concurrency, geometry, compositor, cache, parser, and device
  regression coverage while moving test-only modules out of production LCOV.

### Changed

- Migrated the optional Metal backend from `metal-rs` to the J2K 0.10.0
  `objc2-metal` ownership model. `MetalBackendSessions::system_default` is the
  new common constructor; expert raw-buffer adoption now accepts
  `MetalBuffer`, and the deprecated unsynchronized `MetalDeviceStorage::Buffer`
  variant was removed.
- Reworked region composition, JPEG 2000 decoding, and DICOM frame access around
  explicit planning, validation, I/O, cache, and backend ownership boundaries.
  Decode runtime selection is now passed explicitly at internal operation
  boundaries instead of relying on thread-local state; the public `SlideReader`
  interface remains unchanged.
- Consolidated parity-corpus and OpenSlide test/performance support. Performance
  capture schema 6 removes metadata duplicated by the run records and declared
  capture plan; schema 5 captures remain readable by the checksum-enforcing
  comparator.
- `CpuTile::pixels_arc` now returns `Option<Arc<Vec<u8>>>` and clones the tile's
  existing `Arc` without copying pixels. Callers migrating from `Arc<[u8]>`
  should change the stored type and use `pixels.as_slice()` when they need a
  byte slice; constructing a new `Arc<[u8]>` remains possible but copies.
- The OpenSlide compatibility shim now reports
  `OpenSlide 4.0.1+wsi-rs-0.6.0`, matching the pinned comparison ABI version
  while retaining the shim package version in the compatibility string.
- Consolidated decoded-cache single-flight behavior and split format,
  composition, codec, and test modules along existing ownership boundaries.
  Public APIs remain unchanged except for the planned `pixels_arc` migration.

### Removed

- Removed obsolete JP2K parsing/conversion code, self-only XML helpers, and
  unreachable public visibility identified by the 0.6 source audit.

## [0.5.2] - 2026-07-31

### Added

- Added checked CUDA-to-CPU tile download through `CudaDeviceTile::download_cpu`,
  keeping device surface internals behind the WSI-RS boundary.
- Added byte-sized shared cache ownership for the OpenSlide shim; attached
  slides retain the cache after the C handle is released and may share entries.
- Added cancellation-aware level preparation so DICOM frame indexes can be
  built once in the background and reused by concurrent reads.
- Added opt-in typed controlled-read diagnostics for DICOM frame-index
  strategy, fallback, reuse, and timing; the default path does not sample a
  clock or allocate diagnostic storage.

### Changed

- Upgraded the complete `j2k` codec family to 0.8. Raw JPEG 2000 codestream
  reads retain the codec's strict, fail-closed decode policy.
- Derived DICOM, MIRAX, VMS, and Zeiss private decoded-data cache capacities
  from one aggregate `CacheConfig` budget, including zero-capacity caches for
  excess images or shards, and moved built-in backend composition out of core.
- Applied the caller's cache policy during format probing and reused that
  configured parse during open instead of first constructing default caches.
- Matched OpenSlide's floor-like best-level selection at exact boundaries and
  for non-finite requests.
- Controlled tile reads now preserve the original batch order and cardinality,
  share adaptive CPU/device routing with existing reads, and treat cancellation
  as terminal before additional probes, fallback, or cache publication.
- Split TIFF-family layout construction into focused format modules while
  preserving the existing public reader behavior.

### Fixed

- Rejected oversized or overflowing fragmented DICOM compressed frames before
  allocation, and made truncated MIRAX quickhash ranges fail instead of hashing
  a prefix.
- Replaced predictable Zeiss attachment paths with exclusively created,
  automatically removed temporary files.
- Preserved recoverable shim installer backups and both typed failures through
  the additive `execute_install_detailed` API when a primary install failure is
  followed by rollback failure; the existing `execute_install` string-error API
  remains source compatible.
- Replaced the normal DICOM compressed-frame scan with validated seek-based
  Extended/Basic Offset Table indexing and grouped frame I/O, retaining the
  token parser as a fallback for unusual supported layouts.
- Fixed cancellation races in adaptive route publication and prevented partial
  DICOM indexes from entering the preparation cache.
- Kept logical TIFF edge-tile dimensions conformant across CPU and Metal JPEG
  and JPEG 2000 output, including right and bottom edge regression coverage.

## [0.5.1] - 2026-07-17

### Added

- Added cloneable cancellation tokens and controlled tile-read APIs while
  preserving the existing tile-read interfaces.
- Added an opt-in Metal edge-tile conformance test for local SVS fixtures.

### Fixed

- Cropped Metal JPEG and JPEG 2000 edge tiles to their logical dimensions so
  GPU and CPU reads return identical geometry instead of repeating padded
  pixels at the right or bottom slide edge.
- Used the TIFF tile span for logical compressed-tile dimensions rather than
  the codec's padded physical dimensions.
- Added cancellation checks around tile I/O and codec admission so obsolete
  viewer generations can stop before producing stale results.

## [0.5.0] - 2026-07-14

### Changed

- Renamed the public crate and repository identity from `statumen` to `wsi-rs`.
- Raised the public `j2k` crate family dependency floor to 0.7.2 and removed
  the yanked pre-rename `signinum-*` 0.5 dependency aliases.
- Metal decode and conversion outputs now retain their owning GPU allocation
  through `ResidentMetalImage`; safe encode paths reject legacy raw-buffer
  storage whose completion and lifetime cannot be verified.
- Added fail-closed CUDA resident-decode validation on the self-hosted CUDA
  release runner.
- Refreshed public API snapshots for source ICC profile metadata and format
  vendor detection surfaces.

### Fixed

- Fixed Metal YCbCr conversion addressing beyond 4 GiB with checked host-side
  span validation and a 64-bit shader path, while retaining the validated
  32-bit path for smaller images.
- Fixed API stability tooling package selection after the crate rename.
- Fixed CUDA feature matrix compilation after the j2k dependency rename.
- Removed stale cargo-deny duplicate skip configuration.
- Bumped `.svcache` to schema 3 so freshness includes canonical source identity
  and a bounded sampled content digest rather than only size and modification
  time. Schema 2 caches must be rebuilt.
- Hardened parser budgets, companion-path confinement, probe cache identity,
  decoder cardinality handling, transactional shim installation, and bounded
  fuzz campaigns for the 0.5 release candidate.
- Added reproducible Cargo Vet policy and documented time-bound upstream
  exceptions for the unmaintained DICOM and Metal transitives.

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

[Unreleased]: https://github.com/frames-sg/wsi-rs/compare/v0.7.0...HEAD
[0.7.0]: https://github.com/frames-sg/wsi-rs/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/frames-sg/wsi-rs/compare/v0.5.2...v0.6.0
[0.5.2]: https://github.com/frames-sg/wsi-rs/compare/v0.5.1...v0.5.2
[0.5.1]: https://github.com/frames-sg/wsi-rs/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/frames-sg/wsi-rs/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/frames-sg/wsi-rs/compare/v0.3.1...v0.4.0
[0.3.1]: https://github.com/frames-sg/wsi-rs/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/frames-sg/wsi-rs/compare/v0.1.5...v0.3.0
[0.1.5]: https://github.com/frames-sg/wsi-rs/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/frames-sg/wsi-rs/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/frames-sg/wsi-rs/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/frames-sg/wsi-rs/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/frames-sg/wsi-rs/releases/tag/v0.1.1
