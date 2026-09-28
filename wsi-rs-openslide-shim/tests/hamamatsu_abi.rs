use wsi_rs_openslide_shim::*;

mod support;

use support::{fixture_path, fnv1a_argb};

/// `(x, y, level, width, height)` in `openslide_read_region` argument order.
type Region = (i64, i64, i32, i64, i64);

/// Reads each `(region, checksum)` case through the OpenSlide ABI and checks
/// the fully opaque ARGB output.
fn assert_opaque_region_checksums(path: &std::ffi::CStr, cases: &[(Region, u64)]) {
    // SAFETY: `path` and `pixels` remain live for their calls, and the opened
    // handle is closed exactly once.
    unsafe {
        let osr = openslide_open(path.as_ptr());
        assert!(!osr.is_null());
        assert!(openslide_get_error(osr).is_null());
        for &((x, y, level, width, height), checksum) in cases {
            let mut pixels = vec![0u32; (width * height) as usize];
            openslide_read_region(osr, pixels.as_mut_ptr(), x, y, level, width, height);
            assert!(openslide_get_error(osr).is_null());
            assert!(pixels.iter().all(|pixel| pixel >> 24 == 0xff));
            assert_eq!(fnv1a_argb(&pixels), checksum, "level {level} checksum");
        }
        openslide_close(osr);
    }
}

#[test]
fn ndpi_scaled_levels_match_openslide_reduced_idct() {
    let Some(path) = fixture_path("ndpi-001", "ndpi") else {
        return;
    };
    // OpenSlide derives levels 1 and 5 by decoding the 2048x8 and 128x8
    // restart intervals of stored levels 0 and 4 at libjpeg scale 1/2, and
    // level 8 by decoding stored level 6 (no restart markers) at 1/4. The
    // regions cross strip boundaries. All three checksums were characterized
    // through the pinned OpenSlide 4.0.1 comparator (libjpeg-turbo 3.1.4.1).
    assert_opaque_region_checksums(
        &path,
        &[
            ((12_602, 9_418, 1, 256, 256), 0xd8bb_9a75_aa6f_8c86),
            ((12_832, 9_504, 5, 256, 256), 0xbc65_0589_1c88_c03f),
            ((0, 0, 8, 200, 149), 0x44df_60af_0fd1_fecd),
        ],
    );
}

#[test]
fn vms_scaled_levels_match_openslide_reduced_idct() {
    let Some(path) = fixture_path("vms-001", "d/CMU-1-40x - 2010-01-12 13.24.05.vms") else {
        return;
    };
    // Level 2 is the 2x2 grid of main JPEGs at libjpeg scale 1/4; the region
    // straddles both JPEG seams at level-0 x/y 61440. Level 6 is the map JPEG
    // at 1/8. Both checksums were characterized through the pinned OpenSlide
    // 4.0.1 comparator (libjpeg-turbo 3.1.4.1).
    assert_opaque_region_checksums(
        &path,
        &[
            ((61_200, 61_200, 2, 256, 256), 0x84ed_aee5_2cba_2193),
            ((0, 0, 6, 1600, 1192), 0x982d_9b0f_d4a5_0317),
        ],
    );
}
