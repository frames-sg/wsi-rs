mod support;

use support::corpus::{find_slide_by_alias, load_public};
use wsi_rs::{
    Compression, CpuTileData, EncodedUnit, PlaneSelection, Slide, StoredFraming, TileLayout,
    TileLocation, TileRequest,
};

#[test]
#[ignore = "requires the public ARGOS/Huron corpus"]
fn public_argos_slides_expose_sparse_pyramids_z_planes_and_associated_images() {
    let manifest = load_public().expect("load public corpus manifest");
    let entries = manifest
        .slides
        .iter()
        .filter(|entry| entry.format == "argos")
        .collect::<Vec<_>>();
    assert!(!entries.is_empty(), "public corpus contains no ARGOS rows");
    for entry in entries {
        let alias = entry.alias.as_str();
        let path = find_slide_by_alias(alias)
            .unwrap_or_else(|| panic!("public corpus slide {alias} is missing"));
        assert_eq!(entry.format, "argos");

        let slide = Slide::open(&path).unwrap_or_else(|error| panic!("open {alias}: {error}"));
        assert_eq!(slide.dataset().properties.vendor(), Some("argos"));
        assert!(slide
            .dataset()
            .properties
            .get("openslide.barcode")
            .is_some());
        let series = &slide.dataset().scenes[0].series[0];
        assert!(series.levels.len() >= 3, "{alias} should expose a pyramid");
        if alias == "argos-001" {
            assert_eq!(series.axes.z, 1);
        } else {
            assert!(series.axes.z > 1, "stacked ARGOS must expose every Z plane");
        }

        let TileLayout::Irregular { tiles, .. } = &series.levels[0].tile_layout else {
            panic!("{alias} should model sparse source tiles explicitly");
        };
        let (&(col, row), _) = tiles
            .iter()
            .next()
            .unwrap_or_else(|| panic!("{alias} base level contains no source tiles"));
        let tile = slide
            .read_tile(
                &TileRequest::new(0usize, 0usize, 0u32, col, row)
                    .with_plane(PlaneSelection::default()),
            )
            .unwrap_or_else(|error| panic!("read {alias} source tile: {error}"));
        assert!(matches!(tile.data(), CpuTileData::U8(_)));

        for name in ["thumbnail", "macro"] {
            assert!(
                slide.dataset().associated_images.contains_key(name),
                "{alias} missing {name} metadata"
            );
            let image = slide
                .read_associated(name)
                .unwrap_or_else(|error| panic!("read {alias} {name}: {error}"));
            assert!(image.width() > 0 && image.height() > 0);
        }
    }
}

#[test]
#[ignore = "requires the public ARGOS/Huron corpus"]
fn public_huron_jpeg_and_uncompressed_slides_decode_with_associated_images() {
    let manifest = load_public().expect("load public corpus manifest");
    let entries = manifest
        .slides
        .iter()
        .filter(|entry| entry.format == "huron")
        .collect::<Vec<_>>();
    assert!(!entries.is_empty(), "public corpus contains no Huron rows");
    for entry in entries {
        let alias = entry.alias.as_str();
        let path = find_slide_by_alias(alias)
            .unwrap_or_else(|| panic!("public corpus slide {alias} is missing"));
        assert_eq!(entry.format, "huron");

        let slide = Slide::open(&path).unwrap_or_else(|error| panic!("open {alias}: {error}"));
        assert_eq!(slide.dataset().properties.vendor(), Some("huron"));
        let series = &slide.dataset().scenes[0].series[0];
        assert!(series.levels.len() >= 3, "{alias} should expose a pyramid");

        let tile = slide
            .read_tile(&TileRequest::new(0usize, 0usize, 0u32, 0, 0))
            .unwrap_or_else(|error| panic!("read {alias} tile: {error}"));
        assert!(matches!(tile.data(), CpuTileData::U8(_)));

        for name in ["thumbnail", "label", "macro"] {
            assert!(
                slide.dataset().associated_images.contains_key(name),
                "{alias} missing {name} metadata"
            );
            let image = slide
                .read_associated(name)
                .unwrap_or_else(|error| panic!("read {alias} {name}: {error}"));
            assert!(image.width() > 0 && image.height() > 0);
        }
    }
}

/// Reads `unit`'s segments straight from the slide file and assembles them.
fn assemble_from_file(unit: &EncodedUnit) -> Vec<u8> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(unit.path()).expect("open located slide file");
    let segments: Vec<Vec<u8>> = unit
        .segments()
        .iter()
        .map(|segment| {
            let mut bytes = vec![0; segment.length as usize];
            file.seek(SeekFrom::Start(segment.offset)).unwrap();
            file.read_exact(&mut bytes).unwrap();
            bytes
        })
        .collect();
    unit.assemble(segments.iter().map(Vec::as_slice))
        .expect("assemble located segments")
}

#[test]
fn local_huron_uncompressed_tiles_pass_through_at_their_locations() {
    let Some(path) = find_slide_by_alias("huron-uncompressed-001") else {
        eprintln!("skipping: corpus slide huron-uncompressed-001 not found");
        return;
    };
    let slide = Slide::open(&path).expect("open huron-uncompressed-001");
    let series = &slide.dataset().scenes[0].series[0];
    for (level_index, level) in series.levels.iter().enumerate() {
        let level_index = level_index as u32;
        let locations = slide
            .level_tile_locations(0usize, 0usize, level_index, PlaneSelection::default())
            .unwrap_or_else(|error| panic!("level {level_index} locations: {error}"));
        let TileLayout::Regular {
            tiles_across,
            tiles_down,
            ..
        } = level.tile_layout
        else {
            panic!("Huron level {level_index} should use a regular tile grid");
        };
        assert_eq!(
            (locations.tiles_across, locations.tiles_down),
            (tiles_across, tiles_down)
        );
        assert_eq!(locations.tiles.len() as u64, tiles_across * tiles_down);

        let last = locations.tiles.len() - 1;
        for entry in [&locations.tiles[0], &locations.tiles[last]] {
            let TileLocation::Stored(stored) = &entry.location else {
                panic!("Huron tile ({}, {}) should be stored", entry.col, entry.row);
            };
            let unit = stored.single_unit().expect("one stored TIFF tile");
            assert_eq!(unit.path(), path.as_path());
            assert_eq!(unit.framing(), StoredFraming::BarePayload);
            assert_eq!(unit.codec().compression(), Compression::None);

            let req = TileRequest::new(0usize, 0usize, level_index, entry.col, entry.row);
            let raw = slide
                .read_raw_compressed_tile(&req)
                .unwrap_or_else(|error| panic!("raw Huron tile {req:?}: {error}"));
            assert_eq!(raw.compression(), Compression::None);
            assert_eq!(assemble_from_file(unit), raw.data());

            // The stored tile keeps its padding; its rows up to the tile
            // width are the decoded pixels.
            let tile = slide.read_tile(&req).expect("decode Huron tile");
            let CpuTileData::U8(pixels) = tile.data() else {
                panic!("Huron tiles decode to 8-bit samples");
            };
            let channels = usize::from(raw.samples_per_pixel());
            let stored_row = raw.width() as usize * channels;
            let visible_row = tile.width() as usize * channels;
            let region = raw.tile_region();
            assert_eq!((region.width, region.height), (tile.width(), tile.height()));
            for (row, expected) in pixels.chunks(visible_row).enumerate() {
                let start = row * stored_row;
                assert_eq!(&raw.data()[start..start + visible_row], expected);
            }
        }
    }
}
