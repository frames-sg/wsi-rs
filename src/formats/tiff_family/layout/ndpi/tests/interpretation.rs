use super::super::*;
use super::fixtures::*;

// ── Task 4: Detection + IFD classification tests ──────────────────

#[test]
fn detect_ndpi_container() {
    // Build a TIFF file with NDPI marker tag
    let file = build_synthetic_tiff(
        &[vec![
            long_tag(256, 1024), // IMAGE_WIDTH
            long_tag(257, 768),  // IMAGE_LENGTH
        ]],
        true, // ndpi=true adds tag 65420
    );

    let container = TiffContainer::open(file.path()).unwrap();
    let interpreter = NdpiInterpreter;
    assert!(interpreter.detect(&container));
}

#[test]
fn reject_non_ndpi_container() {
    // Build a normal TIFF without NDPI marker
    let file = build_synthetic_tiff(&[vec![long_tag(256, 1024), long_tag(257, 768)]], false);

    let container = TiffContainer::open(file.path()).unwrap();
    let interpreter = NdpiInterpreter;
    assert!(!interpreter.detect(&container));
}

#[test]
fn ifd_classification_macro_vs_pyramid() {
    // Build an NDPI with two IFDs:
    // IFD 0: SOURCELENS=40.0 (pyramid)
    // IFD 1: SOURCELENS=-1.0 (macro)
    // Both need valid strip offsets; detect() doesn't require valid JPEG data.
    let file = build_synthetic_tiff(
        &[
            vec![
                long_tag(256, 2048),    // IMAGE_WIDTH
                long_tag(257, 1536),    // IMAGE_LENGTH
                float_tag(65421, 40.0), // SOURCELENS
                long_tag(273, 0),       // STRIP_OFFSETS (invalid, but detect doesn't care)
                long_tag(279, 0),       // STRIP_BYTE_COUNTS
            ],
            vec![
                long_tag(256, 800),         // IMAGE_WIDTH
                long_tag(257, 600),         // IMAGE_LENGTH
                float_tag(65421, -1.0_f32), // SOURCELENS = macro
                long_tag(273, 0),
                long_tag(279, 0),
            ],
        ],
        true,
    );

    let container = TiffContainer::open(file.path()).unwrap();
    let interpreter = NdpiInterpreter;
    assert!(interpreter.detect(&container));

    // Verify IFD count
    assert_eq!(container.top_ifds().len(), 2);
}

#[test]
fn interpret_no_pyramid_levels_returns_error() {
    // An NDPI file where all IFDs are macro images (SOURCELENS=-1)
    let file = build_synthetic_tiff(
        &[vec![
            long_tag(256, 800),
            long_tag(257, 600),
            float_tag(65421, -1.0_f32),
            long_tag(273, 100),
            long_tag(279, 500),
        ]],
        true,
    );

    let container = TiffContainer::open(file.path()).unwrap();
    let interpreter = NdpiInterpreter;
    let result = interpreter.interpret(&container);
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("No pyramid levels"),
        "expected 'No pyramid levels', got: {}",
        err_msg,
    );
}

// ── Task 5: Full interpret() tests with embedded JPEG ──────────────

#[test]
fn interpret_single_level() {
    // Single pyramid level at SOURCELENS=40
    let file = build_ndpi_with_jpeg_strips(&[(1024, 768, 40.0, 0)]);

    let container = TiffContainer::open(file.path()).unwrap();
    let interpreter = NdpiInterpreter;
    let layout = interpreter.interpret(&container).unwrap();

    assert_eq!(layout.dataset.scenes.len(), 1);
    let series = &layout.dataset.scenes[0].series[0];
    assert_eq!(series.levels.len(), 9);
    assert_eq!(series.levels[0].dimensions, (1024, 768));
    assert_eq!(series.levels[1].dimensions, (512, 384));
    assert_eq!(series.levels[2].dimensions, (256, 192));
    assert_eq!(series.levels[8].dimensions, (4, 3));
    assert!((series.levels[0].downsample - 1.0).abs() < 0.001);
    assert_eq!(series.axes.z, 1);

    // Verify tile source exists
    let key = TileSourceKey {
        scene: 0usize,
        series: 0usize,
        level: 0u32,
        z: 0,
        c: 0,
        t: 0,
    };
    assert!(layout.tile_sources.contains_key(&key));
}

#[test]
fn interpret_multi_level_sorted() {
    // Two pyramid levels -- interpreter should sort largest first
    let file = build_ndpi_with_jpeg_strips(&[
        (512, 384, 20.0, 0),   // smaller (level 1)
        (2048, 1536, 40.0, 0), // larger (level 0)
    ]);

    let container = TiffContainer::open(file.path()).unwrap();
    let interpreter = NdpiInterpreter;
    let layout = interpreter.interpret(&container).unwrap();

    let series = &layout.dataset.scenes[0].series[0];
    assert_eq!(series.levels.len(), 10);

    // Level 0 should be the larger image
    assert_eq!(series.levels[0].dimensions, (2048, 1536));
    assert!((series.levels[0].downsample - 1.0).abs() < 0.001);

    // Missing 2x level is synthesized, 4x level stays physical.
    assert_eq!(series.levels[1].dimensions, (1024, 768));
    assert_eq!(series.levels[2].dimensions, (512, 384));
    assert_eq!(series.levels[9].dimensions, (4, 3));
}

#[test]
fn interpret_z_stack() {
    // Two IFDs at same SOURCELENS but different FOCAL_PLANEs
    let file = build_ndpi_with_jpeg_strips(&[
        (1024, 768, 40.0, 0), // z=0
        (1024, 768, 40.0, 1), // z=1
    ]);

    let container = TiffContainer::open(file.path()).unwrap();
    let interpreter = NdpiInterpreter;
    let layout = interpreter.interpret(&container).unwrap();

    let series = &layout.dataset.scenes[0].series[0];
    // Same SOURCELENS -> complete synthetic power-of-two pyramid, z=2
    assert_eq!(series.levels.len(), 9);
    assert_eq!(series.axes.z, 2);

    // Both z planes should have tile sources
    let key_z0 = TileSourceKey {
        scene: 0usize,
        series: 0usize,
        level: 0u32,
        z: 0,
        c: 0,
        t: 0,
    };
    let key_z1 = TileSourceKey {
        scene: 0usize,
        series: 0usize,
        level: 0u32,
        z: 1,
        c: 0,
        t: 0,
    };
    assert!(layout.tile_sources.contains_key(&key_z0));
    assert!(layout.tile_sources.contains_key(&key_z1));
}

#[test]
fn interpret_macro_associated_image() {
    // One pyramid + one macro
    let file = build_ndpi_with_jpeg_strips(&[
        (2048, 1536, 40.0, 0), // pyramid
        (800, 600, -1.0, 0),   // macro
    ]);

    let container = TiffContainer::open(file.path()).unwrap();
    let interpreter = NdpiInterpreter;
    let layout = interpreter.interpret(&container).unwrap();

    // Should have macro in associated images
    assert!(layout.dataset.associated_images.contains_key("macro"));
    let macro_img = &layout.dataset.associated_images["macro"];
    assert_eq!(macro_img.dimensions, (800, 600));

    // Should have macro in associated sources
    assert!(layout.associated_sources.contains_key("macro"));

    // Pyramid should still work
    assert_eq!(layout.dataset.scenes[0].series[0].levels.len(), 10);
}

#[test]
fn interpret_preserves_jpeg_tables_for_macro_image() {
    let jpeg_tables = [0xFF, 0xD8, 0xFF, 0xD9];
    let file = build_ndpi_with_strips(
        &[
            (2048, 1536, 40.0, 0, 7), // pyramid
            (800, 600, -1.0, 0, 7),   // JPEG macro
        ],
        Some(jpeg_tables),
    );

    let container = TiffContainer::open(file.path()).unwrap();
    let layout = NdpiInterpreter.interpret(&container).unwrap();

    match layout.associated_sources.get("macro").unwrap() {
        TileSource::Stripped {
            compression,
            jpeg_tables: Some(actual),
            ..
        } => {
            assert_eq!(*compression, Compression::Jpeg);
            assert_eq!(actual, &jpeg_tables);
        }
        other => panic!("expected JPEG macro strip source with tables, got: {other:?}"),
    }
}

#[test]
fn negative_two_sourcelens_is_not_exposed_as_public_thumbnail() {
    let file = build_ndpi_with_strips(
        &[
            (2048, 1536, 40.0, 0, 7), // pyramid
            (196, 572, -2.0, 0, 1),   // thumbnail
        ],
        None,
    );

    let container = TiffContainer::open(file.path()).unwrap();
    let interpreter = NdpiInterpreter;
    let layout = interpreter.interpret(&container).unwrap();

    assert!(!layout.dataset.associated_images.contains_key("thumbnail"));
    assert!(!layout.associated_sources.contains_key("thumbnail"));
    assert!(layout.dataset.associated_images.is_empty());
    assert!(layout.associated_sources.is_empty());
}

#[test]
fn interpret_properties_parsed() {
    let file = build_ndpi_with_jpeg_strips(&[(1024, 768, 40.0, 0)]);

    let container = TiffContainer::open(file.path()).unwrap();
    let interpreter = NdpiInterpreter;
    let layout = interpreter.interpret(&container).unwrap();

    // Vendor should always be set
    assert_eq!(layout.dataset.properties.vendor(), Some("hamamatsu"));
}

#[test]
fn interpret_mcu_geometry_determines_tile_source() {
    // The tiny test JPEG won't have a DRI marker, so it should
    // produce NdpiFullDecode (restart_interval == 0)
    let file = build_ndpi_with_jpeg_strips(&[(1024, 768, 40.0, 0)]);

    let container = TiffContainer::open(file.path()).unwrap();
    let interpreter = NdpiInterpreter;
    let layout = interpreter.interpret(&container).unwrap();

    let key = TileSourceKey {
        scene: 0usize,
        series: 0usize,
        level: 0u32,
        z: 0,
        c: 0,
        t: 0,
    };
    let source = layout.tile_sources.get(&key).unwrap();
    // Our synthetic JPEG has no DRI -> NdpiFullDecode
    match source {
        TileSource::NdpiFullDecode { .. } => {} // expected
        other => panic!("expected NdpiFullDecode, got: {:?}", other),
    }
}

#[test]
fn interpret_adds_synthetic_power_of_two_levels_between_sparse_physical_ifds() {
    let file = build_ndpi_with_jpeg_strips(&[(1024, 768, 40.0, 0), (256, 192, 20.0, 0)]);

    let container = TiffContainer::open(file.path()).unwrap();
    let interpreter = NdpiInterpreter;
    let layout = interpreter.interpret(&container).unwrap();

    match layout
        .tile_sources
        .get(&TileSourceKey {
            scene: 0usize,
            series: 0usize,
            level: 1u32,
            z: 0,
            c: 0,
            t: 0,
        })
        .unwrap()
    {
        TileSource::SyntheticDownsample { base_level, factor } => {
            assert_eq!(*base_level, 0);
            assert_eq!(*factor, 2);
        }
        other => panic!("expected SyntheticDownsample, got: {:?}", other),
    }

    match layout
        .tile_sources
        .get(&TileSourceKey {
            scene: 0usize,
            series: 0usize,
            level: 2u32,
            z: 0,
            c: 0,
            t: 0,
        })
        .unwrap()
    {
        TileSource::NdpiFullDecode { .. } => {}
        other => panic!("expected physical NDPI level, got: {:?}", other),
    }

    match layout
        .tile_sources
        .get(&TileSourceKey {
            scene: 0usize,
            series: 0usize,
            level: 3,
            z: 0,
            c: 0,
            t: 0,
        })
        .unwrap()
    {
        TileSource::SyntheticDownsample { base_level, factor } => {
            assert_eq!(*base_level, 2);
            assert_eq!(*factor, 2);
        }
        other => panic!("expected SyntheticDownsample, got: {:?}", other),
    }
}

#[test]
fn interpret_points_consecutive_synthetic_levels_at_nearest_physical_base() {
    let file = build_ndpi_with_jpeg_strips(&[(1024, 768, 40.0, 0), (128, 96, 5.0, 0)]);

    let container = TiffContainer::open(file.path()).unwrap();
    let interpreter = NdpiInterpreter;
    let layout = interpreter.interpret(&container).unwrap();

    match layout
        .tile_sources
        .get(&TileSourceKey {
            scene: 0usize,
            series: 0usize,
            level: 2u32,
            z: 0,
            c: 0,
            t: 0,
        })
        .unwrap()
    {
        TileSource::SyntheticDownsample { base_level, factor } => {
            assert_eq!(*base_level, 0);
            assert_eq!(*factor, 4);
        }
        other => panic!("expected SyntheticDownsample, got: {:?}", other),
    }

    match layout
        .tile_sources
        .get(&TileSourceKey {
            scene: 0usize,
            series: 0usize,
            level: 3,
            z: 0,
            c: 0,
            t: 0,
        })
        .unwrap()
    {
        TileSource::NdpiFullDecode { .. } => {}
        other => panic!("expected physical NDPI level, got: {:?}", other),
    }
}
