# How wsi-rs works

This is a guide to the code for people changing it. For using the library, see
the [README](../README.md).

## Code layout

| Path | What it does |
| --- | --- |
| `src/core/registry` | `Slide`, format detection, region and tile reads, composition. |
| `src/core/cache.rs`, `src/core/cache/` | Byte-bounded tile caches and shared in-flight decodes. |
| `src/core/limits.rs` | `SlideLimits` and checked size arithmetic. |
| `src/core/batch.rs` | The shared CPU thread pool and idle-core accounting. |
| `src/core/decode_runtime` | Chooses CPU or GPU for JPEG 2000 and runs the measurements. |
| `src/core/types` | Public data types: `Dataset`, `Level`, requests, `CpuTile`. |
| `src/formats/<format>` | One module per file format: parse metadata, locate and read tiles. |
| `src/formats/tiff_family` | Every TIFF-based format. `container` parses TIFF, `layout` maps each vendor's TIFF layout to levels, `pixel_access` reads tiles. |
| `src/decode` | Adapters to the JPEG, JPEG 2000 and JPEG XR codec crates. |
| `src/output` | GPU tile types for Metal and CUDA, and copying GPU tiles to CPU memory. |
| `wsi-rs-openslide-shim` | The OpenSlide-compatible C library. |
| `xtask` | Build, test, release and benchmark commands (`cargo xtask`). |
| `fuzz` | Fuzz targets for every file parser. |
| `perf-runner` | Benchmarks wsi-rs and OpenSlide through the same C API. |

## Opening a slide

`Slide::open` hands the path to a `FormatRegistry`. Every registered format
checks whether it recognizes the file. A `Definite` match beats a `Likely` one;
among equal matches the format registered first wins. The winning format then
parses the file's metadata and builds a `Dataset`.

Parsing draws on an `OpenBudget`: one running total of metadata and index bytes
shared by every file in the slide (MIRAX, VMS and VSI slides are several files).
Every size read from a file goes through checked arithmetic against
`SlideLimits` before anything is allocated. Formats remember each source file's
identity and refuse to read a file that was replaced after opening.

## Reading a region

A region read goes through four steps in `core::registry::composition`:

1. **Plan.** `RegionReadPlan` validates the request and finds the tiles that
   cover the rectangle. If the rectangle lines up with whole pixels on the
   level, the read is *integral*. Otherwise it is *fractional* and needs
   interpolation.
2. **Admit.** The read reserves the memory it needs from the slide's budget.
   Reads that would go over the budget wait in line (first in, first out).
   Requests too large to ever fit are rejected.
3. **Resolve.** `RegionTileResolver` takes tiles from the cache, decodes the
   missing ones in batches, and stores the results. When two reads need the same
   missing tile at the same time, one decodes it and the other waits for that
   result (`core::cache::flights`).
4. **Compose.** Integral reads copy tile rows straight into the output.
   Fractional reads interpolate with the same arithmetic and rounding as
   OpenSlide's renderer (Pixman), so the pixels match OpenSlide.

Large regions stream through in batches sized to the memory budget. A format
can provide its own faster region path (NDPI does); the generic path handles
any request it declines.

Tile reads (`read_tile`, `read_tiles`) skip planning and composition: they
admit, resolve and return tiles in request order. Every batch read returns
exactly one result per request, in the order requested.

## Caches

Each slide has three caches, all limited by bytes and evicting the least
recently used entry:

- **Decoded tiles**, shared by all reads of the slide (64 MiB by default).
- **Display tiles** for `read_display_tile` (32 MiB).
- **Format caches** for format-specific data such as DICOM frames, NDPI restart
  offsets and CZI source blocks. They split one 32 MiB budget.

A cache sized too small for an entry decodes without storing it.

## Threads

All decoding shares one process-wide CPU thread pool. A batch read decodes on
the calling thread and lets idle cores take the remaining tiles. The caller
never waits for a helper that hasn't started, so one slow reader can't hold up
another reader's tiles.

## GPU decoding

With the `metal` or `cuda` feature, ordinary reads of JPEG 2000 and HTJ2K tiles
can decode on the GPU. `core::decode_runtime` decides per *route*: a
combination of slide, level, codec, tile sizes, batch size, CPU thread count
and GPU. For each route:

1. The first read uses the CPU.
2. A later read starts the GPU and times one CPU decode. If starting the GPU
   takes longer than four CPU decodes, the route stays on the CPU.
3. The next three reads each time the CPU and the GPU on the same input,
   alternating which goes first.
4. The route uses the GPU if the median GPU time is at most 85% of the CPU
   time. Otherwise it uses the CPU.

Up to 1,024 route decisions are kept. A GPU error sends the read to the CPU.
`read_tile_metal` and `read_tile_cuda` skip all of this: they always use the
GPU and return an error when they can't.

On Metal, compatible tiles decode together in groups of up to 16 images and
4 MiB of output. Color conversion from YCbCr to RGB also runs on the GPU, using
the same lookup tables as the CPU path, so the colors match exactly. CUDA
decodes one image per submission and rejects YCbCr tiles that the CPU would
convert.

## Formats

**TIFF family** (Aperio, NDPI, Leica, Ventana, Philips, Trestle, ARGOS, Huron,
generic TIFF). `container` reads TIFF and BigTIFF structure. Each module under
`layout` recognizes one vendor and maps its images to pyramid levels and
associated images. A generic layout catches any other tiled TIFF.

**NDPI** stores each level as one huge JPEG with restart markers. wsi-rs presents
it as virtual tiles of at most 256 x 256 pixels and decodes only the strips
between the restart markers that a read needs.

**DICOM** slides are a folder of DICOM files. Compressed frames are located
through an index built once per image on first use, from the Extended Offset
Table, the Basic Offset Table, or by scanning item headers. Every count and
offset in the index is checked against the file size. Tiles that a sparse DICOM
image leaves out come back black.

**MIRAX** stores tiles in separate data files listed in an index file. A batch
read decodes each stored image once even when several requested tiles share it.

**Olympus VSI** keeps pixels in `.ets` companion files, one per scene. Tiles are
JPEG 2000 and use the same CPU pool and GPU routing as other JPEG 2000 tiles.

**Zeiss CZI** stores a mosaic of overlapping blocks. Before the CZI library
reads anything, wsi-rs checks every segment's size and position against the
limits. Output tiles are composed from the blocks in a fixed order, and decoded
blocks are cached because neighboring tiles reuse them.

**Hamamatsu VMU** stores 12-bit samples in column-ordered NGR files. wsi-rs
reads them as virtual tiles of at most 256 x 64 pixels and returns all 16 bits.
The C shim converts to 8-bit the way OpenSlide does (`sample >> 4`).

**Raw JPEG 2000** files have one image. Each wavelet resolution becomes a
level with a downsample of 2, 4, 8 and so on, decoded at that resolution rather
than by shrinking the full image.

## The OpenSlide shim

`wsi-rs-openslide-shim` exports OpenSlide's C functions and forwards them to a
`Slide`. It writes `read_region` output in horizontal bands of at most 262,144
pixels, which keeps memory flat for large requests and gives the same pixels
at any thread count. Slides opened through the shim start with a 32 MiB cache.

## Unsafe code

`src/lib.rs` denies `unsafe` code. The only exception is
`src/output/metal/interop.rs`, which calls the Metal API.
`tests/repo_policy/unsafe_syntax.rs` fails the build if `unsafe` appears in any
other file.

## Tests

- **Unit tests** live in each module's `tests` module or `tests/` folder.
- **Integration tests** in `tests/` read synthetic and real fixture files.
- **OpenSlide comparison.** `scripts/parity-corpus-fetch.sh` downloads a public
  corpus of real slides; `cargo xtask parity-corpus-test` reads every slide with
  wsi-rs and compares pixels with OpenSlide where it supports the format.
  Release preflight covers the available public samples. VMU/NGR currently has
  synthetic-test coverage only. CI runs the public corpus checks.
- **Fuzzing.** `fuzz/fuzz_targets` has one target per parser.
  `cargo xtask fuzz-check` checks that each compiles; CI runs each for 15
  seconds, and release checks run each for five minutes.
- **Coverage.** CI requires 80% line coverage across the workspace and 70% in
  each major component.
- **Release checks.** `cargo xtask rc-preflight` runs the API and dependency
  checks, five-minute fuzz runs, every feature combination, the OpenSlide
  comparison, coverage, the performance gate and a package dry run. Releases
  with the `cuda` feature also need the `CUDA validation` workflow on the CUDA
  runner.
- **Test hooks.** Tests observe internal behavior through two modules:
  `core::execution_telemetry` counts events such as tiles decoded on each path,
  and `core::test_hooks` lets tests pause work at fixed points to force
  races. Both compile to nothing in normal builds. The `route-telemetry` feature
  also exports the event counts for benchmarks.

## Performance gate

Before a release, `rc-preflight` compares three benchmark captures: OpenSlide,
the previous wsi-rs release and the current code. Capture all three on the same
machine with the same corpus, workloads, cache size and thread limits. Build
the previous and current wsi-rs versions with the same Rust toolchain and
release profile. When supplying a prebuilt library through
`WSI_RS_BENCH_WSI_RS_LIBRARY`, check its build settings too; the capture's
toolchain metadata describes the running environment, not that library's
compiler.

To capture the results:

1. Set `WSI_RS_PERF_PINNED_HOST_ID` to a fixed name for the machine, and
   `WSI_RS_PERF_GPU_FEATURE` to `metal` or `cuda`.
2. Set `WSI_RS_BENCH_PREVIOUS_LIBRARY` to the previous release's shim, then run
   `cargo xtask perf-capture-pair <label>`. This interleaves all three libraries
   for each sample, worker count and repeat, reversing their order on alternate
   repeats. It writes `<label>-wsi_rs.json`, `<label>-openslide.json` and
   `<label>-previous.json`.
3. Without the previous-library setting, the command captures only the current
   code and OpenSlide. `perf-capture` can still capture one library separately.
4. Point `WSI_RS_RC_OPENSLIDE_CAPTURE`, `WSI_RS_RC_PREVIOUS_CAPTURE` and
   `WSI_RS_RC_CURRENT_CAPTURE` at the three capture files.

The gate fails if the current code is slower than allowed or if its pixels
differ from OpenSlide beyond the corpus color tolerances.
Worker schema 5 batches checksum updates without changing the checked bytes;
recapture all engines together rather than mixing results from older workers.
