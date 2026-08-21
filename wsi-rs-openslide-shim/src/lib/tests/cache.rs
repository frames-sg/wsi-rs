use super::*;
use std::ffi::CString;
use std::sync::Arc;

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
fn ffi_cache_is_shared_released_safely_and_replaced() {
    let path = fixture_path();
    // SAFETY: Every pointer is created by this shim, remains live for each
    // call, and is released or closed exactly once.
    unsafe {
        let first = openslide_open(path.as_ptr());
        let second = openslide_open(path.as_ptr());
        assert!(!first.is_null() && !second.is_null());
        let cache = openslide_cache_create(1024);
        let owner = Arc::clone(&cache_ref(cache).expect("live cache handle").owner);
        openslide_set_cache(first, cache);
        openslide_set_cache(second, cache);
        openslide_cache_release(cache);

        let mut pixels = [0u32; 16];
        openslide_read_region(first, pixels.as_mut_ptr(), 0, 0, 0, 4, 4);
        let cold = owner.stats();
        assert_eq!(cold.misses, 1);
        assert_eq!(cold.puts, 1);
        openslide_read_region(second, pixels.as_mut_ptr(), 0, 0, 0, 4, 4);
        assert_eq!(owner.stats().hits, 1, "second slide must share the entry");

        let replacement = openslide_cache_create(1);
        let replacement_owner =
            Arc::clone(&cache_ref(replacement).expect("replacement cache").owner);
        openslide_set_cache(second, replacement);
        openslide_cache_release(replacement);
        openslide_read_region(second, pixels.as_mut_ptr(), 0, 0, 0, 4, 4);
        assert_eq!(replacement_owner.stats().misses, 1);
        assert_eq!(replacement_owner.stats().rejected_oversize, 1);
        let before_null = replacement_owner.stats();
        openslide_set_cache(second, ptr::null_mut());
        openslide_read_region(second, pixels.as_mut_ptr(), 0, 0, 0, 4, 4);
        assert_eq!(
            replacement_owner.stats().misses,
            before_null.misses + 1,
            "NULL cache is outside the OpenSlide contract and must not detach the live cache"
        );

        openslide_close(first);
        openslide_close(second);
    }
}
