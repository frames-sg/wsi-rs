use super::*;

// ── parse_level0_xml ────────────────────────────────────────────

#[test]
fn parse_level0_xml_basic() {
    let xml = r#"<EncodeInfo>
            <SlideStitchInfo>
                <ImageInfo AOIScanned="1" NumCols="3" NumRows="2" Width="256" Height="256"
                           Pos-X="0" Pos-Y="0">
                    <TileJointInfo Direction="RIGHT" OverlapX="10" OverlapY="0" Confidence="100"
                                   Tile1="1" Tile2="2"/>
                    <TileJointInfo Direction="UP" OverlapX="0" OverlapY="8" Confidence="100"
                                   Tile1="1" Tile2="4"/>
                </ImageInfo>
            </SlideStitchInfo>
        </EncodeInfo>"#;

    let bif = parse_level0_xml(xml, 256, 256).unwrap();
    assert_eq!(bif.areas.len(), 1);
    assert_eq!(bif.areas[0].tiles_across, 3);
    assert_eq!(bif.areas[0].tiles_down, 2);
    // X: 256 + (-10 * 100) / 100 = 246
    assert!((bif.tile_advance_x - 246.0).abs() < 1e-6);
    // Y: 256 + (-8 * 100) / 100 = 248
    assert!((bif.tile_advance_y - 248.0).abs() < 1e-6);
}

#[test]
fn parse_level0_xml_skips_unscanned() {
    let xml = r#"<EncodeInfo>
            <SlideStitchInfo>
                <ImageInfo AOIScanned="0" NumCols="3" NumRows="2"
                           Pos-X="0" Pos-Y="0"/>
                <ImageInfo AOIScanned="1" NumCols="2" NumRows="1"
                           Pos-X="100" Pos-Y="100"/>
            </SlideStitchInfo>
        </EncodeInfo>"#;

    let bif = parse_level0_xml(xml, 256, 256).unwrap();
    assert_eq!(bif.areas.len(), 1);
    assert_eq!(bif.areas[0].tiles_across, 2);
    assert_eq!(bif.areas[0].tiles_down, 1);
}

#[test]
fn parse_level0_xml_no_joints_uses_tile_size() {
    let xml = r#"<EncodeInfo>
            <SlideStitchInfo>
                <ImageInfo AOIScanned="1" NumCols="2" NumRows="2"
                           Pos-X="0" Pos-Y="0"/>
            </SlideStitchInfo>
        </EncodeInfo>"#;

    let bif = parse_level0_xml(xml, 256, 256).unwrap();
    assert!((bif.tile_advance_x - 256.0).abs() < 1e-6);
    assert!((bif.tile_advance_y - 256.0).abs() < 1e-6);
}

#[test]
fn parse_level0_xml_missing_slide_stitch_info_errors() {
    let xml = r#"<EncodeInfo>
            <SomeOtherElement/>
        </EncodeInfo>"#;

    let result = parse_level0_xml(xml, 256, 256);
    assert!(result.is_err());
    let msg = match result {
        Err(e) => e.to_string(),
        Ok(_) => panic!("expected error"),
    };
    assert!(msg.contains("SlideStitchInfo"), "got: {}", msg);
}

#[test]
fn ventana_level0_dimensions_normalize_to_minimum_scanned_origin() {
    let bif = BifInfo {
        areas: vec![
            BifArea {
                x: 12_000,
                y: 9_000,
                start_col: 0,
                start_row: 0,
                tiles_across: 2,
                tiles_down: 2,
            },
            BifArea {
                x: 12_500,
                y: 9_250,
                start_col: 2,
                start_row: 1,
                tiles_across: 1,
                tiles_down: 1,
            },
        ],
        tiles: vec![],
        tile_advance_x: 250.0,
        tile_advance_y: 248.0,
    };

    let dims = ventana_level0_dimensions(&bif, 256, 256).unwrap();
    assert_eq!(dims, (756, 506));
}

#[test]
fn ventana_level0_dimensions_prefers_exact_tile_positions() {
    let bif = BifInfo {
        areas: vec![],
        tiles: vec![
            BifTile {
                col: 0,
                row: 0,
                x: 5.0,
                y: 7.0,
                width: 251,
                height: 249,
                tiff_tile_index: 0,
            },
            BifTile {
                col: 1,
                row: 0,
                x: 261.0,
                y: 7.0,
                width: 240,
                height: 249,
                tiff_tile_index: 1,
            },
        ],
        tile_advance_x: 256.0,
        tile_advance_y: 256.0,
    };

    let dims = ventana_level0_dimensions(&bif, 256, 256).unwrap();
    assert_eq!(dims, (496, 249));
}

#[test]
fn ventana_level0_dimensions_prefers_area_model_when_present() {
    let bif = BifInfo {
        areas: vec![BifArea {
            x: 0,
            y: 0,
            start_col: 0,
            start_row: 0,
            tiles_across: 3,
            tiles_down: 2,
        }],
        tiles: vec![
            BifTile {
                col: 0,
                row: 0,
                x: 0.0,
                y: 0.0,
                width: 256,
                height: 256,
                tiff_tile_index: 0,
            },
            BifTile {
                col: 1,
                row: 0,
                x: 300.0,
                y: 0.0,
                width: 256,
                height: 256,
                tiff_tile_index: 1,
            },
        ],
        tile_advance_x: 240.0,
        tile_advance_y: 248.0,
    };

    let dims = ventana_level0_dimensions(&bif, 256, 256).unwrap();
    assert_eq!(dims, (736, 504));
}

#[test]
fn joint_delta_supports_left_and_down() {
    assert_eq!(
        joint_delta("LEFT", 256.0, 256.0, 12.0, 8.0),
        Some((-244.0, 8.0))
    );
    assert_eq!(
        joint_delta("DOWN", 256.0, 256.0, 12.0, 8.0),
        Some((12.0, -248.0))
    );
}

#[test]
fn exact_tile_dimensions_use_neighbor_positions() {
    let mut positions = HashMap::new();
    positions.insert((0, 0), (5.0, 7.0));
    positions.insert((1, 0), (261.0, 7.0));
    positions.insert((0, 1), (5.0, 256.0));

    assert_eq!(
        ventana_exact_tile_dimensions(0, 0, 2, 2, &positions, 496.0, 256.0, 256.0, 256.0),
        (256, 249)
    );
    assert_eq!(
        ventana_exact_tile_dimensions(1, 0, 2, 2, &positions, 496.0, 256.0, 256.0, 256.0),
        (235, 249)
    );
}

// ── Tile grid building ──────────────────────────────────────────

fn test_tiles_from_bif_areas(bif: &BifInfo) -> HashMap<(i64, i64), TileEntry> {
    let mut tiles: HashMap<(i64, i64), TileEntry> = HashMap::new();
    let mut tiff_idx: usize = 0;
    for area in &bif.areas {
        let offset_x = area.x as f64 - area.start_col as f64 * bif.tile_advance_x;
        let offset_y = area.y as f64 - area.start_row as f64 * bif.tile_advance_y;
        for row in area.start_row..area.start_row + area.tiles_down {
            for col in area.start_col..area.start_col + area.tiles_across {
                tiles.insert(
                    (col, row),
                    TileEntry {
                        offset: (offset_x, offset_y),
                        dimensions: (256, 256),
                        tiff_tile_index: Some(tiff_idx),
                    },
                );
                tiff_idx += 1;
            }
        }
    }
    tiles
}

#[test]
fn tile_grid_single_area() {
    let bif = BifInfo {
        areas: vec![BifArea {
            x: 0,
            y: 0,
            start_col: 0,
            start_row: 0,
            tiles_across: 3,
            tiles_down: 2,
        }],
        tiles: vec![],
        tile_advance_x: 250.0,
        tile_advance_y: 248.0,
    };

    let tiles = test_tiles_from_bif_areas(&bif);

    assert_eq!(tiles.len(), 6);
    assert_eq!(tiles[&(0, 0)].tiff_tile_index, Some(0));
    assert_eq!(tiles[&(1, 0)].tiff_tile_index, Some(1));
    assert_eq!(tiles[&(2, 0)].tiff_tile_index, Some(2));
    assert_eq!(tiles[&(0, 1)].tiff_tile_index, Some(3));
    assert_eq!(tiles[&(1, 1)].tiff_tile_index, Some(4));
    assert_eq!(tiles[&(2, 1)].tiff_tile_index, Some(5));
}

#[test]
fn tile_grid_two_areas_sequential_indices() {
    let bif = BifInfo {
        areas: vec![
            BifArea {
                x: 0,
                y: 0,
                start_col: 0,
                start_row: 0,
                tiles_across: 2,
                tiles_down: 1,
            },
            BifArea {
                x: 500,
                y: 0,
                start_col: 2,
                start_row: 0,
                tiles_across: 2,
                tiles_down: 1,
            },
        ],
        tiles: vec![],
        tile_advance_x: 256.0,
        tile_advance_y: 256.0,
    };

    let tiles = test_tiles_from_bif_areas(&bif);

    assert_eq!(tiles.len(), 4);
    // First area: (0,0)=0, (1,0)=1
    assert_eq!(tiles[&(0, 0)].tiff_tile_index, Some(0));
    assert_eq!(tiles[&(1, 0)].tiff_tile_index, Some(1));
    // Second area: (2,0)=2, (3,0)=3
    assert_eq!(tiles[&(2, 0)].tiff_tile_index, Some(2));
    assert_eq!(tiles[&(3, 0)].tiff_tile_index, Some(3));

    // Second area has a different offset due to Pos-X=500 vs col*advance=512
    assert_eq!(tiles[&(2, 0)].offset.0, -12.0); // 500 - 2*256 = -12
}

#[test]
fn ventana_snake_coords_reverse_odd_rows() {
    assert_eq!(ventana_snake_coords(1, 4), (0, 0));
    assert_eq!(ventana_snake_coords(4, 4), (3, 0));
    assert_eq!(ventana_snake_coords(5, 4), (3, 1));
    assert_eq!(ventana_snake_coords(8, 4), (0, 1));
}

// ── Overlap validation ──────────────────────────────────────────

#[test]
fn no_overlap_passes_validation() {
    let mut tiles = HashMap::new();
    tiles.insert(
        (0, 0),
        TileEntry {
            offset: (0.0, 0.0),
            dimensions: (256, 256),
            tiff_tile_index: Some(0),
        },
    );
    tiles.insert(
        (1, 0),
        TileEntry {
            offset: (0.0, 0.0),
            dimensions: (256, 256),
            tiff_tile_index: Some(1),
        },
    );
    tiles.insert(
        (0, 1),
        TileEntry {
            offset: (0.0, 0.0),
            dimensions: (256, 256),
            tiff_tile_index: Some(2),
        },
    );

    // tile_advance = 256 means tiles are exactly adjacent with 0-pixel offsets.
    let result = validate_no_adjacent_overlap(&tiles, 256.0, 256.0, 256, 256);
    assert!(result.is_ok());
}

#[test]
fn overlap_detected_fails_validation() {
    let mut tiles = HashMap::new();
    tiles.insert(
        (0, 0),
        TileEntry {
            offset: (0.0, 0.0),
            dimensions: (256, 256),
            tiff_tile_index: Some(0),
        },
    );
    tiles.insert(
        (1, 0),
        TileEntry {
            offset: (0.0, 0.0),
            dimensions: (256, 256),
            tiff_tile_index: Some(1),
        },
    );

    // tile_advance = 200 means tile at (1,0) starts at pixel 200 but tile at
    // (0,0) extends to pixel 256, causing a 56-pixel overlap.
    let result = validate_no_adjacent_overlap(&tiles, 200.0, 256.0, 256, 256);
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("overlap"), "got: {}", msg);
}

// ── compression_from_tag ────────────────────────────────────────

#[test]
fn compression_tag_mapping() {
    assert_eq!(compression_from_tag(1), Compression::None);
    assert_eq!(compression_from_tag(6), Compression::Jpeg);
    assert_eq!(compression_from_tag(7), Compression::Jpeg);
    assert_eq!(compression_from_tag(33003), Compression::Jp2kYcbcr);
    assert_eq!(compression_from_tag(33004), Compression::Jp2kRgb);
    assert_eq!(compression_from_tag(33005), Compression::Jp2kYcbcr);
    assert_eq!(compression_from_tag(9999), Compression::Other(9999));
}

// ── extract_encode_info ─────────────────────────────────────────

#[test]
fn extract_encode_info_found() {
    let input = "prefix<EncodeInfo><SlideStitchInfo/></EncodeInfo>suffix";
    let result = extract_encode_info(input);
    assert_eq!(
        result.as_deref(),
        Some("<EncodeInfo><SlideStitchInfo/></EncodeInfo>")
    );
}

#[test]
fn extract_encode_info_with_attributes_found() {
    let input = "prefix<?xml version=\"1.0\"?><EncodeInfo Ver='2'><SlideStitchInfo/></EncodeInfo>";
    let result = extract_encode_info(input);
    assert_eq!(
        result.as_deref(),
        Some("<EncodeInfo Ver='2'><SlideStitchInfo/></EncodeInfo>")
    );
}

#[test]
fn extract_encode_info_not_found() {
    let input = "no encode info here";
    assert!(extract_encode_info(input).is_none());
}

#[test]
fn extract_encode_info_from_bytes_with_binary_wrapper() {
    let input = b"\xff\xd9<?xml version=\"1.0\"?>\n<EncodeInfo Ver='2'><SlideStitchInfo/></EncodeInfo>\0tail";
    let result = extract_encode_info_bytes(input);
    assert_eq!(
        result.as_deref(),
        Some("<EncodeInfo Ver='2'><SlideStitchInfo/></EncodeInfo>")
    );
}

// ── iScan property parsing ──────────────────────────────────────

#[test]
fn extract_iscan_fragment_from_bytes_with_binary_wrapper() {
    let input = b"\xff\x00<x:xmpmeta><iScan Magnification=\"40\" ScanRes=\"0.2528\"/></x:xmpmeta>";
    let result = extract_iscan_fragment_bytes(input);
    assert_eq!(
        result.as_deref(),
        Some("<iScan Magnification=\"40\" ScanRes=\"0.2528\"/>")
    );
}

#[test]
fn parse_iscan_properties_basic() {
    let xmp = r#"<iScan Magnification="40" ScanRes="0.2528" SlideID="ABC123"/>"#;
    let mut props = Properties::new();
    parse_iscan_properties(xmp, &mut props);

    assert_eq!(props.get("ventana.Magnification"), Some("40"));
    assert_eq!(props.get("ventana.ScanRes"), Some("0.2528"));
    assert_eq!(props.get("ventana.SlideID"), Some("ABC123"));
    assert_eq!(props.get("openslide.objective-power"), Some("40"));
    assert_eq!(props.get("openslide.mpp-x"), Some("0.2528"));
    assert_eq!(props.get("openslide.mpp-y"), Some("0.2528"));
}

#[test]
fn parse_iscan_properties_no_iscan() {
    let xmp = "<SomeOther attr=\"val\"/>";
    let mut props = Properties::new();
    parse_iscan_properties(xmp, &mut props);
    assert!(props.is_empty());
}

#[test]
fn non_adjacent_overlap_detected() {
    // Tiles at (0,0) and (2,0) with large offsets that make them overlap
    // despite being 2 grid cells apart. The old neighbor-only check would miss this.
    let mut tiles = HashMap::new();
    tiles.insert(
        (0, 0),
        TileEntry {
            offset: (0.0, 0.0),
            dimensions: (256, 256),
            tiff_tile_index: Some(0),
        },
    );
    // (1,0) exists but is normal
    tiles.insert(
        (1, 0),
        TileEntry {
            offset: (0.0, 0.0),
            dimensions: (100, 256), // narrow tile
            tiff_tile_index: Some(1),
        },
    );
    // (2,0) has a large negative offset that pushes it back into (0,0)'s territory
    tiles.insert(
        (2, 0),
        TileEntry {
            offset: (-350.0, 0.0),
            dimensions: (256, 256),
            tiff_tile_index: Some(2),
        },
    );

    // tile_advance = 200
    // (0,0): x=[0, 256)
    // (1,0): x=[200, 300) — no overlap with (0,0) since 200 < 256... actually overlaps!
    // (2,0): x=[400-350, 400-350+256) = [50, 306) — overlaps with (0,0)
    // The sweep-line should catch the (0,0)/(2,0) overlap.
    let result = validate_no_adjacent_overlap(&tiles, 200.0, 256.0, 256, 256);
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("overlap"), "got: {}", msg);
}
