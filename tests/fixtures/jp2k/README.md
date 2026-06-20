Raw JPEG 2000 fixtures for the subset covered by wsi-rs tests:

- `.j2k` raw codestreams
- 3 components
- 8-bit samples
- single tile
- RGB and YCbCr 4:4:4 / 4:2:2 / 4:2:0

Regenerate with:

```sh
./.venv/bin/python tests/fixtures/jp2k/generate.py
```

Requires `opj_compress` and `opj_decompress` on `PATH`.
