use wsi_rs_openslide_shim::*;

mod support;

use support::{fixture_path, fnv1a_argb};

#[test]
fn aperio_reduced_level_reads_preserve_fractional_origins() {
    let Some(path) = fixture_path("svs-001", "svs") else {
        return;
    };

    // Characterized through pinned OpenSlide 4.0.1. The first case lands just
    // below an integral level-2 coordinate; the second retains a substantial
    // fractional offset and therefore cannot be satisfied by integer rounding.
    let cases = [
        (320, 224, 64, 0xb4bf_5875_52c8_8e52),
        (100, 100, 64, 0x0434_bddf_2f3f_a88e),
        (1312, 896, 256, 0x9ab2_0c87_749a_258f),
        (2624, 1808, 256, 0xcc33_b2b6_ee78_4e20),
        (4272, 2944, 256, 0xfddb_40f9_38eb_292b),
        (6592, 4528, 256, 0x85bc_0f6a_aa33_e88b),
        (8560, 5888, 256, 0xb962_ce82_3380_aed6),
        (10544, 7248, 256, 0x8694_95ff_475e_4f97),
        (24081, 16561, 256, 0x52f2_e9bb_c189_0cd4),
    ];

    // SAFETY: `path` and each destination remain live for their calls, and
    // the opened handle is closed exactly once.
    unsafe {
        let osr = openslide_open(path.as_ptr());
        assert!(!osr.is_null());
        assert!(openslide_get_error(osr).is_null());

        for (x, y, side, expected) in cases {
            let mut pixels = vec![0u32; side * side];
            openslide_read_region(osr, pixels.as_mut_ptr(), x, y, 2, side as i64, side as i64);
            assert!(openslide_get_error(osr).is_null(), "coordinate {x},{y}");
            assert_eq!(fnv1a_argb(&pixels), expected, "coordinate {x},{y}");
            assert!(pixels.iter().any(|pixel| pixel >> 24 != 0));
        }

        openslide_close(osr);
    }
}
