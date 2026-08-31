use super::*;
use crate::formats::tiff_family::test_support::{build_tiff, SyntheticTag};

fn tiled(width: u32, height: u32, offset: u32, byte_count: u32) -> Vec<SyntheticTag> {
    vec![
        SyntheticTag::long(tags::IMAGE_WIDTH, width),
        SyntheticTag::long(tags::IMAGE_LENGTH, height),
        SyntheticTag::long(tags::TILE_WIDTH, 256),
        SyntheticTag::long(tags::TILE_LENGTH, 256),
        SyntheticTag::short(tags::COMPRESSION, 7),
        SyntheticTag::long(tags::TILE_OFFSETS, offset),
        SyntheticTag::long(tags::TILE_BYTE_COUNTS, byte_count),
    ]
}

fn sparse_tiled(
    width: u32,
    height: u32,
    offsets: &[u32],
    byte_counts: &[u32],
) -> Vec<SyntheticTag> {
    vec![
        SyntheticTag::long(tags::IMAGE_WIDTH, width),
        SyntheticTag::long(tags::IMAGE_LENGTH, height),
        SyntheticTag::long(tags::TILE_WIDTH, 256),
        SyntheticTag::long(tags::TILE_LENGTH, 256),
        SyntheticTag::short(tags::COMPRESSION, 7),
        SyntheticTag::long_array(tags::TILE_OFFSETS, offsets),
        SyntheticTag::long_array(tags::TILE_BYTE_COUNTS, byte_counts),
    ]
}

fn metadata(min_z: i64, max_z: i64) -> String {
    format!(
        "<Argos.Scan.Metadata><MinZ>{min_z}</MinZ><MaxZ>{max_z}</MaxZ><ObjectiveMagnification>20</ObjectiveMagnification><Barcode>ABC-123</Barcode><Nested><Value>kept</Value></Nested></Argos.Scan.Metadata>"
    )
}

fn with_metadata(mut tags: Vec<SyntheticTag>, xml: &str) -> Vec<SyntheticTag> {
    tags.push(SyntheticTag::ascii(ARGOS_METADATA_TAG, xml));
    tags
}

#[test]
fn interpret_exposes_all_z_planes_properties_and_sparse_tiles() {
    let xml = metadata(-1, 0);
    let file = build_tiff(&[
        with_metadata(tiled(64, 32, 8, 1), &xml),
        tiled(32, 16, 8, 1),
        tiled(64, 32, 0, 0),
        tiled(32, 16, 8, 1),
    ]);
    let container = TiffContainer::open(file.path()).unwrap();
    let layout = ArgosInterpreter.interpret(&container).unwrap();

    let series = &layout.dataset.scenes[0].series[0];
    assert_eq!(series.axes, AxesShape::new(2, 1, 1));
    assert_eq!(series.levels.len(), 2);
    assert_eq!(series.levels[0].dimensions, (64, 32));
    assert_eq!(series.levels[1].downsample, 2.0);
    assert_eq!(layout.tile_sources.len(), 4);
    assert!(layout.tile_sources.contains_key(&TileSourceKey {
        scene: 0,
        series: 0,
        level: 1,
        z: 1,
        c: 0,
        t: 0,
    }));
    assert_eq!(
        layout.dataset.properties.get("openslide.objective-power"),
        Some("20")
    );
    assert_eq!(
        layout.dataset.properties.get("openslide.barcode"),
        Some("ABC-123")
    );
    assert_eq!(
        layout.dataset.properties.get("argos.Nested.Value"),
        Some("kept")
    );
    assert!(layout
        .dataset
        .properties
        .get("openslide.quickhash-1")
        .is_some());

    let TileLayout::Irregular { tiles, .. } = &series.levels[0].tile_layout else {
        panic!("ARGOS sparse level should use an irregular tile map");
    };
    assert!(tiles.contains_key(&(0, 0)));

    let second_z_first_level = sparse_level(
        &container,
        &group_z_stacks(&collect_tiled_ifds(&container).unwrap())[1][0],
        1.0,
    )
    .unwrap();
    let TileLayout::Irregular { tiles, .. } = second_z_first_level.level.tile_layout else {
        panic!("ARGOS sparse level should use an irregular tile map");
    };
    assert!(tiles.is_empty(), "zero-byte sparse tiles must remain gaps");
}

#[test]
fn metadata_and_stack_validation_fail_closed() {
    for xml in [
        "<Wrong><MinZ>0</MinZ><MaxZ>0</MaxZ></Wrong>",
        "<Argos.Scan.Metadata><MinZ>x</MinZ><MaxZ>0</MaxZ></Argos.Scan.Metadata>",
        "<Argos.Scan.Metadata><MinZ>2</MinZ><MaxZ>1</MaxZ></Argos.Scan.Metadata>",
    ] {
        let file = build_tiff(&[with_metadata(tiled(64, 32, 8, 1), xml)]);
        let container = TiffContainer::open(file.path()).unwrap();
        assert!(ArgosInterpreter.interpret(&container).is_err(), "{xml}");
    }

    let xml = metadata(0, 1);
    let file = build_tiff(&[with_metadata(tiled(64, 32, 8, 1), &xml)]);
    let container = TiffContainer::open(file.path()).unwrap();
    assert!(ArgosInterpreter
        .interpret(&container)
        .unwrap_err()
        .to_string()
        .contains("declares 2 Z planes"));

    let file = build_tiff(&[
        with_metadata(tiled(64, 32, 8, 1), &xml),
        tiled(64, 31, 8, 1),
    ]);
    let container = TiffContainer::open(file.path()).unwrap();
    assert!(ArgosInterpreter
        .interpret(&container)
        .unwrap_err()
        .to_string()
        .contains("geometry differs"));
}

#[test]
fn metadata_boundary_errors_are_diagnostic() {
    let cases = [
        (tiled(64, 32, 8, 1), "ARGOS metadata tag 65000 is missing"),
        (
            with_metadata(tiled(64, 32, 8, 1), "<Argos.Scan.Metadata>"),
            "failed to parse ARGOS metadata XML",
        ),
        (
            with_metadata(
                tiled(64, 32, 8, 1),
                "<Argos.Scan.Metadata><MaxZ>0</MaxZ></Argos.Scan.Metadata>",
            ),
            "missing argos.MinZ",
        ),
        (
            with_metadata(tiled(64, 32, 8, 1), &metadata(i64::MIN, i64::MAX)),
            "focal-plane range is invalid",
        ),
        (
            with_metadata(tiled(64, 32, 8, 1), &metadata(0, i64::from(u32::MAX))),
            "focal-plane count exceeds u32",
        ),
    ];

    for (tags, expected) in cases {
        let file = build_tiff(&[tags]);
        let container = TiffContainer::open(file.path()).unwrap();
        let error = ArgosInterpreter.interpret(&container).unwrap_err();
        assert!(error.to_string().contains(expected), "{error}");
    }
}

#[test]
fn empty_slide_and_overflowing_sparse_geometry_fail_closed() {
    let empty = build_tiff(&[]);
    let empty_container = TiffContainer::open(empty.path()).unwrap();
    let error = ArgosInterpreter.interpret(&empty_container).unwrap_err();
    assert!(error.to_string().contains("ARGOS slide has no IFDs"));

    let file = build_tiff(&[tiled(1, 1, 8, 1)]);
    let container = TiffContainer::open(file.path()).unwrap();
    let ifd = TiledIfd {
        ifd_id: container.top_ifds()[0],
        width: u64::MAX,
        height: u64::MAX,
        tile_width: 1,
        tile_height: 1,
        compression: Compression::Jpeg,
        jpeg_tables: None,
    };
    let error = match sparse_level(&container, &ifd, 1.0) {
        Ok(_) => panic!("overflowing ARGOS sparse geometry should fail"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("ARGOS tile count overflow"));
}

#[test]
fn normalized_z_zero_uses_the_openslide_compatible_middle_plane() {
    let xml = metadata(-1, 1);
    let file = build_tiff(&[
        with_metadata(tiled(64, 32, 8, 1), &xml),
        tiled(64, 32, 9, 1),
        tiled(64, 32, 10, 1),
    ]);
    let container = TiffContainer::open(file.path()).unwrap();
    let physical_ifds = container.top_ifds().to_vec();
    let layout = ArgosInterpreter.interpret(&container).unwrap();

    let source = layout
        .tile_sources
        .get(&TileSourceKey {
            scene: 0,
            series: 0,
            level: 0,
            z: 0,
            c: 0,
            t: 0,
        })
        .unwrap();
    assert!(matches!(
        source,
        TileSource::TiledIfd { ifd_id, .. } if *ifd_id == physical_ifds[1]
    ));
}

#[test]
fn final_stripped_ifds_are_thumbnail_and_macro() {
    let xml = metadata(0, 0);
    let stripped = |width, height| {
        vec![
            SyntheticTag::long(tags::IMAGE_WIDTH, width),
            SyntheticTag::long(tags::IMAGE_LENGTH, height),
            SyntheticTag::short(tags::COMPRESSION, 1),
            SyntheticTag::long(tags::STRIP_OFFSETS, 8),
            SyntheticTag::long(tags::STRIP_BYTE_COUNTS, 1),
        ]
    };
    let file = build_tiff(&[
        with_metadata(tiled(64, 32, 8, 1), &xml),
        stripped(16, 8),
        stripped(32, 16),
    ]);
    let container = TiffContainer::open(file.path()).unwrap();
    let layout = ArgosInterpreter.interpret(&container).unwrap();
    assert_eq!(
        layout.dataset.associated_images["thumbnail"].dimensions,
        (16, 8)
    );
    assert_eq!(
        layout.dataset.associated_images["macro"].dimensions,
        (32, 16)
    );
}

#[test]
fn sparse_bounds_and_public_layout_use_the_middle_z_stack() {
    let xml = metadata(-1, 1);
    let file = build_tiff(&[
        with_metadata(sparse_tiled(600, 128, &[8, 0, 0], &[1, 0, 0]), &xml),
        sparse_tiled(600, 128, &[0, 8, 9], &[0, 1, 1]),
        sparse_tiled(600, 128, &[8, 9, 10], &[1, 1, 1]),
    ]);
    let container = TiffContainer::open(file.path()).unwrap();
    let layout = ArgosInterpreter.interpret(&container).unwrap();

    let properties = &layout.dataset.properties;
    assert_eq!(properties.get("openslide.bounds-x"), Some("256"));
    assert_eq!(properties.get("openslide.bounds-y"), Some("0"));
    assert_eq!(properties.get("openslide.bounds-width"), Some("344"));
    assert_eq!(properties.get("openslide.bounds-height"), Some("128"));

    let TileLayout::Irregular { tiles, .. } =
        &layout.dataset.scenes[0].series[0].levels[0].tile_layout
    else {
        panic!("ARGOS level must retain its sparse layout");
    };
    assert!(!tiles.contains_key(&(0, 0)));
    assert!(tiles.contains_key(&(1, 0)));
    assert!(tiles.contains_key(&(2, 0)));
}

#[test]
fn full_or_empty_sparse_extents_do_not_publish_bounds() {
    for (offsets, byte_counts) in [([8, 9, 10], [1, 1, 1]), ([0, 0, 0], [0, 0, 0])] {
        let xml = metadata(0, 0);
        let file = build_tiff(&[with_metadata(
            sparse_tiled(600, 128, &offsets, &byte_counts),
            &xml,
        )]);
        let container = TiffContainer::open(file.path()).unwrap();
        let layout = ArgosInterpreter.interpret(&container).unwrap();

        for name in [
            "openslide.bounds-x",
            "openslide.bounds-y",
            "openslide.bounds-width",
            "openslide.bounds-height",
        ] {
            assert_eq!(layout.dataset.properties.get(name), None, "{name}");
        }
    }
}
