use super::*;
use crate::{ColorSpace, CpuTileData, CpuTileLayout};
use std::panic::{catch_unwind, AssertUnwindSafe};

#[test]
fn backend_sessions_expose_identity_and_both_codec_sessions() {
    let sessions = CudaBackendSessions::default();

    assert_eq!(sessions.device_identity(), "cuda");
    assert_eq!(
        sessions
            .with_jpeg(|session| Ok(session.submissions()))
            .unwrap(),
        0
    );
    assert_eq!(
        sessions
            .with_j2k(|session| Ok(session.submissions()))
            .unwrap(),
        0
    );
}

#[test]
fn backend_sessions_report_poisoned_codec_locks() {
    let jpeg_sessions = CudaBackendSessions::new();
    let jpeg_poison = jpeg_sessions.clone();
    assert!(catch_unwind(AssertUnwindSafe(|| {
        let _ = jpeg_poison.with_jpeg::<()>(|_| panic!("poison JPEG session"));
    }))
    .is_err());
    let jpeg_error = jpeg_sessions
        .with_jpeg(|_| Ok(()))
        .expect_err("poisoned JPEG session must be reported");
    assert!(jpeg_error
        .to_string()
        .contains("JPEG session lock is poisoned"));

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
