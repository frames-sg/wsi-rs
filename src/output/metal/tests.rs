use std::sync::Arc;
use std::time::{Duration, Instant};

use super::ycbcr::{YcbcrAddressPlan, YcbcrAddressWidth, YcbcrToRgb8Params, YCBCR_TO_RGB8_METAL};
use super::*;
use crate::{error::WsiError, PixelFormat};

#[test]
fn ycbcr_to_rgb8_converter_is_cached_per_backend_sessions() {
    let Some(device) = metal::Device::system_default() else {
        eprintln!("skipping Metal converter cache test: no Metal device");
        return;
    };
    let sessions = MetalBackendSessions::new(device);

    let first = sessions
        .ycbcr_to_rgb8_converter()
        .expect("first YCbCr converter");
    let second = sessions
        .ycbcr_to_rgb8_converter()
        .expect("second YCbCr converter");

    assert!(Arc::ptr_eq(&first, &second));
}

#[test]
fn ycbcr_to_rgb8_tiles_converts_batch_with_one_cached_converter() {
    let Some(device) = metal::Device::system_default() else {
        eprintln!("skipping Metal batch conversion test: no Metal device");
        return;
    };
    let sessions = MetalBackendSessions::new(device.clone());
    let tiles = [
        ycbcr_test_tile(&device, &[10, 128, 128, 200, 128, 128]),
        ycbcr_test_tile(&device, &[30, 128, 128, 40, 128, 128]),
    ];

    let converted = sessions
        .ycbcr8_tiles_to_rgb8(&tiles)
        .expect("batch YCbCr conversion");

    assert_eq!(converted.len(), 2);
    for (tile, expected) in converted
        .iter()
        .zip([[10, 10, 10, 200, 200, 200], [30, 30, 30, 40, 40, 40]])
    {
        assert_eq!((tile.width, tile.height), (2, 1));
        assert_eq!(tile.pitch_bytes, 6);
        assert_eq!(tile.format, PixelFormat::Rgb8);
        let MetalDeviceStorage::Resident { image } = &tile.storage else {
            panic!("YCbCr conversion must return resident storage");
        };
        assert_eq!(image.byte_offset(), 0);
        assert_eq!(image.byte_len(), 6);
        assert_eq!(interop::resident_bytes(image), expected);
    }
    let first = sessions
        .ycbcr_to_rgb8_converter()
        .expect("cached converter after batch");
    let second = sessions
        .ycbcr_to_rgb8_converter()
        .expect("cached converter after batch");
    assert!(Arc::ptr_eq(&first, &second));
}

fn ycbcr_test_tile(device: &metal::Device, bytes: &[u8]) -> MetalDeviceTile {
    MetalDeviceTile::from_resident(interop::resident_test_image(device, bytes, (2, 1), 6))
        .expect("resident test tile")
}

#[test]
#[allow(deprecated)]
fn ycbcr_conversion_rejects_legacy_raw_buffer_storage() {
    let Some(device) = metal::Device::system_default() else {
        return;
    };
    let sessions = MetalBackendSessions::new(device.clone());
    let tile = MetalDeviceTile {
        width: 1,
        height: 1,
        pitch_bytes: 3,
        format: PixelFormat::Rgb8,
        storage: MetalDeviceStorage::Buffer {
            buffer: device.new_buffer(3, metal::MTLResourceOptions::StorageModeShared),
            byte_offset: 0,
        },
    };

    let error = sessions
        .ycbcr8_tiles_to_rgb8(&[tile])
        .expect_err("legacy raw buffer must be rejected");
    assert!(matches!(error, WsiError::Unsupported { .. }));
}

#[test]
#[allow(deprecated)]
fn resident_accessor_rejects_legacy_raw_buffer_storage_directly() {
    let Some(device) = metal::Device::system_default() else {
        return;
    };
    let tile = MetalDeviceTile {
        width: 1,
        height: 1,
        pitch_bytes: 3,
        format: PixelFormat::Rgb8,
        storage: MetalDeviceStorage::Buffer {
            buffer: device.new_buffer(3, metal::MTLResourceOptions::StorageModeShared),
            byte_offset: 0,
        },
    };

    let error = tile
        .validated_resident_image()
        .expect_err("legacy storage is not resident");
    assert!(matches!(error, WsiError::Unsupported { .. }));
    assert!(error.to_string().contains("explicitly adopted"));
}

#[test]
fn resident_device_validation_rejects_a_different_metal_device_when_available() {
    let devices = metal::Device::all();
    let Some(source_device) = devices.first() else {
        return;
    };
    let Some(other_device) = devices
        .iter()
        .find(|device| device.registry_id() != source_device.registry_id())
    else {
        return;
    };
    let tile = ycbcr_test_tile(source_device, &[16, 128, 128, 32, 128, 128]);

    let error = tile
        .resident_image_for_device(other_device)
        .expect_err("a resident image cannot cross Metal devices");
    assert!(matches!(error, WsiError::Codec { .. }));
}

#[test]
fn ycbcr_conversion_rejects_resident_metadata_mismatch() {
    let Some(device) = metal::Device::system_default() else {
        return;
    };
    let sessions = MetalBackendSessions::new(device.clone());
    let mut tile = ycbcr_test_tile(&device, &[16, 128, 128, 32, 128, 128]);
    tile.pitch_bytes += 1;

    let error = sessions
        .ycbcr8_tiles_to_rgb8(&[tile])
        .expect_err("public tile metadata must match the resident image");

    assert!(matches!(error, WsiError::Unsupported { .. }));
    assert!(error.to_string().contains("metadata"));
}

#[test]
fn ycbcr_address_plan_crosses_u32_without_wrapping() {
    let at_u32 = YcbcrAddressPlan::new(1, 2, u32::MAX as usize - 2, usize::MAX)
        .expect("last byte exactly at u32::MAX");
    let above_u32 = YcbcrAddressPlan::new(1, 2, u32::MAX as usize, usize::MAX)
        .expect("last byte above u32::MAX");

    assert_eq!(
        YcbcrAddressPlan::max_byte(1, 2, at_u32.src_pitch).expect("checked index"),
        u64::from(u32::MAX)
    );
    assert_eq!(
        YcbcrAddressPlan::max_byte(1, 2, above_u32.src_pitch).expect("checked index"),
        u64::from(u32::MAX) + 2
    );
    assert_eq!(at_u32.address_width, YcbcrAddressWidth::U32);
    assert_eq!(above_u32.address_width, YcbcrAddressWidth::U64);
}

#[test]
fn metal_address_probe_returns_the_checked_64_bit_indices() {
    let Some(device) = metal::Device::system_default() else {
        return;
    };
    let options = metal::CompileOptions::new();
    let source = format!(
        "{YCBCR_TO_RGB8_METAL}\n{}",
        include_str!("ycbcr_probe.metal")
    );
    let library = device
        .new_library_with_source(&source, &options)
        .expect("compile YCbCr address probe");
    let function = library
        .get_function("wsi_rs_ycbcr8_address_probe", None)
        .expect("load YCbCr address probe");
    let pipeline = device
        .new_compute_pipeline_state_with_function(&function)
        .expect("create YCbCr address probe pipeline");
    let queue = device.new_command_queue();
    let command_buffer = j2k_metal_support::checked_command_buffer(&queue)
        .expect("create address probe command buffer");
    let output = j2k_metal_support::checked_shared_buffer_for_len::<u64>(&device, 2)
        .expect("allocate address probe output");
    let params = YcbcrToRgb8Params {
        width: 1,
        height: 2,
        src_pitch: u32::MAX,
        dst_pitch: u32::MAX - 2,
    };
    let coordinate = metal::MTLSize {
        width: 0,
        height: 1,
        depth: 0,
    };
    let encoder = command_buffer.new_compute_command_encoder();
    encoder.set_compute_pipeline_state(&pipeline);
    encoder.set_buffer(0, Some(&output), 0);
    encoder.set_bytes(
        1,
        core::mem::size_of_val(&params) as u64,
        std::ptr::from_ref(&params).cast(),
    );
    let coordinate = [coordinate.width as u32, coordinate.height as u32];
    encoder.set_bytes(
        2,
        core::mem::size_of_val(&coordinate) as u64,
        coordinate.as_ptr().cast(),
    );
    encoder.dispatch_threads(
        metal::MTLSize {
            width: 1,
            height: 1,
            depth: 1,
        },
        metal::MTLSize {
            width: 1,
            height: 1,
            depth: 1,
        },
    );
    encoder.end_encoding();
    command_buffer.commit();
    command_buffer.wait_until_completed();
    j2k_metal_support::ensure_completed(&command_buffer).expect("address probe completion");

    assert_eq!(
        interop::u64_buffer_values(&output, 2),
        [u64::from(u32::MAX), u64::from(u32::MAX - 2)]
    );
}

#[test]
fn ycbcr_address_plan_accounts_for_padded_pitch_and_last_pixel() {
    let plan = YcbcrAddressPlan::new(7, 5, 64, 64 * 5).expect("padded source plan");

    assert_eq!(plan.dst_pitch, 21);
    assert_eq!(plan.dst_len, 105);
    assert_eq!(
        YcbcrAddressPlan::max_byte(7, 5, plan.src_pitch).expect("last source byte"),
        276
    );
    assert_eq!(
        YcbcrAddressPlan::max_byte(7, 5, plan.dst_pitch as u32).expect("last destination byte"),
        104
    );
}

#[test]
fn ycbcr_address_plan_rejects_short_source_span() {
    let error = YcbcrAddressPlan::new(7, 5, 64, 276)
        .expect_err("source must include the last addressed byte");

    assert!(matches!(error, WsiError::Unsupported { .. }));
    assert!(error.to_string().contains("source span"));
}

#[test]
fn resident_validation_rejects_every_mutable_compatibility_mirror() {
    let Some(device) = metal::Device::system_default() else {
        return;
    };
    let original = ycbcr_test_tile(&device, &[16, 128, 128, 32, 128, 128]);
    let mut cases = Vec::new();

    let mut width = original.clone();
    width.width += 1;
    cases.push(width);
    let mut height = original.clone();
    height.height += 1;
    cases.push(height);
    let mut pitch = original.clone();
    pitch.pitch_bytes += 1;
    cases.push(pitch);
    let mut format = original;
    format.format = PixelFormat::Rgba8;
    cases.push(format);

    for tile in cases {
        let error = tile
            .validated_resident_image()
            .expect_err("mutated compatibility metadata must be rejected");
        assert!(matches!(error, WsiError::Unsupported { .. }));
        assert!(error.to_string().contains("metadata"));
    }
}

#[test]
#[ignore = "run explicitly in release mode for the three-run Metal address-width gate"]
fn ycbcr_selected_u32_stays_within_five_percent_of_reference() {
    const DIMENSION: u32 = 2_048;
    const DISPATCHES_PER_SAMPLE: usize = 12;
    const SAMPLE_COUNT: usize = 3;

    let Some(device) = metal::Device::system_default() else {
        return;
    };
    let source = format!(
        "{YCBCR_TO_RGB8_METAL}\n{}",
        include_str!("ycbcr_perf.metal")
    );
    let library = device
        .new_library_with_source(&source, &metal::CompileOptions::new())
        .expect("compile YCbCr address performance kernels");
    let pipeline = |name| {
        let function = library
            .get_function(name, None)
            .expect("load YCbCr address performance function");
        device
            .new_compute_pipeline_state_with_function(&function)
            .expect("create YCbCr address performance pipeline")
    };
    let reference_pipeline = pipeline("wsi_rs_ycbcr8_to_rgb8_u32_perf_reference");
    let selected_u32_pipeline = pipeline("wsi_rs_ycbcr8_to_rgb8_u32");
    let u64_pipeline = pipeline("wsi_rs_ycbcr8_to_rgb8");
    let pitch = DIMENSION * 3;
    let byte_len = usize::try_from(pitch)
        .expect("pitch fits usize")
        .checked_mul(usize::try_from(DIMENSION).expect("height fits usize"))
        .expect("performance buffer length");
    let src = j2k_metal_support::checked_shared_buffer_for_len::<u8>(&device, byte_len)
        .expect("allocate performance source");
    let dst = j2k_metal_support::checked_shared_buffer_for_len::<u8>(&device, byte_len)
        .expect("allocate performance destination");
    let params = YcbcrToRgb8Params {
        width: DIMENSION,
        height: DIMENSION,
        src_pitch: pitch,
        dst_pitch: pitch,
    };
    let queue = device.new_command_queue();

    let measure = |pipeline: &metal::ComputePipelineStateRef, dispatches: usize| {
        let command_buffer = j2k_metal_support::checked_command_buffer(&queue)
            .expect("create performance command buffer");
        let thread_width = pipeline.thread_execution_width().max(1);
        let max_threads = pipeline
            .max_total_threads_per_threadgroup()
            .max(thread_width);
        let thread_height = (max_threads / thread_width).max(1);
        let started = Instant::now();
        for _ in 0..dispatches {
            let encoder = command_buffer.new_compute_command_encoder();
            encoder.set_compute_pipeline_state(pipeline);
            encoder.set_buffer(0, Some(&src), 0);
            encoder.set_buffer(1, Some(&dst), 0);
            encoder.set_bytes(
                2,
                core::mem::size_of_val(&params) as u64,
                std::ptr::from_ref(&params).cast(),
            );
            encoder.dispatch_threads(
                metal::MTLSize {
                    width: u64::from(DIMENSION),
                    height: u64::from(DIMENSION),
                    depth: 1,
                },
                metal::MTLSize {
                    width: thread_width,
                    height: thread_height,
                    depth: 1,
                },
            );
            encoder.end_encoding();
        }
        command_buffer.commit();
        command_buffer.wait_until_completed();
        j2k_metal_support::ensure_completed(&command_buffer)
            .expect("complete YCbCr address performance sample");
        started.elapsed()
    };

    measure(&reference_pipeline, 2);
    measure(&selected_u32_pipeline, 2);
    measure(&u64_pipeline, 2);
    let mut reference_samples = Vec::with_capacity(SAMPLE_COUNT);
    let mut selected_u32_samples = Vec::with_capacity(SAMPLE_COUNT);
    let mut u64_samples = Vec::with_capacity(SAMPLE_COUNT);
    for sample in 0..SAMPLE_COUNT {
        if sample % 2 == 0 {
            reference_samples.push(measure(&reference_pipeline, DISPATCHES_PER_SAMPLE));
            selected_u32_samples.push(measure(&selected_u32_pipeline, DISPATCHES_PER_SAMPLE));
            u64_samples.push(measure(&u64_pipeline, DISPATCHES_PER_SAMPLE));
        } else {
            u64_samples.push(measure(&u64_pipeline, DISPATCHES_PER_SAMPLE));
            selected_u32_samples.push(measure(&selected_u32_pipeline, DISPATCHES_PER_SAMPLE));
            reference_samples.push(measure(&reference_pipeline, DISPATCHES_PER_SAMPLE));
        }
    }

    let median = |samples: &mut Vec<Duration>| {
        samples.sort_unstable();
        samples[samples.len() / 2]
    };
    let reference_median = median(&mut reference_samples);
    let selected_u32_median = median(&mut selected_u32_samples);
    let u64_median = median(&mut u64_samples);
    let selected_ratio = selected_u32_median.as_secs_f64() / reference_median.as_secs_f64();
    let u64_ratio = u64_median.as_secs_f64() / reference_median.as_secs_f64();
    eprintln!(
        "Metal YCbCr address-width benchmark: reference={reference_median:?} selected_u32={selected_u32_median:?} u64={u64_median:?} selected_ratio={selected_ratio:.4} u64_ratio={u64_ratio:.4}"
    );
    assert!(
        selected_ratio <= 1.05,
        "selected u32 Metal YCbCr path regressed by more than 5%: ratio={selected_ratio:.4}"
    );
}
