<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# wsi-rs

[![CI](https://github.com/frames-sg/wsi-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/frames-sg/wsi-rs/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/wsi-rs.svg)](https://crates.io/crates/wsi-rs)
[![docs.rs](https://img.shields.io/docsrs/wsi-rs)](https://docs.rs/wsi-rs)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-orange.svg)](#license)

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
  codecs ([J2K](https://frames-sg.github.io/j2k/rust-jpeg2000-codec/) and
  [JXR](https://github.com/frames-sg/jxr)), not libjpeg or OpenJPEG.
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

## Install

```sh
cargo add wsi-rs
```

wsi-rs needs Rust 1.99 or newer and a 64-bit x86 or ARM target.

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
| Other tiled TIFF | `.tif`, `.tiff` |
| Raw JPEG 2000 | `.j2k`, `.j2c` |
| wsi-rs pre-decoded cache | `.svcache` (see below) |

Sakura is not supported: there is no public sample file to test against.

### Format details

- **Zeiss CZI:** single-plane RGB brightfield scans, stored uncompressed, as
  JPEG or as JPEG XR. Fluorescence channels, z-stacks and other pixel types
  return an error. Multiple scenes are combined onto one canvas.
- **Other tiled TIFF:** any tiled TIFF, or a single uncompressed 8-bit RGB
  image stored in strips. JPEG XR tiles must be 8-bit gray or RGB without alpha.
- **DICOM:** 8-bit images in every supported transfer syntax. JPEG Extended and
  progressive JPEG images can also be 12-bit; those come back as 16-bit samples
  holding the original 12-bit values, and decode on the CPU.
- **Hamamatsu VMU:** samples are 12-bit and come back as 16-bit samples with all
  bits kept. Test files are generated from
  [OpenSlide's NGR layout description](https://github.com/openslide/openslide/blob/main/misc/imhex/hamamatsu-vmu-ngr.hexpat);
  no real VMU scan is in the test corpus.

To display 12-bit images as 8-bit, use `read_region_rgba_windowed` with
`DisplayWindow::new(0.0, 4095.0)?`.

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
| Scratch memory per operation | 384 MiB |
| Memory in use per slide at once | 512 MiB |

Large batches are split and processed in order. A request fails only when a
single tile, image or region is over a limit. To change the limits, pass a
`SlideLimits` to `SlideOpenOptions::with_limits`.

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
cargo xtask fuzz-check    # check that every fuzz target compiles
cargo xtask rc-preflight  # every release check
```

[docs/architecture.md](docs/architecture.md) explains how the code is organized
and what the release checks need. Dependency rules are in
[SUPPLY_CHAIN.md](SUPPLY_CHAIN.md).

## Security

Report vulnerabilities privately through GitHub. See [SECURITY.md](SECURITY.md).

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your
option.
