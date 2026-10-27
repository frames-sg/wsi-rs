# Compressed slide tiles in Python

This optional Rust extension exposes `wsi_rs.Slide`, slide metadata, tile
geometry, and compressed tile bytes. It does not expose pixel decoding;
use the separate `j2k` extension for JPEG/JPEG 2000 tensors.

```sh
uvx maturin build --manifest-path wsi-rs-python/Cargo.toml --release --out target/python-wheels
python -m pip install --no-deps --no-index --find-links target/python-wheels wsi-rs
python -m unittest discover -s wsi-rs-python/tests -v
```

```python
import wsi_rs
import j2k

slide = wsi_rs.Slide.open("case.svs")
print(slide.properties)
print(slide.levels())
hits = slide.tiles_for_region((512, 768, 256, 256), level=0)
tiles = slide.read_tiles_compressed([hit.coords for hit in hits], level=0)
requests = [j2k.ImageRequest(tile.data, color_transform=tile.color_transform) for tile in tiles]
batch = j2k.Decoder(device="cpu").decode_batch(requests)
```

`Slide(path)` and `Slide.open(path)` accept strings and path-like objects.
`metadata` describes scenes, series, `(z,c,t)` axes, channels, levels,
associated-image dimensions, and properties. `levels(scene=0, series=0)`
returns independent dictionaries with dimensions, downsample, and tile-grid
metadata. Regular and virtual grids report tile size and grid size. Irregular
grids report advance, extra tiles, and tile count.

`tiles_for_region((x,y,width,height), level=0, scene=0, series=0)` uses pixels
at the selected level. Each hit supplies `(column,row)` as `coords` and the
tile's placement relative to the requested origin as `offset`. Placement may
be fractional for irregular layouts. The reader's order is preserved. This
plans coverage only; patch cropping, overlap handling, and assembly remain
with the application.

`read_tiles_compressed(coords, level=0, scene=0, series=0, plane=(0,0,0))`
returns immutable `CompressedTile` objects in caller order, including duplicate
coordinates. Each has `data` (owned Python bytes), `coords`, `width`, `height`,
`codec`, `bits_allocated`, `samples_per_pixel`, `photometric_interpretation`,
and `color_transform`. JPEG hints are `auto`, `ycbcr`, or `none`; non-JPEG
inputs use `auto`. JPEG tables are spliced and supported NDPI tiles are
re-tiled by the existing Rust reader. Unsupported raw-tile routes raise an
error. The binding never substitutes decoded RGBA or a black tile.

Coordinate sequence length is checked against the metadata budget before
copying it into Rust. Invalid coordinate items are reported by input index.

A tile read failure raises an exception naming the failing input; a failed
batch never returns a partial list. Reader resource limits still apply. Encoded results
are additionally capped at the default 256 MiB batch-chunk limit; request
fewer tiles when needed. Tile planning observes the reader's region and
metadata limits. Payloads stay valid after the slide object is destroyed.

Open and compressed reads release the GIL. Open one `Slide` per DataLoader
worker and prefer multiprocessing `spawn`; inherited handles are rejected
after `fork`. There is no NumPy or PyTorch dependency in this reader wheel.
The Python API is experimental; no wheel publication is part of this change.

When a local `j2k` wheel and NumPy are installed, the tests also exercise TIFF
color hints and table splicing through the tensor decoder. With a Metal build
and PyTorch MPS, they decode a DICOM JPEG 2000 tile directly into an MPS tensor.
Set `WSI_REQUIRE_J2K=1` to require the optional codec integration tests.
