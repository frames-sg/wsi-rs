# wsi-rs for Python

> Whole-slide image metadata, tile geometry and compressed tile bytes

[![PyPI](https://img.shields.io/pypi/v/wsi-rs.svg)](https://pypi.org/project/wsi-rs/)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-orange.svg)](https://github.com/frames-sg/wsi-rs#license)

`wsi_rs.Slide` opens any slide [wsi-rs](https://github.com/frames-sg/wsi-rs) reads and returns its
metadata, tile grid and encoded tile bytes. It does not decode pixels: pass the bytes to the
[`j2k`](https://github.com/frames-sg/j2k) extension for JPEG and JPEG 2000 tensors, or to any
decoder for the tile's codec. The wheel has no NumPy or PyTorch dependency.

```sh
python -m pip install wsi-rs
```

Wheels cover Python 3.10 and later on Linux x86_64 and aarch64 (glibc 2.28 or newer), macOS arm64
and x86_64, and Windows x64.

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

Arguments after `*` are keyword-only.

- `Slide(path)`, `Slide.open(path)`: open a slide from a string or path-like object.
- `metadata`: scenes, series, `(z, c, t)` axes, channels, levels, associated-image sizes and
  properties.
- `levels(*, scene=0, series=0)`: each level's dimensions, downsample and tile grid. Regular and
  virtual grids report tile size and grid size; irregular grids report advance, extra tiles and tile
  count.
- `tiles_for_region(region, *, level=0, scene=0, series=0)`: the tiles covering
  `(x, y, width, height)` in level pixels, in the reader's order. Each hit has `coords`
  (`(col, row)`) and `offset`, the tile's position relative to the region origin, which is
  fractional for irregular layouts. Cropping and assembly are left to the caller.
- `read_tiles_compressed(coords, *, level=0, scene=0, series=0, plane=(0, 0, 0))`: one
  `CompressedTile` per coordinate, in request order, duplicates included.
- `tile_location(col, row, *, ...)` and `level_tile_locations(*, ...)`: where tiles' encoded bytes
  are stored in the slide's files. See [Tile Locations](#tile-locations).

## Compressed Tiles

A `CompressedTile` has `data` (bytes), `coords`, `width`, `height`, `codec`, `bits_allocated`,
`significant_bits` (the low bits of each sample in use, or `None` when all are),
`samples_per_pixel`, `photometric_interpretation`, `color_transform` and `tile_region`.

TIFF `JPEGTables` are merged into each JPEG tile and NDPI tiles are cut from their level JPEG, so a
JPEG tile's `data` decodes on its own. `color_transform` is `auto`, `ycbcr` or `none` for JPEG and
`auto` otherwise.

`tile_region` is `(x, y, width, height)` of the tile inside the encoded image. It is smaller than
the image when the stored image is larger than the tile:

- an edge tile in a padded TIFF tile or DICOM frame;
- a tile cropped losslessly from a larger JPEG whose block (MCU) grid does not start at the tile
  origin, so the image starts at the previous block boundary;
- a tile inside a JPEG 2000 or JPEG XR image, which is returned whole;
- a tile whose lossless JPEG crop does not fit the crop limit (256 MiB of working memory by
  default), which is returned as the whole stored JPEG. `jpegtran -crop`, or decoding and slicing,
  extracts it.

A failed read raises an exception naming the failing input; a batch never returns a partial list, a
black tile or decoded pixels in place of an error. The reader's resource limits apply, and one call
returns at most 256 MiB of encoded data, so request fewer tiles if a batch exceeds it. Payloads stay
valid after the slide object is deleted.

## Tile Locations

`tile_location(col, row, *, level=0, scene=0, series=0, plane=(0, 0, 0))` reports where a tile's
encoded bytes are stored, from the file's index, without decoding the tile. A few formats read a
handful of marker bytes to place a range exactly.
`level_tile_locations(*, level=0, scene=0, series=0, plane=(0, 0, 0))` does this for a whole level
and returns `tile_width`, `tile_height`, `tiles_across`, `tiles_down` and `tiles`, a tuple of
`(col, row, location)` in row-major order. It is bounded by the reader's tile-index limit, and one
unreadable location fails the call with an error naming the tile. Formats without location support
raise `ValueError`.

A `TileLocation` has a `kind`:

- `stored`: `tile_size` is the tile clipped at the level edge, and `units` lists the encoded images
  it is made from. Most formats use one unit per tile; a tile of a strip-based TIFF lists every
  strip it covers.
- `absent`: the file stores no pixels for the tile, such as a sparse or empty TIFF tile, a missing
  DICOM frame or VSI background. Readers paint background; Zarr omits the chunk and uses
  `fill_value`.
- `not_addressable`: no byte range holds the tile's pixels. `reason` is `synthetic_level` (with
  `base_level` and `factor`), `resampled`, `codestream_window` (a window of one large JPEG 2000
  codestream) or `not_stored`.

An `EncodedUnit` has:

- `path`: the slide file as the reader opened it.
- `segments`: `(offset, length)` byte ranges in stream order.
- `framing`, `prefix` and `suffix`: how the segments form a stream (table below).
- `stored_size`: the encoded image size, including edge padding.
- `source_region`: the part of the stored image the tile uses. `tile_region`: where that part lands
  in the tile.
- `codec`: a dictionary with `compression` (the names `CompressedTile.codec` uses),
  `bits_per_sample`, `significant_bits`, `samples_per_pixel`, `photometric_interpretation`,
  `planar_configuration` (`chunky` or `planar`), `channel_order` (`rgb`, or `bgr` for CZI),
  `predictor` (`none`, `horizontal` or `floating_point`), `byte_order` (`little`, `big` or `None`),
  `chroma_subsampling` and `jpeg_tables` (bytes or `None`).

| `framing`      | The bytes at `segments` are                                                                                                                                                            |
| -------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `standalone`   | A complete stream: a JPEG with its tables, a JPEG 2000 codestream, a JPEG XR image.                                                                                                    |
| `jpeg_tables`  | A TIFF abbreviated JPEG from its SOI marker. Its tables are `codec["jpeg_tables"]`; pass them as `tables=` to `imagecodecs.jpeg8_decode` or to the `imagecodecs_jpeg` numcodecs codec. |
| `bare_payload` | A codec payload without framing, such as TIFF LZW, Deflate, Zstandard or uncompressed data. Decode it with `codec`, including its predictor and byte order.                            |
| `prefixed`     | Part of a larger JPEG (NDPI). The stream is `prefix`, the segments, then `suffix`. With the usual `suffix` `b"\xff\xd9"`, the `imagecodecs_jpeg` codec's `header=prefix` rebuilds it.  |

`unit.assemble(parts)` takes the bytes of each segment, in order, and returns a standalone stream.
For a unit that covers its whole tile, that is the data `read_tiles_compressed` returns, except for
bare payloads a `CompressedTile` cannot describe (a predictor, separate sample planes, non-native
byte order, B, G, R order or masked high bits), which `read_tiles_compressed` returns as decoded
`none` samples. Subsampled YCbCr payloads have locations but no compressed read.

`to_dict()` on a level, location or unit returns plain ints, strings, bytes, lists and dictionaries;
only the bytes need encoding before `json.dumps`. Values added by a newer reader are reported as
`unknown`.

## Kerchunk References

A kerchunk reference maps each Zarr chunk to one byte range. This sketch uses only the standard
library and handles 8-bit JPEG levels in which every stored tile is one byte range holding one
full-size tile:

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

Tiled TIFFs store edge tiles padded to the full tile size, so they map to full chunks. Units cut
from a larger image, multi-segment units and tiles joined from several units need
`read_tiles_compressed` or a reader that can crop or join them.

## Threads and Processes

Opening, region planning, compressed reads, location queries and `assemble` release the GIL. Open
one `Slide` per DataLoader worker and prefer the `spawn` start method; every method of a slide
inherited across `fork` raises `RuntimeError`.

## Building From Source

```sh
uvx maturin build --manifest-path wsi-rs-python/Cargo.toml --release --out target/python-wheels
python -m pip install --no-deps --no-index --find-links target/python-wheels wsi-rs
python -m unittest discover -s wsi-rs-python/tests -v
```

With a local `j2k` wheel and NumPy installed, the tests also decode tiles through `j2k`, and with a
Metal build and PyTorch MPS, they decode a DICOM JPEG 2000 tile into an MPS tensor. Set
`WSI_REQUIRE_J2K=1` to fail instead of skipping when `j2k` is missing.

---

Part of [wsi-rs](https://github.com/frames-sg/wsi-rs). See the
[Rust API documentation](https://docs.rs/wsi-rs).
