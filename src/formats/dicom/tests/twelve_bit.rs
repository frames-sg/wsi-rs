use super::fixtures::*;
use super::*;

fn mono_12bit_frame() -> Vec<u8> {
    twelve_bit_fixture_frames("mono2-extended").swap_remove(0)
}

fn first_tile_request() -> TileRequest {
    TileRequest::new(0usize, 0usize, 0u32, 0, 0)
}

fn open_error(path: &Path) -> String {
    match Slide::open(path) {
        Ok(_) => panic!("{} must be rejected at open", path.display()),
        Err(err) => err.to_string(),
    }
}

fn u16_samples(tile: &CpuTile) -> &[u16] {
    assert_eq!((tile.channels, &tile.color_space), (3, &ColorSpace::Rgb));
    tile.data.as_u16().expect("12-bit DICOM decodes to U16 RGB")
}

#[test]
fn twelve_bit_levels_and_associated_images_keep_their_sample_types() {
    let dir = tempfile::tempdir().unwrap();
    write_test_dicom(
        &dir.path().join("a-level.dcm"),
        TestDicomOptions::jpeg_12bit(mono_12bit_frame(), "MONOCHROME2"),
    );
    write_test_dicom(
        &dir.path().join("b-label.dcm"),
        TestDicomOptions {
            sop_instance_uid: "1.2.826.0.1.3680043.10.777.2",
            image_type: "ORIGINAL\\PRIMARY\\LABEL\\NONE",
            ..TestDicomOptions::jpeg_12bit(mono_12bit_frame(), "MONOCHROME2")
        },
    );
    // Overview cameras are often 8-bit even when the volume is 12-bit.
    write_test_dicom(
        &dir.path().join("c-overview.dcm"),
        TestDicomOptions {
            sop_instance_uid: "1.2.826.0.1.3680043.10.777.3",
            image_type: "ORIGINAL\\PRIMARY\\OVERVIEW\\NONE",
            transfer_syntax: uids::JPEG_BASELINE8_BIT,
            photometric_interpretation: "YBR_FULL_422",
            rows: 16,
            columns: 16,
            total_pixel_matrix_rows: 16,
            total_pixel_matrix_columns: 16,
            pixel_data: TestPixelData::Encapsulated(encode_test_jpeg_rgb(16, 16, 9)),
            ..TestDicomOptions::native(Vec::new())
        },
    );

    let slide = Slide::open(dir.path()).expect("open 12-bit DICOM series");
    let dataset = slide.dataset();
    assert_eq!(dataset.scenes[0].series[0].sample_type, SampleType::Uint16);
    assert_eq!(
        dataset.associated_images["label"].sample_type,
        SampleType::Uint16
    );
    assert_eq!(
        dataset.associated_images["macro"].sample_type,
        SampleType::Uint8
    );

    let tile = slide.read_tile(&first_tile_request()).expect("12-bit tile");
    let label = slide.read_associated("label").expect("12-bit label");
    assert_eq!((label.width, label.height), (16, 16));
    assert!(u16_samples(&tile).iter().any(|&sample| sample > 255));
    assert_eq!(u16_samples(&label), u16_samples(&tile));
    let overview = slide.read_associated("macro").expect("8-bit overview");
    assert!(overview.data.as_u8().is_some(), "8-bit overview stays U8");
}

#[test]
fn rejects_pyramids_that_mix_8bit_and_12bit_levels() {
    let dir = tempfile::tempdir().unwrap();
    write_test_dicom(
        &dir.path().join("a-level0.dcm"),
        TestDicomOptions {
            transfer_syntax: uids::JPEG_BASELINE8_BIT,
            photometric_interpretation: "YBR_FULL_422",
            rows: 16,
            columns: 16,
            total_pixel_matrix_rows: 32,
            total_pixel_matrix_columns: 32,
            number_of_frames: 4,
            pixel_data: TestPixelData::EncapsulatedFrames(
                (0..4)
                    .map(|seed| encode_test_jpeg_rgb(16, 16, seed))
                    .collect(),
            ),
            ..TestDicomOptions::native(Vec::new())
        },
    );
    write_test_dicom(
        &dir.path().join("b-level1.dcm"),
        TestDicomOptions {
            sop_instance_uid: "1.2.826.0.1.3680043.10.777.2",
            image_type: "DERIVED\\PRIMARY\\VOLUME\\RESAMPLED",
            ..TestDicomOptions::jpeg_12bit(mono_12bit_frame(), "MONOCHROME2")
        },
    );

    // Parse the directory with the DICOM reader itself: on Windows, other
    // format probes fail to open a directory first and mask its error.
    let error = match DicomSlide::parse(dir.path()) {
        Ok(_) => panic!("a mixed-depth pyramid must be rejected"),
        Err(err) => err.to_string(),
    };
    assert!(
        error.contains(
            "DICOM series mixes 8-bit and 12-bit pyramid images \
             (1.2.826.0.1.3680043.10.777.1 vs. 1.2.826.0.1.3680043.10.777.2)"
        ),
        "{error}"
    );
}

#[test]
fn twelve_bit_samples_are_rejected_outside_lossy_jpeg_syntaxes() {
    for (file_name, transfer_syntax, pixel_data) in [
        (
            "native.dcm",
            uids::EXPLICIT_VR_LITTLE_ENDIAN,
            TestPixelData::Native(vec![0; 16 * 16 * 2]),
        ),
        (
            "rle.dcm",
            RLE_TRANSFER_SYNTAX,
            TestPixelData::Encapsulated(vec![0; 64]),
        ),
        (
            "jpeg2000.dcm",
            uids::JPEG2000,
            TestPixelData::Encapsulated(vec![0xFF, 0x4F, 0x00, 0xFF, 0xD9]),
        ),
        (
            "htj2k.dcm",
            HTJ2K_TRANSFER_SYNTAX,
            TestPixelData::Encapsulated(vec![0xFF, 0x4F, 0x00, 0xFF, 0xD9]),
        ),
        (
            "baseline.dcm",
            uids::JPEG_BASELINE8_BIT,
            TestPixelData::Encapsulated(mono_12bit_frame()),
        ),
        (
            "lossless.dcm",
            uids::JPEG_LOSSLESS_SV1,
            TestPixelData::Encapsulated(mono_12bit_frame()),
        ),
    ] {
        // Separate directories keep same-series siblings out of each open.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(file_name);
        write_test_dicom(
            &path,
            TestDicomOptions {
                transfer_syntax,
                pixel_data,
                ..TestDicomOptions::jpeg_12bit(Vec::new(), "MONOCHROME2")
            },
        );
        assert_eq!(
            open_error(&path),
            format!(
                "invalid slide {}: Attribute BitsAllocated value 16 != 8",
                path.display()
            ),
            "{file_name}"
        );
    }
}

#[test]
fn twelve_bit_jpeg_requires_bits_stored_12_and_high_bit_11() {
    for (bits_stored, high_bit, message) in [
        (16, 15, "Attribute BitsStored value 16 != 12"),
        (8, 7, "Attribute BitsStored value 8 != 12"),
        (12, 15, "Attribute HighBit value 15 != 11"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join(format!("stored-{bits_stored}-high-{high_bit}.dcm"));
        write_test_dicom(
            &path,
            TestDicomOptions {
                bits_stored,
                high_bit,
                ..TestDicomOptions::jpeg_12bit(mono_12bit_frame(), "MONOCHROME2")
            },
        );
        let error = open_error(&path);
        assert!(error.ends_with(message), "{error}");
    }
}

#[test]
fn spectral_selection_syntax_decodes_12bit_progressive_frames() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("spectral-selection.dcm");
    let frame = twelve_bit_fixture_frames("ybr422-progressive").swap_remove(0);
    write_test_dicom(&path, TestDicomOptions::jpeg_12bit(frame, "YBR_FULL_422"));
    // The DICOM writer does not register the retired spectral-selection
    // syntax. Both UIDs have the same length, so patch the file meta in place.
    let mut bytes = std::fs::read(&path).unwrap();
    let extended = uids::JPEG_EXTENDED12_BIT.as_bytes();
    let offsets = bytes
        .windows(extended.len())
        .enumerate()
        .filter(|(_, window)| *window == extended)
        .map(|(offset, _)| offset)
        .collect::<Vec<_>>();
    assert_eq!(offsets.len(), 1, "one transfer syntax UID in the file meta");
    bytes[offsets[0]..offsets[0] + extended.len()]
        .copy_from_slice(JPEG_SPECTRAL_SELECTION_TRANSFER_SYNTAX.as_bytes());
    std::fs::write(&path, bytes).unwrap();

    let slide = Slide::open(&path).expect("open spectral-selection DICOM");
    let tile = slide.read_tile(&first_tile_request()).expect("12-bit tile");
    assert_eq!((tile.width, tile.height), (16, 16));
    assert!(u16_samples(&tile).iter().any(|&sample| sample > 255));
}

#[test]
fn frames_whose_sof_precision_differs_from_bits_stored_fail_on_read() {
    let dir_12 = tempfile::tempdir().unwrap();
    let declared_12_encoded_8 = dir_12.path().join("declared-12-encoded-8.dcm");
    write_test_dicom(
        &declared_12_encoded_8,
        TestDicomOptions {
            rows: 8,
            columns: 8,
            total_pixel_matrix_rows: 8,
            total_pixel_matrix_columns: 8,
            ..TestDicomOptions::jpeg_12bit(extended_sequential_8x8_jpeg(), "YBR_FULL_422")
        },
    );
    let dir_8 = tempfile::tempdir().unwrap();
    let declared_8_encoded_12 = dir_8.path().join("declared-8-encoded-12.dcm");
    write_test_dicom(
        &declared_8_encoded_12,
        TestDicomOptions {
            bits_allocated: 8,
            bits_stored: 8,
            high_bit: 7,
            ..TestDicomOptions::jpeg_12bit(mono_12bit_frame(), "MONOCHROME2")
        },
    );

    for (path, message) in [
        (
            &declared_12_encoded_8,
            "DICOM JPEG frame precision 8 does not match BitsStored 12",
        ),
        (
            &declared_8_encoded_12,
            "DICOM JPEG frame precision 12 does not match BitsStored 8",
        ),
    ] {
        let slide = Slide::open(path).expect("frame precision is checked on read");
        let decoded = slide
            .read_tile(&first_tile_request())
            .expect_err("decode must reject the precision mismatch");
        assert!(decoded.to_string().contains(message), "{decoded}");
        let batched = slide
            .read_tiles(&[first_tile_request()])
            .expect_err("batch decode must reject the precision mismatch");
        assert!(batched.to_string().contains(message), "{batched}");
        let raw = slide
            .read_raw_compressed_tile(&first_tile_request())
            .expect_err("raw passthrough must reject the precision mismatch");
        assert!(raw.to_string().contains(message), "{raw}");
    }
}

#[test]
fn eight_bit_extended_frames_still_decode_to_identical_u8_samples() {
    let baseline_frame = encode_test_jpeg_rgb(8, 8, 17);
    let mut tiles = Vec::new();
    for (file_name, transfer_syntax, frame) in [
        (
            "baseline.dcm",
            uids::JPEG_BASELINE8_BIT,
            baseline_frame.clone(),
        ),
        (
            "extended.dcm",
            uids::JPEG_EXTENDED12_BIT,
            extended_sequential_8x8_jpeg(),
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(file_name);
        write_test_dicom(
            &path,
            TestDicomOptions {
                transfer_syntax,
                photometric_interpretation: "YBR_FULL_422",
                rows: 8,
                columns: 8,
                total_pixel_matrix_rows: 8,
                total_pixel_matrix_columns: 8,
                pixel_data: TestPixelData::Encapsulated(frame),
                ..TestDicomOptions::native(Vec::new())
            },
        );
        let slide = Slide::open(&path).expect("open 8-bit JPEG DICOM");
        assert_eq!(
            slide.dataset().scenes[0].series[0].sample_type,
            SampleType::Uint8
        );
        let raw = slide
            .read_raw_compressed_tile(&first_tile_request())
            .expect("8-bit raw frame");
        assert_eq!(raw.bits_allocated(), 8);
        let tile = slide.read_tile(&first_tile_request()).expect("8-bit tile");
        tiles.push(tile.data.as_u8().expect("8-bit DICOM stays U8").to_vec());
    }
    // SOF1 and SOF0 carry identical 8-bit entropy data.
    assert_eq!(tiles[0], tiles[1]);
}

#[test]
fn sparse_black_tiles_follow_the_decoded_sample_type() {
    let eight = black_sample_buffer(2, 2, DicomBitDepth::Eight).expect("8-bit black tile");
    assert_eq!(eight.data.as_u8(), Some([0; 12].as_slice()));
    let twelve = black_sample_buffer(2, 2, DicomBitDepth::Twelve).expect("12-bit black tile");
    assert_eq!(twelve.data.as_u16(), Some([0; 12].as_slice()));
    assert_eq!((twelve.width, twelve.height, twelve.channels), (2, 2, 3));
}
