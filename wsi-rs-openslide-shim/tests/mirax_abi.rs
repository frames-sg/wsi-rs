use wsi_rs_openslide_shim::*;

mod support;

use support::{fixture_path, fnv1a_argb};

#[test]
fn mirax_empty_regions_are_transparent_without_poisoning_the_handle() {
    let Some(path) = fixture_path("mirax-001", "d/CMU-1.mrxs") else {
        return;
    };
    // SAFETY: The path and output buffer remain live for each call, and the
    // handle is closed exactly once after all reads.
    unsafe {
        let osr = openslide_open(path.as_ptr());
        assert!(!osr.is_null());
        for (x, y) in [(60_963, 113_099), (26_933, 160_457)] {
            let mut pixels = vec![u32::MAX; 256 * 256];
            openslide_read_region(osr, pixels.as_mut_ptr(), x, y, 0, 256, 256);
            assert!(openslide_get_error(osr).is_null());
            assert!(pixels.iter().all(|pixel| *pixel == 0));
        }
        let mut pixels = vec![0; 256 * 256];
        openslide_read_region(osr, pixels.as_mut_ptr(), 26_933, 113_099, 0, 256, 256);
        assert!(openslide_get_error(osr).is_null());
        assert!(pixels.iter().any(|pixel| *pixel != 0));
        openslide_close(osr);
    }
}

#[test]
fn mirax_coarse_levels_resample_fractional_subtiles_like_openslide() {
    // The fixture's 340-pixel images split into 42.5-pixel subtiles from
    // level 5, down to 2.66 pixels at level 9. OpenSlide resamples each onto
    // a ceil-sized surface rather than cropping whole pixels. Both checksums
    // were characterized through the pinned OpenSlide 4.0.1 comparator.
    let Some(path) = fixture_path("mirax-001", "d/CMU-1.mrxs") else {
        return;
    };
    let cases = [
        (
            (10_559, 96_716, 7, 256, 256),
            0x8ab1_d4f8_ab5f_5e3e_u64,
            537,
        ),
        ((9_919, 89_420, 9, 133, 185), 0x512b_cd0a_69c4_ca5f, 650),
    ];
    // SAFETY: `path` and `pixels` remain live for their calls, and the opened
    // handle is closed exactly once.
    unsafe {
        let osr = openslide_open(path.as_ptr());
        assert!(!osr.is_null());
        assert!(openslide_get_error(osr).is_null());
        for ((x, y, level, width, height), checksum, partial) in cases {
            let mut pixels = vec![0u32; (width * height) as usize];
            openslide_read_region(osr, pixels.as_mut_ptr(), x, y, level, width, height);
            assert!(openslide_get_error(osr).is_null());
            assert_eq!(
                pixels
                    .iter()
                    .filter(|pixel| !matches!(**pixel >> 24, 0 | 0xff))
                    .count(),
                partial,
                "level {level} partial-coverage pixels"
            );
            assert_eq!(fnv1a_argb(&pixels), checksum, "level {level} checksum");
        }
        // Vertical bands must retain the complete source clip's Pixman
        // filter. These interior RGB values come from OpenSlide 4.0.1.
        let mut pixels = vec![0u32; 256 * 256];
        openslide_read_region(osr, pixels.as_mut_ptr(), 18_395, 101_252, 2, 256, 256);
        assert!(openslide_get_error(osr).is_null());
        for (x, y, expected) in [
            (49, 0, 0xffbc_6cbc),
            (88, 45, 0xff81_4c84),
            (109, 89, 0xfff5_f4f1),
            (250, 130, 0xffdd_78ca),
            (153, 173, 0xfff1_f7f1),
        ] {
            assert_eq!(pixels[y * 256 + x], expected, "pixel ({x}, {y})");
        }
        openslide_close(osr);
    }
}
