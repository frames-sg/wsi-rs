use super::super::*;
use super::fixtures::*;

#[test]
fn ndpi_restart_tile_decodes_target_strip_via_public_read_path() {
    let reader = build_test_ndpi_restart_reader(false);
    let tile = read_test_ndpi_level0_tile(&reader, 1, 1);

    assert_eq!(tile.width, 64);
    assert_eq!(tile.height, 8);
    let CpuTileData::U8(rgb) = tile.data else {
        panic!("expected RGB data");
    };
    let pixel = [rgb[0], rgb[1], rgb[2]];
    assert!(
        pixel[0] > 170,
        "expected red channel dominance, got {pixel:?}"
    );
    assert!(
        pixel[1] > 170,
        "expected green channel dominance, got {pixel:?}"
    );
    assert!(
        pixel[2] < 120,
        "expected blue channel to stay lower, got {pixel:?}"
    );

    let ifd_id = *reader.container.top_ifds().first().unwrap();
    let cache = &reader.ndpi_strip_cache;
    assert!(cache
        .get(&NdpiStripKey {
            ifd_id,
            col: 1,
            native_row: 1,
        })
        .is_some());
}

#[cfg(any(feature = "metal", feature = "cuda"))]
#[test]
fn ndpi_restart_tile_decodes_to_resident_device_tile() {
    #[cfg(feature = "metal")]
    let Ok(sessions) = crate::output::metal::MetalBackendSessions::system_default() else {
        return;
    };
    #[cfg(all(not(feature = "metal"), feature = "cuda"))]
    if std::env::var_os("J2K_REQUIRE_CUDA_RUNTIME").is_none() {
        eprintln!("skipping CUDA NDPI device test; J2K_REQUIRE_CUDA_RUNTIME is unset");
        return;
    }
    let (file, jpeg_header, strip_byte_count) = build_ndpi_scan_data_tiff_from_blobs(
        128,
        16,
        &[[240, 20, 20], [20, 220, 20], [20, 20, 230], [220, 220, 30]],
        false,
    );
    let container = Arc::new(TiffContainer::open(file.path()).unwrap());
    let ifd_id = *container.top_ifds().first().unwrap();
    let layout = build_test_ndpi_layout_from_header(TestNdpiJpegLayout {
        ifd_id,
        dimensions: (128, 16),
        virtual_tile: (64, 8),
        tile_grid: (2, 2),
        jpeg_header,
        strip_byte_count,
    });
    let reader = TiffPixelReader::new(container, layout);

    #[cfg(feature = "metal")]
    let output =
        TileOutputPreference::require_device_auto_with_metal_and_compressed_decode(sessions)
            .without_adaptive_decode_route();
    #[cfg(all(not(feature = "metal"), feature = "cuda"))]
    let output = TileOutputPreference::require_device_auto_with_cuda_and_compressed_decode(
        crate::output::cuda::CudaBackendSessions::new(),
    )
    .without_adaptive_decode_route();

    let tiles = reader
        .read_tiles(
            &[TileRequest {
                scene: 0usize.into(),
                series: 0usize.into(),
                level: 0u32.into(),
                plane: PlaneSelection::default().into(),
                col: 1,
                row: 1,
            }],
            output,
        )
        .unwrap();

    assert_eq!(tiles.len(), 1);
    #[cfg(feature = "metal")]
    let TilePixels::Device(DeviceTile::Metal(tile)) = &tiles[0] else {
        panic!("expected NDPI tile to decode to Metal");
    };
    #[cfg(all(not(feature = "metal"), feature = "cuda"))]
    let TilePixels::Device(DeviceTile::Cuda(tile)) = &tiles[0] else {
        panic!("expected NDPI tile to decode to CUDA");
    };
    assert_eq!((tile.width, tile.height), (64, 8));
    assert_eq!(tile.format, PixelFormat::Rgb8);
    #[cfg(all(not(feature = "metal"), feature = "cuda"))]
    assert_ne!(tile.storage.device_ptr(), 0);
}

#[test]
fn ndpi_restart_tile_does_not_silently_fallback_to_full_decode_on_bad_mcu_table() {
    let jpeg = {
        let mut encoded = Vec::new();
        let image = image::RgbImage::new(8, 8);
        JpegEncoder::new(&mut encoded, 75)
            .encode(
                image.as_raw().as_slice(),
                image.width() as u16,
                image.height() as u16,
                JpegColorType::Rgb,
            )
            .unwrap();
        encoded
    };
    let file = build_stripped_jpeg_tiff(8, 8, &jpeg);
    let container = Arc::new(TiffContainer::open(file.path()).unwrap());
    let ifd_id = *container.top_ifds().first().unwrap();
    let layout = build_test_ndpi_layout_from_header_with_restart_interval(
        TestNdpiJpegLayout {
            ifd_id,
            dimensions: (8, 8),
            virtual_tile: (8, 8),
            tile_grid: (1, 1),
            jpeg_header: Vec::new(),
            strip_byte_count: jpeg.len() as u64,
        },
        8,
        1,
    );
    let reader = TiffPixelReader::new(container, layout);

    let err = reader
        .read_tile_cpu(&TileRequest {
            scene: 0usize.into(),
            series: 0usize.into(),
            level: 0u32.into(),
            plane: PlaneSelection::default().into(),
            col: 0,
            row: 0,
        })
        .unwrap_err();
    assert!(
        err.to_string().contains("65426") || err.to_string().contains("MCU"),
        "unexpected error: {err}"
    );
}

#[test]
fn ndpi_restart_tile_decodes_zero_sof_segment_from_mcu_table() {
    let reader = build_test_ndpi_restart_reader(true);
    let tile = read_test_ndpi_level0_tile(&reader, 0, 0);

    assert_eq!(tile.width, 64);
    assert_eq!(tile.height, 8);
    let rgb = tile.data.as_u8().expect("expected RGB tile");
    assert!(
        rgb[0] > 180 && rgb[1] < 80 && rgb[2] < 80,
        "unexpected first pixel for zero-SOF NDPI tile: {:?}",
        &rgb[0..3]
    );
}

#[test]
fn ndpi_raw_compressed_display_tile_retiles_restart_jpeg_segments_without_pixel_reencode() {
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
        jpeg_header,
        strip_byte_count,
    });
    let reader = TiffPixelReader::new(container, layout);
    let request = TileViewRequest {
        scene: 0usize.into(),
        series: 0usize.into(),
        level: 0u32.into(),
        plane: PlaneSelection::default().into(),
        col: 0,
        row: 0,
        tile_width: 128,
        tile_height: 16,
    };

    let raw = reader.read_raw_compressed_display_tile(&request).unwrap();

    assert_eq!(raw.compression(), Compression::Jpeg);
    assert_eq!((raw.width(), raw.height()), (128, 16));
    assert_eq!(raw.bits_allocated(), 8);
    assert_eq!(raw.samples_per_pixel(), 3);
    assert!(raw.data().starts_with(&[0xFF, 0xD8]));
    assert!(raw.data().ends_with(&[0xFF, 0xD9]));

    let decoded = decode_jpeg_rgb_with_size_override(
        raw.data(),
        None,
        raw.width(),
        raw.height(),
        None,
        None,
        J2kColorTransform::Auto,
    )
    .expect("decode retiled NDPI JPEG frame");
    let expected = reader.read_display_tile(&request).unwrap();
    assert_eq!(
        (decoded.width, decoded.height),
        (expected.width, expected.height)
    );
    assert_eq!(decoded.pixels, *expected.data.as_u8().unwrap());
}
