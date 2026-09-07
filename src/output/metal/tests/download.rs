use super::*;

#[test]
fn mixed_readback_batches_stage_once_and_preserve_duplicate_cropped_rows() {
    use crate::core::execution_telemetry::{test_count, Event};
    let Some(device) = test_device() else {
        return;
    };
    let sessions = MetalBackendSessions::new(device.clone());
    let a = MetalDeviceTile::from_resident(interop::resident_private_test_image(
        &device,
        &[1, 2, 3, 4, 5, 6],
        (2, 1),
        6,
    ))
    .unwrap();
    let b = MetalDeviceTile::from_resident(interop::resident_private_test_image(
        &device,
        &[9, 8, 7, 6, 5, 4, 0xaa, 0xbb, 3, 2, 1, 5, 6, 7, 0xcc, 0xdd],
        (2, 2),
        8,
    ))
    .unwrap()
    .crop_top_left(1, 2)
    .unwrap();
    let shared =
        MetalDeviceTile::from_resident(resident_test_image(&device, &[31, 32, 33], (1, 1), 3))
            .unwrap();
    let before = test_count(Event::ReadbackSubmissions);
    let actual = sessions
        .download_cpu_batch(&[a.clone(), shared, b, a])
        .unwrap();
    assert_eq!(test_count(Event::ReadbackSubmissions) - before, 1);
    let expected: [&[u8]; 4] = [
        &[1, 2, 3, 4, 5, 6],
        &[31, 32, 33],
        &[9, 8, 7, 3, 2, 1],
        &[1, 2, 3, 4, 5, 6],
    ];
    assert_eq!(actual.len(), expected.len());
    for (tile, expected) in actual.iter().zip(expected) {
        assert_eq!(tile.as_u8().unwrap(), expected);
    }
}

#[test]
fn completed_shared_readback_copies_only_logical_rows_without_staging() {
    let Some(device) = test_device() else {
        return;
    };
    let image = resident_test_image(
        &device,
        &[99, 98, 1, 2, 3, 77, 4, 5, 6, 88, 87, 86],
        (1, 3),
        4,
    );
    let layout =
        j2k_metal_support::MetalImageLayout::new(2, (1, 2), 4, j2k_core::PixelFormat::Rgb8)
            .unwrap();
    let tile = MetalDeviceTile::from_resident(image.view(layout).unwrap()).unwrap();
    interop::READBACK_STAGING_BYTES.with(|bytes| bytes.set(0));
    assert_eq!(
        tile.download_cpu().unwrap().as_u8().unwrap(),
        &[1, 2, 3, 4, 5, 6]
    );
    assert_eq!(
        interop::READBACK_STAGING_BYTES.with(std::cell::Cell::get),
        0,
        "completed shared storage needs no GPU staging allocation"
    );
}

#[test]
fn download_cpu_strips_pitch_and_honors_cropped_geometry() {
    let Some(device) = test_device() else {
        eprintln!("skipping Metal download test: no Metal device");
        return;
    };
    let bytes = [
        1, 2, 3, 4, 5, 6, 0xaa, 0xbb, 7, 8, 9, 10, 11, 12, 0xcc, 0xdd,
    ];
    let image = resident_test_image(&device, &bytes, (2, 2), 8);
    let tile = MetalDeviceTile::from_resident(image)
        .expect("pitched resident image")
        .crop_top_left(1, 2)
        .expect("cropped resident view");

    let downloaded = tile.download_cpu().expect("pitched cropped readback");
    assert_eq!((downloaded.width(), downloaded.height()), (1, 2));
    assert_eq!(downloaded.channels(), 3);
    assert_eq!(downloaded.as_u8(), Some(&[1, 2, 3, 7, 8, 9][..]));
}

#[test]
fn download_limit_is_128_mib() {
    super::super::tile::enforce_download_limit(128 * 1024 * 1024).expect("limit is inclusive");
    let error = super::super::tile::enforce_download_limit(128 * 1024 * 1024 + 1)
        .expect_err("oversized Metal readback must fail before allocation");
    assert!(matches!(
        error,
        WsiError::ResourceLimit {
            resource: "Metal host tile download",
            limit: super::super::tile::MAX_DEVICE_DOWNLOAD_BYTES,
            ..
        }
    ));
}

#[test]
fn private_readback_reuses_the_queue_after_session_drop_and_crop() {
    use crate::core::execution_telemetry::{test_count, Event};
    let Some(device) = test_device() else {
        return;
    };
    let session = MetalBackendSessions::new(device.clone());
    let tile = MetalDeviceTile::from_resident(interop::resident_private_test_image(
        &device,
        &[1, 2, 3, 4, 5, 6, 8, 9, 7, 6, 5, 4],
        (2, 2),
        6,
    ))
    .unwrap();
    let tile = session
        .retain_readback_queue(tile)
        .unwrap()
        .crop_top_left(1, 2)
        .unwrap();
    let clone = tile.clone();
    drop(session);
    let before = test_count(Event::ReadbackQueueCreations);
    for tile in [tile, clone] {
        assert_eq!(
            tile.download_cpu().unwrap().as_u8().unwrap(),
            &[1, 2, 3, 8, 9, 7]
        );
    }
    assert_eq!(test_count(Event::ReadbackQueueCreations) - before, 1);
}
