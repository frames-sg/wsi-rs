use super::*;

#[test]
fn get_u32_from_short() {
    let entries = vec![SyntheticEntry {
        tag: tags::BITS_PER_SAMPLE,
        tiff_type: 3, // SHORT
        count: 1,
        inline_data: Some(vec![8, 0, 0, 0]), // 8 LE
        out_of_line_data: None,
    }];
    let data = make_classic_tiff_single(Endian::Little, &entries);
    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();
    let ifd_id = container.top_ifds()[0];
    assert_eq!(container.get_u32(ifd_id, tags::BITS_PER_SAMPLE).unwrap(), 8);
}

#[test]
fn get_u32_from_long() {
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
    assert_eq!(container.get_u32(ifd_id, tags::IMAGE_WIDTH).unwrap(), 1024);
}

#[test]
fn get_u64_from_long8() {
    let entries = vec![SyntheticEntry {
        tag: tags::IMAGE_WIDTH,
        tiff_type: 16, // LONG8
        count: 1,
        inline_data: Some(vec![0, 0, 0, 1, 0, 0, 0, 0]), // 16777216 LE
        out_of_line_data: None,
    }];
    let data = make_bigtiff_single(Endian::Little, &entries);
    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();
    let ifd_id = container.top_ifds()[0];
    assert_eq!(
        container.get_u64(ifd_id, tags::IMAGE_WIDTH).unwrap(),
        16777216
    );
}

#[test]
fn get_f64_from_rational() {
    let mut rational_bytes = Vec::new();
    rational_bytes.extend_from_slice(&72u32.to_le_bytes());
    rational_bytes.extend_from_slice(&1u32.to_le_bytes());

    let entries = vec![SyntheticEntry {
        tag: 282,
        tiff_type: 5, // RATIONAL
        count: 1,
        inline_data: None,
        out_of_line_data: Some(rational_bytes),
    }];
    let data = make_classic_tiff_single(Endian::Little, &entries);
    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();
    let ifd_id = container.top_ifds()[0];
    let val = container.get_f64(ifd_id, 282).unwrap();
    assert!((val - 72.0).abs() < f64::EPSILON);
}

#[test]
fn get_f64_from_float() {
    let float_val: f32 = std::f32::consts::PI;
    let entries = vec![SyntheticEntry {
        tag: 500,      // arbitrary tag
        tiff_type: 11, // FLOAT
        count: 1,
        inline_data: Some(float_val.to_le_bytes().to_vec()),
        out_of_line_data: None,
    }];
    let data = make_classic_tiff_single(Endian::Little, &entries);
    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();
    let ifd_id = container.top_ifds()[0];
    let val = container.get_f64(ifd_id, 500).unwrap();
    assert!(
        (val - f64::from(std::f32::consts::PI)).abs() < 0.001,
        "got: {}",
        val
    );
}

#[test]
fn get_string_ascii() {
    let text = b"Hello\0";
    let entries = vec![SyntheticEntry {
        tag: tags::IMAGE_DESCRIPTION,
        tiff_type: 2, // ASCII
        count: text.len() as u64,
        inline_data: None,
        out_of_line_data: Some(text.to_vec()),
    }];
    let data = make_classic_tiff_single(Endian::Little, &entries);
    let tmp = write_tiff_tempfile(&data);
    let container = TiffContainer::open(tmp.path()).unwrap();
    let ifd_id = container.top_ifds()[0];
    let s = container
        .get_string(ifd_id, tags::IMAGE_DESCRIPTION)
        .unwrap();
    assert_eq!(s, "Hello");
}

#[test]
fn get_u32_type_mismatch() {
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
    let result = container.get_u32(ifd_id, tags::IMAGE_DESCRIPTION);
    assert!(matches!(
        result.unwrap_err(),
        TiffParseError::InvalidTag { .. }
    ));
}

// ── Typed array accessor tests ────────────────────────────
