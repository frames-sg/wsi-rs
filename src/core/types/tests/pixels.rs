use super::super::*;

#[test]
fn pixel_format_reports_layout_metadata_for_all_variants() {
    let cases = [
        (PixelFormat::Rgb8, ColorSpace::Rgb, SampleType::Uint8, 3),
        (PixelFormat::Rgba8, ColorSpace::Rgba, SampleType::Uint8, 4),
        (
            PixelFormat::Gray8,
            ColorSpace::Grayscale,
            SampleType::Uint8,
            1,
        ),
        (PixelFormat::Rgb16, ColorSpace::Rgb, SampleType::Uint16, 3),
        (PixelFormat::Rgba16, ColorSpace::Rgba, SampleType::Uint16, 4),
        (
            PixelFormat::Gray16,
            ColorSpace::Grayscale,
            SampleType::Uint16,
            1,
        ),
    ];

    for (format, color_space, sample_type, channels) in cases {
        assert_eq!(format.color_space(), color_space);
        assert_eq!(format.sample_type(), sample_type);
        assert_eq!(format.channels(), channels);
        assert_eq!(format.bytes_per_sample(), sample_type.byte_size());
    }
}

#[test]
fn sample_buffer_new_valid() {
    let buf = CpuTile::new(
        2,
        1,
        3,
        ColorSpace::Rgb,
        CpuTileLayout::Interleaved,
        CpuTileData::u8(vec![0; 6]),
    );
    assert!(buf.is_ok());
    assert_eq!(buf.unwrap().width, 2);
}

#[test]
fn cpu_tile_accessors_expose_validated_metadata() {
    let tile = CpuTile::new(
        2,
        1,
        3,
        ColorSpace::Rgb,
        CpuTileLayout::Interleaved,
        CpuTileData::u8(vec![10, 20, 30, 40, 50, 60]),
    )
    .expect("valid tile should build");

    assert_eq!(tile.width(), 2);
    assert_eq!(tile.height(), 1);
    assert_eq!(tile.channels(), 3);
    assert_eq!(tile.color_space(), &ColorSpace::Rgb);
    assert_eq!(tile.layout(), CpuTileLayout::Interleaved);
    assert_eq!(tile.data().as_u8().unwrap(), &[10, 20, 30, 40, 50, 60]);
    assert_eq!(tile.stride_bytes(), 6);
}

#[test]
fn typed_sample_mutation_is_copy_on_write_for_every_sample_type() {
    let original_u8 = CpuTileData::u8(vec![1, 2]);
    let mut changed_u8 = original_u8.clone();
    changed_u8.make_mut_u8().unwrap()[0] = 9;
    assert_eq!(original_u8.as_u8(), Some(&[1, 2][..]));
    assert_eq!(changed_u8.as_u8(), Some(&[9, 2][..]));

    let original_u16 = CpuTileData::u16(vec![10, 20]);
    let mut changed_u16 = original_u16.clone();
    changed_u16.make_mut_u16().unwrap()[1] = 99;
    assert_eq!(original_u16.as_u16(), Some(&[10, 20][..]));
    assert_eq!(changed_u16.as_u16(), Some(&[10, 99][..]));

    let original_f32 = CpuTileData::f32(vec![0.25, 0.5]);
    let mut changed_f32 = original_f32.clone();
    changed_f32.make_mut_f32().unwrap()[0] = 1.0;
    assert_eq!(original_f32.as_f32(), Some(&[0.25, 0.5][..]));
    assert_eq!(changed_f32.as_f32(), Some(&[1.0, 0.5][..]));
}

#[test]
fn pixels_arc_clones_the_underlying_u8_storage() {
    let pixels = Arc::new(vec![10, 20, 30, 40, 50, 60]);
    let tile = CpuTile::new(
        2,
        1,
        3,
        ColorSpace::Rgb,
        CpuTileLayout::Interleaved,
        CpuTileData::U8(Arc::clone(&pixels)),
    )
    .expect("valid tile should build");

    let cloned = tile.pixels_arc().expect("U8 tile has shared pixels");
    assert!(Arc::ptr_eq(&pixels, &cloned));
}

#[test]
fn sample_buffer_new_invalid_length() {
    let buf = CpuTile::new(
        2,
        1,
        3,
        ColorSpace::Rgb,
        CpuTileLayout::Interleaved,
        CpuTileData::u8(vec![0; 5]),
    );
    assert!(buf.is_err());
}

#[test]
fn sample_buffer_new_overflow_dimensions() {
    let buf = CpuTile::new(
        u32::MAX,
        u32::MAX,
        3,
        ColorSpace::Rgb,
        CpuTileLayout::Interleaved,
        CpuTileData::u8(vec![]),
    );
    assert!(buf.is_err());
}

// --- Direct to_rgb() paths ---
