use super::frame_offsets::push_pixel_fragment;
use super::*;

fn explicit_file_meta(elements: &[([u8; 4], [u8; 2], &[u8])]) -> Vec<u8> {
    let group_length = elements
        .iter()
        .map(|(_, _, value)| 8usize + value.len())
        .sum::<usize>();
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"DICM");
    bytes.extend_from_slice(&[0x02, 0x00, 0x00, 0x00]);
    bytes.extend_from_slice(b"UL");
    bytes.extend_from_slice(&4u16.to_le_bytes());
    bytes.extend_from_slice(&(group_length as u32).to_le_bytes());
    for (tag, vr, value) in elements {
        bytes.extend_from_slice(tag);
        bytes.extend_from_slice(vr);
        bytes.extend_from_slice(&(value.len() as u16).to_le_bytes());
        bytes.extend_from_slice(value);
    }
    bytes
}

#[derive(Clone, Copy)]
enum SeekFailure {
    First,
    Preamble,
    SkipValue,
    StreamPosition,
    End,
    DatasetRewind,
}

struct SelectiveSeekFailure {
    inner: std::io::Cursor<Vec<u8>>,
    failure: SeekFailure,
    start_calls: usize,
}

impl std::io::Read for SelectiveSeekFailure {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buffer)
    }
}

impl std::io::Seek for SelectiveSeekFailure {
    fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
        let should_fail = match (self.failure, position) {
            (SeekFailure::First, std::io::SeekFrom::Start(0)) => self.start_calls == 0,
            (SeekFailure::Preamble, std::io::SeekFrom::Start(128)) => true,
            (SeekFailure::SkipValue, std::io::SeekFrom::Current(value)) => value != 0,
            (SeekFailure::StreamPosition, std::io::SeekFrom::Current(0)) => true,
            (SeekFailure::End, std::io::SeekFrom::End(0)) => true,
            (SeekFailure::DatasetRewind, std::io::SeekFrom::Start(_)) => self.start_calls == 1,
            _ => false,
        };
        if matches!(position, std::io::SeekFrom::Start(_)) {
            self.start_calls += 1;
        }
        if should_fail {
            Err(std::io::Error::other("injected seek failure"))
        } else {
            self.inner.seek(position)
        }
    }
}

fn reader_with_seek_failure(bytes: Vec<u8>, failure: SeekFailure) -> SelectiveSeekFailure {
    SelectiveSeekFailure {
        inner: std::io::Cursor::new(bytes),
        failure,
        start_calls: 0,
    }
}

#[test]
fn raw_encapsulated_scan_rejects_fragment_extending_past_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("raw-truncated-fragment.dcm");
    let mut bytes = vec![0; 132];
    bytes[128..132].copy_from_slice(b"DICM");
    bytes.extend_from_slice(&PIXEL_DATA_TAG_LE);
    bytes.extend_from_slice(b"OB");
    bytes.extend_from_slice(&[0, 0]);
    bytes.extend_from_slice(&UNDEFINED_LENGTH_LE);
    bytes.extend_from_slice(&DICOM_ITEM_TAG_LE);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&DICOM_ITEM_TAG_LE);
    bytes.extend_from_slice(&1024u32.to_le_bytes());
    bytes.extend_from_slice(&[1, 2, 3, 4]);
    std::fs::write(&path, bytes).unwrap();

    let error = scan_encapsulated_frames_raw_little_endian(&path, 1)
        .expect_err("truncated fragment must fail safely");
    assert!(error.to_string().contains("beyond the source file"));
}

#[test]
fn raw_item_scan_seeks_over_fragment_payloads() {
    struct CountingCursor {
        inner: std::io::Cursor<Vec<u8>>,
        bytes_read: usize,
    }

    impl std::io::Read for CountingCursor {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let read = self.inner.read(buffer)?;
            self.bytes_read += read;
            Ok(read)
        }
    }

    impl std::io::Seek for CountingCursor {
        fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(position)
        }
    }

    let payload_len = 1024 * 1024u32;
    let mut bytes = Vec::with_capacity(payload_len as usize + 36);
    bytes.extend_from_slice(&PIXEL_DATA_TAG_LE);
    bytes.extend_from_slice(b"OB");
    bytes.extend_from_slice(&[0, 0]);
    bytes.extend_from_slice(&UNDEFINED_LENGTH_LE);
    bytes.extend_from_slice(&DICOM_ITEM_TAG_LE);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&DICOM_ITEM_TAG_LE);
    bytes.extend_from_slice(&payload_len.to_le_bytes());
    bytes.resize(bytes.len() + payload_len as usize, 0xA5);
    bytes.extend_from_slice(&DICOM_SEQUENCE_DELIMITER_TAG_LE);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    let file_len = bytes.len() as u64;
    let mut reader = CountingCursor {
        inner: std::io::Cursor::new(bytes),
        bytes_read: 0,
    };

    let (fragments, basic_offsets) = scan_raw_encapsulated_pixel_sequence_with_reader_controlled(
        &mut reader,
        Path::new("counted-payload.dcm"),
        0,
        file_len,
        None,
        None,
    )
    .expect("item scan succeeds");

    assert_eq!(fragments.len(), 1);
    assert!(basic_offsets.is_empty());
    assert_eq!(
        reader.bytes_read, 24,
        "indexing should read only the BOT, fragment, and delimiter headers"
    );
}

#[test]
fn oversized_basic_offset_table_is_rejected_without_allocating_its_payload() {
    let error = validate_basic_offset_table_len(
        Path::new("oversized-basic-offset-table.dcm"),
        u32::MAX - 3,
        None,
    )
    .expect_err("an untrusted multi-gigabyte basic offset table must be rejected");

    assert!(
        error.to_string().contains("exceeds safety limit"),
        "unexpected error: {error}"
    );
}

#[test]
fn compressed_frame_preflight_enforces_exact_limit_for_every_fragment() {
    let path = Path::new("compressed-frame-limit.dcm");
    let exact = DicomFragmentRef {
        item_offset: 0,
        payload_offset: 8,
        len: crate::core::limits::MAX_COMPRESSED_INPUT_BYTES as u32,
    };
    let total_len = preflight_compressed_frame(path, &[exact])
        .expect("the exact compressed-frame limit must be accepted");
    assert_eq!(
        total_len,
        crate::core::limits::MAX_COMPRESSED_INPUT_BYTES as usize
    );

    for fragments in [
        vec![DicomFragmentRef {
            item_offset: 0,
            payload_offset: 8,
            len: (crate::core::limits::MAX_COMPRESSED_INPUT_BYTES + 1) as u32,
        }],
        vec![
            DicomFragmentRef {
                item_offset: 0,
                payload_offset: 8,
                len: 1,
            },
            DicomFragmentRef {
                item_offset: 9,
                payload_offset: 17,
                len: (crate::core::limits::MAX_COMPRESSED_INPUT_BYTES + 1) as u32,
            },
        ],
        vec![
            DicomFragmentRef {
                item_offset: 0,
                payload_offset: 8,
                len: crate::core::limits::MAX_COMPRESSED_INPUT_BYTES as u32,
            },
            DicomFragmentRef {
                item_offset: crate::core::limits::MAX_COMPRESSED_INPUT_BYTES + 8,
                payload_offset: crate::core::limits::MAX_COMPRESSED_INPUT_BYTES + 16,
                len: 1,
            },
        ],
    ] {
        let error = preflight_compressed_frame(path, &fragments)
            .expect_err("over-limit frame must be rejected before allocation");
        assert!(
            matches!(error, WsiError::ResourceLimit { .. }),
            "expected typed resource limit, got {error:?}"
        );
    }
}

#[test]
fn compressed_frame_preflight_rejects_offset_arithmetic_overflow() {
    let error = preflight_compressed_frame(
        Path::new("compressed-frame-overflow.dcm"),
        &[DicomFragmentRef {
            item_offset: u64::MAX - 9,
            payload_offset: u64::MAX - 1,
            len: 4,
        }],
    )
    .expect_err("fragment end overflow must fail before any read or allocation");

    assert!(error.to_string().contains("offset overflow"), "{error}");
}

#[test]
fn raw_item_scan_rejects_oversized_basic_offset_table_before_reading_payload() {
    struct CountingCursor {
        inner: std::io::Cursor<Vec<u8>>,
        bytes_read: usize,
    }

    impl std::io::Read for CountingCursor {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let read = self.inner.read(buffer)?;
            self.bytes_read += read;
            Ok(read)
        }
    }

    impl std::io::Seek for CountingCursor {
        fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(position)
        }
    }

    let declared_len = u32::MAX - 3;
    let mut bytes = vec![0; EXPLICIT_VR_LONG_HEADER_LEN];
    bytes.extend_from_slice(&DICOM_ITEM_TAG_LE);
    bytes.extend_from_slice(&declared_len.to_le_bytes());
    let file_len = bytes.len() as u64 + u64::from(declared_len);
    let mut reader = CountingCursor {
        inner: std::io::Cursor::new(bytes),
        bytes_read: 0,
    };

    let error = scan_raw_encapsulated_pixel_sequence_with_reader_controlled(
        &mut reader,
        Path::new("oversized-basic-offset-table.dcm"),
        0,
        file_len,
        None,
        None,
    )
    .expect_err("the scanner must reject an oversized basic offset table");

    assert!(error.to_string().contains("exceeds safety limit"));
    assert_eq!(
        reader.bytes_read, 8,
        "only the basic offset table Item header may be read"
    );
}

#[test]
fn cancellation_during_basic_offset_table_read_stops_before_next_chunk() {
    struct CancellingTableReader {
        inner: std::io::Cursor<Vec<u8>>,
        cancellation: crate::ReadCancellationToken,
        bytes_read: usize,
    }

    impl std::io::Read for CancellingTableReader {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let reading_table_payload = self.inner.position()
                >= u64::try_from(EXPLICIT_VR_LONG_HEADER_LEN + 8).expect("header offset");
            let read = self.inner.read(buffer)?;
            self.bytes_read += read;
            if reading_table_payload && read > 0 {
                self.cancellation.cancel();
            }
            Ok(read)
        }
    }

    impl std::io::Seek for CancellingTableReader {
        fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(position)
        }
    }

    let table_len = 128 * 1024u32;
    let mut bytes = vec![0; EXPLICIT_VR_LONG_HEADER_LEN];
    bytes.extend_from_slice(&DICOM_ITEM_TAG_LE);
    bytes.extend_from_slice(&table_len.to_le_bytes());
    bytes.resize(bytes.len() + table_len as usize, 0);
    let file_len = bytes.len() as u64;
    let cancellation = crate::ReadCancellationToken::new();
    let control = crate::ReadControl::new(cancellation.clone());
    let mut reader = CancellingTableReader {
        inner: std::io::Cursor::new(bytes),
        cancellation,
        bytes_read: 0,
    };

    let error = scan_raw_encapsulated_pixel_sequence_with_reader_controlled(
        &mut reader,
        Path::new("cancelled-basic-offset-table.dcm"),
        0,
        file_len,
        Some(table_len / 4),
        Some(&control),
    )
    .expect_err("cancellation after the first table chunk must stop the scan");

    assert!(matches!(error, WsiError::Cancelled));
    assert_eq!(
        reader.bytes_read,
        8 + 64 * 1024,
        "the second table chunk must not be admitted"
    );
}

#[test]
fn raw_item_scan_cancellation_stops_before_the_next_header_admission() {
    struct CancellingCursor {
        inner: std::io::Cursor<Vec<u8>>,
        token: crate::ReadCancellationToken,
        bytes_read: usize,
    }

    impl std::io::Read for CancellingCursor {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let read = self.inner.read(buffer)?;
            self.bytes_read += read;
            if read > 0 {
                self.token.cancel();
            }
            Ok(read)
        }
    }

    impl std::io::Seek for CancellingCursor {
        fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(position)
        }
    }

    let mut bytes = Vec::new();
    bytes.extend_from_slice(&PIXEL_DATA_TAG_LE);
    bytes.extend_from_slice(b"OB");
    bytes.extend_from_slice(&[0, 0]);
    bytes.extend_from_slice(&UNDEFINED_LENGTH_LE);
    bytes.extend_from_slice(&DICOM_ITEM_TAG_LE);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    push_pixel_fragment(&mut bytes, &[1, 2, 3, 4]);
    bytes.extend_from_slice(&DICOM_SEQUENCE_DELIMITER_TAG_LE);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    let file_len = bytes.len() as u64;
    let token = crate::ReadCancellationToken::new();
    let control = crate::ReadControl::new(token.clone());
    let mut reader = CancellingCursor {
        inner: std::io::Cursor::new(bytes),
        token,
        bytes_read: 0,
    };

    let error = scan_raw_encapsulated_pixel_sequence_with_reader_controlled(
        &mut reader,
        Path::new("cancelled-item-scan.dcm"),
        0,
        file_len,
        Some(1),
        Some(&control),
    )
    .expect_err("cancellation must stop before the fragment header is admitted");

    assert!(matches!(error, WsiError::Cancelled));
    assert_eq!(reader.bytes_read, 8, "only the BOT header should be read");
}

#[test]
fn large_basic_offset_table_frame_index_builds_quickly() {
    let frame_count = 25_000usize;
    let mut fragments = Vec::with_capacity(frame_count);
    let mut offset_table = Vec::with_capacity(frame_count);
    let mut item_offset = 1024u64;
    for _ in 0..frame_count {
        offset_table.push((item_offset - 1024) as u32);
        fragments.push(DicomFragmentRef {
            payload_offset: item_offset + 8,
            item_offset,
            len: 64,
        });
        item_offset += 72;
    }

    let started = std::time::Instant::now();
    let frames = build_encapsulated_frame_index(
        Path::new("large-basic-offset-table.dcm"),
        fragments,
        offset_table,
        frame_count as u32,
    )
    .expect("large basic offset table should build");

    assert_eq!(frames.frame_ranges.len(), frame_count);
    assert_eq!(frames.frame_ranges[0], 0..1);
    assert_eq!(
        frames.frame_ranges[frame_count - 1],
        frame_count - 1..frame_count
    );
    assert!(
        started.elapsed() < std::time::Duration::from_millis(250),
        "large DICOM basic offset table frame index should build in linear time"
    );
}

#[test]
fn metadata_preflight_reports_missing_files_and_unsupported_transfer_syntaxes() {
    let missing = match open_metadata_object_until(
        Path::new("definitely-missing-dicom-metadata.dcm"),
        tags::PIXEL_DATA,
    ) {
        Ok(_) => panic!("missing DICOM metadata should fail"),
        Err(error) => error,
    };
    assert!(matches!(missing, WsiError::IoWithPath { .. }));

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("unsupported-transfer-syntax.dcm");
    super::fixtures::write_test_dicom(
        &path,
        super::fixtures::TestDicomOptions::native(super::fixtures::test_rgb_pixel_data()),
    );
    let mut bytes = std::fs::read(&path).unwrap();
    let transfer_syntax_tag = [0x02, 0x00, 0x10, 0x00];
    let tag_offset = bytes
        .windows(transfer_syntax_tag.len())
        .position(|window| window == transfer_syntax_tag)
        .expect("generated DICOM has a transfer syntax element");
    assert_eq!(&bytes[tag_offset + 4..tag_offset + 6], b"UI");
    bytes[tag_offset + 8] = b'9';
    std::fs::write(&path, bytes).unwrap();

    let error = match open_metadata_object_until(&path, tags::PIXEL_DATA) {
        Ok(_) => panic!("unsupported DICOM transfer syntax should fail"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("unsupported transfer syntax"));
}

#[test]
fn file_meta_preflight_rejects_invalid_transfer_syntax_fields() {
    let path = Path::new("invalid-file-meta.dcm");
    let transfer_syntax_tag = [0x02, 0x00, 0x10, 0x00];

    let mut invalid_utf8 = std::io::Cursor::new(explicit_file_meta(&[(
        transfer_syntax_tag,
        *b"UI",
        &[0xFF, 0],
    )]));
    let error = preflight_file_meta(&mut invalid_utf8, path).unwrap_err();
    assert!(error.to_string().contains("not UTF-8"));

    let mut missing = std::io::Cursor::new(explicit_file_meta(&[]));
    let error = preflight_file_meta(&mut missing, path).unwrap_err();
    assert!(error.to_string().contains("no transfer syntax UID"));

    let mut invalid_vr = explicit_file_meta(&[]);
    invalid_vr[8..10].copy_from_slice(b"??");
    let error = preflight_file_meta(&mut std::io::Cursor::new(invalid_vr), path).unwrap_err();
    assert!(error.to_string().contains("invalid VR in DICOM file meta"));
}

#[test]
fn file_meta_preflight_preserves_seek_failure_context_at_each_stage() {
    let path = Path::new("seek-failure.dcm");
    let empty_group = explicit_file_meta(&[]);
    let non_transfer_element = explicit_file_meta(&[([0x02, 0x00, 0x12, 0x00], *b"UI", b"x\0")]);
    let cases = [
        (vec![0; 132], SeekFailure::First),
        (vec![0; 132], SeekFailure::Preamble),
        (non_transfer_element, SeekFailure::SkipValue),
        (empty_group.clone(), SeekFailure::StreamPosition),
        (empty_group.clone(), SeekFailure::End),
        (empty_group, SeekFailure::DatasetRewind),
    ];

    for (bytes, failure) in cases {
        let mut reader = reader_with_seek_failure(bytes, failure);
        let error = preflight_file_meta(&mut reader, path).unwrap_err();
        assert!(matches!(error, WsiError::IoWithPath { .. }));
        assert!(error.to_string().contains("seek-failure.dcm"));
        assert!(error.to_string().contains("injected seek failure"));
    }
}
