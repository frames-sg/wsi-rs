use super::super::*;

#[test]
fn clamp_ndpi_strip_crop_limits_edge_requests_to_strip_bounds() {
    assert_eq!(
        TiffPixelReader::clamp_ndpi_strip_crop(112, 0, 136, 240, 104, 240),
        None
    );
    assert_eq!(
        TiffPixelReader::clamp_ndpi_strip_crop(0, 0, 136, 240, 104, 240),
        Some((104, 240))
    );
    assert_eq!(
        TiffPixelReader::clamp_ndpi_strip_crop(112, 16, 136, 240, 248, 240),
        Some((136, 224))
    );
}

fn make_ndpi_strip(width: u32, height: u32, rgb: [u8; 3]) -> Arc<CpuTile> {
    let mut data = vec![0u8; width as usize * height as usize * 3];
    for pixel in data.chunks_exact_mut(3) {
        pixel.copy_from_slice(&rgb);
    }
    Arc::new(CpuTile {
        width,
        height,
        channels: 3,
        color_space: ColorSpace::Rgb,
        layout: CpuTileLayout::Interleaved,
        data: CpuTileData::u8(data),
    })
}

#[test]
fn ndpi_display_tile_only_populates_requested_strip_keys() {
    let (reader, ifd_id) = build_test_ndpi_reader_for_strip_cache(680, 72, 5);

    let tile = reader
        .read_display_tile(&TileViewRequest {
            scene: 0usize.into(),
            series: 0usize.into(),
            level: 1u32.into(),
            plane: PlaneSelection::default().into(),
            col: 0,
            row: 0,
            tile_width: 250,
            tile_height: 32,
        })
        .unwrap();

    assert_eq!(tile.width, 250);
    assert_eq!(tile.height, 32);

    let cache = &reader.ndpi_strip_cache;
    assert!(cache
        .get(&NdpiStripKey {
            ifd_id,
            col: 0,
            native_row: 0
        })
        .is_some());
    assert!(cache
        .get(&NdpiStripKey {
            ifd_id,
            col: 1,
            native_row: 0
        })
        .is_some());
    assert!(cache
        .get(&NdpiStripKey {
            ifd_id,
            col: 0,
            native_row: 1
        })
        .is_some());
    assert!(cache
        .get(&NdpiStripKey {
            ifd_id,
            col: 1,
            native_row: 1
        })
        .is_some());
    assert!(cache
        .get(&NdpiStripKey {
            ifd_id,
            col: 2,
            native_row: 1
        })
        .is_none());
}

#[test]
fn ndpi_display_tile_composites_from_strip_cache_across_rows_and_columns() {
    let (reader, ifd_id) = build_test_ndpi_reader_for_strip_cache(256, 48, 2);
    {
        let cache = &reader.ndpi_strip_cache;
        cache.put(
            NdpiStripKey {
                ifd_id,
                col: 0,
                native_row: 0,
            },
            make_ndpi_strip(128, 16, [10, 0, 0]),
        );
        cache.put(
            NdpiStripKey {
                ifd_id,
                col: 1,
                native_row: 0,
            },
            make_ndpi_strip(128, 16, [20, 0, 0]),
        );
        cache.put(
            NdpiStripKey {
                ifd_id,
                col: 0,
                native_row: 1,
            },
            make_ndpi_strip(128, 16, [30, 0, 0]),
        );
        cache.put(
            NdpiStripKey {
                ifd_id,
                col: 1,
                native_row: 1,
            },
            make_ndpi_strip(128, 16, [40, 0, 0]),
        );
    }

    let tile = reader
        .read_display_tile(&TileViewRequest {
            scene: 0usize.into(),
            series: 0usize.into(),
            level: 1u32.into(),
            plane: PlaneSelection::default().into(),
            row: 0,
            col: 0,
            tile_width: 200,
            tile_height: 32,
        })
        .unwrap();

    let CpuTileData::U8(rgb) = tile.data else {
        panic!("expected RGB data");
    };
    assert_eq!(&rgb[0..3], &[10, 0, 0]);
    let right = 128 * 3;
    assert_eq!(&rgb[right..right + 3], &[20, 0, 0]);
    let lower = (16 * tile.width as usize) * 3;
    assert_eq!(&rgb[lower..lower + 3], &[30, 0, 0]);
    let lower_right = ((16 * tile.width as usize) + 128) * 3;
    assert_eq!(&rgb[lower_right..lower_right + 3], &[40, 0, 0]);
}

#[test]
fn ndpi_display_tile_composites_across_multiple_strip_rows_and_columns() {
    let (reader, ifd_id) = build_test_ndpi_reader_for_strip_cache(320, 600, 3);
    {
        let cache = &reader.ndpi_strip_cache;
        for native_row in 16..=31 {
            for col in 0..=1 {
                cache.put(
                    NdpiStripKey {
                        ifd_id,
                        col,
                        native_row,
                    },
                    make_ndpi_strip(128, 16, [(col * 50) as u8, native_row as u8, 7]),
                );
            }
        }
    }

    let tile = reader
        .read_display_tile(&TileViewRequest {
            scene: 0usize.into(),
            series: 0usize.into(),
            level: 1u32.into(),
            plane: PlaneSelection::default().into(),
            col: 0,
            row: 1,
            tile_width: 256,
            tile_height: 256,
        })
        .unwrap();

    assert_eq!(tile.width, 256);
    assert_eq!(tile.height, 256);
    let rgb = tile.data.as_u8().unwrap();
    let pixel = |x: usize, y: usize| -> [u8; 3] {
        let idx = (y * tile.width as usize + x) * 3;
        [rgb[idx], rgb[idx + 1], rgb[idx + 2]]
    };

    assert_eq!(pixel(50, 4), [0, 16, 7]);
    assert_eq!(pixel(50, 20), [0, 17, 7]);
    assert_eq!(pixel(200, 20), [50, 17, 7]);
}

#[test]
fn ndpi_region_fastpath_matches_clipped_sequential_composition() {
    let reader = super::fixtures::build_test_ndpi_restart_reader(false);
    for budget in [0, 1, 1024 * 1024] {
        let cache = crate::TileCache::new(budget);
        for (origin, size) in [
            ((0, 0), (128, 16)),
            ((-7, -2), (77, 19)),
            ((120, 8), (23, 13)),
        ] {
            let req = RegionRequest::new(0usize, 0usize, 0u32, origin, size);
            let expected = composite_region_from_source(&reader, None, &req, 8192).unwrap();
            let mut ctx = crate::core::registry::SlideReadContext::new(Some(&cache), 8192);
            let actual = reader
                .read_region_fastpath(&mut ctx, &req)
                .expect("restart strips should use bounded region batches")
                .unwrap();
            assert_eq!((actual.width(), actual.height()), size);
            assert_eq!(actual.as_u8(), expected.as_u8());
        }
    }
}

#[test]
fn ndpi_batch_preserves_duplicates_and_first_error_order() {
    let reader = super::fixtures::build_test_ndpi_restart_reader(false);
    let reqs =
        [(1, 1), (0, 0), (1, 0), (1, 1)].map(|(x, y)| TileRequest::new(0usize, 0usize, 0u32, x, y));
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(4)
        .build()
        .unwrap();
    let region_reader = super::super::super::ndpi_batch::NdpiRegionReader(&reader);
    let actual = pool
        .install(|| region_reader.read_tiles_cpu(&reqs))
        .unwrap();
    for (req, tile) in reqs.iter().zip(actual) {
        assert_eq!(tile.as_u8(), reader.read_tile_cpu(req).unwrap().as_u8());
    }
    let bad = [
        TileRequest::new(0usize, 0usize, 0u32, 9, 0),
        TileRequest::new(0usize, 0usize, 0u32, -1, 0),
    ];
    let error = pool
        .install(|| region_reader.read_tiles_cpu(&bad))
        .unwrap_err();
    assert!(matches!(error, WsiError::TileRead { col: 9, .. }));
}

#[test]
fn cancelled_ndpi_batch_does_not_start_source_decodes() {
    let reader = super::fixtures::build_test_ndpi_restart_reader(false);
    let token = crate::ReadCancellationToken::new();
    token.cancel();
    let reqs = [
        TileRequest::new(0usize, 0usize, 0u32, 0, 0),
        TileRequest::new(0usize, 0usize, 0u32, 1, 0),
    ];
    assert!(matches!(
        reader.read_tiles_cpu_controlled(&reqs, &crate::ReadControl::new(token)),
        Err(WsiError::Cancelled)
    ));
    assert_eq!(reader.ndpi_strip_cache.current_bytes(), 0);
}
