use super::*;
use crate::decode::jp2k_codestream::parse_codestream_header;
use crate::test_support::assert_cpu_tile_matches_rgb_fixture_with_tolerance;
use image::{DynamicImage, ImageFormat, RgbaImage};
use std::io::Cursor;

fn load_fixture_rgb(ppm_bytes: &[u8]) -> image::RgbImage {
    match image::load(Cursor::new(ppm_bytes), ImageFormat::Pnm).unwrap() {
        DynamicImage::ImageRgb8(image) => image,
        other => other.to_rgb8(),
    }
}

const MAX_CHANNEL_DELTA: u8 = 50;
const MAX_AVG_CHANNEL_DELTA_X100: u64 = 1600;

#[cfg(feature = "metal")]
fn test_metal_sessions() -> Option<crate::output::metal::MetalBackendSessions> {
    let device = metal::Device::system_default()?;
    Some(crate::output::metal::MetalBackendSessions::new(device))
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
fn require_cuda_jp2k_without_session_returns_unsupported() {
    let codestream = include_bytes!("../../../tests/fixtures/jp2k/rgb_nomct.j2k");
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
    let codestream = include_bytes!("../../../tests/fixtures/jp2k/rgb_nomct.j2k");
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

fn assert_rgba_matches_rgb_fixture(decoded: &RgbaImage, expected_rgb: &image::RgbImage) {
    assert_eq!(decoded.width(), expected_rgb.width());
    assert_eq!(decoded.height(), expected_rgb.height());

    let mut total_delta = 0u64;
    let mut max_delta = 0u8;
    let mut channels = 0u64;

    for (decoded_pixel, expected_pixel) in decoded.pixels().zip(expected_rgb.pixels()) {
        for channel in 0..3 {
            let delta = decoded_pixel.0[channel].abs_diff(expected_pixel.0[channel]);
            total_delta += u64::from(delta);
            max_delta = max_delta.max(delta);
            channels += 1;
        }
        assert_eq!(decoded_pixel.0[3], 255);
    }

    let avg_delta_x100 = (total_delta * 100).checked_div(channels).unwrap_or(0);

    assert!(
        max_delta <= MAX_CHANNEL_DELTA,
        "JP2K decode drift too large: max channel delta {max_delta} > {MAX_CHANNEL_DELTA}",
    );
    assert!(
        avg_delta_x100 <= MAX_AVG_CHANNEL_DELTA_X100,
        "JP2K decode drift too large: average channel delta {:.2} > {:.2}",
        avg_delta_x100 as f64 / 100.0,
        MAX_AVG_CHANNEL_DELTA_X100 as f64 / 100.0,
    );
}

fn assert_sample_buffer_matches_rgb_fixture(image: &CpuTile, expected_rgb: &image::RgbImage) {
    assert_cpu_tile_matches_rgb_fixture_with_tolerance(
        image,
        expected_rgb,
        MAX_CHANNEL_DELTA,
        MAX_AVG_CHANNEL_DELTA_X100,
        "JP2K decode",
    );
}

fn assert_fixture_decodes_to_expected(
    codestream: &[u8],
    expected_ppm: &[u8],
    colorspace: Jp2kColorSpace,
) {
    let header = parse_codestream_header(codestream).unwrap();
    let expected = load_fixture_rgb(expected_ppm);
    let decoded = decode_jp2k(
        codestream,
        header.image_width,
        header.image_height,
        colorspace,
    )
    .unwrap();
    assert_rgba_matches_rgb_fixture(&decoded, &expected);
}

#[test]
fn decode_jp2k_rejects_empty_data() {
    let result = decode_jp2k(&[], 8, 8, Jp2kColorSpace::Rgb);
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("empty"), "unexpected error: {msg}");
}

#[test]
fn decode_jp2k_rejects_invalid_data() {
    let result = decode_jp2k(&[0xFF; 100], 8, 8, Jp2kColorSpace::Rgb);
    assert!(result.is_err());
}

#[test]
fn decode_jp2k_rejects_truncated_stream() {
    let mut buf = vec![0xFF, 0x4F, 0xFF, 0x51];
    buf.extend_from_slice(&[0x00; 50]);
    let result = decode_jp2k(&buf, 8, 8, Jp2kColorSpace::Rgb);
    assert!(result.is_err());
}

#[test]
fn colorspace_enum_values() {
    assert_ne!(Jp2kColorSpace::Rgb, Jp2kColorSpace::YCbCr);
    assert_eq!(Jp2kColorSpace::Rgb, Jp2kColorSpace::Rgb);
}

#[test]
fn dimensions_from_bounds_respects_origin_offsets() {
    assert_eq!(dimensions_from_bounds(10, 18, 20, 32), Some((8, 12)));
    assert_eq!(dimensions_from_bounds(5, 4, 0, 1), None);
}

#[test]
fn fixture_rgb_nomct_decodes_to_reference_rgb() {
    let codestream = include_bytes!("../../../tests/fixtures/jp2k/rgb_nomct.j2k");
    let expected = include_bytes!("../../../tests/fixtures/jp2k/rgb_nomct.ppm");
    assert_fixture_decodes_to_expected(codestream, expected, Jp2kColorSpace::Rgb);
}

#[test]
fn fixture_rgb_nomct_sample_buffer_matches_rgba_decode_exactly() {
    let codestream = include_bytes!("../../../tests/fixtures/jp2k/rgb_nomct.j2k");
    let header = parse_codestream_header(codestream).unwrap();

    for (expected_width, expected_height) in [
        (header.image_width, header.image_height),
        (header.image_width, header.image_height - 1),
    ] {
        let rgba = decode_jp2k(
            codestream,
            expected_width,
            expected_height,
            Jp2kColorSpace::Rgb,
        )
        .unwrap();
        let sample = decode_jp2k_to_sample_buffer(
            codestream,
            expected_width,
            expected_height,
            Jp2kColorSpace::Rgb,
        )
        .unwrap();

        assert_eq!(sample.width, expected_width);
        assert_eq!(sample.height, expected_height);

        let sample_rgb = sample.data.as_u8().unwrap();
        let expected_rgb: Vec<u8> = rgba
            .pixels()
            .flat_map(|pixel| {
                assert_eq!(pixel.0[3], 255);
                [pixel.0[0], pixel.0[1], pixel.0[2]]
            })
            .collect();

        assert_eq!(sample_rgb, expected_rgb.as_slice());
    }
}

#[cfg(feature = "metal")]
#[test]
fn fixture_rgb_device_batch_returns_metal_tiles() {
    let Some(sessions) = test_metal_sessions() else {
        eprintln!("skipping JP2K device batch test: no Metal device");
        return;
    };
    let codestream = include_bytes!("../../../tests/fixtures/jp2k/rgb_nomct.j2k");
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

    let decoded = decode_jp2k_tile_batch_to_device_pixels(&requests, false, &sessions).unwrap();

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
    let codestream = include_bytes!("../../../tests/fixtures/jp2k/ycbcr_444.j2k");
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
    let crate::output::metal::MetalDeviceStorage::Resident { image } = &tile.storage else {
        panic!("converted JP2K tile must be resident");
    };
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
    let codestream = include_bytes!("../../../tests/fixtures/jp2k/ycbcr_444.j2k");
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

    let decoded = decode_jp2k_tile_batch_to_device_pixels(&requests, true, &sessions).unwrap();

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

#[test]
fn tile_batch_decodes_in_submission_order_with_cpu_fallback_policy() {
    let first_codestream = include_bytes!("../../../tests/fixtures/jp2k/ycbcr_420.j2k");
    let first_header = parse_codestream_header(first_codestream).unwrap();
    let first_expected =
        load_fixture_rgb(include_bytes!("../../../tests/fixtures/jp2k/ycbcr_420.ppm"));
    let second_codestream = include_bytes!("../../../tests/fixtures/jp2k/rgb_nomct.j2k");
    let second_header = parse_codestream_header(second_codestream).unwrap();
    let second_expected =
        load_fixture_rgb(include_bytes!("../../../tests/fixtures/jp2k/rgb_nomct.ppm"));

    let requests = [
        Jp2kDecodeJob {
            data: Cow::Borrowed(first_codestream),
            expected_width: first_header.image_width,
            expected_height: first_header.image_height,
            rgb_color_space: false,
            backend: J2kBackendRequest::Cpu,
        },
        Jp2kDecodeJob {
            data: Cow::Borrowed(second_codestream),
            expected_width: second_header.image_width,
            expected_height: second_header.image_height,
            rgb_color_space: true,
            backend: J2kBackendRequest::Cpu,
        },
    ];

    let decoded = decode_jp2k_tile_batch_to_sample_buffers(&requests).unwrap();

    assert_eq!(decoded.len(), 2);
    assert_sample_buffer_matches_rgb_fixture(&decoded[0], &first_expected);
    assert_sample_buffer_matches_rgb_fixture(&decoded[1], &second_expected);
}

#[test]
fn rgb_tile_batch_j2k_helper_decodes_in_submission_order() {
    let codestream = include_bytes!("../../../tests/fixtures/jp2k/rgb_nomct.j2k");
    let header = parse_codestream_header(codestream).unwrap();
    let expected = load_fixture_rgb(include_bytes!("../../../tests/fixtures/jp2k/rgb_nomct.ppm"));

    let requests = [
        Jp2kDecodeJob {
            data: Cow::Borrowed(codestream),
            expected_width: header.image_width,
            expected_height: header.image_height,
            rgb_color_space: true,
            backend: J2kBackendRequest::Cpu,
        },
        Jp2kDecodeJob {
            data: Cow::Borrowed(codestream),
            expected_width: header.image_width,
            expected_height: header.image_height,
            rgb_color_space: true,
            backend: J2kBackendRequest::Cpu,
        },
    ];

    let decoded = decode_jp2k_tile_batch_with_j2k(&requests).unwrap();

    assert_eq!(decoded.len(), 2);
    assert_sample_buffer_matches_rgb_fixture(&decoded[0], &expected);
    assert_sample_buffer_matches_rgb_fixture(&decoded[1], &expected);
}

#[test]
fn j2k_cpu_batch_fast_path_decodes_in_submission_order() {
    let codestream = include_bytes!("../../../tests/fixtures/jp2k/rgb_nomct.j2k");
    let header = parse_codestream_header(codestream).unwrap();
    let expected = load_fixture_rgb(include_bytes!("../../../tests/fixtures/jp2k/rgb_nomct.ppm"));
    let runtime =
        DecodeRuntime::new(crate::core::decode_runtime::DecodeExecutionOptions::default()).unwrap();

    let requests = [
        Jp2kDecodeJob {
            data: Cow::Borrowed(codestream),
            expected_width: header.image_width,
            expected_height: header.image_height,
            rgb_color_space: true,
            backend: J2kBackendRequest::Cpu,
        },
        Jp2kDecodeJob {
            data: Cow::Borrowed(codestream),
            expected_width: header.image_width,
            expected_height: header.image_height,
            rgb_color_space: true,
            backend: J2kBackendRequest::Cpu,
        },
    ];

    let decoded = try_decode_batch_jp2k_with_j2k(&requests, &runtime)
        .expect("valid CPU JP2K jobs should take the j2k batch fast path");

    assert_eq!(decoded.len(), 2);
    assert_sample_buffer_matches_rgb_fixture(&decoded[0], &expected);
    assert_sample_buffer_matches_rgb_fixture(&decoded[1], &expected);
}

#[test]
fn materialize_jp2k_batch_outputs_preserves_order_and_converts_ycbcr() {
    let prepared = vec![
        PreparedJp2kBatchJob {
            decoded_width: 1,
            decoded_height: 1,
            expected_width: 1,
            expected_height: 1,
            output_colorspace: Jp2kColorSpace::YCbCr,
            row_bytes: 3,
            output_len: 3,
        },
        PreparedJp2kBatchJob {
            decoded_width: 1,
            decoded_height: 1,
            expected_width: 1,
            expected_height: 1,
            output_colorspace: Jp2kColorSpace::Rgb,
            row_bytes: 3,
            output_len: 3,
        },
    ];
    let outputs = vec![vec![128, 128, 128], vec![10, 20, 30]];
    let runtime =
        DecodeRuntime::new(crate::core::decode_runtime::DecodeExecutionOptions::default()).unwrap();

    let decoded = materialize_jp2k_batch_outputs(prepared, outputs, &runtime).unwrap();

    assert_eq!(decoded[0].data.as_u8().unwrap(), &[128, 128, 128]);
    assert_eq!(decoded[1].data.as_u8().unwrap(), &[10, 20, 30]);
}

#[test]
fn decode_batch_jp2k_preserves_order_and_per_tile_results() {
    let first_codestream = include_bytes!("../../../tests/fixtures/jp2k/rgb_nomct.j2k");
    let first_header = parse_codestream_header(first_codestream).unwrap();
    let second_codestream = include_bytes!("../../../tests/fixtures/jp2k/ycbcr_420.j2k");
    let second_header = parse_codestream_header(second_codestream).unwrap();
    let jobs = [
        Jp2kDecodeJob {
            data: Cow::Borrowed(first_codestream),
            expected_width: first_header.image_width,
            expected_height: first_header.image_height,
            rgb_color_space: true,
            backend: J2kBackendRequest::Cpu,
        },
        Jp2kDecodeJob {
            data: Cow::Borrowed(second_codestream),
            expected_width: second_header.image_width,
            expected_height: second_header.image_height,
            rgb_color_space: false,
            backend: J2kBackendRequest::Cpu,
        },
    ];

    let decoded = decode_batch_jp2k(&jobs);

    assert_eq!(decoded.len(), 2);
    assert!(decoded[0].is_ok());
    assert!(decoded[1].is_ok());
    assert_eq!(decoded[0].as_ref().unwrap().width, first_header.image_width);
    assert_eq!(
        decoded[1].as_ref().unwrap().width,
        second_header.image_width
    );
}

#[test]
fn decode_batch_jp2k_reports_malformed_tile_without_losing_good_tiles() {
    let codestream = include_bytes!("../../../tests/fixtures/jp2k/rgb_nomct.j2k");
    let header = parse_codestream_header(codestream).unwrap();
    let jobs = [
        Jp2kDecodeJob {
            data: Cow::Borrowed(codestream),
            expected_width: header.image_width,
            expected_height: header.image_height,
            rgb_color_space: true,
            backend: J2kBackendRequest::Cpu,
        },
        Jp2kDecodeJob {
            data: Cow::Borrowed(b"not j2k"),
            expected_width: header.image_width,
            expected_height: header.image_height,
            rgb_color_space: true,
            backend: J2kBackendRequest::Cpu,
        },
    ];

    let decoded = decode_batch_jp2k(&jobs);

    assert_eq!(decoded.len(), 2);
    assert!(decoded[0].is_ok());
    assert!(decoded[1].is_err());
}

#[test]
fn fixture_rgb_mct_decodes_with_ycbcr_hint() {
    let codestream = include_bytes!("../../../tests/fixtures/jp2k/rgb_mct.j2k");
    let expected = include_bytes!("../../../tests/fixtures/jp2k/rgb_mct.ppm");
    assert_fixture_decodes_to_expected(codestream, expected, Jp2kColorSpace::YCbCr);
}

#[test]
fn fixture_ycbcr_444_decodes_to_reference_rgb() {
    let codestream = include_bytes!("../../../tests/fixtures/jp2k/ycbcr_444.j2k");
    let expected = include_bytes!("../../../tests/fixtures/jp2k/ycbcr_444.ppm");
    assert_fixture_decodes_to_expected(codestream, expected, Jp2kColorSpace::YCbCr);
}

#[test]
fn fixture_ycbcr_422_decodes_to_reference_rgb() {
    let codestream = include_bytes!("../../../tests/fixtures/jp2k/ycbcr_422.j2k");
    let expected = include_bytes!("../../../tests/fixtures/jp2k/ycbcr_422.ppm");
    assert_fixture_decodes_to_expected(codestream, expected, Jp2kColorSpace::YCbCr);
}

#[test]
fn fixture_ycbcr_420_decodes_to_reference_rgb() {
    let codestream = include_bytes!("../../../tests/fixtures/jp2k/ycbcr_420.j2k");
    let expected = include_bytes!("../../../tests/fixtures/jp2k/ycbcr_420.ppm");
    assert_fixture_decodes_to_expected(codestream, expected, Jp2kColorSpace::YCbCr);
}
