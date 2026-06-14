# statumen-openslide-shim

OpenSlide-compatible C ABI shim backed by `statumen`.

Build:

```sh
cargo build -p statumen-openslide-shim --release
```

Library names:

| Platform | Build output | OpenSlide-compatible names |
| --- | --- | --- |
| macOS | `target/release/libstatumen_openslide_shim.dylib` | `libopenslide.1.dylib`, `libopenslide.dylib`, `libopenslide.4.dylib` |
| Linux | `target/release/libstatumen_openslide_shim.so` | `libopenslide.so.1`, `libopenslide.so`, `libopenslide.so.4` |

Install into a private prefix:

```sh
cargo run -p statumen-openslide-shim --bin statumen-openslide-install -- \
  install --shim target/release/libstatumen_openslide_shim.dylib \
  --prefix /tmp/statumen-openslide
```

Use the `.so` build output on Linux. Test with `DYLD_LIBRARY_PATH` or
`LD_LIBRARY_PATH` pointed at the private prefix before replacing any system
OpenSlide library.

Implemented ABI surface: vendor detection, open/close/error/version, level
metadata, `read_region`, properties, associated images, ICC defaults, and cache
compatibility hooks.
