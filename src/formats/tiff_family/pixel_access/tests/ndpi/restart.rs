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
            scale_denom: 1,
        })
        .is_some());
}

#[cfg(any(feature = "metal", feature = "cuda"))]
#[test]
fn strict_device_read_rejects_ndpi_jpeg_restart_tile() {
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

    let requests = [TileRequest {
        scene: 0usize.into(),
        series: 0usize.into(),
        level: 0u32.into(),
        plane: PlaneSelection::default().into(),
        col: 1,
        row: 1,
    }];
    #[cfg(feature = "metal")]
    let error = reader
        .read_tiles_metal(&requests, &sessions)
        .expect_err("strict Metal output supports JP2K/HTJ2K only");
    #[cfg(all(not(feature = "metal"), feature = "cuda"))]
    let error = reader
        .read_tiles_cuda(&requests, &crate::output::cuda::CudaBackendSessions::new())
        .expect_err("strict CUDA output supports JP2K/HTJ2K only");

    assert!(matches!(error, WsiError::TileRead { .. }));
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
    for rgb_components in [false, true] {
        let colors = [[240, 20, 20], [20, 220, 20], [20, 20, 230], [220, 220, 30]];
        let (file, mut jpeg_header, strip_byte_count) =
            build_ndpi_scan_data_tiff_from_blobs(128, 16, &colors, false);
        if rgb_components {
            // Treat the fixture's component samples as untransformed RGB. The
            // reader and the retiled codestream must preserve that interpretation.
            jpeg_header = crate::test_support::jpeg_without_jfif(&jpeg_header);
            jpeg_header.splice(
                2..2,
                [
                    0xFF, 0xEE, 0, 14, b'A', b'd', b'o', b'b', b'e', 0, 100, 0, 0, 0, 0, 0,
                ],
            );
        }
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
        assert_eq!(
            jpeg_segment_color_hint(raw.data()),
            if rgb_components {
                JpegBitstreamColorHint::Rgb
            } else {
                JpegBitstreamColorHint::YCbCr
            },
            "retiling must retain explicit JPEG color semantics for downstream consumers"
        );
        assert_eq!(
            raw.photometric_interpretation(),
            if rgb_components {
                EncodedTilePhotometricInterpretation::Rgb
            } else {
                EncodedTilePhotometricInterpretation::YbrFull422
            }
        );
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
}

#[test]
fn ndpi_retile_window_holds_only_the_frames_quantized_mcu_columns() {
    use j2k_jpeg::transcode::extract_dct_blocks;

    // One 16-row MCU row of textured 4:2:0 content, as one native tile.
    let pixels = (0..16u32)
        .flat_map(|y| (0..128u32).flat_map(move |x| [x * 7 + y * 5, x ^ (y * 9), x * 3 + y * 11]))
        .map(|value| value as u8)
        .collect::<Vec<_>>();
    let mut jpeg = Vec::new();
    let mut encoder = JpegEncoder::new(&mut jpeg, 90);
    encoder.set_sampling_factor(JpegSamplingFactor::F_2_2);
    encoder
        .encode(&pixels, 128, 16, JpegColorType::Rgb)
        .unwrap();
    let full = extract_dct_blocks(&jpeg, DctExtractOptions::quantized_only()).unwrap();

    // 48 pixels starting 32 into the tile: MCU columns 2..5 of 8.
    let window = extract_ndpi_retile_window(&jpeg, (160, 48), 32, 48).unwrap();
    assert_eq!(window.components.len(), 3);
    for (source, cut) in full.components.iter().zip(&window.components) {
        let h = usize::from(source.h_samp);
        assert_eq!(
            (cut.block_cols, cut.block_rows),
            (3 * h as u32, source.block_rows)
        );
        assert!(cut.dequantized_blocks.is_empty());
        let stride = source.block_cols as usize;
        let expected = source
            .quantized_blocks
            .chunks(stride)
            .flat_map(|row| row[2 * h..5 * h].to_vec())
            .collect::<Vec<_>>();
        assert_eq!(cut.quantized_blocks, expected);
    }

    // Frame origins and crop starts off the 16-pixel MCU grid are refused,
    // and so is a crop past the tile, before any coefficient is copied.
    for (origin, crop_start_x, crop_width, expected) in [
        ((168, 48), 32, 48, "MCU-aligned frame origins (mcu 16x16)"),
        ((160, 40), 32, 48, "MCU-aligned frame origins (mcu 16x16)"),
        ((160, 48), 24, 48, "MCU-aligned frame origins (mcu 16x16)"),
        ((160, 48), 96, 48, "exceeds its source segment"),
    ] {
        let err = extract_ndpi_retile_window(&jpeg, origin, crop_start_x, crop_width).unwrap_err();
        assert!(
            matches!(&err, WsiError::Unsupported { reason } if reason.contains(expected)),
            "{err:?}"
        );
    }
    assert!(matches!(
        extract_ndpi_retile_window(&jpeg[..jpeg.len() / 2], (0, 0), 0, 16),
        Err(WsiError::Jpeg(_))
    ));
}

#[test]
fn ndpi_raw_compressed_display_tile_retiles_frames_that_start_inside_a_native_tile() {
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
    // 96-pixel frames: the first takes native tile 0 and the left half of
    // tile 1, the second starts 32 pixels into tile 1.
    for (col, width) in [(0, 96), (1, 32)] {
        let request = TileViewRequest::new(0usize, 0usize, 0u32, col, 0, 96, 16);
        let raw = reader.read_raw_compressed_display_tile(&request).unwrap();
        assert_eq!((raw.width(), raw.height()), (width, 16));
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
}

/// A textured `width` x `height` JPEG, as one native NDPI tile.
fn retile_source(
    width: u16,
    height: u16,
    quality: u8,
    color: JpegColorType,
    progressive: bool,
) -> Vec<u8> {
    let channels = if color == JpegColorType::Luma { 1 } else { 3 };
    let pixels = (0..u32::from(width) * u32::from(height) * channels)
        .map(|i| (i * 13 + i / 7) as u8)
        .collect::<Vec<_>>();
    let mut jpeg = Vec::new();
    let mut encoder = JpegEncoder::new(&mut jpeg, quality);
    encoder.set_sampling_factor(JpegSamplingFactor::F_1_1);
    encoder.set_progressive(progressive);
    encoder.encode(&pixels, width, height, color).unwrap();
    jpeg
}

#[test]
fn ndpi_retile_assembly_refuses_segments_that_do_not_form_the_frame() {
    let request = TileViewRequest::new(0usize, 0usize, 0u32, 0, 0, 64, 8);
    let rgb =
        |width, height, quality| retile_source(width, height, quality, JpegColorType::Rgb, false);
    let segment = |jpeg: &[u8], native_row, crop_width| NdpiDctRetileSegment {
        native_row,
        image: extract_ndpi_retile_window(jpeg, (0, 0), 0, crop_width).unwrap(),
    };
    let tile = rgb(64, 8, 90);
    // One native tile wide and tall, unless stated otherwise.
    let build = |segments: &[NdpiDctRetileSegment], native_col_end, native_row_end| {
        build_ndpi_retiled_dct_image(
            &request,
            64,
            8,
            0,
            native_col_end,
            0,
            native_row_end,
            segments,
        )
    };

    let whole = build(&[segment(&tile, 0, 64)], 0, 0).unwrap();
    assert_eq!((whole.width, whole.height), (64, 8));
    assert_eq!(whole.components[0].quantized_blocks.len(), 8);

    let mut short = segment(&tile, 0, 64);
    short.image.components[1].quantized_blocks.truncate(3);
    let cases: [(Vec<NdpiDctRetileSegment>, u32, u32, &str); 9] = [
        (Vec::new(), 0, 0, "found no source segments"),
        (
            vec![segment(&tile, 0, 64)],
            1,
            0,
            "row 0 has 1 source segments, expected 2",
        ),
        (
            vec![segment(&tile, 0, 64)],
            0,
            1,
            "row 1 has 0 source segments, expected 1",
        ),
        (
            vec![segment(&rgb(64, 16, 90), 0, 64)],
            0,
            0,
            "expects one MCU row per source segment, got 2 block rows for component 0",
        ),
        (
            vec![short],
            0,
            0,
            "source segment holds fewer blocks than component 1's grid",
        ),
        (
            vec![segment(&tile, 0, 32)],
            0,
            0,
            "copied 4 blocks for component 0, expected 8",
        ),
        (
            vec![segment(&tile, 0, 32), segment(&rgb(64, 8, 40), 0, 32)],
            1,
            0,
            "source segment component 0 coding profile changed",
        ),
        (
            vec![
                segment(&tile, 0, 32),
                segment(&retile_source(64, 8, 90, JpegColorType::Luma, false), 0, 32),
            ],
            1,
            0,
            "source segment color profile changed",
        ),
        (
            vec![segment(
                &retile_source(64, 8, 90, JpegColorType::Rgb, true),
                0,
                64,
            )],
            0,
            0,
            "supports baseline sequential JPEG only",
        ),
    ];
    for (segments, native_col_end, native_row_end, expected) in cases {
        let error = match build(&segments, native_col_end, native_row_end) {
            Ok(_) => panic!("{expected}: the segments should be refused"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains(expected), "{expected}: {error}");
    }
    // A later segment of another coding mode is refused too.
    let mixed = [
        segment(&tile, 0, 32),
        segment(&retile_source(64, 8, 90, JpegColorType::Rgb, true), 0, 32),
    ];
    assert!(build(&mixed, 1, 0)
        .unwrap_err()
        .to_string()
        .contains("supports baseline sequential JPEG only"));
}
