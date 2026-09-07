Raw JPEG 2000 fixtures for the subset covered by wsi-rs tests:

- `.j2k` raw codestreams
- 3 components
- 8-bit samples
- single tile
- RGB without MCT, RGB with irreversible/reversible MCT, and YCbCr
  4:4:4 / 4:2:2 / 4:2:0

Regenerate with:

```sh
./.venv/bin/python tests/fixtures/jp2k/generate.py
```

Requires `opj_compress` and `opj_decompress` on `PATH`.

## Independent lossless HTJ2K

`openjph_rgb_u8_53.j2k` and its `.ppm` source are unchanged fixtures from
[frames-sg/j2k v0.11.0](https://github.com/frames-sg/j2k/tree/v0.11.0/crates/j2k-test-support/fixtures/htj2k/openjph_batch).
They were encoded with OpenJPH 0.27.0, independently of J2K, using a synthetic
19 x 13 RGB image, 11 x 7 tiles, reversible 5/3, two decompositions, 8 x 8 code
blocks, and no color transform. The linked README and generator provide the
exact reproduction commands. No patient content is present. These assets are
redistributed under the source repository's MIT license.
The lossless release parity test compares the complete image against the original
PPM pixels exactly, including odd edges and multiple codestream tiles.
