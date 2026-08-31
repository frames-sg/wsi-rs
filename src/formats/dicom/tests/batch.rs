use super::fixtures::*;
use super::runtime::*;
use super::*;
#[test]
fn read_tiles_cpu_decodes_jpeg_frames_in_request_order() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jpeg-cpu-batch.dcm");
    let mut options = TestDicomOptions::native(Vec::new());
    options.transfer_syntax = JPEG_TRANSFER_SYNTAX;
    options.rows = 16;
    options.columns = 16;
    options.total_pixel_matrix_rows = 16;
    options.total_pixel_matrix_columns = 32;
    options.number_of_frames = 2;
    options.pixel_data = TestPixelData::EncapsulatedFrames(vec![
        encode_test_jpeg_rgb(16, 16, 3),
        encode_test_jpeg_rgb(16, 16, 41),
    ]);
    write_test_dicom(&path, options);

    let slide = Slide::open(&path).expect("open generated DICOM JPEG slide");
    let tiles = slide
        .read_tiles(&[tile_request(1, 0), tile_request(0, 0)])
        .expect("read JPEG CPU tile batch");

    assert_eq!(tiles.len(), 2);
    let first = &tiles[0];
    let second = &tiles[1];
    assert_ne!(
        first.data.as_u8().expect("first JPEG tile").get(0..3),
        second.data.as_u8().expect("second JPEG tile").get(0..3),
        "request order should be preserved across distinct decoded frames"
    );
}

type RecordedTileAdmissions = Arc<Mutex<Vec<Vec<(i64, i64)>>>>;

struct RecordingDicomReader {
    inner: DicomReader,
    controlled_admissions: RecordedTileAdmissions,
}

impl SlideReader for RecordingDicomReader {
    fn dataset(&self) -> &Dataset {
        self.inner.dataset()
    }

    fn tile_codec_kind(&self, req: &TileRequest) -> TileCodecKind {
        self.inner.tile_codec_kind(req)
    }

    fn read_tiles_cpu(&self, reqs: &[TileRequest]) -> Result<Vec<CpuTile>, WsiError> {
        self.inner.read_tiles_cpu(reqs)
    }

    fn read_tiles_cpu_controlled(
        &self,
        reqs: &[TileRequest],
        control: &crate::ReadControl,
    ) -> Result<Vec<CpuTile>, WsiError> {
        self.controlled_admissions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(reqs.iter().map(|req| (req.col, req.row)).collect());
        self.inner.read_tiles_cpu_controlled(reqs, control)
    }

    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        self.inner.read_tile_cpu(req)
    }
}

#[test]
fn controlled_batch_of_eight_reaches_dicom_once_in_original_order() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jpeg-controlled-batch-eight.dcm");
    let mut options = TestDicomOptions::native(Vec::new());
    options.transfer_syntax = JPEG_TRANSFER_SYNTAX;
    options.rows = 16;
    options.columns = 16;
    options.total_pixel_matrix_rows = 16;
    options.total_pixel_matrix_columns = 16 * 8;
    options.number_of_frames = 8;
    options.pixel_data = TestPixelData::EncapsulatedFrames(
        (0..8)
            .map(|index| encode_test_jpeg_rgb(16, 16, 3 + index * 19))
            .collect(),
    );
    write_test_dicom(&path, options);

    let (inner, _) = reader_and_first_image(&path);
    let controlled_admissions = Arc::new(Mutex::new(Vec::new()));
    let slide = Slide::from_source_with_cache_bytes(
        Box::new(RecordingDicomReader {
            inner,
            controlled_admissions: Arc::clone(&controlled_admissions),
        }),
        1024 * 1024,
    );
    let requests = [7, 0, 5, 2, 6, 1, 4, 3]
        .into_iter()
        .map(|col| tile_request(col, 0))
        .collect::<Vec<_>>();

    let tiles = slide
        .read_tiles_controlled(&requests, &crate::ReadControl::default())
        .expect("controlled DICOM batch of eight");

    assert_eq!(tiles.len(), requests.len());
    assert_eq!(
        *controlled_admissions
            .lock()
            .unwrap_or_else(|error| error.into_inner()),
        vec![requests
            .iter()
            .map(|request| (request.col, request.row))
            .collect::<Vec<_>>()],
        "the adaptive wrapper must admit one unchanged DICOM batch"
    );
    for (tile, expected) in tiles.iter().zip(
        requests
            .iter()
            .map(|request| slide.read_tile(request).expect("matching sequential tile")),
    ) {
        assert_eq!(tile.data.as_u8(), expected.data.as_u8());
    }
}

#[test]
fn read_tiles_cpu_decodes_jp2k_frames_in_request_order() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jp2k-cpu-batch.dcm");
    let codestream = include_bytes!("../../../../tests/fixtures/jp2k/rgb_nomct.j2k").to_vec();
    let mut options = TestDicomOptions::native(Vec::new());
    options.transfer_syntax = uids::JPEG2000_LOSSLESS;
    options.rows = 12;
    options.columns = 16;
    options.total_pixel_matrix_rows = 12;
    options.total_pixel_matrix_columns = 32;
    options.number_of_frames = 2;
    options.pixel_data = TestPixelData::EncapsulatedFrames(vec![codestream.clone(), codestream]);
    write_test_dicom(&path, options);

    let slide = Slide::open(&path).expect("open generated DICOM JP2K slide");
    let tiles = slide
        .read_tiles_controlled(
            &[tile_request(1, 0), tile_request(0, 0)],
            &crate::ReadControl::default(),
        )
        .expect("read JP2K CPU tile batch");

    assert_eq!(tiles.len(), 2);
}

#[test]
fn controlled_jp2k_batch_preserves_edge_tile_dimensions() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jp2k-controlled-edge-batch.dcm");
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

    let slide = Slide::open(&path).expect("open generated DICOM JP2K slide");
    let tiles = slide
        .read_tiles_controlled(
            &[tile_request(1, 0), tile_request(0, 0)],
            &crate::ReadControl::default(),
        )
        .expect("read controlled JP2K edge batch");

    let dimensions = tiles
        .into_iter()
        .map(|tile| (tile.width, tile.height))
        .collect::<Vec<_>>();
    assert_eq!(dimensions, vec![(8, 12), (16, 12)]);
}

#[test]
fn controlled_dicom_batch_cancelled_before_io_does_not_build_index() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jp2k-cancelled-before-io.dcm");
    let codestream = include_bytes!("../../../../tests/fixtures/jp2k/rgb_nomct.j2k").to_vec();
    let mut options = TestDicomOptions::native(Vec::new());
    options.transfer_syntax = uids::JPEG2000_LOSSLESS;
    options.rows = 12;
    options.columns = 16;
    options.total_pixel_matrix_rows = 12;
    options.total_pixel_matrix_columns = 32;
    options.number_of_frames = 2;
    options.pixel_data = TestPixelData::EncapsulatedFrames(vec![codestream.clone(), codestream]);
    write_test_dicom(&path, options);

    let (reader, image) = reader_and_first_image(&path);
    let cancellation = crate::ReadCancellationToken::new();
    cancellation.cancel();
    let error = reader
        .read_tiles_cpu_controlled(
            &[tile_request(1, 0), tile_request(0, 0)],
            &crate::ReadControl::new(cancellation),
        )
        .expect_err("cancelled DICOM batch must stop before I/O");

    assert!(matches!(error, WsiError::Cancelled));
    assert!(
        image
            .frame_store
            .encapsulated_frames
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .is_none(),
        "cancellation before admission must not build the frame index"
    );
}

#[test]
fn read_tiles_cpu_skips_decoded_cache_when_batch_exceeds_cache_capacity() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jp2k-cache-churn.dcm");
    let codestream = include_bytes!("../../../../tests/fixtures/jp2k/rgb_nomct.j2k").to_vec();
    let mut options = TestDicomOptions::native(Vec::new());
    options.transfer_syntax = uids::JPEG2000_LOSSLESS;
    options.rows = 12;
    options.columns = 16;
    options.total_pixel_matrix_rows = 12;
    options.total_pixel_matrix_columns = 48;
    options.number_of_frames = 3;
    options.pixel_data =
        TestPixelData::EncapsulatedFrames(vec![codestream.clone(), codestream.clone(), codestream]);
    write_test_dicom(&path, options);

    let (reader, image) = reader_and_first_image_with_cache_config(
        &path,
        CacheConfig::deterministic().with_shared_tile_bytes(9 * 1024),
    );
    let tiles = reader
        .read_tiles_cpu(&[tile_request(0, 0), tile_request(1, 0), tile_request(2, 0)])
        .expect("read JP2K CPU tile batch");

    assert_eq!(tiles.len(), 3);
    assert!(
        (0..3).all(|frame_index| image.cached_decoded_frame(frame_index).is_none()),
        "batch larger than the decoded cache should not clone decoded JP2K frames into the LRU"
    );
}

#[test]
fn read_tiles_cpu_mixes_cache_hits_and_frame_misses_in_request_order() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jpeg-cache-hit-and-miss.dcm");
    let mut options = TestDicomOptions::native(Vec::new());
    options.transfer_syntax = JPEG_TRANSFER_SYNTAX;
    options.rows = 16;
    options.columns = 16;
    options.total_pixel_matrix_rows = 16;
    options.total_pixel_matrix_columns = 32;
    options.number_of_frames = 2;
    options.pixel_data = TestPixelData::EncapsulatedFrames(vec![
        encode_test_jpeg_rgb(16, 16, 3),
        encode_test_jpeg_rgb(16, 16, 41),
    ]);
    write_test_dicom(&path, options);

    let (reader, image) = reader_and_first_image_with_cache_config(
        &path,
        CacheConfig::deterministic().with_shared_tile_bytes(64 * 1024),
    );
    let cached =
        CpuTile::from_u8_interleaved(16, 16, 3, ColorSpace::Rgb, [251, 7, 3].repeat(16 * 16))
            .expect("build cached marker tile");
    image.cache_decoded_frame(1, Arc::new(cached));

    let tiles = reader
        .read_tiles_cpu(&[tile_request(1, 0), tile_request(0, 0)])
        .expect("read mixed cache-hit/cache-miss DICOM batch");

    let hit = &tiles[0];
    let miss = &tiles[1];
    assert_eq!(hit.data.as_u8().unwrap().get(0..3), Some(&[251, 7, 3][..]));
    assert_ne!(miss.data.as_u8().unwrap().get(0..3), Some(&[251, 7, 3][..]));
    assert!(
        image.cached_decoded_frame(0).is_some(),
        "the decoded miss should retain the existing batch cache policy"
    );
}

#[test]
fn read_tiles_cpu_reports_the_first_invalid_request_deterministically() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("batch-request-errors.dcm");
    write_test_dicom(&path, TestDicomOptions::native(test_rgb_pixel_data()));
    let (reader, _) = reader_and_first_image(&path);
    let mut missing_level = tile_request(0, 0);
    missing_level.level = LevelIdx::new(9);

    let bounds_first = match reader.read_tiles_cpu(&[tile_request(-1, 0), missing_level.clone()]) {
        Ok(_) => panic!("out-of-range tile should fail before the later missing level"),
        Err(error) => error,
    };
    match bounds_first {
        WsiError::TileRead {
            col: -1,
            row: 0,
            level: 0,
            reason,
        } => assert_eq!(reason, "tile (-1,0) out of range (1x1)"),
        other => panic!("unexpected first bounds error: {other}"),
    }

    let level_first = match reader.read_tiles_cpu(&[missing_level, tile_request(-1, 0)]) {
        Ok(_) => panic!("missing level should fail before the later out-of-range tile"),
        Err(error) => error,
    };
    assert!(matches!(
        level_first,
        WsiError::LevelOutOfRange { level: 9, count: 1 }
    ));
}

#[test]
fn extract_encapsulated_frames_batch_preserves_requested_frames() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("batch-frames.dcm");
    let frames = vec![vec![1, 2, 3, 4], vec![5, 6, 7, 8], vec![9, 10, 11, 12]];
    let mut options = TestDicomOptions::native(Vec::new());
    options.transfer_syntax = JPEG_TRANSFER_SYNTAX;
    options.rows = 2;
    options.columns = 2;
    options.total_pixel_matrix_rows = 2;
    options.total_pixel_matrix_columns = 6;
    options.number_of_frames = frames.len() as u32;
    options.pixel_data = TestPixelData::EncapsulatedFrames(frames.clone());
    write_test_dicom(&path, options);

    let (_reader, image) = reader_and_first_image(&path);
    let extracted = image
        .extract_encapsulated_frames_controlled(&[2, 0], 0, 0, 0, true, None)
        .expect("batch extract frames");

    assert_eq!(extracted.get(&2).unwrap().as_slice(), frames[2].as_slice());
    assert_eq!(extracted.get(&0).unwrap().as_slice(), frames[0].as_slice());
}

#[test]
fn grouped_frame_read_validates_item_header_from_the_grouped_window() {
    struct CountingCursor {
        inner: std::io::Cursor<Vec<u8>>,
        read_calls: usize,
    }

    impl std::io::Read for CountingCursor {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            self.read_calls += 1;
            self.inner.read(buffer)
        }
    }

    impl std::io::Seek for CountingCursor {
        fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(position)
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("grouped-read-io-count.dcm");
    write_test_dicom(&path, TestDicomOptions::native(test_rgb_pixel_data()));
    let (_reader, image) = reader_and_first_image(&path);
    let payload = [1, 2, 3, 4];
    let fragment = DicomFragmentRef {
        item_offset: 0,
        payload_offset: 8,
        len: payload.len() as u32,
    };
    let frames = DicomEncapsulatedFrames {
        fragments: vec![fragment],
        frame_ranges: std::iter::once(0..1).collect(),
    };
    let group = DicomFrameReadGroup {
        start: 0,
        end: 12,
        spans: vec![DicomFrameReadSpan {
            frame_index: 0,
            frame_range: 0..1,
            start: 0,
            end: 12,
        }],
    };
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&DICOM_ITEM_TAG_LE);
    bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&payload);
    let mut reader = CountingCursor {
        inner: std::io::Cursor::new(bytes),
        read_calls: 0,
    };

    let extracted = image
        .read_encapsulated_frame_group(&mut reader, &frames, &group)
        .expect("grouped read validates and extracts its frame");

    assert_eq!(extracted, vec![(0, payload.to_vec())]);
    assert_eq!(
        reader.read_calls, 1,
        "one grouped window read must provide both Item headers and payload bytes"
    );
}
