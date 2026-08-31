use super::*;

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
