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
