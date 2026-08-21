use super::super::*;

#[test]
fn regular_tiles_for_region_basic() {
    let layout = TileLayout::Regular {
        tile_width: 256,
        tile_height: 256,
        tiles_across: 4,
        tiles_down: 4,
    };
    // 300x300 at (100, 100) → cols 0-1, rows 0-1 → 4 tiles
    let tiles = layout.tiles_for_region(100, 100, 300, 300);
    assert_eq!(tiles.len(), 4);
    let coords: Vec<(i64, i64)> = tiles.iter().map(|t| (t.col, t.row)).collect();
    assert!(coords.contains(&(0, 0)));
    assert!(coords.contains(&(1, 0)));
    assert!(coords.contains(&(0, 1)));
    assert!(coords.contains(&(1, 1)));
}

#[test]
fn regular_tiles_single_tile() {
    let layout = TileLayout::Regular {
        tile_width: 256,
        tile_height: 256,
        tiles_across: 4,
        tiles_down: 4,
    };
    let tiles = layout.tiles_for_region(0, 0, 100, 100);
    assert_eq!(tiles.len(), 1);
    assert_eq!(tiles[0].col, 0);
    assert_eq!(tiles[0].row, 0);
}

#[test]
fn regular_tiles_clipped_at_bounds() {
    let layout = TileLayout::Regular {
        tile_width: 256,
        tile_height: 256,
        tiles_across: 2,
        tiles_down: 2,
    };
    // Region extends beyond grid
    let tiles = layout.tiles_for_region(256, 256, 512, 512);
    assert_eq!(tiles.len(), 1);
    assert_eq!(tiles[0].col, 1);
    assert_eq!(tiles[0].row, 1);
}

#[test]
fn regular_tiles_negative_coords() {
    let layout = TileLayout::Regular {
        tile_width: 256,
        tile_height: 256,
        tiles_across: 4,
        tiles_down: 4,
    };
    // Negative start — only in-bounds tiles returned
    let tiles = layout.tiles_for_region(-100, -100, 200, 200);
    assert_eq!(tiles.len(), 1);
    assert_eq!(tiles[0].col, 0);
    assert_eq!(tiles[0].row, 0);
}

#[test]
fn regular_fractional_region_preserves_subpixel_tile_placement() {
    let layout = TileLayout::Regular {
        tile_width: 4,
        tile_height: 4,
        tiles_across: 2,
        tiles_down: 2,
    };

    let tiles = layout.tiles_for_fractional_region(3.75, 1.25, 2, 2);

    assert_eq!(tiles.len(), 2);
    assert_eq!((tiles[0].col, tiles[0].row), (1, 0));
    assert_eq!((tiles[0].dest_x_f64, tiles[0].dest_y_f64), (0.25, -1.25));
    assert_eq!((tiles[1].col, tiles[1].row), (0, 0));
    assert_eq!((tiles[1].dest_x_f64, tiles[1].dest_y_f64), (-3.75, -1.25));
}

#[test]
fn regular_fractional_region_retains_filter_and_fixed_raster_coordinates() {
    let layout = TileLayout::Regular {
        tile_width: 4,
        tile_height: 4,
        tiles_across: 2,
        tiles_down: 2,
    };

    let tiles = layout.tiles_for_fractional_region(3.995, 0.0, 2, 1);

    assert_eq!(tiles.len(), 2);
    assert_eq!(tiles[0].dest_x_f64, 4.0 - 3.995);
    assert_eq!(tiles[1].dest_x_f64, -3.995);
    assert_eq!(tiles[0].cairo_fixed_dest.unwrap().0, 0.005_004_882_812_5);
    assert_eq!(tiles[1].cairo_fixed_dest.unwrap().0, -3.994_995_117_187_5);
}

#[test]
fn whole_level_tiles_for_region() {
    let layout = TileLayout::WholeLevel {
        width: 1024,
        height: 768,
        virtual_tile_width: 256,
        virtual_tile_height: 256,
    };
    // Region covering the entire image → ceil(1024/256) * ceil(768/256) = 4*3 = 12 tiles
    let tiles = layout.tiles_for_region(0, 0, 1024, 768);
    assert_eq!(tiles.len(), 12);
}

#[test]
fn whole_level_small_region() {
    let layout = TileLayout::WholeLevel {
        width: 4096,
        height: 4096,
        virtual_tile_width: 512,
        virtual_tile_height: 512,
    };
    // 100x100 at origin → 1 tile
    let tiles = layout.tiles_for_region(0, 0, 100, 100);
    assert_eq!(tiles.len(), 1);
    assert_eq!(tiles[0].col, 0);
    assert_eq!(tiles[0].row, 0);
}

#[test]
fn whole_level_negative_coords_clamp_to_first_tile() {
    let layout = TileLayout::WholeLevel {
        width: 1024,
        height: 1024,
        virtual_tile_width: 256,
        virtual_tile_height: 256,
    };

    let tiles = layout.tiles_for_region(-300, -300, 400, 400);
    assert_eq!(tiles.len(), 1);
    assert_eq!(tiles[0].col, 0);
    assert_eq!(tiles[0].row, 0);
    assert_eq!(tiles[0].dest_x, 300);
    assert_eq!(tiles[0].dest_y, 300);
}

#[test]
fn irregular_tiles_for_region_basic() {
    let mut tiles_map = std::collections::HashMap::new();
    tiles_map.insert(
        (0i64, 0i64),
        TileEntry {
            offset: (0.0, 0.0),
            dimensions: (256, 256),
            tiff_tile_index: None,
        },
    );
    tiles_map.insert(
        (1, 0),
        TileEntry {
            offset: (5.0, 0.0),
            dimensions: (256, 256),
            tiff_tile_index: None,
        },
    );
    tiles_map.insert(
        (0, 1),
        TileEntry {
            offset: (0.0, 3.0),
            dimensions: (256, 256),
            tiff_tile_index: None,
        },
    );

    let layout = TileLayout::Irregular {
        tile_advance: (256.0, 256.0),
        extra_tiles: (1, 0, 1, 0),
        tiles: tiles_map,
    };

    let result = layout.tiles_for_region(0, 0, 512, 512);
    assert_eq!(result.len(), 3);
}

#[test]
fn irregular_tiles_negative_offset() {
    let mut tiles_map = std::collections::HashMap::new();
    tiles_map.insert(
        (0i64, 0i64),
        TileEntry {
            offset: (-10.0, -5.0),
            dimensions: (256, 256),
            tiff_tile_index: None,
        },
    );

    let layout = TileLayout::Irregular {
        tile_advance: (256.0, 256.0),
        extra_tiles: (0, 1, 0, 1),
        tiles: tiles_map,
    };

    // Tile actual position is (-10, -5) to (246, 251)
    // Region (0, 0, 100, 100) should hit it
    let result = layout.tiles_for_region(0, 0, 100, 100);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].dest_x, -10);
    assert_eq!(result[0].dest_y, -5);
}

#[test]
fn irregular_tiles_no_match() {
    let mut tiles_map = std::collections::HashMap::new();
    tiles_map.insert(
        (0i64, 0i64),
        TileEntry {
            offset: (0.0, 0.0),
            dimensions: (256, 256),
            tiff_tile_index: None,
        },
    );

    let layout = TileLayout::Irregular {
        tile_advance: (256.0, 256.0),
        extra_tiles: (0, 0, 0, 0),
        tiles: tiles_map,
    };

    let result = layout.tiles_for_region(10000, 10000, 100, 100);
    assert_eq!(result.len(), 0);
}

#[test]
fn tile_layout_zero_tile_dimensions_return_no_hits_instead_of_panicking() {
    let regular = TileLayout::Regular {
        tile_width: 0,
        tile_height: 256,
        tiles_across: 1,
        tiles_down: 1,
    };
    assert!(regular.tiles_for_region(0, 0, 64, 64).is_empty());

    let regular = TileLayout::Regular {
        tile_width: 256,
        tile_height: 0,
        tiles_across: 1,
        tiles_down: 1,
    };
    assert!(regular.tiles_for_region(0, 0, 64, 64).is_empty());

    let whole_level = TileLayout::WholeLevel {
        width: 1024,
        height: 1024,
        virtual_tile_width: 0,
        virtual_tile_height: 256,
    };
    assert!(whole_level.tiles_for_region(0, 0, 64, 64).is_empty());

    let whole_level = TileLayout::WholeLevel {
        width: 1024,
        height: 1024,
        virtual_tile_width: 256,
        virtual_tile_height: 0,
    };
    assert!(whole_level.tiles_for_region(0, 0, 64, 64).is_empty());
}

#[test]
fn tile_layout_extreme_region_coordinates_return_no_hits_instead_of_panicking() {
    let regular = TileLayout::Regular {
        tile_width: 256,
        tile_height: 256,
        tiles_across: 4,
        tiles_down: 4,
    };
    assert!(regular
        .tiles_for_region(i64::MAX - 8, i64::MAX - 8, 64, 64)
        .is_empty());
    assert!(regular
        .tiles_for_region(i64::MIN + 8, i64::MIN + 8, 64, 64)
        .is_empty());

    let whole_level = TileLayout::WholeLevel {
        width: 1024,
        height: 1024,
        virtual_tile_width: 256,
        virtual_tile_height: 256,
    };
    assert!(whole_level
        .tiles_for_region(i64::MAX - 8, i64::MAX - 8, 64, 64)
        .is_empty());
    assert!(whole_level
        .tiles_for_region(i64::MIN + 8, i64::MIN + 8, 64, 64)
        .is_empty());
}

// --- Compression ---
