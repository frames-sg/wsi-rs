# Compressed slide tiles in Python

This optional Rust extension exposes `wsi_rs.Slide`, slide metadata, tile
geometry, and compressed tile bytes. It does not expose pixel decoding;
use the separate `j2k` extension for JPEG/JPEG 2000 tensors.

```sh
python -m pip install wsi-rs
```

Releases publish wheels to PyPI for Python 3.10 and later on Linux x86_64
and aarch64 (glibc 2.28 or newer), macOS arm64 and x86_64, and Windows x64,
and attach them to the GitHub release. To build from a checkout instead:

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
`codec`, `bits_allocated`, `significant_bits` (the low bits of each sample
that carry it, or `None` when all do), `samples_per_pixel`,
`photometric_interpretation`, `color_transform`, and `tile_region`. JPEG hints are `auto`, `ycbcr`, or `none`; non-JPEG
inputs use `auto`. JPEG tables are spliced and supported NDPI tiles are
re-tiled by the existing Rust reader. Unsupported raw-tile routes raise an
error. The binding never substitutes decoded RGBA or a black tile.

Coordinate sequence length is checked against the metadata budget before
copying it into Rust. Invalid coordinate items are reported by input index.

`tile_region` is `(x,y,width,height)` of the requested tile inside the encoded
image. It is smaller than the image when the stored image is larger than the
tile: an edge tile in a padded TIFF tile or DICOM frame, a tile cut losslessly
from a larger JPEG whose block (MCU) grid does not start at the tile origin
(the encoded image then starts at the previous block boundary), or a tile
inside a whole JPEG 2000 or JPEG XR image that is returned unchanged. A tile
whose lossless JPEG crop would exceed the reader's crop limit (256 MiB of
working memory by default) also comes back as the whole stored JPEG, unchanged
and standalone, with `tile_region` locating the tile; `jpegtran -crop` or a
decode followed by slicing extracts it.

A tile read failure raises an exception naming the failing input; a failed
batch never returns a partial list. Reader resource limits still apply. Encoded results
are additionally capped at the default 256 MiB batch-chunk limit; request
fewer tiles when needed. Tile planning observes the reader's region and
metadata limits. Payloads stay valid after the slide object is destroyed.

Open, compressed reads, location queries and `assemble` release the GIL.
Open one `Slide` per DataLoader worker and prefer multiprocessing `spawn`;
inherited handles are rejected after `fork`. There is no NumPy or PyTorch dependency in this reader wheel.
The Python API is experimental; no wheel publication is part of this change.

When a local `j2k` wheel and NumPy are installed, the tests also exercise TIFF
color hints and table splicing through the tensor decoder. With a Metal build
and PyTorch MPS, they decode a DICOM JPEG 2000 tile directly into an MPS tensor.
Set `WSI_REQUIRE_J2K=1` to require the optional codec integration tests.

## Tile byte locations

`tile_location(col, row, level=0, scene=0, series=0, plane=(0,0,0))` reports
where a tile's encoded bytes are stored, from the file's index, without
decoding the tile (a few formats read a handful of marker bytes). `level_tile_locations(level=0, scene=0, series=0, plane=(0,0,0))` does
the same for a whole level in one call and returns `tile_width`,
`tile_height`, `tiles_across`, `tiles_down`, and `tiles`, a tuple of
`(col, row, location)` in row-major order. It is bounded by the reader's
tile-index limit; one unreadable location fails the whole call and names the
tile. Formats without location support raise `ValueError`.

A `TileLocation` has a `kind`:

- `stored`: `tile_size` is the tile clipped at the level edge, and `units`
  lists the stored encoded images it is made from. Most formats use one unit
  per tile; a tile of a strip-based TIFF lists every strip it covers.
- `absent`: the file stores no pixels for the tile (a sparse or empty TIFF
  tile, a missing DICOM frame, VSI background). Readers paint background;
  Zarr leaves the chunk out and uses `fill_value`.
- `not_addressable`: no byte range holds the tile's pixels. `reason` is
  `synthetic_level` (with `base_level` and `factor`), `resampled`,
  `codestream_window` (a window of one large JPEG 2000 codestream), or
  `not_stored`.

An `EncodedUnit` has `path` (the slide file as the reader opened it),
`segments` (`(offset, length)` byte ranges in stream order), `framing`,
`prefix` and `suffix` bytes, `codec`, `stored_size` (including edge padding),
`source_region` (the part of the stored image the tile uses) and
`tile_region` (where that part lands in the tile). `codec` is a dictionary
with `compression` (the same names as `CompressedTile.codec`),
`bits_per_sample`, `significant_bits` (the low bits of each sample that
carry it, or `None` when all do), `samples_per_pixel`,
`photometric_interpretation`,
`planar_configuration` (`chunky` or `planar`), `channel_order` (`rgb`, or
`bgr` for CZI's B, G, R samples), `predictor` (`none`,
`horizontal` or `floating_point`), `byte_order` (`little`, `big` or `None`),
`chroma_subsampling` and `jpeg_tables` (bytes or `None`).

| `framing` | The bytes at `segments` are |
| --- | --- |
| `standalone` | A complete stream: a JPEG with its tables, a JPEG 2000 codestream, a JPEG XR image. |
| `jpeg_tables` | A TIFF abbreviated JPEG from its SOI marker. Its tables are `codec["jpeg_tables"]`; pass them as `tables=` to `imagecodecs.jpeg8_decode` or to the `imagecodecs_jpeg` numcodecs codec. |
| `bare_payload` | A codec payload without framing, such as TIFF LZW, Deflate, Zstandard or uncompressed data. Decode it with `codec`, including its predictor and byte order. |
| `prefixed` | Part of a larger JPEG (NDPI). The standalone stream is `prefix`, the segments, then `suffix`. With the usual `suffix` `b"\xff\xd9"`, the `imagecodecs_jpeg` codec's `header=prefix` rebuilds it. |

`unit.assemble(parts)` takes the bytes of each segment, read in order, and
returns the standalone stream. For a unit that covers its whole tile, that is
the same data `read_tiles_compressed` returns, with one exception: a
`CompressedTile` cannot carry a predictor, separate sample planes, a
non-native byte order, B, G, R sample order or masked high bits, so such bare
payloads are read as exact `none` samples instead. Subsampled (packed) YCbCr
payloads have locations but no raw read. `to_dict()` on a level, a
location or a unit returns plain ints, strings, bytes, lists and dictionaries;
only the bytes need encoding before `json.dumps`. Values a newer reader adds
are reported as `unknown`.

A kerchunk-style reference needs one byte range per chunk. This sketch uses
only the standard library and accepts 8-bit JPEG levels in which every stored
tile is one byte range holding one full-size encoded tile:

```python
import base64
import json
import wsi_rs

slide = wsi_rs.Slide.open("case.svs")
width, height = slide.levels()[0]["dimensions"]
locations = slide.level_tile_locations(level=0)
tile = (locations.tile_width, locations.tile_height)
refs, codec = {}, None
for col, row, location in locations.tiles:
    if location.kind == "absent":
        continue  # zarr fills missing chunks with fill_value
    unit = location.units[0] if location.kind == "stored" and len(location.units) == 1 else None
    if (unit is None or len(unit.segments) != 1 or unit.framing not in ("standalone", "jpeg_tables")
            or unit.stored_size != tile or unit.source_region != (0, 0, *location.tile_size)
            or unit.tile_region != unit.source_region):
        raise ValueError(f"tile {col},{row} is not one stored chunk: {location.to_dict()}")
    if codec not in (None, unit.codec):
        raise ValueError("tiles of this level use different codecs")
    codec = unit.codec
    offset, length = unit.segments[0]
    refs[f"0/{row}.{col}.0"] = [unit.path, offset, length]
if codec is None or codec["compression"] != "jpeg" or codec["bits_per_sample"] != 8:
    raise ValueError("this sketch handles 8-bit JPEG levels only")
compressor = {"id": "imagecodecs_jpeg"}
if codec["jpeg_tables"] is not None:
    compressor["tables"] = base64.b64encode(codec["jpeg_tables"]).decode()
if codec["photometric_interpretation"] == "rgb":
    compressor["colorspace_jpeg"] = "RGB"  # RGB components, no YCbCr transform
samples = codec["samples_per_pixel"]
refs[".zgroup"] = json.dumps({"zarr_format": 2})
refs["0/.zarray"] = json.dumps({
    "zarr_format": 2, "shape": [height, width, samples], "chunks": [tile[1], tile[0], samples],
    "dtype": "|u1", "compressor": compressor, "fill_value": 0, "order": "C", "filters": None,
})
reference = {"version": 1, "refs": refs}
```

Edge tiles of tiled TIFFs are stored padded to the full tile size, so they
map to full chunks. Units cut from a larger image, multi-segment units and
tiles built from several units need `read_tiles_compressed` or a reader that
can crop or join them.
