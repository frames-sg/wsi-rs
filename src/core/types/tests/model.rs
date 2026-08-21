use super::super::*;

fn minimal_dataset_for_tests() -> Dataset {
    Dataset {
        id: DatasetId(1),
        scenes: vec![Scene {
            id: "scene-0".into(),
            name: None,
            series: vec![Series {
                id: "series-0".into(),
                axes: AxesShape::default(),
                levels: vec![Level {
                    dimensions: (1, 1),
                    downsample: 1.0,
                    tile_layout: TileLayout::Regular {
                        tile_width: 1,
                        tile_height: 1,
                        tiles_across: 1,
                        tiles_down: 1,
                    },
                }],
                sample_type: SampleType::Uint8,
                channels: vec![],
            }],
        }],
        associated_images: std::collections::HashMap::new(),
        properties: Properties::new(),
        icc_profiles: std::collections::HashMap::new(),
        source_icc_profiles: Vec::new(),
    }
}

// --- AxesShape ---

#[test]
fn axes_shape_default_is_2d() {
    let axes = AxesShape::default();
    assert_eq!(axes.z, 1);
    assert_eq!(axes.c, 1);
    assert_eq!(axes.t, 1);
}

#[test]
fn axes_shape_new_sets_axis_extents() {
    let axes = AxesShape::new(2, 3, 4);
    assert_eq!(axes.z, 2);
    assert_eq!(axes.c, 3);
    assert_eq!(axes.t, 4);
}

#[test]
fn metadata_constructors_build_dataset_hierarchy() {
    let level = Level::new(
        (1024, 768),
        1.0,
        TileLayout::WholeLevel {
            width: 1024,
            height: 768,
            virtual_tile_width: 512,
            virtual_tile_height: 512,
        },
    );
    let channel = ChannelInfo::new()
        .with_name("DAPI")
        .with_color([20, 80, 255])
        .with_excitation_nm(405.0)
        .with_emission_nm(450.0);
    let series = Series::new(
        "series-0",
        AxesShape::new(1, 1, 1),
        vec![level],
        SampleType::Uint16,
        vec![channel],
    );
    let scene = Scene::new("scene-0", vec![series]).with_name("main");

    let mut associated_images = HashMap::new();
    associated_images.insert(
        "label".into(),
        AssociatedImage::new((128, 64), SampleType::Uint8, 3),
    );
    let mut properties = Properties::new();
    properties.insert("openslide.vendor", "fixture");
    let icc_key = IccProfileKey::new(SceneId::new(0), SeriesId::new(0));
    let icc_profiles = HashMap::from([(icc_key, vec![1, 2, 3])]);

    let dataset = Dataset::new(DatasetId::new(42), vec![scene])
        .with_associated_images(associated_images)
        .with_properties(properties)
        .with_icc_profiles(icc_profiles);

    assert_eq!(dataset.id, DatasetId::new(42));
    assert_eq!(dataset.scenes[0].id, "scene-0");
    assert_eq!(dataset.scenes[0].name.as_deref(), Some("main"));
    assert_eq!(dataset.scenes[0].series[0].id, "series-0");
    assert_eq!(
        dataset.scenes[0].series[0].levels[0].dimensions,
        (1024, 768)
    );
    assert_eq!(
        dataset.scenes[0].series[0].channels[0].name.as_deref(),
        Some("DAPI")
    );
    assert_eq!(dataset.associated_images["label"].dimensions, (128, 64));
    assert_eq!(dataset.properties.vendor(), Some("fixture"));
    assert_eq!(dataset.icc_profiles[&icc_key], vec![1, 2, 3]);
}

#[test]
fn source_icc_profile_builders_and_conflicts_preserve_normalized_identity() {
    let key = SourceIccProfileKey::new(SceneId::new(2), SeriesId::new(3))
        .with_optical_path(4)
        .with_channel(5);
    let profile = SourceIccProfile::new(
        key,
        vec![1, 2, 3],
        IccProfileProvenance::ReaderMetadata {
            source: "fixture".into(),
        },
    );
    assert_eq!(profile.key, key);
    assert_eq!(profile.key.optical_path, Some(4));
    assert_eq!(profile.key.channel, Some(5));
    assert_eq!(profile.bytes, vec![1, 2, 3]);

    let conflict = SourceIccProfileConflict {
        scene: key.scene,
        series: key.series,
    };
    assert_eq!(
        conflict.to_string(),
        "conflicting source ICC profiles for scene 2 series 3"
    );
    assert_eq!(ChannelInfo::default().name, None);
}

#[test]
fn index_newtypes_round_trip_through_constructor_accessors() {
    assert_eq!(DatasetId::new(42).get(), 42);
    assert_eq!(SceneId::new(1).get(), 1);
    assert_eq!(SeriesId::new(2).get(), 2);
    assert_eq!(LevelIdx::new(3).get(), 3);

    let plane = PlaneSelection::new(4, 5, 6);
    assert_eq!(PlaneIdx::new(plane).get(), plane);

    let icc_key = IccProfileKey::new(SceneId::new(7), SeriesId::new(8));
    assert_eq!(icc_key.scene.get(), 7);
    assert_eq!(icc_key.series.get(), 8);
}

#[test]
fn dataset_id_equality() {
    let a = DatasetId::new(42);
    let b = DatasetId::new(42);
    let c = DatasetId::new(99);
    assert_eq!(a, b);
    assert_ne!(a, c);
}

#[test]
fn dataset_id_hash_consistent() {
    use std::collections::HashSet;
    let mut set = HashSet::new();
    set.insert(DatasetId::new(1));
    set.insert(DatasetId::new(1));
    assert_eq!(set.len(), 1);
}

#[test]
fn dataset_source_icc_helper_populates_structured_and_legacy_metadata() {
    let mut dataset = minimal_dataset_for_tests();
    let bytes = vec![1, 2, 3, 4];
    let profile = SourceIccProfile {
        key: SourceIccProfileKey {
            scene: SceneId::new(0),
            series: SeriesId::new(0),
            optical_path: None,
            channel: None,
        },
        bytes: bytes.clone(),
        provenance: IccProfileProvenance::TiffTag {
            ifd_id: 1024,
            tag: 34675,
        },
    };

    dataset.push_source_icc_profile(profile.clone()).unwrap();

    assert_eq!(dataset.source_icc_profiles, vec![profile]);
    assert_eq!(
        dataset
            .icc_profiles
            .get(&IccProfileKey::new(SceneId::new(0), SeriesId::new(0))),
        Some(&bytes)
    );
}

#[test]
fn dataset_source_icc_helper_does_not_legacy_map_channel_specific_profile() {
    let mut dataset = minimal_dataset_for_tests();
    let profile = SourceIccProfile {
        key: SourceIccProfileKey {
            scene: SceneId::new(0),
            series: SeriesId::new(0),
            optical_path: Some(2),
            channel: Some(1),
        },
        bytes: vec![9, 8, 7],
        provenance: IccProfileProvenance::DicomOpticalPath {
            sop_instance_uid: "1.2.3".into(),
            optical_path_identifier: Some("path-2".into()),
        },
    };

    dataset.push_source_icc_profile(profile.clone()).unwrap();

    assert_eq!(dataset.source_icc_profiles, vec![profile]);
    assert!(dataset.icc_profiles.is_empty());
}

#[test]
fn dataset_source_icc_helper_rejects_conflicting_legacy_profile_without_mutating() {
    let mut dataset = minimal_dataset_for_tests();
    let first = SourceIccProfile {
        key: SourceIccProfileKey {
            scene: SceneId::new(0),
            series: SeriesId::new(0),
            optical_path: None,
            channel: None,
        },
        bytes: vec![1, 2, 3],
        provenance: IccProfileProvenance::ReaderMetadata {
            source: "first".into(),
        },
    };
    let conflicting = SourceIccProfile {
        key: first.key,
        bytes: vec![4, 5, 6],
        provenance: IccProfileProvenance::ReaderMetadata {
            source: "conflicting".into(),
        },
    };

    dataset.push_source_icc_profile(first.clone()).unwrap();
    let source_profiles_before = dataset.source_icc_profiles.clone();
    let legacy_profiles_before = dataset.icc_profiles.clone();

    let err = dataset
        .push_source_icc_profile(conflicting)
        .expect_err("conflicting legacy ICC profile should be rejected");

    assert_eq!(
        err,
        SourceIccProfileConflict {
            scene: SceneId::new(0),
            series: SeriesId::new(0),
        }
    );
    assert_eq!(dataset.source_icc_profiles, source_profiles_before);
    assert_eq!(dataset.icc_profiles, legacy_profiles_before);
}

#[test]
fn source_icc_profiles_for_series_filters_matching_profiles() {
    let mut dataset = minimal_dataset_for_tests();
    let matching_scene_series = SourceIccProfile {
        key: SourceIccProfileKey {
            scene: SceneId::new(0),
            series: SeriesId::new(0),
            optical_path: None,
            channel: None,
        },
        bytes: vec![1],
        provenance: IccProfileProvenance::ReaderMetadata {
            source: "matching-scene-series".into(),
        },
    };
    let matching_channel = SourceIccProfile {
        key: SourceIccProfileKey {
            scene: SceneId::new(0),
            series: SeriesId::new(0),
            optical_path: Some(2),
            channel: Some(1),
        },
        bytes: vec![2],
        provenance: IccProfileProvenance::ReaderMetadata {
            source: "matching-channel".into(),
        },
    };
    let non_matching_scene = SourceIccProfile {
        key: SourceIccProfileKey {
            scene: SceneId::new(1),
            series: SeriesId::new(0),
            optical_path: None,
            channel: None,
        },
        bytes: vec![3],
        provenance: IccProfileProvenance::ReaderMetadata {
            source: "non-matching-scene".into(),
        },
    };
    let non_matching_series = SourceIccProfile {
        key: SourceIccProfileKey {
            scene: SceneId::new(0),
            series: SeriesId::new(1),
            optical_path: None,
            channel: None,
        },
        bytes: vec![4],
        provenance: IccProfileProvenance::ReaderMetadata {
            source: "non-matching-series".into(),
        },
    };

    dataset
        .push_source_icc_profile(matching_scene_series.clone())
        .unwrap();
    dataset
        .push_source_icc_profile(matching_channel.clone())
        .unwrap();
    dataset.push_source_icc_profile(non_matching_scene).unwrap();
    dataset
        .push_source_icc_profile(non_matching_series)
        .unwrap();

    let profiles = dataset
        .source_icc_profiles_for_series(0, 0)
        .cloned()
        .collect::<Vec<_>>();

    assert_eq!(profiles, vec![matching_scene_series, matching_channel]);
}

#[test]
fn compression_equality() {
    assert_eq!(Compression::Jpeg, Compression::Jpeg);
    assert_ne!(Compression::Jpeg, Compression::Jp2kRgb);
    assert_eq!(Compression::Other(99), Compression::Other(99));
    assert_ne!(Compression::Other(99), Compression::Other(100));
}

// --- SampleType ---

#[test]
fn sample_type_byte_size() {
    assert_eq!(SampleType::Uint8.byte_size(), 1);
    assert_eq!(SampleType::Uint16.byte_size(), 2);
    assert_eq!(SampleType::Float32.byte_size(), 4);
}

// --- CpuTile display conversion ---
