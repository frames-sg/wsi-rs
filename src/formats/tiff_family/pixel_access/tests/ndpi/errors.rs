use super::super::*;
use super::fixtures::*;

#[test]
fn ndpi_cpu_tile_falls_back_to_full_decode_when_mcu_table_is_invalid() {
    let colors = [[240, 20, 20], [20, 220, 20], [20, 20, 230], [220, 220, 30]];
    let (file, jpeg_header, strip_byte_count, strip_offset) =
        build_ndpi_scan_data_tiff_from_blobs_with_mcu_mode_and_offset(
            128,
            16,
            &colors,
            false,
            TestMcuStartsMode::InvalidFileAbsolute,
        );
    let container = Arc::new(TiffContainer::open(file.path()).unwrap());
    let ifd_id = *container.top_ifds().first().unwrap();
    let layout = build_test_ndpi_layout_from_header_with_strip_offset(
        TestNdpiJpegLayout {
            ifd_id,
            dimensions: (128, 16),
            virtual_tile: (64, 8),
            tile_grid: (2, 2),
            jpeg_header,
            strip_byte_count,
        },
        strip_offset,
    );
    let reader = TiffPixelReader::new(container, layout);
    let req = TileRequest::new(0usize, 0usize, 0u32, 0, 0);

    let raw_err = reader.read_raw_compressed_tile(&req).unwrap_err();
    assert!(raw_err.to_string().contains("exceeds strip byte count"));

    let tile = reader.read_tile_cpu(&req).unwrap();
    assert_eq!((tile.width, tile.height), (64, 8));
    assert_eq!(tile.channels, 3);
}

#[test]
fn ndpi_raw_compressed_display_tile_rejects_invalid_layouts_and_coordinates() {
    let colors = [[240, 20, 20], [20, 220, 20], [20, 20, 230], [220, 220, 30]];
    let (file, jpeg_header, strip_byte_count) =
        build_ndpi_scan_data_tiff_from_blobs(128, 16, &colors, false);
    let container = Arc::new(TiffContainer::open(file.path()).unwrap());
    let ifd_id = *container.top_ifds().first().unwrap();
    let layout = build_test_ndpi_layout_from_header(TestNdpiJpegLayout {
        ifd_id,
        dimensions: (128, 16),
        virtual_tile: (64, 8),
        tile_grid: (2, 2),
        jpeg_header: jpeg_header.clone(),
        strip_byte_count,
    });
    let mut reader = TiffPixelReader::new(container, layout);
    let request = TileViewRequest::new(0usize, 0usize, 0u32, 0, 0, 128, 16);

    reader.layout.dataset.scenes[0].series[0].levels[0].tile_layout = TileLayout::WholeLevel {
        width: 128,
        height: 16,
        virtual_tile_width: 0,
        virtual_tile_height: 8,
    };
    let err = reader
        .read_ndpi_raw_compressed_display_tile(
            &request,
            ifd_id,
            &jpeg_header,
            65426,
            2,
            2,
            8,
            8,
            strip_byte_count,
        )
        .unwrap_err();
    assert!(err
        .to_string()
        .contains("nonzero WholeLevel virtual tile dimensions"));

    reader.layout.dataset.scenes[0].series[0].levels[0].tile_layout = TileLayout::Regular {
        tile_width: 64,
        tile_height: 8,
        tiles_across: 2,
        tiles_down: 2,
    };
    let err = reader
        .read_ndpi_raw_compressed_display_tile(
            &request,
            ifd_id,
            &jpeg_header,
            65426,
            2,
            2,
            8,
            8,
            strip_byte_count,
        )
        .unwrap_err();
    assert!(err.to_string().contains("expects WholeLevel tile layout"));

    reader.layout.dataset.scenes[0].series[0].levels[0].tile_layout = TileLayout::WholeLevel {
        width: 128,
        height: 16,
        virtual_tile_width: 64,
        virtual_tile_height: 8,
    };

    for (request, expected) in [
        (
            TileViewRequest::new(0usize, 0usize, 0u32, i64::MAX, 0, 2, 16),
            "tile x offset overflow",
        ),
        (
            TileViewRequest::new(0usize, 0usize, 0u32, 0, i64::MAX, 128, 2),
            "tile y offset overflow",
        ),
        (
            TileViewRequest::new(0usize, 0usize, 0u32, 2, 0, 128, 16),
            "origin out of bounds",
        ),
        (
            TileViewRequest::new(0usize, 0usize, 0u32, 0, 0, 0, 16),
            "requested empty frame",
        ),
    ] {
        let err = reader
            .read_ndpi_raw_compressed_display_tile(
                &request,
                ifd_id,
                &jpeg_header,
                65426,
                2,
                2,
                8,
                8,
                strip_byte_count,
            )
            .unwrap_err();
        assert!(
            err.to_string().contains(expected),
            "expected {expected:?}, got {err}"
        );
    }
}

#[test]
fn ndpi_display_tile_rejects_invalid_layout_coordinates_and_cached_strips() {
    let (mut reader, ifd_id) = build_test_ndpi_reader_for_strip_cache(128, 16, 1);
    let TileSource::NdpiJpeg {
        jpeg_header,
        mcu_starts_tag,
        tiles_across,
        tiles_down,
        strip_offset,
        strip_byte_count,
        ..
    } = reader
        .layout
        .tile_sources
        .values()
        .next()
        .expect("NDPI tile source")
        .clone()
    else {
        panic!("expected NDPI tile source");
    };
    let request = TileViewRequest::new(0usize, 0usize, 1u32, 0, 0, 128, 16);

    reader.layout.dataset.scenes[0].series[0].levels[1].tile_layout = TileLayout::Regular {
        tile_width: 128,
        tile_height: 16,
        tiles_across: 1,
        tiles_down: 1,
    };
    let err = reader
        .read_ndpi_display_tile(
            &request,
            ifd_id,
            &jpeg_header,
            mcu_starts_tag,
            tiles_across,
            tiles_down,
            strip_offset,
            strip_byte_count,
        )
        .unwrap_err();
    assert!(err.to_string().contains("expects WholeLevel layout"));

    reader.layout.dataset.scenes[0].series[0].levels[1].tile_layout = TileLayout::WholeLevel {
        width: 128,
        height: 16,
        virtual_tile_width: 128,
        virtual_tile_height: 16,
    };
    let err = reader
        .read_ndpi_display_tile(
            &TileViewRequest::new(0usize, 0usize, 1u32, 1, 0, 128, 16),
            ifd_id,
            &jpeg_header,
            mcu_starts_tag,
            tiles_across,
            tiles_down,
            strip_offset,
            strip_byte_count,
        )
        .unwrap_err();
    assert!(err.to_string().contains("origin out of bounds"));

    let key = NdpiStripKey {
        ifd_id,
        col: 0,
        native_row: 0,
    };
    reader.ndpi_strip_cache.put(
        key,
        Arc::new(CpuTile {
            width: 128,
            height: 16,
            channels: 3,
            color_space: ColorSpace::Rgb,
            layout: CpuTileLayout::Planar,
            data: CpuTileData::u8(vec![0; 128 * 16 * 3]),
        }),
    );
    let err = reader
        .read_ndpi_display_tile(
            &request,
            ifd_id,
            &jpeg_header,
            mcu_starts_tag,
            tiles_across,
            tiles_down,
            strip_offset,
            strip_byte_count,
        )
        .unwrap_err();
    assert!(err.to_string().contains("expected interleaved RGB strips"));

    reader.ndpi_strip_cache.put(
        key,
        Arc::new(CpuTile {
            width: 128,
            height: 16,
            channels: 3,
            color_space: ColorSpace::Rgb,
            layout: CpuTileLayout::Interleaved,
            data: CpuTileData::u16(vec![0; 128 * 16 * 3]),
        }),
    );
    let err = reader
        .read_ndpi_display_tile(
            &request,
            ifd_id,
            &jpeg_header,
            mcu_starts_tag,
            tiles_across,
            tiles_down,
            strip_offset,
            strip_byte_count,
        )
        .unwrap_err();
    assert!(err.to_string().contains("expected U8 RGB strip data"));
}

#[test]
fn ndpi_raw_jpeg_tile_rejects_invalid_layout_and_coordinates() {
    let (mut reader, ifd_id) = build_test_ndpi_reader_for_strip_cache(128, 16, 1);
    let TileSource::NdpiJpeg {
        jpeg_header,
        mcu_starts_tag,
        tiles_across,
        tiles_down,
        restart_interval,
        strip_offset,
        strip_byte_count,
        ..
    } = reader
        .layout
        .tile_sources
        .values()
        .next()
        .expect("NDPI tile source")
        .clone()
    else {
        panic!("expected NDPI tile source");
    };

    let err = reader
        .read_ndpi_raw_jpeg_tile(
            &TileRequest::new(0usize, 0usize, 1u32, 1, 0),
            ifd_id,
            &jpeg_header,
            mcu_starts_tag,
            tiles_across,
            tiles_down,
            restart_interval,
            strip_offset,
            strip_byte_count,
        )
        .unwrap_err();
    assert!(err.to_string().contains("raw JPEG tile (1,0) out of range"));

    let err = reader
        .read_ndpi_restart_tile(
            &TileRequest::new(0usize, 0usize, 1u32, 1, 0),
            ifd_id,
            &jpeg_header,
            mcu_starts_tag,
            tiles_across,
            tiles_down,
            restart_interval,
            strip_offset,
            strip_byte_count,
        )
        .unwrap_err();
    assert!(err.to_string().contains("tile (1,0) out of range"));

    reader.layout.dataset.scenes[0].series[0].levels[1].tile_layout = TileLayout::WholeLevel {
        width: 128,
        height: 16,
        virtual_tile_width: 0,
        virtual_tile_height: 16,
    };
    let err = reader
        .read_ndpi_raw_jpeg_tile(
            &TileRequest::new(0usize, 0usize, 1u32, 0, 0),
            ifd_id,
            &jpeg_header,
            mcu_starts_tag,
            tiles_across,
            tiles_down,
            restart_interval,
            strip_offset,
            strip_byte_count,
        )
        .unwrap_err();
    assert!(err
        .to_string()
        .contains("requires nonzero WholeLevel virtual tile dimensions"));

    reader.layout.dataset.scenes[0].series[0].levels[1].tile_layout = TileLayout::Regular {
        tile_width: 128,
        tile_height: 16,
        tiles_across: 1,
        tiles_down: 1,
    };
    let err = reader
        .read_ndpi_raw_jpeg_tile(
            &TileRequest::new(0usize, 0usize, 1u32, 0, 0),
            ifd_id,
            &jpeg_header,
            mcu_starts_tag,
            tiles_across,
            tiles_down,
            restart_interval,
            strip_offset,
            strip_byte_count,
        )
        .unwrap_err();
    assert!(err.to_string().contains("expects WholeLevel tile layout"));

    let err = reader
        .read_ndpi_restart_tile(
            &TileRequest::new(0usize, 0usize, 1u32, 0, 0),
            ifd_id,
            &jpeg_header,
            mcu_starts_tag,
            tiles_across,
            tiles_down,
            restart_interval,
            strip_offset,
            strip_byte_count,
        )
        .unwrap_err();
    assert!(err.to_string().contains("expects WholeLevel tile layout"));
}
