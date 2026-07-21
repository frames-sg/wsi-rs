use super::super::*;
use super::fixtures::{build_aperio_tiff, SyntheticTag};
// ── Interpretation tests ─────────────────────────────────────────

#[test]
fn interpret_single_level() {
    let file = build_aperio_tiff(&[vec![
        SyntheticTag::long(tags::IMAGE_WIDTH, 4096),
        SyntheticTag::long(tags::IMAGE_LENGTH, 3072),
        SyntheticTag::long(tags::TILE_WIDTH, 256),
        SyntheticTag::long(tags::TILE_LENGTH, 256),
        SyntheticTag::short(tags::COMPRESSION, 7), // JPEG
        SyntheticTag::ascii(
            tags::IMAGE_DESCRIPTION,
            "Aperio Image Library v1.0|AppMag = 40|MPP = 0.25",
        ),
    ]]);

    let container = TiffContainer::open(file.path()).unwrap();
    let interpreter = AperioInterpreter;
    let layout = interpreter.interpret(&container).unwrap();

    assert_eq!(layout.dataset.scenes.len(), 1);
    let series = &layout.dataset.scenes[0].series[0];
    assert_eq!(series.levels.len(), 1);
    assert_eq!(series.levels[0].dimensions, (4096, 3072));
    assert!((series.levels[0].downsample - 1.0).abs() < 0.001);

    // Tile layout
    match &series.levels[0].tile_layout {
        TileLayout::Regular {
            tile_width,
            tile_height,
            tiles_across,
            tiles_down,
        } => {
            assert_eq!(*tile_width, 256);
            assert_eq!(*tile_height, 256);
            assert_eq!(*tiles_across, 16); // 4096/256
            assert_eq!(*tiles_down, 12); // 3072/256
        }
        other => panic!("expected Regular, got: {:?}", other),
    }

    // Tile source
    let key = TileSourceKey {
        scene: 0usize,
        series: 0usize,
        level: 0u32,
        z: 0,
        c: 0,
        t: 0,
    };
    assert!(layout.tile_sources.contains_key(&key));
    match layout.tile_sources.get(&key).unwrap() {
        TileSource::TiledIfd { compression, .. } => {
            assert_eq!(*compression, Compression::Jpeg);
        }
        other => panic!("expected TiledIfd, got: {:?}", other),
    }
}

#[test]
fn interpret_populates_source_and_legacy_icc_from_tiff_tag() {
    let icc_bytes = vec![0, 1, 2, 3, 0, 255];
    let file = build_aperio_tiff(&[vec![
        SyntheticTag::long(tags::IMAGE_WIDTH, 4096),
        SyntheticTag::long(tags::IMAGE_LENGTH, 3072),
        SyntheticTag::long(tags::TILE_WIDTH, 256),
        SyntheticTag::long(tags::TILE_LENGTH, 256),
        SyntheticTag::short(tags::COMPRESSION, 7),
        SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, "Aperio Image Library v1.0"),
        SyntheticTag::bytes(tags::ICC_PROFILE, icc_bytes.clone()),
    ]]);

    let container = TiffContainer::open(file.path()).unwrap();
    let ifd_id = container.top_ifds()[0];
    let interpreter = AperioInterpreter;
    let layout = interpreter.interpret(&container).unwrap();

    assert_eq!(layout.dataset.source_icc_profiles.len(), 1);
    let profile = &layout.dataset.source_icc_profiles[0];
    assert_eq!(profile.bytes, icc_bytes);
    assert_eq!(
        layout
            .dataset
            .icc_profiles
            .get(&IccProfileKey::new(SceneId::new(0), SeriesId::new(0))),
        Some(&icc_bytes)
    );
    assert_eq!(
        profile.provenance,
        IccProfileProvenance::TiffTag {
            ifd_id: ifd_id.0,
            tag: tags::ICC_PROFILE,
        }
    );
}

#[test]
fn interpret_ignores_associated_only_icc_for_main_series() {
    let associated_icc = vec![9, 8, 7, 6, 5];
    let file = build_aperio_tiff(&[
        vec![
            SyntheticTag::long(tags::IMAGE_WIDTH, 4096),
            SyntheticTag::long(tags::IMAGE_LENGTH, 3072),
            SyntheticTag::long(tags::TILE_WIDTH, 256),
            SyntheticTag::long(tags::TILE_LENGTH, 256),
            SyntheticTag::short(tags::COMPRESSION, 7),
            SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, "Aperio Image Library v1.0"),
        ],
        vec![
            SyntheticTag::long(tags::IMAGE_WIDTH, 400),
            SyntheticTag::long(tags::IMAGE_LENGTH, 300),
            SyntheticTag::short(tags::COMPRESSION, 7),
            SyntheticTag::long(tags::STRIP_OFFSETS, 100),
            SyntheticTag::long(tags::STRIP_BYTE_COUNTS, 5000),
            SyntheticTag::bytes(tags::ICC_PROFILE, associated_icc),
        ],
    ]);

    let container = TiffContainer::open(file.path()).unwrap();
    let interpreter = AperioInterpreter;
    let layout = interpreter.interpret(&container).unwrap();

    assert!(layout.dataset.source_icc_profiles.is_empty());
    assert!(layout.dataset.icc_profiles.is_empty());
    assert!(layout.dataset.associated_images.contains_key("thumbnail"));
}

#[test]
fn interpret_multi_level_sorted_by_area() {
    let file = build_aperio_tiff(&[
        // IFD 0: large (level 0 after sorting)
        vec![
            SyntheticTag::long(tags::IMAGE_WIDTH, 4096),
            SyntheticTag::long(tags::IMAGE_LENGTH, 3072),
            SyntheticTag::long(tags::TILE_WIDTH, 256),
            SyntheticTag::long(tags::TILE_LENGTH, 256),
            SyntheticTag::short(tags::COMPRESSION, 7),
            SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, "Aperio Image Library v1.0"),
        ],
        // IFD 1: smaller (level 1 after sorting)
        vec![
            SyntheticTag::long(tags::IMAGE_WIDTH, 1024),
            SyntheticTag::long(tags::IMAGE_LENGTH, 768),
            SyntheticTag::long(tags::TILE_WIDTH, 256),
            SyntheticTag::long(tags::TILE_LENGTH, 256),
            SyntheticTag::short(tags::COMPRESSION, 7),
        ],
    ]);

    let container = TiffContainer::open(file.path()).unwrap();
    let interpreter = AperioInterpreter;
    let layout = interpreter.interpret(&container).unwrap();

    let series = &layout.dataset.scenes[0].series[0];
    assert_eq!(series.levels.len(), 2);

    // Level 0 = largest
    assert_eq!(series.levels[0].dimensions, (4096, 3072));
    assert!((series.levels[0].downsample - 1.0).abs() < 0.001);

    // Level 1 = smaller, downsample ~4.0
    assert_eq!(series.levels[1].dimensions, (1024, 768));
    assert!(series.levels[1].downsample > 3.5);
    assert!(series.levels[1].downsample < 4.5);
}

#[test]
fn interpret_multi_level_reverse_order() {
    // Small IFD first in chain, large IFD second — should still sort correctly
    let file = build_aperio_tiff(&[
        vec![
            SyntheticTag::long(tags::IMAGE_WIDTH, 512),
            SyntheticTag::long(tags::IMAGE_LENGTH, 384),
            SyntheticTag::long(tags::TILE_WIDTH, 256),
            SyntheticTag::long(tags::TILE_LENGTH, 256),
            SyntheticTag::short(tags::COMPRESSION, 7),
            SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, "Aperio Image Library v1.0"),
        ],
        vec![
            SyntheticTag::long(tags::IMAGE_WIDTH, 4096),
            SyntheticTag::long(tags::IMAGE_LENGTH, 3072),
            SyntheticTag::long(tags::TILE_WIDTH, 256),
            SyntheticTag::long(tags::TILE_LENGTH, 256),
            SyntheticTag::short(tags::COMPRESSION, 7),
        ],
    ]);

    let container = TiffContainer::open(file.path()).unwrap();
    let interpreter = AperioInterpreter;
    let layout = interpreter.interpret(&container).unwrap();

    let series = &layout.dataset.scenes[0].series[0];
    // Largest first regardless of IFD chain order
    assert_eq!(series.levels[0].dimensions, (4096, 3072));
    assert_eq!(series.levels[1].dimensions, (512, 384));
}

#[test]
fn interpret_tiles_across_rounds_up() {
    // 4100 / 256 = 16.015... → tiles_across = 17
    let file = build_aperio_tiff(&[vec![
        SyntheticTag::long(tags::IMAGE_WIDTH, 4100),
        SyntheticTag::long(tags::IMAGE_LENGTH, 3072),
        SyntheticTag::long(tags::TILE_WIDTH, 256),
        SyntheticTag::long(tags::TILE_LENGTH, 256),
        SyntheticTag::short(tags::COMPRESSION, 7),
        SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, "Aperio Image Library v1.0"),
    ]]);

    let container = TiffContainer::open(file.path()).unwrap();
    let interpreter = AperioInterpreter;
    let layout = interpreter.interpret(&container).unwrap();

    match &layout.dataset.scenes[0].series[0].levels[0].tile_layout {
        TileLayout::Regular { tiles_across, .. } => {
            assert_eq!(*tiles_across, 17);
        }
        other => panic!("expected Regular, got: {:?}", other),
    }
}

#[test]
fn interpret_no_tiled_ifds_returns_error() {
    // All stripped — no pyramid levels
    let file = build_aperio_tiff(&[vec![
        SyntheticTag::long(tags::IMAGE_WIDTH, 800),
        SyntheticTag::long(tags::IMAGE_LENGTH, 600),
        SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, "Aperio Image Library v1.0"),
    ]]);

    let container = TiffContainer::open(file.path()).unwrap();
    let interpreter = AperioInterpreter;
    let result = interpreter.interpret(&container);
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("No tiled pyramid levels"),
        "expected 'No tiled pyramid levels', got: {}",
        msg,
    );
}

// ── Associated image tests ───────────────────────────────────────

#[test]
fn interpret_thumbnail_at_index_1() {
    let file = build_aperio_tiff(&[
        // IFD 0: tiled pyramid
        vec![
            SyntheticTag::long(tags::IMAGE_WIDTH, 4096),
            SyntheticTag::long(tags::IMAGE_LENGTH, 3072),
            SyntheticTag::long(tags::TILE_WIDTH, 256),
            SyntheticTag::long(tags::TILE_LENGTH, 256),
            SyntheticTag::short(tags::COMPRESSION, 7),
            SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, "Aperio Image Library v1.0"),
        ],
        // IFD 1: stripped → "thumbnail"
        vec![
            SyntheticTag::long(tags::IMAGE_WIDTH, 400),
            SyntheticTag::long(tags::IMAGE_LENGTH, 300),
            SyntheticTag::short(tags::COMPRESSION, 7),
            SyntheticTag::long(tags::STRIP_OFFSETS, 100),
            SyntheticTag::long(tags::STRIP_BYTE_COUNTS, 5000),
        ],
    ]);

    let container = TiffContainer::open(file.path()).unwrap();
    let interpreter = AperioInterpreter;
    let layout = interpreter.interpret(&container).unwrap();

    assert!(layout.dataset.associated_images.contains_key("thumbnail"));
    let thumb = &layout.dataset.associated_images["thumbnail"];
    assert_eq!(thumb.dimensions, (400, 300));

    assert!(layout.associated_sources.contains_key("thumbnail"));
    match layout.associated_sources.get("thumbnail").unwrap() {
        TileSource::Stripped {
            strip_offsets,
            strip_byte_counts,
            ..
        } => {
            assert_eq!(strip_offsets.as_slice(), &[100]);
            assert_eq!(strip_byte_counts.as_slice(), &[5000]);
        }
        other => panic!("expected Stripped, got: {:?}", other),
    }
}

#[test]
fn interpret_label_and_macro_by_description() {
    let file = build_aperio_tiff(&[
        // IFD 0: tiled pyramid
        vec![
            SyntheticTag::long(tags::IMAGE_WIDTH, 4096),
            SyntheticTag::long(tags::IMAGE_LENGTH, 3072),
            SyntheticTag::long(tags::TILE_WIDTH, 256),
            SyntheticTag::long(tags::TILE_LENGTH, 256),
            SyntheticTag::short(tags::COMPRESSION, 7),
            SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, "Aperio Image Library v1.0"),
        ],
        // IFD 1: thumbnail (stripped, index 1)
        vec![
            SyntheticTag::long(tags::IMAGE_WIDTH, 400),
            SyntheticTag::long(tags::IMAGE_LENGTH, 300),
            SyntheticTag::short(tags::COMPRESSION, 7),
            SyntheticTag::long(tags::STRIP_OFFSETS, 100),
            SyntheticTag::long(tags::STRIP_BYTE_COUNTS, 5000),
        ],
        // IFD 2: label (stripped)
        vec![
            SyntheticTag::long(tags::IMAGE_WIDTH, 200),
            SyntheticTag::long(tags::IMAGE_LENGTH, 100),
            SyntheticTag::short(tags::COMPRESSION, 7),
            SyntheticTag::long(tags::STRIP_OFFSETS, 200),
            SyntheticTag::long(tags::STRIP_BYTE_COUNTS, 2000),
            SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, "label image"),
        ],
        // IFD 3: macro (stripped)
        vec![
            SyntheticTag::long(tags::IMAGE_WIDTH, 800),
            SyntheticTag::long(tags::IMAGE_LENGTH, 600),
            SyntheticTag::short(tags::COMPRESSION, 7),
            SyntheticTag::long(tags::STRIP_OFFSETS, 300),
            SyntheticTag::long(tags::STRIP_BYTE_COUNTS, 10000),
            SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, "macro scan"),
        ],
    ]);

    let container = TiffContainer::open(file.path()).unwrap();
    let interpreter = AperioInterpreter;
    let layout = interpreter.interpret(&container).unwrap();

    assert!(layout.dataset.associated_images.contains_key("thumbnail"));
    assert!(layout.dataset.associated_images.contains_key("label"));
    assert!(layout.dataset.associated_images.contains_key("macro"));
    assert_eq!(
        layout.dataset.associated_images["label"].dimensions,
        (200, 100)
    );
    assert_eq!(
        layout.dataset.associated_images["macro"].dimensions,
        (800, 600)
    );
}

#[test]
fn interpret_stripped_fallback_name() {
    // IFD at index 2 with no recognized description → "image_2"
    let file = build_aperio_tiff(&[
        vec![
            SyntheticTag::long(tags::IMAGE_WIDTH, 4096),
            SyntheticTag::long(tags::IMAGE_LENGTH, 3072),
            SyntheticTag::long(tags::TILE_WIDTH, 256),
            SyntheticTag::long(tags::TILE_LENGTH, 256),
            SyntheticTag::short(tags::COMPRESSION, 7),
            SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, "Aperio Image Library v1.0"),
        ],
        // IFD 1: tiled (another pyramid level)
        vec![
            SyntheticTag::long(tags::IMAGE_WIDTH, 1024),
            SyntheticTag::long(tags::IMAGE_LENGTH, 768),
            SyntheticTag::long(tags::TILE_WIDTH, 256),
            SyntheticTag::long(tags::TILE_LENGTH, 256),
            SyntheticTag::short(tags::COMPRESSION, 7),
        ],
        // IFD 2: stripped with unknown description
        vec![
            SyntheticTag::long(tags::IMAGE_WIDTH, 100),
            SyntheticTag::long(tags::IMAGE_LENGTH, 50),
            SyntheticTag::short(tags::COMPRESSION, 7),
            SyntheticTag::long(tags::STRIP_OFFSETS, 100),
            SyntheticTag::long(tags::STRIP_BYTE_COUNTS, 1000),
            SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, "something else"),
        ],
    ]);

    let container = TiffContainer::open(file.path()).unwrap();
    let interpreter = AperioInterpreter;
    let layout = interpreter.interpret(&container).unwrap();

    assert!(layout.dataset.associated_images.contains_key("image_2"));
}

// ── JPEG tables test ─────────────────────────────────────────────

#[test]
fn jpeg_tables_propagated_to_tile_source() {
    let fake_tables = vec![0xFF, 0xD8, 0xFF, 0xDB, 0x00, 0x43]; // minimal JPEG header fragment
    let file = build_aperio_tiff(&[vec![
        SyntheticTag::long(tags::IMAGE_WIDTH, 4096),
        SyntheticTag::long(tags::IMAGE_LENGTH, 3072),
        SyntheticTag::long(tags::TILE_WIDTH, 256),
        SyntheticTag::long(tags::TILE_LENGTH, 256),
        SyntheticTag::short(tags::COMPRESSION, 7),
        SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, "Aperio Image Library v1.0"),
        SyntheticTag::bytes(tags::JPEG_TABLES, fake_tables.clone()),
    ]]);

    let container = TiffContainer::open(file.path()).unwrap();
    let interpreter = AperioInterpreter;
    let layout = interpreter.interpret(&container).unwrap();

    let key = TileSourceKey {
        scene: 0usize,
        series: 0usize,
        level: 0u32,
        z: 0,
        c: 0,
        t: 0,
    };
    match layout.tile_sources.get(&key).unwrap() {
        TileSource::TiledIfd { jpeg_tables, .. } => {
            assert!(jpeg_tables.is_some());
            assert_eq!(jpeg_tables.as_ref().unwrap(), &fake_tables);
        }
        other => panic!("expected TiledIfd, got: {:?}", other),
    }
}

#[test]
fn jpeg_tables_are_kept_per_pyramid_ifd() {
    let level0_tables = vec![0xFF, 0xD8, 0xFF, 0xDB, 0x00, 0x43, 0x00];
    let level1_tables = vec![0xFF, 0xD8, 0xFF, 0xDB, 0x00, 0x43, 0x01];
    let file = build_aperio_tiff(&[
        vec![
            SyntheticTag::long(tags::IMAGE_WIDTH, 4096),
            SyntheticTag::long(tags::IMAGE_LENGTH, 3072),
            SyntheticTag::long(tags::TILE_WIDTH, 256),
            SyntheticTag::long(tags::TILE_LENGTH, 256),
            SyntheticTag::short(tags::COMPRESSION, 7),
            SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, "Aperio Image Library v1.0"),
            SyntheticTag::bytes(tags::JPEG_TABLES, level0_tables.clone()),
        ],
        vec![
            SyntheticTag::long(tags::IMAGE_WIDTH, 1024),
            SyntheticTag::long(tags::IMAGE_LENGTH, 768),
            SyntheticTag::long(tags::TILE_WIDTH, 256),
            SyntheticTag::long(tags::TILE_LENGTH, 256),
            SyntheticTag::short(tags::COMPRESSION, 7),
            SyntheticTag::ascii(
                tags::IMAGE_DESCRIPTION,
                "Aperio Image Library v1.0 -> 1024x768 JPEG/RGB",
            ),
            SyntheticTag::bytes(tags::JPEG_TABLES, level1_tables.clone()),
        ],
    ]);

    let container = TiffContainer::open(file.path()).unwrap();
    let interpreter = AperioInterpreter;
    let layout = interpreter.interpret(&container).unwrap();

    for (level, expected) in [(0, level0_tables), (1, level1_tables)] {
        let key = TileSourceKey {
            scene: 0usize,
            series: 0usize,
            level,
            z: 0,
            c: 0,
            t: 0,
        };
        match layout.tile_sources.get(&key).unwrap() {
            TileSource::TiledIfd { jpeg_tables, .. } => {
                assert_eq!(jpeg_tables.as_ref(), Some(&expected));
            }
            other => panic!("expected TiledIfd, got: {:?}", other),
        }
    }
}

// ── Dataset ID test ──────────────────────────────────────────────

#[test]
fn dataset_id_deterministic() {
    let file = build_aperio_tiff(&[vec![
        SyntheticTag::long(tags::IMAGE_WIDTH, 4096),
        SyntheticTag::long(tags::IMAGE_LENGTH, 3072),
        SyntheticTag::long(tags::TILE_WIDTH, 256),
        SyntheticTag::long(tags::TILE_LENGTH, 256),
        SyntheticTag::short(tags::COMPRESSION, 7),
        SyntheticTag::ascii(tags::IMAGE_DESCRIPTION, "Aperio Image Library v1.0"),
    ]]);

    let container = TiffContainer::open(file.path()).unwrap();
    let interpreter = AperioInterpreter;
    let layout1 = interpreter.interpret(&container).unwrap();
    let layout2 = interpreter.interpret(&container).unwrap();
    assert_eq!(layout1.dataset.id, layout2.dataset.id);
}

// ── Compression mapping tests ────────────────────────────────────

#[test]
fn compression_from_tag_values() {
    assert_eq!(compression_from_tag(1), Compression::None);
    assert_eq!(compression_from_tag(6), Compression::Jpeg);
    assert_eq!(compression_from_tag(7), Compression::Jpeg);
    assert_eq!(compression_from_tag(33003), Compression::Jp2kYcbcr);
    assert_eq!(compression_from_tag(33005), Compression::Jp2kYcbcr);
    assert_eq!(compression_from_tag(33004), Compression::Jp2kRgb);
    assert_eq!(compression_from_tag(9999), Compression::Other(9999));
}
