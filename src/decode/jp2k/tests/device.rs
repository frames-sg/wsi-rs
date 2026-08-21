use super::*;
#[cfg(any(feature = "parity-metal", feature = "cuda"))]
use crate::core::types::ColorSpace;
#[cfg(feature = "cuda")]
use crate::core::types::CpuTileLayout;

#[cfg(feature = "metal")]
fn test_metal_sessions() -> Option<crate::output::metal::MetalBackendSessions> {
    crate::output::metal::MetalBackendSessions::system_default().ok()
}

#[cfg(feature = "metal")]
fn rgb_metal_job(backend: J2kBackendRequest) -> Jp2kDecodeJob<'static> {
    let codestream = include_bytes!("../../../../tests/fixtures/jp2k/rgb_nomct.j2k");
    let header = parse_codestream_header(codestream).expect("RGB JP2K fixture header");
    Jp2kDecodeJob {
        data: Cow::Borrowed(codestream),
        expected_width: header.image_width,
        expected_height: header.image_height,
        rgb_color_space: true,
        backend,
    }
}

#[cfg(feature = "parity-metal")]
#[test]
fn j2k_metal_vs_cpu_within_tolerance() {
    let Some(sessions) = test_metal_sessions() else {
        eprintln!("skipping JP2K Metal parity test: no Metal device");
        return;
    };
    let fixtures: &[(&str, &[u8], bool)] = &[
        (
            "rgb-nomct",
            include_bytes!("../../../../tests/fixtures/jp2k/rgb_nomct.j2k"),
            true,
        ),
        (
            "ycbcr-444",
            include_bytes!("../../../../tests/fixtures/jp2k/ycbcr_444.j2k"),
            false,
        ),
        (
            "ycbcr-422",
            include_bytes!("../../../../tests/fixtures/jp2k/ycbcr_422.j2k"),
            false,
        ),
        (
            "ycbcr-420",
            include_bytes!("../../../../tests/fixtures/jp2k/ycbcr_420.j2k"),
            false,
        ),
    ];

    for &(label, codestream, rgb_color_space) in fixtures {
        assert_metal_decode_matches_cpu(label, codestream, rgb_color_space, &sessions);
    }
}

#[cfg(feature = "parity-metal")]
fn assert_metal_decode_matches_cpu(
    label: &str,
    codestream: &[u8],
    rgb_color_space: bool,
    sessions: &crate::output::metal::MetalBackendSessions,
) {
    let header = parse_codestream_header(codestream).expect("fixture header");
    let job = |backend| Jp2kDecodeJob {
        data: Cow::Borrowed(codestream),
        expected_width: header.image_width,
        expected_height: header.image_height,
        rgb_color_space,
        backend,
    };
    let TilePixels::Cpu(cpu) =
        decode_one_jp2k_pixels(&job(J2kBackendRequest::Cpu), false, None, None)
            .expect("CPU fixture decode")
    else {
        panic!("CPU fixture decode returned device pixels");
    };
    let TilePixels::Device(DeviceTile::Metal(metal)) =
        decode_one_jp2k_pixels(&job(J2kBackendRequest::Auto), true, Some(sessions), None)
            .expect("Metal fixture decode")
    else {
        panic!("Metal fixture decode did not return a Metal tile");
    };
    assert_eq!(metal.format, PixelFormat::Rgb8, "{label} pixel format");
    assert_eq!(
        metal.pitch_bytes,
        metal.width as usize * 3,
        "{label} fixture should have tightly packed Metal rows"
    );
    let image = metal
        .validated_resident_image()
        .expect("validated Metal fixture image");
    let metal_bytes = crate::output::metal::resident_bytes(image);
    let metal_cpu =
        CpuTile::from_u8_interleaved(metal.width, metal.height, 3, ColorSpace::Rgb, metal_bytes)
            .expect("Metal readback tile");
    let cpu_rgb = image::RgbImage::from_raw(
        cpu.width,
        cpu.height,
        cpu.data.as_u8().expect("CPU RGB8 fixture").to_vec(),
    )
    .expect("CPU fixture dimensions");

    assert_cpu_tile_matches_rgb_fixture_with_tolerance(
        &metal_cpu,
        &cpu_rgb,
        4,
        100,
        &format!("{label} Metal vs CPU"),
    );
}

#[cfg(feature = "cuda")]
fn cuda_unavailable_reason(reason: &str) -> bool {
    reason.contains("CUDA is unavailable") || reason.contains("CUDA runtime error")
}

#[cfg(feature = "cuda")]
fn rgb8_htj2k_fixture(width: u32, height: u32) -> Vec<u8> {
    let mut pixels = Vec::with_capacity(width as usize * height as usize * 3);
    for idx in 0..width * height {
        pixels.push(u8::try_from((idx * 17 + idx / 3) & 0xff).expect("red"));
        pixels.push(u8::try_from((idx * 29 + 7) & 0xff).expect("green"));
        pixels.push(u8::try_from((idx * 43 + 19) & 0xff).expect("blue"));
    }
    let options = j2k_native::EncodeOptions {
        reversible: true,
        num_decomposition_levels: 1,
        ..j2k_native::EncodeOptions::default()
    };
    j2k_native::encode_htj2k(&pixels, width, height, 3, 8, false, &options)
        .expect("encode RGB HTJ2K fixture")
}

#[cfg(feature = "cuda")]
#[test]
fn htj2k_strict_cuda_decodes_to_cuda_surface() {
    let codestream = rgb8_htj2k_fixture(32, 32);
    let job = Jp2kDecodeJob {
        data: Cow::Borrowed(codestream.as_slice()),
        expected_width: 32,
        expected_height: 32,
        rgb_color_space: true,
        backend: J2kBackendRequest::Cuda,
    };
    let sessions = crate::output::cuda::CudaBackendSessions::new();

    let decoded = decode_one_jp2k_pixels(&job, true, None, Some(&sessions));
    let decoded = match decoded {
        Ok(decoded) => decoded,
        Err(WsiError::Unsupported { reason })
            if cuda_unavailable_reason(&reason)
                && std::env::var_os("J2K_REQUIRE_CUDA_RUNTIME").is_none() =>
        {
            eprintln!("skipping CUDA HTJ2K decode test: {reason}");
            return;
        }
        Err(err) => panic!("strict CUDA HTJ2K decode failed unexpectedly: {err}"),
    };

    let TilePixels::Device(DeviceTile::Cuda(tile)) = decoded else {
        panic!("strict CUDA HTJ2K decode must return DeviceTile::Cuda");
    };
    assert_eq!((tile.width, tile.height), (32, 32));
    assert_eq!(tile.format, PixelFormat::Rgb8);
    assert!(tile.storage.jpeg_surface().is_none());
    assert_ne!(tile.storage.device_ptr(), 0);
    assert!(tile.storage.byte_len() >= tile.pitch_bytes * tile.height as usize);
    let surface = tile
        .storage
        .j2k_surface()
        .expect("CUDA J2K storage must expose J2k J2K surface");
    assert_eq!(
        surface.residency(),
        j2k_cuda::SurfaceResidency::CudaResidentDecode
    );
    let stats = surface
        .cuda_surface()
        .expect("resident CUDA J2K surface")
        .stats();
    assert!(
        stats.decode_kernel_dispatches() > 0,
        "strict CUDA HTJ2K should include CUDA decode dispatches"
    );
}

#[cfg(feature = "cuda")]
#[test]
fn htj2k_cuda_batch_preserves_order_and_residency() {
    let codestream = rgb8_htj2k_fixture(32, 32);
    let jobs = [0, 1].map(|_| Jp2kDecodeJob {
        data: Cow::Borrowed(codestream.as_slice()),
        expected_width: 32,
        expected_height: 32,
        rgb_color_space: true,
        backend: J2kBackendRequest::Cuda,
    });
    let sessions = crate::output::cuda::CudaBackendSessions::new();
    let decoded = decode_batch_jp2k_pixels(&jobs, true, None, Some(&sessions));

    assert_eq!(decoded.len(), jobs.len());
    for (index, result) in decoded.into_iter().enumerate() {
        match result {
            Ok(TilePixels::Device(DeviceTile::Cuda(tile))) => {
                assert_eq!((tile.width, tile.height), (32, 32), "tile {index}");
                assert_ne!(tile.storage.device_ptr(), 0, "tile {index}");
            }
            Err(WsiError::Unsupported { reason })
                if cuda_unavailable_reason(&reason)
                    && std::env::var_os("J2K_REQUIRE_CUDA_RUNTIME").is_none() =>
            {
                eprintln!("skipping CUDA HTJ2K batch test: {reason}");
                return;
            }
            other => panic!("CUDA HTJ2K batch tile {index} was not resident: {other:?}"),
        }
    }
}

#[cfg(feature = "cuda")]
#[test]
fn htj2k_cuda_download_cpu_rgb8_matches_cpu_decode() {
    let codestream = rgb8_htj2k_fixture(32, 32);
    let job = |backend| Jp2kDecodeJob {
        data: Cow::Borrowed(codestream.as_slice()),
        expected_width: 32,
        expected_height: 32,
        rgb_color_space: true,
        backend,
    };
    let expected = decode_batch_jp2k(&[job(J2kBackendRequest::Cpu)])
        .pop()
        .expect("one CPU result")
        .expect("CPU HTJ2K decode");
    let sessions = crate::output::cuda::CudaBackendSessions::new();
    let decoded =
        decode_one_jp2k_pixels(&job(J2kBackendRequest::Cuda), true, None, Some(&sessions));
    let decoded = match decoded {
        Ok(decoded) => decoded,
        Err(WsiError::Unsupported { reason })
            if cuda_unavailable_reason(&reason)
                && std::env::var_os("J2K_REQUIRE_CUDA_RUNTIME").is_none() =>
        {
            eprintln!("skipping CUDA HTJ2K host-download parity test: {reason}");
            return;
        }
        Err(err) => panic!("strict CUDA HTJ2K decode failed unexpectedly: {err}"),
    };
    let TilePixels::Device(DeviceTile::Cuda(tile)) = decoded else {
        panic!("strict CUDA HTJ2K decode must return DeviceTile::Cuda");
    };
    let mut invalid = tile.clone();
    invalid.height += 1;
    let metadata_error = invalid
        .download_cpu()
        .expect_err("mismatched CUDA J2K metadata must fail before download");
    assert!(metadata_error
        .to_string()
        .contains("does not match its surface"));
    let actual = tile.download_cpu().expect("download CUDA HTJ2K tile");

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
fn htj2k_cuda_download_cpu_rgb16_matches_cpu_decode() {
    let width = 17;
    let height = 19;
    let samples = (0..width * height * 3)
        .map(|index| u16::try_from((index * 257 + index / 7) & 0xffff).expect("u16 sample"))
        .collect::<Vec<_>>();
    let input = samples
        .iter()
        .flat_map(|sample| sample.to_ne_bytes())
        .collect::<Vec<_>>();
    let options = j2k_native::EncodeOptions {
        reversible: true,
        num_decomposition_levels: 1,
        ..j2k_native::EncodeOptions::default()
    };
    let codestream = j2k_native::encode_htj2k(&input, width, height, 3, 16, false, &options)
        .expect("encode RGB16 HTJ2K fixture");

    let mut device_decoder = j2k_cuda::J2kDecoder::new(&codestream).expect("CUDA decoder");
    let surface = match device_decoder
        .decode_to_device(j2k_core::PixelFormat::Rgb16, J2kBackendRequest::Cuda)
    {
        Ok(surface) => surface,
        Err(j2k_cuda::Error::CudaUnavailable | j2k_cuda::Error::CudaRuntime { .. })
            if std::env::var_os("J2K_REQUIRE_CUDA_RUNTIME").is_none() =>
        {
            eprintln!("skipping CUDA RGB16 host-download parity test: CUDA unavailable");
            return;
        }
        Err(err) => panic!("strict CUDA RGB16 decode failed unexpectedly: {err}"),
    };
    let tile = crate::output::cuda::CudaDeviceTile::from_j2k(surface)
        .expect("validate CUDA surface")
        .expect("strict CUDA decode must be resident");
    let actual = tile.download_cpu().expect("download CUDA RGB16 tile");

    let row_bytes = width as usize * j2k_core::PixelFormat::Rgb16.bytes_per_pixel();
    let mut expected_bytes = vec![0; row_bytes * height as usize];
    let mut cpu_decoder = j2k_cuda::J2kDecoder::new(&codestream).expect("CPU decoder");
    cpu_decoder
        .decode_into(&mut expected_bytes, row_bytes, j2k_core::PixelFormat::Rgb16)
        .expect("CPU RGB16 decode");
    let expected = expected_bytes
        .chunks_exact(2)
        .map(|sample| u16::from_ne_bytes([sample[0], sample[1]]))
        .collect::<Vec<_>>();

    assert_eq!((actual.width, actual.height), (width, height));
    assert_eq!(actual.channels, 3);
    assert_eq!(actual.color_space, ColorSpace::Rgb);
    assert_eq!(actual.layout, CpuTileLayout::Interleaved);
    assert_eq!(actual.data.as_u16(), Some(expected.as_slice()));
}

#[cfg(feature = "cuda")]
#[test]
fn require_cuda_jp2k_without_session_returns_unsupported() {
    let codestream = include_bytes!("../../../../tests/fixtures/jp2k/rgb_nomct.j2k");
    let header = parse_codestream_header(codestream).unwrap();
    let job = Jp2kDecodeJob {
        data: Cow::Borrowed(codestream),
        expected_width: header.image_width,
        expected_height: header.image_height,
        rgb_color_space: true,
        backend: J2kBackendRequest::Cuda,
    };

    let err = decode_one_jp2k_pixels(&job, true, None, None).unwrap_err();
    let WsiError::Unsupported { reason } = err else {
        panic!("strict CUDA JP2K without session must be Unsupported, got {err:?}");
    };
    assert!(
        reason.contains("CUDA session"),
        "unexpected strict CUDA JP2K error: {reason}"
    );
}

#[cfg(feature = "cuda")]
#[test]
fn require_cuda_classic_jp2k_decodes_to_resident_surface_without_copy_dispatches() {
    let codestream = include_bytes!("../../../../tests/fixtures/jp2k/rgb_nomct.j2k");
    let header = parse_codestream_header(codestream).unwrap();
    let job = Jp2kDecodeJob {
        data: Cow::Borrowed(codestream),
        expected_width: header.image_width,
        expected_height: header.image_height,
        rgb_color_space: true,
        backend: J2kBackendRequest::Cuda,
    };
    let sessions = crate::output::cuda::CudaBackendSessions::new();

    let decoded = match decode_one_jp2k_pixels(&job, true, None, Some(&sessions)) {
        Ok(decoded) => decoded,
        Err(WsiError::Unsupported { reason })
            if cuda_unavailable_reason(&reason)
                && std::env::var_os("J2K_REQUIRE_CUDA_RUNTIME").is_none() =>
        {
            eprintln!("skipping CUDA classic JP2K decode test: {reason}");
            return;
        }
        Err(err) => panic!("strict CUDA classic JP2K decode failed unexpectedly: {err}"),
    };

    let TilePixels::Device(DeviceTile::Cuda(tile)) = decoded else {
        panic!("strict CUDA classic JP2K decode must return DeviceTile::Cuda");
    };
    let surface = tile
        .storage
        .j2k_surface()
        .expect("CUDA classic JP2K storage must expose a J2K surface");
    assert_eq!(
        surface.residency(),
        j2k_cuda::SurfaceResidency::CudaResidentDecode
    );
    let stats = surface
        .cuda_surface()
        .expect("resident CUDA classic JP2K surface")
        .stats();
    assert_eq!(
        stats.copy_kernel_dispatches(),
        0,
        "strict CUDA classic JP2K must not stage through a copy kernel"
    );
    assert!(
        stats.decode_kernel_dispatches() > 0,
        "strict CUDA classic JP2K must execute CUDA decode kernels"
    );
}

#[cfg(feature = "metal")]
#[test]
fn fixture_rgb_device_batch_returns_metal_tiles() {
    let Some(sessions) = test_metal_sessions() else {
        eprintln!("skipping JP2K device batch test: no Metal device");
        return;
    };
    let codestream = include_bytes!("../../../../tests/fixtures/jp2k/rgb_nomct.j2k");
    let header = parse_codestream_header(codestream).unwrap();
    let requests = [
        Jp2kDecodeJob {
            data: Cow::Borrowed(codestream),
            expected_width: header.image_width - 1,
            expected_height: header.image_height - 2,
            rgb_color_space: true,
            backend: J2kBackendRequest::Auto,
        },
        Jp2kDecodeJob {
            data: Cow::Borrowed(codestream),
            expected_width: header.image_width,
            expected_height: header.image_height,
            rgb_color_space: true,
            backend: J2kBackendRequest::Auto,
        },
    ];

    let decoded = decode_jp2k_tile_batch_to_device_pixels(&requests, &sessions).unwrap();

    assert_eq!(decoded.len(), 2);
    for (tile, dimensions) in decoded.into_iter().zip([
        (header.image_width - 1, header.image_height - 2),
        (header.image_width, header.image_height),
    ]) {
        let TilePixels::Device(DeviceTile::Metal(tile)) = tile else {
            panic!("expected Metal device tile");
        };
        assert_eq!((tile.width, tile.height), dimensions);
        assert_eq!(tile.format, PixelFormat::Rgb8);
    }
}

#[cfg(feature = "metal")]
#[test]
fn fixture_ycbcr_device_decode_returns_rgb_metal_tile() {
    let Some(sessions) = test_metal_sessions() else {
        eprintln!("skipping JP2K YCbCr device decode test: no Metal device");
        return;
    };
    let codestream = include_bytes!("../../../../tests/fixtures/jp2k/ycbcr_444.j2k");
    let header = parse_codestream_header(codestream).unwrap();
    let request = Jp2kDecodeJob {
        data: Cow::Borrowed(codestream),
        expected_width: header.image_width - 1,
        expected_height: header.image_height - 2,
        rgb_color_space: false,
        backend: J2kBackendRequest::Auto,
    };

    let decoded = decode_one_jp2k_pixels(&request, true, Some(&sessions), None).unwrap();
    let TilePixels::Device(DeviceTile::Metal(tile)) = decoded else {
        panic!("expected converted Metal device tile");
    };
    assert_eq!(
        (tile.width, tile.height),
        (header.image_width - 1, header.image_height - 2)
    );
    assert_eq!(tile.format, PixelFormat::Rgb8);
    let crate::output::metal::MetalDeviceStorage::Resident { image } = &tile.storage;
    assert_eq!(image.byte_offset(), 0);
    assert!(image.byte_len() >= tile.pitch_bytes * tile.height as usize);
}

#[cfg(feature = "metal")]
#[test]
fn fixture_ycbcr_device_batch_returns_rgb_metal_tiles() {
    let Some(sessions) = test_metal_sessions() else {
        eprintln!("skipping JP2K YCbCr device batch test: no Metal device");
        return;
    };
    let codestream = include_bytes!("../../../../tests/fixtures/jp2k/ycbcr_444.j2k");
    let header = parse_codestream_header(codestream).unwrap();
    let requests = [
        Jp2kDecodeJob {
            data: Cow::Borrowed(codestream),
            expected_width: header.image_width - 1,
            expected_height: header.image_height - 2,
            rgb_color_space: false,
            backend: J2kBackendRequest::Auto,
        },
        Jp2kDecodeJob {
            data: Cow::Borrowed(codestream),
            expected_width: header.image_width,
            expected_height: header.image_height,
            rgb_color_space: false,
            backend: J2kBackendRequest::Auto,
        },
    ];

    let decoded = decode_jp2k_tile_batch_to_device_pixels(&requests, &sessions).unwrap();

    assert_eq!(decoded.len(), 2);
    for (tile, dimensions) in decoded.into_iter().zip([
        (header.image_width - 1, header.image_height - 2),
        (header.image_width, header.image_height),
    ]) {
        let TilePixels::Device(DeviceTile::Metal(tile)) = tile else {
            panic!("expected Metal device tile");
        };
        assert_eq!((tile.width, tile.height), dimensions);
        assert_eq!(tile.format, PixelFormat::Rgb8);
    }
}

#[cfg(feature = "metal")]
#[test]
fn metal_jp2k_without_session_falls_back_unless_device_is_required() {
    let job = rgb_metal_job(J2kBackendRequest::Auto);

    let decoded = decode_one_jp2k_pixels(&job, false, None, None).expect("CPU fallback");
    let TilePixels::Cpu(tile) = decoded else {
        panic!("missing Metal session must use the CPU fallback");
    };
    assert_eq!(
        (tile.width, tile.height),
        (job.expected_width, job.expected_height)
    );

    let error = decode_one_jp2k_pixels(&job, true, None, None)
        .expect_err("required Metal output needs a session");
    assert!(matches!(
        error,
        WsiError::Unsupported { reason } if reason.contains("without Metal session")
    ));
}

#[cfg(feature = "metal")]
#[test]
fn metal_jp2k_batch_without_session_preserves_empty_and_cpu_fallback_results() {
    assert!(decode_batch_jp2k_pixels(&[], false, None, None).is_empty());

    let jobs = [
        rgb_metal_job(J2kBackendRequest::Auto),
        rgb_metal_job(J2kBackendRequest::Auto),
    ];
    let decoded = decode_batch_jp2k_pixels(&jobs, false, None, None);

    assert_eq!(decoded.len(), jobs.len());
    for (decoded, job) in decoded.into_iter().zip(jobs) {
        let TilePixels::Cpu(tile) = decoded.expect("CPU fallback") else {
            panic!("missing Metal session must use the CPU fallback");
        };
        assert_eq!(
            (tile.width, tile.height),
            (job.expected_width, job.expected_height)
        );
    }
}

#[cfg(feature = "metal")]
#[test]
fn disabled_metal_jp2k_batch_still_decodes_each_request_on_device() {
    let Some(sessions) = test_metal_sessions() else {
        return;
    };
    let jobs = [
        rgb_metal_job(J2kBackendRequest::Metal),
        rgb_metal_job(J2kBackendRequest::Metal),
    ];
    let previous = std::env::var_os("WSI_RS_JP2K_DEVICE_BATCH");
    std::env::set_var("WSI_RS_JP2K_DEVICE_BATCH", "off");

    let decoded = decode_jp2k_tile_batch_to_pixels(&jobs, true, Some(&sessions));

    if let Some(previous) = previous {
        std::env::set_var("WSI_RS_JP2K_DEVICE_BATCH", previous);
    } else {
        std::env::remove_var("WSI_RS_JP2K_DEVICE_BATCH");
    }
    let decoded = decoded.expect("sequential Metal decode");
    assert_eq!(decoded.len(), jobs.len());
    assert!(decoded
        .into_iter()
        .all(|tile| matches!(tile, TilePixels::Device(DeviceTile::Metal(_)))));
}

#[cfg(feature = "metal")]
#[test]
fn jp2k_device_batch_flag_defaults_to_enabled_with_disable_escape_hatch() {
    assert!(parse_jp2k_device_batch_flag(None));
    assert!(!parse_jp2k_device_batch_flag(Some("0")));
    assert!(!parse_jp2k_device_batch_flag(Some("false")));
    assert!(!parse_jp2k_device_batch_flag(Some("OFF")));
    assert!(!parse_jp2k_device_batch_flag(Some("no")));
    assert!(parse_jp2k_device_batch_flag(Some("1")));
    assert!(parse_jp2k_device_batch_flag(Some("true")));
    assert!(parse_jp2k_device_batch_flag(Some("ON")));
    assert!(parse_jp2k_device_batch_flag(Some("yes")));
}
