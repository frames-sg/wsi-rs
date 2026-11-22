# Architecture

How the code is organized and how a read flows through it. For using the library, see the
[README](../README.md) and the [API documentation](https://docs.rs/wsi-rs). For tests, conformance
runs and release checks, see [Testing](testing.md).

## Repository Layout

- `src/`: the `wsi-rs` library, plus the `svcache` command in `src/bin/`.
- `wsi-rs-openslide-shim/`: the OpenSlide-compatible C library and its installer.
- `wsi-rs-python/`: the Python binding for metadata, tile geometry, compressed tiles and tile
  locations.
- `tests/`: integration tests and fixtures. `tests/repo_policy/` holds repository checks such as the
  unsafe-code policy.
- `test-support/`: helpers shared by tests, including the parity-corpus manifest and the OpenSlide
  oracle.
- `fuzz/`: one fuzz target per parser.
- `perf-runner/`: benchmarks wsi-rs and OpenSlide through the same C API.
- `xtask/`: `cargo xtask` commands for checks, tests, releases and benchmarks.
- `api/`: public API snapshots checked by `cargo xtask api-check`. Refresh them with
  `WSI_RS_UPDATE_PUBLIC_API=1 cargo xtask api-check`.
- `scripts/`: corpus download, fixture generation and release scripts.
- `supply-chain/`: cargo-vet data. See [SUPPLY_CHAIN.md](../SUPPLY_CHAIN.md).

## Library Layout

| Path                                   | Responsibility                                                                                                                                                                                                                 |
| -------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `src/core/registry`                    | `Slide`, format detection, open options and read entry points. `slide/budget.rs` estimates read sizes. `managed.rs` holds `ConservativeManagedReader`, which admits reads for readers without their own encoded-size planning. |
| `src/core/registry/composition`        | Region planning, tile resolution and composition.                                                                                                                                                                              |
| `src/core/cache.rs`, `src/core/cache/` | Byte-bounded tile caches and shared in-flight decodes.                                                                                                                                                                         |
| `src/core/limits.rs`                   | `SlideLimits`, checked size arithmetic and the first-in, first-out admission queue (`SlideAdmission`).                                                                                                                         |
| `src/core/batch.rs`                    | The process-wide CPU thread pool (`process_codec_cpu_pool`, `install_codec_cpu`) and sharing a batch's work between the caller and idle pool threads.                                                                          |
| `src/core/decode_runtime.rs`           | The decode policy and the reader that runs JPEG 2000 batches on the codec pool.                                                                                                                                                |
| `src/core/read_control.rs`             | `ReadControl`: cancellation and diagnostics for controlled reads.                                                                                                                                                              |
| `src/core/file_identity.rs`            | Records a source file's identity and checks it before reads.                                                                                                                                                                   |
| `src/core/types`                       | Public data types: `Dataset`, `Level`, requests, `CpuTile` and tile locations (`locations.rs`). `pixels/ycbcr.rs` converts YCbCr to RGB with OpenSlide 4.0.1's integer lookup tables.                                          |
| `src/formats/<format>`                 | One module per file format: parse metadata, locate and read tiles.                                                                                                                                                             |
| `src/formats/tiff_family`              | Every TIFF-based format. `container` parses TIFF, `layout` maps each vendor's layout to levels (`layout/common.rs` holds shared pieces), `pixel_access` reads tiles.                                                           |
| `src/formats/svcache`                  | The `.svcache` pre-decoded cache: reader, builder and lookup paths.                                                                                                                                                            |
| `src/decode`                           | Adapters to the codec crates: `j2k-jpeg`, `j2k` (JPEG 2000) and `jxr` (JPEG XR). `jpeg/frame.rs` and `jpeg/lossless.rs` hold WSI policy over j2k-jpeg's marker parsing and lossless crop.                                      |
| `src/output`                           | GPU tile types for Metal and CUDA, and copying GPU tiles to CPU memory.                                                                                                                                                        |
| `src/properties.rs`, `src/error.rs`    | `Properties` with OpenSlide property names, and `WsiError`.                                                                                                                                                                    |

## Opening a Slide

`Slide::open` resolves the path through the `.svcache` policy, then hands it to a `FormatRegistry`.
Every registered format checks whether it recognizes the file. A `Definite` match beats a `Likely`
one; among equal matches the format registered first wins. The winning format parses the file's
metadata and builds a `Dataset`.

Parsing draws on an `OpenBudget`, which keeps two running totals across every file in the slide
(MIRAX, VMS and VSI slides are several files): metadata bytes against
`SlideLimits::aggregate_metadata_bytes`, and index bytes against `SlideLimits::tile_index_bytes`.
Every size read from a file goes through checked arithmetic against `SlideLimits` before anything is
allocated. The TIFF-family, DICOM, CZI, ZVI, MIRAX, VMS and `.svcache` readers record each source
file's identity with `core::file_identity` and refuse to read a file that was replaced after
opening.

## Reading a Region

A region read goes through four steps:

1. **Plan.** `RegionReadPlan` validates the request and finds the tiles that cover the rectangle. If
   the rectangle lines up with whole pixels on the level, the read is _integral_. Otherwise it is
   _fractional_ and needs interpolation.
2. **Admit.** `Slide::with_admission` reserves the memory the read needs from the slide's budget.
   Reads that would go over it wait in the `SlideAdmission` queue, first in, first out. Requests too
   large to ever fit are rejected.
3. **Resolve.** `RegionTileResolver` takes tiles from the cache, decodes the missing ones in batches
   and stores the results. When two reads need the same missing tile at once, one decodes it and the
   other waits for the result (`core::cache::flights`).
4. **Compose.** Integral reads copy tile rows into the output. Fractional reads interpolate with the
   same arithmetic and rounding as OpenSlide's renderer (Pixman), so the pixels match OpenSlide.

Planning, resolution and composition live in `core::registry::composition`. Large regions stream
through in batches sized to the memory budget. A format can provide its own region path (NDPI does);
the generic path handles any request it declines.

Tile reads (`read_tile`, `read_tiles`) skip planning and composition: they admit, resolve and return
tiles in request order. Every batch read returns exactly one result per request, in order.

## Raw Tiles and Byte Locations

`read_raw_compressed_tile` returns a tile's encoded bytes and `tile_location` reports where they are
stored (`src/core/types/locations.rs`). Each format implements both from the same index, so for a
stored unit that covers its tile, `EncodedUnit::assemble` over the bytes at its ranges equals the
raw tile; format tests check that round trip. `Slide::level_tile_locations` lists every cell of a
regular grid, or the stored entries of an irregular layout, and counts shared header and table
buffers once against the tile-index limit.

A tile that is part of a stored JPEG is cropped in the coefficient domain. j2k-jpeg does the work
and `src/decode/jpeg/lossless.rs` holds the policy around it:

- `lossless_crop_or_whole_jpeg` crops one stored JPEG with `j2k_jpeg::transcode::lossless_crop`.
  When j2k-jpeg's estimate of the crop's working set exceeds its budget, when the crop's output
  would exceed `encoded_unit_bytes`, or when the crop cannot cut the source (such as a single
  component sampled 2x2), it returns the whole standalone JPEG with `tile_region` locating the tile:
  the same bytes `tile_location` and `EncodedUnit::assemble` give.
- `lossless_crop_jpeg_stack` crops vertically stacked JPEGs such as TIFF strips. It extracts each
  part's coefficient window and re-emits the assembled image through `dct_reemit.rs`, a thin wrapper
  over j2k-jpeg's baseline encoder. With no single JPEG to fall back to, a crop that does not fit
  returns `WsiError::ResourceLimit`. The NDPI display-tile retiler uses the same re-emission.

`Slide::read_raw_compressed_tile` reserves `ManagedSlideReader::raw_tile_encoded_upper_bound`, which
defaults to the decode bound. Formats that crop raise it to the crop working set for crop tiles
only: TIFF (Ventana subtiles, JPEG strips, NDPI level JPEGs) and MIRAX, which caps its computed
reservation at the limit. CZI and ZVI are admitted by `ConservativeManagedReader` at one encoded
unit per read; their `SlideReader::raw_compressed_tile_reservation_hint` returns the crop limit for
crop tiles, and the wrapper reserves the larger of the two for raw reads only. The reservation must
fit `operation_transient_bytes`.

## Caches

Each slide has three caches, all bounded by bytes and evicting the least recently used entry:

- **Decoded tiles**, shared by all reads of the slide (64 MiB by default).
- **Display tiles** for `read_display_tile` (32 MiB).
- **Format caches** for format-specific data such as DICOM frames, NDPI restart offsets, CZI source
  blocks and decoded TIFF strips. They split one budget: 32 MiB by default, or half of
  `CacheConfig`'s shared tile bytes when that is set.

A cache too small for an entry decodes without storing it.

## Threads

All decoding shares one process-wide CPU thread pool. A batch read decodes on the calling thread and
lets idle cores take the remaining tiles. The caller never waits for a helper that hasn't started,
so one slow reader can't hold up another reader's tiles.

## GPU Decoding

Reads that return tiles in CPU memory (`read_tile`, `read_tiles`, `read_region` and the OpenSlide
shim) decode on the CPU in every build. `DecodeAcceleration::Auto` and `CpuOnly` behave the same;
`Auto` allows a later minor release to move a codec to the GPU where measurements show a win.

The CPU wins because most of a JPEG 2000 decode is entropy decoding, which is serial within each
code-block (about 85% of CPU time on 256×256 slide tiles), and GPU output must then be read back.
Measured on an M4 Pro (8 performance and 4 efficiency cores) with `svs-jp2k-001`'s densest 256×256
tiles, Metal output read back, in milliseconds (2026-11-14):

| CPU decode threads | 1 tile, CPU / Metal | 16 tiles | 256 tiles |
| -----------------: | ------------------- | -------- | --------- |
|                  1 | 0.7 / 4.7           | 20 / 12  | 340 / 67  |
|                  2 | 0.7 / 4.7           | 13 / 12  | 175 / 67  |
|                  4 | 0.7 / 4.7           | 7.3 / 12 | 91 / 67   |
|                 12 | 0.7 / 4.7           | 3.8 / 12 | 43 / 67   |

The Metal column is one batch; wsi-rs submits device work in windows (below), one after another, so
256 tiles take about 200 ms.

`read_tile_metal`, `read_tiles_metal`, `read_tile_cuda` and `read_tiles_cuda` decode JPEG 2000 and
HTJ2K on the GPU and return an error when they can't; they never fall back to the CPU.
`download_cpu()` copies a tile to CPU memory.

- **Metal** decodes compatible tiles in windows of up to 16 images and 4 MiB of output, counting 4
  bytes per pixel. Tiles must be unsigned 8-bit RGB. YCbCr-to-RGB conversion runs on the GPU with
  the CPU path's lookup tables, so colors match exactly.
- **Cooperative Metal.** `MetalBackendSessions::with_cooperative_cpu_workers` opts a session into
  J2K's cooperative batch decoder: eligible classic reversible RGB8 batches overlap CPU entropy
  decoding with Metal reconstruction. The CPU width is capped by the pool width. Clones share one
  cooperative slot; a read that finds it taken uses ordinary Metal. J2K needs at least eight
  distinct eligible images, so 512×512 tiles cannot reach it within the 4 MiB window.
  `metal_cpu_tier1_images` in route telemetry reports actual CPU participation.
- **CUDA** uses the same window, from `decode/jp2k/device.rs`, and rejects YCbCr tiles.

## Formats

**TIFF family** (Aperio, NDPI, Leica, Ventana, Philips, Trestle, ARGOS, Huron, OME-TIFF, generic
TIFF). `container` reads TIFF and BigTIFF structure, including SubIFDs. Each module under `layout`
recognizes one vendor and maps its images to pyramid levels and associated images. A generic layout
catches any other tiled TIFF, and strip-only TIFFs whose largest image is 8-bit grayscale or RGB,
which it presents as 256×256 tiles. An uncompressed tile reads only its own rows. Compressed strips
are decoded whole and kept in the format cache, so neighboring tiles share them; each must fit the
decoded-image limit, but the whole image need not. The generic layout declines a strip-only `.vsi`
file, which reaches it only when the Olympus companion folder is missing.

**NDPI** stores each level as one large JPEG. When the JPEG has restart markers, wsi-rs presents it
as virtual tiles one restart interval wide and one MCU tall, and decodes only the intervals a read
needs. A level without restart markers is one tile. Levels wsi-rs generates below the stored ones
use tiles of at most 256×256.

**DICOM** slides are a folder of DICOM files. Compressed frames are located through an index built
once per image on first use, from the Extended Offset Table, the Basic Offset Table, or by scanning
item headers. `transfer_syntax` classifies each image's transfer syntax once, so tile reads don't
compare UIDs. Every count and offset in the index is checked against the file size. Tiles that a
sparse image leaves out come back black.

**MIRAX** stores tiles in data files listed in an index file. A batch read decodes each stored image
once even when several requested tiles share it.

**Hamamatsu VMS** lists JPEG files in an index. They store no per-tile offsets, so wsi-rs scans each
JPEG's restart markers to place tiles.

**Hamamatsu VMU** stores 12-bit samples in column-ordered NGR files. wsi-rs reads them as virtual
tiles of at most 256×64 pixels and returns all 16 bits. The C shim converts to 8 bits the way
OpenSlide does (`sample >> 4`).

**Olympus VSI** keeps pixels in `.ets` companion files, one per scene. Tiles are JPEG 2000 and
decode on the codec pool like other JPEG 2000 tiles.

**Zeiss CZI** stores a mosaic of overlapping blocks. The parser is in this repository
(`formats/zeiss/container`); `formats/zeiss/preflight.rs` checks every segment's size and position
against the limits before anything else is read. Output tiles are composed from the blocks in a
fixed order, and decoded blocks are cached because neighboring tiles reuse them.

**Zeiss ZVI** is an OLE compound file of image planes. wsi-rs places the planes on a mosaic from
their stage positions and keeps channels and Z planes.

**Raw JPEG 2000** files have one image. Each wavelet resolution becomes a level with a downsample of
2, 4, 8 and so on, decoded at that resolution rather than by shrinking the full image. A level with
a side over 4096 pixels is read as virtual 1024×1024 tiles, each a region decode of the codestream,
so the decoded-image limit applies per tile. A batch decodes its tiles serially side by side, which
lets J2K decode Tier-1 in bands as the inverse DWT reads them. Smaller levels decode whole. Metal
decodes tiles at reductions up to 3 as regions sharing the codestream's bytes, in the usual windows;
deeper reductions return `WsiError::Unsupported`. CUDA needs the full resolution as one tile. A raw
read of a full-resolution tile returns the whole codestream with `tile_region` set; reduced
resolutions have no raw read and no byte location.

**`.svcache`** files hold every level of a slide's default plane already decoded and
zstd-compressed, with its associated images. `SvcachePolicy` decides whether `Slide::open` uses one,
and `src/bin/svcache.rs` builds them.

## OpenSlide Shim

`wsi-rs-openslide-shim` exports OpenSlide's C functions and forwards them to a `Slide`. Large
`read_region` calls are written in horizontal bands of 262,144 pixels, or 32,768 pixels for
irregular tile layouts such as MIRAX, so memory stays bounded and the pixels do not depend on thread
count. Irregular reads up to 256×256 are written in one pass, as is any read that splitting would
change at the edges or that exceeds the slide's read limits. Slides opened through the shim use a 32
MiB tile cache, matching OpenSlide 4.0.1, and no display cache.

## Unsafe Code

`src/lib.rs` denies `unsafe` code. The only exception is `src/output/metal/interop.rs`, which calls
the Metal API and is allowed by `#[allow(unsafe_code)] mod interop;` in `src/output/metal.rs`.
`tests/repo_policy/unsafe_syntax.rs` fails the tests if `unsafe` appears anywhere else in `src/`.
`wsi-rs-python` also denies `unsafe`. The OpenSlide shim implements a C ABI and is outside this
policy.
