use super::*;

#[cfg(feature = "metal")]
fn baseline_metal_jpeg_job() -> JpegDecodeJob<'static> {
    JpegDecodeJob {
        data: Cow::Borrowed(include_bytes!(
            "../../../../tests/fixtures/jpeg/baseline_420_16x16.jpg"
        )),
        tables: None,
        expected_width: 16,
        expected_height: 16,
        color_transform: J2kColorTransform::Auto,
        force_dimensions: false,
        requested_size: None,
    }
}

#[cfg(feature = "cuda")]
fn cuda_unavailable_reason(reason: &str) -> bool {
    reason.contains("CUDA is unavailable") || reason.contains("CUDA runtime error")
}

#[cfg(feature = "cuda")]
fn baseline_cuda_jpeg_job() -> JpegDecodeJob<'static> {
    JpegDecodeJob {
        data: Cow::Borrowed(include_bytes!(
            "../../../../tests/fixtures/jpeg/baseline_420_16x16.jpg"
        )),
        tables: None,
        expected_width: 16,
        expected_height: 16,
        color_transform: J2kColorTransform::Auto,
        force_dimensions: false,
        requested_size: None,
    }
}

#[cfg(feature = "cuda")]
#[test]
fn baseline_420_jpeg_strict_cuda_decodes_to_owned_cuda_surface() {
    let sessions = crate::output::cuda::CudaBackendSessions::new();
    let decoded = decode_one_jpeg_pixels(
        &baseline_cuda_jpeg_job(),
        J2kBackendRequest::Cuda,
        true,
        None,
        Some(&sessions),
    );

    let decoded = match decoded {
        Ok(decoded) => decoded,
        Err(WsiError::Unsupported { reason })
            if cuda_unavailable_reason(&reason)
                && std::env::var_os("J2K_REQUIRE_CUDA_RUNTIME").is_none() =>
        {
            eprintln!("skipping CUDA JPEG decode test: {reason}");
            return;
        }
        Err(err) => panic!("strict CUDA JPEG decode failed unexpectedly: {err}"),
    };

    let TilePixels::Device(DeviceTile::Cuda(tile)) = decoded else {
        panic!("strict CUDA JPEG decode must return DeviceTile::Cuda");
    };
    assert_eq!((tile.width, tile.height), (16, 16));
    assert_eq!(tile.format, crate::PixelFormat::Rgb8);
    assert!(tile.storage.j2k_surface().is_none());
    assert_ne!(tile.storage.device_ptr(), 0);
    assert!(tile.storage.byte_len() >= tile.pitch_bytes * tile.height as usize);
    let surface = tile
        .storage
        .jpeg_surface()
        .expect("CUDA JPEG storage must expose J2k JPEG surface");
    let cuda = surface.cuda_surface().expect("resident CUDA JPEG surface");
    let stats = cuda.stats();
    assert!(
        stats.used_owned_cuda_decode(),
        "strict CUDA JPEG must use owned CUDA decode, got {:?}",
        stats.decode_path()
    );
    assert!(
        !stats.used_hardware_decode(),
        "strict CUDA JPEG success must not be counted through hardware JPEG decode"
    );
}

#[cfg(feature = "cuda")]
#[test]
fn baseline_jpeg_cuda_batch_preserves_order_and_residency() {
    let jobs = [baseline_cuda_jpeg_job(), baseline_cuda_jpeg_job()];
    let sessions = crate::output::cuda::CudaBackendSessions::new();
    let decoded =
        decode_batch_jpeg_pixels(&jobs, J2kBackendRequest::Cuda, true, None, Some(&sessions));

    assert_eq!(decoded.len(), jobs.len());
    for (index, result) in decoded.into_iter().enumerate() {
        match result {
            Ok(TilePixels::Device(DeviceTile::Cuda(tile))) => {
                assert_eq!((tile.width, tile.height), (16, 16), "tile {index}");
                assert_ne!(tile.storage.device_ptr(), 0, "tile {index}");
            }
            Err(WsiError::Unsupported { reason })
                if cuda_unavailable_reason(&reason)
                    && std::env::var_os("J2K_REQUIRE_CUDA_RUNTIME").is_none() =>
            {
                eprintln!("skipping CUDA JPEG batch test: {reason}");
                return;
            }
            other => panic!("CUDA JPEG batch tile {index} was not resident: {other:?}"),
        }
    }
}

#[cfg(feature = "cuda")]
#[test]
fn baseline_jpeg_cuda_download_cpu_matches_cpu_decode() {
    let job = baseline_cuda_jpeg_job();
    let expected = decode_one_jpeg_job(&job).expect("CPU JPEG decode");
    let sessions = crate::output::cuda::CudaBackendSessions::new();
    let decoded =
        decode_one_jpeg_pixels(&job, J2kBackendRequest::Cuda, true, None, Some(&sessions));
    let decoded = match decoded {
        Ok(decoded) => decoded,
        Err(WsiError::Unsupported { reason })
            if cuda_unavailable_reason(&reason)
                && std::env::var_os("J2K_REQUIRE_CUDA_RUNTIME").is_none() =>
        {
            eprintln!("skipping CUDA JPEG host-download parity test: {reason}");
            return;
        }
        Err(err) => panic!("strict CUDA JPEG decode failed unexpectedly: {err}"),
    };
    let TilePixels::Device(DeviceTile::Cuda(tile)) = decoded else {
        panic!("strict CUDA JPEG decode must return DeviceTile::Cuda");
    };
    let mut invalid = tile.clone();
    invalid.width += 1;
    let metadata_error = invalid
        .download_cpu()
        .expect_err("mismatched CUDA JPEG metadata must fail before download");
    assert!(metadata_error
        .to_string()
        .contains("does not match its surface"));
    let actual = tile.download_cpu().expect("download CUDA JPEG tile");

    assert_eq!(
        (actual.width, actual.height),
        (expected.width, expected.height)
    );
    assert_eq!(actual.channels, expected.channels);
    assert_eq!(actual.color_space, expected.color_space);
    assert_eq!(actual.layout, expected.layout);
    assert_eq!(actual.data.as_u8(), expected.data.as_u8());
}

#[cfg(feature = "cuda")]
#[test]
fn require_cuda_jpeg_without_session_returns_unsupported() {
    let err = decode_one_jpeg_pixels(
        &baseline_cuda_jpeg_job(),
        J2kBackendRequest::Cuda,
        true,
        None,
        None,
    )
    .unwrap_err();

    let WsiError::Unsupported { reason } = err else {
        panic!("strict CUDA JPEG without session must be Unsupported, got {err:?}");
    };
    assert!(
        reason.contains("CUDA session"),
        "unexpected strict CUDA JPEG error: {reason}"
    );
}

#[cfg(feature = "cuda")]
#[test]
fn require_cuda_progressive_jpeg_returns_unsupported_without_cpu_fallback() {
    let sessions = crate::output::cuda::CudaBackendSessions::new();
    let job = JpegDecodeJob {
        data: Cow::Owned(progressive_8x8_jpeg()),
        tables: None,
        expected_width: 8,
        expected_height: 8,
        color_transform: J2kColorTransform::Auto,
        force_dimensions: false,
        requested_size: None,
    };

    let err = decode_one_jpeg_pixels(&job, J2kBackendRequest::Cuda, true, None, Some(&sessions))
        .unwrap_err();

    let WsiError::Unsupported { reason } = err else {
        panic!("strict CUDA progressive JPEG must be Unsupported, got {err:?}");
    };
    assert!(
        reason.contains("Progressive8 JPEG") && reason.contains("CUDA"),
        "unexpected strict CUDA progressive JPEG error: {reason}"
    );
}

#[cfg(feature = "metal")]
#[test]
fn progressive_jpeg_device_route_uses_cpu_unless_device_is_required() {
    let jpeg_data = progressive_8x8_jpeg();
    let view = J2kJpegView::parse_with_options(
        &jpeg_data,
        J2kDecodeOptions::default().with_color_transform(J2kColorTransform::Auto),
    )
    .unwrap();

    assert!(progressive_jpeg_requires_cpu_device_route(&view, false, "Metal").unwrap());
    let err = progressive_jpeg_requires_cpu_device_route(&view, true, "Metal").unwrap_err();
    assert!(matches!(
        err,
        WsiError::Unsupported { reason }
            if reason.contains("Progressive8") && reason.contains("Metal")
    ));
}

#[cfg(feature = "metal")]
#[test]
fn metal_jpeg_without_session_falls_back_unless_device_is_required() {
    let job = baseline_metal_jpeg_job();

    let decoded = decode_one_jpeg_pixels(&job, J2kBackendRequest::Metal, false, None, None)
        .expect("CPU fallback");
    let TilePixels::Cpu(tile) = decoded else {
        panic!("missing Metal session must use the CPU fallback");
    };
    assert_eq!((tile.width, tile.height), (16, 16));

    let error = decode_one_jpeg_pixels(&job, J2kBackendRequest::Metal, true, None, None)
        .expect_err("required Metal output needs a session");
    assert!(matches!(
        error,
        WsiError::Unsupported { reason } if reason.contains("without Metal session")
    ));
}

#[cfg(feature = "metal")]
#[test]
fn metal_jpeg_batch_without_session_handles_empty_single_and_parallel_fallbacks() {
    assert!(decode_batch_jpeg_pixels(&[], J2kBackendRequest::Metal, false, None, None,).is_empty());

    let jobs = [baseline_metal_jpeg_job(), baseline_metal_jpeg_job()];
    for decoded in [
        decode_batch_jpeg_pixels(&jobs[..1], J2kBackendRequest::Metal, false, None, None),
        decode_batch_jpeg_pixels(&jobs, J2kBackendRequest::Metal, false, None, None),
    ] {
        assert!(!decoded.is_empty());
        for tile in decoded {
            assert!(matches!(tile.expect("CPU fallback"), TilePixels::Cpu(_)));
        }
    }
}

#[cfg(all(feature = "metal", target_os = "macos"))]
#[test]
fn progressive_metal_jpeg_decode_uses_cpu_fallback_or_strict_error() {
    let Ok(sessions) = crate::output::metal::MetalBackendSessions::system_default() else {
        return;
    };
    let job = JpegDecodeJob {
        data: Cow::Owned(progressive_8x8_jpeg()),
        tables: None,
        expected_width: 8,
        expected_height: 8,
        color_transform: J2kColorTransform::Auto,
        force_dimensions: false,
        requested_size: None,
    };

    let decoded =
        decode_one_jpeg_pixels(&job, J2kBackendRequest::Metal, false, Some(&sessions), None)
            .expect("progressive CPU fallback");
    assert!(matches!(decoded, TilePixels::Cpu(_)));

    let error = decode_one_jpeg_pixels(&job, J2kBackendRequest::Metal, true, Some(&sessions), None)
        .expect_err("strict progressive Metal output is unsupported");
    assert!(matches!(
        error,
        WsiError::Unsupported { reason }
            if reason.contains("Progressive8") && reason.contains("Metal")
    ));
}

#[cfg(all(feature = "metal", target_os = "macos"))]
#[test]
fn private_metal_jpeg_decode_returns_private_device_tile() {
    let Ok(sessions) = crate::output::metal::MetalBackendSessions::system_default() else {
        return;
    };
    let sessions = sessions.with_private_jpeg_decode();
    let mut rgb = image::RgbImage::new(16, 16);
    for (idx, pixel) in rgb.pixels_mut().enumerate() {
        *pixel = image::Rgb([
            ((idx * 17) & 0xff) as u8,
            ((idx * 31 + 9) & 0xff) as u8,
            ((idx * 7 + 3) & 0xff) as u8,
        ]);
    }
    let jpeg_data = encode_test_jpeg(&rgb);
    let job = JpegDecodeJob {
        data: Cow::Borrowed(jpeg_data.as_slice()),
        tables: None,
        expected_width: 16,
        expected_height: 16,
        color_transform: J2kColorTransform::Auto,
        force_dimensions: false,
        requested_size: None,
    };

    let pixels =
        decode_one_jpeg_pixels(&job, J2kBackendRequest::Metal, true, Some(&sessions), None)
            .expect("private JPEG Metal tile");
    let TilePixels::Device(DeviceTile::Metal(tile)) = pixels else {
        panic!("expected private Metal tile");
    };
    let crate::output::metal::MetalDeviceStorage::Resident { image } = tile.storage;
    assert_eq!(image.dimensions(), (16, 16));
    assert_eq!(tile.width, 16);
    assert_eq!(tile.height, 16);
}

#[cfg(all(feature = "metal", target_os = "macos"))]
#[test]
fn metal_jpeg_edge_tile_is_cropped_to_expected_dimensions() {
    let Ok(sessions) = crate::output::metal::MetalBackendSessions::system_default() else {
        return;
    };
    let mut rgb = image::RgbImage::new(16, 16);
    for (idx, pixel) in rgb.pixels_mut().enumerate() {
        *pixel = image::Rgb([
            ((idx * 17) & 0xff) as u8,
            ((idx * 31 + 9) & 0xff) as u8,
            ((idx * 7 + 3) & 0xff) as u8,
        ]);
    }
    let jpeg_data = encode_test_jpeg(&rgb);
    let job = JpegDecodeJob {
        data: Cow::Borrowed(jpeg_data.as_slice()),
        tables: None,
        expected_width: 7,
        expected_height: 11,
        color_transform: J2kColorTransform::Auto,
        force_dimensions: false,
        requested_size: None,
    };

    let pixels =
        decode_one_jpeg_pixels(&job, J2kBackendRequest::Metal, true, Some(&sessions), None)
            .expect("cropped Metal JPEG edge tile");
    let TilePixels::Device(DeviceTile::Metal(tile)) = pixels else {
        panic!("expected resident Metal tile");
    };

    assert_eq!((tile.width, tile.height), (7, 11));
    assert_eq!(
        tile.validated_resident_image().unwrap().dimensions(),
        (7, 11)
    );
}

#[cfg(all(feature = "metal", target_os = "macos"))]
#[test]
fn decode_batch_jpeg_pixels_uses_session_backed_device_batch() {
    let Ok(sessions) = crate::output::metal::MetalBackendSessions::system_default() else {
        return;
    };
    let mut first = image::RgbImage::new(16, 16);
    for (idx, pixel) in first.pixels_mut().enumerate() {
        *pixel = image::Rgb([idx as u8, 80, 180]);
    }
    let mut second = image::RgbImage::new(16, 16);
    for (idx, pixel) in second.pixels_mut().enumerate() {
        *pixel = image::Rgb([200, idx as u8, 40]);
    }
    let first_jpeg = encode_test_jpeg(&first);
    let second_jpeg = encode_test_jpeg(&second);
    let jobs = [
        JpegDecodeJob {
            data: Cow::Borrowed(first_jpeg.as_slice()),
            tables: None,
            expected_width: 16,
            expected_height: 16,
            color_transform: J2kColorTransform::Auto,
            force_dimensions: false,
            requested_size: None,
        },
        JpegDecodeJob {
            data: Cow::Borrowed(second_jpeg.as_slice()),
            tables: None,
            expected_width: 16,
            expected_height: 16,
            color_transform: J2kColorTransform::Auto,
            force_dimensions: false,
            requested_size: None,
        },
    ];

    reset_jpeg_device_batch_attempts_for_test();
    let pixels =
        decode_batch_jpeg_pixels(&jobs, J2kBackendRequest::Metal, true, Some(&sessions), None);

    assert_eq!(jpeg_device_batch_attempts_for_test(), 1);
    assert_eq!(pixels.len(), 2);
    for pixels in pixels {
        assert!(matches!(pixels.unwrap(), TilePixels::Device(_)));
    }
}
