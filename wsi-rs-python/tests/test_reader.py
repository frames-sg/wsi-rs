# SPDX-License-Identifier: MIT OR Apache-2.0

"""Read-only Python contract tests using small existing codec fixtures."""

import multiprocessing
from collections.abc import Sequence
from pathlib import Path
import struct
import tempfile
import unittest

import wsi_rs

ROOT = Path(__file__).resolve().parents[2]
JPEG = (ROOT / "tests/fixtures/jpeg/baseline_420_16x16.jpg").read_bytes()
SECOND_JPEG = JPEG[:2] + b"\xff\xfe\x00\x08second" + JPEG[2:]
J2K = (ROOT / "tests/fixtures/jp2k/rgb_rct.j2k").read_bytes()


def write_tiff(path, payloads, *, compression=7, photometric=6, tile_size=(16, 16), tables=None):
    """A little-endian tiled TIFF, two tiles across with no pixel decoding."""
    width, height = tile_size
    tags = {
        256: (4, [width * len(payloads)]), 257: (4, [height]),
        258: (3, [8, 8, 8]), 259: (3, [compression]),
        262: (3, [photometric]), 277: (3, [3]), 284: (3, [1]),
        322: (4, [width]), 323: (4, [height]),
        324: (4, [0] * len(payloads)), 325: (4, [len(p) for p in payloads]),
    }
    if tables is not None:
        tags[347] = (7, tables)
    data = bytearray(struct.pack("<2sHIH", b"II", 42, 8, len(tags)))
    data.extend(bytes(12 * len(tags) + 4))
    for index, (tag, (kind, values)) in enumerate(sorted(tags.items())):
        encoded = bytes(values) if kind == 7 else struct.pack("<" + ("H" if kind == 3 else "I") * len(values), *values)
        entry = 10 + index * 12
        struct.pack_into("<HHI", data, entry, tag, kind, len(values))
        if len(encoded) <= 4:
            position = entry + 8
            data[position:position + len(encoded)] = encoded
        else:
            position = len(data)
            struct.pack_into("<I", data, entry + 8, position)
            data.extend(encoded)
        if tag == 324:
            offsets_position = position
    offsets = []
    for payload in payloads:
        offsets.append(len(data))
        data.extend(payload)
    struct.pack_into("<" + "I" * len(offsets), data, offsets_position, *offsets)
    path.write_bytes(data)


def separate_tables(jpeg):
    tables, image = bytearray(b"\xff\xd8"), bytearray(b"\xff\xd8")
    offset = 2
    while jpeg[offset:offset + 2] != b"\xff\xda":
        size = 2 + int.from_bytes(jpeg[offset + 2:offset + 4], "big")
        target = tables if jpeg[offset + 1] in (0xDB, 0xC4) else image
        target.extend(jpeg[offset:offset + size])
        offset += size
    tables.extend(b"\xff\xd9")
    image.extend(jpeg[offset:])
    return bytes(tables), bytes(image)


def read_in_worker(path):
    slide = wsi_rs.Slide(path)
    return slide.read_tiles_compressed([(0, 0)])[0].data


class GuardedCoordinates(Sequence):
    def __init__(self, size):
        self.size = size
        self.item_reads = 0

    def __len__(self):
        return self.size

    def __getitem__(self, index):
        self.item_reads += 1
        raise AssertionError("coordinates must be checked before extraction")


def inherited_slide_error(slide, queue):
    try:
        slide.read_tiles_compressed(GuardedCoordinates(1))
    except Exception as error:
        queue.put(str(error))
    else:
        queue.put("inherited slide unexpectedly accepted")


class ReaderTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.path = Path(self.tmp.name) / "tiles.tif"
        write_tiff(self.path, [JPEG, SECOND_JPEG])

    def test_metadata_grid_and_patch_tiles_use_level_coordinates(self):
        slide = wsi_rs.Slide.open(self.path)
        levels = slide.levels()
        self.assertEqual(levels[0]["dimensions"], (32, 16))
        self.assertEqual(levels[0]["downsample"], 1.0)
        self.assertEqual(levels[0]["tile_grid"], {"kind": "regular", "tile_size": (16, 16), "grid_size": (2, 1)})
        self.assertEqual(slide.metadata["scenes"][0]["series"][0]["levels"], levels)
        self.assertIsInstance(slide.properties, dict)
        hits = slide.tiles_for_region((10, 3, 18, 9))
        self.assertEqual([hit.coords for hit in hits], [(0, 0), (1, 0)])
        self.assertEqual([hit.offset for hit in hits], [(-10.0, -3.0), (6.0, -3.0)])
        self.assertEqual(slide.tiles_for_region((100, 100, 4, 4)), [])
        with self.assertRaises(ValueError):
            slide.tiles_for_region((0, 0, 0, 1))
        with self.assertRaises(ValueError):
            slide.tiles_for_region((0, 0, 2**32 - 1, 2**32 - 1))

    def test_compressed_reads_preserve_order_duplicates_hints_and_ownership(self):
        slide = wsi_rs.Slide(self.path)
        self.assertEqual(slide.read_tiles_compressed([]), [])
        tiles = slide.read_tiles_compressed([(1, 0), (0, 0), (1, 0)])
        self.assertEqual([tile.coords for tile in tiles], [(1, 0), (0, 0), (1, 0)])
        self.assertEqual([tile.data for tile in tiles], [SECOND_JPEG, JPEG, SECOND_JPEG])
        for tile in tiles:
            self.assertEqual((tile.width, tile.height, tile.bits_allocated, tile.samples_per_pixel), (16, 16, 8, 3))
            self.assertEqual((tile.codec, tile.photometric_interpretation, tile.color_transform), ("jpeg", "ybr_full_422", "ycbcr"))
        del slide
        self.assertEqual(tiles[0].data, SECOND_JPEG)
        with self.assertRaises(AttributeError):
            tiles[0].width = 99

    def test_jpeg_tables_are_spliced_and_jpeg2000_bytes_are_preserved(self):
        tables, abbreviated = separate_tables(JPEG)
        write_tiff(self.path, [abbreviated], tables=tables)
        tile = wsi_rs.Slide(self.path).read_tiles_compressed([(0, 0)])[0]
        self.assertIn(b"\xff\xdb", tile.data)
        self.assertIn(b"\xff\xc4", tile.data)
        self.assertTrue(tile.data.startswith(b"\xff\xd8") and tile.data.endswith(b"\xff\xd9"))
        write_tiff(self.path, [J2K], compression=33004, photometric=2, tile_size=(16, 12))
        tile = wsi_rs.Slide(self.path).read_tiles_compressed([(0, 0)])[0]
        self.assertEqual((tile.codec, tile.color_transform, tile.data), ("jpeg2000", "auto", J2K))

    def test_invalid_selections_and_batch_errors_are_explicit(self):
        slide = wsi_rs.Slide(self.path)
        for kwargs in ({"scene": 1}, {"series": 1}, {"level": 4}, {"plane": (1, 0, 0)}):
            with self.subTest(kwargs=kwargs), self.assertRaises(ValueError):
                slide.read_tiles_compressed([(0, 0)], **kwargs)
        with self.assertRaisesRegex(ValueError, "input 1"):
            slide.read_tiles_compressed([(0, 0), (9, 0)])
        with self.assertRaisesRegex(TypeError, "input 1"):
            slide.read_tiles_compressed([(0, 0), ("x", 0)])
        with self.assertRaises(OSError):
            wsi_rs.Slide(self.path.with_name("missing.tif"))

    def test_batch_metadata_limit_precedes_coordinate_extraction(self):
        coords = GuardedCoordinates(1_000_000)
        with self.assertRaisesRegex(ValueError, "metadata"):
            wsi_rs.Slide(self.path).read_tiles_compressed(coords)
        self.assertEqual(coords.item_reads, 0)

    def test_workers_open_their_own_slide_and_forked_handles_are_rejected(self):
        with multiprocessing.get_context("spawn").Pool(1) as pool:
            self.assertEqual(pool.apply(read_in_worker, (self.path,)), JPEG)
        if "fork" not in multiprocessing.get_all_start_methods():
            return
        ctx = multiprocessing.get_context("fork")
        queue = ctx.Queue()
        process = ctx.Process(target=inherited_slide_error, args=(wsi_rs.Slide(self.path), queue))
        process.start()
        try:
            self.assertIn("fork", queue.get(timeout=10))
        finally:
            process.join(timeout=10)
            if process.is_alive():
                process.terminate()
                process.join()
            queue.close()
        self.assertEqual(process.exitcode, 0)


if __name__ == "__main__":
    unittest.main()
