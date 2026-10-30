<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# Changelog

## [Unreleased]

### Added

- Opens TIFFs stored in strips when the largest image is 8-bit grayscale or
  RGB, uncompressed or compressed with LZW, Deflate, Zstd or JPEG, including
  YCbCr JPEG and the horizontal predictor. Before, only a single-image
  uncompressed 8-bit RGB TIFF opened. A tile read decodes only the strips it
  covers, so images over the 128 MiB decoded-image limit open; each compressed
  strip must fit it. Other images in the file become associated images.
  16-bit, palette, CMYK and alpha strip images still return an error, and a
  strip-only `.vsi` file without its companion folder is not opened as a
  plain TIFF.
- `Slide::tile_location` and `Slide::level_tile_locations` report where tiles'
  encoded bytes are stored: the file, byte ranges, framing (`Standalone`,
  `JpegTables`, `BarePayload` or `Prefixed`) and the codec parameters a decoder
  outside wsi-rs needs, or that a tile is `Absent` or `NotAddressable` and why.
  `EncodedUnit::assemble` turns the bytes at those ranges into a standalone
  stream; for a unit that covers its whole tile it equals what
  `read_raw_compressed_tile` returns, except for bare payloads that raw reads
  return as reordered or decoded samples. A unit a tile is cut from assembles
  to the whole stored image. Every range is checked against the file size,
  and a level listing stays within the tile-index limit. Every slide format
  reports locations; `.svcache` files, which hold wsi-rs's own decoded tiles,
  do not. ([#27](https://github.com/frames-sg/wsi-rs/issues/27))
- `read_raw_compressed_tile` covers tiles it used to reject. A tile cut from a
  larger stored JPEG (Ventana subtiles, NDPI levels without restart markers,
  MIRAX and CZI crops, strip-based TIFFs, ZVI windows) is cropped in the
  coefficient domain, as `jpegtran -crop` does: quantized coefficients are kept
  and only the entropy coding is redone. When the tile origin is not on the
  JPEG's MCU grid, the crop starts at the previous MCU boundary and the new
  `RawCompressedTile::tile_region` locates the tile. A crop holds the whole
  source unit's DCT coefficients, about 12 bytes per pixel for a 4:4:4 JPEG,
  and its working set must fit the new `SlideLimits::lossless_crop_bytes`
  (256 MiB by default). A crop that does not fit returns the whole stored
  JPEG unchanged, made standalone as `tile_location` describes it, with
  `tile_region` locating the tile, as JPEG 2000 and JPEG XR crops do;
  `jpegtran -crop` can cut it. So does a crop whose output would exceed the
  encoded-unit limit. Apart from the usual admission limits, only a stored
  JPEG over the encoded-unit limit, or a tile spanning several JPEG strips
  whose crop does not fit, returns `WsiError::ResourceLimit`. TIFF LZW,
  Deflate, Zstd, uncompressed and JPEG XR tiles, DICOM native and RLE frames, MIRAX PNG and
  BMP records, CZI subblocks, Hamamatsu VMU and Zeiss ZVI planes now pass
  through. Tiles of lossless codecs that cannot be returned as stored come
  back as exact uncompressed samples.
- `SlideLimits::lossless_crop_bytes` and `with_lossless_crop_bytes` set the
  working set of a lossless JPEG crop in a raw tile read, 256 MiB by default,
  so 4096x4096 4:4:4 sources and large NDPI levels crop. Raw reads that may
  crop reserve it as per-operation transient work, so it must stay within
  `operation_transient_bytes` (384 MiB); decoded reads and other raw reads
  do not reserve it. `encoded_unit_bytes` still caps every stored unit.
  Custom `SlideReader`s admitted at the encoded-unit limit can ask for more
  for a raw read with `SlideReader::raw_compressed_tile_reservation_hint`.
- Python: `Slide.tile_location`, `Slide.level_tile_locations` and
  `CompressedTile.tile_region`, with a kerchunk-style reference example in the
  Python README. The `tile_locations` example checks reported ranges against
  `read_raw_compressed_tile`.
- `Compression::Png`, `Compression::Bmp` and
  `EncodedTilePhotometricInterpretation::Monochrome1`.
- Python wheels: publishing a GitHub release builds abi3 wheels for Python
  3.10 and later (Linux x86_64 and aarch64 on glibc 2.28 or newer, macOS
  arm64 and x86_64, Windows x64) and an sdist. Each wheel is tested, then all
  of them are attached to the release and published to PyPI as `wsi-rs`
  through trusted publishing, so `pip install wsi-rs` works. Pull requests
  that touch the bindings build and test the same wheels.

### Changed

- Raw DICOM `MONOCHROME1` tiles and raw grayscale TIFF JPEG tiles tagged
  MinIsWhite report `EncodedTilePhotometricInterpretation::Monochrome1`
  instead of `Monochrome2`.
- Raw TIFF JPEG and JPEG 2000 edge tiles report the size of the encoded
  image, which usually keeps the whole stored tile, with `tile_region`
  marking the clipped tile in it. They used to report the clipped size, which
  did not match the image the bytes decode to. A JPEG or codestream smaller
  than its tile returns an error.
- Raw Hamamatsu VMS tiles carry a JPEG header with the restart interval
  cleared, the same header `tile_location` reports. Only that header field
  changes; decoded pixels are unchanged.
- J2K dependencies follow the J2K `main` branch instead of the 0.8.1
  maintenance revision. Main carries the CUDA packet-header fix along with
  later decoder fixes and speedups.
- JXR follows its `main` branch, which builds on J2K `main`, so wsi-rs compiles
  one J2K instead of also J2K 0.12 for JXR. It requires JXR 0.4, the release
  line built on J2K 0.13.
- Builds take the newest J2K and JXR `main` commits: `cargo latest` updates
  them, and every CI job but packaging runs it before building; packaging
  checks the committed `Cargo.lock`. cargo-vet treats J2K and JXR
  as first-party code, reviewed in their own repositories, instead of
  requiring an audit for each revision.
- The J2K dependencies require 0.13.1, so a published wsi-rs resolves at
  least J2K 0.13.1, with its CPU and Metal batch region fixes, instead of
  accepting 0.13.0.
- Raw `.j2k` and `.j2c` levels with a side over 4096 pixels are read as
  1024 x 1024 tiles, each a region decode, instead of one tile covering the
  level, so a read decodes only the tiles it covers. A tile batch decodes its
  tiles serially side by side, where J2K decodes Tier-1 in bands as the inverse
  DWT reads them. Smaller levels still decode whole. Metal decodes tile batches
  at reductions up to 3 as one batch of regions sharing the codestream's bytes;
  deeper levels decode on the CPU. CUDA decoding is offered only when the full
  resolution is one tile. A raw read of a full-resolution tile returns the
  whole codestream with `tile_region` locating the tile; reduced resolutions
  have no raw read.

### Fixed

- NDPI DCT retiling returns an error for an AC coefficient outside the
  8-bit baseline range, which it encoded wrongly, and for corrupt block grids,
  where it copied blocks wrapped from the next row.
- Merging TIFF `JPEGTables` into a tile shorter than its SOI marker returns
  an error instead of panicking.
- Raw `.j2k` and `.j2c` files whose decoded image exceeds 128 MiB, about 44.7
  megapixels of RGB, open. The decoded-image limit now bounds each decoded
  tile; it rejected the whole image at open. Reduced levels of such files
  decode, where they failed against the full-resolution size.
- A tile read from an uncompressed strip-based RGB TIFF reads only the rows it
  covers. It read the whole image, and read it again for every tile when the
  image was larger than the format cache.
- Some complete LZW-compressed TIFF tiles and strips failed to decode as
  truncated: J2K's LZW decoder stopped when the input ran out, before reading
  the end code in its last bytes. J2K `main` reads to the end code.

## [0.8.1] - 2026-10-27

This is a GitHub source release. The fixed J2K dependency is pinned to a Git
commit; 0.8.1 is not published on crates.io.

### Added

- Experimental Python reader bindings expose slide metadata, tile coverage,
  and ordered compressed-tile batches with JPEG color hints. Reads release
  the GIL; each DataLoader worker opens its own slide. Pixel decoding and
  tensor output remain in the separate `j2k` extension.
- Reads single-file OME-TIFF. Pyramid levels come from each plane's SubIFDs;
  channels, Z planes and time points follow the OME-XML `TiffData` mapping, and
  `label`, `macro` and `thumbnail` images become associated images. Multi-file
  datasets return an error.
- Opt-in cooperative CPU and Metal JPEG 2000 batch decoding through
  `MetalBackendSessions::with_cooperative_cpu_workers`. Cloned sessions share
  one cooperative slot and fall back to ordinary Metal while it is busy.
  Automatic slide acceleration retains its existing policy.
- `Slide::level_encoding` and `Slide::associated_encoding` report how source
  pixels are stored: the codec, decoded and stored sizes, and
  `SourceEncoding::lossy` when the transfer syntax or format establishes it.
  DICOM reports each associated image and each level, summed over the level's
  files when they share codec and lossiness. Both return an error when the
  source sizes cannot be read.
- `Slide::tile_is_present` reports whether metadata marks a tile as acquired,
  without decoding it: tiled TIFF (a zero byte count is absent), Zeiss CZI and
  sparsely tiled DICOM answer; other formats return `None`, to be treated as
  present.
- DICOM reads images stored in 16-bit words with 9 to 16 bits stored, in the
  native, RLE, lossless JPEG, JPEG 2000 and HTJ2K transfer syntaxes, as `Uint16`
  samples. RLE sources report `Compression::Rle`.
- DICOM slides list the files behind each level and associated image in the
  `dicom.level.<n>.source-files` and `dicom.associated.<name>.source-file`
  properties.

### Changed

- Uses J2K 0.13.0 with its CUDA packet-header race fix backported and pinned
  to an exact Git revision. This includes the TIFF LZW decoder and updated
  JPEG integrity checks. Direct Zstandard use moves to 0.14.3.
- Removes the external CZI test oracle and its dependency. CZI parsing and
  corpus sampling use the native reader.
- Uses JXR 0.3.1, whose faster CPU entropy decoding and direct coefficient
  storage speed up JPEG XR tiles in Zeiss CZI slides and tiled TIFF files.
  Decoded pixels are unchanged.
- TIFF files have their SubIFDs parsed when opened. A TIFF whose SubIFDs tag
  points at an IFD that was already parsed, or outside the file, fails to open.
- `CpuTile::to_rgba`, and so `Slide::read_region_rgba` on uncompressed YCbCr
  TIFF tiles, converts YCbCr with the lookup tables that OpenSlide and the
  JPEG 2000 path use instead of floating-point math. About 0.08% of YCbCr
  values change by 1 in one channel, to match OpenSlide.
- Region reads such as `Slide::read_region` report `PlaneOutOfRange` with the
  last valid plane index as `max`, as MIRAX, VSI and ZVI tile reads do,
  instead of the axis count.

### Fixed

- JPEG XR tiles decode inside the codec thread pool. A caller holding slide
  admission no longer waits on global Rayon workers blocked on the same slide.
- TIFF files whose level areas exceed 64 bits no longer panic in debug builds
  or order pyramid levels wrongly.
- Generic tiled TIFF files read each level's own JPEG tables, falling back to
  the first level's, instead of using the first level's tables for every level.
- DICOM files with an SV or UV element before Pixel Data are indexed by the
  fast frame scanner instead of falling back to the slower parser.
- JPEG tiles decoded with forced dimensions patch the frame header itself, not
  `FF Cx` bytes inside an earlier APPn, DQT or COM segment.
- MIRAX data files and the VMS macro image must still be the files the slide
  was opened on. A read after one is replaced fails instead of reading the new
  file, and `Slide::associated_encoding` for a replaced VMS macro fails instead
  of reporting the new file's size.
- `.svcache` errors about a corrupt cache name the cache file, not the source
  slide or an empty path, and a tile payload that cannot be read reports
  `IoWithPath` with the cache file instead of a bare `Io` error, as does
  probing a cache file that cannot be opened. A bad scene or
  series in an `.svcache` read or a partial build reports `SceneOutOfRange` or
  `SeriesOutOfRange` with the real count, not `LevelOutOfRange` with a count
  of 0.
- Partial `.svcache` builds that merge an existing cache copy nothing from a
  level whose dimensions or tile grid changed, instead of copying its tiles to
  the wrong positions.
- DICOM tile reads, raw tile reads and `Slide::tile_is_present` reject a
  scene, series or plane the slide doesn't have with `SceneOutOfRange`,
  `SeriesOutOfRange` or `PlaneOutOfRange`, instead of answering for the first
  one. `Slide::tile_codec_kind` reports `TileCodecKind::Other` for them.
- Zeiss CZI tile reads report a series the slide doesn't have as
  `SeriesOutOfRange` with the real count, instead of `SceneOutOfRange`.
- Opening a VMU slide whose macro image size cannot be read reports
  `IoWithPath` with the macro file instead of a bare `Io` error.
- XML metadata parsing applies its nesting-depth limit to self-closing
  elements too, not only to elements with content.
- CUDA JPEG 2000 batches size their decode windows at 4 bytes per pixel, like
  Metal, so a window no longer admits about a third more pixels than its
  4 MiB target.
- TIFF associated images (Aperio, Huron, ARGOS, Leica) whose SamplesPerPixel
  tag is 0 or above 65535 report a channel count clamped to 1–65535 instead of
  a wrapped value such as 0.
- A missing tile in a TIFF slide no longer allocates an unbounded blank tile;
  blank tiles larger than the decoded-image limit fail with an error.
- `Slide::open_with_options` with `SvcachePolicy::PreferFresh` and a custom
  registry (`SlideOpenOptions::with_registry`) that has no `.svcache` backend
  opens the source slide instead of redirecting to a `.svcache` file and
  failing.
- Hamamatsu VMS errors for a level with no JPEG shards, and Olympus VSI errors
  for negative or missing ETS tile coordinates, name the slide file instead of
  an empty path.
- I/O errors while opening or reading the header of a `.svcache` file name the
  cache file.

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
- MIRAX source batches decode on the caller with help from the shared CPU
  pool. JPEG tiles with known dimensions use J2K's direct output API and
  reusable decode scratch, including TIFF tiles whose color hints match the
  JPEG header. Larger MIRAX reads use wider bounded row bands to reduce
  repeated tile planning.
- The first GPU calibration read no longer includes Metal kernel compilation,
  which could make a decode route settle on the CPU.

### Security

- Tiny raw JPEG 2000 files with impossible tile grids are rejected before
  decoding can spend excessive time processing absent tiles.
- Malformed DICOM file-meta headers using `SV` or `UV` no longer bypass length
  checks and trigger oversized allocations.

## 0.7.0 (unpublished) - 2026-09-28

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

## [0.5.0] - 2026-07-15

### Changed

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

## [0.4.0] - 2026-06-20

- First release under the `wsi-rs` name. Earlier versions were published as
  `statumen` and are now yanked.
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

[Unreleased]: https://github.com/frames-sg/wsi-rs/compare/v0.8.1...HEAD
[0.8.1]: https://github.com/frames-sg/wsi-rs/compare/v0.8.0...v0.8.1
[0.8.0]: https://github.com/frames-sg/wsi-rs/compare/v0.6.0...v0.8.0
[0.6.0]: https://github.com/frames-sg/wsi-rs/compare/v0.5.2...v0.6.0
[0.5.2]: https://github.com/frames-sg/wsi-rs/compare/v0.5.1...v0.5.2
[0.5.1]: https://github.com/frames-sg/wsi-rs/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/frames-sg/wsi-rs/compare/398dd681fc7c27b7020d9b35ae066c9ecbf127f2...v0.5.0
[0.4.0]: https://github.com/frames-sg/wsi-rs/tree/398dd681fc7c27b7020d9b35ae066c9ecbf127f2
[0.3.1]: https://docs.rs/statumen/0.3.1/statumen/
[0.3.0]: https://docs.rs/statumen/0.3.0/statumen/
[0.1.5]: https://docs.rs/statumen/0.1.5/statumen/
[0.1.4]: https://docs.rs/statumen/0.1.4/statumen/
[0.1.3]: https://docs.rs/statumen/0.1.3/statumen/
[0.1.2]: https://docs.rs/statumen/0.1.2/statumen/
[0.1.1]: https://docs.rs/statumen/0.1.1/statumen/
