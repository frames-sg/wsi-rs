use super::*;

#[test]
fn synthetic_ndpi_levels_downsample_smallest_physical_level() {
    let mut image = image::RgbImage::new(4, 4);
    let source_pixels = [
        [10, 20, 30],
        [30, 40, 50],
        [50, 60, 70],
        [70, 80, 90],
        [90, 100, 110],
        [110, 120, 130],
        [130, 140, 150],
        [150, 160, 170],
        [20, 30, 40],
        [40, 50, 60],
        [60, 70, 80],
        [80, 90, 100],
        [100, 110, 120],
        [120, 130, 140],
        [140, 150, 160],
        [160, 170, 180],
    ];
    for (pixel, rgb) in image.pixels_mut().zip(source_pixels) {
        *pixel = image::Rgb(rgb);
    }
    let mut jpeg = Vec::new();
    JpegEncoder::new(&mut jpeg, 100)
        .encode(
            image.as_raw().as_slice(),
            image.width() as u16,
            image.height() as u16,
            JpegColorType::Rgb,
        )
        .unwrap();
    let file = build_stripped_jpeg_tiff(4, 4, &jpeg);
    let container = Arc::new(TiffContainer::open(file.path()).unwrap());
    let ifd_id = *container.top_ifds().first().unwrap();
    let layout = single_series_layout(
        DatasetId::new(99),
        vec![
            whole_level((4, 4), 1.0, (4, 4)),
            whole_level((2, 2), 2.0, (2, 2)),
            whole_level((1, 1), 4.0, (1, 1)),
        ],
        HashMap::from([
            (
                tile_source_key(0),
                TileSource::NdpiFullDecode {
                    ifd_id,
                    jpeg_header: Vec::new(),
                    strip_offset: 8,
                    strip_byte_count: jpeg.len() as u64,
                },
            ),
            (
                tile_source_key(1),
                TileSource::SyntheticDownsample {
                    base_level: 0u32,
                    factor: 2,
                },
            ),
            (
                tile_source_key(2),
                TileSource::SyntheticDownsample {
                    base_level: 0u32,
                    factor: 4,
                },
            ),
        ]),
    );
    let reader = TiffPixelReader::new(container, layout);

    let level1 = reader
        .read_tile_cpu(&TileRequest {
            scene: 0usize.into(),
            series: 0usize.into(),
            level: 1u32.into(),
            plane: PlaneSelection::default().into(),
            col: 0,
            row: 0,
        })
        .unwrap();
    assert_eq!(level1.width, 2);
    assert_eq!(level1.height, 2);
    let level1_rgb = level1.data.as_u8().unwrap();
    assert_rgb_close(&level1_rgb[0..3], &[60, 70, 80], 1);
    assert_rgb_close(&level1_rgb[3..6], &[100, 110, 120], 1);
    assert_rgb_close(&level1_rgb[6..9], &[70, 80, 90], 1);
    assert_rgb_close(&level1_rgb[9..12], &[110, 120, 130], 1);

    let level2 = reader
        .read_tile_cpu(&TileRequest {
            scene: 0usize.into(),
            series: 0usize.into(),
            level: 2u32.into(),
            plane: PlaneSelection::default().into(),
            col: 0,
            row: 0,
        })
        .unwrap();
    assert_eq!(level2.width, 1);
    assert_eq!(level2.height, 1);
    let level2_rgb = level2.data.as_u8().unwrap();
    assert_rgb_close(&level2_rgb[0..3], &[85, 95, 105], 1);
}

fn assert_rgb_close(actual: &[u8], expected: &[u8; 3], tolerance: u8) {
    assert_eq!(actual.len(), 3);
    for (actual, expected) in actual.iter().zip(expected.iter()) {
        assert!(
            actual.abs_diff(*expected) <= tolerance,
            "actual RGB channel {actual} differs from expected {expected} by more than {tolerance}"
        );
    }
}

fn synthetic_ndpi_base_pixel(x: u32, y: u32) -> [u8; 3] {
    [
        (10 + x.saturating_mul(7) + y.saturating_mul(3)).min(255) as u8,
        (20 + x.saturating_mul(5) + y.saturating_mul(11)).min(255) as u8,
        (30 + x.saturating_mul(13) + y.saturating_mul(2)).min(255) as u8,
    ]
}

fn synthetic_ndpi_base_image(width: u32, height: u32) -> image::RgbImage {
    image::RgbImage::from_fn(width, height, |x, y| {
        image::Rgb(synthetic_ndpi_base_pixel(x, y))
    })
}

fn crop_rgb_with_zero_fill(source: &CpuTile, x: i64, y: i64, w: u32, h: u32) -> CpuTile {
    assert_eq!(source.channels, 3);
    assert_eq!(source.color_space, ColorSpace::Rgb);
    assert_eq!(source.layout, CpuTileLayout::Interleaved);
    let src = source.data.as_u8().unwrap();
    let mut out = vec![0u8; w as usize * h as usize * 3];
    let clipped_x0 = x.max(0).min(i64::from(source.width));
    let clipped_y0 = y.max(0).min(i64::from(source.height));
    let clipped_x1 = x
        .saturating_add(i64::from(w))
        .max(0)
        .min(i64::from(source.width));
    let clipped_y1 = y
        .saturating_add(i64::from(h))
        .max(0)
        .min(i64::from(source.height));
    if clipped_x1 <= clipped_x0 || clipped_y1 <= clipped_y0 {
        return CpuTile {
            width: w,
            height: h,
            channels: 3,
            color_space: ColorSpace::Rgb,
            layout: CpuTileLayout::Interleaved,
            data: CpuTileData::u8(out),
        };
    }

    let copy_w = (clipped_x1 - clipped_x0) as usize;
    let copy_h = (clipped_y1 - clipped_y0) as usize;
    let dst_x = (clipped_x0 - x) as usize;
    let dst_y = (clipped_y0 - y) as usize;
    let src_stride = source.width as usize * 3;
    let dst_stride = w as usize * 3;
    for row in 0..copy_h {
        let src_off = (clipped_y0 as usize + row) * src_stride + clipped_x0 as usize * 3;
        let dst_off = (dst_y + row) * dst_stride + dst_x * 3;
        out[dst_off..dst_off + copy_w * 3].copy_from_slice(&src[src_off..src_off + copy_w * 3]);
    }

    CpuTile {
        width: w,
        height: h,
        channels: 3,
        color_space: ColorSpace::Rgb,
        layout: CpuTileLayout::Interleaved,
        data: CpuTileData::u8(out),
    }
}

fn expected_synthetic_ndpi_region(
    reader: &TiffPixelReader,
    factor: u32,
    x: i64,
    y: i64,
    w: u32,
    h: u32,
) -> CpuTile {
    let tile_req = TileRequest {
        scene: 0usize.into(),
        series: 0usize.into(),
        level: 1u32.into(),
        plane: PlaneSelection::default().into(),
        col: 0,
        row: 0,
    };
    let full = if let Some(image) = reader
        .try_decode_synthetic_level_with_j2k(&tile_req, 0, factor)
        .unwrap()
    {
        image
    } else {
        let mut base = reader
            .read_tile_cpu(&TileRequest {
                scene: 0usize.into(),
                series: 0usize.into(),
                level: 0u32.into(),
                plane: PlaneSelection::default().into(),
                col: 0,
                row: 0,
            })
            .unwrap();
        if base.layout != CpuTileLayout::Interleaved
            || base.channels != 3
            || base.color_space != ColorSpace::Rgb
            || base.data.as_u8().is_none()
        {
            base = rgba_image_to_sample_buffer(base.to_rgba().unwrap());
        }
        let target = &reader.layout.dataset.scenes[0].series[0].levels[1];
        fit_synthetic_rgb_tile_to_dimensions(
            downsample_rgb_pow2_box(&base, factor).unwrap(),
            target.dimensions.0 as u32,
            target.dimensions.1 as u32,
        )
        .unwrap()
    };
    crop_rgb_with_zero_fill(&full, x, y, w, h)
}

fn assert_tile_eq(actual: &CpuTile, expected: &CpuTile) {
    assert_eq!(
        (actual.width, actual.height),
        (expected.width, expected.height)
    );
    assert_eq!(actual.channels, expected.channels);
    assert_eq!(actual.color_space, expected.color_space);
    assert_eq!(actual.layout, expected.layout);
    assert_eq!(actual.data.as_u8().unwrap(), expected.data.as_u8().unwrap());
}

fn read_synthetic_ndpi_region(reader: &TiffPixelReader, x: i64, y: i64, w: u32, h: u32) -> CpuTile {
    let req = region_request(0, 0, 1, PlaneSelection::default(), x, y, w, h);
    let mut ctx = crate::core::registry::SlideReadContext::new(
        None,
        TileOutputPreference::cpu(),
        256 * 1024 * 1024,
    );
    reader
        .read_region_fastpath(&mut ctx, &req)
        .expect("synthetic level should have a region fast path")
        .expect("synthetic region fast path should produce pixels")
}

fn build_synthetic_ndpi_reader(
    width: u32,
    height: u32,
    synthetic: &[(u64, u64, u32)],
) -> TiffPixelReader {
    let image = synthetic_ndpi_base_image(width, height);
    let mut jpeg = Vec::new();
    JpegEncoder::new(&mut jpeg, 95)
        .encode(
            image.as_raw().as_slice(),
            image.width() as u16,
            image.height() as u16,
            JpegColorType::Rgb,
        )
        .unwrap();
    let file = build_stripped_jpeg_tiff(width, height, &jpeg);
    let container = Arc::new(TiffContainer::open(file.path()).unwrap());
    let ifd_id = *container.top_ifds().first().unwrap();

    let mut levels = vec![whole_level(
        (u64::from(width), u64::from(height)),
        1.0,
        (width, height),
    )];
    let mut tile_sources = HashMap::from([(
        tile_source_key(0),
        TileSource::NdpiFullDecode {
            ifd_id,
            jpeg_header: Vec::new(),
            strip_offset: 8,
            strip_byte_count: jpeg.len() as u64,
        },
    )]);

    for (idx, (level_width, level_height, factor)) in synthetic.iter().copied().enumerate() {
        let level_idx = (idx + 1) as u32;
        levels.push(whole_level(
            (level_width, level_height),
            f64::from(factor),
            (level_width as u32, level_height as u32),
        ));
        tile_sources.insert(
            tile_source_key(level_idx),
            TileSource::SyntheticDownsample {
                base_level: 0u32,
                factor,
            },
        );
    }

    let layout = single_series_layout(DatasetId::new(100), levels, tile_sources);
    TiffPixelReader::new(container, layout)
}

#[test]
fn synthetic_ndpi_dataset_access_does_not_prime_level_cache() {
    let reader = build_synthetic_ndpi_reader(8, 8, &[(4, 4, 2)]);

    assert_eq!(
        reader
            .synthetic_region_cache
            .lock()
            .unwrap()
            .current_bytes(),
        0,
        "synthetic region cache should start empty"
    );
    let _ = reader.dataset();
    assert_eq!(
        reader
            .synthetic_region_cache
            .lock()
            .unwrap()
            .current_bytes(),
        0,
        "metadata access must not decode or cache synthetic pixels"
    );
}

#[test]
fn synthetic_ndpi_level_source_kind_marks_generated_downsamples() {
    let reader = build_synthetic_ndpi_reader(8, 8, &[(4, 4, 2)]);

    assert_eq!(
        reader
            .level_source_kind(SceneId::new(0), SeriesId::new(0), LevelIdx::new(0))
            .unwrap(),
        LevelSourceKind::Physical
    );
    assert_eq!(
        reader
            .level_source_kind(SceneId::new(0), SeriesId::new(0), LevelIdx::new(1))
            .unwrap(),
        LevelSourceKind::SyntheticDownsample
    );
}

#[test]
fn synthetic_ndpi_subregion_fastpath_matches_center_roi_without_materializing_level() {
    let reader = build_synthetic_ndpi_reader(8, 8, &[(4, 4, 2)]);
    let tile = read_synthetic_ndpi_region(&reader, 1, 1, 2, 2);
    let expected = expected_synthetic_ndpi_region(&reader, 2, 1, 1, 2, 2);

    assert_tile_eq(&tile, &expected);
    assert_eq!(
        reader.synthetic_level_cache.current_bytes(),
        0,
        "ROI reads must not materialize the whole synthetic level"
    );
    assert_eq!(
        reader
            .synthetic_region_cache
            .lock()
            .unwrap()
            .current_bytes(),
        0,
        "ROI reads must not populate full synthetic region cache entries"
    );
}

#[test]
fn synthetic_ndpi_display_tile_materializes_cacheable_level_for_reuse() {
    let reader = build_synthetic_ndpi_reader(8, 8, &[(4, 4, 2)]);
    let tile = reader
        .read_display_tile(&TileViewRequest {
            scene: 0usize.into(),
            series: 0usize.into(),
            level: 1u32.into(),
            plane: PlaneSelection::default().into(),
            col: 1,
            row: 1,
            tile_width: 2,
            tile_height: 2,
        })
        .unwrap();
    let expected = expected_synthetic_ndpi_region(&reader, 2, 2, 2, 2, 2);

    assert_tile_eq(&tile, &expected);
    assert!(
        reader.synthetic_level_cache.current_bytes() > 0,
        "cacheable display-tile reads should materialize the synthetic level for reuse"
    );
}

#[test]
fn synthetic_ndpi_subregion_fastpath_zero_fills_negative_origin() {
    let reader = build_synthetic_ndpi_reader(8, 8, &[(4, 4, 2)]);
    let tile = read_synthetic_ndpi_region(&reader, -1, -1, 3, 3);
    let expected = expected_synthetic_ndpi_region(&reader, 2, -1, -1, 3, 3);

    assert_tile_eq(&tile, &expected);
}

#[test]
fn synthetic_subregion_u32_sized_clipping_reaches_source_validation_without_overflow() {
    let reader = build_tiled_jpeg_reader(1, 1, 1, 1, &[encode_solid_rgb_jpeg(1, 1, [1, 2, 3])]);
    let request = RegionRequest {
        scene: 0usize.into(),
        series: 0usize.into(),
        level: 0u32.into(),
        plane: PlaneSelection::default().into(),
        origin_px: (i64::MAX, 0),
        size_px: (u32::MAX, 1),
    };

    let error = reader
        .read_synthetic_subregion_fastpath(None, &request, 9, 2, (u64::MAX, 1), u64::MAX)
        .unwrap_err();
    assert!(matches!(error, WsiError::TileRead { .. }));
}

#[test]
fn synthetic_ndpi_subregion_fastpath_keeps_odd_ceil_edge_pixels() {
    let reader = build_synthetic_ndpi_reader(5, 5, &[(3, 3, 2)]);
    let tile = read_synthetic_ndpi_region(&reader, 2, 2, 1, 1);
    let expected = expected_synthetic_ndpi_region(&reader, 2, 2, 2, 1, 1);

    assert_tile_eq(&tile, &expected);
}

#[test]
fn synthetic_ndpi_subregion_fastpath_respects_cropped_synthetic_dimensions() {
    let reader = build_synthetic_ndpi_reader(5, 5, &[(2, 2, 2)]);
    let tile = read_synthetic_ndpi_region(&reader, 1, 1, 1, 1);
    let expected = expected_synthetic_ndpi_region(&reader, 2, 1, 1, 1, 1);

    assert_tile_eq(&tile, &expected);
}

#[test]
fn synthetic_ndpi_subregion_fastpath_does_not_prime_deepest_synthetic_level() {
    let reader = build_synthetic_ndpi_reader(8, 8, &[(3, 3, 2), (2, 2, 4)]);
    let tile = read_synthetic_ndpi_region(&reader, 1, 1, 1, 1);
    let expected = expected_synthetic_ndpi_region(&reader, 2, 1, 1, 1, 1);

    assert_tile_eq(&tile, &expected);
    assert_eq!(
        reader.synthetic_level_cache.current_bytes(),
        0,
        "ROI reads must not materialize the requested synthetic level"
    );
    assert_eq!(
        reader
            .synthetic_region_cache
            .lock()
            .unwrap()
            .current_bytes(),
        0,
        "ROI reads must not prime unrelated full synthetic levels"
    );
}

#[test]
fn synthetic_ndpi_subregion_fastpath_matches_factor_four_repeated_box_edges() {
    let reader = build_synthetic_ndpi_reader(9, 7, &[(3, 2, 4)]);
    let tile = read_synthetic_ndpi_region(&reader, 1, 1, 2, 1);
    let expected = expected_synthetic_ndpi_region(&reader, 4, 1, 1, 2, 1);

    assert_tile_eq(&tile, &expected);
}

#[test]
fn synthetic_ndpi_tile_path_uses_j2k_downscale_when_dimensions_match() {
    let reader = build_synthetic_ndpi_reader(8, 8, &[(4, 4, 2)]);
    let direct_req = TileRequest {
        scene: 0usize.into(),
        series: 0usize.into(),
        level: 1u32.into(),
        plane: PlaneSelection::default().into(),
        col: 0,
        row: 0,
    };
    let direct = reader
        .try_decode_synthetic_level_with_j2k(&direct_req, 0, 2)
        .expect("j2k synthetic downscale should decode")
        .expect("matching synthetic dimensions should use j2k downscale");
    assert_eq!((direct.width, direct.height), (4, 4));

    let tile = reader
        .read_tile_cpu(&TileRequest {
            scene: 0usize.into(),
            series: 0usize.into(),
            level: 1u32.into(),
            plane: PlaneSelection::default().into(),
            col: 0,
            row: 0,
        })
        .unwrap();

    assert_eq!((tile.width, tile.height), (4, 4));
}

#[test]
fn synthetic_ndpi_region_fastpath_falls_back_when_j2k_scaled_dims_do_not_match() {
    let reader = build_synthetic_ndpi_reader(5, 5, &[(2, 2, 2)]);
    let direct_req = TileRequest {
        scene: 0usize.into(),
        series: 0usize.into(),
        level: 1u32.into(),
        plane: PlaneSelection::default().into(),
        col: 0,
        row: 0,
    };
    assert!(
        reader
            .try_decode_synthetic_level_with_j2k(&direct_req, 0, 2)
            .expect("j2k synthetic downscale should decode")
            .is_none(),
        "odd source dimensions should reject j2k result with mismatched target dimensions"
    );

    let req = region_request(0, 0, 1, PlaneSelection::default(), 0, 0, 2, 2);
    let mut ctx = crate::core::registry::SlideReadContext::new(
        None,
        TileOutputPreference::cpu(),
        256 * 1024 * 1024,
    );
    let tile = reader
        .read_region_fastpath(&mut ctx, &req)
        .expect("synthetic fast path should handle whole-level region")
        .expect("odd-dimension j2k downscale mismatch should fall back");

    assert_eq!((tile.width, tile.height), (2, 2));
}
