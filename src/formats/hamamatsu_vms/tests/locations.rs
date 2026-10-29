use std::fs;

use super::super::*;
use super::fixtures::VmsFixture;
use crate::core::registry::Slide;

fn single_unit(slide: &Slide, req: &TileRequest) -> EncodedUnit {
    let TileLocation::Stored(stored) = slide.tile_location(req).expect("VMS tile location") else {
        panic!("VMS tile {req:?} must be stored");
    };
    stored
        .single_unit()
        .expect("a VMS tile is one restart interval")
        .clone()
}

fn assembled(unit: &EncodedUnit) -> Vec<u8> {
    let file = fs::read(unit.path()).expect("read VMS JPEG");
    unit.assemble(
        unit.segments()
            .iter()
            .map(|range| &file[range.offset as usize..range.end().unwrap() as usize]),
    )
    .expect("assemble VMS tile")
}

#[test]
fn full_resolution_tiles_are_prefixed_restart_intervals() {
    let (fixture, slide) = open();
    for row in 0..2 {
        for col in 0..4 {
            let req = TileRequest::new(0, 0, 0, col, row);
            let unit = single_unit(&slide, &req);
            assert_eq!(
                unit.path().file_name(),
                fixture.image_paths[col as usize / 2].file_name()
            );
            assert_eq!(unit.framing(), StoredFraming::Prefixed);
            assert_eq!(unit.codec().compression(), Compression::Jpeg);
            assert_eq!(unit.codec().samples_per_pixel(), 3);
            assert_eq!(unit.stored_size(), (64, 8));
            assert_eq!(unit.suffix(), [0xFF, 0xD9]);

            // The segment ends just before the interval's RSTn or EOI marker.
            let file = fs::read(unit.path()).unwrap();
            let end = unit.segments()[0].end().unwrap() as usize;
            assert_eq!(file[end], 0xFF);
            assert!(matches!(file[end + 1], 0xD0..=0xD7 | 0xD9));
            // The derived header keeps no restart interval.
            assert!(unit
                .prefix()
                .windows(6)
                .any(|segment| segment == [0xFF, 0xDD, 0, 4, 0, 0]));

            let raw = slide.read_raw_compressed_tile(&req).unwrap();
            assert_eq!(assembled(&unit), raw.data());
            let decoded = crate::decode::jpeg::decode_standalone_jpeg(raw.data()).unwrap();
            assert_eq!(decoded.as_u8(), slide.read_tile(&req).unwrap().as_u8());
        }
    }
}

#[test]
fn map_level_is_stored_and_scaled_levels_are_synthetic() {
    let (fixture, slide) = open();
    let map = single_unit(&slide, &TileRequest::new(0, 0, 2, 0, 0));
    assert_eq!(map.path().file_name(), fixture.map_path.file_name());
    assert_eq!(
        assembled(&map),
        slide
            .read_raw_compressed_tile(&TileRequest::new(0, 0, 2, 0, 0))
            .unwrap()
            .data()
    );

    for (level, base_level, factor) in [(1, 0, 2), (3, 2, 2), (4, 2, 4), (5, 2, 8)] {
        assert_eq!(
            slide
                .tile_location(&TileRequest::new(0, 0, level, 0, 0))
                .unwrap(),
            TileLocation::NotAddressable(NotAddressableReason::SyntheticLevel {
                base_level: LevelIdx::new(base_level),
                factor,
            }),
            "level {level}"
        );
    }
    let error = slide
        .read_raw_compressed_tile(&TileRequest::new(0, 0, 1, 0, 0))
        .unwrap_err();
    assert!(error.to_string().contains("tile_location"), "{error}");
}

#[test]
fn level_locations_cover_the_tile_grid() {
    let (_fixture, slide) = open();
    let base = slide
        .level_tile_locations(0, 0, 0, PlaneSelection::default())
        .unwrap();
    assert_eq!((base.tiles_across, base.tiles_down), (4, 2));
    assert_eq!(base.tiles.len(), 8);
    assert!(base
        .tiles
        .iter()
        .all(|tile| matches!(tile.location, TileLocation::Stored(_))));

    let scaled = slide
        .level_tile_locations(0, 0, 1, PlaneSelection::default())
        .unwrap();
    assert!(scaled.tiles.iter().all(|tile| matches!(
        tile.location,
        TileLocation::NotAddressable(NotAddressableReason::SyntheticLevel { .. })
    )));
}

#[test]
fn wrong_optimisation_offsets_are_recovered_and_truncated_scans_fail() {
    let fixture = VmsFixture::complete();
    let reference = Slide::open(&fixture.path).unwrap();
    let expected = single_unit(&reference, &TileRequest::new(0, 0, 0, 0, 1));
    drop(reference);

    // Row starts past the file end are ignored in favour of a marker scan.
    let mut opt = fs::read(&fixture.opt_path).unwrap();
    for record in opt.chunks_mut(40) {
        record[..8].copy_from_slice(&u64::MAX.to_le_bytes());
    }
    fs::write(&fixture.opt_path, opt).unwrap();
    let slide = Slide::open(&fixture.path).unwrap();
    assert_eq!(
        single_unit(&slide, &TileRequest::new(0, 0, 0, 0, 1)).segments(),
        expected.segments()
    );
    drop(slide);

    // A scan that cannot find the next restart marker is an error.
    let image = fs::read(&fixture.image_paths[1]).unwrap();
    let header_len = super::super::jpeg::read_vms_jpeg_header(&fixture.image_paths[1])
        .unwrap()
        .header
        .len();
    fs::write(&fixture.image_paths[1], &image[..header_len + 4]).unwrap();
    let slide = Slide::open(&fixture.path).unwrap();
    assert!(slide
        .tile_location(&TileRequest::new(0, 0, 0, 2, 0))
        .is_err());
    assert!(slide
        .level_tile_locations(0, 0, 0, PlaneSelection::default())
        .is_err());
}

fn open() -> (VmsFixture, Slide) {
    let fixture = VmsFixture::complete();
    let slide = Slide::open(&fixture.path).expect("open synthetic VMS");
    (fixture, slide)
}
