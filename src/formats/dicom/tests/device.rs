use super::fixtures::*;
use super::runtime::{empty_dataset, test_dicom_image_with_transfer_syntax, tile_request};
use super::*;

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
type TestDeviceSessions = crate::output::metal::MetalBackendSessions;
#[cfg(all(not(feature = "metal"), feature = "cuda"))]
type TestDeviceSessions = crate::output::cuda::CudaBackendSessions;

#[cfg(feature = "metal")]
type TestDeviceTile = crate::output::metal::MetalDeviceTile;
#[cfg(all(not(feature = "metal"), feature = "cuda"))]
type TestDeviceTile = crate::output::cuda::CudaDeviceTile;

#[cfg(feature = "metal")]
fn test_device_sessions() -> Option<TestDeviceSessions> {
    crate::output::metal::MetalBackendSessions::system_default().ok()
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
fn read_reader_device(
    reader: &DicomReader,
    requests: &[TileRequest],
    sessions: &TestDeviceSessions,
) -> Result<Vec<TestDeviceTile>, WsiError> {
    reader.read_tiles_metal(requests, sessions)
}

#[cfg(all(not(feature = "metal"), feature = "cuda"))]
fn read_reader_device(
    reader: &DicomReader,
    requests: &[TileRequest],
    sessions: &TestDeviceSessions,
) -> Result<Vec<TestDeviceTile>, WsiError> {
    reader.read_tiles_cuda(requests, sessions)
}

#[cfg(feature = "metal")]
fn read_slide_device(
    slide: &Slide,
    requests: &[TileRequest],
    sessions: &TestDeviceSessions,
) -> Result<Vec<TestDeviceTile>, WsiError> {
    slide.read_tiles_metal(requests, sessions)
}

#[cfg(all(not(feature = "metal"), feature = "cuda"))]
fn read_slide_device(
    slide: &Slide,
    requests: &[TileRequest],
    sessions: &TestDeviceSessions,
) -> Result<Vec<TestDeviceTile>, WsiError> {
    slide.read_tiles_cuda(requests, sessions)
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
fn strict_device_rejects_sparse_missing_dicom_tile() {
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
            encoded_unit_bytes: crate::SlideLimits::default().encoded_unit_bytes(),
            dataset: empty_dataset(),
            levels,
            associated: HashMap::new(),
        }),
    };

    let error = read_reader_device(&reader, &[tile_request(1, 0)], &sessions)
        .expect_err("strict device reads must not synthesize a CPU black tile");

    assert!(matches!(error, WsiError::Unsupported { .. }));
}

#[test]
fn classic_jp2k_and_htj2k_decode_to_resident_tiles() {
    let Some(sessions) = test_device_sessions() else {
        return;
    };
    let directory = tempfile::tempdir().unwrap();
    let classic = include_bytes!("../../../../tests/fixtures/jp2k/rgb_nomct.j2k").to_vec();
    let htj2k = encode_test_htj2k_rgb(16, 12);

    for (name, transfer_syntax, codestream) in [
        ("classic", uids::JPEG2000_LOSSLESS, classic),
        ("htj2k", HTJ2K_LOSSLESS_TRANSFER_SYNTAX, htj2k),
    ] {
        let path = directory.path().join(format!("strict-{name}.dcm"));
        let mut options = TestDicomOptions::native(Vec::new());
        options.transfer_syntax = transfer_syntax;
        options.rows = 12;
        options.columns = 16;
        options.total_pixel_matrix_rows = 12;
        options.total_pixel_matrix_columns = 16;
        options.pixel_data = TestPixelData::EncapsulatedFrames(vec![codestream]);
        write_test_dicom(&path, options);
        let slide = Slide::open(&path).expect("open generated JP2K DICOM");

        let tiles = read_slide_device(&slide, &[tile_request(0, 0)], &sessions)
            .unwrap_or_else(|error| panic!("strict {name} device decode failed: {error}"));

        assert_eq!(tiles.len(), 1);
        assert_eq!((tiles[0].width, tiles[0].height), (16, 12));
        assert_eq!(tiles[0].format, PixelFormat::Rgb8);
        let downloaded = tiles[0].download_cpu().expect("download device tile");
        assert_eq!((downloaded.width(), downloaded.height()), (16, 12));
    }
}

#[test]
fn strict_device_rejects_dicom_jpeg() {
    let Some(sessions) = test_device_sessions() else {
        return;
    };
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("jpeg-device-rejection.dcm");
    let mut options = TestDicomOptions::native(Vec::new());
    options.transfer_syntax = JPEG_TRANSFER_SYNTAX;
    options.rows = 16;
    options.columns = 16;
    options.total_pixel_matrix_rows = 16;
    options.total_pixel_matrix_columns = 16;
    options.pixel_data = TestPixelData::EncapsulatedFrames(vec![encode_test_jpeg_rgb(16, 16, 3)]);
    write_test_dicom(&path, options);
    let slide = Slide::open(&path).expect("open generated JPEG DICOM");

    let error = read_slide_device(&slide, &[tile_request(0, 0)], &sessions)
        .expect_err("strict device reads support JP2K/HTJ2K only");

    assert!(matches!(error, WsiError::Unsupported { .. }));
}

#[test]
fn local_htj2k_device_pixels_match_cpu() {
    let Some(path) = local_htj2k_dicom_fixture() else {
        return;
    };
    let Some(sessions) = test_device_sessions() else {
        return;
    };
    let slide = Slide::open(&path).expect("open local HTJ2K DICOM slide");
    let requests = [tile_request(0, 0)];
    let cpu = slide.read_tiles(&requests).expect("read CPU parity tile");
    let device = read_slide_device(&slide, &requests, &sessions).expect("read device parity tile");
    let downloaded = device[0]
        .download_cpu()
        .expect("download device parity tile");
    let cpu_bytes = cpu[0].data.as_u8().expect("CPU parity tile is RGB8");
    let device_bytes = downloaded
        .data
        .as_u8()
        .expect("downloaded parity tile is RGB8");

    assert_eq!(device_bytes.len(), cpu_bytes.len());
    let max_delta = device_bytes
        .iter()
        .zip(cpu_bytes)
        .map(|(device, cpu)| device.abs_diff(*cpu))
        .max()
        .unwrap_or(0);
    assert!(max_delta <= 4, "max channel delta {max_delta}");
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
    assert!(
        elapsed < std::time::Duration::from_millis(75),
        "DICOM level preparation should remain inside the 75 ms interactive budget: {elapsed:?}"
    );
}

#[cfg(feature = "metal")]
#[test]
fn concurrent_admission_charges_actual_dicom_frame_copies() {
    use crate::core::execution_telemetry::{test_count, Event};
    use crate::core::limits::{ReadExecutionContext, SlideAdmission};
    use crate::core::registry::ManagedSlideReader;
    let Some(sessions) = test_device_sessions() else {
        return;
    };
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("bounded-native.dcm");
    let mut options = TestDicomOptions::native(Vec::new());
    options.transfer_syntax = uids::JPEG2000_LOSSLESS;
    options.rows = 12;
    options.columns = 16;
    options.total_pixel_matrix_rows = 12;
    options.total_pixel_matrix_columns = 16;
    options.pixel_data = TestPixelData::EncapsulatedFrames(vec![include_bytes!(
        "../../../../tests/fixtures/jp2k/rgb_nomct.j2k"
    )
    .to_vec()]);
    write_test_dicom(&path, options);
    let reader = DicomReader {
        slide: Arc::new(DicomSlide::parse(&path).unwrap()),
    };
    let reqs = [tile_request(0, 0), tile_request(0, 0)];
    let expected = reader.read_tiles_cpu(&reqs).unwrap();
    let reader = crate::core::decode_runtime::AdaptiveDecodeReader::new_managed(
        Box::new(reader),
        crate::core::decode_runtime::DecodeRuntime::default_arc(),
    );
    let admission = SlideAdmission::new(384 * 1024 * 1024);
    let _other_caller = admission.reserve(128 * 1024 * 1024, None).unwrap();
    let ordinary = admission
        .reserve(128 * 1024 * 1024 + 2 * 2 * 16 * 12 * 4, None)
        .unwrap();
    let calibration = std::sync::atomic::AtomicBool::new(false);
    let context = ReadExecutionContext::new(&ordinary, 384 * 1024 * 1024, None, &calibration);
    let before = test_count(Event::MetalBatchGroups);
    let actual = reader
        .read_metal_with_context(&reqs, &sessions, &context)
        .unwrap();
    assert_eq!(
        test_count(Event::MetalBatchGroups) - before,
        1,
        "small frame copies fit beside the other caller's reservation"
    );
    for (actual, expected) in actual.iter().zip(&expected) {
        assert_eq!(actual.download_cpu().unwrap().as_u8(), expected.as_u8());
    }
}

#[cfg(feature = "metal")]
#[test]
fn cancelled_adaptive_preparation_does_not_publish_a_dicom_index() {
    use crate::core::registry::ManagedSlideReader;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cancelled-native-prepare.dcm");
    let mut options = TestDicomOptions::native(Vec::new());
    options.transfer_syntax = uids::JPEG2000_LOSSLESS;
    options.rows = 12;
    options.columns = 16;
    options.total_pixel_matrix_rows = 12;
    options.total_pixel_matrix_columns = 16;
    options.pixel_data = TestPixelData::EncapsulatedFrames(vec![include_bytes!(
        "../../../../tests/fixtures/jp2k/rgb_nomct.j2k"
    )
    .to_vec()]);
    write_test_dicom(&path, options);
    let reader = DicomReader {
        slide: Arc::new(DicomSlide::parse(&path).unwrap()),
    };
    let token = crate::ReadCancellationToken::new();
    token.cancel();
    let control = crate::ReadControl::new(token);
    assert!(matches!(
        reader
            .prepare_adaptive_jp2k(&[tile_request(0, 0)], 1, Some(&control))
            .unwrap(),
        Err(WsiError::Cancelled)
    ));
    assert!(reader.slide.levels[0].parts[0]
        .frame_store
        .encapsulated_frames
        .lock()
        .unwrap()
        .is_none());
}

#[cfg(feature = "metal")]
#[test]
fn automatic_region_and_chunked_batch_advance_calibration_once_per_public_read() {
    use crate::core::execution_telemetry::{test_count, Event};
    let Some(_sessions) = test_device_sessions() else {
        return;
    };
    let directory = tempfile::tempdir().unwrap();
    for region in [true, false] {
        let path = directory.path().join(format!("deferred-{region}.dcm"));
        let mut options = TestDicomOptions::native(Vec::new());
        options.series_instance_uid = if region {
            "1.2.826.0.1.3680043.10.777.995"
        } else {
            "1.2.826.0.1.3680043.10.777.996"
        };
        options.sop_instance_uid = if region {
            "1.2.826.0.1.3680043.10.777.995.1"
        } else {
            "1.2.826.0.1.3680043.10.777.996.1"
        };
        options.transfer_syntax = uids::JPEG2000_LOSSLESS;
        options.rows = 12;
        options.columns = 16;
        options.total_pixel_matrix_rows = 36;
        options.total_pixel_matrix_columns = 48;
        options.number_of_frames = 9;
        options.pixel_data = TestPixelData::EncapsulatedFrames(vec![
            include_bytes!(
                "../../../../tests/fixtures/jp2k/rgb_nomct.j2k"
            )
            .to_vec();
            9
        ]);
        write_test_dicom(&path, options);
        let open = || {
            crate::SlideOpenOptions::default()
                .with_cache_config(crate::CacheConfig::default().with_shared_tile_bytes(0))
                .with_limits(
                    crate::SlideLimits::default()
                        .with_batch_chunk_bytes(1)
                        .unwrap(),
                )
        };
        let cpu = Slide::open_with_options(
            &path,
            open().with_decode_execution_options(
                crate::DecodeExecutionOptions::default()
                    .with_acceleration(crate::DecodeAcceleration::CpuOnly),
            ),
        )
        .unwrap();
        let slide = Slide::open_with_options(&path, open()).unwrap();
        let read = |slide: &Slide| {
            if region {
                vec![slide
                    .read_region(&crate::RegionRequest::new(0, 0, 0, (1, 1), (32, 24)))
                    .unwrap()]
            } else {
                slide
                    .read_tiles(
                        &(0..9)
                            .map(|n| tile_request(n % 3, n / 3))
                            .collect::<Vec<_>>(),
                    )
                    .unwrap()
            }
        };
        let expected = read(&cpu);
        for call in 0..5 {
            let before = test_count(Event::MetalBatchSubmissions);
            let actual = read(&slide);
            assert_eq!(actual.len(), expected.len());
            for (actual, expected) in actual.iter().zip(&expected) {
                assert_eq!(actual.as_u8(), expected.as_u8());
            }
            assert_eq!(
                test_count(Event::MetalBatchSubmissions) - before,
                u64::from(call != 0),
                "public read {call}, region={region}"
            );
        }
    }
}

#[test]
fn cached_dicom_region_does_not_dispatch_cpu_work() {
    use crate::core::execution_telemetry::{test_count, Event};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cached-region.dcm");
    write_test_dicom(&path, TestDicomOptions::native(test_rgb_pixel_data()));
    let slide = Slide::open_with_options(
        &path,
        crate::SlideOpenOptions::default().with_decode_execution_options(
            crate::DecodeExecutionOptions::default()
                .with_acceleration(crate::DecodeAcceleration::CpuOnly),
        ),
    )
    .unwrap();
    let request = crate::RegionRequest::new(0, 0, 0, (0, 0), (2, 2));
    let expected = slide.read_region(&request).unwrap();
    let before = test_count(Event::CpuPoolDispatches);
    let actual = slide.read_region(&request).unwrap();
    assert_eq!(actual.as_u8(), expected.as_u8());
    assert_eq!(
        test_count(Event::CpuPoolDispatches) - before,
        0,
        "a cached region must not wait for a decoder worker to decline a fast path"
    );
}

#[test]
fn cached_native_dicom_batches_do_not_dispatch_decoder_workers() {
    use crate::core::execution_telemetry::{test_count, Event};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cached-native.dcm");
    let mut options = TestDicomOptions::native(Vec::new());
    options.transfer_syntax = uids::JPEG2000_LOSSLESS;
    options.rows = 12;
    options.columns = 16;
    options.total_pixel_matrix_rows = 12;
    options.total_pixel_matrix_columns = 32;
    options.number_of_frames = 2;
    options.pixel_data = TestPixelData::EncapsulatedFrames(vec![
        include_bytes!(
            "../../../../tests/fixtures/jp2k/rgb_nomct.j2k"
        )
        .to_vec();
        2
    ]);
    write_test_dicom(&path, options);
    let slide = Slide::open_with_options(
        &path,
        crate::SlideOpenOptions::default().with_decode_execution_options(
            crate::DecodeExecutionOptions::default()
                .with_acceleration(crate::DecodeAcceleration::CpuOnly),
        ),
    )
    .unwrap();
    let requests = vec![tile_request(0, 0); 8];
    let expected = slide.read_tiles(&requests).unwrap();
    let before = test_count(Event::CpuPoolDispatches);
    let cached = slide.read_tiles(&requests).unwrap();
    assert_eq!(
        test_count(Event::CpuPoolDispatches) - before,
        0,
        "an entirely decoded native batch must not wait for a decoder worker"
    );
    for (actual, expected) in cached.iter().zip(&expected) {
        assert_eq!(actual.as_u8(), expected.as_u8());
    }
    let before = test_count(Event::CpuPoolDispatches);
    let mixed = slide
        .read_tiles(&[tile_request(0, 0), tile_request(1, 0), tile_request(0, 0)])
        .unwrap();
    assert_eq!(
        test_count(Event::CpuPoolDispatches) - before,
        1,
        "a partial cache miss must retain the existing decoder pool"
    );
    assert_eq!(mixed.len(), 3);
    for tile in mixed {
        assert_eq!(tile.as_u8(), expected[0].as_u8());
    }
}
