#[cfg(all(feature = "metal", target_os = "macos"))]
use super::device::{
    jpeg_device_batch_attempts_for_test, reset_jpeg_device_batch_attempts_for_test,
};
use super::input::{ensure_jpeg_eoi, patch_jpeg_dimensions, try_decode_jpeg_rgb_scaled};
use super::*;
use jpeg_encoder::{ColorType as JpegColorType, Encoder as JpegEncoder};

fn encode_test_jpeg(img: &image::RgbImage) -> Vec<u8> {
    let mut encoded = Vec::new();
    JpegEncoder::new(&mut encoded, 90)
        .encode(
            img.as_raw().as_slice(),
            img.width() as u16,
            img.height() as u16,
            JpegColorType::Rgb,
        )
        .unwrap();
    encoded
}

#[cfg(feature = "cuda")]
fn cuda_unavailable_reason(reason: &str) -> bool {
    reason.contains("CUDA is unavailable") || reason.contains("CUDA runtime error")
}

#[cfg(feature = "cuda")]
fn baseline_cuda_jpeg_job() -> JpegDecodeJob<'static> {
    JpegDecodeJob {
        data: Cow::Borrowed(include_bytes!(
            "../../tests/fixtures/jpeg/baseline_420_16x16.jpg"
        )),
        tables: None,
        expected_width: 16,
        expected_height: 16,
        color_transform: SigninumColorTransform::Auto,
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
        SigninumBackendRequest::Cuda,
        true,
        None,
        Some(&sessions),
    );

    let decoded = match decoded {
        Ok(decoded) => decoded,
        Err(WsiError::Unsupported { reason })
            if cuda_unavailable_reason(&reason)
                && std::env::var_os("SIGNINUM_REQUIRE_CUDA_RUNTIME").is_none() =>
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
    let surface = tile
        .storage
        .jpeg_surface()
        .expect("CUDA JPEG storage must expose Signinum JPEG surface");
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
fn require_cuda_jpeg_without_session_returns_unsupported() {
    let err = decode_one_jpeg_pixels(
        &baseline_cuda_jpeg_job(),
        SigninumBackendRequest::Cuda,
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
        color_transform: SigninumColorTransform::Auto,
        force_dimensions: false,
        requested_size: None,
    };

    let err = decode_one_jpeg_pixels(
        &job,
        SigninumBackendRequest::Cuda,
        true,
        None,
        Some(&sessions),
    )
    .unwrap_err();

    let WsiError::Unsupported { reason } = err else {
        panic!("strict CUDA progressive JPEG must be Unsupported, got {err:?}");
    };
    assert!(
        reason.contains("Progressive8 JPEG") && reason.contains("CUDA"),
        "unexpected strict CUDA progressive JPEG error: {reason}"
    );
}

fn progressive_8x8_jpeg() -> Vec<u8> {
    const HEX: &str = concat!(
            "ffd8ffe000104a46494600010100000100010000ffdb0043000302020302020303030304030304050805050404050a07",
            "0706080c0a0c0c0b0a0b0b0d0e12100d0e110e0b0b1016101113141515150c0f171816141812141514ffdb0043010304",
            "0405040509050509140d0b0d141414141414141414141414141414141414141414141414141414141414141414141414",
            "1414141414141414141414141414ffc20011080008000803012200021101031101ffc400150001010000000000000000",
            "0000000000000006ffc4001501010100000000000000000000000000000506ffda000c0301000210031000000188136f",
            "7fffc4001410010000000000000000000000000000000000ffda00080101000105027fffc40014110100000000000000",
            "000000000000000000ffda0008010301013f017fffc40014110100000000000000000000000000000000ffda00080102",
            "01013f017fffc40014100100000000000000000000000000000000ffda0008010100063f027fffc40014100100000000",
            "000000000000000000000000ffda0008010100013f217fffda000c03010002000300000010f7ffc40014110100000000",
            "000000000000000000000000ffda0008010301013f107fffc40014110100000000000000000000000000000000ffda00",
            "08010201013f107fffc40014100100000000000000000000000000000000ffda0008010100013f107fffd9",
        );
    assert_eq!(HEX.len() % 2, 0);
    HEX.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = (pair[0] as char).to_digit(16).unwrap();
            let low = (pair[1] as char).to_digit(16).unwrap();
            ((high << 4) | low) as u8
        })
        .collect()
}

#[test]
fn decode_valid_jpeg() {
    let mut rgb = image::RgbImage::new(8, 8);
    for pixel in rgb.pixels_mut() {
        *pixel = image::Rgb([200, 100, 50]);
    }
    let jpeg_data = encode_test_jpeg(&rgb);
    let decoded = decode_jpeg(&jpeg_data, None, 8, 8).unwrap();
    assert_eq!(decoded.width(), 8);
    assert_eq!(decoded.height(), 8);
    // All alpha channels should be 255
    for pixel in decoded.pixels() {
        assert_eq!(pixel[3], 255);
    }
}

#[test]
fn decode_empty_data_fails() {
    let result = decode_jpeg(&[], None, 0, 0);
    assert!(result.is_err());
}

#[test]
fn decode_with_jpeg_tables() {
    // Create a valid JPEG
    let mut rgb = image::RgbImage::new(8, 8);
    for pixel in rgb.pixels_mut() {
        *pixel = image::Rgb([100, 150, 200]);
    }
    let jpeg_data = encode_test_jpeg(&rgb);

    // Find SOS marker (0xFF, 0xDA) to split into tables and scan data.
    // Tables = everything up to (but not including) SOS marker, plus EOI.
    // Data = SOI + SOS marker onward.
    let sos_pos = jpeg_data
        .windows(2)
        .position(|w| w == [0xFF, 0xDA])
        .expect("SOS marker not found");

    // tables: from start to just before SOS, with EOI appended
    let mut tables = jpeg_data[..sos_pos].to_vec();
    tables.extend_from_slice(&[0xFF, 0xD9]); // EOI

    // data: SOI + from SOS onward
    let mut data = vec![0xFF, 0xD8]; // SOI
    data.extend_from_slice(&jpeg_data[sos_pos..]);

    let decoded = decode_jpeg(&data, Some(&tables), 8, 8).unwrap();
    assert_eq!(decoded.width(), 8);
    assert_eq!(decoded.height(), 8);
    for pixel in decoded.pixels() {
        assert_eq!(pixel[3], 255);
    }
}

#[test]
fn decode_jpeg_rgb_returns_interleaved_rgb() {
    let mut rgb = image::RgbImage::new(4, 4);
    for (idx, pixel) in rgb.pixels_mut().enumerate() {
        *pixel = image::Rgb([idx as u8, 200, 50]);
    }
    let jpeg_data = encode_test_jpeg(&rgb);

    let decoded = decode_jpeg_rgb(&jpeg_data, None, 4, 4).unwrap();
    assert_eq!(decoded.width, 4);
    assert_eq!(decoded.height, 4);
    assert_eq!(decoded.pixels.len(), 4 * 4 * 3);
}

#[test]
fn decode_progressive_jpeg_rgb_returns_interleaved_rgb() {
    let jpeg_data = progressive_8x8_jpeg();

    let decoded = decode_jpeg_rgb(&jpeg_data, None, 8, 8).unwrap();

    assert_eq!(decoded.width, 8);
    assert_eq!(decoded.height, 8);
    assert_eq!(decoded.pixels.len(), 8 * 8 * 3);
}

#[test]
fn progressive_scaled_decode_falls_back_to_full_decode_resize() {
    let jpeg_data = progressive_8x8_jpeg();

    let decoded = decode_jpeg_rgb_with_size_override(
        &jpeg_data,
        None,
        8,
        8,
        Some(4),
        Some(4),
        SigninumColorTransform::Auto,
    )
    .unwrap();

    assert_eq!(decoded.width, 4);
    assert_eq!(decoded.height, 4);
    assert_eq!(decoded.pixels.len(), 4 * 4 * 3);
}

#[cfg(feature = "metal")]
#[test]
fn progressive_jpeg_device_route_uses_cpu_unless_device_is_required() {
    let jpeg_data = progressive_8x8_jpeg();
    let view = SigninumJpegView::parse_with_options(
        &jpeg_data,
        SigninumDecodeOptions::default().with_color_transform(SigninumColorTransform::Auto),
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

#[cfg(all(feature = "metal", target_os = "macos"))]
#[test]
fn private_metal_jpeg_decode_returns_private_device_tile() {
    let Some(device) = metal::Device::system_default() else {
        return;
    };
    let sessions =
        crate::output::metal::MetalBackendSessions::new(device).with_private_jpeg_decode();
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
        color_transform: SigninumColorTransform::Auto,
        force_dimensions: false,
        requested_size: None,
    };

    let pixels = decode_one_jpeg_pixels(
        &job,
        SigninumBackendRequest::Metal,
        true,
        Some(&sessions),
        None,
    )
    .expect("private JPEG Metal tile");
    let TilePixels::Device(DeviceTile::Metal(tile)) = pixels else {
        panic!("expected private Metal tile");
    };
    let crate::output::metal::MetalDeviceStorage::Buffer { buffer, .. } = tile.storage;
    assert_eq!(buffer.storage_mode(), metal::MTLStorageMode::Private);
    assert_eq!(tile.width, 16);
    assert_eq!(tile.height, 16);
}

#[cfg(all(feature = "metal", target_os = "macos"))]
#[test]
fn decode_batch_jpeg_pixels_uses_session_backed_device_batch() {
    let Some(device) = metal::Device::system_default() else {
        return;
    };
    let sessions = crate::output::metal::MetalBackendSessions::new(device);
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
            color_transform: SigninumColorTransform::Auto,
            force_dimensions: false,
            requested_size: None,
        },
        JpegDecodeJob {
            data: Cow::Borrowed(second_jpeg.as_slice()),
            tables: None,
            expected_width: 16,
            expected_height: 16,
            color_transform: SigninumColorTransform::Auto,
            force_dimensions: false,
            requested_size: None,
        },
    ];

    reset_jpeg_device_batch_attempts_for_test();
    let pixels = decode_batch_jpeg_pixels(
        &jobs,
        SigninumBackendRequest::Metal,
        true,
        Some(&sessions),
        None,
    );

    assert_eq!(jpeg_device_batch_attempts_for_test(), 1);
    assert_eq!(pixels.len(), 2);
    for pixels in pixels {
        assert!(matches!(pixels.unwrap(), TilePixels::Device(_)));
    }
}

#[test]
fn decode_jpeg_rgb_scaled_returns_scaled_dimensions() {
    let mut rgb = image::RgbImage::new(16, 16);
    for (idx, pixel) in rgb.pixels_mut().enumerate() {
        *pixel = image::Rgb([idx as u8, 100, 200]);
    }
    let jpeg_data = encode_test_jpeg(&rgb);

    let decoded = try_decode_jpeg_rgb_scaled(ScaledJpegDecode {
        data: &jpeg_data,
        tables: None,
        expected_width: 16,
        expected_height: 16,
        requested_width: 4,
        requested_height: 4,
        force_dimensions: false,
        color_transform: SigninumColorTransform::Auto,
    })
    .unwrap()
    .expect("power-of-two downscale should use signinum IDCT scale");

    assert_eq!(decoded.width, 4);
    assert_eq!(decoded.height, 4);
    assert_eq!(decoded.pixels.len(), 4 * 4 * 3);
}

#[test]
fn signinum_batch_fast_path_matches_single_tile_for_forced_color_transform() {
    let mut rgb = image::RgbImage::new(16, 16);
    for (idx, pixel) in rgb.pixels_mut().enumerate() {
        *pixel = image::Rgb([idx as u8, 100, 200]);
    }
    let jpeg_data = encode_test_jpeg(&rgb);
    let jobs = (0..4)
        .map(|_| JpegDecodeJob {
            data: Cow::Borrowed(jpeg_data.as_slice()),
            tables: None,
            expected_width: 16,
            expected_height: 16,
            color_transform: SigninumColorTransform::ForceRgb,
            force_dimensions: false,
            requested_size: None,
        })
        .collect::<Vec<_>>();

    let fast = try_decode_batch_jpeg_with_signinum(&jobs)
        .expect("forced color transform should use signinum batch fast path");
    let sequential = jobs.iter().map(decode_one_jpeg_job).collect::<Vec<_>>();

    assert_eq!(fast.len(), sequential.len());
    for (fast, sequential) in fast.into_iter().zip(sequential) {
        let fast = fast.unwrap();
        let sequential = sequential.unwrap();
        assert_eq!(fast.width, sequential.width);
        assert_eq!(fast.height, sequential.height);
        assert_eq!(fast.data.as_u8(), sequential.data.as_u8());
    }
}

#[test]
fn signinum_batch_fast_path_matches_single_tile_for_scaled_decode() {
    let mut rgb = image::RgbImage::new(16, 16);
    for (idx, pixel) in rgb.pixels_mut().enumerate() {
        *pixel = image::Rgb([idx as u8, 100, 200]);
    }
    let jpeg_data = encode_test_jpeg(&rgb);
    let jobs = (0..4)
        .map(|_| JpegDecodeJob {
            data: Cow::Borrowed(jpeg_data.as_slice()),
            tables: None,
            expected_width: 16,
            expected_height: 16,
            color_transform: SigninumColorTransform::ForceRgb,
            force_dimensions: false,
            requested_size: Some((4, 4)),
        })
        .collect::<Vec<_>>();

    let fast = try_decode_batch_jpeg_with_signinum(&jobs)
        .expect("scaled decode should use signinum batch fast path");
    let sequential = jobs.iter().map(decode_one_jpeg_job).collect::<Vec<_>>();

    assert_eq!(fast.len(), sequential.len());
    for (fast, sequential) in fast.into_iter().zip(sequential) {
        let fast = fast.unwrap();
        let sequential = sequential.unwrap();
        assert_eq!(fast.width, 4);
        assert_eq!(fast.height, 4);
        assert_eq!(fast.data.as_u8(), sequential.data.as_u8());
    }
}

#[test]
fn patch_jpeg_dimensions_overwrites_zero_sized_sof() {
    let jpeg = vec![
        0xFF, 0xD8, // SOI
        0xFF, 0xC0, // SOF0
        0x00, 0x11, // length
        0x08, // precision
        0x00, 0x00, // height
        0x00, 0x00, // width
        0x03, // components
        0x01, 0x11, 0x00, 0x02, 0x11, 0x00, 0x03, 0x11, 0x00,
    ];

    let patched = patch_jpeg_dimensions(&jpeg, 512, 256, false);
    let patched = patched.as_ref();
    assert_eq!(&patched[7..9], &256u16.to_be_bytes());
    assert_eq!(&patched[9..11], &512u16.to_be_bytes());

    // Original input is unchanged.
    assert_eq!(&jpeg[7..9], &[0, 0]);
    assert_eq!(&jpeg[9..11], &[0, 0]);
}

#[test]
fn patch_jpeg_dimensions_leaves_nonzero_sof_alone() {
    let jpeg = vec![
        0xFF, 0xD8, // SOI
        0xFF, 0xC0, // SOF0
        0x00, 0x11, // length
        0x08, // precision
        0x01, 0x00, // height
        0x02, 0x00, // width
        0x03, // components
        0x01, 0x11, 0x00, 0x02, 0x11, 0x00, 0x03, 0x11, 0x00,
    ];

    let patched = patch_jpeg_dimensions(&jpeg, 512, 256, false);
    assert!(matches!(patched, Cow::Borrowed(_)));
}

#[test]
fn patch_jpeg_dimensions_forces_nonzero_sof_when_requested() {
    let jpeg = vec![
        0xFF, 0xD8, // SOI
        0xFF, 0xC0, // SOF0
        0x00, 0x11, // length
        0x08, // precision
        0x00, 0x10, // height = 16
        0x04, 0x00, // width = 1024
        0x03, // components
        0x01, 0x11, 0x00, 0x02, 0x11, 0x00, 0x03, 0x11, 0x00,
    ];

    let patched = patch_jpeg_dimensions(&jpeg, 1024, 4, true);
    let patched = patched.as_ref();
    assert_eq!(&patched[7..9], &4u16.to_be_bytes());
    assert_eq!(&patched[9..11], &1024u16.to_be_bytes());
}

#[test]
fn ensure_jpeg_eoi_appends_missing_marker() {
    let jpeg = vec![0xFF, 0xD8, 0x00, 0x01];
    let repaired = ensure_jpeg_eoi(&jpeg);
    assert_eq!(
        repaired.as_ref()[repaired.as_ref().len() - 2..],
        [0xFF, 0xD9]
    );
}

#[test]
fn ensure_jpeg_eoi_keeps_valid_trailer() {
    let jpeg = vec![0xFF, 0xD8, 0xFF, 0xD9];
    let repaired = ensure_jpeg_eoi(&jpeg);
    assert!(matches!(repaired, Cow::Borrowed(_)));
}

#[test]
fn jpeg_tile_geometry_parses_dri_after_sof() {
    let jpeg = vec![
        0xFF, 0xD8, // SOI
        0xFF, 0xC0, // SOF0
        0x00, 0x11, // len
        0x08, // precision
        0x00, 0x08, // height
        0x00, 0x20, // width
        0x03, // components
        0x01, 0x22, 0x00, // h=2, v=2
        0x02, 0x11, 0x00, 0x03, 0x11, 0x00, 0xFF, 0xDD, // DRI
        0x00, 0x04, // len
        0x00, 0x02, // restart interval
        0xFF, 0xDA, // SOS
        0x00, 0x0C, 0x03, 0x01, 0x00, 0x02, 0x11, 0x03, 0x11, 0x00, 0x3F, 0x00,
    ];

    let geometry = jpeg_tile_geometry(&jpeg).unwrap();
    assert_eq!(geometry.width, 32);
    assert_eq!(geometry.height, 8);
    assert_eq!(geometry.tile_width, 32);
    assert_eq!(geometry.tile_height, 16);
}

#[test]
fn jpeg_tile_geometry_rejects_missing_restart_markers() {
    let jpeg = vec![
        0xFF, 0xD8, // SOI
        0xFF, 0xC0, // SOF0
        0x00, 0x11, // len
        0x08, // precision
        0x00, 0x08, // height
        0x00, 0x10, // width
        0x03, // components
        0x01, 0x11, 0x00, 0x02, 0x11, 0x00, 0x03, 0x11, 0x00, 0xFF, 0xDA, // SOS
        0x00, 0x0C, 0x03, 0x01, 0x00, 0x02, 0x11, 0x03, 0x11, 0x00, 0x3F, 0x00,
    ];

    let err = jpeg_tile_geometry(&jpeg).unwrap_err();
    assert!(err.to_string().contains("restart markers"));
}
