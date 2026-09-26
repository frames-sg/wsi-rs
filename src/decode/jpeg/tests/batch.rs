use super::*;

fn with_adobe_rgb_marker(jpeg: &[u8]) -> Vec<u8> {
    assert!(jpeg.starts_with(&[0xFF, 0xD8]));
    let mut marked = Vec::with_capacity(jpeg.len() + 16);
    marked.extend_from_slice(&jpeg[..2]);
    marked.extend_from_slice(&[
        0xFF, 0xEE, 0x00, 0x0E, b'A', b'd', b'o', b'b', b'e', 0x00, 0x64, 0x00, 0x00, 0x00, 0x00,
        0x00,
    ]);
    marked.extend_from_slice(&jpeg[2..]);
    marked
}

fn assert_fast_batch_matches_individual(
    jpeg_data: &[u8],
    matching_transform: J2kColorTransform,
    requested_size: Option<(u32, u32)>,
) {
    let jobs = [
        J2kColorTransform::Auto,
        matching_transform,
        J2kColorTransform::Auto,
        matching_transform,
    ]
    .into_iter()
    .map(|color_transform| JpegDecodeJob {
        data: Cow::Borrowed(jpeg_data),
        tables: None,
        expected_width: 16,
        expected_height: 16,
        color_transform,
        force_dimensions: false,
        requested_size,
    })
    .collect::<Vec<_>>();

    let fast = try_decode_batch_jpeg_with_j2k(&jobs)
        .expect("matching color overrides should use the j2k batch fast path");
    let sequential = jobs.iter().map(decode_one_jpeg_job).collect::<Vec<_>>();

    assert_eq!(fast.len(), sequential.len());
    for (fast, sequential) in fast.into_iter().zip(sequential) {
        let fast = fast.unwrap();
        let sequential = sequential.unwrap();
        assert_eq!(
            (fast.width, fast.height),
            (sequential.width, sequential.height)
        );
        assert_eq!(fast.data.as_u8(), sequential.data.as_u8());
    }
}

#[test]
fn j2k_batch_fast_path_reuses_default_color_plan_for_matching_overrides() {
    let mut rgb = image::RgbImage::new(16, 16);
    for (idx, pixel) in rgb.pixels_mut().enumerate() {
        *pixel = image::Rgb([idx as u8, 100, 200]);
    }
    let ycbcr_jpeg = encode_test_jpeg(&rgb);
    let rgb_jpeg = with_adobe_rgb_marker(&ycbcr_jpeg);

    for requested_size in [None, Some((4, 4))] {
        assert_fast_batch_matches_individual(
            &ycbcr_jpeg,
            J2kColorTransform::ForceYCbCr,
            requested_size,
        );
        assert_fast_batch_matches_individual(
            &rgb_jpeg,
            J2kColorTransform::ForceRgb,
            requested_size,
        );
    }
}

#[test]
fn j2k_batch_fast_path_rejects_mixed_effective_color_overrides() {
    let mut rgb = image::RgbImage::new(16, 16);
    for (idx, pixel) in rgb.pixels_mut().enumerate() {
        *pixel = image::Rgb([idx as u8, 100, 200]);
    }
    let jpeg_data = encode_test_jpeg(&rgb);
    let jobs = [J2kColorTransform::ForceRgb, J2kColorTransform::ForceYCbCr]
        .into_iter()
        .map(|color_transform| JpegDecodeJob {
            data: Cow::Borrowed(jpeg_data.as_slice()),
            tables: None,
            expected_width: 16,
            expected_height: 16,
            color_transform,
            force_dimensions: false,
            requested_size: None,
        })
        .collect::<Vec<_>>();

    assert!(try_decode_batch_jpeg_with_j2k(&jobs).is_none());
    let batch = decode_batch_jpeg(&jobs)
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let sequential = jobs
        .iter()
        .map(decode_one_jpeg_job)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_ne!(batch[0].data.as_u8(), batch[1].data.as_u8());
    for (batch, sequential) in batch.iter().zip(&sequential) {
        assert_eq!(batch.data.as_u8(), sequential.data.as_u8());
    }
}

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
