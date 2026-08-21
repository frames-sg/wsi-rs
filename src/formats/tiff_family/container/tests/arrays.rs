use super::*;

#[test]
fn get_u64_array_from_long() {
    // Two LONGs = 8 bytes -> out-of-line for classic
    let mut ool = Vec::new();
    ool.extend_from_slice(&100u32.to_le_bytes());
    ool.extend_from_slice(&200u32.to_le_bytes());

    let entries = vec![SyntheticEntry {
        tag: tags::TILE_OFFSETS,
        tiff_type: 4, // LONG
        count: 2,
        inline_data: None,
        out_of_line_data: Some(ool),
    }];
    let data = make_classic_tiff_single(Endian::Little, &entries);
    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();
    let ifd_id = container.top_ifds()[0];
    let arr = container.get_u64_array(ifd_id, tags::TILE_OFFSETS).unwrap();
    assert_eq!(arr, &[100, 200]);
}

#[test]
fn get_u64_array_from_long8() {
    let mut ool = Vec::new();
    ool.extend_from_slice(&5_000_000_000u64.to_le_bytes());
    ool.extend_from_slice(&6_000_000_000u64.to_le_bytes());

    let entries = vec![SyntheticEntry {
        tag: tags::TILE_OFFSETS,
        tiff_type: 16, // LONG8
        count: 2,
        inline_data: None,
        out_of_line_data: Some(ool),
    }];
    let data = make_bigtiff_single(Endian::Little, &entries);
    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();
    let ifd_id = container.top_ifds()[0];
    let arr = container.get_u64_array(ifd_id, tags::TILE_OFFSETS).unwrap();
    assert_eq!(arr, &[5_000_000_000, 6_000_000_000]);
}

#[test]
fn get_u64_array_cached_pointer_equality() {
    let mut ool = Vec::new();
    ool.extend_from_slice(&100u32.to_le_bytes());
    ool.extend_from_slice(&200u32.to_le_bytes());

    let entries = vec![SyntheticEntry {
        tag: tags::TILE_OFFSETS,
        tiff_type: 4,
        count: 2,
        inline_data: None,
        out_of_line_data: Some(ool),
    }];
    let data = make_classic_tiff_single(Endian::Little, &entries);
    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();
    let ifd_id = container.top_ifds()[0];

    let arr1 = container.get_u64_array(ifd_id, tags::TILE_OFFSETS).unwrap();
    let arr2 = container.get_u64_array(ifd_id, tags::TILE_OFFSETS).unwrap();
    // Same pointer — cached, not re-decoded
    assert_eq!(arr1.as_ptr(), arr2.as_ptr());
}

#[test]
fn get_u64_array_type_mismatch() {
    let entries = vec![SyntheticEntry {
        tag: tags::IMAGE_DESCRIPTION,
        tiff_type: 2, // ASCII
        count: 4,
        inline_data: Some(b"foo\0".to_vec()),
        out_of_line_data: None,
    }];
    let data = make_classic_tiff_single(Endian::Little, &entries);
    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();
    let ifd_id = container.top_ifds()[0];
    let result = container.get_u64_array(ifd_id, tags::IMAGE_DESCRIPTION);
    assert!(matches!(
        result.unwrap_err(),
        TiffParseError::InvalidTag { .. }
    ));
}
