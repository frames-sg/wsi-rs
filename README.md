<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# wsi-rs

> Whole-slide image reader for Rust

[![CI](https://github.com/frames-sg/wsi-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/frames-sg/wsi-rs/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/wsi-rs.svg)](https://crates.io/crates/wsi-rs)
[![docs.rs](https://img.shields.io/docsrs/wsi-rs)](https://docs.rs/wsi-rs)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-orange.svg)](#license)

[Project website](https://frames-sg.com/whole-slide-imaging-rust/) ·
[API documentation](https://docs.rs/wsi-rs)

wsi-rs reads whole-slide images: microscope slide scans stored as tiled, multi-resolution pyramids
in scanner-specific file formats. Ask for a rectangle of pixels at a zoom level, and wsi-rs finds
the tiles that cover it, decodes them and stitches them together, with the same API for every
format. It does the job of [OpenSlide](https://openslide.org/), and its
[OpenSlide-compatible C library](wsi-rs-openslide-shim/README.md) lets OpenSlide programs use it
without code changes.

```sh
cargo add wsi-rs
```

wsi-rs needs Rust 1.99 or newer. CI tests Linux x86_64, macOS arm64 and Windows x86_64.

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

- `Slide`: opens a file and reads it. `read_region_rgba` returns any rectangle on any level as an
  `image::RgbaImage`; `read_tile` and `read_tiles` return tiles as the file stores them;
  `read_display_tile` returns tiles on a regular grid of a size you choose; `read_associated`
  returns an associated image by name.
- `Dataset` (`slide.dataset()`): scenes, each with a series whose levels form the pyramid. Level 0
  is full resolution, and each level's `downsample` says how much smaller it is. Also associated
  images (`label`, `macro`, `thumbnail`), ICC profiles as stored (wsi-rs does not apply them) and
  properties. Common properties use OpenSlide's names (`openslide.mpp-x`); vendor values keep a
  vendor prefix (`aperio.AppMag`).
- `RegionRequest`, `TileRequest`: what to read, from checked builders.
- `SlideOpenOptions`: cache sizes (`CacheConfig`), resource limits (`SlideLimits`), decode options
  and `.svcache` lookup, passed to `Slide::open_with_options`.
- `read_raw_compressed_tile`, `tile_location`, `level_tile_locations`: a tile's encoded bytes and
  where they are stored, without decoding. See [Encoded Tiles](#encoded-tiles).
- `WsiError`: every failure. A read never returns black or partial pixels in place of an error.

## Why wsi-rs

- **Memory-safe.** The library denies `unsafe` code except in one module that calls Apple's Metal
  API. JPEG, JPEG 2000 and JPEG XR are decoded by Rust codecs
  ([J2K](https://frames-sg.com/j2k/rust-jpeg2000-codec/) and
  [JXR](https://frames-sg.com/rust-jpeg-xr-codec/)), not libjpeg or OpenJPEG. Deflate and Zstd tiles
  go through zlib and zstd.
- **Bounded on bad files.** Every size, count and offset read from a file is checked before use, and
  memory is capped, so a corrupt or malicious file gets an error rather than a crash or a runaway
  allocation. The parsers are fuzzed. Report any file that gets past these checks as described in
  [SECURITY.md](SECURITY.md).
- **Checked against OpenSlide.** CI compares pixels with OpenSlide on a public corpus of real
  slides, and region interpolation uses OpenSlide's arithmetic.
- **GPU output.** With the `metal` or `cuda` feature, JPEG 2000 and HTJ2K tiles decode straight into
  GPU memory for renderers, encoders and models that use them there.

## Supported Formats

| Format                   | Files                                     |
| ------------------------ | ----------------------------------------- |
| Aperio SVS               | `.svs`, `.tif`                            |
| Hamamatsu NDPI           | `.ndpi`                                   |
| Hamamatsu VMS and VMU    | `.vms`, `.vmu` with their companion files |
| Leica SCN                | `.scn`                                    |
| Roche Ventana BIF        | `.bif`, `.tif`                            |
| Philips TIFF             | `.tiff`                                   |
| Trestle                  | `.tif`                                    |
| ARGOS                    | `.avs`                                    |
| Huron                    | `.tif`                                    |
| 3DHISTECH MIRAX          | `.mrxs` with its data folder              |
| Olympus VSI              | `.vsi` with its `.ets` files              |
| Zeiss CZI and ZVI        | `.czi`, `.zvi`                            |
| DICOM whole-slide images | `.dcm` files or a folder of them          |
| OME-TIFF                 | `.ome.tif`, `.ome.tiff`                   |
| Other TIFF               | `.tif`, `.tiff`                           |
| Raw JPEG 2000            | `.j2k`, `.j2c`                            |
| wsi-rs pre-decoded cache | `.svcache` (see below)                    |

Sakura is not supported: there is no public sample file to test against.

- **Zeiss CZI:** single-plane RGB brightfield scans, stored uncompressed, as JPEG or as JPEG XR.
  Fluorescence channels, Z-stacks and other pixel types return an error. Multiple scenes are
  combined onto one canvas.
- **OME-TIFF:** single-file OME-TIFF with tiled planes; each plane's SubIFDs are its pyramid levels.
  Planes can be 8-bit RGB, or 8-bit, 16-bit or float channels, with Z planes and time points. Images
  named `label`, `macro` or `thumbnail` become associated images; others become scenes. Multi-file
  datasets, `.companion.ome` metadata and stripped planes return an error.
- **Other TIFF:** any tiled TIFF, and stripped TIFFs whose largest image is 8-bit grayscale or RGB,
  uncompressed or compressed with LZW, Deflate, Zstd or JPEG. A read decodes only the strips it
  covers, so the whole image may exceed the decoded-image limit. Stripped images with 16-bit
  samples, a palette, CMYK or alpha return an error. JPEG XR tiles must be 8-bit gray or RGB without
  alpha.
- **DICOM:** unsigned 8-bit images and images stored in 16-bit words. Native, RLE, lossless JPEG,
  JPEG 2000 and HTJ2K frames keep 9- to 16-bit samples as `Uint16`; JPEG Extended and progressive
  JPEG frames keep their 12-bit precision.
- **Hamamatsu VMU:** 12-bit samples come back as 16-bit samples with all bits kept. Test files are
  generated from
  [OpenSlide's NGR layout description](https://github.com/openslide/openslide/blob/main/misc/imhex/hamamatsu-vmu-ngr.hexpat);
  no real VMU scan is in the test corpus.

To display samples above 8 bits as 8-bit RGBA, use `read_region_rgba_windowed` with an explicit
range, such as `DisplayWindow::new(0.0, 4095.0)?` for 12-bit data.

## Encoded Tiles

`read_raw_compressed_tile` returns a tile's encoded bytes without decoding its pixels, as a stream a
standard decoder reads on its own: TIFF `JPEGTables` are merged into each JPEG, and NDPI tiles get a
JPEG header. A tile cut from a larger stored JPEG is cropped losslessly in the coefficient domain,
as `jpegtran -crop` does, and `RawCompressedTile::tile_region` locates the tile when the crop starts
at the previous MCU boundary or when the whole stored image is returned instead. Tiles of lossless
codecs that cannot be returned as stored come back as exact uncompressed samples.

`tile_location` and `level_tile_locations` report where each tile's encoded bytes are stored: the
file, byte ranges, framing and codec parameters, or that the tile is `Absent` or `NotAddressable`.
`EncodedUnit::assemble` turns the bytes at those ranges into a standalone stream. See the
[`TileLocation`](https://docs.rs/wsi-rs/latest/wsi_rs/enum.TileLocation.html) and
[`RawCompressedTile`](https://docs.rs/wsi-rs/latest/wsi_rs/struct.RawCompressedTile.html) docs for
every case, and run the example that rebuilds tiles from their byte ranges:

```sh
cargo run --example tile_locations -- <slide>
```

## Memory

Each slide caches decoded tiles in 128 MiB by default: 64 MiB for decoded tiles, 32 MiB for display
tiles and 32 MiB for format-specific data. `SlideOpenOptions::with_cache_config` sets the tile and
display sizes; when the tile size is set, format caches get half of it.

Every slide also has hard limits that protect against bad files. Change them with
`SlideOpenOptions::with_limits`.

| Limit                                     | Default                             |
| ----------------------------------------- | ----------------------------------- |
| All metadata in a slide                   | 128 MiB                             |
| One metadata value                        | 16 MiB                              |
| Tile index                                | 128 MiB                             |
| One compressed tile or frame              | 128 MiB                             |
| One decoded tile or image                 | 128 MiB                             |
| One region                                | 33,554,432 pixels (128 MiB as RGBA) |
| One lossless JPEG crop in a raw tile read | 256 MiB                             |
| Scratch memory per operation              | 384 MiB                             |
| Memory in use per slide at once           | 512 MiB                             |

Large batches are split into chunks of at most 256 MiB (`SlideLimits::batch_chunk_bytes`) and
processed in order; a request fails only when a single tile, image or region is over a limit. A raw
read that may crop a stored JPEG reserves the crop limit as scratch memory, so keep it within the
per-operation limit.

## Feature Flags

- `metal`: decode JPEG 2000 and HTJ2K tiles into Metal buffers on macOS with `read_tile_metal` and
  `read_tiles_metal`.
- `cuda`: decode JPEG 2000 and HTJ2K tiles into CUDA memory with `read_tile_cuda` and
  `read_tiles_cuda`.
- `route-telemetry`: export decode-route counters for benchmarks.
- `fuzzing`, `parity-openslide`, `parity-metal`: fuzzing entry points and parity tests.

GPU features add GPU-resident reads; they don't change the normal read methods, which decode on the
CPU in every build. For slide tiles, the multithreaded CPU decoder is faster than a GPU once the
GPU's output is copied back ([measurements](docs/architecture.md#gpu-decoding)). GPU reads return an
error for anything they can't decode, never falling back to the CPU: Metal needs unsigned 8-bit RGB,
and CUDA rejects YCbCr tiles. `download_cpu()` copies a GPU tile to CPU memory.

`DecodeAcceleration::CpuOnly` guarantees CPU decoding for normal reads in later releases too. The
default, `DecodeAcceleration::Auto`, also decodes on the CPU today, but a later minor release may
move a codec to the GPU where measurements show a win.

## Pre-decoded Cache Files

Decoding JPEG and JPEG 2000 is most of the work of reading a slide. A `.svcache` file stores every
level of a slide's default plane (first Z plane, channel and time point) already decoded and
compressed with zstd, plus the associated images, so reads skip that decoding. Build one next to the
slide:

```sh
cargo run --release --bin svcache -- build sample.svs
```

This writes `sample.svs.svcache`. Open the slide with `SlideOpenOptions::with_svcache_policy` to use
it:

| Policy                         | Behavior                                                            |
| ------------------------------ | ------------------------------------------------------------------- |
| `SvcachePolicy::Off` (default) | Ignore cache files.                                                 |
| `SvcachePolicy::PreferFresh`   | Use a cache file that matches the slide; otherwise read the slide.  |
| `SvcachePolicy::RequireFresh`  | Use a cache file that matches the slide; otherwise return an error. |

wsi-rs looks for the cache file next to the slide and in `~/.cache/wsi-rs/svcache/`. Checking that a
cache file matches its slide hashes the whole slide file with SHA-256, which takes a while for large
slides.

## Logging

wsi-rs logs through [`tracing`](https://docs.rs/tracing) and installs no subscriber. At debug level
it logs cache hits and misses and decode timings:

```sh
RUST_LOG=wsi_rs=debug your-app
```

## Python and C

- [Python binding](wsi-rs-python/README.md) (`pip install wsi-rs`): metadata, tile geometry,
  compressed tile bytes and tile locations. Pixel decoding uses the separate `j2k` extension.
- [OpenSlide-compatible C library](wsi-rs-openslide-shim/README.md): a shared library with
  OpenSlide's C functions, backed by wsi-rs.

## Development

```sh
cargo xtask validate   # format, lint, tests, doc tests and docs
```

[Architecture](docs/architecture.md) explains how the code is organized, and
[Testing](docs/testing.md) covers the conformance lanes, performance experiments and release checks.
Dependency rules are in [SUPPLY_CHAIN.md](SUPPLY_CHAIN.md).

## Security

Report vulnerabilities privately through GitHub. See [SECURITY.md](SECURITY.md).

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
