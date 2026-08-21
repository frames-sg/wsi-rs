use super::*;

#[test]
fn j2k_batch_fast_path_matches_single_tile_for_forced_color_transform() {
    let mut rgb = image::RgbImage::new(16, 16);
    for (idx, pixel) in rgb.pixels_mut().enumerate() {
        *pixel = image::Rgb([idx as u8, 100, 200]);
    }
    let jpeg_data = encode_test_jpeg(&rgb);
    let jobs = (0..4)
        .map(|_| JpegDecodeJob {
            data: Cow::Borrowed(jpeg_data.as_slice()),
            tables: None,
            expected_width: 16,
            expected_height: 16,
            color_transform: J2kColorTransform::ForceRgb,
            force_dimensions: false,
            requested_size: None,
        })
        .collect::<Vec<_>>();

    let fast = try_decode_batch_jpeg_with_j2k(&jobs)
        .expect("forced color transform should use j2k batch fast path");
    let sequential = jobs.iter().map(decode_one_jpeg_job).collect::<Vec<_>>();

    assert_eq!(fast.len(), sequential.len());
    for (fast, sequential) in fast.into_iter().zip(sequential) {
        let fast = fast.unwrap();
        let sequential = sequential.unwrap();
        assert_eq!(fast.width, sequential.width);
        assert_eq!(fast.height, sequential.height);
        assert_eq!(fast.data.as_u8(), sequential.data.as_u8());
    }
}

#[test]
fn j2k_batch_fast_path_matches_single_tile_for_scaled_decode() {
    let mut rgb = image::RgbImage::new(16, 16);
    for (idx, pixel) in rgb.pixels_mut().enumerate() {
        *pixel = image::Rgb([idx as u8, 100, 200]);
    }
    let jpeg_data = encode_test_jpeg(&rgb);
    let jobs = (0..4)
        .map(|_| JpegDecodeJob {
            data: Cow::Borrowed(jpeg_data.as_slice()),
            tables: None,
            expected_width: 16,
            expected_height: 16,
            color_transform: J2kColorTransform::ForceRgb,
            force_dimensions: false,
            requested_size: Some((4, 4)),
        })
        .collect::<Vec<_>>();

    let fast = try_decode_batch_jpeg_with_j2k(&jobs)
        .expect("scaled decode should use j2k batch fast path");
    let sequential = jobs.iter().map(decode_one_jpeg_job).collect::<Vec<_>>();

    assert_eq!(fast.len(), sequential.len());
    for (fast, sequential) in fast.into_iter().zip(sequential) {
        let fast = fast.unwrap();
        let sequential = sequential.unwrap();
        assert_eq!(fast.width, 4);
        assert_eq!(fast.height, 4);
        assert_eq!(fast.data.as_u8(), sequential.data.as_u8());
    }
}
