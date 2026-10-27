use super::*;

#[test]
fn decode_empty_data_fails() {
    let result = decode_jpeg_rgb(&[], None, 0, 0);
    assert!(result.is_err());
}

#[test]
fn jpeg_jobs_preserve_codec_errors_for_every_resize_path() {
    for force_dimensions in [false, true] {
        for requested_size in [None, Some((4, 4)), Some((3, 5))] {
            let job = JpegDecodeJob {
                data: Cow::Borrowed(&[]),
                tables: None,
                expected_width: 8,
                expected_height: 8,
                color_transform: J2kColorTransform::Auto,
                force_dimensions,
                requested_size,
            };
            let error = decode_one_jpeg_job(&job).unwrap_err();
            assert!(
                matches!(&error, WsiError::Codec { codec: "jpeg", source }
                    if source.downcast_ref::<WsiError>().is_some_and(|error|
                        matches!(error, WsiError::Jpeg(_)))),
                "{force_dimensions:?}, {requested_size:?}: {error:?}"
            );
        }
    }
}
