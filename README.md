<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# wsi-rs

[![CI](https://github.com/frames-sg/wsi-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/frames-sg/wsi-rs/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/wsi-rs.svg)](https://crates.io/crates/wsi-rs)
[![docs.rs](https://img.shields.io/docsrs/wsi-rs)](https://docs.rs/wsi-rs)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-orange.svg)](#license)

[Project website](https://frames-sg.com/whole-slide-imaging-rust/) ·
[API documentation](https://docs.rs/wsi-rs)

wsi-rs is a Rust library for reading whole-slide images.

A whole-slide image is a scan of an entire microscope slide, usually a tissue
sample in pathology. One scan holds billions of pixels, far too many to load at
once. Scanners store it as a pyramid: the full-resolution image plus several
smaller copies, each cut into tiles and compressed, usually as JPEG or
JPEG 2000. Every scanner vendor has its own file format.

wsi-rs reads those files. You ask for a rectangle of pixels at a zoom level.
wsi-rs finds the tiles that cover it, decodes them, stitches them together and
gives you an image. The API is the same for every format.

## Why use it

[OpenSlide](https://openslide.org/) is the C library most pathology software
uses for this. wsi-rs does the same job, and:

- **Is memory-safe.** The library denies `unsafe` code everywhere except one
  module that calls Apple's Metal API. JPEG, JPEG 2000 and JPEG XR are decoded by Rust
  codecs ([J2K](https://frames-sg.com/j2k/rust-jpeg2000-codec/) and
  [JXR](https://frames-sg.com/rust-jpeg-xr-codec/)), not libjpeg or OpenJPEG.
- **Handles bad files.** Every size, count and offset read from a file is
  checked before use, and memory is capped, so a corrupt or malicious file
  should get an error rather than a crash or a runaway allocation. The parsers
  are fuzzed. Report any file that gets past these checks as described in
  [SECURITY.md](SECURITY.md).
- **Reports decode failures.** If a tile can't be decoded correctly, you get an
  error rather than a black or half-drawn tile.
- **Can decode on the GPU.** With the `metal` or `cuda` feature, JPEG 2000 tiles
  decode on the GPU when that is faster on your hardware.
- **Works with existing OpenSlide programs.** The included
  [OpenSlide-compatible C library](wsi-rs-openslide-shim/README.md) lets
  software built on OpenSlide use wsi-rs without code changes.

CI compares wsi-rs output with OpenSlide on a public corpus of real slides.

DICOM metadata uses [dicom-rs](https://github.com/Enet4/dicom-rs). CZI reading is implemented
in this repository; LZW, DEFLATE, and Zstd decoding use the Frames
`j2k-tilecodec` crate. ICC profiles are presented unmodified, with their source
and provenance, through the dataset; wsi-rs does not apply them. PNG/BMP decoding, public RGB/RGBA display
buffers, and image saving use the `image` crate. Display methods retain their
`image::RgbImage` and `image::RgbaImage` return types and `.save()` support.

## Install

```sh
cargo add wsi-rs
```

wsi-rs needs Rust 1.99 or newer and a 64-bit x86 or ARM target.

Version 0.8.1 is a GitHub source release with an exact Git pin for the J2K
CUDA packet-header fix. It is not on crates.io. To use this version:

```toml
[dependencies]
wsi-rs = { git = "https://github.com/frames-sg/wsi-rs", tag = "v0.8.1" }
```

For Python access to metadata, compressed tile bytes and tile byte locations,
see the experimental [reader binding](wsi-rs-python/README.md); releases
publish it to PyPI as `wsi-rs`. Pixel decoding and tensors use the separate
`j2k` Python extension.

## Example

```rust,no_run
use wsi_rs::{RegionRequest, Slide};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let slide = Slide::open("sample.svs")?;

    // The pyramid of the first image, from full resolution down.
    let levels = &slide.dataset().scenes[0].series[0].levels;
    for (index, level) in levels.iter().enumerate() {
        println!(
            "level {index}: {} x {} pixels, downsample {}",
            level.dimensions.0, level.dimensions.1, level.downsample
        );
    }

    // A 1024 x 1024 region from the top-left corner of level 0.
    let region = RegionRequest::builder(0usize, 0usize, 0u32)
        .origin_px((0, 0))
        .size_px((1024, 1024))
        .build()?;
    slide.read_region_rgba(&region)?.save("region.png")?;

    // The photo of the slide label, if the scanner stored one.
    if slide.dataset().associated_images.contains_key("label") {
        slide.read_associated("label")?.to_rgba()?.save("label.png")?;
    }

    // Scanner metadata, using OpenSlide's property names.
    if let Some(mpp) = slide.dataset().properties.get("openslide.mpp-x") {
        println!("{mpp} microns per pixel");
    }
    Ok(())
}
```

## What's in a slide

`Slide::open` reads the file's metadata. `slide.dataset()` describes it:

- **Scenes.** Most files hold one scanned area. Some formats hold several
  separate areas; each one is a scene.
- **Series.** The image data of a scene. Nearly every scene has exactly one.
- **Levels.** The pyramid. Level 0 is full resolution. Each level's
  `downsample` says how much smaller it is: 4.0 means a quarter of the width
  and a quarter of the height.
- **Associated images.** Small extra pictures stored with the scan, such as
  `label`, `macro` (a photo of the whole glass slide) and `thumbnail`.
- **Properties.** Scanner metadata as text. Common values use OpenSlide's
  names (`openslide.mpp-x`, `openslide.objective-power`, `openslide.vendor`).
  Vendor values keep a vendor prefix (`aperio.AppMag`).

Ways to read pixels:

| Method | Returns |
| --- | --- |
| `read_region_rgba`, `read_region` | Any rectangle on any level, in that level's pixel coordinates. |
| `read_tile`, `read_tiles` | Tiles exactly as the file stores them. |
| `read_display_tile` | Tiles on a regular grid of a size you choose, whatever the file uses. Useful for viewers. |
| `read_associated` | An associated image by name. |

## Lossless tile extraction and byte locations

`read_raw_compressed_tile` returns a tile's encoded bytes without decoding its
pixels. The result is a standalone stream that a standard decoder reads on its
own: TIFF `JPEGTables` are merged into each JPEG, and NDPI restart intervals
get a JPEG header. When a tile is part of a larger stored JPEG, wsi-rs crops
the JPEG losslessly: the quantized coefficients are kept and only the entropy
coding is redone, so no pixels are decoded or re-encoded. A crop starts on the
JPEG's block (MCU) grid. When the tile origin is not on that grid, the image
starts at the previous block boundary and `RawCompressedTile::tile_region`
locates the tile inside it. A crop holds the whole stored JPEG's DCT
coefficients, about 12 bytes per pixel for a 4:4:4 JPEG and more for a
progressive one, and its working set must fit
`SlideLimits::lossless_crop_bytes` (256 MiB by default). When it does not,
the tile comes back as the whole stored JPEG, unchanged and still lossless,
with `tile_region` locating the tile; `jpegtran -crop` can cut it, as it is
when the crop's output would exceed the encoded-unit limit. Apart from the
usual per-operation and per-slide admission limits, only a stored JPEG over
the encoded-unit limit, or a tile spanning several stored JPEGs (TIFF strips)
whose crop does not fit, returns `WsiError::ResourceLimit`.
A tile cut from a larger JPEG 2000 or JPEG XR image returns the whole stored
image, with `tile_region` set. A tile cut from, or joined from, units of a
lossless codec (strips of an LZW TIFF, a crop of a PNG, uncompressed planes)
is returned as its exact uncompressed samples
(`Compression::None`). So is a lossless tile whose decoding needs parameters
`RawCompressedTile` does not carry: a TIFF predictor, separate sample planes,
a non-native byte order, B, G, R sample order or masked high bits. In TIFF,
separate sample planes are handled for 8-bit RGB; other planar tiles,
floating-point predictors and subsampled YCbCr payloads return
`WsiError::Unsupported`. `RawCompressedTile::significant_bits` reports how
many low bits of each sample are used when fewer than its bit depth. Tiles with no stored pixels, and tiles joined from
several lossy units, return `WsiError::Unsupported`; `tile_location` still
describes them.

`tile_location` says where a tile's encoded bytes are, from the file's index,
without decoding the tile. A few formats read a handful of marker bytes to
place a range exactly: the end of a DICOM frame, NDPI restart markers once
per level, and the restart-marker scan VMS files need because they store no
per-tile offsets. `level_tile_locations` does this for every tile of a level
plane in one call, within the tile-index limit. Each result is one of:

- `TileLocation::Stored`: one `EncodedUnit` per stored image the tile uses,
  usually one. A unit has the file path, byte ranges, framing, codec
  parameters (compression, bit depth, samples, photometric interpretation,
  planar configuration, channel order, predictor, byte order, chroma
  subsampling, JPEG tables), the stored image size, the part of it the tile uses
  (`source_region`) and where that part lands in the tile (`tile_region`).
- `TileLocation::Absent`: the file stores no pixels for the tile, such as a
  sparse or empty TIFF tile, a missing DICOM frame or VSI background.
- `TileLocation::NotAddressable`: no byte range holds the tile's pixels. The
  reason is a synthetic level (with its base level and factor), a resampled
  tile, a window of one large JPEG 2000 codestream, or pixels the file does
  not store.

| Framing | The bytes at a unit's ranges are |
| --- | --- |
| `Standalone` | A complete stream: a JPEG with its tables, a JPEG 2000 codestream, a JPEG XR image. |
| `JpegTables` | A TIFF abbreviated JPEG. The tables it needs are in `codec().jpeg_tables()`. |
| `BarePayload` | A codec payload without framing, such as TIFF LZW, Deflate, Zstd or uncompressed data. |
| `Prefixed` | Part of a larger JPEG (NDPI). `prefix()`, the ranges and `suffix()` form a standalone JPEG. |

`EncodedUnit::assemble` builds the standalone stream from the bytes at the
ranges. For a unit that covers its whole tile, the result equals
`read_raw_compressed_tile`'s data, except for the bare payloads above that raw
reads return as reordered or decoded uncompressed samples. Every range is checked against the file
size; a range past the end of the file is an error. `.svcache` files, which
hold wsi-rs's own decoded tiles, have no locations and return
`WsiError::Unsupported`.

```rust,no_run
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use wsi_rs::{PlaneSelection, Slide, TileLocation, TileRequest};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let slide = Slide::open("sample.svs")?;

    // Where every tile of level 0 is stored.
    let level = slide.level_tile_locations(0usize, 0usize, 0u32, PlaneSelection::default())?;
    for entry in &level.tiles {
        if let TileLocation::Stored(stored) = &entry.location {
            for unit in stored.units() {
                println!("({}, {}) {:?} {:?}", entry.col, entry.row, unit.framing(), unit.segments());
            }
        }
    }

    // Rebuild one tile from its byte ranges.
    let req = TileRequest::new(0usize, 0usize, 0u32, 0, 0);
    if let TileLocation::Stored(stored) = slide.tile_location(&req)? {
        let unit = stored.single_unit().ok_or("tile is joined from several units")?;
        let mut file = File::open(unit.path())?;
        let mut parts = Vec::new();
        for range in unit.segments() {
            let mut bytes = vec![0; usize::try_from(range.length)?];
            file.seek(SeekFrom::Start(range.offset))?;
            file.read_exact(&mut bytes)?;
            parts.push(bytes);
        }
        let stream = unit.assemble(parts.iter().map(Vec::as_slice))?;
        assert_eq!(stream, slide.read_raw_compressed_tile(&req)?.data());
    }
    Ok(())
}
```

`cargo run --example tile_locations -- <slide>` checks these ranges against
`read_raw_compressed_tile` for the first tiles of a level. The
[Python binding](wsi-rs-python/README.md) exposes the same locations, with a
kerchunk-style reference example.

## Supported formats

| Format | Files |
| --- | --- |
| Aperio SVS | `.svs`, `.tif` |
| Hamamatsu NDPI | `.ndpi` |
| Hamamatsu VMS and VMU | `.vms`, `.vmu` with their companion files |
| Leica SCN | `.scn` |
| Roche Ventana BIF | `.bif`, `.tif` |
| Philips TIFF | `.tiff` |
| Trestle | `.tif` |
| ARGOS | `.avs` |
| Huron | `.tif` |
| 3DHISTECH MIRAX | `.mrxs` with its data folder |
| Olympus VSI | `.vsi` with its `.ets` files |
| Zeiss CZI and ZVI | `.czi`, `.zvi` |
| DICOM whole-slide images | `.dcm` files or a folder of them |
| OME-TIFF | `.ome.tif`, `.ome.tiff` |
| Other TIFF | `.tif`, `.tiff` |
| Raw JPEG 2000 | `.j2k`, `.j2c` |
| wsi-rs pre-decoded cache | `.svcache` (see below) |

Sakura is not supported: there is no public sample file to test against.

### Format details

- **Zeiss CZI:** single-plane RGB brightfield scans, stored uncompressed, as
  JPEG or as JPEG XR. Fluorescence channels, z-stacks and other pixel types
  return an error. Multiple scenes are combined onto one canvas.
- **OME-TIFF:** single-file OME-TIFF with tiled planes. Each plane's SubIFDs
  are its pyramid levels. Planes can be 8-bit RGB, or 8-bit, 16-bit or float
  channels, with Z planes and time points. Images named `label`, `macro` or
  `thumbnail` become associated images; other images become scenes.
  Multi-file datasets, `.companion.ome` metadata and stripped planes return an
  error.
- **Other TIFF:** any tiled TIFF. A TIFF stored in strips opens when its
  largest image is 8-bit grayscale or RGB, uncompressed or compressed with
  LZW, Deflate, Zstd or JPEG; its other images become associated images. A
  read decodes only the strips its tiles cover, so the whole image may exceed
  the decoded-image limit. Strip images with 16-bit samples, a color palette,
  CMYK or alpha return an error. JPEG XR tiles must be 8-bit gray or RGB
  without alpha.
- **DICOM:** unsigned 8-bit images and images stored in 16-bit words. Native,
  RLE, lossless JPEG, JPEG 2000 and HTJ2K frames keep 9- through 16-bit samples
  as `Uint16`; JPEG Extended and progressive JPEG frames keep their 12-bit
  precision. Higher-bit compressed frames decode on the CPU.
- **Hamamatsu VMU:** samples are 12-bit and come back as 16-bit samples with all
  bits kept. Test files are generated from
  [OpenSlide's NGR layout description](https://github.com/openslide/openslide/blob/main/misc/imhex/hamamatsu-vmu-ngr.hexpat);
  no real VMU scan is in the test corpus.

To display higher-bit samples as 8-bit RGBA, use `read_region_rgba_windowed`
with an explicit range, such as `DisplayWindow::new(0.0, 4095.0)?` for 12-bit
data.

## Memory

Each slide caches decoded tiles in 128 MiB by default: 64 MiB for decoded
tiles, 32 MiB for display tiles and 32 MiB for format-specific data. Set your
own sizes with `SlideOpenOptions::with_cache_config`.

Every slide also has hard limits that protect against bad files:

| Limit | Default |
| --- | --- |
| All metadata in a slide | 128 MiB |
| One metadata value | 16 MiB |
| Tile index | 128 MiB |
| One compressed tile or frame | 128 MiB |
| One decoded tile or image | 128 MiB |
| One region | 33,554,432 pixels (128 MiB as RGBA) |
| One lossless JPEG crop in a raw tile read | 256 MiB |
| Scratch memory per operation | 384 MiB |
| Memory in use per slide at once | 512 MiB |

Large batches are split and processed in order. A request fails only when a
single tile, image or region is over a limit. A raw tile read that may crop a
stored JPEG reserves the crop limit as scratch memory, so keep it within the
per-operation limit; other reads do not reserve it. To change the limits, pass
a `SlideLimits` to `SlideOpenOptions::with_limits`.

## GPU decoding

| Feature | Hardware |
| --- | --- |
| `metal` | Apple GPUs on macOS |
| `cuda` | NVIDIA GPUs |

Both features accelerate JPEG 2000 and HTJ2K tiles. The normal read methods
still return tiles in CPU memory. Behind the scenes, wsi-rs times the GPU and
the CPU on the first few reads of each kind of tile and uses the GPU only if it
is at least 15% faster. If the GPU fails, the read uses the CPU. To turn this
off, open the slide with:

```rust,ignore
SlideOpenOptions::default().with_decode_execution_options(
    DecodeExecutionOptions::default().with_acceleration(DecodeAcceleration::CpuOnly),
)
```

To keep decoded tiles in GPU memory, call `read_tile_metal`, `read_tiles_metal`,
`read_tile_cuda` or `read_tiles_cuda`. These only handle JPEG 2000 and HTJ2K
tiles and return an error for anything else. They never fall back to the CPU.
Call `download_cpu()` on the result to copy a tile to CPU memory.

## Pre-decoded cache files

Decoding JPEG and JPEG 2000 is most of the work of reading a slide. A
`.svcache` file stores every tile of every level already decoded (compressed
with zstd), plus the associated images, so reads skip JPEG and JPEG 2000
decoding.

Build one next to the slide:

```sh
cargo run --release --bin svcache -- build sample.svs
```

This writes `sample.svs.svcache`. To use it, open the slide with
`SlideOpenOptions::with_svcache_policy`:

| Policy | Behavior |
| --- | --- |
| `SvcachePolicy::Off` (default) | Ignore cache files. |
| `SvcachePolicy::PreferFresh` | Use a cache file that matches the slide; otherwise read the slide. |
| `SvcachePolicy::RequireFresh` | Use a cache file that matches the slide; otherwise return an error. |

wsi-rs looks for the cache file next to the slide and in
`~/.cache/wsi-rs/svcache/`. Checking that a cache file matches its slide
reads the whole slide file and hashes it with SHA-256, which takes a while for
large slides.

## Logging

wsi-rs logs through [`tracing`](https://docs.rs/tracing). It installs no
subscriber; your application chooses where logs go. At debug level it logs
cache hits and misses and decode timings:

```sh
RUST_LOG=wsi_rs=debug your-app
```

## OpenSlide-compatible C library

[`wsi-rs-openslide-shim`](wsi-rs-openslide-shim/README.md) builds a shared
library with the same C functions as OpenSlide. Programs that load OpenSlide
can load it instead and read slides through wsi-rs.

## Development

```sh
cargo xtask validate      # format, lint, tests, docs
cargo xtask bench-check   # build release performance and OpenSlide shim binaries
cargo xtask fuzz-check    # check that every fuzz target compiles
cargo xtask rc-preflight  # every release check
```

Common tests run once. OpenSlide adds only oracle tests to the quick run;
its real-slide comparisons run through `cargo xtask parity-corpus-test`.
On macOS, the library suite and Metal surface check also run with Metal enabled.

[docs/architecture.md](docs/architecture.md) explains how the code is organized
and what the release checks need. Dependency rules are in
[SUPPLY_CHAIN.md](SUPPLY_CHAIN.md).

## Explicit conformance and performance runs

Generated correctness regressions, including cooperative Metal decoding, run in
normal tests under their relevant platform and feature. Real-slide checks and
controlled timing experiments use the explicit commands below. Missing required
inputs or hardware fail the run. The commands list the selected ignored tests
first and reject a selection that matches no tests.

`cargo xtask parity-corpus-test` covers the public manifest plus DICOM folder
opening, Olympus VSI reading, and selected SVS/NDPI/DICOM color checks.
Color checks require every selected sample; they are ignored by quick tests.
`WSI_RS_PARITY_ALIASES` limits it to the
manifest aliases it lists, separated by commas, semicolons or whitespace; unset
or empty selects the whole corpus. When Olympus or ZVI is selected, it also runs
independent native-pixel probes. Generate those probes with the existing
SlideIO 2.9.0 environment after fetching the two samples:

```sh
scripts/parity-corpus-fetch.sh olympus-vsi-001 zeiss-zvi-001
python scripts/prepare-vendor-reference.py --output target/vendor-reference
WSI_RS_VENDOR_REFERENCE_ROOT=target/vendor-reference cargo xtask conformance-test vendor
```

The generator resolves these two sources from `WSI_RS_PARITY_CORPUS_CACHE`,
with explicit `--vsi` and `--zvi` overrides available. Corpus CI and source
release preflight create an isolated pinned SlideIO environment and run the
comparison. ZVI references must cover all three native U16 channels. No source
file is changed by generation.

| `cargo xtask conformance-test` suite | Required input and checks |
| --- | --- |
| `czi` | `WSI_RS_ZEISS_CZI_PATH` for Zeiss-5-Uncompressed.czi and `WSI_RS_CZI_JXR_PATH` for Zeiss-5-JXR.czi; five canvas/cache regressions and the JPEG XR readback case. |
| `vendor` | `WSI_RS_VENDOR_REFERENCE_ROOT` from the generator; independent VSI RGB and ZVI U16 pixels. |
| `zvi-variants` | `WSI_RS_ZVI_ROOT` containing the six filenames listed in `tests/zeiss_zvi.rs`; channels, Z planes, mosaic geometry and thumbnail decoding. |
| `vmu` | Independent libopenslide; the task builds the shim and points `WSI_RS_VMU_SHIM_LIBRARY` at it. Generated RGB12 geometry and ABI pixel parity. |
| `openslide` | Independent libopenslide, `WSI_RS_OPENSLIDE_COMPARE_PATHS`, and `WSI_RS_APERIO_JPEG_RGB_PATH`; region and reduced-level RGBA comparisons. |
| `metal-color` | macOS Metal; exhaustive YCbCr8 parity for both addressing kernels. |
| `metal-edges` | macOS Metal and `WSI_RS_METAL_EDGE_PATHS`; each source must have a regular eligible edge tile. |
| `local-htj2k` | macOS Metal and `WSI_RS_LOCAL_HTJ2K_DICOM`; real HTJ2K resident pixel parity. |
| `legacy-slides` | `WSI_RS_NDPI_WRAPPED_FIXTURE` (Hamamatsu-1.ndpi), `WSI_RS_LEICA_MULTI_SCENE_FIXTURE` (Leica-3.scn), `WSI_RS_LEICA_FLUORESCENCE_FIXTURE` (Leica-Fluorescence-1.scn), and `WSI_RS_DICOM_SPLIT_FIXTURE` (3DHISTECH-2 split-level folder); retained format regressions using explicit paths. |
| `ome-tiff` | `WSI_RS_OME_TIFF_FIXTURE` naming a single-file OME-TIFF; SubIFD pyramid levels and full- and lowest-resolution tiles of every channel. |

Use `cargo xtask perf-experiment <test-name>` for the following retained
workloads. This builds the selected target in release mode with route telemetry
and runs only the named experiment. Use separate output files for each run and
compare captures only under controlled corpus, cache and thread conditions.
These timing runs are outside ordinary correctness counts.

| Experiment name | Required environment |
| --- | --- |
| `metal_window_performance`, `metal_cooperative_performance` | macOS Metal, `WSI_RS_PREPARE_PATH`, `WSI_RS_PREPARE_OUTPUT` |
| `prepared_cpu_performance`, `pooled_cpu_scheduling_performance` | `WSI_RS_PREPARE_PATH`, `WSI_RS_PREPARE_OUTPUT` |
| `ycbcr_selected_u32_stays_within_five_percent_of_reference` | macOS Metal; controlled five-run address-width gate |
| `composition_performance` | `WSI_RS_COMPOSITION_OUTPUT` |
| `native_decode_matrix` | `WSI_RS_DECODE_PATH`, `WSI_RS_DECODE_OUTPUT`; optional CPU/automatic/device modes documented in the test |
| `native_decode_revisits`, `native_decode_fresh_readers` | `WSI_RS_DECODE_STEADY_INPUT`, `WSI_RS_DECODE_OUTPUT` |
| `panning_matrix` | `WSI_RS_PAN_PATH`, `WSI_RS_PAN_OUTPUT` |
| `warm_region_revisits` | `WSI_RS_PAN_WARM_INPUT`, `WSI_RS_PAN_WARM_OUTPUT` |
| `czi_reader_workloads` | `WSI_RS_CZI_JXR_PATH`, `WSI_RS_CZI_PERF_OUTPUT` |
| `local_htj2k_dicom_level_preparation_meets_interactive_budget` | macOS build and `WSI_RS_LOCAL_HTJ2K_DICOM`; controlled 75 ms preparation check |

The historical two-tile scheduling comparison and fixture-export test were
removed. Production batch regressions and the maintained workloads above cover
their relevant behavior.

## Security

Report vulnerabilities privately through GitHub. See [SECURITY.md](SECURITY.md).

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your
option.
