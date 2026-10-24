use std::ffi::CStr;

use wsi_rs_openslide_shim::*;

mod support;

use support::public_fixture;

const TWELVE_BIT_FIXTURES: [&str; 4] = [
    "mono2-extended",
    "mono2-progressive",
    "ybr422-extended",
    "ybr422-progressive",
];

#[test]
fn eight_bit_dicom_still_detects_and_opens() {
    let cpath = public_fixture("public_dicom/progressive-sof2.dcm");
    // SAFETY: `cpath` is a live NUL-terminated path for each ABI call. The
    // vendor string is interned by the shim, and the handle is closed once.
    unsafe {
        let vendor = openslide_detect_vendor(cpath.as_ptr());
        assert!(!vendor.is_null());
        assert_eq!(CStr::from_ptr(vendor).to_string_lossy(), "dicom");
        let osr = openslide_open(cpath.as_ptr());
        assert!(!osr.is_null());
        assert!(openslide_get_error(osr).is_null());
        assert_eq!(openslide_get_level_count(osr), 1);
        openslide_close(osr);
    }
}

#[test]
fn twelve_bit_dicom_keeps_openslide_rejection() {
    // OpenSlide's DICOM driver requires BitsAllocated 8, BitsStored 8 and
    // HighBit 7. The native wsi-rs API decodes these files to U16, but the
    // shim keeps the exact rejection it reported before 12-bit support.
    for name in TWELVE_BIT_FIXTURES {
        let cpath = public_fixture(&format!("dicom_12bit/{name}.dcm"));
        let path = cpath.to_string_lossy();
        // SAFETY: `cpath` is a live NUL-terminated path for each ABI call. The
        // error pointer is read while its handle is open, and the handle is
        // closed exactly once.
        unsafe {
            assert!(
                openslide_detect_vendor(cpath.as_ptr()).is_null(),
                "{name}: vendor detection"
            );
            let osr = openslide_open(cpath.as_ptr());
            assert!(!osr.is_null(), "{name}: open returns an error handle");
            let error = openslide_get_error(osr);
            assert!(!error.is_null(), "{name}: open error is set");
            assert_eq!(
                CStr::from_ptr(error).to_string_lossy(),
                format!("invalid slide {path}: Attribute BitsAllocated value 16 != 8"),
                "{name}: open error"
            );
            assert_eq!(openslide_get_level_count(osr), -1, "{name}: level count");
            openslide_close(osr);
        }
    }
}
