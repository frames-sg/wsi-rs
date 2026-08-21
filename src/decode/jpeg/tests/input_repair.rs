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
