use super::fixtures::*;
use super::runtime::*;
use super::*;

fn poison<T>(mutex: &Mutex<T>) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = mutex.lock().unwrap();
        panic!("poison test mutex");
    }));
    assert!(result.is_err());
}

fn generated_jpeg_image() -> (tempfile::TempDir, Arc<DicomImage>) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jpeg-cache-recovery.dcm");
    let frame = encode_test_jpeg_rgb(16, 16, 3);
    let mut options = TestDicomOptions::native(Vec::new());
    options.transfer_syntax = JPEG_TRANSFER_SYNTAX;
    options.rows = 16;
    options.columns = 16;
    options.total_pixel_matrix_rows = 16;
    options.total_pixel_matrix_columns = 16;
    options.pixel_data = TestPixelData::Encapsulated(frame);
    write_test_dicom(&path, options);
    let (_, image) = reader_and_first_image(&path);
    (dir, image)
}

#[test]
fn crop_sample_buffer_rgb_borrows_source_and_preserves_contiguous_rows() {
    let source = CpuTile::from_u8_interleaved(
        3,
        2,
        3,
        ColorSpace::Rgb,
        vec![
            1, 2, 3, 4, 5, 6, 7, 8, 9, //
            10, 11, 12, 13, 14, 15, 16, 17, 18,
        ],
    )
    .expect("source tile");

    let cropped = crop_sample_buffer_rgb(&source, 2, 2).expect("crop borrowed source");

    assert_eq!(source.width, 3, "source tile remains available after crop");
    assert_eq!(cropped.width, 2);
    assert_eq!(cropped.height, 2);
    assert_eq!(
        cropped.data.as_u8().expect("cropped RGB"),
        &[1, 2, 3, 4, 5, 6, 10, 11, 12, 13, 14, 15]
    );
}

fn assert_cached_edge_frame_crop(path: &Path, expected_width: u32, expected_height: u32) {
    let (reader, image) = reader_and_first_image(path);
    let req = tile_request(1, 0);

    assert!(
        image.cached_decoded_frame(1).is_none(),
        "test must start without a cached edge frame"
    );
    let first = reader.read_tile_cpu(&req).expect("read edge tile");
    assert!(
        image.cached_decoded_frame(1).is_some(),
        "first read should cache the full decoded frame"
    );
    let second = reader.read_tile_cpu(&req).expect("read cached edge tile");

    assert_eq!(
        (first.width, first.height),
        (expected_width, expected_height)
    );
    assert_eq!(
        (second.width, second.height),
        (expected_width, expected_height)
    );
    assert_eq!(
        first.data.as_u8().expect("first edge tile"),
        second.data.as_u8().expect("second edge tile"),
        "cached full frame crop must match the first edge-frame crop"
    );
}

#[test]
fn cached_jpeg_edge_frame_preserves_cropped_dimensions_and_pixels() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jpeg-edge-cache.dcm");
    let mut options = TestDicomOptions::native(Vec::new());
    options.transfer_syntax = JPEG_TRANSFER_SYNTAX;
    options.rows = 16;
    options.columns = 16;
    options.total_pixel_matrix_rows = 16;
    options.total_pixel_matrix_columns = 24;
    options.number_of_frames = 2;
    options.pixel_data = TestPixelData::EncapsulatedFrames(vec![
        encode_test_jpeg_rgb(16, 16, 3),
        encode_test_jpeg_rgb(16, 16, 41),
    ]);
    write_test_dicom(&path, options);

    assert_cached_edge_frame_crop(&path, 8, 16);
}

#[test]
fn cached_jp2k_edge_frame_preserves_cropped_dimensions_and_pixels() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jp2k-edge-cache.dcm");
    let codestream = include_bytes!("../../../../tests/fixtures/jp2k/rgb_nomct.j2k").to_vec();
    let mut options = TestDicomOptions::native(Vec::new());
    options.transfer_syntax = uids::JPEG2000_LOSSLESS;
    options.rows = 12;
    options.columns = 16;
    options.total_pixel_matrix_rows = 12;
    options.total_pixel_matrix_columns = 24;
    options.number_of_frames = 2;
    options.pixel_data = TestPixelData::EncapsulatedFrames(vec![codestream.clone(), codestream]);
    write_test_dicom(&path, options);

    assert_cached_edge_frame_crop(&path, 8, 12);
}

#[test]
fn cached_rle_edge_frame_preserves_cropped_dimensions_and_pixels() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rle-edge-cache.dcm");
    let pixels = 16usize;
    let mut options = TestDicomOptions::native(Vec::new());
    options.transfer_syntax = RLE_TRANSFER_SYNTAX;
    options.rows = 4;
    options.columns = 4;
    options.total_pixel_matrix_rows = 4;
    options.total_pixel_matrix_columns = 6;
    options.number_of_frames = 2;
    options.pixel_data = TestPixelData::EncapsulatedFrames(vec![
        rle_rgb_frame(&vec![10; pixels], &vec![20; pixels], &vec![30; pixels]),
        rle_rgb_frame(&vec![40; pixels], &vec![50; pixels], &vec![60; pixels]),
    ]);
    write_test_dicom(&path, options);

    assert_cached_edge_frame_crop(&path, 2, 4);
}

#[test]
fn decoded_frame_cache_recovers_from_poisoning() {
    let image = test_dicom_image_with_transfer_syntax(
        "poisoned-decoded-cache",
        DicomGrid::Full,
        JPEG_TRANSFER_SYNTAX,
    );
    poison(&image.decoded_frame_cache);
    let tile = Arc::new(black_sample_buffer(2, 2).expect("black tile"));

    assert!(image.cached_decoded_frame(7).is_none());
    image.cache_decoded_frame(7, tile.clone());
    assert!(Arc::ptr_eq(
        &image.cached_decoded_frame(7).expect("cached tile"),
        &tile
    ));
    assert!(image.should_cache_decoded_frames_for_batch(1));
}

#[test]
fn encapsulated_frame_caches_recover_from_poisoning() {
    let (_single_dir, single_image) = generated_jpeg_image();
    poison(&single_image.frame_store.compressed_frame_cache);
    let single = single_image
        .extract_encapsulated_frame(0, 0, 0, 0, true)
        .expect("single-frame extraction after cache poisoning");
    assert!(!single.is_empty());

    let (_batch_dir, batch_image) = generated_jpeg_image();
    poison(&batch_image.frame_store.compressed_frame_cache);
    let batch = batch_image
        .extract_encapsulated_frames_controlled(&[0], 0, 0, 0, true, None)
        .expect("batch extraction after cache poisoning");
    assert_eq!(
        batch.get(&0).map(|bytes| bytes.as_slice()),
        Some(single.as_slice())
    );

    let (_index_dir, index_image) = generated_jpeg_image();
    poison(&index_image.frame_store.encapsulated_frames);
    assert_eq!(
        index_image
            .ensure_encapsulated_frames()
            .expect("index build after mutex poisoning")
            .frame_ranges,
        vec![0..1]
    );
}

#[test]
fn associated_and_native_decode_errors_preserve_tile_context() {
    let image = test_dicom_image_with_transfer_syntax(
        "missing-native-pixels",
        DicomGrid::Full,
        uids::EXPLICIT_VR_LITTLE_ENDIAN,
    );

    let native = image
        .decode_uncompressed_frame_sample_buffer(0, 3, 4, 5)
        .expect_err("missing native Pixel Data location must fail");
    let WsiError::TileRead {
        col,
        row,
        level,
        reason,
    } = native
    else {
        panic!("expected contextual tile-read error, got {native:?}");
    };
    assert_eq!((col, row, level), (4, 5, 3));
    assert!(reason.contains("native DICOM Pixel Data location"));

    let associated = image
        .read_associated("label")
        .expect_err("missing associated image source must fail");
    assert!(associated
        .to_string()
        .contains("native DICOM Pixel Data location"));
}

#[test]
fn native_decode_rejects_untrusted_dimensions_before_file_io() {
    let mut image = test_dicom_image_with_transfer_syntax(
        "oversized-native-frame",
        DicomGrid::Full,
        uids::EXPLICIT_VR_LITTLE_ENDIAN,
    );
    let image_mut = Arc::get_mut(&mut image).expect("test owns the only image reference");
    image_mut.tile_width = u32::MAX;
    image_mut.tile_height = u32::MAX;

    let error = match image.decode_uncompressed_frame_sample_buffer(0, 3, 4, 5) {
        Ok(_) => panic!("oversized native frame dimensions must fail before file I/O"),
        Err(error) => error,
    };
    let WsiError::TileRead {
        col,
        row,
        level,
        reason,
    } = error
    else {
        panic!("expected contextual tile-read error, got {error:?}");
    };
    assert_eq!((col, row, level), (4, 5, 3));
    assert!(reason.contains("native DICOM frame"));
}

#[test]
fn encapsulated_extraction_rejects_invalid_index_graphs_before_io() {
    let image = test_dicom_image_with_transfer_syntax(
        "invalid-frame-graph",
        DicomGrid::Full,
        JPEG_TRANSFER_SYNTAX,
    );
    *image.frame_store.encapsulated_frames.lock().unwrap() =
        Some(Arc::new(DicomEncapsulatedFrames {
            fragments: vec![DicomFragmentRef {
                item_offset: 0,
                payload_offset: 8,
                len: 1,
            }],
            frame_ranges: Vec::new(),
        }));

    let single = match image.extract_encapsulated_frame(0, 2, 3, 4, false) {
        Ok(_) => panic!("missing single-frame range must fail"),
        Err(error) => error,
    };
    assert!(single.to_string().contains("out of range for 0 frames"));

    let batch = match image.extract_encapsulated_frames_controlled(&[0], 2, 3, 4, false, None) {
        Ok(_) => panic!("missing batch-frame range must fail"),
        Err(error) => error,
    };
    assert!(batch.to_string().contains("out of range for 0 frames"));

    *image.frame_store.encapsulated_frames.lock().unwrap() =
        Some(Arc::new(DicomEncapsulatedFrames {
            fragments: vec![DicomFragmentRef {
                item_offset: 0,
                payload_offset: 8,
                len: 1,
            }],
            frame_ranges: std::iter::once(0..1).collect(),
        }));
    let missing_source =
        match image.extract_encapsulated_frames_controlled(&[0], 2, 3, 4, false, None) {
            Ok(_) => panic!("valid graph with a missing source must fail at file admission"),
            Err(error) => error,
        };
    assert!(matches!(missing_source, WsiError::IoWithPath { .. }));

    let missing_fragment_source = match image.read_encapsulated_fragments(&[DicomFragmentRef {
        item_offset: 0,
        payload_offset: 8,
        len: 1,
    }]) {
        Ok(_) => panic!("fragment read against a missing source must fail"),
        Err(error) => error,
    };
    assert!(matches!(
        missing_fragment_source,
        WsiError::IoWithPath { .. }
    ));
}

#[test]
fn native_pixel_data_is_not_misclassified_as_encapsulated() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("native-pixel-data.dcm");
    write_test_dicom(&path, TestDicomOptions::native(test_rgb_pixel_data()));
    let (reader, image) = reader_and_first_image(&path);

    let error = match image.extract_encapsulated_frame(0, 0, 0, 0, false) {
        Ok(_) => panic!("native Pixel Data must not be returned as an encapsulated fragment"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("pixel data is not encapsulated"));

    let missing_associated = match reader.read_associated("missing") {
        Ok(_) => panic!("missing associated image must fail at reader lookup"),
        Err(error) => error,
    };
    assert!(matches!(
        missing_associated,
        WsiError::AssociatedImageNotFound(name) if name == "missing"
    ));
}
