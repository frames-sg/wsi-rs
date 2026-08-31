use super::*;

#[cfg(feature = "metal")]
type TestDeviceSessions = crate::output::metal::MetalBackendSessions;
#[cfg(all(not(feature = "metal"), feature = "cuda"))]
type TestDeviceSessions = crate::output::cuda::CudaBackendSessions;

#[cfg(feature = "metal")]
type TestDeviceTile = crate::output::metal::MetalDeviceTile;
#[cfg(all(not(feature = "metal"), feature = "cuda"))]
type TestDeviceTile = crate::output::cuda::CudaDeviceTile;

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
fn read_device(
    reader: &TiffPixelReader,
    requests: &[TileRequest],
    sessions: &TestDeviceSessions,
) -> Result<Vec<TestDeviceTile>, WsiError> {
    reader.read_tiles_metal(requests, sessions)
}

#[cfg(all(not(feature = "metal"), feature = "cuda"))]
fn read_device(
    reader: &TiffPixelReader,
    requests: &[TileRequest],
    sessions: &TestDeviceSessions,
) -> Result<Vec<TestDeviceTile>, WsiError> {
    reader.read_tiles_cuda(requests, sessions)
}

fn assert_resident_tiles(tiles: &[TestDeviceTile], dimensions: (u32, u32)) {
    assert!(!tiles.is_empty());
    for tile in tiles {
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

    let decoded = read_device(
        &reader,
        &[tile_request(0, 0), tile_request(1, 0)],
        &sessions,
    )
    .expect("strict tiled JP2K device batch");

    assert_resident_tiles(&decoded, (width, height));
}

#[test]
fn tiled_jp2k_device_download_matches_cpu() {
    let Some(sessions) = test_sessions() else {
        return;
    };
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

    let cpu = reader.read_tiles_cpu(&request).expect("CPU JP2K tile");
    let device = read_device(&reader, &request, &sessions).expect("device JP2K tile");
    let downloaded = device[0].download_cpu().expect("download JP2K tile");

    assert_eq!(
        (
            downloaded.width(),
            downloaded.height(),
            downloaded.channels()
        ),
        (cpu[0].width(), cpu[0].height(), cpu[0].channels())
    );
    assert_eq!(downloaded.data.as_u8(), cpu[0].data.as_u8());
}

#[test]
fn strict_tiled_device_read_rejects_jpeg() {
    let Some(sessions) = test_sessions() else {
        return;
    };
    let tiles = [encode_solid_rgb_jpeg(8, 8, [210, 20, 30])];
    let reader = build_tiled_jpeg_reader(8, 8, 8, 8, &tiles);

    let error = read_device(&reader, &[tile_request(0, 0)], &sessions)
        .expect_err("strict device output supports JP2K/HTJ2K only");

    assert!(matches!(error, WsiError::TileRead { .. }));
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

    let error = read_device(&reader, &[tile_request(0, 0)], &sessions)
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
fn empty_strict_device_batch_preserves_cardinality() {
    let Some(sessions) = test_sessions() else {
        return;
    };
    let reader = build_tiled_encoded_reader(
        8,
        8,
        8,
        8,
        &[b"unused".to_vec()],
        Compression::Jp2kRgb,
        33004,
        3,
        2,
    );

    let decoded = read_device(&reader, &[], &sessions).expect("empty strict device batch");

    assert!(decoded.is_empty());
}
