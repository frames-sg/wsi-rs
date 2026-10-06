<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# Changelog

## [Unreleased]

## [0.8.0] - 2026-10-06

### Added

- `MetalBackendSessions::prewarm` compiles a session's JPEG 2000 GPU kernels
  ahead of time, so the first slide read doesn't wait for them.

### Changed

- Requires Rust 1.99 or newer.
- Uses J2K 0.12.0 and JXR 0.3.0, including the updated CUDA and Metal batch
  decode paths.
- CUDA JPEG 2000 tile reads use bounded encoded-image batches for parallel
  planning and pooled uploads. Results keep their request order and logical
  crop; a recoverable tile failure does not discard neighboring results.
- Metal JPEG 2000 batches share Tier-1 dispatch across compatible prepared
  groups while retaining the existing 4 MiB execution window.
- `.svcache` files are looked up in `~/.cache/wsi-rs/svcache/` instead of
  `~/.cache/slideviewer/svcache/`. Move existing cache files there or rebuild
  them.
- The OpenSlide shim opens slides with a 32 MiB tile cache, matching OpenSlide
  4.0.1, and no display cache; format-specific caches scale down with it.
- The OpenSlide shim composes small MIRAX reads in one pass and writes cached
  regular-grid regions directly into the caller's pixel buffer.
- Faster XML metadata parsing when opening ARGOS, Leica, Philips and Ventana
  slides.
- With the `route-telemetry` feature, route tile counts are reported only in
  the per-device sections; the `execution` section no longer repeats them.

### Fixed

- Concurrent single-tile reads of one DICOM slide no longer queue behind each
  other under the default memory limits, and Metal decodes DICOM frames in
  groups instead of one at a time.
- DICOM slides that store progressive JPEG frames under the baseline JPEG
  transfer syntax, as some 3DHISTECH scanners do, now read in full.
- Multi-tile JPEG reads also use CPU cores that free up while the read is
  running, so concurrent reads finish sooner.
- CPU batch helpers yield cores when new callers need them. Concurrent tiled
  TIFF JPEG reads avoid adding caller threads to a pool that already uses
  every CPU core.
- MIRAX JPEG reads reserve memory from indexed source sizes, so small tiles
  can share a bounded batch instead of each reserving the maximum input size.
- The first GPU calibration read no longer includes Metal kernel compilation,
  which could make a decode route settle on the CPU.

### Security

- Tiny raw JPEG 2000 files with impossible tile grids are rejected before
  decoding can spend excessive time processing absent tiles.
- Malformed DICOM file-meta headers using `SV` or `UV` no longer bypass length
  checks and trigger oversized allocations.

## [0.7.0] - 2026-09-28

### Added

- 12-bit JPEG DICOM slides (JPEG Extended and progressive JPEG). Reads return
  16-bit RGB samples holding the 12-bit values; grayscale expands to RGB.
  12-bit decoding runs on the CPU. The OpenSlide shim rejects 12-bit DICOM, as
  OpenSlide does.
- Hamamatsu VMU slides, with full 16-bit samples, metadata and macro images.
  The OpenSlide shim returns them as 8-bit RGB, as OpenSlide does.
- ARGOS and Huron TIFF slides, including ARGOS sparse tiles and z-planes, and
  associated images for both.
- Zeiss CZI slides: single-plane brightfield scans stored uncompressed, as JPEG
  or as JPEG XR. 16-bit embedded preview images keep all 16 bits.
- JPEG XR tiles in tiled TIFF files.
- DICOM JPEG Extended, progressive JPEG and lossless JPEG transfer syntaxes.
- Raw JPEG 2000 codestreams (`.j2k`, `.j2c`) expose each resolution as a
  pyramid level, matching OpenJPEG's reduced-resolution decodes exactly.
- Raw compressed JPEG tile access for CZI, MIRAX, full-resolution VMS and
  ordinary tiled TIFF.
- Olympus VSI raw JPEG 2000 tile access and GPU-resident Metal and CUDA tile
  reads.
- Per-slide resource limits (`SlideLimits`).
- ICC profiles for associated images, also through the OpenSlide shim.
- GPU-resident JPEG 2000 and HTJ2K tile reads on Metal and CUDA
  (`read_tile_metal`, `read_tiles_metal`, `read_tile_cuda`, `read_tiles_cuda`).

### Changed

- All normal reads (tiles, batches, regions, display tiles and associated
  images) return `CpuTile`. Custom `SlideReader` implementations provide one CPU
  tile method, and batch reads keep request order by default.
- `DecodeAcceleration::{Auto, CpuOnly}` replaces the output-routing and sampling
  controls. `Auto` uses the GPU for JPEG 2000 only when it measures at least 15%
  faster, including copying the result back, and falls back to the CPU on any
  GPU error.
- The GPU is timed on a background thread instead of during the read, except
  for batches too large to keep after the read. The CPU is timed on the read
  that needs the tiles. If GPU startup is much slower than CPU decoding, that
  kind of tile stays on the CPU.
- All JPEG 2000 CPU decoding shares one thread pool for the process.
  Per-slide thread-pool settings are gone.
- Default caches are 64 MiB for decoded tiles, 32 MiB for display tiles and
  32 MiB shared by format-specific caches. Older per-cache settings are scaled
  to fit that total.
- Every built-in format counts the metadata and indexes it parses against the
  slide's limits. Custom readers are trusted while opening, and their reads
  reserve memory conservatively.
- Ventana reduced levels are slower than in 0.6 because they are now stitched
  correctly (see Fixed). They are still faster than OpenSlide.
- On Apple Silicon Macs, slide fingerprints use the CPU's SHA-256 instructions.

### Removed

- `TileOutputPreference`, `DeviceOutputContext`, `OutputBackendRequest`,
  `TilePixels`, `DeviceTile`, the public route-decision types, the route-sample
  setting and GPU decoding of ordinary JPEG. Use `DecodeAcceleration` and the
  GPU-resident tile APIs instead.
- `SlideReader::recommended_shared_cache_bytes`. Cache sizes are set per slide.

### Fixed

Pixels that differed from OpenSlide:

- NDPI and VMS levels that OpenSlide derives by JPEG scaling (1/2, 1/4, 1/8)
  now match OpenSlide 4.0.1 exactly. They differed by up to 3 levels, or up to
  about 160 for slides with subsampled color.
- Ventana BIF reduced levels are built from the stitched full-resolution tile
  map, as OpenSlide does. They used the unstitched overview images, which put
  tissue 36 to 60 µm out of place and showed blank filler.
- Overlapping tiles in Ventana level 0 and MIRAX are painted in OpenSlide's
  order and arithmetic, so overlaps look the same as in OpenSlide.
- MIRAX levels whose images split into fractional sub-tiles are resampled at
  their exact offsets, and RGB sources use Pixman's rounding, as in OpenSlide.
- The OpenSlide shim keeps partly covered edge pixels on Ventana and MIRAX
  reduced levels.
- Metal YCbCr-to-RGB conversion matches the CPU exactly; non-neutral colors
  were off.
- Sparse Philips and generic TIFF tiles stay transparent in single and batched
  reads.
- DICOM honors RGB and YBR color-transform metadata.
- Invalid levels, missing associated images, the shim's error state,
  zero-length ICC reads, Leica barcodes and slide bounds behave as in OpenSlide.
- Raw JPEG 2000 slides report `openslide.vendor` and `openslide.quickhash-1`
  like other formats.
- J2K 0.11.3 fixes JPEG 2000 decoding of subsampled images whose origin is not
  zero.

Crashes, hangs and bad files:

- Fixed a deadlock when several readers read display tiles from the same
  generated NDPI level.
- Concurrent MIRAX reads no longer interfere with each other.
- Generated NDPI levels use virtual tiles, so zooming and fractional regions
  don't decode an entire level.
- Short TIFF payloads, malformed CZI payload lengths and invalid CZI geometry
  are rejected before decoding.
- Aperio, Ventana, DICOM, NDPI and MIRAX reject bad geometry, bad sparse data,
  out-of-range offsets and non-finite physical metadata.
- Corrupt JPEG 2000 codestreams fail immediately instead of retrying, and no
  longer build excessive coding trees (J2K 0.11.3).
- Corrupt MIRAX slides are recognized as MIRAX, so opening reports the real
  error.
- CZI keeps planes separate, respects resource limits and keeps mosaic overlap
  order. Requests for missing native levels return errors.
- Generic TIFF and Philips associated images use each image's own JPEG tables
  and decode one strip at a time.
- VMS reads keep complete index records when the file ends with an incomplete
  row.
- The shim installer no longer creates incorrect `libopenslide.4` aliases.

Speed and memory:

- Multi-tile JPEG reads from tiled TIFF slides (SVS, Leica and similar) decode
  in parallel again.
- NDPI region reads no longer queue behind other readers' work.
- MIRAX handles opened on the same files share one parsed index.
- OpenSlide-compatible region reads use row bands to cap memory, compose cached
  regions straight into the output, and skip empty areas of sparse slides.
- Ventana reduced levels decode each stored tile once per batch and no longer
  re-decode tiles while panning.
- Aperio reduced-level, zoom and thumbnail reads are back to 0.6 speed.
- Cached region reads from MIRAX, VMS, CZI and ZVI are faster with many
  concurrent readers.
- CZI reuses decoded source blocks across neighboring output tiles.
- Region reads stream large requests in batches sized to the memory budget.

## [0.6.0] - 2026-08-25

### Changed

- The Metal backend uses `objc2-metal` (through J2K 0.10.0) instead of
  `metal-rs`. Create sessions with `MetalBackendSessions::system_default`.
  Raw-buffer adoption takes a `MetalBuffer`, and the unsynchronized
  `MetalDeviceStorage::Buffer` variant is removed.
- `CpuTile::pixels_arc` returns `Option<Arc<Vec<u8>>>` and no longer copies
  pixels. Store an `Arc<Vec<u8>>` and call `as_slice()` where you need a byte
  slice.
- The OpenSlide shim reports its version as `OpenSlide 4.0.1+wsi-rs-0.6.0`.

## [0.5.2] - 2026-07-31

### Added

- `CudaDeviceTile::download_cpu` copies a CUDA tile to CPU memory.
- OpenSlide shim caches are sized in bytes and can be shared. A slide keeps its
  cache alive after the C cache handle is released.
- `Slide::prepare_level_controlled` builds a DICOM frame index ahead of time,
  can be cancelled, and is reused by every later read.
- Optional diagnostics for DICOM frame indexing (method, fallback, reuse and
  timing). They cost nothing when off.

### Changed

- Requires the `j2k` 0.8 codec crates.
- DICOM, MIRAX, VMS and Zeiss caches share one budget from `CacheConfig`.
- Opening a slide applies your cache settings during format detection and
  reuses that parse.
- Picking the best level for a downsample matches OpenSlide at exact level
  boundaries and for non-finite requests.
- Controlled tile reads keep request order, use the same CPU/GPU choice as
  other reads, and stop for good once cancelled.

### Fixed

- Oversized DICOM frames split across fragments are rejected before
  allocation. Truncated MIRAX files fail fingerprinting instead of hashing
  partial data.
- Zeiss attachments are extracted to securely created temporary files instead
  of predictable paths.
- When a shim install fails and putting the original files back also fails,
  the backups are kept and both errors are reported
  (`execute_install_detailed`).
- DICOM frames are located through the file's offset tables and checked
  against the file size, instead of scanning all pixel data. Unusual layouts
  fall back to scanning.
- Fixed races between cancellation and the CPU/GPU choice; partial DICOM
  indexes are never cached.
- TIFF edge tiles have the right size in CPU and Metal JPEG and JPEG 2000
  output.

## [0.5.1] - 2026-07-17

### Added

- Cancellation tokens (`ReadCancellationToken`) and controlled tile reads, so a
  viewer can stop reads it no longer needs.

### Fixed

- Metal JPEG and JPEG 2000 edge tiles are cropped to their real size, matching
  CPU reads, instead of repeating padding pixels at the right and bottom edges.
- Compressed tile sizes come from the TIFF tile size, not the codec's padded
  size.
- Reads check for cancellation around tile I/O and decoding.

## [0.5.0] - 2026-07-14

### Changed

- The crate is renamed from `statumen` to `wsi-rs`.
- Requires the `j2k` 0.7.2 codec crates; the old `signinum-*` aliases are gone.
- Metal outputs keep their GPU memory alive through `ResidentMetalImage`.
  Raw-buffer storage whose lifetime can't be checked is rejected.
- `.svcache` files use schema 3, which checks the source file's identity and a
  sampled content hash, not just its size and modification time. Rebuild
  schema 2 cache files.

### Fixed

- Metal YCbCr conversion works for images larger than 4 GiB.
- Stricter parser limits. Companion file names in slide metadata can't point
  outside the slide's files. The shim installer rolls back cleanly when it
  fails.

## [0.4.0] - 2026-05-27

- Public constructors and request builders check their inputs.

## [0.3.1] - 2026-05-26

- Requires the `j2k` 0.4.4 codec crates.

## [0.3.0] - 2026-05-12

- Uses the `j2k` 0.4 codec crates. The repository moved to `frames-sg/wsi-rs`.

## [0.1.5] - 2026-05-06

- Requires `j2k-jpeg-metal` 0.2.2.

## [0.1.4] - 2026-05-06

- Added a tile output preference that requires compressed device tiles.

## [0.1.3] - 2026-05-05

- Clearer errors for malformed NDPI files.

## [0.1.2] - 2026-05-05

- Raw compressed JPEG tile access and batched NDPI tile decoding on Metal.
- JPEG 2000 decoding goes through the `j2k` crate.
- Updated `lru` to fix `RUSTSEC-2026-0002`.

## [0.1.1]

- First public release.

[Unreleased]: https://github.com/frames-sg/wsi-rs/compare/v0.8.0...HEAD
[0.8.0]: https://github.com/frames-sg/wsi-rs/compare/v0.7.0...v0.8.0
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
