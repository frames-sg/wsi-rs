use std::sync::Arc;

use objc2_metal::MTLDevice;

use super::*;

#[test]
fn ycbcr_to_rgb8_converter_is_cached_per_backend_sessions() {
    let Some(device) = test_device() else {
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
fn backend_sessions_identify_the_device_and_keep_converter_debug_opaque() {
    let Some(device) = test_device() else {
        eprintln!("skipping Metal session identity test: no Metal device");
        return;
    };
    let expected_identity = device.name().to_string();
    let sessions = MetalBackendSessions::new(device);

    assert_eq!(sessions.device_identity(), expected_identity);
    let converter = sessions.ycbcr_to_rgb8_converter().expect("YCbCr converter");
    assert_eq!(format!("{converter:?}"), "YcbcrToRgb8Converter { .. }");
}

#[test]
fn ycbcr_to_rgb8_tiles_converts_batch_with_one_cached_converter() {
    let Some(device) = test_device() else {
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
        let MetalDeviceStorage::Resident { image } = &tile.storage;
        assert_eq!(image.byte_offset(), 0);
        assert_eq!(image.byte_len(), 6);
        assert_eq!(resident_bytes(image), expected);
    }
    let first = sessions
        .ycbcr_to_rgb8_converter()
        .expect("cached converter after batch");
    let second = sessions
        .ycbcr_to_rgb8_converter()
        .expect("cached converter after batch");
    assert!(Arc::ptr_eq(&first, &second));
}

#[test]
fn ycbcr_single_tile_and_empty_batch_use_the_shared_converter() {
    let Some(device) = test_device() else {
        return;
    };
    let sessions = MetalBackendSessions::new(device.clone());
    let converter = sessions.ycbcr_to_rgb8_converter().expect("YCbCr converter");
    let tile = ycbcr_test_tile(&device, &[16, 128, 128, 32, 128, 128]);

    let converted = tile
        .ycbcr8_to_rgb8(&converter)
        .expect("single YCbCr tile conversion");

    let MetalDeviceStorage::Resident { image } = &converted.storage;
    assert_eq!(resident_bytes(image), [16, 16, 16, 32, 32, 32]);
    assert!(sessions
        .ycbcr8_tiles_to_rgb8(&[])
        .expect("empty YCbCr batch")
        .is_empty());
}

#[test]
fn ycbcr_u64_pipeline_compiles_once_and_is_cached() {
    let Some(device) = test_device() else {
        return;
    };
    let sessions = MetalBackendSessions::new(device);
    let converter = sessions.ycbcr_to_rgb8_converter().expect("YCbCr converter");

    let first = converter.pipeline_u64().expect("u64 YCbCr pipeline");
    let second = converter.pipeline_u64().expect("cached u64 YCbCr pipeline");

    assert!(std::ptr::eq(first, second));
}

#[test]
fn ycbcr_conversion_rejects_non_rgb8_resident_input() {
    let Some(device) = test_device() else {
        return;
    };
    let sessions = MetalBackendSessions::new(device.clone());
    let mut tile = ycbcr_test_tile(&device, &[16, 128, 128, 32, 128, 128]);
    tile.format = PixelFormat::Rgba8;

    let error = sessions
        .ycbcr8_tiles_to_rgb8(&[tile])
        .expect_err("YCbCr conversion requires RGB8-compatible planes");

    assert!(matches!(error, WsiError::Unsupported { .. }));
    assert!(error.to_string().contains("Rgb8"));
}

#[test]
fn resident_device_validation_rejects_a_different_metal_device_when_available() {
    let devices = objc2_metal::MTLCopyAllDevices()
        .into_iter()
        .collect::<Vec<_>>();
    let Some(source_device) = devices.first() else {
        return;
    };
    let Some(other_device) = devices
        .iter()
        .find(|device| device.registryID() != source_device.registryID())
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
    let Some(device) = test_device() else {
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
