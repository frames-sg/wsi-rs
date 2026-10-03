# wsi-rs-openslide-shim

A drop-in replacement for the OpenSlide C library, backed by wsi-rs.

Many pathology tools read slides through [OpenSlide](https://openslide.org/), a
C library (`libopenslide`). This crate builds a shared library that exports the
same C functions under the same file names. Point a program at it instead of
OpenSlide, and the program reads slides through wsi-rs with no code changes.

## Build

```sh
cargo build -p wsi-rs-openslide-shim --release
```

| Platform | Build output | Installed as |
| --- | --- | --- |
| macOS | `target/release/libwsi_rs_openslide_shim.dylib` | `libopenslide.1.dylib`, `libopenslide.dylib` |
| Linux | `target/release/libwsi_rs_openslide_shim.so` | `libopenslide.so.1`, `libopenslide.so` |
| Windows | `target/release/wsi_rs_openslide_shim.dll` | `libopenslide-1.dll` |

## Try it without touching your system

Install into a scratch folder, then point the program's library search path at
it:

```sh
cargo run -p wsi-rs-openslide-shim --bin wsi-rs-openslide-install -- \
  install --shim target/release/libwsi_rs_openslide_shim.dylib \
  --prefix /tmp/wsi-rs-openslide

DYLD_LIBRARY_PATH=/tmp/wsi-rs-openslide/lib your-program   # macOS
LD_LIBRARY_PATH=/tmp/wsi-rs-openslide/lib your-program     # Linux
```

## Replace OpenSlide

Without `--prefix`, `install` writes to `/opt/homebrew/lib` on macOS and
`/usr/local/lib` on Linux. Pass `--prefix` to target a different folder.

- `install` renames any existing OpenSlide library in that folder to a backup,
  copies the shim in under OpenSlide's file names, checks that the result loads,
  and records what it did in `lib/.wsi-rs-openslide-shim-install.tsv`. If any
  step fails, it puts the original files back.
- `restore` puts the backed-up OpenSlide libraries back.

```sh
wsi-rs-openslide-install install --shim <path-to-built-library> [--prefix <folder>]
wsi-rs-openslide-install restore [--prefix <folder>]
```

## What it supports

The shim implements the parts of the OpenSlide API that programs use to read
slides: format detection, open and close, errors, version, level sizes and
downsamples, `read_region`, properties, associated images, ICC profiles, and
OpenSlide's shared cache functions.

It reads every format wsi-rs supports, with two differences from the Rust API:

- 12-bit DICOM images return an error, as they do in OpenSlide.
- Hamamatsu VMU images come back as 8-bit RGB, converted the way OpenSlide
  does (`sample >> 4`).

A cache created with `openslide_cache_create` stays valid as long as any slide
uses it, even after `openslide_cache_release`.

`openslide_get_version()` returns `OpenSlide 4.0.1+wsi-rs-<shim version>`: the
OpenSlide version the shim matches, then the shim's own version.
