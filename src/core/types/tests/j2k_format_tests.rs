use super::PixelFormat;

#[test]
fn every_wsi_pixel_format_round_trips_through_j2k_core() {
    for format in [
        PixelFormat::Rgb8,
        PixelFormat::Rgba8,
        PixelFormat::Gray8,
        PixelFormat::Rgb16,
        PixelFormat::Rgba16,
        PixelFormat::Gray16,
    ] {
        let j2k = j2k_core::PixelFormat::from(format);
        assert_eq!(
            PixelFormat::try_from(j2k).expect("supported J2K format"),
            format
        );
    }
}
