use super::*;

#[test]
fn decode_valid_jpeg() {
    let mut rgb = image::RgbImage::new(8, 8);
    for pixel in rgb.pixels_mut() {
        *pixel = image::Rgb([200, 100, 50]);
    }
    let jpeg_data = encode_test_jpeg(&rgb);
    let decoded = decode_jpeg(&jpeg_data, None, 8, 8).unwrap();
    assert_eq!(decoded.width(), 8);
    assert_eq!(decoded.height(), 8);
    // All alpha channels should be 255
    for pixel in decoded.pixels() {
        assert_eq!(pixel[3], 255);
    }
}

#[test]
fn decode_with_jpeg_tables() {
    // Create a valid JPEG
    let mut rgb = image::RgbImage::new(8, 8);
    for pixel in rgb.pixels_mut() {
        *pixel = image::Rgb([100, 150, 200]);
    }
    let jpeg_data = encode_test_jpeg(&rgb);

    // Find SOS marker (0xFF, 0xDA) to split into tables and scan data.
    // Tables = everything up to (but not including) SOS marker, plus EOI.
    // Data = SOI + SOS marker onward.
    let sos_pos = jpeg_data
        .windows(2)
        .position(|w| w == [0xFF, 0xDA])
        .expect("SOS marker not found");

    // tables: from start to just before SOS, with EOI appended
    let mut tables = jpeg_data[..sos_pos].to_vec();
    tables.extend_from_slice(&[0xFF, 0xD9]); // EOI

    // data: SOI + from SOS onward
    let mut data = vec![0xFF, 0xD8]; // SOI
    data.extend_from_slice(&jpeg_data[sos_pos..]);

    let decoded = decode_jpeg(&data, Some(&tables), 8, 8).unwrap();
    assert_eq!(decoded.width(), 8);
    assert_eq!(decoded.height(), 8);
    for pixel in decoded.pixels() {
        assert_eq!(pixel[3], 255);
    }
}

#[test]
fn decode_jpeg_rgb_returns_interleaved_rgb() {
    let mut rgb = image::RgbImage::new(4, 4);
    for (idx, pixel) in rgb.pixels_mut().enumerate() {
        *pixel = image::Rgb([idx as u8, 200, 50]);
    }
    let jpeg_data = encode_test_jpeg(&rgb);

    let decoded = decode_jpeg_rgb(&jpeg_data, None, 4, 4).unwrap();
    assert_eq!(decoded.width, 4);
    assert_eq!(decoded.height, 4);
    assert_eq!(decoded.pixels.len(), 4 * 4 * 3);
}

#[test]
fn decode_progressive_jpeg_rgb_returns_interleaved_rgb() {
    let jpeg_data = progressive_8x8_jpeg();

    let decoded = decode_jpeg_rgb(&jpeg_data, None, 8, 8).unwrap();

    assert_eq!(decoded.width, 8);
    assert_eq!(decoded.height, 8);
    assert_eq!(decoded.pixels.len(), 8 * 8 * 3);
}

#[test]
fn progressive_scaled_decode_falls_back_to_full_decode_resize() {
    let jpeg_data = progressive_8x8_jpeg();

    let decoded = decode_jpeg_rgb_with_size_override(
        &jpeg_data,
        None,
        8,
        8,
        Some(4),
        Some(4),
        J2kColorTransform::Auto,
    )
    .unwrap();

    assert_eq!(decoded.width, 4);
    assert_eq!(decoded.height, 4);
    assert_eq!(decoded.pixels.len(), 4 * 4 * 3);
}
#[test]
fn decode_jpeg_rgb_scaled_returns_scaled_dimensions() {
    let mut rgb = image::RgbImage::new(16, 16);
    for (idx, pixel) in rgb.pixels_mut().enumerate() {
        *pixel = image::Rgb([idx as u8, 100, 200]);
    }
    let jpeg_data = encode_test_jpeg(&rgb);

    let decoded = try_decode_jpeg_rgb_scaled(ScaledJpegDecode {
        data: &jpeg_data,
        tables: None,
        expected_width: 16,
        expected_height: 16,
        requested_width: 4,
        requested_height: 4,
        force_dimensions: false,
        color_transform: J2kColorTransform::Auto,
    })
    .unwrap()
    .expect("power-of-two downscale should use j2k IDCT scale");

    assert_eq!(decoded.width, 4);
    assert_eq!(decoded.height, 4);
    assert_eq!(decoded.pixels.len(), 4 * 4 * 3);
}

#[test]
fn non_power_of_two_requested_sizes_use_full_decode_resize_in_both_entry_points() {
    let mut rgb = image::RgbImage::new(8, 8);
    for (index, pixel) in rgb.pixels_mut().enumerate() {
        *pixel = image::Rgb([index as u8, (index * 3) as u8, (index * 7) as u8]);
    }
    let jpeg_data = encode_test_jpeg(&rgb);
    let full = decode_jpeg_rgb(&jpeg_data, None, 8, 8).unwrap();
    let mut expected = Vec::with_capacity(3 * 5 * 3);
    for y in 0..5 {
        let src_y = y * full.height as usize / 5;
        for x in 0..3 {
            let src_x = x * full.width as usize / 3;
            let offset = (src_y * full.width as usize + src_x) * 3;
            expected.extend_from_slice(&full.pixels[offset..offset + 3]);
        }
    }

    let override_decode = decode_jpeg_rgb_with_size_override(
        &jpeg_data,
        None,
        8,
        8,
        Some(3),
        Some(5),
        J2kColorTransform::Auto,
    )
    .unwrap();
    assert_eq!((override_decode.width, override_decode.height), (3, 5));
    assert_eq!(override_decode.pixels, expected);

    let job_decode = decode_one_jpeg_job(&JpegDecodeJob {
        data: Cow::Borrowed(&jpeg_data),
        tables: None,
        expected_width: 8,
        expected_height: 8,
        color_transform: J2kColorTransform::Auto,
        force_dimensions: false,
        requested_size: Some((3, 5)),
    })
    .unwrap();
    assert_eq!((job_decode.width, job_decode.height), (3, 5));
    assert_eq!(job_decode.data.as_u8().unwrap(), expected);
}
