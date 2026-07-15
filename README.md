<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# wsi-rs

[![CI](https://github.com/frames-sg/wsi-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/frames-sg/wsi-rs/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/wsi-rs.svg)](https://crates.io/crates/wsi-rs)
[![docs.rs](https://img.shields.io/docsrs/wsi-rs)](https://docs.rs/wsi-rs)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-orange.svg)](#license)

`wsi-rs` is a Rust whole-slide image reader. It opens TIFF-family WSI,
DICOM VL WSI, Zeiss CZI/ZVI, MIRAX, Hamamatsu VMS/VMU, Olympus VSI/ETS, raw
JPEG 2000 codestream fixtures, and `.svcache` containers. JPEG, JPEG 2000,
and HTJ2K decode is delegated to the `j2k-*` crates.

The main crate forbids `unsafe` code.
Unsupported or incomplete sources return `WsiError`; they should not silently
produce black or partial pixels.

## Install

```sh
cargo add wsi-rs
```

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
| TIFF-family WSI | `.svs`, `.tif`, `.tiff`, `.ndpi`, `.scn`, `.bif` |
| DICOM VL WSI | `.dcm` files or a DICOM series directory |
| Zeiss | `.czi`, `.zvi` |
| MIRAX | `.mrxs` plus sibling data files |
| Hamamatsu VMS/VMU | `.vms`, `.vmu` plus sibling image files |
| Olympus VSI | `.vsi` plus matching ETS companion data |
| Raw JPEG 2000 codestream | `.j2k`, `.j2c` |
| `.svcache` | `.svcache` |

## Features

| Feature | Default | Description |
| --- | --- | --- |
| `metal` | off | Metal-backed device payloads on macOS. |
| `cuda` | off | CUDA-backed payload surface. |
| `parity-openslide` | off | OpenSlide oracle parity tests. |
| `parity-metal` | off | Metal parity checks on macOS. |

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
