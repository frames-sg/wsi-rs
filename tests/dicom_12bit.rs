//! 12-bit JPEG DICOM WSI decode through the native API.
//!
//! Fixtures and references come from libjpeg-turbo 3.x cjpeg/djpeg; see
//! tests/fixtures/dicom_12bit/README.md.

use std::path::{Path, PathBuf};

use wsi_rs::{
    ColorSpace, Compression, CpuTile, CpuTileData, CpuTileLayout, DisplayWindow, LevelIdx,
    RegionRequest, SampleType, SceneId, SeriesId, Slide, StoredFraming, TileLayout, TileLocation,
    TileRequest,
};

const MATRIX: (u32, u32) = (40, 28);
const TILE: u32 = 16;
const TILES: (i64, i64) = (3, 2);

/// `(fixture, samples per pixel)`.
const FIXTURES: [(&str, u16); 4] = [
    ("mono2-extended", 1),
    ("mono2-progressive", 1),
    ("ybr422-extended", 3),
    ("ybr422-progressive", 3),
];

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dicom_12bit")
}

/// Reads a djpeg `-rgb` reference: binary PPM with maxval 4095 and
/// big-endian 16-bit samples.
fn read_reference(name: &str) -> Vec<u16> {
    let bytes = std::fs::read(fixture_root().join(format!("{name}.ppm"))).expect("reference PPM");
    let header = format!("P6\n{} {}\n4095\n", MATRIX.0, MATRIX.1);
    assert!(
        bytes.starts_with(header.as_bytes()),
        "{name}: unexpected reference header"
    );
    let body = &bytes[header.len()..];
    assert_eq!(body.len(), (MATRIX.0 * MATRIX.1 * 3 * 2) as usize);
    body.as_chunks::<2>()
        .0
        .iter()
        .map(|sample| u16::from_be_bytes([sample[0], sample[1]]))
        .collect()
}

fn crop_reference(reference: &[u16], x: u32, y: u32, width: u32, height: u32) -> Vec<u16> {
    (y..y + height)
        .flat_map(|row| {
            let start = ((row * MATRIX.0 + x) * 3) as usize;
            reference[start..start + (width * 3) as usize]
                .iter()
                .copied()
        })
        .collect()
}

fn rgb16_samples<'a>(name: &str, tile: &'a CpuTile) -> &'a [u16] {
    assert_eq!(tile.channels(), 3, "{name}: RGB channels");
    assert_eq!(tile.color_space(), &ColorSpace::Rgb, "{name}: color space");
    assert_eq!(tile.layout(), CpuTileLayout::Interleaved, "{name}: layout");
    let CpuTileData::U16(samples) = tile.data() else {
        panic!("{name}: 12-bit DICOM must decode to U16 samples");
    };
    samples.as_slice()
}

fn tile_request(col: i64, row: i64) -> TileRequest {
    TileRequest::new(
        SceneId::new(0),
        SeriesId::new(0),
        LevelIdx::new(0),
        col,
        row,
    )
}

#[test]
fn twelve_bit_dicom_series_is_uint16_with_regular_tiles() {
    for (name, _) in FIXTURES {
        let slide = Slide::open(fixture_root().join(format!("{name}.dcm"))).expect(name);
        let series = &slide.dataset().scenes[0].series[0];
        assert_eq!(series.sample_type, SampleType::Uint16, "{name}");
        assert_eq!(series.levels.len(), 1, "{name}");
        assert_eq!(
            series.levels[0].dimensions,
            (u64::from(MATRIX.0), u64::from(MATRIX.1))
        );
        assert!(matches!(
            series.levels[0].tile_layout,
            TileLayout::Regular {
                tile_width: TILE,
                tile_height: TILE,
                tiles_across: 3,
                tiles_down: 2,
            }
        ));
    }
}

#[test]
fn twelve_bit_dicom_regions_match_libjpeg_turbo_reference() {
    for (name, _) in FIXTURES {
        let reference = read_reference(name);
        assert!(
            reference.iter().any(|&sample| sample > 255),
            "{name}: reference must exercise the 12-bit range"
        );
        let slide = Slide::open(fixture_root().join(format!("{name}.dcm"))).expect(name);
        let region = slide
            .read_region(&RegionRequest::new(
                SceneId::new(0),
                SeriesId::new(0),
                LevelIdx::new(0),
                (0, 0),
                MATRIX,
            ))
            .expect(name);
        assert_eq!((region.width(), region.height()), MATRIX, "{name}");
        assert_eq!(
            rgb16_samples(name, &region),
            reference,
            "{name}: full region"
        );
        let display = region
            .to_rgba_windowed(&DisplayWindow::new(0.0, 4095.0).unwrap())
            .expect(name);
        assert_eq!(display.dimensions(), MATRIX, "{name}: windowed display");
        assert!(display.pixels().all(|pixel| pixel[3] == 255), "{name}");

        // An interior region that straddles all four tile seams.
        let region = slide
            .read_region(&RegionRequest::new(
                SceneId::new(0),
                SeriesId::new(0),
                LevelIdx::new(0),
                (5, 9),
                (30, 17),
            ))
            .expect(name);
        assert_eq!(
            rgb16_samples(name, &region),
            crop_reference(&reference, 5, 9, 30, 17),
            "{name}: seam-crossing region"
        );
    }
}

#[test]
fn twelve_bit_dicom_single_and_batched_tiles_match_reference() {
    for (name, _) in FIXTURES {
        let reference = read_reference(name);
        let requests = (0..TILES.1)
            .flat_map(|row| (0..TILES.0).map(move |col| tile_request(col, row)))
            .collect::<Vec<_>>();
        // Separate handles keep the decoded-frame caches independent, so the
        // batch path and the single-tile path both decode every frame.
        let batched = Slide::open(fixture_root().join(format!("{name}.dcm")))
            .expect(name)
            .read_tiles(&requests)
            .expect(name);
        let single = Slide::open(fixture_root().join(format!("{name}.dcm"))).expect(name);
        for (request, batched) in requests.iter().zip(&batched) {
            let x = request.col as u32 * TILE;
            let y = request.row as u32 * TILE;
            let width = TILE.min(MATRIX.0 - x);
            let height = TILE.min(MATRIX.1 - y);
            let expected = crop_reference(&reference, x, y, width, height);
            let tile = single.read_tile(request).expect(name);
            for (path, tile) in [("single", &tile), ("batched", batched)] {
                assert_eq!(
                    (tile.width(), tile.height()),
                    (width, height),
                    "{name} {path} tile ({}, {})",
                    request.col,
                    request.row
                );
                assert_eq!(
                    rgb16_samples(name, tile),
                    expected,
                    "{name} {path} tile ({}, {})",
                    request.col,
                    request.row
                );
            }
        }
    }
}

#[test]
fn twelve_bit_dicom_raw_frames_report_16_bits_allocated() {
    for (name, samples_per_pixel) in FIXTURES {
        let slide = Slide::open(fixture_root().join(format!("{name}.dcm"))).expect(name);
        let raw = slide
            .read_raw_compressed_tile(&tile_request(2, 1))
            .expect(name);
        assert_eq!(raw.compression(), Compression::Jpeg, "{name}");
        assert_eq!(raw.bits_allocated(), 16, "{name}");
        assert_eq!(raw.samples_per_pixel(), samples_per_pixel, "{name}");
        assert_eq!((raw.width(), raw.height()), (TILE, TILE), "{name}");
        assert!(raw.data().starts_with(&[0xFF, 0xD8]), "{name}: SOI");
        assert!(raw.data().ends_with(&[0xFF, 0xD9]), "{name}: EOI");
    }
}

#[test]
fn twelve_bit_dicom_frame_locations_name_the_raw_frame_bytes() {
    for (name, samples_per_pixel) in FIXTURES {
        let path = fixture_root().join(format!("{name}.dcm"));
        let slide = Slide::open(&path).expect(name);
        let file = std::fs::read(&path).expect(name);
        let level = slide
            .level_tile_locations(0usize, 0usize, 0u32, wsi_rs::PlaneSelection::default())
            .expect(name);
        assert_eq!((level.tiles_across, level.tiles_down), (3, 2), "{name}");
        for tile in &level.tiles {
            let TileLocation::Stored(stored) = &tile.location else {
                panic!("{name}: tile ({}, {}) is stored", tile.col, tile.row);
            };
            let unit = stored.single_unit().expect("one frame per tile");
            assert_eq!(unit.path(), path.as_path(), "{name}");
            assert_eq!(unit.framing(), StoredFraming::Standalone, "{name}");
            assert_eq!(unit.codec().compression(), Compression::Jpeg, "{name}");
            assert_eq!(
                unit.codec().samples_per_pixel(),
                samples_per_pixel,
                "{name}"
            );
            let segments = unit
                .segments()
                .iter()
                .map(|range| {
                    let start = range.offset as usize;
                    assert_eq!(
                        file[start - 8..start - 4],
                        [0xFE, 0xFF, 0x00, 0xE0],
                        "{name}: each segment is a fragment value after its Item header"
                    );
                    &file[start..start + range.length as usize]
                })
                .collect::<Vec<_>>();
            // Several of these frames have odd lengths and a DICOM padding
            // byte; locations leave it out exactly as raw reads trim it.
            let assembled = unit.assemble(segments).expect(name);
            let raw = slide
                .read_raw_compressed_tile(&tile_request(tile.col, tile.row))
                .expect(name);
            assert_eq!(
                assembled,
                raw.data(),
                "{name} tile ({}, {})",
                tile.col,
                tile.row
            );
        }
    }
}

#[test]
fn twelve_bit_dicom_edge_frames_locate_the_tile_and_state_significant_bits() {
    for (name, _) in FIXTURES {
        let slide = Slide::open(fixture_root().join(format!("{name}.dcm"))).expect(name);
        for (col, row, region) in [
            (0, 0, wsi_rs::PixelRect::new(0, 0, 16, 16)),
            (2, 0, wsi_rs::PixelRect::new(0, 0, 8, 16)),
            (0, 1, wsi_rs::PixelRect::new(0, 0, 16, 12)),
            (2, 1, wsi_rs::PixelRect::new(0, 0, 8, 12)),
        ] {
            let request = tile_request(col, row);
            let raw = slide.read_raw_compressed_tile(&request).expect(name);
            assert_eq!((raw.width(), raw.height()), (TILE, TILE), "{name}");
            assert_eq!(raw.tile_region(), region, "{name} tile ({col}, {row})");
            assert_eq!(raw.significant_bits(), Some(12), "{name}");
            let TileLocation::Stored(stored) = slide.tile_location(&request).expect(name) else {
                panic!("{name}: tile ({col}, {row}) is stored");
            };
            assert_eq!(stored.tile_size(), (region.width, region.height), "{name}");
            let codec = stored.single_unit().expect(name).codec();
            assert_eq!(codec.bits_per_sample(), 16, "{name}");
            assert_eq!(codec.significant_bits(), Some(12), "{name}");
        }
    }
}
