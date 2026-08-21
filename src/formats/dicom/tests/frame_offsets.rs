use super::*;
fn push_explicit_vr_long_element(bytes: &mut Vec<u8>, tag: [u8; 4], vr: &[u8; 2], value: &[u8]) {
    bytes.extend_from_slice(&tag);
    bytes.extend_from_slice(vr);
    bytes.extend_from_slice(&[0, 0]);
    bytes.extend_from_slice(&(value.len() as u32).to_le_bytes());
    bytes.extend_from_slice(value);
}

pub(super) fn push_pixel_fragment(bytes: &mut Vec<u8>, payload: &[u8]) -> u64 {
    let item_offset = bytes.len() as u64;
    bytes.extend_from_slice(&DICOM_ITEM_TAG_LE);
    bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    bytes.extend_from_slice(payload);
    item_offset
}

#[test]
fn raw_encapsulated_scan_handles_extended_offset_table_layout() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("raw-eot-htj2k.dcm");
    let first = [0xFF, 0x4F, 0x01, 0x02];
    let second = [0xFF, 0x4F, 0x03, 0x04, 0x05, 0x06];
    let mut bytes = vec![0; 132];
    bytes[128..132].copy_from_slice(b"DICM");

    let mut eot = Vec::new();
    eot.extend_from_slice(&0u64.to_le_bytes());
    eot.extend_from_slice(&(first.len() as u64 + 8).to_le_bytes());
    push_explicit_vr_long_element(&mut bytes, [0xE0, 0x7F, 0x01, 0x00], b"OV", &eot);

    let mut eot_lengths = Vec::new();
    eot_lengths.extend_from_slice(&(first.len() as u64).to_le_bytes());
    eot_lengths.extend_from_slice(&(second.len() as u64).to_le_bytes());
    push_explicit_vr_long_element(&mut bytes, [0xE0, 0x7F, 0x02, 0x00], b"OV", &eot_lengths);

    bytes.extend_from_slice(&PIXEL_DATA_TAG_LE);
    bytes.extend_from_slice(b"OB");
    bytes.extend_from_slice(&[0, 0]);
    bytes.extend_from_slice(&UNDEFINED_LENGTH_LE);
    bytes.extend_from_slice(&DICOM_ITEM_TAG_LE);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    let first_item_offset = push_pixel_fragment(&mut bytes, &first);
    let second_item_offset = push_pixel_fragment(&mut bytes, &second);
    bytes.extend_from_slice(&DICOM_SEQUENCE_DELIMITER_TAG_LE);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    std::fs::write(&path, bytes).unwrap();

    let frames = scan_encapsulated_frames_raw_little_endian(&path, 2)
        .expect("raw scan succeeds")
        .expect("Pixel Data is found");
    assert_eq!(frames.frame_ranges, vec![0..1, 1..2]);
    assert_eq!(frames.fragments.len(), 2);
    assert_eq!(frames.fragments[0].item_offset, first_item_offset);
    assert_eq!(frames.fragments[0].len, first.len() as u32);
    assert_eq!(frames.fragments[1].item_offset, second_item_offset);
    assert_eq!(frames.fragments[1].len, second.len() as u32);
}

#[test]
fn controlled_indexing_reports_basic_offset_table_mapping() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("raw-bot-diagnostics.dcm");
    let frames = [[0xFF, 0x4F, 0x01, 0x02], [0xFF, 0x4F, 0x03, 0x04]];
    let mut bytes = vec![0; 132];
    bytes[128..132].copy_from_slice(b"DICM");
    bytes.extend_from_slice(&PIXEL_DATA_TAG_LE);
    bytes.extend_from_slice(b"OB");
    bytes.extend_from_slice(&[0, 0]);
    bytes.extend_from_slice(&UNDEFINED_LENGTH_LE);
    bytes.extend_from_slice(&DICOM_ITEM_TAG_LE);
    bytes.extend_from_slice(&8u32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&12u32.to_le_bytes());
    for frame in &frames {
        push_pixel_fragment(&mut bytes, frame);
    }
    bytes.extend_from_slice(&DICOM_SEQUENCE_DELIMITER_TAG_LE);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    std::fs::write(&path, bytes).unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&events);
    let control = crate::ReadControl::default().with_diagnostic_sink(Arc::new(
        move |event: crate::DicomIndexDiagnostic| captured.lock().unwrap().push(event),
    ));

    let index = scan_encapsulated_frames_controlled(
        &path,
        uids::EXPLICIT_VR_LITTLE_ENDIAN,
        2,
        Some(&control),
    )
    .expect("fast indexing should resolve the nonzero BOT offset once");

    assert_eq!(index.frame_ranges, vec![0..1, 1..2]);
    let outcomes = events
        .lock()
        .unwrap()
        .iter()
        .map(|event| event.outcome)
        .collect::<Vec<_>>();
    assert_eq!(
        outcomes,
        vec![crate::DicomIndexOutcome::BuiltFast {
            mapping: crate::DicomIndexMapping::BasicOffsetTableItems,
        }]
    );
}

#[test]
fn raw_encapsulated_scan_uses_extended_offsets_for_multi_fragment_frames() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("raw-eot-multi-fragment.dcm");
    let fragments = [[0xFF, 0x4F], [0x01, 0x02], [0xFF, 0x4F], [0x03, 0x04]];
    let mut bytes = vec![0; 132];
    bytes[128..132].copy_from_slice(b"DICM");

    let second_frame_offset = 2 * (8 + fragments[0].len() as u64);
    let mut eot = Vec::new();
    eot.extend_from_slice(&0u64.to_le_bytes());
    eot.extend_from_slice(&second_frame_offset.to_le_bytes());
    push_explicit_vr_long_element(&mut bytes, [0xE0, 0x7F, 0x01, 0x00], b"OV", &eot);

    let mut eot_lengths = Vec::new();
    eot_lengths.extend_from_slice(&4u64.to_le_bytes());
    eot_lengths.extend_from_slice(&4u64.to_le_bytes());
    push_explicit_vr_long_element(&mut bytes, [0xE0, 0x7F, 0x02, 0x00], b"OV", &eot_lengths);

    bytes.extend_from_slice(&PIXEL_DATA_TAG_LE);
    bytes.extend_from_slice(b"OB");
    bytes.extend_from_slice(&[0, 0]);
    bytes.extend_from_slice(&UNDEFINED_LENGTH_LE);
    bytes.extend_from_slice(&DICOM_ITEM_TAG_LE);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    for fragment in &fragments {
        push_pixel_fragment(&mut bytes, fragment);
    }
    bytes.extend_from_slice(&DICOM_SEQUENCE_DELIMITER_TAG_LE);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    std::fs::write(&path, bytes).unwrap();

    let frames = scan_encapsulated_frames_raw_little_endian(&path, 2)
        .expect("raw scan succeeds")
        .expect("Pixel Data is found");
    assert_eq!(frames.frame_ranges, vec![0..2, 2..4]);
}

#[test]
fn raw_encapsulated_scan_rejects_pixel_data_pattern_inside_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("raw-false-pixel-data-pattern.dcm");
    let frame = [0xFF, 0x4F, 0x01, 0x02];
    let mut bytes = vec![0; 132];
    bytes[128..132].copy_from_slice(b"DICM");

    let mut metadata_value = Vec::new();
    metadata_value.extend_from_slice(&PIXEL_DATA_TAG_LE);
    metadata_value.extend_from_slice(b"OB");
    metadata_value.extend_from_slice(&[0, 0]);
    metadata_value.extend_from_slice(&UNDEFINED_LENGTH_LE);
    metadata_value.extend_from_slice(&[0; 16]);
    push_explicit_vr_long_element(&mut bytes, [0x11, 0x00, 0x10, 0x10], b"OB", &metadata_value);

    bytes.extend_from_slice(&PIXEL_DATA_TAG_LE);
    bytes.extend_from_slice(b"OB");
    bytes.extend_from_slice(&[0, 0]);
    bytes.extend_from_slice(&UNDEFINED_LENGTH_LE);
    bytes.extend_from_slice(&DICOM_ITEM_TAG_LE);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    let item_offset = push_pixel_fragment(&mut bytes, &frame);
    bytes.extend_from_slice(&DICOM_SEQUENCE_DELIMITER_TAG_LE);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    std::fs::write(&path, bytes).unwrap();

    let frames = scan_encapsulated_frames_raw_little_endian(&path, 1)
        .expect("raw scan skips the false metadata candidate")
        .expect("real Pixel Data is found");
    assert_eq!(frames.frame_ranges, vec![0..1]);
    assert_eq!(frames.fragments[0].item_offset, item_offset);
}

#[test]
fn raw_encapsulated_scan_rejects_complete_pixel_sequence_inside_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("raw-complete-false-pixel-sequence.dcm");
    let fake_frame = [0xDE, 0xAD, 0xBE, 0xEF];
    let real_frame = [0xFF, 0x4F, 0x01, 0x02];
    let mut bytes = vec![0; 132];
    bytes[128..132].copy_from_slice(b"DICM");

    let mut metadata_value = Vec::new();
    metadata_value.extend_from_slice(&PIXEL_DATA_TAG_LE);
    metadata_value.extend_from_slice(b"OB");
    metadata_value.extend_from_slice(&[0, 0]);
    metadata_value.extend_from_slice(&UNDEFINED_LENGTH_LE);
    metadata_value.extend_from_slice(&DICOM_ITEM_TAG_LE);
    metadata_value.extend_from_slice(&0u32.to_le_bytes());
    push_pixel_fragment(&mut metadata_value, &fake_frame);
    metadata_value.extend_from_slice(&DICOM_SEQUENCE_DELIMITER_TAG_LE);
    metadata_value.extend_from_slice(&0u32.to_le_bytes());
    push_explicit_vr_long_element(&mut bytes, [0x11, 0x00, 0x10, 0x10], b"OB", &metadata_value);

    bytes.extend_from_slice(&PIXEL_DATA_TAG_LE);
    bytes.extend_from_slice(b"OB");
    bytes.extend_from_slice(&[0, 0]);
    bytes.extend_from_slice(&UNDEFINED_LENGTH_LE);
    bytes.extend_from_slice(&DICOM_ITEM_TAG_LE);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    let real_item_offset = push_pixel_fragment(&mut bytes, &real_frame);
    bytes.extend_from_slice(&DICOM_SEQUENCE_DELIMITER_TAG_LE);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    std::fs::write(&path, bytes).unwrap();

    let frames = scan_encapsulated_frames_raw_little_endian(&path, 1)
        .expect("raw scan skips a complete false metadata sequence")
        .expect("real Pixel Data is found");

    assert_eq!(frames.frame_ranges, vec![0..1]);
    assert_eq!(frames.fragments[0].item_offset, real_item_offset);
}

#[test]
fn extended_offset_direct_path_rejects_invalid_intermediate_item_header() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("raw-eot-invalid-middle-item.dcm");
    let mut bytes = vec![0; 132];
    bytes[128..132].copy_from_slice(b"DICM");

    let mut offsets = Vec::new();
    for offset in [0u64, 12, 24] {
        offsets.extend_from_slice(&offset.to_le_bytes());
    }
    push_explicit_vr_long_element(&mut bytes, EXTENDED_OFFSET_TABLE_TAG_LE, b"OV", &offsets);
    let mut lengths = Vec::new();
    for length in [4u64, 4, 4] {
        lengths.extend_from_slice(&length.to_le_bytes());
    }
    push_explicit_vr_long_element(
        &mut bytes,
        EXTENDED_OFFSET_TABLE_LENGTHS_TAG_LE,
        b"OV",
        &lengths,
    );

    bytes.extend_from_slice(&PIXEL_DATA_TAG_LE);
    bytes.extend_from_slice(b"OB");
    bytes.extend_from_slice(&[0, 0]);
    bytes.extend_from_slice(&UNDEFINED_LENGTH_LE);
    bytes.extend_from_slice(&DICOM_ITEM_TAG_LE);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&DICOM_ITEM_TAG_LE);
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&[1, 2, 3, 4]);
    bytes.extend_from_slice(&[0xAA; 8]);
    bytes.extend_from_slice(&[5, 6, 7, 8]);
    push_pixel_fragment(&mut bytes, &[9, 10, 11, 12]);
    bytes.extend_from_slice(&DICOM_SEQUENCE_DELIMITER_TAG_LE);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    std::fs::write(&path, bytes).unwrap();

    scan_encapsulated_frames_raw_little_endian(&path, 3)
        .expect_err("an EOT offset into payload bytes must not be accepted as an Item");
}

#[test]
fn extended_offset_table_values_are_bounds_checked_before_reading() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("raw-eot-values-out-of-bounds.dcm");
    std::fs::write(&path, vec![0u8; 64]).unwrap();
    let mut file = File::open(&path).unwrap();

    let error = read_extended_offset_tables_le(&mut file, &path, Some(56), Some(16), 16, None)
        .expect_err("EOT values beyond EOF must fail before allocation/read");

    assert!(error.to_string().contains("outside the source file"));
}

#[test]
fn extended_fragment_padding_rejects_u32_overflow() {
    let error = checked_padded_fragment_len(
        Path::new("overflowing-extended-length.dcm"),
        0,
        u64::from(u32::MAX),
    )
    .expect_err("odd u32::MAX payload length cannot be represented after padding");

    assert!(error.to_string().contains("padded length"));
}

#[test]
fn malformed_extended_offsets_fall_back_to_valid_basic_offsets() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("raw-invalid-eot-valid-bot.dcm");
    let frames = [[0xFF, 0x4F, 0x01, 0x02], [0xFF, 0x4F, 0x03, 0x04]];
    let mut bytes = vec![0; 132];
    bytes[128..132].copy_from_slice(b"DICM");

    let mut invalid_eot = Vec::new();
    invalid_eot.extend_from_slice(&0u64.to_le_bytes());
    invalid_eot.extend_from_slice(&1u64.to_le_bytes());
    push_explicit_vr_long_element(
        &mut bytes,
        EXTENDED_OFFSET_TABLE_TAG_LE,
        b"OV",
        &invalid_eot,
    );
    let mut lengths = Vec::new();
    lengths.extend_from_slice(&4u64.to_le_bytes());
    lengths.extend_from_slice(&4u64.to_le_bytes());
    push_explicit_vr_long_element(
        &mut bytes,
        EXTENDED_OFFSET_TABLE_LENGTHS_TAG_LE,
        b"OV",
        &lengths,
    );

    bytes.extend_from_slice(&PIXEL_DATA_TAG_LE);
    bytes.extend_from_slice(b"OB");
    bytes.extend_from_slice(&[0, 0]);
    bytes.extend_from_slice(&UNDEFINED_LENGTH_LE);
    bytes.extend_from_slice(&DICOM_ITEM_TAG_LE);
    bytes.extend_from_slice(&8u32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&(8u32 + frames[0].len() as u32).to_le_bytes());
    for frame in &frames {
        push_pixel_fragment(&mut bytes, frame);
    }
    bytes.extend_from_slice(&DICOM_SEQUENCE_DELIMITER_TAG_LE);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    std::fs::write(&path, bytes).unwrap();

    let index = scan_encapsulated_frames_raw_little_endian(&path, 2)
        .expect("valid BOT safely replaces malformed EOT")
        .expect("Pixel Data is found");
    assert_eq!(index.frame_ranges, vec![0..1, 1..2]);
}

#[test]
fn malformed_extended_offsets_without_safe_mapping_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("raw-invalid-eot-no-fallback.dcm");
    let fragments = [[1, 2], [3, 4], [5, 6], [7, 8]];
    let mut bytes = vec![0; 132];
    bytes[128..132].copy_from_slice(b"DICM");

    let mut invalid_eot = Vec::new();
    invalid_eot.extend_from_slice(&0u64.to_le_bytes());
    invalid_eot.extend_from_slice(&1u64.to_le_bytes());
    push_explicit_vr_long_element(
        &mut bytes,
        EXTENDED_OFFSET_TABLE_TAG_LE,
        b"OV",
        &invalid_eot,
    );
    let mut lengths = Vec::new();
    lengths.extend_from_slice(&4u64.to_le_bytes());
    lengths.extend_from_slice(&4u64.to_le_bytes());
    push_explicit_vr_long_element(
        &mut bytes,
        EXTENDED_OFFSET_TABLE_LENGTHS_TAG_LE,
        b"OV",
        &lengths,
    );
    bytes.extend_from_slice(&PIXEL_DATA_TAG_LE);
    bytes.extend_from_slice(b"OB");
    bytes.extend_from_slice(&[0, 0]);
    bytes.extend_from_slice(&UNDEFINED_LENGTH_LE);
    bytes.extend_from_slice(&DICOM_ITEM_TAG_LE);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    for fragment in &fragments {
        push_pixel_fragment(&mut bytes, fragment);
    }
    bytes.extend_from_slice(&DICOM_SEQUENCE_DELIMITER_TAG_LE);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    std::fs::write(&path, bytes).unwrap();

    let error = scan_encapsulated_frames_raw_little_endian(&path, 2)
        .expect_err("malformed EOT without BOT/item mapping must fail");
    assert!(error.to_string().contains("extended offset table"));
}

#[test]
fn basic_offset_table_maps_a_nonzero_second_frame_offset_once() {
    let frames = build_encapsulated_frame_index(
        Path::new("two-frame-basic-offset-table.dcm"),
        vec![
            DicomFragmentRef {
                payload_offset: 108,
                item_offset: 100,
                len: 4,
            },
            DicomFragmentRef {
                payload_offset: 120,
                item_offset: 112,
                len: 4,
            },
        ],
        vec![0, 12],
        2,
    )
    .expect("the BOT offset is relative to the first fragment Item exactly once");

    assert_eq!(frames.frame_ranges, vec![0..1, 1..2]);
}

#[test]
fn extended_offset_validation_rejects_non_monotonic_and_overflowing_offsets() {
    let path = Path::new("malformed-extended-offsets.dcm");
    let fragments = vec![
        DicomFragmentRef {
            payload_offset: 108,
            item_offset: 100,
            len: 4,
        },
        DicomFragmentRef {
            payload_offset: 120,
            item_offset: 112,
            len: 4,
        },
    ];
    let non_monotonic = DicomExtendedOffsetTables {
        offsets: vec![0, 0],
        lengths: vec![4, 4],
    };
    let error = frame_ranges_from_extended_offsets(path, &fragments, &non_monotonic, 2)
        .expect_err("non-monotonic EOT must fail");
    assert!(error.to_string().contains("strictly increasing"));

    let overflowing = DicomExtendedOffsetTables {
        offsets: vec![0, u64::MAX],
        lengths: vec![4, 4],
    };
    let error = frame_ranges_from_extended_offsets(path, &fragments, &overflowing, 2)
        .expect_err("overflowing EOT must fail");
    assert!(error.to_string().contains("overflow"));
}
