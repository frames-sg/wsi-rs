use super::*;

#[test]
fn resolve_inline_tag_returns_bytes() {
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

    let ifd_id = container.top_ifds()[0];
    let bytes = container.resolve_tag(ifd_id, tags::IMAGE_WIDTH).unwrap();
    assert_eq!(bytes, &[0, 4, 0, 0]);
}

#[test]
fn resolve_lazy_tag_triggers_io() {
    // Create a tag with out-of-line data (>4 bytes for classic)
    let ool_data: Vec<u8> = vec![1, 0, 0, 0, 2, 0, 0, 0]; // two LONGs
    let entries = vec![SyntheticEntry {
        tag: tags::TILE_OFFSETS,
        tiff_type: 4, // LONG
        count: 2,
        inline_data: None,
        out_of_line_data: Some(ool_data.clone()),
    }];
    let data = make_classic_tiff_single(Endian::Little, &entries);
    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();

    let ifd_id = container.top_ifds()[0];
    let bytes = container.resolve_tag(ifd_id, tags::TILE_OFFSETS).unwrap();
    assert_eq!(bytes, &ool_data);
}

#[test]
fn resolve_lazy_tag_cached() {
    let ool_data: Vec<u8> = vec![1, 0, 0, 0, 2, 0, 0, 0];
    let entries = vec![SyntheticEntry {
        tag: tags::TILE_OFFSETS,
        tiff_type: 4,
        count: 2,
        inline_data: None,
        out_of_line_data: Some(ool_data.clone()),
    }];
    let data = make_classic_tiff_single(Endian::Little, &entries);
    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();

    let ifd_id = container.top_ifds()[0];
    let bytes1 = container.resolve_tag(ifd_id, tags::TILE_OFFSETS).unwrap();
    let bytes2 = container.resolve_tag(ifd_id, tags::TILE_OFFSETS).unwrap();
    // Same slice returned (same OnceLock)
    assert_eq!(bytes1.as_ptr(), bytes2.as_ptr());
}

#[test]
fn resolve_tag_not_found() {
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

    let ifd_id = container.top_ifds()[0];
    let result = container.resolve_tag(ifd_id, tags::TILE_OFFSETS);
    assert!(matches!(
        result.unwrap_err(),
        TiffParseError::TagNotFound { .. }
    ));
}

#[test]
fn inline_classification_correct() {
    // 1 LONG = 4 bytes -> inline in classic TIFF (slot_size=4)
    let entries = vec![SyntheticEntry {
        tag: tags::IMAGE_WIDTH,
        tiff_type: 4,
        count: 1,
        inline_data: Some(vec![0, 4, 0, 0]),
        out_of_line_data: None,
    }];
    let data = make_classic_tiff_single(Endian::Little, &entries);
    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();
    let ifd_id = container.top_ifds()[0];
    let ifd = container.ifd_by_id(ifd_id).unwrap();
    let entry = ifd.tags.get(&tags::IMAGE_WIDTH).unwrap();
    assert!(matches!(entry.value, TagValue::Inline(_)));
}

// ── Typed scalar accessor tests ───────────────────────────
