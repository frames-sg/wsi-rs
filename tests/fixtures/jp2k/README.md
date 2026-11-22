# Raw JPEG 2000 Fixtures

`generate.py` produces raw `.j2k` codestreams with 3 components, 8-bit samples and a single tile:
RGB without a color transform, RGB with the irreversible and reversible transforms, and YCbCr 4:4:4,
4:2:2 and 4:2:0. Each has a `.ppm` reference decoded by OpenJPEG. Regenerate them with:

```sh
./.venv/bin/python tests/fixtures/jp2k/generate.py
```

This requires `opj_compress` and `opj_decompress` on `PATH`.

## Exact Output Regressions

RGB fixtures are compared byte for byte with their PPM references. The three `ycbcr_*.wsi-rgb` files
are wsi-rs's 16×12 interleaved RGB8 output from 2026-10-25: regression baselines, not independent
references. Their color conversion differs from the PPM references by at most one channel value.
Tests require exact equality with the baselines and keep the PPM tolerance check. Update the
baselines only for an intentional change to the output contract; tests never regenerate them.

## Reduced-Resolution References

`{stem}.r1.ppm` and `{stem}.r2.ppm` are OpenJPEG 2.5.4 `opj_decompress -r 1` and `-r 2` decodes of
`rgb_nomct`, `rgb_mct`, `rgb_rct`, `openjph_rgb_u8_53` and `openjpeg_rgb_75x56_origin5x3_53`. They
are independent references for raw JPEG 2000 pyramid levels. Regenerate only these, without
re-encoding any codestream, with:

```sh
./.venv/bin/python tests/fixtures/jp2k/generate.py --reduced-only
```

`openjpeg_rgb_75x56_origin5x3_53.j2k` is a copy of j2k's `crates/j2k-native/fixtures/image_origin`
fixture, whose README gives its samples and `opj_compress` command. Its reference-grid origin (5, 3)
makes reduced levels `ceil((origin + size) / 2^r) - ceil(origin / 2^r)` wide (37×28 and 18×14), not
`ceil(size / 2^r)` (38×28 and 19×14).

## Independent Lossless HTJ2K

`openjph_rgb_u8_53.j2k` and its `.ppm` source come from
[frames-sg/j2k v0.11.0](https://github.com/frames-sg/j2k/tree/v0.11.0/crates/j2k-test-support/fixtures/htj2k/openjph_batch),
where the README and generator give the exact commands. They were encoded with OpenJPH 0.27.0,
independently of J2K, from a synthetic 19×13 RGB image: 11×7 tiles, reversible 5/3, two
decompositions, 8×8 code blocks and no color transform. They hold no patient content and are
redistributed under that repository's MIT license. The lossless parity test compares the whole image
with the PPM pixels exactly, including odd edges and multiple codestream tiles.

## Corrupt Codestreams

`truncated_tall_tag_tree.j2k` is a 166-byte codestream whose packet data ends early. It is not
generated. `open_rejects_truncated_packet_data_during_reduction_probing` checks that opening it
fails instead of probing past the end.
