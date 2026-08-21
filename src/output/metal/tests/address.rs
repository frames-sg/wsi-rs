use super::*;
use objc2_metal::{MTLCommandEncoder, MTLComputeCommandEncoder};

#[test]
fn ycbcr_address_plan_crosses_u32_without_wrapping() {
    let at_u32 = YcbcrAddressPlan::new(1, 2, u32::MAX as usize - 2, usize::MAX)
        .expect("last byte exactly at u32::MAX");
    let above_u32 = YcbcrAddressPlan::new(1, 2, u32::MAX as usize, usize::MAX)
        .expect("last byte above u32::MAX");

    assert_eq!(
        YcbcrAddressPlan::max_byte(1, 2, at_u32.src_pitch),
        u64::from(u32::MAX)
    );
    assert_eq!(
        YcbcrAddressPlan::max_byte(1, 2, above_u32.src_pitch),
        u64::from(u32::MAX) + 2
    );
    assert_eq!(at_u32.address_width, YcbcrAddressWidth::U32);
    assert_eq!(above_u32.address_width, YcbcrAddressWidth::U64);
}

#[test]
fn metal_address_probe_returns_the_checked_64_bit_indices() {
    let Some(device) = test_device() else {
        return;
    };
    let source = format!(
        "{YCBCR_TO_RGB8_METAL}\n{}",
        include_str!("../ycbcr_probe.metal")
    );
    let loader = j2k_metal_support::MetalPipelineLoader::new(&device, &source)
        .expect("compile YCbCr address probe");
    let pipeline = loader
        .pipeline("wsi_rs_ycbcr8_address_probe")
        .expect("create YCbCr address probe pipeline");
    let queue = j2k_metal_support::checked_command_queue(&device)
        .expect("create address probe command queue");
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
    let coordinate = [0, 1];
    let encoder = j2k_metal_support::checked_compute_command_encoder(&command_buffer)
        .expect("create address probe encoder");
    encoder.setComputePipelineState(&pipeline);
    interop::bind_compute_buffer(&encoder, 0, &output);
    interop::bind_ycbcr_params(&encoder, 1, &params);
    interop::bind_probe_coordinate(&encoder, 2, &coordinate);
    j2k_metal_support::dispatch_single_thread(&encoder);
    encoder.endEncoding();
    j2k_metal_support::commit_and_wait(&command_buffer).expect("address probe completion");

    assert_eq!(
        u64_buffer_values(&output, 2),
        [u64::from(u32::MAX), u64::from(u32::MAX - 2)]
    );
}

#[test]
fn ycbcr_address_plan_accounts_for_padded_pitch_and_last_pixel() {
    let plan = YcbcrAddressPlan::new(7, 5, 64, 64 * 5).expect("padded source plan");

    assert_eq!(plan.dst_pitch, 21);
    assert_eq!(plan.dst_len, 105);
    assert_eq!(YcbcrAddressPlan::max_byte(7, 5, plan.src_pitch), 276);
    assert_eq!(YcbcrAddressPlan::max_byte(7, 5, plan.dst_pitch as u32), 104);
}

#[test]
fn ycbcr_address_plan_rejects_invalid_dimensions_and_pitch() {
    for dimensions in [(0, 1), (1, 0)] {
        let error = YcbcrAddressPlan::new(dimensions.0, dimensions.1, 3, 3)
            .expect_err("zero dimensions must be rejected");
        assert!(error.to_string().contains("nonzero dimensions"));
    }

    let short_pitch =
        YcbcrAddressPlan::new(2, 1, 5, 6).expect_err("short source pitch must be rejected");
    assert!(short_pitch.to_string().contains("shorter than row bytes"));

    let oversized_pitch = YcbcrAddressPlan::new(1, 1, u32::MAX as usize + 1, usize::MAX)
        .expect_err("shader pitch is limited to u32");
    assert!(oversized_pitch.to_string().contains("shader ABI"));
}

#[test]
fn ycbcr_max_address_fits_the_full_u32_domain() {
    assert_eq!(
        YcbcrAddressPlan::max_byte(u32::MAX, u32::MAX, u32::MAX),
        u64::MAX - 1
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
    let Some(device) = test_device() else {
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
