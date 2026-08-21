#[cfg(any(feature = "metal", feature = "cuda"))]
use super::fixtures::*;
#[cfg(any(feature = "metal", feature = "cuda"))]
use super::runtime::*;
use super::*;
#[cfg(any(feature = "metal", feature = "cuda"))]
fn encode_test_htj2k_rgb(width: u32, height: u32) -> Vec<u8> {
    let mut pixels = Vec::with_capacity(width as usize * height as usize * 3);
    for index in 0..width * height {
        pixels.push(((index * 17 + index / 3) & 0xff) as u8);
        pixels.push(((index * 29 + 7) & 0xff) as u8);
        pixels.push(((index * 43 + 19) & 0xff) as u8);
    }
    let options = j2k_native::EncodeOptions {
        reversible: true,
        num_decomposition_levels: 1,
        ..j2k_native::EncodeOptions::default()
    };
    j2k_native::encode_htj2k(&pixels, width, height, 3, 8, false, &options)
        .expect("encode RGB HTJ2K fixture")
}

#[cfg(feature = "metal")]
fn test_metal_sessions() -> Option<crate::output::metal::MetalBackendSessions> {
    crate::output::metal::MetalBackendSessions::system_default().ok()
}

#[cfg(feature = "metal")]
type TestDeviceSessions = crate::output::metal::MetalBackendSessions;
#[cfg(all(not(feature = "metal"), feature = "cuda"))]
type TestDeviceSessions = crate::output::cuda::CudaBackendSessions;

#[cfg(feature = "metal")]
fn test_device_sessions() -> Option<TestDeviceSessions> {
    test_metal_sessions()
}

#[cfg(all(not(feature = "metal"), feature = "cuda"))]
fn test_device_sessions() -> Option<TestDeviceSessions> {
    if std::env::var_os("J2K_REQUIRE_CUDA_RUNTIME").is_none() {
        eprintln!("skipping CUDA DICOM device test; J2K_REQUIRE_CUDA_RUNTIME is unset");
        return None;
    }
    Some(crate::output::cuda::CudaBackendSessions::new())
}

#[cfg(feature = "metal")]
fn require_test_device(sessions: TestDeviceSessions) -> TileOutputPreference {
    TileOutputPreference::require_device_auto_with_metal_and_compressed_decode(sessions)
}

#[cfg(all(not(feature = "metal"), feature = "cuda"))]
fn require_test_device(sessions: TestDeviceSessions) -> TileOutputPreference {
    TileOutputPreference::require_device_auto_with_cuda_and_compressed_decode(sessions)
}

#[cfg(feature = "metal")]
fn prefer_test_device(sessions: TestDeviceSessions) -> TileOutputPreference {
    TileOutputPreference::prefer_device_auto_with_metal_and_compressed_decode(sessions)
}

#[cfg(all(not(feature = "metal"), feature = "cuda"))]
fn prefer_test_device(sessions: TestDeviceSessions) -> TileOutputPreference {
    TileOutputPreference::prefer_device_auto_with_cuda_and_compressed_decode(sessions)
}

#[cfg(feature = "metal")]
fn is_test_device_tile(tile: &TilePixels) -> bool {
    matches!(tile, TilePixels::Device(DeviceTile::Metal(_)))
}

#[cfg(all(not(feature = "metal"), feature = "cuda"))]
fn is_test_device_tile(tile: &TilePixels) -> bool {
    matches!(tile, TilePixels::Device(DeviceTile::Cuda(_)))
}

#[test]
#[cfg(any(feature = "metal", feature = "cuda"))]
fn require_device_rejects_sparse_missing_dicom_tile_cpu_black_fallback() {
    let Some(sessions) = test_device_sessions() else {
        return;
    };
    let mut present_tiles = HashMap::new();
    present_tiles.insert((0, 0), 0);
    let levels = build_levels(
        Path::new("sparse-device.dcm"),
        vec![test_dicom_image_with_transfer_syntax(
            "1.2.3.1",
            DicomGrid::Sparse(present_tiles),
            uids::JPEG2000_LOSSLESS,
        )],
    )
    .expect("sparse level should build");
    let reader = DicomReader {
        slide: Arc::new(DicomSlide {
            dataset: empty_dataset(),
            levels,
            associated: HashMap::new(),
        }),
    };

    let err = reader
        .read_tiles(&[tile_request(1, 0)], require_test_device(sessions))
        .expect_err("RequireDevice must not return CPU black sparse tile");

    assert!(matches!(err, WsiError::Unsupported { .. }));
}

#[test]
#[cfg(any(feature = "metal", feature = "cuda"))]
fn local_htj2k_dicom_full_tile_can_require_device_output() {
    let Some(path) = local_htj2k_dicom_fixture() else {
        return;
    };
    let Some(sessions) = test_device_sessions() else {
        return;
    };

    let slide = Slide::open(&path).expect("open local HTJ2K DICOM slide");
    let tile = slide
        .read_tile_controlled(
            &TileRequest {
                scene: 0usize.into(),
                series: 0usize.into(),
                level: 0u32.into(),
                plane: PlaneSelection::default().into(),
                col: 0,
                row: 0,
            },
            require_test_device(sessions),
            &crate::ReadControl::default(),
        )
        .expect("read full HTJ2K tile with required device output");

    assert!(matches!(tile, TilePixels::Device(_)));
}

#[test]
#[cfg(any(feature = "metal", feature = "cuda"))]
fn controlled_classic_jp2k_and_htj2k_keep_device_output() {
    let Some(sessions) = test_device_sessions() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let classic = include_bytes!("../../../../tests/fixtures/jp2k/rgb_nomct.j2k").to_vec();
    let htj2k = encode_test_htj2k_rgb(16, 12);

    for (name, transfer_syntax, codestream) in [
        ("classic", uids::JPEG2000_LOSSLESS, classic),
        ("htj2k", HTJ2K_LOSSLESS_TRANSFER_SYNTAX, htj2k),
    ] {
        let path = dir.path().join(format!("controlled-{name}.dcm"));
        let mut options = TestDicomOptions::native(Vec::new());
        options.transfer_syntax = transfer_syntax;
        options.rows = 12;
        options.columns = 16;
        options.total_pixel_matrix_rows = 12;
        options.total_pixel_matrix_columns = 16;
        options.pixel_data = TestPixelData::EncapsulatedFrames(vec![codestream]);
        write_test_dicom(&path, options);
        let slide = Slide::open(&path).expect("open generated JP2K DICOM");

        let tile = slide
            .read_tile_controlled(
                &tile_request(0, 0),
                require_test_device(sessions.clone()),
                &crate::ReadControl::default(),
            )
            .unwrap_or_else(|error| panic!("controlled {name} device decode failed: {error}"));

        assert!(
            is_test_device_tile(&tile),
            "controlled {name} decode must remain device-resident"
        );
    }
}

#[test]
#[cfg(any(feature = "metal", feature = "cuda"))]
fn local_htj2k_dicom_prefer_device_batch_keeps_full_tiles_on_device() {
    let Some(path) = local_htj2k_dicom_fixture() else {
        return;
    };
    let Some(sessions) = test_device_sessions() else {
        return;
    };

    let slide = Slide::open(&path).expect("open local HTJ2K DICOM slide");
    let tiles = slide
        .read_tiles_controlled(
            &[
                TileRequest {
                    scene: 0usize.into(),
                    series: 0usize.into(),
                    level: 0u32.into(),
                    plane: PlaneSelection::default().into(),
                    col: 0,
                    row: 0,
                },
                TileRequest {
                    scene: 0usize.into(),
                    series: 0usize.into(),
                    level: 0u32.into(),
                    plane: PlaneSelection::default().into(),
                    col: 1,
                    row: 0,
                },
            ],
            prefer_test_device(sessions).without_adaptive_decode_route(),
            &crate::ReadControl::default(),
        )
        .expect("read full HTJ2K tile batch with residency-preferred device output");

    assert!(
        tiles
            .iter()
            .any(|tile| matches!(tile, TilePixels::Device(_))),
        "prefer-device HTJ2K batch should return device tiles when full tiles are decodable"
    );
}

#[test]
#[cfg(feature = "parity-metal")]
fn local_htj2k_dicom_full_tile_pixels_match_cpu_on_metal() {
    let Some(path) = local_htj2k_dicom_fixture() else {
        return;
    };
    let Some(sessions) = test_metal_sessions() else {
        eprintln!("skipping local HTJ2K DICOM parity test; no Metal device");
        return;
    };

    let slide = Slide::open(&path).expect("open local HTJ2K DICOM slide");
    let level = &slide.dataset().scenes[0].series[0].levels[0];
    let TileLayout::Regular {
        tile_width,
        tile_height,
        ..
    } = level.tile_layout
    else {
        panic!("local HTJ2K DICOM fixture must use a regular tile grid");
    };
    assert!(level.dimensions.0 >= u64::from(tile_width));
    assert!(level.dimensions.1 >= u64::from(tile_height));
    let requests = [TileRequest {
        scene: 0usize.into(),
        series: 0usize.into(),
        level: 0u32.into(),
        plane: PlaneSelection::default().into(),
        col: 0,
        row: 0,
    }];
    let cpu = slide
        .read_tiles_controlled(
            &requests,
            TileOutputPreference::cpu(),
            &crate::ReadControl::default(),
        )
        .expect("read CPU parity tiles");
    let device = slide
        .read_tiles_controlled(
            &requests,
            TileOutputPreference::require_device_auto_with_metal_and_compressed_decode(sessions)
                .without_adaptive_decode_route(),
            &crate::ReadControl::default(),
        )
        .expect("read Metal parity tiles");

    for (index, (cpu, device)) in cpu.into_iter().zip(device).enumerate() {
        let TilePixels::Cpu(cpu) = cpu else {
            panic!("CPU parity request {index} returned device pixels");
        };
        let TilePixels::Device(DeviceTile::Metal(device)) = device else {
            panic!("Metal parity request {index} returned CPU pixels");
        };
        let resident = device
            .validated_resident_image()
            .expect("validated resident Metal tile");
        let metal = crate::output::metal::resident_bytes(resident);
        let cpu = cpu.data.as_u8().expect("CPU parity tile is RGB8");
        assert_eq!(metal.len(), cpu.len(), "tile {index} byte cardinality");
        let max_delta = metal
            .iter()
            .zip(cpu)
            .map(|(metal, cpu)| metal.abs_diff(*cpu))
            .max()
            .unwrap_or(0);
        assert!(max_delta <= 4, "tile {index} max channel delta {max_delta}");
    }
}

#[test]
fn local_htj2k_dicom_level_preparation_meets_interactive_budget() {
    let Some(path) = local_htj2k_dicom_fixture() else {
        return;
    };
    let slide = Slide::open(&path).expect("open local HTJ2K DICOM slide");
    let started = std::time::Instant::now();
    slide
        .prepare_level_controlled(
            SceneId::new(0),
            SeriesId::new(0),
            LevelIdx::new(0),
            &crate::ReadControl::default(),
        )
        .expect("prepare local HTJ2K DICOM base level");
    let elapsed = started.elapsed();
    eprintln!("local HTJ2K DICOM level preparation: {elapsed:?}");
    assert!(
        elapsed < std::time::Duration::from_millis(75),
        "DICOM level preparation should remain inside the 75 ms interactive budget"
    );
}

#[test]
#[cfg(any(feature = "metal", feature = "cuda"))]
fn dicom_jpeg_require_device_batch_uses_jpeg_device_route() {
    let Some(sessions) = test_device_sessions() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jpeg-batch.dcm");
    let mut options = TestDicomOptions::native(Vec::new());
    options.transfer_syntax = JPEG_TRANSFER_SYNTAX;
    options.rows = 16;
    options.columns = 16;
    options.total_pixel_matrix_rows = 16;
    options.total_pixel_matrix_columns = 32;
    options.number_of_frames = 2;
    options.pixel_data = TestPixelData::EncapsulatedFrames(vec![
        encode_test_jpeg_rgb(16, 16, 3),
        encode_test_jpeg_rgb(16, 16, 41),
    ]);
    write_test_dicom(&path, options);

    let slide = Slide::open(&path).expect("open generated DICOM JPEG slide");
    let tiles = slide
        .read_tiles(
            &[tile_request(0, 0), tile_request(1, 0)],
            require_test_device(sessions).without_adaptive_decode_route(),
        )
        .expect("DICOM JPEG full-tile batch should support required device output");

    assert_eq!(tiles.len(), 2);
    assert!(
        tiles
            .iter()
            .all(|tile| matches!(tile, TilePixels::Device(_))),
        "DICOM JPEG batch should keep all full tiles on device"
    );
}

fn local_htj2k_dicom_fixture() -> Option<PathBuf> {
    let Some(path) = std::env::var_os("WSI_RS_LOCAL_HTJ2K_DICOM").map(PathBuf::from) else {
        eprintln!("skipping local HTJ2K DICOM device test; WSI_RS_LOCAL_HTJ2K_DICOM unset");
        return None;
    };
    if !path.is_file() {
        eprintln!(
            "skipping local HTJ2K DICOM device test; missing {}",
            path.display()
        );
        return None;
    }
    Some(path)
}

#[test]
#[cfg(any(feature = "metal", feature = "cuda"))]
fn dicom_jp2k_device_batch_policy_is_selective() {
    let prefer_device = TileOutputPreference::prefer_device_auto_with_compressed_decode();
    let explicit_device = TileOutputPreference::prefer_device_auto_with_compressed_decode()
        .without_adaptive_decode_route();
    let require_device = TileOutputPreference::require_device_auto_with_compressed_decode();

    assert!(dicom_jp2k_device_batch_allowed_for_output(
        HTJ2K_LOSSLESS_TRANSFER_SYNTAX,
        &prefer_device,
        false,
        1,
    ));
    assert!(!dicom_jp2k_device_batch_allowed_for_output(
        uids::JPEG2000_LOSSLESS,
        &prefer_device,
        false,
        4,
    ));
    assert!(dicom_jp2k_device_batch_allowed_for_output(
        uids::JPEG2000_LOSSLESS,
        &prefer_device,
        false,
        8,
    ));
    assert!(dicom_jp2k_device_batch_allowed_for_output(
        uids::JPEG2000_LOSSLESS,
        &explicit_device,
        false,
        1,
    ));
    assert!(dicom_jp2k_device_batch_allowed_for_output(
        uids::JPEG2000_LOSSLESS,
        &require_device,
        false,
        1,
    ));
    assert!(dicom_jp2k_device_batch_allowed_for_output(
        uids::JPEG2000_LOSSLESS,
        &prefer_device,
        true,
        1,
    ));
}

#[test]
#[cfg(any(feature = "metal", feature = "cuda"))]
fn mixed_device_batch_admits_one_ordered_cpu_remainder_batch() {
    fn marker_tile(value: u8) -> CpuTile {
        CpuTile::from_u8_interleaved(1, 1, 3, ColorSpace::Rgb, vec![value, 0, 0]).unwrap()
    }

    let requests = [0, 1, 2, 3]
        .into_iter()
        .map(|col| tile_request(col, 0))
        .collect::<Vec<_>>();
    let results = vec![
        Some(TilePixels::Cpu(marker_tile(10))),
        None,
        Some(TilePixels::Cpu(marker_tile(30))),
        None,
    ];
    let codec_admissions = std::cell::RefCell::new(Vec::new());

    let completed = complete_mixed_device_batch_with_cpu_remainder(
        &requests,
        &TileOutputPreference::prefer_device_auto_with_compressed_decode(),
        BackendRequest::Auto,
        results,
        None,
        |remainder, _, _| {
            codec_admissions.borrow_mut().push(
                remainder
                    .iter()
                    .map(|request| request.col)
                    .collect::<Vec<_>>(),
            );
            Ok(vec![marker_tile(20), marker_tile(40)])
        },
    )
    .expect("complete mixed device/CPU batch");

    assert_eq!(*codec_admissions.borrow(), vec![vec![1, 3]]);
    assert_eq!(completed.len(), requests.len());
    assert_eq!(
        completed
            .iter()
            .map(|tile| match tile {
                TilePixels::Cpu(tile) => tile.data.as_u8().unwrap()[0],
                TilePixels::Device(_) => panic!("synthetic completion uses CPU marker tiles"),
            })
            .collect::<Vec<_>>(),
        vec![10, 20, 30, 40],
        "CPU remainder results must return to their original request slots"
    );
}

#[test]
#[cfg(any(feature = "metal", feature = "cuda"))]
fn cancelled_mixed_device_batch_never_admits_a_cpu_remainder() {
    let token = crate::ReadCancellationToken::new();
    token.cancel();
    let control = crate::ReadControl::new(token);
    let admissions = std::cell::Cell::new(0_usize);

    let error = complete_mixed_device_batch_with_cpu_remainder(
        &[tile_request(0, 0)],
        &TileOutputPreference::prefer_device_auto_with_compressed_decode(),
        BackendRequest::Auto,
        vec![None],
        Some(&control),
        |_, _, _| {
            admissions.set(admissions.get() + 1);
            Ok(Vec::new())
        },
    )
    .expect_err("cancelled mixed batch must not enter CPU fallback");

    assert!(matches!(error, WsiError::Cancelled));
    assert_eq!(admissions.get(), 0);
}
