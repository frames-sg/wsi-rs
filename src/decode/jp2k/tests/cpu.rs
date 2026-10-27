use super::*;

#[test]
fn fixtures_preserve_exact_pixels_and_independent_reference_compatibility() {
    for (name, codestream, ppm, baseline, colorspace) in [
        (
            "rgb_nomct",
            include_bytes!("../../../../tests/fixtures/jp2k/rgb_nomct.j2k").as_slice(),
            include_bytes!("../../../../tests/fixtures/jp2k/rgb_nomct.ppm").as_slice(),
            None,
            Jp2kColorSpace::Rgb,
        ),
        (
            "rgb_mct",
            include_bytes!("../../../../tests/fixtures/jp2k/rgb_mct.j2k").as_slice(),
            include_bytes!("../../../../tests/fixtures/jp2k/rgb_mct.ppm").as_slice(),
            None,
            Jp2kColorSpace::YCbCr,
        ),
        (
            "rgb_rct",
            include_bytes!("../../../../tests/fixtures/jp2k/rgb_rct.j2k").as_slice(),
            include_bytes!("../../../../tests/fixtures/jp2k/rgb_rct.ppm").as_slice(),
            None,
            Jp2kColorSpace::Rgb,
        ),
        (
            "ycbcr_444",
            include_bytes!("../../../../tests/fixtures/jp2k/ycbcr_444.j2k").as_slice(),
            include_bytes!("../../../../tests/fixtures/jp2k/ycbcr_444.ppm").as_slice(),
            Some(include_bytes!("../../../../tests/fixtures/jp2k/ycbcr_444.wsi-rgb").as_slice()),
            Jp2kColorSpace::YCbCr,
        ),
        (
            "ycbcr_422",
            include_bytes!("../../../../tests/fixtures/jp2k/ycbcr_422.j2k").as_slice(),
            include_bytes!("../../../../tests/fixtures/jp2k/ycbcr_422.ppm").as_slice(),
            Some(include_bytes!("../../../../tests/fixtures/jp2k/ycbcr_422.wsi-rgb").as_slice()),
            Jp2kColorSpace::YCbCr,
        ),
        (
            "ycbcr_420",
            include_bytes!("../../../../tests/fixtures/jp2k/ycbcr_420.j2k").as_slice(),
            include_bytes!("../../../../tests/fixtures/jp2k/ycbcr_420.ppm").as_slice(),
            Some(include_bytes!("../../../../tests/fixtures/jp2k/ycbcr_420.wsi-rgb").as_slice()),
            Jp2kColorSpace::YCbCr,
        ),
    ] {
        let reference = load_fixture_rgb(ppm);
        let decoded = decode_jp2k_to_sample_buffer(
            codestream,
            reference.width(),
            reference.height(),
            colorspace,
        )
        .unwrap();
        assert_eq!(
            (decoded.width(), decoded.height()),
            reference.dimensions(),
            "{name}"
        );
        assert_eq!(
            decoded.as_u8().unwrap(),
            baseline.unwrap_or(reference.as_raw()),
            "{name}: decoded bytes changed"
        );
        // Retain the independent decoder check for YCbCr rounding differences.
        assert_sample_buffer_matches_rgb_fixture(&decoded, &reference);
    }
}

#[test]
fn cropped_sample_buffer_preserves_reference_pixels_exactly() {
    let codestream = include_bytes!("../../../../tests/fixtures/jp2k/rgb_nomct.j2k");
    let reference = load_fixture_rgb(include_bytes!(
        "../../../../tests/fixtures/jp2k/rgb_nomct.ppm"
    ));
    let height = reference.height() - 1;
    let decoded =
        decode_jp2k_to_sample_buffer(codestream, reference.width(), height, Jp2kColorSpace::Rgb)
            .unwrap();
    assert_eq!(
        (decoded.width(), decoded.height()),
        (reference.width(), height)
    );
    let expected_len = reference.width() as usize * height as usize * 3;
    assert_eq!(
        decoded.as_u8().unwrap(),
        &reference.as_raw()[..expected_len]
    );
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
