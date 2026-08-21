use super::super::*;

#[test]
fn to_rgba_from_rgb_u8() {
    let buf = CpuTile {
        width: 2,
        height: 1,
        channels: 3,
        color_space: ColorSpace::Rgb,
        layout: CpuTileLayout::Interleaved,
        data: CpuTileData::u8(vec![255, 0, 0, 0, 255, 0]),
    };
    let img = buf.to_rgba().unwrap();
    assert_eq!(img.get_pixel(0, 0).0, [255, 0, 0, 255]);
    assert_eq!(img.get_pixel(1, 0).0, [0, 255, 0, 255]);
}

#[test]
fn to_rgba_from_rgb_u8_planar() {
    let buf = CpuTile {
        width: 2,
        height: 1,
        channels: 3,
        color_space: ColorSpace::Rgb,
        layout: CpuTileLayout::Planar,
        data: CpuTileData::u8(vec![255, 0, 0, 255, 0, 0]),
    };
    let img = buf.to_rgba().unwrap();
    assert_eq!(img.get_pixel(0, 0).0, [255, 0, 0, 255]);
    assert_eq!(img.get_pixel(1, 0).0, [0, 255, 0, 255]);
}

#[test]
fn to_rgba_from_grayscale_u8() {
    let buf = CpuTile {
        width: 1,
        height: 1,
        channels: 1,
        color_space: ColorSpace::Grayscale,
        layout: CpuTileLayout::Interleaved,
        data: CpuTileData::u8(vec![128]),
    };
    let img = buf.to_rgba().unwrap();
    assert_eq!(img.get_pixel(0, 0).0, [128, 128, 128, 255]);
}

#[test]
fn to_rgba_from_palette() {
    let lut = vec![[255, 0, 0], [0, 255, 0]];
    let buf = CpuTile {
        width: 2,
        height: 1,
        channels: 1,
        color_space: ColorSpace::Palette(Arc::new(lut)),
        layout: CpuTileLayout::Interleaved,
        data: CpuTileData::u8(vec![0, 1]),
    };
    let img = buf.to_rgba().unwrap();
    assert_eq!(img.get_pixel(0, 0).0, [255, 0, 0, 255]);
    assert_eq!(img.get_pixel(1, 0).0, [0, 255, 0, 255]);
}

#[test]
fn to_rgba_rejects_non_u8() {
    let buf = CpuTile {
        width: 1,
        height: 1,
        channels: 1,
        color_space: ColorSpace::Grayscale,
        layout: CpuTileLayout::Interleaved,
        data: CpuTileData::u16(vec![1000]),
    };
    assert!(buf.to_rgba().is_err());
}

#[test]
fn to_rgba_windowed_u16() {
    let buf = CpuTile {
        width: 2,
        height: 1,
        channels: 1,
        color_space: ColorSpace::Grayscale,
        layout: CpuTileLayout::Interleaved,
        data: CpuTileData::u16(vec![0, 1000]),
    };
    let window = DisplayWindow::new(0.0, 1000.0).unwrap();
    let img = buf.to_rgba_windowed(&window).unwrap();
    assert_eq!(img.get_pixel(0, 0).0[0], 0); // 0 maps to 0
    assert_eq!(img.get_pixel(1, 0).0[0], 255); // 1000 maps to 255
}

#[test]
fn to_rgba_windowed_u16_planar_rgb() {
    let buf = CpuTile {
        width: 2,
        height: 1,
        channels: 3,
        color_space: ColorSpace::Rgb,
        layout: CpuTileLayout::Planar,
        data: CpuTileData::u16(vec![0, 1000, 0, 1000, 0, 0]),
    };
    let window = DisplayWindow::new(0.0, 1000.0).unwrap();
    let img = buf.to_rgba_windowed(&window).unwrap();
    assert_eq!(img.get_pixel(0, 0).0, [0, 0, 0, 255]);
    assert_eq!(img.get_pixel(1, 0).0, [255, 255, 0, 255]);
}

#[test]
fn display_window_new_accepts_positive_finite_range() {
    let window = DisplayWindow::new(0.0, 1000.0).unwrap();
    assert_eq!(window.min(), 0.0);
    assert_eq!(window.max(), 1000.0);
}

#[test]
fn display_window_new_rejects_invalid_bounds() {
    for (min, max) in [
        (50.0, 50.0),
        (100.0, 50.0),
        (f64::NAN, 100.0),
        (0.0, f64::INFINITY),
    ] {
        let err = DisplayWindow::new(min, max).unwrap_err();
        assert!(matches!(err, WsiError::DisplayConversion(_)));
    }
}

#[test]
fn display_window_new_rejects_zero_range_before_conversion() {
    let err = DisplayWindow::new(50.0, 50.0).unwrap_err();
    assert!(matches!(err, WsiError::DisplayConversion(_)));
}

#[test]
fn to_rgb_from_rgb_u8() {
    let buf = CpuTile {
        width: 1,
        height: 1,
        channels: 3,
        color_space: ColorSpace::Rgb,
        layout: CpuTileLayout::Interleaved,
        data: CpuTileData::u8(vec![100, 150, 200]),
    };
    let img = buf.to_rgb().unwrap();
    assert_eq!(img.get_pixel(0, 0).0, [100, 150, 200]);
}

#[test]
fn into_rgb_reuses_interleaved_rgb_storage() {
    let raw = vec![100, 150, 200];
    let ptr = raw.as_ptr();
    let buf = CpuTile {
        width: 1,
        height: 1,
        channels: 3,
        color_space: ColorSpace::Rgb,
        layout: CpuTileLayout::Interleaved,
        data: CpuTileData::u8(raw),
    };
    let img = buf.into_rgb().unwrap();
    assert_eq!(img.as_raw().as_ptr(), ptr);
    assert_eq!(img.get_pixel(0, 0).0, [100, 150, 200]);
}

#[test]
fn into_rgba_reuses_interleaved_rgba_storage() {
    let raw = vec![100, 150, 200, 255];
    let ptr = raw.as_ptr();
    let buf = CpuTile {
        width: 1,
        height: 1,
        channels: 4,
        color_space: ColorSpace::Rgba,
        layout: CpuTileLayout::Interleaved,
        data: CpuTileData::u8(raw),
    };
    let img = buf.into_rgba().unwrap();
    assert_eq!(img.as_raw().as_ptr(), ptr);
    assert_eq!(img.get_pixel(0, 0).0, [100, 150, 200, 255]);
}

#[test]
fn to_rgba_reads_interleaved_and_planar_rgba_layouts_identically() {
    let interleaved = CpuTile {
        width: 1,
        height: 1,
        channels: 4,
        color_space: ColorSpace::Rgba,
        layout: CpuTileLayout::Interleaved,
        data: CpuTileData::u8(vec![10, 20, 30, 40]),
    };
    let planar = CpuTile {
        width: 1,
        height: 1,
        channels: 4,
        color_space: ColorSpace::Rgba,
        layout: CpuTileLayout::Planar,
        data: CpuTileData::u8(vec![10, 20, 30, 40]),
    };

    assert_eq!(interleaved.to_rgba().unwrap().as_raw(), &[10, 20, 30, 40]);
    assert_eq!(planar.to_rgba().unwrap().as_raw(), &[10, 20, 30, 40]);
}

// --- CpuTile::new() ---

#[test]
fn to_rgb_direct_path_rgb8() {
    let buf = CpuTile {
        width: 2,
        height: 1,
        channels: 3,
        color_space: ColorSpace::Rgb,
        layout: CpuTileLayout::Interleaved,
        data: CpuTileData::u8(vec![255, 0, 0, 0, 255, 0]),
    };
    let img = buf.to_rgb().unwrap();
    assert_eq!(img.get_pixel(0, 0).0, [255, 0, 0]);
    assert_eq!(img.get_pixel(1, 0).0, [0, 255, 0]);
}

#[test]
fn to_rgb_direct_path_grayscale() {
    let buf = CpuTile {
        width: 1,
        height: 1,
        channels: 1,
        color_space: ColorSpace::Grayscale,
        layout: CpuTileLayout::Interleaved,
        data: CpuTileData::u8(vec![128]),
    };
    let img = buf.to_rgb().unwrap();
    assert_eq!(img.get_pixel(0, 0).0, [128, 128, 128]);
}

#[test]
fn to_rgb_rejects_non_u8() {
    let buf = CpuTile {
        width: 1,
        height: 1,
        channels: 1,
        color_space: ColorSpace::Grayscale,
        layout: CpuTileLayout::Interleaved,
        data: CpuTileData::u16(vec![1000]),
    };
    assert!(buf.to_rgb().is_err());
}

#[test]
fn to_rgb_windowed_u16_direct() {
    let buf = CpuTile {
        width: 2,
        height: 1,
        channels: 1,
        color_space: ColorSpace::Grayscale,
        layout: CpuTileLayout::Interleaved,
        data: CpuTileData::u16(vec![0, 1000]),
    };
    let window = DisplayWindow::new(0.0, 1000.0).unwrap();
    let img = buf.to_rgb_windowed(&window).unwrap();
    assert_eq!(img.get_pixel(0, 0).0, [0, 0, 0]);
    assert_eq!(img.get_pixel(1, 0).0, [255, 255, 255]);
}

#[test]
fn to_rgb_windowed_f32_3ch_direct() {
    let buf = CpuTile {
        width: 1,
        height: 1,
        channels: 3,
        color_space: ColorSpace::Rgb,
        layout: CpuTileLayout::Interleaved,
        data: CpuTileData::f32(vec![0.0, 0.5, 1.0]),
    };
    let window = DisplayWindow::new(0.0, 1.0).unwrap();
    let img = buf.to_rgb_windowed(&window).unwrap();
    assert_eq!(img.get_pixel(0, 0).0, [0, 128, 255]);
}

// --- Arc Palette ---

#[test]
fn palette_clone_is_cheap() {
    let lut = Arc::new(vec![[255, 0, 0]; 256]);
    let cs = ColorSpace::Palette(lut.clone());
    let cs2 = cs.clone();
    drop(cs2);
    assert_eq!(Arc::strong_count(&lut), 2); // original + cs
}
