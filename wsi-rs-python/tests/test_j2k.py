# SPDX-License-Identifier: MIT OR Apache-2.0

"""Optional interoperability tests against a separately installed j2k wheel."""

import os
from pathlib import Path
import tempfile
import unittest

import wsi_rs
from test_reader import ROOT, separate_tables, write_tiff

try:
    import j2k
    import numpy as np
except ImportError:
    j2k = None

if os.environ.get("WSI_REQUIRE_J2K") == "1" and j2k is None:
    raise RuntimeError("The j2k wheel and NumPy are required for interoperability tests")


@unittest.skipIf(j2k is None, "requires the separate j2k wheel and NumPy")
class J2kInteropTests(unittest.TestCase):
    def test_tiff_photometric_hint_reaches_the_tensor_decoder(self):
        pixels = bytes((i * 37) % 256 for i in range(16 * 16 * 3))
        jpeg = j2k.encode_rgb(pixels, 16, 16, codec="jpeg", quality=90)
        tables, abbreviated = separate_tables(jpeg)
        decoder = j2k.Decoder(layout="nhwc")
        results = []
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "hint.tif"
            for photometric, hint in ((2, "none"), (6, "ycbcr")):
                write_tiff(path, [abbreviated], photometric=photometric, tables=tables)
                tile = wsi_rs.Slide(path).read_tiles_compressed([(0, 0)])[0]
                self.assertEqual(tile.color_transform, hint)
                batch = decoder.decode_batch([j2k.ImageRequest(tile.data, color_transform=tile.color_transform)])
                self.assertFalse(batch.errors or batch.group_errors)
                actual = batch.groups[0].tensor.to_numpy()
                reference = decoder.decode_batch([j2k.ImageRequest(jpeg, color_transform=hint)])
                np.testing.assert_array_equal(actual, reference.groups[0].tensor.to_numpy())
                results.append(actual)
        self.assertFalse(np.array_equal(*results), "RGB and YCbCr interpretations must differ")

    def test_dicom_jpeg2000_tile_decodes_directly_on_mps(self):
        try:
            import torch
        except ImportError:
            self.skipTest("requires PyTorch for MPS")
        if not getattr(j2k, "metal_enabled", False) or not torch.backends.mps.is_available():
            self.skipTest("requires the Metal j2k wheel and MPS hardware")
        tile = wsi_rs.Slide(ROOT / "tests/fixtures/public_dicom/ybr-rct.dcm").read_tiles_compressed([(0, 0)])[0]
        self.assertEqual((tile.codec, tile.color_transform), ("jpeg2000", "auto"))
        request = j2k.ImageRequest(tile.data, color_transform=tile.color_transform)
        batch = j2k.Decoder(device="mps").decode_batch([request])
        self.assertFalse(batch.errors or batch.group_errors)
        image = batch.groups[0].tensor.to_torch()[0]
        self.assertEqual(image.device.type, "mps")
        self.assertEqual(tuple(image.shape), (3, tile.height, tile.width))
        self.assertTrue(torch.equal(image.cpu(), j2k.Decoder().decode(tile.data).to_torch()))
