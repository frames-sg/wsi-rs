use std::borrow::Cow;

use j2k_core::BackendRequest as J2kBackendRequest;

use super::*;

fn rgb_job(backend: J2kBackendRequest) -> Jp2kDecodeJob<'static> {
    let codestream = include_bytes!("../../../../tests/fixtures/jp2k/rgb_nomct.j2k");
    let header = parse_codestream_header(codestream).expect("fixture header");
    Jp2kDecodeJob {
        data: Cow::Borrowed(codestream),
        expected_width: header.image_width,
        expected_height: header.image_height,
        rgb_color_space: true,
        backend,
    }
}

#[cfg(feature = "metal")]
fn metal_sessions() -> Option<crate::output::metal::MetalBackendSessions> {
    crate::output::metal::MetalBackendSessions::system_default().ok()
}

#[cfg(feature = "metal")]
#[test]
fn strict_metal_decode_returns_resident_tile_with_cpu_download_parity() {
    let Some(sessions) = metal_sessions() else {
        eprintln!("skipping JP2K Metal test: no Metal device");
        return;
    };
    let job = rgb_job(J2kBackendRequest::Metal);
    let expected = decode_batch_jp2k(&[rgb_job(J2kBackendRequest::Cpu)])
        .pop()
        .expect("CPU result")
        .expect("CPU decode");

    let tile = decode_one_jp2k_metal(&job, &sessions).expect("strict Metal decode");
    assert_eq!(
        (tile.width, tile.height),
        (job.expected_width, job.expected_height)
    );
    assert_eq!(tile.format, PixelFormat::Rgb8);
    let downloaded = tile.download_cpu().expect("Metal readback");
    assert_eq!(downloaded.as_u8(), expected.as_u8());
}

#[cfg(feature = "metal")]
#[test]
fn strict_metal_batch_preserves_order_cardinality_and_logical_geometry() {
    let Some(sessions) = metal_sessions() else {
        return;
    };
    let full = rgb_job(J2kBackendRequest::Metal);
    let mut cropped = rgb_job(J2kBackendRequest::Metal);
    cropped.expected_width -= 1;
    cropped.expected_height -= 2;
    let expected = [
        (cropped.expected_width, cropped.expected_height),
        (full.expected_width, full.expected_height),
    ];
    let decoded = decode_batch_jp2k_metal(&[cropped, full], &sessions);

    assert_eq!(decoded.len(), expected.len());
    for (decoded, dimensions) in decoded.into_iter().zip(expected) {
        let tile = decoded.expect("strict Metal batch tile");
        assert_eq!((tile.width, tile.height), dimensions);
        assert_eq!(
            tile.download_cpu()
                .expect("cropped readback")
                .as_u8()
                .expect("RGB8 data")
                .len(),
            dimensions.0 as usize * dimensions.1 as usize * 3
        );
    }
}

#[cfg(feature = "metal")]
#[test]
fn strict_metal_batch_reports_each_malformed_job_without_cpu_fallback() {
    let Some(sessions) = metal_sessions() else {
        return;
    };
    let malformed = Jp2kDecodeJob {
        data: Cow::Borrowed(b"not a codestream"),
        expected_width: 1,
        expected_height: 1,
        rgb_color_space: true,
        backend: J2kBackendRequest::Metal,
    };
    let results = decode_batch_jp2k_metal(&[malformed], &sessions);
    assert_eq!(results.len(), 1);
    assert!(results[0].is_err());
}

#[cfg(feature = "cuda")]
#[test]
fn strict_cuda_decode_returns_resident_jp2k_tile() {
    let sessions = crate::output::cuda::CudaBackendSessions::new();
    let mut job = rgb_job(J2kBackendRequest::Cuda);
    job.expected_width -= 1;
    job.expected_height -= 2;
    let tile = match decode_one_jp2k_cuda(&job, &sessions) {
        Ok(tile) => tile,
        Err(crate::WsiError::Unsupported { reason })
            if std::env::var_os("J2K_REQUIRE_CUDA_RUNTIME").is_none() =>
        {
            eprintln!("skipping JP2K CUDA test: {reason}");
            return;
        }
        Err(err) => panic!("strict CUDA decode failed: {err}"),
    };
    assert_eq!(
        (tile.width, tile.height),
        (job.expected_width, job.expected_height)
    );
    assert_eq!(tile.format, PixelFormat::Rgb8);
    assert_ne!(tile.storage.device_ptr(), 0);
    assert_eq!(
        tile.storage.j2k_surface().residency(),
        j2k_cuda::SurfaceResidency::CudaResidentDecode
    );
    let downloaded = tile.download_cpu().expect("CUDA readback");
    assert_eq!(
        (downloaded.width(), downloaded.height()),
        (tile.width, tile.height)
    );
    assert_eq!(
        downloaded.as_u8().expect("RGB8 CUDA download").len(),
        tile.width as usize * tile.height as usize * 3
    );
}

#[cfg(feature = "cuda")]
#[test]
fn strict_cuda_batch_preserves_empty_and_result_cardinality() {
    let sessions = crate::output::cuda::CudaBackendSessions::new();
    assert!(decode_batch_jp2k_cuda(&[], &sessions).is_empty());
    let results = decode_batch_jp2k_cuda(
        &[
            rgb_job(J2kBackendRequest::Cuda),
            rgb_job(J2kBackendRequest::Cuda),
        ],
        &sessions,
    );
    assert_eq!(results.len(), 2);
}
