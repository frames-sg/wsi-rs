use super::*;
use crate::formats::tiff_family::test_support::{build_tiff, SyntheticTag};

fn tiled_ifd(width: u32, height: u32, tile_width: u32, tile_height: u32) -> Vec<SyntheticTag> {
    vec![
        SyntheticTag::long(tags::IMAGE_WIDTH, width),
        SyntheticTag::long(tags::IMAGE_LENGTH, height),
        SyntheticTag::long(tags::TILE_WIDTH, tile_width),
        SyntheticTag::long(tags::TILE_LENGTH, tile_height),
        SyntheticTag::short(tags::COMPRESSION, 7),
    ]
}

#[test]
fn interpret_builds_overlap_adjusted_levels_and_properties() {
    let mut first = tiled_ifd(512, 512, 256, 256);
    first.extend([
        SyntheticTag::ascii(TAG_SOFTWARE, "MedScan 2.0"),
        SyntheticTag::ascii(
            tags::IMAGE_DESCRIPTION,
            "Background Color=E6E6E6;White Balance=C0AAA1;Objective Power=10;OverlapsXY=16 8 4 2",
        ),
        SyntheticTag::ascii(super::super::TAG_DATETIME, "2026:01:02 03:04:05"),
        SyntheticTag::ascii(super::super::TAG_HOST_COMPUTER, "scanner-host"),
    ]);
    let file = build_tiff(&[first, tiled_ifd(256, 256, 128, 128)]);
    let container = TiffContainer::open(file.path()).unwrap();
    let interpreter = TrestleInterpreter;

    assert!(interpreter.detect(&container));
    assert_eq!(interpreter.vendor_name(), "trestle");
    let layout = interpreter.interpret(&container).unwrap();

    let levels = &layout.dataset.scenes[0].series[0].levels;
    assert_eq!(levels.len(), 2);
    assert_eq!(levels[0].dimensions, (496, 504));
    assert_eq!(levels[1].dimensions, (252, 254));
    assert!(matches!(
        levels[0].tile_layout,
        TileLayout::Irregular { .. }
    ));
    assert_eq!(layout.tile_sources.len(), 2);
    assert!(layout.associated_sources.is_empty());
    let properties = &layout.dataset.properties;
    assert_eq!(properties.get("openslide.vendor"), Some("trestle"));
    assert_eq!(properties.get("openslide.objective-power"), Some("10"));
    assert_eq!(properties.get("openslide.background-color"), Some("E6E6E6"));
    assert_eq!(properties.get("trestle.White Balance"), Some("C0AAA1"));
    assert_eq!(properties.get("tiff.DateTime"), Some("2026:01:02 03:04:05"));
    assert_eq!(properties.get("tiff.HostComputer"), Some("scanner-host"));
}

#[test]
fn interpret_rejects_overlap_that_consumes_a_tile() {
    let mut tags = tiled_ifd(512, 512, 256, 256);
    tags.extend([
        SyntheticTag::ascii(TAG_SOFTWARE, "MedScan"),
        SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, "OverlapsXY=256 0"),
    ]);
    let file = build_tiff(&[tags]);
    let container = TiffContainer::open(file.path()).unwrap();
    let error = TrestleInterpreter.interpret(&container).unwrap_err();
    assert!(error.to_string().contains("consumes tile 256x256"));
}

#[test]
fn detect_requires_vendor_description_and_all_tiled_ifds() {
    let missing_vendor = build_tiff(&[tiled_ifd(64, 64, 32, 32)]);
    assert!(!TrestleInterpreter.detect(&TiffContainer::open(missing_vendor.path()).unwrap()));

    let file = build_tiff(&[
        vec![
            SyntheticTag::ascii(TAG_SOFTWARE, "MedScan"),
            SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, "Objective Power=20"),
            SyntheticTag::long(tags::IMAGE_WIDTH, 64),
            SyntheticTag::long(tags::IMAGE_LENGTH, 64),
            SyntheticTag::long(tags::TILE_WIDTH, 32),
            SyntheticTag::long(tags::TILE_LENGTH, 32),
        ],
        vec![
            SyntheticTag::long(tags::IMAGE_WIDTH, 16),
            SyntheticTag::long(tags::IMAGE_LENGTH, 16),
        ],
    ]);
    assert!(!TrestleInterpreter.detect(&TiffContainer::open(file.path()).unwrap()));
}

#[test]
fn parse_trestle_description_extracts_key_value_pairs() {
    let parsed = parse_trestle_description(
        "Background Color=E6E6E6;White Balance=C0AAA1;Objective Power=10;OverlapsXY= 64 64 32 32;bad;empty=",
    );

    assert_eq!(
        parsed.get("Background Color").map(String::as_str),
        Some("E6E6E6")
    );
    assert_eq!(
        parsed.get("White Balance").map(String::as_str),
        Some("C0AAA1")
    );
    assert_eq!(
        parsed.get("Objective Power").map(String::as_str),
        Some("10")
    );
    assert_eq!(
        parsed.get("OverlapsXY").map(String::as_str),
        Some("64 64 32 32")
    );
    assert!(!parsed.contains_key("empty"));
}

#[test]
fn parse_overlap_pairs_groups_values_by_level_and_ignores_tail() {
    assert_eq!(
        parse_overlap_pairs(Some(&"64 64 32 32 16 16 9".to_string())),
        vec![(64, 64), (32, 32), (16, 16)]
    );
    assert!(parse_overlap_pairs(None).is_empty());
}

#[test]
fn trestle_tile_grid_rejects_invalid_and_oversized_dimensions() {
    assert!(checked_trestle_tile_grid(1024, 1024, 0, 256).is_err());
    assert!(checked_trestle_tile_grid(0, 1024, 256, 256).is_err());
    assert_eq!(
        checked_trestle_tile_grid(MAX_TRESTLE_TILES_PER_LEVEL, 1, 1, 1).unwrap(),
        (
            MAX_TRESTLE_TILES_PER_LEVEL,
            1,
            MAX_TRESTLE_TILES_PER_LEVEL as usize
        )
    );
    assert!(checked_trestle_tile_grid(MAX_TRESTLE_TILES_PER_LEVEL + 1, 1, 1, 1).is_err());
    assert!(checked_trestle_tile_grid(u64::MAX, u64::MAX, 1, 1).is_err());
}

#[test]
fn sibling_path_requires_an_existing_sibling() {
    let directory = tempfile::tempdir().unwrap();
    let slide = directory.path().join("sample.tif");
    let sibling = directory.path().join("sample.Full");
    std::fs::write(&slide, []).unwrap();
    assert_eq!(sibling_path(&slide, ".Full"), None);
    std::fs::write(&sibling, []).unwrap();
    assert_eq!(sibling_path(&slide, ".Full"), Some(sibling));
}
