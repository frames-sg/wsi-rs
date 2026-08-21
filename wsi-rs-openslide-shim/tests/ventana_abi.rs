use wsi_rs_openslide_shim::*;

mod support;

use support::{fixture_path, fnv1a_argb};

#[test]
fn ventana_level_zero_tissue_matches_openslide_tilemap_semantics() {
    let Some(path) = fixture_path("ventana-001", "bif") else {
        return;
    };
    let slide = wsi_rs::Slide::open(path.as_c_str().to_str().expect("UTF-8 fixture path"))
        .expect("open Ventana fixture through the core API");
    let hits = slide.dataset().scenes[0].series[0].levels[0]
        .tile_layout
        .tiles_for_region(28_671, 19_023, 256, 256);
    assert_eq!(hits.len(), 1);
    assert_eq!((hits[0].col, hits[0].row), (30, 15));
    assert_eq!((hits[0].dest_x, hits[0].dest_y), (-397, -276));
    assert_eq!(hits[0].dest_x_f64, -397.0);
    assert!((hits[0].dest_y_f64 + 275.871_526_272_621_85).abs() < 1e-9);

    // This occupied 256x256 region and checksum were characterized through
    // the pinned OpenSlide 4.0.1 comparator and manifest-pinned fixture.
    // SAFETY: `path` and `pixels` remain live for their calls, and the opened
    // handle is closed exactly once.
    unsafe {
        let osr = openslide_open(path.as_ptr());
        assert!(!osr.is_null());
        assert!(openslide_get_error(osr).is_null());

        let mut pixels = vec![0u32; 256 * 256];
        openslide_read_region(osr, pixels.as_mut_ptr(), 28_671, 19_023, 0, 256, 256);

        assert!(openslide_get_error(osr).is_null());
        assert_eq!(fnv1a_argb(&pixels), 0x2fd2_3e2f_d440_0689);
        assert!(pixels.iter().all(|pixel| pixel >> 24 == 0xff));
        openslide_close(osr);
    }
}
