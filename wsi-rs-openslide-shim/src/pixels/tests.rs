use super::*;
use wsi_rs::{ColorSpace, CpuTile};

#[test]
fn rgba_converts_to_premultiplied_argb_words() {
    let tile = CpuTile::from_u8_interleaved(
        2,
        1,
        4,
        ColorSpace::Rgba,
        vec![100, 50, 200, 128, 10, 20, 30, 0],
    )
    .expect("valid tile");

    let argb = tile_to_premultiplied_argb(tile).expect("convert");

    assert_eq!(argb, vec![0x8032_1964, 0x0000_0000]);
}

#[test]
fn rgb_converts_directly_into_the_caller_buffer() {
    let tile =
        CpuTile::from_u8_interleaved(2, 1, 3, ColorSpace::Rgb, vec![10, 20, 30, 200, 150, 100])
            .expect("valid tile");
    let mut argb = [0; 2];

    tile_to_premultiplied_argb_into(tile, &mut argb).expect("convert");

    assert_eq!(argb, [0xff0a_141e, 0xffc8_9664]);
}

#[test]
fn direct_conversion_rejects_a_mismatched_destination() {
    let tile = CpuTile::from_u8_interleaved(1, 1, 3, ColorSpace::Rgb, vec![10, 20, 30])
        .expect("valid tile");

    let error =
        tile_to_premultiplied_argb_into(tile, &mut []).expect_err("destination size must be exact");

    assert!(error.to_string().contains("destination has 0 pixels"));
}
