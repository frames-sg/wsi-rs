use super::*;

#[test]
fn fixture_rgb_nomct_decodes_to_reference_rgb() {
    let codestream = include_bytes!("../../../../tests/fixtures/jp2k/rgb_nomct.j2k");
    let expected = include_bytes!("../../../../tests/fixtures/jp2k/rgb_nomct.ppm");
    assert_fixture_decodes_to_expected(codestream, expected, Jp2kColorSpace::Rgb);
}

#[test]
fn fixture_rgb_nomct_sample_buffer_matches_rgba_decode_exactly() {
    let codestream = include_bytes!("../../../../tests/fixtures/jp2k/rgb_nomct.j2k");
    let header = parse_codestream_header(codestream).unwrap();

    for (expected_width, expected_height) in [
        (header.image_width, header.image_height),
        (header.image_width, header.image_height - 1),
    ] {
        let rgba = decode_jp2k(
            codestream,
            expected_width,
            expected_height,
            Jp2kColorSpace::Rgb,
        )
        .unwrap();
        let sample = decode_jp2k_to_sample_buffer(
            codestream,
            expected_width,
            expected_height,
            Jp2kColorSpace::Rgb,
        )
        .unwrap();

        assert_eq!(sample.width, expected_width);
        assert_eq!(sample.height, expected_height);

        let sample_rgb = sample.data.as_u8().unwrap();
        let expected_rgb: Vec<u8> = rgba
            .pixels()
            .flat_map(|pixel| {
                assert_eq!(pixel.0[3], 255);
                [pixel.0[0], pixel.0[1], pixel.0[2]]
            })
            .collect();

        assert_eq!(sample_rgb, expected_rgb.as_slice());
    }
}

#[test]
fn fixture_rgb_mct_decodes_with_ycbcr_hint() {
    let codestream = include_bytes!("../../../../tests/fixtures/jp2k/rgb_mct.j2k");
    let expected = include_bytes!("../../../../tests/fixtures/jp2k/rgb_mct.ppm");
    assert_fixture_decodes_to_expected(codestream, expected, Jp2kColorSpace::YCbCr);
}

#[test]
fn fixture_ycbcr_444_decodes_to_reference_rgb() {
    let codestream = include_bytes!("../../../../tests/fixtures/jp2k/ycbcr_444.j2k");
    let expected = include_bytes!("../../../../tests/fixtures/jp2k/ycbcr_444.ppm");
    assert_fixture_decodes_to_expected(codestream, expected, Jp2kColorSpace::YCbCr);
}

#[test]
fn fixture_ycbcr_422_decodes_to_reference_rgb() {
    let codestream = include_bytes!("../../../../tests/fixtures/jp2k/ycbcr_422.j2k");
    let expected = include_bytes!("../../../../tests/fixtures/jp2k/ycbcr_422.ppm");
    assert_fixture_decodes_to_expected(codestream, expected, Jp2kColorSpace::YCbCr);
}

#[test]
fn fixture_ycbcr_420_decodes_to_reference_rgb() {
    let codestream = include_bytes!("../../../../tests/fixtures/jp2k/ycbcr_420.j2k");
    let expected = include_bytes!("../../../../tests/fixtures/jp2k/ycbcr_420.ppm");
    assert_fixture_decodes_to_expected(codestream, expected, Jp2kColorSpace::YCbCr);
}
