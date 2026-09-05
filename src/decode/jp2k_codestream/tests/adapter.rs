use super::*;

#[test]
fn codec_metadata_agrees_with_the_independent_fixture_oracle() {
    for bytes in [
        include_bytes!("../../../../tests/fixtures/jp2k/rgb_nomct.j2k").as_slice(),
        include_bytes!("../../../../tests/fixtures/jp2k/rgb_mct.j2k").as_slice(),
        include_bytes!("../../../../tests/fixtures/jp2k/rgb_rct.j2k").as_slice(),
        include_bytes!("../../../../tests/fixtures/jp2k/ycbcr_420.j2k").as_slice(),
        include_bytes!("../../../../tests/fixtures/jp2k/ycbcr_422.j2k").as_slice(),
    ] {
        let expected = reference::parse_codestream_header(bytes).unwrap();
        let actual = parse_codestream_header(bytes).unwrap();
        assert_eq!(
            (actual.image_width, actual.image_height),
            (expected.image_width, expected.image_height)
        );
        assert_eq!(
            actual.multiple_component_transform,
            expected.coding_style.multiple_component_transform
        );
        assert_eq!(actual.components.len(), expected.components.len());
        for (a, e) in actual.components.iter().zip(&expected.components) {
            assert_eq!(
                (a.bit_depth, a.signed, a.x_rsiz, a.y_rsiz),
                (
                    e.precision_bits,
                    e.is_signed,
                    e.horizontal_sample_separation,
                    e.vertical_sample_separation
                )
            );
        }
        validate_pixel_contract(&actual).unwrap();
    }
}

#[test]
fn wsi_pixel_contract_rejects_unsupported_samples_and_output_sizes() {
    let bytes = include_bytes!("../../../../tests/fixtures/jp2k/rgb_nomct.j2k");
    let mut info = parse_codestream_header(bytes).unwrap();
    info.image_width = u32::MAX;
    assert!(matches!(
        validate_pixel_contract(&info),
        Err(WsiError::ResourceLimit { .. })
    ));
    for (bits, signed) in [(12, false), (8, true)] {
        let mut info = parse_codestream_header(bytes).unwrap();
        info.components[0].bit_depth = bits;
        info.components[0].signed = signed;
        assert!(validate_pixel_contract(&info).is_err());
    }
    let mut info = parse_codestream_header(bytes).unwrap();
    info.components.pop();
    assert!(validate_pixel_contract(&info).is_err());
}
