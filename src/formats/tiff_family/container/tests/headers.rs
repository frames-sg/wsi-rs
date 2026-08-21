use super::*;

#[test]
fn parse_classic_le_header() {
    let entries = vec![SyntheticEntry {
        tag: tags::IMAGE_WIDTH,
        tiff_type: 4, // LONG
        count: 1,
        inline_data: Some(vec![0, 4, 0, 0]), // 1024 LE
        out_of_line_data: None,
    }];
    let data = make_classic_tiff_single(Endian::Little, &entries);
    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();
    assert_eq!(container.endian(), Endian::Little);
    assert!(!container.is_bigtiff());
    assert!(!container.is_ndpi());
}

#[test]
fn parse_classic_be_header() {
    let entries = vec![SyntheticEntry {
        tag: tags::IMAGE_WIDTH,
        tiff_type: 4,
        count: 1,
        inline_data: Some(vec![0, 0, 4, 0]), // 1024 BE
        out_of_line_data: None,
    }];
    let data = make_classic_tiff_single(Endian::Big, &entries);
    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();
    assert_eq!(container.endian(), Endian::Big);
    assert!(!container.is_bigtiff());
}

#[test]
fn parse_bigtiff_le_header() {
    let entries = vec![SyntheticEntry {
        tag: tags::IMAGE_WIDTH,
        tiff_type: 16, // LONG8
        count: 1,
        inline_data: Some(vec![0, 4, 0, 0, 0, 0, 0, 0]),
        out_of_line_data: None,
    }];
    let data = make_bigtiff_single(Endian::Little, &entries);
    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();
    assert_eq!(container.endian(), Endian::Little);
    assert!(container.is_bigtiff());
}

#[test]
fn parse_bigtiff_be_header() {
    let entries = vec![SyntheticEntry {
        tag: tags::IMAGE_WIDTH,
        tiff_type: 16,
        count: 1,
        inline_data: Some(vec![0, 0, 0, 0, 0, 0, 4, 0]),
        out_of_line_data: None,
    }];
    let data = make_bigtiff_single(Endian::Big, &entries);
    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();
    assert_eq!(container.endian(), Endian::Big);
    assert!(container.is_bigtiff());
}

#[test]
fn reject_invalid_magic() {
    let mut data = vec![b'I', b'I'];
    data.extend_from_slice(&99u16.to_le_bytes()); // bad magic
    data.extend_from_slice(&8u32.to_le_bytes()); // dummy offset
    let tmp = write_tiff_tempfile(&data);
    let result = TiffContainer::open(tmp.path());
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        matches!(err, TiffParseError::Structure(_)),
        "got: {:?}",
        err
    );
}

#[test]
fn reject_bad_bigtiff_offset_size() {
    let mut data = Vec::new();
    data.extend_from_slice(b"II");
    data.extend_from_slice(&43u16.to_le_bytes()); // BigTIFF magic
    data.extend_from_slice(&4u16.to_le_bytes()); // wrong offset size (should be 8)
    data.extend_from_slice(&0u16.to_le_bytes()); // reserved
    data.extend_from_slice(&16u64.to_le_bytes()); // first IFD offset
    let tmp = write_tiff_tempfile(&data);
    let result = TiffContainer::open(tmp.path());
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), TiffParseError::Structure(_)));
}

#[test]
fn pread_bounds_check_rejects_out_of_bounds() {
    // Create a minimal valid TIFF so TiffContainer::open() succeeds
    let entries = vec![SyntheticEntry {
        tag: tags::IMAGE_WIDTH,
        tiff_type: 4,
        count: 1,
        inline_data: Some(vec![0, 1, 0, 0]),
        out_of_line_data: None,
    }];
    let data = make_classic_tiff_single(Endian::Little, &entries);
    let file_len = data.len() as u64;
    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();
    // Try to read beyond file end
    let result = container.pread(file_len - 2, 10);
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), TiffParseError::Bounds { .. }));
}

#[test]
fn pread_offset_overflow_rejected() {
    let entries = vec![SyntheticEntry {
        tag: tags::IMAGE_WIDTH,
        tiff_type: 4,
        count: 1,
        inline_data: Some(vec![0, 1, 0, 0]),
        out_of_line_data: None,
    }];
    let data = make_classic_tiff_single(Endian::Little, &entries);
    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();
    // Offset that would overflow u64
    let result = container.pread(u64::MAX, 10);
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), TiffParseError::Bounds { .. }));
}

// ── Multi-IFD test helper ─────────────────────────────────
