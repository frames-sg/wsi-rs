# Public DICOM codec fixtures

These CC0-1.0 fixtures contain unchanged compressed frames from the public
[OpenSlide testdata](https://openslide.cs.cmu.edu/download/openslide-testdata/DICOM/).
Regenerate them with `scripts/prepare-public-dicom-fixtures.py`; its source archive
hashes bind the input frames to the published OpenSlide files. The script uses
pydicom 3.0.2, Pillow 12.1.1 (libjpeg), and OpenJPEG 2.5.4.

| Fixture | Source member | Frame (zero-based) | Independent pixel decoder |
| --- | --- | --- | --- |
| progressive-sof2 | 3DHISTECH-1.zip / 000005.dcm | 0 | Pillow / libjpeg |
| ybr-rct | CMU-1-JP2K-RCT-v2.zip / DCM_0.dcm | 836 | OpenJPEG |
| ybr-ict | CMU-1-JP2K-ICT-v2.zip / img_0.dcm | 1194 | OpenJPEG |

Each DICOM container holds one frame with made-up metadata, identifiers and
pixel spacing. No patient, specimen, institution or private metadata and no
original identifiers are copied. The compressed pixels are copied unchanged.
The progressive frame shows background; the JPEG 2000 frames show tissue and
no slide label.

The source scanner stores progressive (SOF2) JPEG frames under the baseline
transfer syntax. The SOF2 fixture declares JPEG Full Progression
(1.2.840.10008.1.2.4.55), and a DICOM unit test relabels it as baseline to
cover that scanner's layout.

The `.ppm` files are decoded with independent decoders.
`public_dicom_frames_match_independent_decoder_pixels` compares every pixel:
lossless YBR_RCT must match exactly; progressive JPEG and irreversible YBR_ICT
must match within the decoder tolerances.

`ybr-rct.svcache` is a `.svcache` file built from the RCT fixture. The test
reads it with the source file absent and compares every pixel with the OpenJPEG
reference exactly. Rebuilding it changes the file's bytes (it records the source
path and modification time), so update its digest in the corpus manifest after
rebuilding. Rebuilding needs Cargo.
