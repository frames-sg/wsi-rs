<!-- SPDX-License-Identifier: Apache-2.0 -->

# statumen

[![CI](https://github.com/frames-sg/statumen/actions/workflows/ci.yml/badge.svg)](https://github.com/frames-sg/statumen/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/statumen.svg)](https://crates.io/crates/statumen)
[![docs.rs](https://img.shields.io/docsrs/statumen)](https://docs.rs/statumen)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

`statumen` is a Rust whole-slide image reader. It opens TIFF-family WSI,
DICOM VL WSI, Zeiss CZI/ZVI, MIRAX, Hamamatsu VMS/VMU, Olympus VSI/ETS, raw
JPEG 2000 codestream fixtures, and `.svcache` containers. JPEG, JPEG 2000,
and HTJ2K decode is delegated to the `signinum-*` crates.

The main crate forbids `unsafe` code.
Unsupported or incomplete sources return `WsiError`; they should not silently
produce black or partial pixels.

## Install

```sh
cargo add statumen
```

## Quick Start

```rust,no_run
use statumen::{RegionRequest, Slide, TileOutputPreference, TilePixels, TileRequest};

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

The workspace includes `statumen-openslide-shim`, a C ABI library that exports
OpenSlide-compatible symbols and routes reads through Statumen.

```sh
cargo build -p statumen-openslide-shim --release
cargo run -p statumen-openslide-shim --bin statumen-openslide-install -- \
  install --shim target/release/libstatumen_openslide_shim.dylib \
  --prefix /tmp/statumen-openslide
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
type-checking, feature-combination checks, validation, and package dry-run
checks.

## Security

Report vulnerabilities privately through GitHub private vulnerability reporting
or the repository owner profile.

## License

Apache-2.0. See [LICENSE](LICENSE).
