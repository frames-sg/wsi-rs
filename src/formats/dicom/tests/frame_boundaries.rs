use super::fixtures::*;
use super::runtime::test_dicom_image_with_transfer_syntax;
use super::*;

fn assert_error_contains<T>(result: Result<T, WsiError>, expected: &str) {
    let error = match result {
        Ok(_) => panic!("expected an error containing {expected:?}"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains(expected),
        "expected {error:?} to contain {expected:?}"
    );
}

struct FillingReader {
    pattern: [u8; 8],
}

impl Read for FillingReader {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        for (index, byte) in buffer.iter_mut().enumerate() {
            *byte = self.pattern[index % self.pattern.len()];
        }
        Ok(buffer.len())
    }
}

impl Seek for FillingReader {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        match position {
            SeekFrom::Start(offset) => Ok(offset),
            SeekFrom::Current(_) | SeekFrom::End(_) => {
                Err(std::io::Error::other("unsupported synthetic seek"))
            }
        }
    }
}

struct SeekErrorReader;

impl Read for SeekErrorReader {
    fn read(&mut self, _buffer: &mut [u8]) -> std::io::Result<usize> {
        unreachable!("a failed seek must prevent the read")
    }
}

impl Seek for SeekErrorReader {
    fn seek(&mut self, _position: SeekFrom) -> std::io::Result<u64> {
        Err(std::io::Error::other("synthetic seek failure"))
    }
}

fn fragment(item_offset: u64, len: u32) -> DicomFragmentRef {
    DicomFragmentRef {
        item_offset,
        payload_offset: item_offset + 8,
        len,
    }
}

#[test]
fn compressed_frame_preflight_rejects_item_offset_overflow() {
    let invalid = DicomFragmentRef {
        item_offset: u64::MAX - 4,
        payload_offset: 0,
        len: 1,
    };

    assert_error_contains(
        preflight_compressed_frame(Path::new("overflow.dcm"), &[invalid]),
        "Item offset overflow",
    );
}

#[test]
fn fragment_window_copy_rejects_underflow_and_truncation() {
    let path = Path::new("window.dcm");
    let valid = fragment(100, 4);

    assert_error_contains(
        copy_fragments_from_window(path, 109, &[0; 4], &[valid]),
        "offset underflow",
    );
    assert_error_contains(
        copy_fragments_from_window(path, 100, &[0; 10], &[valid]),
        "outside read window",
    );
}

#[test]
fn reopening_a_missing_dicom_preserves_path_context() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("missing.dcm");

    assert_error_contains(reopen_dicom_object(&path), "failed to reopen DICOM object");
}

#[test]
fn positional_reads_preserve_seek_and_read_failures() {
    let path = Path::new("positional-read.dcm");
    let mut seek_failure = SeekErrorReader;
    assert_error_contains(
        read_exact_at(&mut seek_failure, path, 7, &mut [0; 1]),
        "I/O error",
    );

    let mut truncated = std::io::Cursor::new(Vec::<u8>::new());
    assert_error_contains(
        read_exact_at(&mut truncated, path, 0, &mut [0; 1]),
        "I/O error",
    );
}

#[test]
fn extended_offset_table_reader_rejects_overflow_and_invalid_width() {
    let path = Path::new("extended-offsets.dcm");
    let mut reader = std::io::Cursor::new(vec![0; 16]);
    assert_error_contains(
        read_extended_offset_tables_with_reader(
            &mut reader,
            path,
            u64::MAX - 3,
            0,
            8,
            u64::MAX,
            None,
        ),
        "range overflow",
    );

    assert_error_contains(
        read_extended_offset_tables_with_reader(&mut reader, path, 0, 4, 4, 16, None),
        "multiple of eight",
    );
}

#[test]
fn raw_pixel_sequence_scan_rejects_offset_arithmetic_overflow() {
    let path = Path::new("raw-offsets.dcm");
    let mut reader = FillingReader { pattern: [0; 8] };
    assert_error_contains(
        scan_raw_encapsulated_pixel_sequence_with_reader_controlled(
            &mut reader,
            path,
            u64::MAX - 4,
            u64::MAX,
            None,
            None,
        ),
        "Pixel Data offset overflow",
    );

    assert_error_contains(
        scan_raw_encapsulated_pixel_sequence_with_reader_controlled(
            &mut reader,
            path,
            u64::MAX - 16,
            u64::MAX,
            None,
            None,
        ),
        "raw item offset overflow",
    );

    let mut item_header = [0; 8];
    item_header[..4].copy_from_slice(&DICOM_ITEM_TAG_LE);
    item_header[4..].copy_from_slice(&8u32.to_le_bytes());
    let mut reader = FillingReader {
        pattern: item_header,
    };
    assert_error_contains(
        scan_raw_encapsulated_pixel_sequence_with_reader_controlled(
            &mut reader,
            path,
            u64::MAX - 24,
            u64::MAX,
            None,
            None,
        ),
        "payload offset overflow",
    );
}

#[test]
fn chunked_basic_offset_table_reader_rejects_late_offset_overflow() {
    let mut reader = FillingReader { pattern: [0; 8] };

    assert_error_contains(
        read_basic_offset_table_at(
            &mut reader,
            Path::new("basic-offsets.dcm"),
            u64::MAX - 10,
            65_540,
            None,
            None,
        ),
        "offset overflow",
    );
}

#[test]
fn basic_offset_mapping_rejects_overflow_and_missing_items() {
    let path = Path::new("basic-offsets.dcm");
    assert_error_contains(
        build_encapsulated_frame_index(
            path,
            vec![
                DicomFragmentRef {
                    item_offset: u64::MAX - 3,
                    payload_offset: 0,
                    len: 1,
                },
                fragment(0, 1),
            ],
            vec![0, 8],
            2,
        ),
        "offset overflow",
    );

    assert_error_contains(
        build_encapsulated_frame_index(
            path,
            vec![fragment(100, 1), fragment(120, 1)],
            vec![0, 12],
            2,
        ),
        "missing fragment offset",
    );
}

#[test]
fn offset_table_metadata_rejects_frame_count_overflow() {
    assert_error_contains(
        validate_basic_offset_table_len(Path::new("basic-offsets.dcm"), 4, Some(u32::MAX)),
        "length overflow",
    );
}

#[test]
fn extended_offset_mapping_rejects_missing_items_and_invalid_physical_lengths() {
    let path = Path::new("extended-offsets.dcm");
    let fragments = [fragment(100, 4), fragment(112, 4)];
    assert_error_contains(
        frame_ranges_from_extended_offsets(
            path,
            &fragments,
            &DicomExtendedOffsetTables {
                offsets: vec![0, 13],
                lengths: vec![4, 4],
            },
            2,
        ),
        "missing fragment offset",
    );

    let overflowing_end = DicomFragmentRef {
        item_offset: 0,
        payload_offset: u64::MAX - 1,
        len: 4,
    };
    assert_error_contains(
        frame_ranges_from_extended_offsets(
            path,
            &[overflowing_end],
            &DicomExtendedOffsetTables {
                offsets: vec![0],
                lengths: vec![4],
            },
            1,
        ),
        "end offset overflow",
    );

    let underflowing_length = DicomFragmentRef {
        item_offset: 100,
        payload_offset: 50,
        len: 4,
    };
    assert_error_contains(
        frame_ranges_from_extended_offsets(
            path,
            &[underflowing_length],
            &DicomExtendedOffsetTables {
                offsets: vec![0],
                lengths: vec![4],
            },
            1,
        ),
        "length underflow",
    );
}

#[test]
fn grouped_frame_reads_reject_invalid_spans_and_fragment_windows() {
    let image = test_dicom_image_with_transfer_syntax(
        "grouped-frame-boundaries",
        DicomGrid::Full,
        JPEG_TRANSFER_SYNTAX,
    );
    let empty_frames = DicomEncapsulatedFrames {
        fragments: Vec::new(),
        frame_ranges: Vec::new(),
    };
    let mut reader = std::io::Cursor::new(Vec::<u8>::new());
    assert_error_contains(
        image.read_encapsulated_frame_group(
            &mut reader,
            &empty_frames,
            &DicomFrameReadGroup {
                start: 1,
                end: 0,
                spans: Vec::new(),
            },
        ),
        "span underflow",
    );

    assert_error_contains(
        image.read_encapsulated_frame_group(
            &mut reader,
            &empty_frames,
            &DicomFrameReadGroup {
                start: 0,
                end: 0,
                spans: vec![DicomFrameReadSpan {
                    frame_index: 0,
                    frame_range: 0..1,
                    start: 0,
                    end: 0,
                }],
            },
        ),
        "fragment range out of bounds",
    );

    let frames = DicomEncapsulatedFrames {
        fragments: vec![fragment(0, 1)],
        frame_ranges: std::iter::once(0..1).collect(),
    };
    let span = || DicomFrameReadSpan {
        frame_index: 0,
        frame_range: 0..1,
        start: 0,
        end: 9,
    };
    let mut reader = std::io::Cursor::new(vec![0; 9]);
    assert_error_contains(
        image.read_encapsulated_frame_group(
            &mut reader,
            &frames,
            &DicomFrameReadGroup {
                start: 1,
                end: 9,
                spans: vec![span()],
            },
        ),
        "Item offset underflow",
    );

    let frames = DicomEncapsulatedFrames {
        fragments: vec![fragment(4, 1)],
        frame_ranges: std::iter::once(0..1).collect(),
    };
    let mut reader = std::io::Cursor::new(vec![0; 8]);
    assert_error_contains(
        image.read_encapsulated_frame_group(
            &mut reader,
            &frames,
            &DicomFrameReadGroup {
                start: 0,
                end: 8,
                spans: vec![span()],
            },
        ),
        "Item header is outside",
    );
}

#[test]
fn token_fallback_indexes_implicit_vr_encapsulated_frames() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("implicit-vr-encapsulated.dcm");
    let mut options = TestDicomOptions::native(Vec::new());
    options.transfer_syntax = uids::IMPLICIT_VR_LITTLE_ENDIAN;
    options.number_of_frames = 2;
    options.total_pixel_matrix_columns = 4;
    options.pixel_data =
        TestPixelData::EncapsulatedFrames(vec![vec![1, 2, 3, 4], vec![5, 6, 7, 8]]);
    write_test_dicom(&path, options);
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&events);
    let control = crate::ReadControl::default().with_diagnostic_sink(Arc::new(
        move |event: crate::DicomIndexDiagnostic| captured.lock().unwrap().push(event),
    ));

    let frames = scan_encapsulated_frames_controlled(
        &path,
        uids::IMPLICIT_VR_LITTLE_ENDIAN,
        2,
        Some(&control),
    )
    .expect("token fallback should index implicit-VR encapsulated frames");

    assert_eq!(frames.frame_ranges, vec![0..1, 1..2]);
    let outcomes = events
        .lock()
        .unwrap()
        .iter()
        .map(|event| event.outcome)
        .collect::<Vec<_>>();
    assert_eq!(
        outcomes,
        vec![
            crate::DicomIndexOutcome::FastPathFallback,
            crate::DicomIndexOutcome::TokenFallback,
        ]
    );
}

#[test]
fn raw_layout_scan_skips_nested_undefined_length_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested-undefined-metadata.dcm");
    let mut bytes = vec![0; 132];
    bytes[128..132].copy_from_slice(b"DICM");

    bytes.extend_from_slice(&[0x08, 0x00, 0x11, 0x11]);
    bytes.extend_from_slice(b"SQ");
    bytes.extend_from_slice(&[0, 0]);
    bytes.extend_from_slice(&u32::MAX.to_le_bytes());
    bytes.extend_from_slice(&DICOM_ITEM_TAG_LE);
    bytes.extend_from_slice(&u32::MAX.to_le_bytes());
    bytes.extend_from_slice(&[0x10, 0x00, 0x10, 0x00]);
    bytes.extend_from_slice(b"PN");
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(b"A ");
    bytes.extend_from_slice(&[0xFE, 0xFF, 0x0D, 0xE0]);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&DICOM_SEQUENCE_DELIMITER_TAG_LE);
    bytes.extend_from_slice(&0u32.to_le_bytes());

    bytes.extend_from_slice(&PIXEL_DATA_TAG_LE);
    bytes.extend_from_slice(b"OB");
    bytes.extend_from_slice(&[0, 0]);
    bytes.extend_from_slice(&u32::MAX.to_le_bytes());
    bytes.extend_from_slice(&DICOM_ITEM_TAG_LE);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    let item_offset = bytes.len() as u64;
    bytes.extend_from_slice(&DICOM_ITEM_TAG_LE);
    bytes.extend_from_slice(&4u32.to_le_bytes());
    bytes.extend_from_slice(&[1, 2, 3, 4]);
    bytes.extend_from_slice(&DICOM_SEQUENCE_DELIMITER_TAG_LE);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    std::fs::write(&path, bytes).unwrap();

    let frames = scan_encapsulated_frames_raw_little_endian(&path, 1)
        .expect("nested undefined-length metadata should be skipped")
        .expect("Pixel Data should be found");

    assert_eq!(frames.frame_ranges, vec![0..1]);
    assert_eq!(frames.fragments[0].item_offset, item_offset);
}
