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
    // OpenSlide accumulates the tilemap translation in separate double steps,
    // so the unsnapped origin carries float residue that Cairo snaps away.
    assert!((hits[0].dest_x_f64 + 397.0).abs() < 1e-9);
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

#[test]
fn ventana_reduced_levels_paint_level0_subtiles_like_openslide() {
    let Some(path) = fixture_path("ventana-001", "bif") else {
        return;
    };
    // Reduced BIF levels are the level-0 tilemap at 1/downsample scale, one
    // stored-tile subtile per level-0 cell. The level-5 region crosses an AOI
    // edge with partial coverage and uses 32x42.5 subtiles; the level-9
    // thumbnail paints every cell from 2x2.66 subtiles. Both checksums were
    // characterized through the pinned OpenSlide 4.0.1 comparator.
    let cases = [
        (
            (27_704, 18_056, 5, 256, 256),
            0xaae6_7b3b_2db1_9c1b_u64,
            256,
        ),
        ((0, 0, 9, 224, 149), 0xd6f4_f793_97d1_fcee, 144),
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
        // Coarse subtiles still contribute bilinear coverage past their
        // nominal extent at internal row-band boundaries. Values come from
        // pinned OpenSlide 4.0.1 reads of the same public fixture.
        for ((x, y, level, width, height), samples) in [
            (
                (20_480, 10_880, 6, 512, 512),
                vec![(121, 320, 0x5e52_5252), (121, 496, 0x504b_4b4b)],
            ),
            (
                (12_288, 2_688, 7, 512, 512),
                vec![(124, 224, 0x2520_2020), (124, 400, 0x1e1c_1c1b)],
            ),
            (
                (0, 0, 8, 448, 298),
                vec![
                    (110, 108, 0xd8cc_cccc),
                    (39, 162, 0xd5c9_c9c9),
                    (110, 162, 0xaea3_a3a3),
                ],
            ),
        ] {
            let mut pixels = vec![0; (width * height) as usize];
            openslide_read_region(osr, pixels.as_mut_ptr(), x, y, level, width, height);
            assert!(openslide_get_error(osr).is_null());
            for (col, row, expected) in samples {
                assert_eq!(
                    pixels[row * width as usize + col],
                    expected,
                    "level {level} pixel ({col}, {row})"
                );
            }
        }
        openslide_close(osr);
    }
}
