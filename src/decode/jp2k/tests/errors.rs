use super::*;

#[test]
fn decode_jp2k_rejects_empty_data() {
    let result = decode_jp2k(&[], 8, 8, Jp2kColorSpace::Rgb);
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("empty"), "unexpected error: {msg}");
}

#[test]
fn decode_jp2k_rejects_invalid_data() {
    let result = decode_jp2k(&[0xFF; 100], 8, 8, Jp2kColorSpace::Rgb);
    assert!(result.is_err());
}

#[test]
fn decode_jp2k_rejects_truncated_stream() {
    let mut buf = vec![0xFF, 0x4F, 0xFF, 0x51];
    buf.extend_from_slice(&[0x00; 50]);
    let result = decode_jp2k(&buf, 8, 8, Jp2kColorSpace::Rgb);
    assert!(result.is_err());
}

#[test]
fn colorspace_enum_values() {
    assert_ne!(Jp2kColorSpace::Rgb, Jp2kColorSpace::YCbCr);
    assert_eq!(Jp2kColorSpace::Rgb, Jp2kColorSpace::Rgb);
}

#[test]
fn dimensions_from_bounds_respects_origin_offsets() {
    assert_eq!(dimensions_from_bounds(10, 18, 20, 32), Some((8, 12)));
    assert_eq!(dimensions_from_bounds(5, 4, 0, 1), None);
}

#[test]
fn rgb_output_rejects_dimension_arithmetic_overflow() {
    let error = super::super::output::sample_buffer_from_rgb8_bytes(
        Vec::new(),
        u32::MAX,
        u32::MAX,
        u32::MAX,
        u32::MAX,
        Jp2kColorSpace::Rgb,
    )
    .unwrap_err();
    assert!(error.to_string().contains("size overflow"));
}
