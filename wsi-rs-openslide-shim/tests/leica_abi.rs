use std::ffi::{CStr, CString};
use wsi_rs_openslide_shim::*;

mod support;

use support::{fixture_path, fnv1a_argb};

const LEICA_LEVEL_DIMENSIONS: [(i64, i64); 5] = [
    (53_130, 153_470),
    (13_283, 38_368),
    (3_321, 9_592),
    (831, 2_401),
    (208, 601),
];
const LEICA_LEVEL_DOWNSAMPLES: [f64; 5] = [
    1.0,
    3.999_898_652_415_998,
    15.998_992_404_088_622,
    63.927_109_191_868_01,
    255.395_214_706_258_8,
];

/// # Safety
///
/// `osr` must be a live shim handle for the duration of the call. Any returned
/// property pointer must remain valid until this helper copies its C string.
unsafe fn property(osr: *mut openslide_t, name: &str) -> Option<String> {
    let name = CString::new(name).expect("property name has no NUL");
    // SAFETY: `osr` is a live handle for the duration of the call and `name`
    // is a live NUL-terminated string. The returned pointer remains owned by
    // the handle and is copied before the next operation.
    let value = unsafe { openslide_get_property_value(osr, name.as_ptr()) };
    (!value.is_null()).then(|| {
        // SAFETY: A non-null property pointer returned by the shim identifies
        // a handle-owned NUL-terminated string.
        unsafe { CStr::from_ptr(value) }
            .to_string_lossy()
            .into_owned()
    })
}

#[test]
fn leica_exposes_the_openslide_collection_canvas() {
    let Some(path) = fixture_path("leica-001", "scn") else {
        return;
    };

    // SAFETY: `path` remains live and NUL-terminated, dimension outputs are
    // valid stack locations, and the opened handle is closed exactly once.
    unsafe {
        let osr = openslide_open(path.as_ptr());
        assert!(!osr.is_null());
        assert!(openslide_get_error(osr).is_null());

        assert_eq!(openslide_get_level_count(osr), 5);
        for (level, expected) in LEICA_LEVEL_DIMENSIONS.into_iter().enumerate() {
            let mut width = -1;
            let mut height = -1;
            openslide_get_level_dimensions(osr, level as i32, &mut width, &mut height);
            assert_eq!((width, height), expected, "level {level}");
            assert_eq!(
                openslide_get_level_downsample(osr, level as i32),
                LEICA_LEVEL_DOWNSAMPLES[level],
                "level {level} downsample"
            );
        }

        assert_eq!(
            property(osr, "openslide.bounds-x").as_deref(),
            Some("10778")
        );
        assert_eq!(
            property(osr, "openslide.bounds-y").as_deref(),
            Some("35096")
        );
        assert_eq!(
            property(osr, "openslide.bounds-width").as_deref(),
            Some("36832")
        );
        assert_eq!(
            property(osr, "openslide.bounds-height").as_deref(),
            Some("38432")
        );

        openslide_close(osr);
    }
}

#[test]
fn leica_reads_global_canvas_coordinates_at_each_level() {
    let Some(path) = fixture_path("leica-001", "scn") else {
        return;
    };

    // These checksums were characterized through the pinned OpenSlide 4.0.1
    // comparator using the manifest-pinned `leica-001` fixture. The level-3
    // row also catches the per-level rounding of the Leica scene origin.
    let cases = [
        (10_778, 35_096, 0, 0x6bee_e660_f55a_7b29),
        (11_290, 35_608, 1, 0x4d30_582c_432c_9821),
        (10_778, 35_096, 3, 0x83c7_986c_5290_a67a),
    ];

    // SAFETY: `path` and each destination remain live for their calls, and
    // the opened handle is closed exactly once.
    unsafe {
        let osr = openslide_open(path.as_ptr());
        assert!(!osr.is_null());
        for (x, y, level, expected) in cases {
            let mut pixels = vec![0u32; 64 * 64];
            openslide_read_region(osr, pixels.as_mut_ptr(), x, y, level, 64, 64);
            assert!(openslide_get_error(osr).is_null(), "level {level}");
            assert_eq!(fnv1a_argb(&pixels), expected, "level {level}");
            assert!(pixels.iter().all(|pixel| pixel >> 24 == 0xff));
        }
        openslide_close(osr);
    }
}
