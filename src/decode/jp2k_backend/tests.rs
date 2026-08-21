use super::*;
use crate::decode::jp2k_codestream::{
    Jp2kCodingStyleInfo, Jp2kProgressionOrder, Jp2kQuantStep, Jp2kQuantizationInfo,
    Jp2kQuantizationStyle, Jp2kWaveletTransform,
};

fn test_header(multiple_component_transform: bool) -> Jp2kCodestreamInfo {
    Jp2kCodestreamInfo {
        image_origin_x: 0,
        image_origin_y: 0,
        image_width: 2,
        image_height: 1,
        tile_width: 2,
        tile_height: 1,
        tile_origin_x: 0,
        tile_origin_y: 0,
        tile_count_x: 1,
        tile_count_y: 1,
        components: vec![],
        coding_style: Jp2kCodingStyleInfo {
            progression_order: Jp2kProgressionOrder::Lrcp,
            layers: 1,
            multiple_component_transform,
            decomposition_levels: 0,
            code_block_width_exponent: 4,
            code_block_height_exponent: 4,
            code_block_style: 0,
            transform: Jp2kWaveletTransform::Irreversible9x7,
            custom_precincts: false,
            sop_markers: false,
            eph_markers: false,
        },
        quantization: Jp2kQuantizationInfo {
            style: Jp2kQuantizationStyle::ScalarExpounded,
            guard_bits: 2,
            steps: vec![Jp2kQuantStep {
                exponent: 8,
                mantissa: 0,
            }],
        },
        main_header_length: 0,
        tile_parts: vec![],
        seen_markers: vec![],
    }
}

#[test]
fn multiple_component_transform_forces_rgb_output() {
    let header = test_header(true);
    assert_eq!(
        effective_output_colorspace(&header, Jp2kColorSpace::YCbCr),
        Jp2kColorSpace::Rgb
    );
}

#[test]
fn raw_ycbcr_without_mct_preserves_requested_colorspace() {
    let header = test_header(false);
    assert_eq!(
        effective_output_colorspace(&header, Jp2kColorSpace::YCbCr),
        Jp2kColorSpace::YCbCr
    );
}
