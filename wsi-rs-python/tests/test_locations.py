# SPDX-License-Identifier: MIT OR Apache-2.0

"""Tile byte locations, checked against the bytes the reader returns."""

import base64
import json
import multiprocessing
import os
from pathlib import Path
import tempfile
import unittest
import zlib

import wsi_rs
from test_reader import JPEG, J2K, ROOT, SECOND_JPEG, separate_tables, write_tiff, write_tiff_payloads

PIXELS = bytes((index * 7) % 256 for index in range(16 * 16 * 3))


def horizontal_difference(pixels, width, samples):
    """TIFF predictor 2 applied to 8-bit chunky rows."""
    out = bytearray(pixels)
    stride = width * samples
    for start in range(0, len(pixels), stride):
        for index in range(start + stride - 1, start + samples - 1, -1):
            out[index] = (pixels[index] - pixels[index - samples]) % 256
    return bytes(out)


def read_segments(unit):
    parts = []
    with open(unit.path, "rb") as file:
        for offset, length in unit.segments:
            file.seek(offset)
            parts.append(file.read(length))
    return parts


def write_strip_tiff(path, width, height, rows_per_strip):
    """An uncompressed 8-bit RGB TIFF stored in strips; returns its pixels."""
    pixels = bytes((index * 11) % 251 for index in range(width * height * 3))
    stride = width * 3
    strips = [pixels[row * stride:(row + rows_per_strip) * stride] for row in range(0, height, rows_per_strip)]
    tags = {256: (4, [width]), 257: (4, [height]), 278: (4, [rows_per_strip])}
    write_tiff_payloads(path, strips, tags, compression=1, photometric=2, offsets_tag=273, counts_tag=279)
    return pixels


def place_uncompressed_units(location):
    """Rebuilds an uncompressed 8-bit chunky tile from its units' bytes."""
    tile_width, tile_height = location.tile_size
    samples = location.units[0].codec["samples_per_pixel"]
    tile = bytearray(tile_width * tile_height * samples)
    for unit in location.units:
        payload = b"".join(read_segments(unit))
        stored_width, _ = unit.stored_size
        sx, sy, width, height = unit.source_region
        tx, ty, _, _ = unit.tile_region
        for row in range(height):
            source = ((sy + row) * stored_width + sx) * samples
            target = ((ty + row) * tile_width + tx) * samples
            tile[target:target + width * samples] = payload[source:source + width * samples]
    return bytes(tile)


def kerchunk_refs(slide, level=0):
    """The Python README's reference builder: one byte range per chunk."""
    width, height = slide.levels()[level]["dimensions"]
    locations = slide.level_tile_locations(level=level)
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
        compressor["colorspace_jpeg"] = "RGB"
    samples = codec["samples_per_pixel"]
    refs[".zgroup"] = json.dumps({"zarr_format": 2})
    refs["0/.zarray"] = json.dumps({
        "zarr_format": 2, "shape": [height, width, samples], "chunks": [tile[1], tile[0], samples],
        "dtype": "|u1", "compressor": compressor, "fill_value": 0, "order": "C", "filters": None,
    })
    return {"version": 1, "refs": refs}


def inherited_location_errors(slide, queue):
    errors = []
    for call in (lambda: slide.tile_location(0, 0), lambda: slide.level_tile_locations()):
        try:
            call()
        except Exception as error:
            errors.append(str(error))
        else:
            errors.append("inherited slide unexpectedly accepted")
    queue.put(errors)


class LocationTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.path = Path(self.tmp.name) / "tiles.tif"

    def assert_plain(self, value):
        """Only ints, strings, bytes, None, lists and string-keyed dicts."""
        if isinstance(value, dict):
            for key, item in value.items():
                self.assertIsInstance(key, str)
                self.assert_plain(item)
        elif isinstance(value, list):
            for item in value:
                self.assert_plain(item)
        else:
            self.assertIsInstance(value, (int, str, bytes, type(None)))

    def assert_round_trip(self, slide, col, row, unit):
        tile = slide.read_tiles_compressed([(col, row)])[0]
        self.assertEqual(unit.assemble(read_segments(unit)), tile.data)
        return tile

    def test_standalone_jpeg_tiles_round_trip_through_their_byte_ranges(self):
        write_tiff(self.path, [JPEG, SECOND_JPEG])
        slide = wsi_rs.Slide(self.path)
        location = slide.tile_location(1, 0)
        self.assertEqual((location.kind, location.reason, location.base_level, location.factor), ("stored", None, None, None))
        self.assertEqual(location.tile_size, (16, 16))
        (unit,) = location.units
        self.assertTrue(os.path.samefile(unit.path, self.path))
        self.assertEqual((unit.framing, unit.prefix, unit.suffix), ("standalone", b"", b""))
        self.assertEqual(read_segments(unit), [SECOND_JPEG])
        self.assertEqual((unit.stored_size, unit.source_region, unit.tile_region), ((16, 16), (0, 0, 16, 16), (0, 0, 16, 16)))
        codec = unit.codec
        self.assertEqual(
            {key: codec[key] for key in ("compression", "bits_per_sample", "samples_per_pixel", "photometric_interpretation", "planar_configuration", "predictor", "jpeg_tables")},
            {"compression": "jpeg", "bits_per_sample": 8, "samples_per_pixel": 3, "photometric_interpretation": "ybr_full_422",
             "planar_configuration": "chunky", "predictor": "none", "jpeg_tables": None},
        )
        tile = self.assert_round_trip(slide, 1, 0, unit)
        self.assertEqual((tile.data, tile.tile_region), (SECOND_JPEG, (0, 0, 16, 16)))
        for target, name in ((location, "kind"), (unit, "framing")):
            with self.assertRaises(AttributeError):
                setattr(target, name, "absent")

    def test_abbreviated_jpeg_keeps_its_stored_stream_and_shared_tables(self):
        tables, abbreviated = separate_tables(JPEG)
        write_tiff(self.path, [abbreviated], tables=tables)
        slide = wsi_rs.Slide(self.path)
        (unit,) = slide.tile_location(0, 0).units
        self.assertEqual((unit.framing, unit.codec["jpeg_tables"]), ("jpeg_tables", tables))
        self.assertEqual(read_segments(unit), [abbreviated])
        self.assert_round_trip(slide, 0, 0, unit)

    def test_jpeg2000_and_bare_payload_tiles_describe_their_codecs(self):
        write_tiff(self.path, [J2K], compression=33004, photometric=2, tile_size=(16, 12))
        slide = wsi_rs.Slide(self.path)
        (unit,) = slide.tile_location(0, 0).units
        self.assertEqual((unit.framing, unit.codec["compression"], read_segments(unit)), ("standalone", "jpeg2000", [J2K]))
        self.assert_round_trip(slide, 0, 0, unit)

        deflated = zlib.compress(PIXELS)
        write_tiff(self.path, [deflated, deflated], compression=8, photometric=2)
        slide = wsi_rs.Slide(self.path)
        (unit,) = slide.tile_location(1, 0).units
        codec = unit.codec
        self.assertEqual((unit.framing, codec["compression"], codec["predictor"], codec["photometric_interpretation"]),
                         ("bare_payload", "deflate", "none", "rgb"))
        self.assertEqual(self.assert_round_trip(slide, 1, 0, unit).codec, "deflate")

        # A compressed tile cannot carry a predictor, so its raw read is the
        # decoded samples while the location keeps the stored stream.
        differenced = horizontal_difference(PIXELS, 16, 3)
        write_tiff(self.path, [zlib.compress(differenced)] * 2, compression=8, photometric=2, extra_tags={317: (3, [2])})
        slide = wsi_rs.Slide(self.path)
        (unit,) = slide.tile_location(1, 0).units
        self.assertEqual((unit.framing, unit.codec["compression"], unit.codec["predictor"]), ("bare_payload", "deflate", "horizontal"))
        self.assertEqual(zlib.decompress(unit.assemble(read_segments(unit))), differenced)
        tile = slide.read_tiles_compressed([(1, 0)])[0]
        self.assertEqual((tile.codec, tile.data), ("none", PIXELS))

        write_tiff(self.path, [PIXELS], compression=1, photometric=2)
        slide = wsi_rs.Slide(self.path)
        (unit,) = slide.tile_location(0, 0).units
        self.assertEqual((unit.framing, unit.codec["compression"], read_segments(unit)), ("bare_payload", "none", [PIXELS]))
        self.assertEqual(self.assert_round_trip(slide, 0, 0, unit).codec, "none")

    def test_strip_tiles_list_every_strip_they_use(self):
        pixels = write_strip_tiff(self.path, 300, 10, 4)
        slide = wsi_rs.Slide(self.path)
        location = slide.tile_location(1, 0)
        self.assertEqual((location.kind, location.tile_size, len(location.units)), ("stored", (44, 10), 3))
        for unit in location.units:
            self.assertEqual((unit.framing, unit.codec["compression"]), ("bare_payload", "none"))
        tile = slide.read_tiles_compressed([(1, 0)])[0]
        self.assertEqual((tile.codec, tile.width, tile.height), ("none", 44, 10))
        expected = b"".join(pixels[(row * 300 + 256) * 3:(row * 300 + 300) * 3] for row in range(10))
        self.assertEqual(place_uncompressed_units(location), expected)
        self.assertEqual(tile.data, expected)

    def test_level_lists_every_cell_with_absent_tiles_and_plain_dicts(self):
        write_tiff(self.path, [JPEG, b""])
        slide = wsi_rs.Slide(self.path)
        level = slide.level_tile_locations()
        self.assertEqual((level.tile_width, level.tile_height, level.tiles_across, level.tiles_down), (16, 16, 2, 1))
        self.assertIsInstance(level.tiles, tuple)
        self.assertEqual([(col, row, location.kind) for col, row, location in level.tiles], [(0, 0, "stored"), (1, 0, "absent")])
        absent = level.tiles[1][2]
        self.assertEqual((absent.units, absent.tile_size, absent.reason), ((), None, None))
        self.assertEqual(absent.to_dict(), {"kind": "absent"})
        self.assertEqual(slide.tile_location(1, 0).kind, "absent")

        plain = level.to_dict()
        self.assert_plain(plain)
        self.assertEqual(plain["tiles"][1], [1, 0, {"kind": "absent"}])
        stored = plain["tiles"][0][2]
        self.assertEqual(stored, level.tiles[0][2].to_dict())
        self.assertEqual(stored["tile_size"], [16, 16])
        (unit,) = stored["units"]
        self.assertEqual((unit["framing"], unit["segments"], unit["prefix"]), ("standalone", [list(level.tiles[0][2].units[0].segments[0])], b""))
        self.assertEqual((unit["source_region"], unit["tile_region"], unit["stored_size"]), ([0, 0, 16, 16], [0, 0, 16, 16], [16, 16]))
        self.assertEqual(unit["codec"], level.tiles[0][2].units[0].codec)
        json.dumps(plain, default=lambda value: base64.b64encode(value).decode())
        with self.assertRaises(AttributeError):
            level.tiles = ()

    def test_kerchunk_style_references_point_at_the_tile_bytes(self):
        tables, abbreviated = separate_tables(JPEG)
        _, second = separate_tables(SECOND_JPEG)
        write_tiff(self.path, [abbreviated, second], tables=tables)
        slide = wsi_rs.Slide(self.path)
        reference = kerchunk_refs(slide)
        json.dumps(reference)
        zarray = json.loads(reference["refs"]["0/.zarray"])
        self.assertEqual((zarray["shape"], zarray["chunks"]), ([16, 32, 3], [16, 16, 3]))
        self.assertEqual(base64.b64decode(zarray["compressor"]["tables"]), tables)
        data = self.path.read_bytes()
        for key, stream in (("0/0.0.0", abbreviated), ("0/0.1.0", second)):
            path, offset, length = reference["refs"][key]
            self.assertTrue(os.path.samefile(path, self.path))
            self.assertEqual(data[offset:offset + length], stream)

    def test_dicom_czi_and_jpeg2000_files_round_trip_every_tile(self):
        fixtures = {
            "public_dicom/progressive-sof2.dcm": ("jpeg", (1, 1)),
            "dicom_12bit/mono2-extended.dcm": ("jpeg", (3, 2)),
            "jxr/rgb.czi": ("jpegxr", (1, 1)),
            "jp2k/rgb_rct.j2k": ("jpeg2000", (1, 1)),
        }
        for name, (compression, grid) in fixtures.items():
            with self.subTest(fixture=name):
                slide = wsi_rs.Slide(ROOT / "tests/fixtures" / name)
                level = slide.level_tile_locations()
                self.assertEqual((level.tiles_across, level.tiles_down), grid)
                self.assert_plain(level.to_dict())
                for col, row, location in level.tiles:
                    (unit,) = location.units
                    self.assertEqual((location.kind, unit.framing, unit.codec["compression"]), ("stored", "standalone", compression))
                    self.assertEqual(unit.tile_region, (0, 0, *location.tile_size))
                    tile = self.assert_round_trip(slide, col, row, unit)
                    self.assertEqual(tile.codec, compression)

    def test_assemble_checks_its_segments_and_outlives_the_slide(self):
        write_tiff(self.path, [JPEG, SECOND_JPEG])
        slide = wsi_rs.Slide(self.path)
        (unit,) = slide.tile_location(0, 0).units
        del slide
        self.assertEqual(unit.assemble([bytearray(JPEG)]), JPEG)
        with self.assertRaisesRegex(ValueError, "1 segments, but 2"):
            unit.assemble([JPEG, JPEG])
        with self.assertRaisesRegex(ValueError, "segment 0"):
            unit.assemble([JPEG[:-1]])
        with self.assertRaisesRegex(TypeError, "segment 0"):
            unit.assemble(["not bytes"])

    def test_invalid_selections_are_explicit(self):
        write_tiff(self.path, [JPEG, SECOND_JPEG])
        slide = wsi_rs.Slide(self.path)
        for kwargs in ({"scene": 1}, {"series": 1}, {"level": 4}, {"plane": (1, 0, 0)}):
            with self.subTest(kwargs=kwargs):
                with self.assertRaises(ValueError):
                    slide.tile_location(0, 0, **kwargs)
                with self.assertRaises(ValueError):
                    slide.level_tile_locations(**kwargs)
        with self.assertRaises(ValueError):
            slide.tile_location(9, 0)

    def test_forked_handles_are_rejected(self):
        if "fork" not in multiprocessing.get_all_start_methods():
            self.skipTest("requires the fork start method")
        write_tiff(self.path, [JPEG, SECOND_JPEG])
        ctx = multiprocessing.get_context("fork")
        queue = ctx.Queue()
        process = ctx.Process(target=inherited_location_errors, args=(wsi_rs.Slide(self.path), queue))
        process.start()
        try:
            errors = queue.get(timeout=10)
        finally:
            process.join(timeout=10)
            if process.is_alive():
                process.terminate()
                process.join()
            queue.close()
        self.assertEqual(len(errors), 2)
        for error in errors:
            self.assertIn("fork", error)


if __name__ == "__main__":
    unittest.main()
