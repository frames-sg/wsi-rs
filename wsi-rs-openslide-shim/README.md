# wsi-rs-openslide-shim

OpenSlide-compatible C ABI shim backed by `wsi-rs`.

Build:

```sh
cargo build -p wsi-rs-openslide-shim --release
```

Library names:

| Platform | Build output | OpenSlide-compatible names |
| --- | --- | --- |
| macOS | `target/release/libwsi_rs_openslide_shim.dylib` | `libopenslide.1.dylib`, `libopenslide.dylib` |
| Linux | `target/release/libwsi_rs_openslide_shim.so` | `libopenslide.so.1`, `libopenslide.so` |

Install into a private prefix:

```sh
cargo run -p wsi-rs-openslide-shim --bin wsi-rs-openslide-install -- \
  install --shim target/release/libwsi_rs_openslide_shim.dylib \
  --prefix /tmp/wsi-rs-openslide
```

Use the `.so` build output on Linux. Test with `DYLD_LIBRARY_PATH` or
`LD_LIBRARY_PATH` pointed at the private prefix before replacing any system
OpenSlide library.

Implemented ABI surface: vendor detection, open/close/error/version, level
metadata, `read_region`, properties, associated images, ICC defaults, and cache
compatibility hooks. Cache handles own byte-bounded shared caches; attaching a
cache clones its owner into the slide, so releasing the C cache handle does not
invalidate slides that still use it.

`openslide_get_version()` reports `OpenSlide 4.0.1+wsi-rs-<shim-version>`.
The OpenSlide prefix identifies the compatibility target; the suffix identifies
the exact shim package release.
