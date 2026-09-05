use super::*;
fn test_header(multiple_component_transform: bool) -> Jp2kCodestreamInfo {
    Jp2kCodestreamInfo {
        image_width: 2,
        image_height: 1,
        components: vec![],
        multiple_component_transform,
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
