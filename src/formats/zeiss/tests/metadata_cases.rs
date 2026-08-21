use crate::core::types::{Level, TileLayout};
use crate::formats::zeiss::metadata::*;
use crate::formats::zeiss::MAX_CZI_SCENES;
use czi_rs::{
    BoundingBoxes, ChannelInfo as CziChannelInfo, CompressionMode, Coordinate, Dimension,
    DirectorySubBlockInfo, FileHeaderInfo, IntRect, IntSize, MetadataSummary, PixelType,
    SubBlockStatistics,
};
use std::collections::BTreeSet;

fn subblock(
    index: usize,
    scene: Option<i32>,
    rect: IntRect,
    stored_size: IntSize,
) -> DirectorySubBlockInfo {
    let mut coordinate = Coordinate::new();
    if let Some(scene) = scene {
        coordinate.set(Dimension::S, scene);
    }
    DirectorySubBlockInfo {
        index,
        file_position: index as u64 * 100,
        file_part: 0,
        pixel_type: PixelType::Bgr24,
        compression: CompressionMode::UnCompressed,
        coordinate,
        rect,
        stored_size,
        m_index: None,
        pyramid_type: None,
    }
}

fn statistics_with_scenes(scenes: &[(i32, IntRect)]) -> SubBlockStatistics {
    let mut statistics = SubBlockStatistics::default();
    for &(index, rect) in scenes {
        statistics.scene_bounding_boxes.insert(
            index,
            BoundingBoxes {
                all: rect,
                layer0: rect,
            },
        );
    }
    statistics
}

#[test]
fn channels_normalize_rgb_and_argb_colors_and_keep_first_channel() {
    for (value, expected) in [
        ("#112233", Some([0x11, 0x22, 0x33])),
        ("FF445566", Some([0x44, 0x55, 0x66])),
        ("not-a-color", None),
    ] {
        let summary = MetadataSummary {
            channels: vec![
                CziChannelInfo {
                    index: 0,
                    name: Some("first".into()),
                    color: Some(value.into()),
                    ..CziChannelInfo::default()
                },
                CziChannelInfo {
                    index: 1,
                    name: Some("second".into()),
                    ..CziChannelInfo::default()
                },
            ],
            ..MetadataSummary::default()
        };
        let channels = build_channels(&summary);
        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0].name.as_deref(), Some("first"));
        assert_eq!(channels[0].color, expected);
    }
    assert!(build_channels(&MetadataSummary::default()).is_empty());
}

#[test]
fn scene_indices_prefer_observed_nonnegative_scenes_and_bound_metadata_fallback() {
    let statistics = statistics_with_scenes(&[
        (-1, IntRect::new(0, 0, 1, 1)),
        (7, IntRect::new(0, 0, 1, 1)),
        (2, IntRect::new(0, 0, 1, 1)),
    ]);
    assert_eq!(
        scene_indices(&statistics, &MetadataSummary::default()).expect("observed scenes"),
        vec![2, 7]
    );

    let mut summary = MetadataSummary::default();
    summary.image.sizes.insert(Dimension::S, 3);
    assert_eq!(
        scene_indices(&SubBlockStatistics::default(), &summary).expect("metadata scenes"),
        vec![0, 1, 2]
    );
    summary.image.sizes.insert(Dimension::S, MAX_CZI_SCENES + 1);
    let error = scene_indices(&SubBlockStatistics::default(), &summary)
        .expect_err("excessive metadata scene count");
    assert!(error.to_string().contains("scene count"));
}

#[test]
fn scene_slot_and_canvas_geometry_handle_sparse_negative_and_metadata_free_inputs() {
    let sparse = [2, 7];
    assert_eq!(
        scene_slot_for_subblock(
            &sparse,
            &subblock(0, Some(7), IntRect::new(0, 0, 1, 1), IntSize { w: 1, h: 1 })
        ),
        Some(1)
    );
    assert_eq!(
        scene_slot_for_subblock(
            &sparse,
            &subblock(
                0,
                Some(-1),
                IntRect::new(0, 0, 1, 1),
                IntSize { w: 1, h: 1 }
            )
        ),
        None
    );
    assert_eq!(
        scene_slot_for_subblock(
            &[0],
            &subblock(0, None, IntRect::new(0, 0, 1, 1), IntSize { w: 1, h: 1 })
        ),
        Some(0)
    );
    assert_eq!(
        scene_slot_for_subblock(
            &sparse,
            &subblock(0, None, IntRect::new(0, 0, 1, 1), IntSize { w: 1, h: 1 })
        ),
        None
    );

    let statistics = statistics_with_scenes(&[
        (2, IntRect::new(-10, -5, 4, 3)),
        (7, IntRect::new(5, 6, 5, 4)),
    ]);
    assert_eq!(canvas_origin(&statistics), (-10, -5));
    assert_eq!(
        canvas_dimensions(
            &statistics,
            &MetadataSummary::default(),
            std::path::Path::new("sparse.czi")
        )
        .expect("statistics canvas"),
        (20, 15)
    );
    let mut summary = MetadataSummary::default();
    summary.image.sizes.insert(Dimension::X, 12);
    summary.image.sizes.insert(Dimension::Y, 9);
    assert_eq!(
        canvas_dimensions(&statistics, &summary, std::path::Path::new("metadata.czi"))
            .expect("metadata canvas"),
        (12, 9)
    );
    let error = canvas_dimensions(
        &SubBlockStatistics::default(),
        &MetadataSummary::default(),
        std::path::Path::new("empty.czi"),
    )
    .expect_err("missing canvas dimensions");
    assert!(error
        .to_string()
        .contains("missing Zeiss canvas dimensions"));
}

#[test]
fn ratios_levels_origins_and_tile_associations_preserve_geometry() {
    let subblocks = vec![
        subblock(
            0,
            Some(0),
            IntRect::new(10, 20, 512, 256),
            IntSize { w: 512, h: 256 },
        ),
        subblock(
            1,
            Some(0),
            IntRect::new(10, 20, 512, 256),
            IntSize { w: 256, h: 128 },
        ),
        subblock(
            2,
            Some(1),
            IntRect::new(522, 20, 512, 256),
            IntSize { w: 512, h: 256 },
        ),
        subblock(
            3,
            Some(1),
            IntRect::new(522, 20, 512, 256),
            IntSize { w: 256, h: 128 },
        ),
    ];
    assert_eq!(subblock_origin(&subblocks), (10, 20));
    assert_eq!(subblock_ratio(&subblocks[0]), Some(1));
    assert_eq!(subblock_ratio(&subblocks[1]), Some(2));
    assert_eq!(
        common_level_ratios(&subblocks, &[0, 1], &SubBlockStatistics::default())
            .expect("common ratios"),
        vec![1, 2]
    );
    let levels = build_levels((1024, 512), &[1, 2]);
    assert_eq!(levels[0].dimensions, (1024, 512));
    assert_eq!(levels[1].dimensions, (512, 256));
    assert!(matches!(
        levels[0].tile_layout,
        TileLayout::Regular {
            tiles_across: 4,
            tiles_down: 2,
            ..
        }
    ));

    let maps =
        build_canvas_level_tile_subblocks(&subblocks, &[vec![0, 2], vec![1, 3]], &levels, (10, 20))
            .expect("tile association maps");
    assert_eq!(maps[0].get(&(0, 0)), Some(&vec![0]));
    assert_eq!(maps[0].get(&(2, 0)), Some(&vec![2]));
    assert_eq!(maps[1].get(&(0, 0)), Some(&vec![1]));
    assert_eq!(maps[1].get(&(1, 0)), Some(&vec![3]));
}

#[test]
fn invalid_ratios_and_irregular_levels_are_ignored_without_inventing_tiles() {
    for info in [
        subblock(0, Some(0), IntRect::new(0, 0, 0, 2), IntSize { w: 1, h: 1 }),
        subblock(0, Some(0), IntRect::new(0, 0, 2, 2), IntSize { w: 0, h: 1 }),
        subblock(0, Some(0), IntRect::new(0, 0, 4, 6), IntSize { w: 2, h: 2 }),
    ] {
        assert_eq!(subblock_ratio(&info), None);
    }

    let info = subblock(
        0,
        Some(0),
        IntRect::new(0, 0, 32, 32),
        IntSize { w: 32, h: 32 },
    );
    let levels = [Level {
        dimensions: (32, 32),
        downsample: 1.0,
        tile_layout: TileLayout::WholeLevel {
            width: 32,
            height: 32,
            virtual_tile_width: 16,
            virtual_tile_height: 16,
        },
    }];
    let maps = build_canvas_level_tile_subblocks(&[info], &[vec![0]], &levels, (0, 0))
        .expect("whole-level map is ignored");
    assert!(maps[0].is_empty());
}

#[test]
fn quickhash_and_objective_parsing_are_deterministic_and_validate_guids() {
    let header = FileHeaderInfo {
        major: 1,
        minor: 0,
        primary_file_guid: "00112233-4455-6677-8899-aabbccddeeff".into(),
        file_guid: "ffeeddcc-bbaa-9988-7766-554433221100".into(),
        file_part: 0,
        subblock_directory_position: 544,
        metadata_position: 1024,
        attachment_directory_position: 0,
        update_pending: false,
    };
    let first = quickhash_for_zeiss(&header, "<Metadata/>").expect("quickhash");
    let second = quickhash_for_zeiss(&header, "<Metadata/>").expect("repeat quickhash");
    assert_eq!(first, second);
    assert_eq!(first.len(), 64);

    let mut invalid = header;
    invalid.file_guid = "not-a-guid".into();
    assert!(quickhash_for_zeiss(&invalid, "<Metadata/>").is_err());

    let xml = r#"<ObjectiveRef Id="obj"/><Objective Id="obj"><NominalMagnification> 63 </NominalMagnification></Objective>"#;
    assert_eq!(extract_objective_magnification(xml).as_deref(), Some("63"));
    assert_eq!(extract_objective_magnification("<Objective/>"), None);
}

#[test]
fn common_ratios_fall_back_to_base_for_empty_or_disjoint_scene_sets() {
    assert_eq!(
        common_level_ratios(&[], &[0], &SubBlockStatistics::default()).expect("empty ratios"),
        vec![1]
    );
    let subblocks = [
        subblock(0, Some(0), IntRect::new(0, 0, 4, 4), IntSize { w: 2, h: 2 }),
        subblock(1, Some(1), IntRect::new(0, 0, 3, 3), IntSize { w: 1, h: 1 }),
    ];
    let ratios = common_level_ratios(&subblocks, &[0, 1], &SubBlockStatistics::default())
        .expect("disjoint ratios");
    assert_eq!(
        ratios.into_iter().collect::<BTreeSet<_>>(),
        BTreeSet::from([1])
    );
}
