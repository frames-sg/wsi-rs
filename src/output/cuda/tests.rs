use super::*;
use crate::{ColorSpace, CpuTileData, CpuTileLayout};
use std::panic::{catch_unwind, AssertUnwindSafe};

#[test]
fn backend_sessions_expose_identity_and_jp2k_session() {
    let sessions = CudaBackendSessions::default();

    assert_eq!(sessions.device_identity(), "cuda:auto");
    assert_eq!(
        sessions
            .with_j2k(|session| Ok(session.submissions()))
            .unwrap(),
        0
    );
}

#[test]
fn backend_sessions_preserve_injected_device_ordinal_identity() {
    let sessions =
        CudaBackendSessions::from_session_for_device(j2k_cuda::CudaSession::default(), 7);

    assert_eq!(sessions.device_identity(), "cuda:7");
}

#[test]
fn system_default_sessions_dynamically_fall_back_without_a_cuda_runtime() {
    match CudaBackendSessions::system_default() {
        Ok(sessions) => {
            let ordinal = sessions
                .device_identity()
                .strip_prefix("cuda:")
                .expect("CUDA identity prefix")
                .parse::<usize>()
                .expect("CUDA identity ordinal");
            assert_eq!(sessions.device_identity(), format!("cuda:{ordinal}"));
        }
        Err(WsiError::Unsupported { reason }) => {
            assert!(
                reason.contains("CUDA JP2K acceleration unavailable"),
                "{reason}"
            );
        }
        Err(error) => panic!("unexpected CUDA initialization error: {error}"),
    }
}

#[test]
fn backend_sessions_report_poisoned_jp2k_lock() {
    let j2k_sessions = CudaBackendSessions::new();
    let j2k_poison = j2k_sessions.clone();
    assert!(catch_unwind(AssertUnwindSafe(|| {
        let _ = j2k_poison.with_j2k::<()>(|_| panic!("poison J2K session"));
    }))
    .is_err());
    let j2k_error = j2k_sessions
        .with_j2k(|_| Ok(()))
        .expect_err("poisoned J2K session must be reported");
    assert!(j2k_error
        .to_string()
        .contains("J2K session lock is poisoned"));
}

#[test]
fn tight_download_layout_rejects_dimension_overflow() {
    let error = tight_download_layout(u32::MAX, u32::MAX, PixelFormat::Rgba16)
        .expect_err("overflowing CUDA host download must fail before allocation");
    assert!(error.to_string().contains("overflow"), "{error}");
}

#[test]
fn download_limit_is_128_mib() {
    enforce_download_limit(128 * 1024 * 1024).expect("limit is inclusive");
    let error = enforce_download_limit(128 * 1024 * 1024 + 1)
        .expect_err("oversized CUDA readback must fail before allocation");
    assert!(matches!(
        error,
        WsiError::ResourceLimit {
            resource: "CUDA host tile download",
            limit: MAX_DEVICE_DOWNLOAD_BYTES,
            ..
        }
    ));
}

#[test]
fn downloaded_bytes_convert_all_public_pixel_families() {
    for (format, channels, color_space) in [
        (PixelFormat::Gray8, 1, ColorSpace::Grayscale),
        (PixelFormat::Rgb8, 3, ColorSpace::Rgb),
        (PixelFormat::Rgba8, 4, ColorSpace::Rgba),
        (PixelFormat::Gray16, 1, ColorSpace::Grayscale),
        (PixelFormat::Rgb16, 3, ColorSpace::Rgb),
        (PixelFormat::Rgba16, 4, ColorSpace::Rgba),
    ] {
        let (_, byte_len) = tight_download_layout(2, 1, format).expect("valid layout");
        let bytes = (0..byte_len).map(|value| value as u8).collect();
        let tile = downloaded_bytes_to_cpu_tile(2, 1, format, bytes)
            .expect("downloaded bytes form a valid CpuTile");
        assert_eq!(tile.channels, channels);
        assert_eq!(tile.color_space, color_space);
        assert_eq!(tile.layout, CpuTileLayout::Interleaved);
        match (format.sample_type(), &tile.data) {
            (crate::SampleType::Uint8, CpuTileData::U8(samples)) => {
                assert_eq!(samples.len(), 2 * channels as usize);
            }
            (crate::SampleType::Uint16, CpuTileData::U16(samples)) => {
                assert_eq!(samples.len(), 2 * channels as usize);
            }
            other => panic!("unexpected CUDA CPU tile storage: {other:?}"),
        }
    }
}

#[test]
fn downloaded_bytes_reject_undersized_output() {
    let error = downloaded_bytes_to_cpu_tile(2, 2, PixelFormat::Rgb8, vec![0; 11])
        .expect_err("undersized CUDA download must fail validation");
    assert!(error.to_string().contains("expected 12 bytes"), "{error}");
}
