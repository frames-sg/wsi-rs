use super::*;
use std::ffi::CString;

fn fixture_path() -> CString {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("tests")
        .join("fixtures")
        .join("jp2k")
        .join("rgb_nomct.j2k");
    CString::new(path.to_string_lossy().as_bytes()).expect("fixture path has no NUL")
}

#[test]
fn invalid_region_level_clears_output_without_poisoning_the_handle() {
    let path = fixture_path();
    // SAFETY: The handle comes from this shim and remains live until the final
    // close. Every destination points to the declared number of pixels.
    unsafe {
        let osr = openslide_open(path.as_ptr());
        assert!(!osr.is_null());

        for level in [-1, openslide_get_level_count(osr)] {
            let mut pixels = [u32::MAX; 4];
            openslide_read_region(osr, pixels.as_mut_ptr(), 0, 0, level, 2, 2);
            assert_eq!(pixels, [0; 4]);
            assert!(openslide_get_error(osr).is_null());
        }

        let mut pixels = [0; 4];
        openslide_read_region(osr, pixels.as_mut_ptr(), 0, 0, 0, 2, 2);
        assert!(pixels.iter().any(|pixel| *pixel != 0));
        assert!(openslide_get_error(osr).is_null());
        openslide_close(osr);
    }
}

#[test]
fn negative_region_dimensions_are_terminal_even_for_an_invalid_level() {
    let path = fixture_path();
    // SAFETY: The handle comes from this shim and remains live until close.
    // A negative width has no destination extent, so the sentinel is not
    // writable by the ABI call.
    unsafe {
        let osr = openslide_open(path.as_ptr());
        assert!(!osr.is_null());

        let mut sentinel = u32::MAX;
        openslide_read_region(osr, &mut sentinel, 0, 0, -1, -1, 1);
        assert_eq!(sentinel, u32::MAX);
        assert!(!openslide_get_error(osr).is_null());

        openslide_close(osr);
    }
}

#[test]
fn null_region_destination_is_non_terminal_for_non_negative_dimensions() {
    let path = fixture_path();
    // SAFETY: The handle comes from this shim and remains live until close.
    // OpenSlide permits a null destination for non-negative region sizes.
    unsafe {
        let osr = openslide_open(path.as_ptr());
        assert!(!osr.is_null());

        openslide_read_region(osr, std::ptr::null_mut(), 0, 0, 0, 2, 2);
        assert!(openslide_get_error(osr).is_null());

        openslide_read_region(osr, std::ptr::null_mut(), 0, 0, 0, 0, 2);
        assert!(openslide_get_error(osr).is_null());

        let mut pixels = [0; 4];
        openslide_read_region(osr, pixels.as_mut_ptr(), 0, 0, 0, 2, 2);
        assert!(pixels.iter().any(|pixel| *pixel != 0));
        assert!(openslide_get_error(osr).is_null());
        openslide_close(osr);
    }
}

#[test]
fn negative_region_dimensions_are_terminal_with_a_null_destination() {
    let path = fixture_path();
    // SAFETY: The handle comes from this shim and remains live until close.
    // A null destination is permitted, but negative dimensions are not.
    unsafe {
        let osr = openslide_open(path.as_ptr());
        assert!(!osr.is_null());

        openslide_read_region(osr, std::ptr::null_mut(), 0, 0, 0, -1, 1);
        assert!(!openslide_get_error(osr).is_null());
        openslide_close(osr);
    }
}

#[test]
fn unknown_associated_image_is_a_non_terminal_miss() {
    let path = fixture_path();
    let missing = CString::new("missing").unwrap();
    // SAFETY: The handle comes from this shim and remains live until the final
    // close. OpenSlide permits querying a name that is not present.
    unsafe {
        let osr = openslide_open(path.as_ptr());
        assert!(!osr.is_null());

        let mut width = 7;
        let mut height = 9;
        openslide_get_associated_image_dimensions(osr, missing.as_ptr(), &mut width, &mut height);
        assert_eq!((width, height), (-1, -1));

        let mut pixel = u32::MAX;
        openslide_read_associated_image(osr, missing.as_ptr(), &mut pixel);
        assert_eq!(pixel, u32::MAX);
        assert_eq!(
            openslide_get_associated_image_icc_profile_size(osr, missing.as_ptr()),
            -1
        );
        let mut profile_byte = u8::MAX;
        openslide_read_associated_image_icc_profile(
            osr,
            missing.as_ptr(),
            (&mut profile_byte as *mut u8).cast(),
        );
        assert_eq!(profile_byte, u8::MAX);

        assert!(openslide_get_error(osr).is_null());
        assert!(openslide_get_level_count(osr) > 0);
        openslide_close(osr);
    }
}

#[test]
fn full_canvas_slide_does_not_invent_bounds_properties() {
    let path = fixture_path();
    let bounds_x = CString::new("openslide.bounds-x").unwrap();
    // SAFETY: The handle comes from this shim and remains live until the final
    // close. The property name is a valid NUL-terminated string.
    unsafe {
        let osr = openslide_open(path.as_ptr());
        assert!(!osr.is_null());
        assert!(openslide_get_property_value(osr, bounds_x.as_ptr()).is_null());
        assert!(openslide_get_error(osr).is_null());
        openslide_close(osr);
    }
}
