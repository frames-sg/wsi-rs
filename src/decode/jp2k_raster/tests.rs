use super::*;
use crate::decode::jp2k_backend::DecodedInterleavedImage;

#[test]
fn crop_sample_buffer_trims_to_requested_bounds() {
    let buffer = CpuTile {
        width: 4,
        height: 3,
        channels: 3,
        color_space: ColorSpace::Rgb,
        layout: CpuTileLayout::Interleaved,
        data: CpuTileData::u8((0..36).collect()),
    };

    let cropped = crop_sample_buffer(buffer, 2, 2).unwrap();
    assert_eq!(cropped.width, 2);
    assert_eq!(cropped.height, 2);
    assert_eq!(
        cropped.data.as_u8().unwrap(),
        &[0, 1, 2, 3, 4, 5, 12, 13, 14, 15, 16, 17]
    );
}

#[test]
fn interleaved_rgb_image_wraps_without_repacking() {
    let image = DecodedInterleavedImage {
        width: 2,
        height: 1,
        colorspace: Jp2kColorSpace::Rgb,
        pixels: vec![10, 20, 30, 40, 50, 60],
    };

    let buffer = interleaved_image_to_sample_buffer(image).unwrap();
    assert_eq!(buffer.data.as_u8().unwrap(), &[10, 20, 30, 40, 50, 60]);
}

#[test]
fn interleaved_ycbcr_image_converts_to_rgb() {
    let image = DecodedInterleavedImage {
        width: 1,
        height: 1,
        colorspace: Jp2kColorSpace::YCbCr,
        pixels: vec![100, 128, 128],
    };

    let buffer = interleaved_image_to_sample_buffer(image).unwrap();
    assert_eq!(buffer.data.as_u8().unwrap(), &[100, 100, 100]);
}

#[test]
fn interleaved_ycbcr_image_matches_openslide_4_0_1_rounding() {
    let image = DecodedInterleavedImage {
        width: 7,
        height: 1,
        colorspace: Jp2kColorSpace::YCbCr,
        pixels: vec![
            100, 128, 130, // positive R chroma rounds up
            100, 128, 126, // negative R chroma rounds down
            100, 130, 128, // positive B and negative G chroma
            100, 126, 128, // negative B and positive G chroma
            100, 0, 1, // combined fixed-point G rounding
            250, 255, 255, // upper clamp
            5, 0, 0, // lower clamp
        ],
    };

    let buffer = interleaved_image_to_sample_buffer(image).unwrap();
    assert_eq!(
        buffer.data.as_u8().unwrap(),
        &[
            103, 99, 100, 97, 101, 100, 100, 99, 104, 100, 101, 96, 0, 235, 0, 255, 116, 255, 0,
            140, 0,
        ]
    );
}

#[test]
fn interleaved_image_rejects_dimensions_that_overflow_the_rgb_buffer_size() {
    let err = interleaved_image_to_sample_buffer(DecodedInterleavedImage {
        width: usize::MAX,
        height: 2,
        colorspace: Jp2kColorSpace::Rgb,
        pixels: Vec::new(),
    })
    .unwrap_err();

    assert!(err.to_string().contains("image size overflow"));
}
