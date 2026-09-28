use super::*;
use crate::core::types::CpuTileData;

fn rgb_rows(rows: &[[u8; 3]], width: u32) -> CpuTile {
    let mut data = Vec::new();
    for row in rows {
        for _ in 0..width {
            data.extend_from_slice(row);
        }
    }
    CpuTile::from_u8_interleaved(width, rows.len() as u32, 3, ColorSpace::Rgb, data).unwrap()
}

fn bytes(tile: &CpuTile) -> &[u8] {
    match &tile.data {
        CpuTileData::U8(data) => data.as_slice(),
        _ => panic!("expected u8 tile"),
    }
}

#[test]
fn whole_pixel_window_is_an_exact_opaque_crop() {
    let source = rgb_rows(&[[10, 20, 30], [40, 50, 60], [70, 80, 90]], 2);
    let surface = cairo_subtile_surface_u8(&source, (0.0, 1.0), (2, 2)).unwrap();
    assert_eq!(
        (surface.channels, surface.color_space.clone()),
        (3, ColorSpace::Rgb)
    );
    assert_eq!(
        bytes(&surface),
        &[40, 50, 60, 40, 50, 60, 70, 80, 90, 70, 80, 90]
    );
}

#[test]
fn half_pixel_window_blends_adjacent_rows() {
    let source = rgb_rows(&[[0, 0, 0], [100, 200, 50], [100, 200, 50]], 1);
    let surface = cairo_subtile_surface_u8(&source, (0.0, 0.5), (1, 2)).unwrap();
    assert_eq!(surface.channels, 3);
    let data = bytes(&surface);
    assert_eq!(&data[3..6], &[100, 200, 50]);
    for (blended, full) in data[..3].iter().zip([100u8, 200, 50]) {
        assert!((i16::from(*blended) - i16::from(full / 2)).abs() <= 1);
    }
}

#[test]
fn window_past_the_source_edge_keeps_partial_coverage() {
    let source = rgb_rows(&[[100, 200, 50], [100, 200, 50]], 1);
    // Cairo's ceil(2.5) = 3 row surface for a 2.5-row subtile starting at 0.5.
    let surface = cairo_subtile_surface_u8(&source, (0.0, 0.5), (1, 3)).unwrap();
    assert_eq!(
        (surface.channels, surface.color_space.clone()),
        (4, ColorSpace::Rgba)
    );
    let data = bytes(&surface);
    assert_eq!(&data[..4], &[100, 200, 50, 255]);
    let alpha = data[7];
    assert!(
        (126..=129).contains(&alpha),
        "half-covered row alpha {alpha}"
    );
    for (straight, full) in data[4..7].iter().zip([100u8, 200, 50]) {
        assert!((i16::from(*straight) - i16::from(full)).abs() <= 1);
    }
    assert_eq!(data[11], 0, "row wholly past the source is transparent");
}
