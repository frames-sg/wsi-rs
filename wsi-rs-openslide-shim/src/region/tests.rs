use super::*;
use std::collections::HashMap;
use wsi_rs::{TileEntry, TileLayout};

#[test]
fn opaque_irregular_gaps_are_canonical_transparent_argb() {
    let level = Level::new(
        (3, 1),
        1.0,
        TileLayout::Irregular {
            tile_advance: (2.0, 1.0),
            extra_tiles: (0, 0, 0, 0),
            tiles: HashMap::from([((0, 0), TileEntry::new((0.0, 0.0), (1, 1)))]),
        },
    );
    let mut pixels = [0xff11_2233; 3];

    clear_uncovered_pixels(&level, (0, 0), (3, 1), &mut pixels, true)
        .expect("mark irregular coverage");

    assert_eq!(pixels, [0xff11_2233, 0, 0]);
}

#[test]
fn regular_regions_clear_pixels_outside_level_extent() {
    let level = Level::new(
        (1, 1),
        1.0,
        TileLayout::WholeLevel {
            width: 1,
            height: 1,
            virtual_tile_width: 1,
            virtual_tile_height: 1,
        },
    );
    let mut pixels = [0xff01_0203; 3];

    clear_uncovered_pixels(&level, (-1, 0), (3, 1), &mut pixels, true)
        .expect("clip regular coverage");

    assert_eq!(pixels, [0, 0xff01_0203, 0]);
}

#[test]
fn coverage_rejects_a_destination_with_the_wrong_length() {
    let level = Level::new(
        (1, 1),
        1.0,
        TileLayout::WholeLevel {
            width: 1,
            height: 1,
            virtual_tile_width: 1,
            virtual_tile_height: 1,
        },
    );

    let error = clear_uncovered_pixels(&level, (0, 0), (1, 1), &mut [], true)
        .expect_err("coverage destination length must be exact");

    assert!(error.to_string().contains("has 0 pixels, expected 1"));
}
