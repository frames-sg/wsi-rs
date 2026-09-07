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

The DICOM containers have synthetic allowlisted metadata, identifiers, one-frame
matrix geometry, and synthetic pixel spacing. Patient, specimen, institution,
private metadata and original identifiers are not copied. Compressed pixels are
not re-encoded. The reference images were visually reviewed: the progressive
frame is background, and the JPEG 2000 frames contain tissue without labels.

The scanner's source object mixes SOF0 and SOF2 JPEG frames under the baseline
transfer syntax. The derived SOF2 fixture correctly declares JPEG Full Progression
(1.2.840.10008.1.2.4.55). It tests the real process/container boundary, not varied
progressive color content; generated JPEG tests cover that separately.

The `.ppm` files are independent decoded references. The ordinary
`public_dicom_frames_match_independent_decoder_pixels` integration test compares
all pixels: lossless YBR_RCT is byte-exact, while progressive JPEG and irreversible
YBR_ICT use the established decoder tolerances. These small codec fixtures do not
replace full-slide geometry, pyramid, or performance coverage.

`ybr-rct.svcache` is a complete schema-4 cache made by the workspace's 0.7.0
candidate writer from a temporary copy of the deidentified RCT fixture. Its
source is removed before pixel validation, demonstrating that the complete
cache is independently readable. The same integration test compares all cache
pixels with the OpenJPEG PPM exactly. The cache retains its temporary source
path and modification time as diagnostic identity, so rebuilding changes the
container bytes and requires updating the corpus manifest digest; expected
image samples remain identical. The generator requires Cargo for this step.
