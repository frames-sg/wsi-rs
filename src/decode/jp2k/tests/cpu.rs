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

#[test]
fn codec_supported_multitile_codestream_is_read_without_a_second_codec_policy() {
    let pixels: Vec<u8> = (0..32 * 16)
        .flat_map(|n| [(n % 251) as u8, (n % 127) as u8, 90])
        .collect();
    let mut options = j2k::J2kLosslessEncodeOptions::default();
    options.tile_size = Some((16, 16));
    options.tile_part_packet_limit = Some(1);
    let encoded = j2k::encode_j2k_lossless(
        j2k::J2kLosslessSamples {
            data: &pixels,
            width: 32,
            height: 16,
            components: 3,
            bit_depth: 8,
            signed: false,
        },
        &options,
    )
    .unwrap();
    let decoded = decode_jp2k_to_sample_buffer(&encoded.codestream, 32, 16, Jp2kColorSpace::Rgb)
        .expect("j2k-supported tiled input");
    assert_eq!(decoded.data.as_u8(), Some(pixels.as_slice()));
}
