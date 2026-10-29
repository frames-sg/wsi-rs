use super::*;
use crate::test_support::{regular_rgb_dataset_for_test, RegularLevelForTest};
use std::path::Path;

/// A 3x2 grid whose tiles cover each location kind.
struct LocationSource {
    dataset: Dataset,
}

impl LocationSource {
    fn new() -> Self {
        Self {
            dataset: regular_rgb_dataset_for_test(
                DatasetId::new(7),
                "s0",
                "ser0",
                RegularLevelForTest {
                    dimensions: (40, 32),
                    tile_width: 16,
                    tile_height: 16,
                    tiles_across: 3,
                    tiles_down: 2,
                },
            ),
        }
    }
}

fn stored(col: i64, row: i64) -> TileLocation {
    let codec = EncodedCodec::new(
        Compression::Jpeg,
        8,
        3,
        EncodedTilePhotometricInterpretation::YbrFull422,
    );
    let offset = 1000 + 100 * (row * 3 + col) as u64;
    let unit = EncodedUnit::builder(Arc::from(Path::new("/slides/grid.tif")), codec, 16, 16)
        .segment(ByteRange::new(offset, 64))
        .build(10_000)
        .unwrap();
    TileLocation::Stored(StoredTile::new(16, 16, vec![unit]).unwrap())
}

impl SlideReader for LocationSource {
    fn dataset(&self) -> &Dataset {
        &self.dataset
    }

    fn read_tile_cpu(&self, _req: &TileRequest) -> Result<CpuTile, WsiError> {
        unreachable!("tile locations must not decode pixels")
    }

    fn read_raw_compressed_tile(&self, _req: &TileRequest) -> Result<RawCompressedTile, WsiError> {
        unreachable!("tile locations must not read tile bodies")
    }

    fn tile_location(&self, req: &TileRequest) -> Result<TileLocation, WsiError> {
        Ok(match (req.col, req.row) {
            (1, 0) => TileLocation::Absent,
            (2, 1) => TileLocation::NotAddressable(NotAddressableReason::Resampled),
            (col, row) => stored(col, row),
        })
    }
}

fn slide() -> Slide {
    Slide::from_source_with_cache_bytes(Box::new(LocationSource::new()), 0)
}

#[test]
fn tile_location_forwards_through_managed_wrappers() {
    let slide = slide();

    assert_eq!(
        slide
            .tile_location(&TileRequest::new(0, 0, 0, 1, 0))
            .unwrap(),
        TileLocation::Absent
    );
    assert_eq!(
        slide
            .tile_location(&TileRequest::new(0, 0, 0, 2, 1))
            .unwrap(),
        TileLocation::NotAddressable(NotAddressableReason::Resampled)
    );
    assert_eq!(
        slide
            .tile_location(&TileRequest::new(0, 0, 0, 1, 1))
            .unwrap(),
        stored(1, 1)
    );
}

#[test]
fn tile_location_validates_indices_before_the_reader() {
    let slide = slide();

    assert!(matches!(
        slide.tile_location(&TileRequest::new(0, 0, 3, 0, 0)),
        Err(WsiError::LevelOutOfRange { level: 3, .. })
    ));
    assert!(matches!(
        slide.tile_location(&TileRequest::new(1, 0, 0, 0, 0)),
        Err(WsiError::SceneOutOfRange { .. })
    ));
    let plane = TileRequest::new(0, 0, 0, 0, 0).with_plane(PlaneSelection::new(0, 1, 0));
    assert!(matches!(
        slide.tile_location(&plane),
        Err(WsiError::PlaneOutOfRange { .. })
    ));
}

#[test]
fn level_tile_locations_lists_the_grid_in_row_major_order() {
    let locations = slide()
        .level_tile_locations(0, 0, 0, PlaneSelection::default())
        .unwrap();

    assert_eq!((locations.tile_width, locations.tile_height), (16, 16));
    assert_eq!((locations.tiles_across, locations.tiles_down), (3, 2));
    let cells: Vec<(i64, i64)> = locations.tiles.iter().map(|t| (t.col, t.row)).collect();
    assert_eq!(cells, [(0, 0), (1, 0), (2, 0), (0, 1), (1, 1), (2, 1)]);
    assert_eq!(locations.tiles[0].location, stored(0, 0));
    assert_eq!(locations.tiles[1].location, TileLocation::Absent);
    assert_eq!(
        locations.tiles[5].location,
        TileLocation::NotAddressable(NotAddressableReason::Resampled)
    );
}

#[test]
fn level_tile_locations_respects_the_tile_index_budget() {
    let mut slide = slide();
    slide.limits = SlideLimits::default().with_tile_index_bytes(64).unwrap();

    let error = slide
        .level_tile_locations(0, 0, 0, PlaneSelection::default())
        .unwrap_err()
        .to_string();
    assert!(error.contains("64-byte tile index limit"), "{error}");
}

#[test]
fn level_tile_locations_names_the_tile_whose_location_fails() {
    struct FailingSource(LocationSource);
    impl SlideReader for FailingSource {
        fn dataset(&self) -> &Dataset {
            self.0.dataset()
        }
        fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
            self.0.read_tile_cpu(req)
        }
        fn tile_location(&self, req: &TileRequest) -> Result<TileLocation, WsiError> {
            if (req.col, req.row) == (1, 1) {
                return Err(WsiError::Unsupported {
                    reason: "corrupt offset".into(),
                });
            }
            self.0.tile_location(req)
        }
    }
    let slide =
        Slide::from_source_with_cache_bytes(Box::new(FailingSource(LocationSource::new())), 0);

    let error = slide
        .level_tile_locations(0, 0, 0, PlaneSelection::default())
        .unwrap_err();
    assert!(
        matches!(&error, WsiError::TileRead { col: 1, row: 1, reason, .. } if reason.contains("corrupt offset")),
        "{error:?}"
    );
}

#[test]
fn level_grid_lists_irregular_entries_row_major() {
    let mut tiles = std::collections::HashMap::new();
    for key in [(3, 1), (-1, 0), (2, 0), (0, 1)] {
        tiles.insert(key, TileEntry::new((0.0, 0.0), (10, 10)));
    }
    let grid = LevelGrid::of(&TileLayout::Irregular {
        tile_advance: (9.5, 10.0),
        extra_tiles: (0, 0, 0, 0),
        tiles,
    })
    .unwrap();

    assert_eq!(
        grid.cells().collect::<Vec<_>>(),
        [(-1, 0), (2, 0), (0, 1), (3, 1)]
    );
    assert_eq!((grid.tiles_across, grid.tiles_down), (5, 2));
    assert_eq!((grid.tile_width, grid.tile_height), (10, 10));
}

#[test]
fn level_grid_covers_whole_level_layouts_with_partial_edge_tiles() {
    let grid = LevelGrid::of(&TileLayout::WholeLevel {
        width: 1000,
        height: 300,
        virtual_tile_width: 256,
        virtual_tile_height: 256,
    })
    .unwrap();

    assert_eq!((grid.tiles_across, grid.tiles_down), (4, 2));
    assert_eq!(grid.count(), 8);
    assert!(LevelGrid::of(&TileLayout::WholeLevel {
        width: 1,
        height: 1,
        virtual_tile_width: 0,
        virtual_tile_height: 1,
    })
    .is_err());
}

#[test]
fn retained_bytes_count_shared_prefixes_once() {
    let prefix: Arc<[u8]> = Arc::from(vec![0u8; 1000]);
    let location = || {
        let codec = EncodedCodec::new(
            Compression::Jpeg,
            8,
            3,
            EncodedTilePhotometricInterpretation::YbrFull422,
        );
        let unit = EncodedUnit::builder(Arc::from(Path::new("/slides/a.ndpi")), codec, 16, 16)
            .segment(ByteRange::new(0, 10))
            .framing(StoredFraming::Prefixed)
            .prefix_suffix(prefix.clone(), Arc::from(&[0xFF, 0xD9][..]))
            .build(10)
            .unwrap();
        TileLocation::Stored(StoredTile::new(16, 16, vec![unit]).unwrap())
    };
    let mut retained = RetainedBytes::default();
    let first = retained.add(&location()).unwrap();
    let second = retained.add(&location()).unwrap();

    assert!(first > 1000, "{first}");
    // The second tile adds its own suffix buffer but not the shared prefix.
    assert!(second - first < 1000, "{first} {second}");
}
