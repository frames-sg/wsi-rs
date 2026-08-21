use super::*;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::num::NonZeroUsize;
use std::panic::{catch_unwind, AssertUnwindSafe};

#[test]
fn tile_size_uses_virtual_size_for_whole_level_layout() {
    let layout = TileLayout::WholeLevel {
        width: 1024,
        height: 768,
        virtual_tile_width: 512,
        virtual_tile_height: 256,
    };

    assert_eq!(tile_size(&layout), Some((512, 256)));
}

#[test]
fn tile_size_rounds_irregular_tile_advance() {
    let layout = TileLayout::Irregular {
        tile_advance: (127.6, 0.2),
        extra_tiles: (0, 0, 0, 0),
        tiles: HashMap::new(),
    };

    assert_eq!(tile_size(&layout), Some((128, 1)));
}

#[test]
fn handle_error_state_recovers_from_poisoned_mutex() {
    let handle = OpenSlideHandle::from_error("initial error".to_string());

    let _ = catch_unwind(AssertUnwindSafe(|| {
        let _guard = handle.error.lock().expect("lock error mutex");
        panic!("poison error mutex");
    }));

    handle.set_error("later error");

    assert!(handle.has_error());
    assert!(!handle.error_ptr().is_null());
    assert_eq!(handle.property_names(), empty_names());
}

#[test]
fn benchmark_jp2k_thread_budget_parser_is_strict() {
    assert_eq!(parse_benchmark_jp2k_threads(None).unwrap(), None);
    assert_eq!(
        parse_benchmark_jp2k_threads(Some(OsStr::new("3"))).unwrap(),
        NonZeroUsize::new(3)
    );
    for invalid in ["", "0", "-1", "many"] {
        assert!(parse_benchmark_jp2k_threads(Some(OsStr::new(invalid))).is_err());
    }
}

#[test]
fn benchmark_open_applies_the_requested_jp2k_pool_size() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("tests")
        .join("fixtures")
        .join("jp2k")
        .join("rgb_nomct.j2k");
    let threads = NonZeroUsize::new(1).unwrap();

    let handle = OpenSlideHandle::open_with_jp2k_threads(path, Some(threads))
        .expect("open raw JP2K through benchmark-configured shim");
    assert_eq!(
        handle
            .slide()
            .expect("open slide")
            .decode_execution_options()
            .jp2k_cpu_threads(),
        Some(threads)
    );
}
