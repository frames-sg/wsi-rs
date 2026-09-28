# 12-bit JPEG DICOM fixtures

These synthetic DICOM VL WSI fixtures test 12-bit JPEG decoding through the
native `Uint16` API. Regenerate them with

```sh
uv run --with pydicom==3.0.2 scripts/prepare-dicom-12bit-fixtures.py
```

The script requires `cjpeg` and `djpeg` from libjpeg-turbo 3.x; the committed
files were made with libjpeg-turbo 3.1.4.1. Pixels are synthetic and
deterministic, and the metadata contains no patient or specimen data.

Each fixture is a 40x28 `TILED_FULL` matrix of six 16x16 frames (3x2 tiles),
so the right and bottom tiles are partial. The DICOM attributes are
BitsAllocated 16, BitsStored 12, HighBit 11 and PixelRepresentation 0.

| Fixture | Transfer syntax | Photometric | Frame encoder |
| --- | --- | --- | --- |
| `mono2-extended` | JPEG Extended (1.2.840.10008.1.2.4.51) | MONOCHROME2 | `cjpeg -precision 12 -quality 92 -grayscale` |
| `mono2-progressive` | JPEG Full Progression (1.2.840.10008.1.2.4.55) | MONOCHROME2 | `cjpeg -precision 12 -quality 92 -grayscale -progressive` |
| `ybr422-extended` | JPEG Extended (1.2.840.10008.1.2.4.51) | YBR_FULL_422 | `cjpeg -precision 12 -quality 92 -sample 2x1,1x1,1x1` |
| `ybr422-progressive` | JPEG Full Progression (1.2.840.10008.1.2.4.55) | YBR_FULL_422 | `cjpeg -precision 12 -quality 92 -sample 2x1,1x1,1x1 -progressive` |

Each `.ppm` file is the independent reference for the whole matrix: every frame
decoded with `djpeg -rgb`, stitched, and cropped to 40x28. It is a binary P6 file
with maxval 4095 and big-endian 16-bit samples. Grayscale references carry
R=G=B, matching the reader's grayscale-to-RGB expansion. The
`tests/dicom_12bit.rs` integration test requires exact equality for regions,
single tiles and batched tiles.
