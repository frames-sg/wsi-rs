<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# wsi-rs

[![CI](https://github.com/frames-sg/wsi-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/frames-sg/wsi-rs/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/wsi-rs.svg)](https://crates.io/crates/wsi-rs)
[![docs.rs](https://img.shields.io/docsrs/wsi-rs)](https://docs.rs/wsi-rs)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-orange.svg)](#license)

`wsi-rs` is a Rust whole-slide image reader. It opens TIFF-family WSI,
DICOM VL WSI, Zeiss CZI/ZVI, MIRAX, Hamamatsu VMS/VMU, Olympus VSI/ETS, raw
JPEG 2000 codestream fixtures, and `.svcache` containers. JPEG, JPEG 2000,
and HTJ2K decode is delegated to the
[J2K pure-Rust JPEG 2000 codec](https://frames-sg.github.io/j2k/rust-jpeg2000-codec/)
crates.

The main crate forbids `unsafe` code.
Unsupported or incomplete sources return `WsiError`; they should not silently
produce black or partial pixels.

## Install

```sh
cargo add wsi-rs
```

Supported architectures are x86_64 and aarch64. The JPEG backend in the
required j2k 0.7 series does not support 32-bit targets.

## Quick Start

```rust,no_run
use wsi_rs::{RegionRequest, Slide, TileOutputPreference, TilePixels, TileRequest};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let slide = Slide::open("sample.svs")?;

    let region = RegionRequest::builder(0usize, 0usize, 0u32)
        .origin_px((0, 0))
        .size_px((1024, 1024))
        .build()?;
    slide.read_region_rgba(&region)?.save("region.png")?;

    let tile = TileRequest::builder(0usize, 0usize, 0u32).tile(0, 0).build()?;
    if let TilePixels::Cpu(cpu_tile) = slide.read_tile(&tile, TileOutputPreference::cpu())? {
        println!("{}x{}", cpu_tile.width(), cpu_tile.height());
    }

    Ok(())
}
```

Use `SlideOpenOptions` for explicit cache budgets, read-through `.svcache`
lookup, custom registries, region limits, or decode execution settings.

## Architecture

`wsi-rs` presents a format-independent `Slide` façade backed by a
`SlideReader`. Format modules own metadata validation, tile or frame lookup,
and source I/O; the shared core owns typed requests, byte-bounded caches,
decode policy, and CPU or device output types. Unsupported or invalid input
returns a typed `WsiError` instead of silently producing a partial tile.

Batch reads preserve request order and cardinality. The default controlled-read
adapter checks cancellation, submits the complete request slice once, validates
the result count with `WsiError::BackendContract`, and checks cancellation again
before returning. Format-specific implementations may group internal I/O, but
they restore results to the original request slots. Cancellation is terminal for
that attempt and is not reinterpreted as a codec error or a reason to fall back
from device output to CPU. JPEG and JPEG 2000 kernels already running remain
non-preemptive.

Encapsulated DICOM images lazily share one validated frame index between reads
and `prepare_level_controlled`. The normal indexer seeks over compressed
payloads, prefers a valid Extended Offset Table, and otherwise validates the
Basic Offset Table and Item headers. Unusual supported layouts retain a token
parser fallback. Index construction checks counts, monotonic offsets, arithmetic,
file bounds, frame lengths, and `NumberOfFrames`; it publishes only complete
indexes and creates no sidecar files.

`SlideOpenOptions` owns cache configuration. The shared source-tile and display
composition caches are byte-bounded LRUs, while narrowly scoped format caches
cover source-specific data such as DICOM frame bytes. Those private caches share
one aggregate allocation derived from the configured source-tile budget; excess
per-image or per-shard caches remain disabled instead of preallocating outside
that policy. Cache effects completed by a legacy reader are not rolled back when
a controlled read is cancelled, but a cancelled result is not returned to the
caller.

Metal and CUDA features expose renderer-uploadable device payloads. Output
residency and codec backend selection are related but separate choices. Metal
tiles normally retain an immutable resident allocation through downstream GPU
use; within the main `wsi-rs` library crate, the only production unsafe-code
exception is the audited Metal ownership adapter. CUDA payload support does not
by itself provide cross-API renderer interoperability. Consumers that present
through another GPU API can call `CudaDeviceTile::download_cpu` for checked,
pitch-aware, tightly packed host staging without accessing CUDA surface internals.

Controlled-read diagnostics are opt-in and delivered outside internal locks.
The library emits operational events through `tracing`, but installs no
subscriber and owns no application UI or JSONL output.

For viewer zoom/pan debugging on tiled SVS inputs:

```sh
RUST_LOG=wsi_rs=debug WSI_RS_TILE_CACHE_BYTES=134217728 \
  WSI_RS_DISPLAY_TILE_CACHE_BYTES=67108864 your-viewer
```

`WSI_RS_TILE_CACHE_BYTES` controls the shared decoded source-tile cache and
`WSI_RS_DISPLAY_TILE_CACHE_BYTES` controls display-tile composition cache
capacity. The debug logs include cache hit/miss summaries for region/display
tiles and timing for TIFF/SVS JPEG tile batches when the host application
installs a `tracing` subscriber.

Build cache files with:

```sh
cargo run --release --bin svcache -- build sample.svs --out sample.svs.svcache
```

## Supported Inputs

| Input family | Typical paths |
| --- | --- |
| TIFF-family WSI and uncompressed RGB TIFF | `.svs`, `.tif`, `.tiff`, `.ndpi`, `.scn`, `.bif` |
| DICOM VL WSI | `.dcm` files or a DICOM series directory |
| Zeiss | `.czi`, `.zvi` |
| MIRAX | `.mrxs` plus sibling data files |
| Hamamatsu VMS/VMU | `.vms`, `.vmu` plus sibling image files |
| Olympus VSI | `.vsi` plus matching ETS companion data |
| Raw JPEG 2000 codestream | `.j2k`, `.j2c` |
| `.svcache` | `.svcache` |

Generic strip-based TIFF support is intentionally limited to one top-level,
uncompressed 8-bit RGB image with top-left orientation, no predictor, and
either interleaved or separate sample planes. Other ordinary TIFF variants
remain unsupported unless they use a registered WSI layout.

## Features

| Feature | Default | Description |
| --- | --- | --- |
| `metal` | off | Metal-backed device payloads on macOS. |
| `cuda` | off | CUDA-backed payload surface. |
| `parity-openslide` | off | OpenSlide oracle parity tests. |
| `parity-metal` | off | CPU-vs-Metal pixel parity checks on macOS. |

## OpenSlide Compatibility Shim

The workspace includes `wsi-rs-openslide-shim`, a C ABI library that exports
OpenSlide-compatible symbols and routes reads through wsi-rs.

```sh
cargo build -p wsi-rs-openslide-shim --release
cargo run -p wsi-rs-openslide-shim --bin wsi-rs-openslide-install -- \
  install --shim target/release/libwsi_rs_openslide_shim.dylib \
  --prefix /tmp/wsi-rs-openslide
```

Use `.so` instead of `.dylib` on Linux. Test in a private prefix before
replacing any system OpenSlide library.

## Development

```sh
cargo xtask validate
cargo xtask rc-preflight
cargo xtask fuzz-check
```

`cargo xtask validate` runs the default local gate.
`cargo xtask rc-preflight` runs API checks, supply-chain checks, fuzz target
checks, feature-combination checks, validation, and package dry-run checks. CI
also executes bounded fuzz campaigns from the tracked seed corpus. Temporary
dependency exceptions and their expiry dates are recorded in
[SUPPLY_CHAIN.md](SUPPLY_CHAIN.md).
Releases that include the `cuda` feature also require the fail-closed
`CUDA validation` workflow on the self-hosted CUDA runner.

## Security

Report vulnerabilities privately through GitHub private vulnerability reporting
or the repository owner profile.

## License

Dual-licensed under either [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE), at your option.
