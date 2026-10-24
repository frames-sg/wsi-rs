use super::*;

#[test]
fn patch_jpeg_dimensions_overwrites_zero_sized_sof() {
    let jpeg = vec![
        0xFF, 0xD8, // SOI
        0xFF, 0xC0, // SOF0
        0x00, 0x11, // length
        0x08, // precision
        0x00, 0x00, // height
        0x00, 0x00, // width
        0x03, // components
        0x01, 0x11, 0x00, 0x02, 0x11, 0x00, 0x03, 0x11, 0x00,
    ];

    let patched = patch_jpeg_dimensions(&jpeg, 512, 256, false);
    let patched = patched.as_ref();
    assert_eq!(&patched[7..9], &256u16.to_be_bytes());
    assert_eq!(&patched[9..11], &512u16.to_be_bytes());

    // Original input is unchanged.
    assert_eq!(&jpeg[7..9], &[0, 0]);
    assert_eq!(&jpeg[9..11], &[0, 0]);
}

#[test]
fn patch_jpeg_dimensions_leaves_nonzero_sof_alone() {
    let jpeg = vec![
        0xFF, 0xD8, // SOI
        0xFF, 0xC0, // SOF0
        0x00, 0x11, // length
        0x08, // precision
        0x01, 0x00, // height
        0x02, 0x00, // width
        0x03, // components
        0x01, 0x11, 0x00, 0x02, 0x11, 0x00, 0x03, 0x11, 0x00,
    ];

    let patched = patch_jpeg_dimensions(&jpeg, 512, 256, false);
    assert!(matches!(patched, Cow::Borrowed(_)));
}

#[test]
fn patch_jpeg_dimensions_forces_nonzero_sof_when_requested() {
    let jpeg = vec![
        0xFF, 0xD8, // SOI
        0xFF, 0xC0, // SOF0
        0x00, 0x11, // length
        0x08, // precision
        0x00, 0x10, // height = 16
        0x04, 0x00, // width = 1024
        0x03, // components
        0x01, 0x11, 0x00, 0x02, 0x11, 0x00, 0x03, 0x11, 0x00,
    ];

    let patched = patch_jpeg_dimensions(&jpeg, 1024, 4, true);
    let patched = patched.as_ref();
    assert_eq!(&patched[7..9], &4u16.to_be_bytes());
    assert_eq!(&patched[9..11], &1024u16.to_be_bytes());
}

#[test]
fn ensure_jpeg_eoi_appends_missing_marker() {
    let jpeg = vec![0xFF, 0xD8, 0x00, 0x01];
    let repaired = ensure_jpeg_eoi(&jpeg);
    assert_eq!(
        repaired.as_ref()[repaired.as_ref().len() - 2..],
        [0xFF, 0xD9]
    );
}

#[test]
fn ensure_jpeg_eoi_keeps_valid_trailer() {
    let jpeg = vec![0xFF, 0xD8, 0xFF, 0xD9];
    let repaired = ensure_jpeg_eoi(&jpeg);
    assert!(matches!(repaired, Cow::Borrowed(_)));
}

#[test]
fn jpeg_preparation_length_reserves_space_for_repaired_eoi() {
    let limit = crate::core::limits::MAX_COMPRESSED_INPUT_BYTES as usize;
    assert_eq!(checked_jpeg_preparation_len(limit - 2, 0).unwrap(), limit);
    assert!(checked_jpeg_preparation_len(limit - 1, 0).is_err());
    assert!(checked_jpeg_preparation_len(usize::MAX, 1).is_err());
}

#[test]
fn jpeg_preparation_combines_dimension_and_eoi_repairs() {
    let image = image::RgbImage::new(8, 4);
    let mut jpeg = super::encode_test_jpeg(&image);
    let sof = jpeg
        .windows(2)
        .position(|bytes| bytes[0] == 0xff && is_sof_marker(bytes[1]))
        .expect("encoded fixture has SOF");
    jpeg[sof + 5..sof + 9].fill(0);
    jpeg.truncate(jpeg.len() - 2);
    let original = jpeg.clone();

    let prepared = prepare_jpeg_input(&jpeg, None, 8, 4, false).unwrap();
    assert_eq!(jpeg, original, "preparation must not mutate source bytes");
    assert_eq!(&prepared[sof + 5..sof + 7], &4_u16.to_be_bytes());
    assert_eq!(&prepared[sof + 7..sof + 9], &8_u16.to_be_bytes());
    assert!(prepared.ends_with(&[0xff, 0xd9]));
}

#[test]
fn forced_dimensions_patch_the_real_sof_not_sof_bytes_inside_app1() {
    let image = image::RgbImage::from_pixel(16, 8, image::Rgb([200, 40, 40]));
    let encoded = super::encode_test_jpeg(&image);
    // An APP1 payload (an EXIF thumbnail, say) that holds SOF0-like bytes.
    let fake_sof = [0xFF, 0xC0, 0x00, 0x11, 0x08, 0x00, 0x01, 0x00, 0x01, 0x03];
    let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE1];
    jpeg.extend_from_slice(&(fake_sof.len() as u16 + 2).to_be_bytes());
    jpeg.extend_from_slice(&fake_sof);
    let real_sof = jpeg.len()
        + encoded[2..]
            .windows(2)
            .position(|bytes| bytes[0] == 0xFF && is_sof_marker(bytes[1]))
            .expect("encoded fixture has SOF");
    jpeg.extend_from_slice(&encoded[2..]);
    // Only a patch that lands on the real SOF makes this decodable.
    jpeg[real_sof + 5..real_sof + 9].fill(0);

    let prepared = prepare_jpeg_input(&jpeg, None, 16, 8, true).unwrap();
    assert_eq!(&prepared[6..6 + fake_sof.len()], &fake_sof);
    assert_eq!(&prepared[real_sof + 5..real_sof + 9], &[0, 8, 0, 16]);

    let decoded =
        decode_jpeg_rgb_with_size_override(&jpeg, None, 16, 8, None, None, J2kColorTransform::Auto)
            .expect("forced dimensions decode");
    assert_eq!((decoded.width, decoded.height), (16, 8));
}

#[test]
fn jpeg_tile_geometry_parses_dri_after_sof() {
    let jpeg = vec![
        0xFF, 0xD8, // SOI
        0xFF, 0xC0, // SOF0
        0x00, 0x11, // len
        0x08, // precision
        0x00, 0x08, // height
        0x00, 0x20, // width
        0x03, // components
        0x01, 0x22, 0x00, // h=2, v=2
        0x02, 0x11, 0x00, 0x03, 0x11, 0x00, 0xFF, 0xDD, // DRI
        0x00, 0x04, // len
        0x00, 0x02, // restart interval
        0xFF, 0xDA, // SOS
        0x00, 0x0C, 0x03, 0x01, 0x00, 0x02, 0x11, 0x03, 0x11, 0x00, 0x3F, 0x00,
    ];

    let geometry = jpeg_tile_geometry(&jpeg).unwrap();
    assert_eq!(geometry.width, 32);
    assert_eq!(geometry.height, 8);
    assert_eq!(geometry.tile_width, 32);
    assert_eq!(geometry.tile_height, 16);
}

#[test]
fn jpeg_tile_geometry_rejects_missing_restart_markers() {
    let jpeg = vec![
        0xFF, 0xD8, // SOI
        0xFF, 0xC0, // SOF0
        0x00, 0x11, // len
        0x08, // precision
        0x00, 0x08, // height
        0x00, 0x10, // width
        0x03, // components
        0x01, 0x11, 0x00, 0x02, 0x11, 0x00, 0x03, 0x11, 0x00, 0xFF, 0xDA, // SOS
        0x00, 0x0C, 0x03, 0x01, 0x00, 0x02, 0x11, 0x03, 0x11, 0x00, 0x3F, 0x00,
    ];

    let err = jpeg_tile_geometry(&jpeg).unwrap_err();
    assert!(err.to_string().contains("restart markers"));
}
