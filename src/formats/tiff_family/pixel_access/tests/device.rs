use super::*;

#[cfg(feature = "metal")]
type TestDeviceSessions = crate::output::metal::MetalBackendSessions;
#[cfg(all(not(feature = "metal"), feature = "cuda"))]
type TestDeviceSessions = crate::output::cuda::CudaBackendSessions;

#[cfg(feature = "metal")]
fn test_sessions() -> Option<TestDeviceSessions> {
    crate::output::metal::MetalBackendSessions::system_default().ok()
}

#[cfg(all(not(feature = "metal"), feature = "cuda"))]
fn test_sessions() -> Option<TestDeviceSessions> {
    if std::env::var_os("J2K_REQUIRE_CUDA_RUNTIME").is_none() {
        eprintln!("skipping CUDA TIFF device test; J2K_REQUIRE_CUDA_RUNTIME is unset");
        return None;
    }
    Some(crate::output::cuda::CudaBackendSessions::new())
}

fn tile_request(col: i64, row: i64) -> TileRequest {
    TileRequest {
        scene: 0usize.into(),
        series: 0usize.into(),
        level: 0u32.into(),
        plane: PlaneSelection::default().into(),
        col,
        row,
    }
}

#[cfg(feature = "metal")]
fn require_test_device(sessions: TestDeviceSessions) -> TileOutputPreference {
    TileOutputPreference::require_device_auto_with_metal_and_compressed_decode(sessions)
        .without_adaptive_decode_route()
}

#[cfg(all(not(feature = "metal"), feature = "cuda"))]
fn require_test_device(sessions: TestDeviceSessions) -> TileOutputPreference {
    TileOutputPreference::require_device_auto_with_cuda_and_compressed_decode(sessions)
        .without_adaptive_decode_route()
}

#[cfg(feature = "metal")]
fn test_backend() -> BackendRequest {
    BackendRequest::Metal
}

#[cfg(all(not(feature = "metal"), feature = "cuda"))]
fn test_backend() -> BackendRequest {
    BackendRequest::Cuda
}

fn assert_device_tiles(tiles: &[TilePixels], dimensions: (u32, u32)) {
    assert!(!tiles.is_empty());
    for tile in tiles {
        #[cfg(feature = "metal")]
        let TilePixels::Device(DeviceTile::Metal(tile)) = tile
        else {
            panic!("required Metal TIFF decode returned CPU pixels");
        };
        #[cfg(all(not(feature = "metal"), feature = "cuda"))]
        let TilePixels::Device(DeviceTile::Cuda(tile)) = tile
        else {
            panic!("required CUDA TIFF decode returned CPU pixels");
        };
        assert_eq!((tile.width, tile.height), dimensions);
        assert_eq!(tile.format, PixelFormat::Rgb8);
        #[cfg(feature = "metal")]
        tile.validated_resident_image()
            .expect("validated resident TIFF tile");
        #[cfg(all(not(feature = "metal"), feature = "cuda"))]
        assert_ne!(tile.storage.device_ptr(), 0, "resident CUDA TIFF tile");
    }
}

#[test]
fn tiled_jpeg_batch_decodes_to_resident_device_tiles() {
    let Some(sessions) = test_sessions() else {
        return;
    };
    let tiles = [
        encode_solid_rgb_jpeg(8, 8, [210, 20, 30]),
        encode_solid_rgb_jpeg(8, 8, [30, 190, 40]),
    ];
    let reader = build_tiled_jpeg_reader(16, 8, 8, 8, &tiles);

    let decoded = reader
        .read_tiles(
            &[tile_request(0, 0), tile_request(1, 0)],
            require_test_device(sessions),
        )
        .expect("tiled JPEG device batch");

    assert_device_tiles(&decoded, (8, 8));
}

#[test]
fn tiled_jp2k_batch_decodes_to_resident_device_tiles() {
    let Some(sessions) = test_sessions() else {
        return;
    };
    let codestream = include_bytes!("../../../../../tests/fixtures/jp2k/rgb_nomct.j2k").to_vec();
    let expected = load_fixture_rgb(include_bytes!(
        "../../../../../tests/fixtures/jp2k/rgb_nomct.ppm"
    ));
    let width = expected.width();
    let height = expected.height();
    let reader = build_tiled_encoded_reader(
        width * 2,
        height,
        width,
        height,
        &[codestream.clone(), codestream],
        Compression::Jp2kRgb,
        33004,
        3,
        2,
    );

    let decoded = reader
        .read_tiles(
            &[tile_request(0, 0), tile_request(1, 0)],
            require_test_device(sessions),
        )
        .expect("tiled JP2K device batch");

    assert_device_tiles(&decoded, (width, height));
}

#[test]
fn invalid_tiled_jpeg_device_batch_reports_requested_tile() {
    let Some(sessions) = test_sessions() else {
        return;
    };
    let reader = build_tiled_jpeg_reader(8, 8, 8, 8, &[b"not a jpeg".to_vec()]);

    let error = reader
        .read_tiles(&[tile_request(0, 0)], require_test_device(sessions))
        .expect_err("invalid JPEG device batch must fail");

    assert!(matches!(
        error,
        WsiError::TileRead {
            col: 0,
            row: 0,
            level: 0,
            ..
        }
    ));
}

#[test]
fn invalid_tiled_jp2k_device_batch_reports_requested_tile() {
    let Some(sessions) = test_sessions() else {
        return;
    };
    let reader = build_tiled_encoded_reader(
        8,
        8,
        8,
        8,
        &[b"not jp2k".to_vec()],
        Compression::Jp2kRgb,
        33004,
        3,
        2,
    );

    let error = reader
        .read_tiles(&[tile_request(0, 0)], require_test_device(sessions))
        .expect_err("invalid JP2K device batch must fail");

    assert!(matches!(
        error,
        WsiError::TileRead {
            col: 0,
            row: 0,
            level: 0,
            ..
        }
    ));
}

#[test]
fn empty_tiled_jpeg_cannot_satisfy_required_device_output() {
    let Some(sessions) = test_sessions() else {
        return;
    };
    let reader = build_tiled_jpeg_reader(8, 8, 8, 8, &[Vec::new()]);

    let error = reader
        .read_tiles(&[tile_request(0, 0)], require_test_device(sessions))
        .expect_err("empty JPEG tile has no device payload");

    assert!(matches!(error, WsiError::Unsupported { .. }));
    assert!(error.to_string().contains("empty jpeg tile"));
}

#[test]
fn device_batch_classification_rejects_empty_and_non_matching_sources() {
    let reader = build_tiled_jpeg_reader(8, 8, 8, 8, &[encode_solid_rgb_jpeg(8, 8, [20, 30, 40])]);

    assert!(!reader
        .ndpi_jpeg_batchable(&[])
        .expect("empty NDPI classification"));
    assert!(!reader
        .ndpi_jpeg_batchable(&[tile_request(0, 0)])
        .expect("tiled JPEG is not NDPI"));
}

#[test]
fn tiled_device_helpers_reject_codec_mismatches_before_decode() {
    let codestream = include_bytes!("../../../../../tests/fixtures/jp2k/rgb_nomct.j2k").to_vec();
    let expected = load_fixture_rgb(include_bytes!(
        "../../../../../tests/fixtures/jp2k/rgb_nomct.ppm"
    ));
    let reader = build_tiled_encoded_reader(
        expected.width(),
        expected.height(),
        expected.width(),
        expected.height(),
        &[codestream],
        Compression::Jp2kRgb,
        33004,
        3,
        2,
    );
    let request = [tile_request(0, 0)];

    let jpeg_error = reader
        .collect_tiled_ifd_jpeg_jobs(&request)
        .expect_err("JP2K source is not a JPEG device job");
    assert!(matches!(jpeg_error, WsiError::TileRead { .. }));

    let jp2k_error = reader
        .decode_tiled_ifd_jp2k_pixels(
            &request,
            Compression::Jp2kYcbcr,
            test_backend(),
            true,
            None,
            None,
        )
        .expect_err("mixed JP2K compression must fail before decode");
    assert!(matches!(jp2k_error, WsiError::TileRead { .. }));
}
