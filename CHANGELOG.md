<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# Changelog

## [Unreleased]

### Added

- `MetalBackendSessions::prewarm` builds a session's JPEG 2000 decode kernels
  before the first read, so applications can compile shaders off the first
  slide's critical path.

### Fixed

- DICOM reads reserve each frame's indexed encoded length instead of the whole
  per-unit encoded limit. Concurrent single-tile reads of one slide no longer
  queue three at a time under default limits, and Metal reads keep their grouped
  batches instead of falling back to one submission per frame. Frames read
  before the lazy frame index exists keep the previous bound.
- DICOM lossy JPEG frames decode by their own DCT process when their precision
  matches BitsStored. 3DHISTECH slides that store progressive frames under the
  JPEG Baseline transfer syntax now read in full. Lossless syntaxes still require
  lossless frames.
- Shared CPU work recruits cores that become idle while a batch runs, not only
  those idle when it starts. Concurrent multi-tile JPEG reads no longer finish
  later batches on their calling thread alone.

## [0.7.0] - 2026-09-28

### Added

- DICOM VL WSI with 12-bit JPEG frames (BitsAllocated 16, BitsStored 12,
  HighBit 11) under the JPEG Extended, spectral-selection and full-progression
  transfer syntaxes. Native reads return interleaved RGB `Uint16` tiles with the
  12-bit samples unchanged, and MONOCHROME2 expands to R=G=B as for 8-bit.
  12-bit associated images report `Uint16`. Raw frame passthrough reports 16 bits
  allocated. Each frame's SOF precision must match BitsStored, and pyramid levels
  must share one bit depth. 12-bit decoding stays on the CPU. Other transfer
  syntaxes still require 8-bit samples, and the OpenSlide shim still rejects
  12-bit DICOM with its previous error.

- Hamamatsu VMU base/map NGR reading with native RGB16 samples, bounded positional
  tile reads, metadata, and JPEG macro images. The OpenSlide shim converts RGB12
  before region composition, including fractional reads. Validation uses synthetic
  fixtures and independent OpenSlide comparisons; real scanner VMU validation remains pending.
- Raw `.j2k`/`.j2c` codestreams expose their wavelet resolution levels as pyramid
  levels, decoded from discarded resolutions without resampling and matching
  OpenJPEG reduced decodes exactly.
- Olympus VSI supports raw JP2K tile passthrough and strict Metal/CUDA tile reads
  for stored ETS tiles.

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

### Fixed

- OpenSlide-compatible region reads bound intermediate color and coverage
  buffers with row bands where splitting preserves source coverage and Pixman
  filtering. Cached regions retain dense composition, opaque
  bands avoid redundant alpha reconstruction, and singleton staging avoids
  initializing an unused JP2K worker pool.
- Tiled TIFF JPEG reads (SVS, Leica and similar) decode a read's tiles in
  parallel again when cores are idle. The calling thread decodes tiles in
  order while otherwise idle cores help, and it never waits on queued work.
  Streamed batches of tiles larger than the region widen only when the read's
  admission grants optional staging memory. A single viewer no longer decodes
  the tiles of a multi-tile read one after another.
- MIRAX image records share data-file paths, retain compact source origins,
  and discard excess descriptor capacity. Tile-map extents and surface flags
  share one compact representation.
- MIRAX handles opened on the same unchanged files under the same resource
  limits share one parsed index instead of each walking and retaining it.
  Caches and open files stay per handle, and a changed Slidedat.ini, index or
  data file, or any difference in limits, parses a separate index.
- Automatic JP2K routing stays on CPU when device warmup takes more than four
  uncached CPU decodes, avoiding repeated costly foreground calibration probes.
  Clipped tiles reuse that strong CPU preference for the same level, codec,
  batch count, device and worker budget.
- JP2K route calibration no longer delays reads with device work. The read
  that needs the tiles times their uncached CPU decode, and one background
  thread per runtime times the device decode, including one-time Metal setup
  and kernel compilation. Batches too large to retain after their read keep
  the foreground comparison.
- MIRAX composition uses Pixman's integer interpolation for fully covered
  RGB24 source clips, preserving the distinct rounding of ARGB32 surfaces.
  RGB and RGBA tiles compose directly into one premultiplied RGBA image,
  avoiding full-size color and coverage copies while preserving fractional
  filtering, gaps, partial coverage and overlap order. Fractional tiles paint
  through bounded multi-row bands that keep vectorized bilinear sampling
  instead of one row at a time.
- OpenSlide-compatible reads of cached, opaque, densely tiled irregular
  regions, such as revisited MIRAX level 0 views, compose straight into the
  caller's premultiplied ARGB pixels. They no longer build banded intermediate
  images or run a separate conversion pass.
- Ventana reduced levels decode each stored tile once per tile batch. TIFF
  layouts without NDPI sources give stored-tile decodes the complete private
  cache budget instead of reserving shares for unused NDPI caches, so reduced
  levels no longer re-decode stored tiles while panning.
- VMS reads retain complete optimization records when the file ends with an
  incomplete row, and reuse restart markers already found in a scan chunk.
  Missing offsets still fall back to scanning the JPEG.
- Sparse OpenSlide-compatible reads avoid image construction in empty tile-map
  regions. Integral composition copies uncovered rows in bulk and converts
  fully opaque output without per-pixel alpha arithmetic.
- Fixes a deadlock when concurrent NDPI display-tile reads share a generated
  level. The decode that other readers wait on processes strip bands on its
  calling thread.
- NDPI region reads no longer fan their strips out to the shared worker
  pool. An incomplete level-0 region decodes its strips in order on one
  worker, and generated levels decode scaled strips on the calling thread.
  Concurrent readers no longer queue behind each other's strip work, which
  had set their tail latency.
- Updates J2K to 0.11.3 to fix subsampled nonzero-origin decoding and excessive
  tag-tree construction found by the raw-codestream fuzz target, and JXR to
  0.2.1 for the current codec release set.
- Raw JP2K resolution probing returns corrupt-codestream errors immediately,
  while still trying shallower reductions when component coding styles require
  them. The truncated tall-tile reproducer is retained in the fuzz corpus.

- Raw JP2K datasets publish `openslide.vendor` and `openslide.quickhash-1` like other
  formats, with unchanged dataset IDs, instead of relying on the OpenSlide shim.

- Progressive JPEG DICOM metadata accepts the supported retired spectral-selection and full-progression transfer syntaxes, retaining frame process validation.

- Generated NDPI pyramid levels expose bounded virtual tiles and crop native reads to that grid, allowing zoom and fractional-region reads without materializing an entire generated level.

- Sparse Philips and generic TIFF tiles preserve transparent holes in both single and batched JPEG reads.

- MIRAX concurrent reads use positional source access instead of cloned file
  cursors, preventing reads from interfering with one another.
- Metal YCbCr conversion uses the canonical CPU lookup tables, correcting
  non-neutral colors and clipping to match CPU/OpenSlide-compatible RGB8 output.

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
- Restored 0.6 fractional-composition speed for Aperio reduced-level, zoom and
  thumbnail reads by selecting the sampling mode once per blit instead of mapping
  optional taps per channel. Output pixels and alpha are unchanged.
- MIRAX, VMS, CZI and ZVI region reads no longer enter the decode pool only to
  try a region fast path they do not implement, restoring cached-read latency
  under concurrent handles.
- Irregular tile maps (Ventana level 0, MIRAX) paint overlapping tiles in
  OpenSlide's bottom-right-first order with its translation and saturation
  arithmetic, so overlapping areas keep the lower tile like OpenSlide.
- Ventana BIF reduced levels are painted from the stitched level-0 tilemap, one
  stored-tile subtile per level-0 cell, as OpenSlide does. They previously showed
  the unstitched overview directories, placing tissue 36–60 µm off level 0 and
  exposing blank filler. Fractional subtiles use OpenSlide's intermediate
  surface, and reduced-level reads match OpenSlide 4.0.1 within one level.
- MIRAX levels whose images split into fractional subtiles (for example 42.5
  pixels from 340-pixel images) resample each subtile at its exact source offset
  as OpenSlide does, instead of cropping rounded whole-pixel windows.
- The OpenSlide shim keeps partially covered edge pixels of tilemap levels read
  at fractional level origins instead of clearing them from whole-pixel tile
  bounds (Ventana and MIRAX reduced levels).
- NDPI and VMS levels that OpenSlide derives by libjpeg DCT scaling (1/2, 1/4
  and 1/8) now match OpenSlide 4.0.1 exactly; they differed by up to 3 levels.
  J2K's reduced-size IDCTs now round like libjpeg-turbo, and NDPI levels derived
  from restart-marker levels decode each restart interval at reduced scale,
  shared through the tile cache, instead of box-filtering full-resolution pixels.
  J2K 0.11.2 also decodes scaled JPEG like libjpeg-turbo when the JPEG stores
  subsampled color (4:2:0, 4:2:2 and other layouts), so NDPI/VMS slides with
  subsampled chroma match as well; earlier builds differed there by up to about
  160 levels.

### Changed

- Generic region reads reuse one validated plan and decode bounded consecutive
  batches, with exact encoded-size accounting for tiled TIFF. Cached ordinary
  regions avoid empty CPU-worker dispatches. CZI and MIRAX
  batches share bounded source blocks and coalesce concurrent source misses.
- VSI retains ETS handles and submits CPU codec batches; `.svcache` uses
  positional payload reads where supported while preserving checksums.
- VSI JP2K decodes run on the shared JP2K CPU pool and participate in automatic
  device routing.
- Owned CPU YCbCr conversion reuses its allocation, cropped conversion allocates
  only logical output, and direct single-image JP2K execution retains its parsed view.
- Metal JP2K uses bounded native prepared batches, grouped color conversion
  and direct shared-memory readback. Other storage uses bounded staged readback
  through a retained session queue. Strict APIs retain strict Metal fallback.
- Automatic JP2K reads start on CPU, then defer warmup and three alternating
  comparisons to later reads. Decisions use the median ratio for the actual bounded
  execution batch, require a 15% device advantage and skip optional work without memory
  headroom. Calibration advances at most once per public read across its internal
  batches and admission chunks. Only one caller calibrates each route, without
  blocking competitors. The pending route remains owned until the initial CPU
  output is ready, preventing concurrent warmup from racing startup. Cached native
  DICOM batches avoid decoder-worker handoffs and unnecessary selected-device work.
- Dense row-span planning and bounded fractional sampling reuse reduce composition
  work while preserving pixel, overlap and alpha semantics.
- Optional performance diagnostics separate reader-active time from verification
  and retain existing wall-clock throughput and acceptance fields.
- Route telemetry accounts every routed JP2K tile, including warmup reads and
  reads that skip calibration, and reports a failed device warmup as a fallback.
  GPU performance acceptance requires each JP2K benchmark process to measure the
  device route with zero fallback, rather than device-selected tiles in every cell.
- Performance captures cap pre-0.7 shims to one JP2K thread per handle, so
  previous-release baselines keep the equalized decode thread budget.
- Tilemap composition treats straight-alpha RGBA source tiles as coverage under
  OpenSlide's saturating paint, and composes interior pixels of each placed
  tile with constant bilinear weights. Stitched Ventana reduced-level reads
  resample every level-0 cell, so they cost more than 0.6's unstitched reads
  but remain faster than OpenSlide.

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
