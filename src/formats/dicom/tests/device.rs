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
