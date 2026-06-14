use super::*;

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
fn raw_compressed_tile_builder_sets_payload_metadata() {
    let data = vec![1, 2, 3, 4];
    let tile = RawCompressedTile::builder(Compression::Jpeg)
        .dimensions(256, 128)
        .bits_allocated(8)
        .samples_per_pixel(3)
        .photometric_interpretation(EncodedTilePhotometricInterpretation::YbrFull422)
        .data(data.clone())
        .build()
        .expect("complete raw compressed tile metadata should build");

    assert_eq!(tile.compression(), Compression::Jpeg);
    assert_eq!(tile.width(), 256);
    assert_eq!(tile.height(), 128);
    assert_eq!(tile.bits_allocated(), 8);
    assert_eq!(tile.samples_per_pixel(), 3);
    assert_eq!(
        tile.photometric_interpretation(),
        EncodedTilePhotometricInterpretation::YbrFull422
    );
    assert_eq!(tile.data(), data.as_slice());
    assert_eq!(tile.into_data(), data);
}

#[test]
fn raw_compressed_tile_builder_reports_missing_required_fields() {
    assert_eq!(
        RawCompressedTile::builder(Compression::Jpeg).build(),
        Err(RawCompressedTileBuildError::MissingDimensions)
    );
    assert_eq!(
        RawCompressedTile::builder(Compression::Jpeg)
            .dimensions(256, 128)
            .build(),
        Err(RawCompressedTileBuildError::MissingBitsAllocated)
    );
    assert_eq!(
        RawCompressedTile::builder(Compression::Jpeg)
            .dimensions(256, 128)
            .bits_allocated(8)
            .build(),
        Err(RawCompressedTileBuildError::MissingSamplesPerPixel)
    );
    assert_eq!(
        RawCompressedTile::builder(Compression::Jpeg)
            .dimensions(256, 128)
            .bits_allocated(8)
            .samples_per_pixel(3)
            .build(),
        Err(RawCompressedTileBuildError::MissingPhotometricInterpretation)
    );
    assert_eq!(
        RawCompressedTile::builder(Compression::Jpeg)
            .dimensions(256, 128)
            .bits_allocated(8)
            .samples_per_pixel(3)
            .photometric_interpretation(EncodedTilePhotometricInterpretation::YbrFull422)
            .build(),
        Err(RawCompressedTileBuildError::MissingData)
    );
}

#[test]
fn raw_compressed_tile_builder_rejects_invalid_payload_metadata() {
    let base = || {
        RawCompressedTile::builder(Compression::Jpeg)
            .dimensions(16, 16)
            .bits_allocated(8)
            .samples_per_pixel(3)
            .photometric_interpretation(EncodedTilePhotometricInterpretation::YbrFull422)
            .data(vec![0xff, 0xd8, 0xff, 0xd9])
    };

    assert_eq!(
        base().dimensions(0, 16).build(),
        Err(RawCompressedTileBuildError::InvalidDimensions)
    );
    assert_eq!(
        base().dimensions(16, 0).build(),
        Err(RawCompressedTileBuildError::InvalidDimensions)
    );
    assert_eq!(
        base().bits_allocated(0).build(),
        Err(RawCompressedTileBuildError::InvalidBitsAllocated)
    );
    assert_eq!(
        base().samples_per_pixel(0).build(),
        Err(RawCompressedTileBuildError::InvalidSamplesPerPixel)
    );
    assert_eq!(
        base().data(Vec::new()).build(),
        Err(RawCompressedTileBuildError::EmptyData)
    );
}

#[test]
fn raw_compressed_tile_build_errors_are_human_readable() {
    let cases = [
        (
            RawCompressedTileBuildError::MissingDimensions,
            "raw compressed tile dimensions are required",
        ),
        (
            RawCompressedTileBuildError::MissingBitsAllocated,
            "raw compressed tile bit depth is required",
        ),
        (
            RawCompressedTileBuildError::MissingSamplesPerPixel,
            "raw compressed tile sample count is required",
        ),
        (
            RawCompressedTileBuildError::MissingPhotometricInterpretation,
            "raw compressed tile photometric interpretation is required",
        ),
        (
            RawCompressedTileBuildError::MissingData,
            "raw compressed tile payload data is required",
        ),
        (
            RawCompressedTileBuildError::InvalidDimensions,
            "raw compressed tile dimensions must be positive",
        ),
        (
            RawCompressedTileBuildError::InvalidBitsAllocated,
            "raw compressed tile bit depth must be positive",
        ),
        (
            RawCompressedTileBuildError::InvalidSamplesPerPixel,
            "raw compressed tile sample count must be positive",
        ),
        (
            RawCompressedTileBuildError::EmptyData,
            "raw compressed tile payload data must not be empty",
        ),
    ];

    for (err, message) in cases {
        assert_eq!(err.to_string(), message);
        let wsi_error = WsiError::from(err);
        assert!(
            wsi_error.to_string().contains(message),
            "converted error should preserve context: {wsi_error}"
        );
    }
}

// --- PlaneSelection ---

#[test]
fn plane_selection_default_is_origin() {
    let plane = PlaneSelection::default();
    assert_eq!(plane.z, 0);
    assert_eq!(plane.c, 0);
    assert_eq!(plane.t, 0);
}

#[test]
fn plane_selection_new_sets_axis_indices() {
    let plane = PlaneSelection::new(1, 2, 3);
    assert_eq!(plane.z, 1);
    assert_eq!(plane.c, 2);
    assert_eq!(plane.t, 3);
}

#[test]
fn tile_entry_constructor_sets_optional_tiff_index() {
    let entry = TileEntry::new((10.5, 20.25), (256, 128)).with_tiff_tile_index(7);
    assert_eq!(entry.offset, (10.5, 20.25));
    assert_eq!(entry.dimensions, (256, 128));
    assert_eq!(entry.tiff_tile_index, Some(7));
}

// --- Request builders ---

#[test]
fn request_builders_default_to_origin_plane() {
    let region = RegionRequest::new(1usize, 2usize, 3u32, (10, 20), (30, 40));
    assert_eq!(region.scene, SceneId::new(1));
    assert_eq!(region.series, SeriesId::new(2));
    assert_eq!(region.level, LevelIdx::new(3));
    assert_eq!(region.plane, PlaneIdx::default());
    assert_eq!(region.origin_px, (10, 20));
    assert_eq!(region.size_px, (30, 40));

    let tile = TileRequest::new(SceneId::new(1), SeriesId::new(2), LevelIdx::new(3), 4, 5);
    assert_eq!(tile.scene, SceneId::new(1));
    assert_eq!(tile.series, SeriesId::new(2));
    assert_eq!(tile.level, LevelIdx::new(3));
    assert_eq!(tile.plane, PlaneIdx::default());
    assert_eq!(tile.col, 4);
    assert_eq!(tile.row, 5);

    let view = TileViewRequest::new(
        SceneId::new(1),
        SeriesId::new(2),
        LevelIdx::new(3),
        4,
        5,
        256,
        512,
    );
    assert_eq!(view.scene, SceneId::new(1));
    assert_eq!(view.series, SeriesId::new(2));
    assert_eq!(view.level, LevelIdx::new(3));
    assert_eq!(view.plane, PlaneIdx::default());
    assert_eq!(view.col, 4);
    assert_eq!(view.row, 5);
    assert_eq!(view.tile_width, 256);
    assert_eq!(view.tile_height, 512);
}

#[test]
fn tile_request_builders_use_typed_indices_for_public_read_paths() {
    let plane = PlaneIdx::new(PlaneSelection::new(1, 2, 3));

    let tile = TileRequest::new(SceneId::new(1), SeriesId::new(2), LevelIdx::new(3), 4, 5)
        .with_plane(plane);
    assert_eq!(tile.scene, SceneId::new(1));
    assert_eq!(tile.series, SeriesId::new(2));
    assert_eq!(tile.level, LevelIdx::new(3));
    assert_eq!(tile.plane, plane);
    assert_eq!(tile.col, 4);
    assert_eq!(tile.row, 5);

    let view = TileViewRequest::builder(SceneId::new(1), SeriesId::new(2), LevelIdx::new(3))
        .tile(4, 5)
        .tile_size(256, 512)
        .plane(plane)
        .build()
        .expect("complete display tile request should build");
    assert_eq!(view.scene, SceneId::new(1));
    assert_eq!(view.series, SeriesId::new(2));
    assert_eq!(view.level, LevelIdx::new(3));
    assert_eq!(view.plane, plane);
    assert_eq!(view.col, 4);
    assert_eq!(view.row, 5);
    assert_eq!(view.tile_width, 256);
    assert_eq!(view.tile_height, 512);
}

#[test]
fn tile_view_request_builder_supports_individual_coordinate_setters() {
    let view = TileViewRequest::builder(SceneId::new(1), SeriesId::new(2), LevelIdx::new(3))
        .col(4)
        .row(5)
        .tile_size(256, 512)
        .build()
        .expect("individual display tile coordinate setters should build");

    assert_eq!(view.scene, SceneId::new(1));
    assert_eq!(view.series, SeriesId::new(2));
    assert_eq!(view.level, LevelIdx::new(3));
    assert_eq!(view.col, 4);
    assert_eq!(view.row, 5);
    assert_eq!(view.tile_width, 256);
    assert_eq!(view.tile_height, 512);
}

#[test]
fn request_builders_set_planes_immutably() {
    let plane = PlaneSelection { z: 1, c: 2, t: 3 };

    let region = RegionRequest::new(
        SceneId::new(0),
        SeriesId::new(0),
        LevelIdx::new(0),
        (0, 0),
        (64, 64),
    )
    .with_plane(plane);
    assert_eq!(region.plane, PlaneIdx::new(plane));

    let tile = TileRequest::new(SceneId::new(0), SeriesId::new(0), LevelIdx::new(0), 0, 0)
        .with_plane(plane);
    assert_eq!(tile.plane, PlaneIdx::new(plane));

    let view = TileViewRequest::new(
        SceneId::new(0),
        SeriesId::new(0),
        LevelIdx::new(0),
        0,
        0,
        256,
        256,
    )
    .with_plane(plane);
    assert_eq!(view.plane, PlaneIdx::new(plane));
}

#[test]
fn request_builders_create_requests_with_named_required_fields() {
    let plane = PlaneSelection { z: 2, c: 3, t: 4 };

    let region = RegionRequest::builder(1usize, 2usize, 3u32)
        .origin_px((-10, 20))
        .size_px((300, 400))
        .plane(plane)
        .build()
        .expect("complete region request should build");
    assert_eq!(region.scene, SceneId::new(1));
    assert_eq!(region.series, SeriesId::new(2));
    assert_eq!(region.level, LevelIdx::new(3));
    assert_eq!(region.origin_px, (-10, 20));
    assert_eq!(region.size_px, (300, 400));
    assert_eq!(region.plane, PlaneIdx::new(plane));

    let tile = TileRequest::builder(SceneId::new(1), SeriesId::new(2), LevelIdx::new(3))
        .tile(4, 5)
        .plane(plane)
        .build()
        .expect("complete tile request should build");
    assert_eq!(tile.scene, SceneId::new(1));
    assert_eq!(tile.series, SeriesId::new(2));
    assert_eq!(tile.level, LevelIdx::new(3));
    assert_eq!(tile.col, 4);
    assert_eq!(tile.row, 5);
    assert_eq!(tile.plane, PlaneIdx::new(plane));

    let view = TileViewRequest::builder(SceneId::new(1), SeriesId::new(2), LevelIdx::new(3))
        .tile(4, 5)
        .tile_size(256, 512)
        .plane(plane)
        .build()
        .expect("complete tile view request should build");
    assert_eq!(view.scene, SceneId::new(1));
    assert_eq!(view.series, SeriesId::new(2));
    assert_eq!(view.level, LevelIdx::new(3));
    assert_eq!(view.col, 4);
    assert_eq!(view.row, 5);
    assert_eq!(view.tile_width, 256);
    assert_eq!(view.tile_height, 512);
    assert_eq!(view.plane, PlaneIdx::new(plane));
}

#[test]
fn request_builders_surface_missing_required_fields() {
    assert_eq!(
        RegionRequest::builder(SceneId::new(0), SeriesId::new(0), LevelIdx::new(0))
            .size_px((64, 64))
            .build()
            .unwrap_err(),
        RequestBuildError::MissingOrigin
    );
    assert_eq!(
        RegionRequest::builder(SceneId::new(0), SeriesId::new(0), LevelIdx::new(0))
            .origin_px((0, 0))
            .build()
            .unwrap_err(),
        RequestBuildError::MissingSize
    );
    assert_eq!(
        TileRequest::builder(SceneId::new(0), SeriesId::new(0), LevelIdx::new(0))
            .row(1)
            .build()
            .unwrap_err(),
        RequestBuildError::MissingColumn
    );
    assert_eq!(
        TileRequest::builder(SceneId::new(0), SeriesId::new(0), LevelIdx::new(0))
            .col(1)
            .build()
            .unwrap_err(),
        RequestBuildError::MissingRow
    );
    assert_eq!(
        TileViewRequest::builder(SceneId::new(0), SeriesId::new(0), LevelIdx::new(0))
            .tile(0, 0)
            .tile_height(256)
            .build()
            .unwrap_err(),
        RequestBuildError::MissingTileWidth
    );
    assert_eq!(
        TileViewRequest::builder(SceneId::new(0), SeriesId::new(0), LevelIdx::new(0))
            .tile(0, 0)
            .tile_width(256)
            .build()
            .unwrap_err(),
        RequestBuildError::MissingTileHeight
    );
}

#[test]
fn request_build_errors_are_human_readable() {
    let cases = [
        (
            RequestBuildError::MissingOrigin,
            "region request origin is required",
        ),
        (
            RequestBuildError::MissingSize,
            "region request size is required",
        ),
        (RequestBuildError::MissingColumn, "tile column is required"),
        (RequestBuildError::MissingRow, "tile row is required"),
        (
            RequestBuildError::MissingTileWidth,
            "display tile width is required",
        ),
        (
            RequestBuildError::MissingTileHeight,
            "display tile height is required",
        ),
    ];

    for (err, message) in cases {
        assert_eq!(err.to_string(), message);
    }
}

// --- DatasetId ---

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
fn tile_output_preference_constructors_map_correctly() {
    match TileOutputPreference::cpu() {
        TileOutputPreference::Cpu { backend } => {
            assert!(matches!(backend, OutputBackendRequest::Auto));
        }
        other => panic!("cpu() must produce Cpu/Auto, got {other:?}"),
    }
    match TileOutputPreference::cpu_only() {
        TileOutputPreference::Cpu { backend } => {
            assert!(matches!(backend, OutputBackendRequest::Cpu));
        }
        other => panic!("cpu_only() must produce Cpu/Cpu, got {other:?}"),
    }
    assert!(matches!(
        TileOutputPreference::prefer_device_auto(),
        TileOutputPreference::PreferDevice {
            backend: OutputBackendRequest::Auto,
            ..
        }
    ));
    assert!(matches!(
        TileOutputPreference::require_device_auto(),
        TileOutputPreference::RequireDevice {
            backend: OutputBackendRequest::Auto,
            ..
        }
    ));
    #[cfg(feature = "metal")]
    assert!(matches!(
        TileOutputPreference::require_metal(),
        TileOutputPreference::RequireDevice {
            backend: OutputBackendRequest::Metal,
            ..
        }
    ));
    #[cfg(feature = "cuda")]
    assert!(matches!(
        TileOutputPreference::require_cuda(),
        TileOutputPreference::RequireDevice {
            backend: OutputBackendRequest::Cuda,
            ..
        }
    ));
}

#[test]
fn pixel_format_reports_layout_metadata_for_all_variants() {
    let cases = [
        (PixelFormat::Rgb8, ColorSpace::Rgb, SampleType::Uint8, 3),
        (PixelFormat::Rgba8, ColorSpace::Rgba, SampleType::Uint8, 4),
        (
            PixelFormat::Gray8,
            ColorSpace::Grayscale,
            SampleType::Uint8,
            1,
        ),
        (PixelFormat::Rgb16, ColorSpace::Rgb, SampleType::Uint16, 3),
        (PixelFormat::Rgba16, ColorSpace::Rgba, SampleType::Uint16, 4),
        (
            PixelFormat::Gray16,
            ColorSpace::Grayscale,
            SampleType::Uint16,
            1,
        ),
    ];

    for (format, color_space, sample_type, channels) in cases {
        assert_eq!(format.color_space(), color_space);
        assert_eq!(format.sample_type(), sample_type);
        assert_eq!(format.channels(), channels);
        assert_eq!(format.bytes_per_sample(), sample_type.byte_size());
    }
}

#[test]
fn tile_output_preference_compressed_device_decode_is_explicit() {
    assert!(!TileOutputPreference::prefer_device_auto().compressed_device_decode_enabled());
    assert!(
        TileOutputPreference::prefer_device_auto_with_compressed_decode()
            .compressed_device_decode_enabled()
    );
    assert!(
        TileOutputPreference::require_device_auto_with_compressed_decode()
            .compressed_device_decode_enabled()
    );
    assert!(!TileOutputPreference::require_device_auto().compressed_device_decode_enabled());
    assert!(TileOutputPreference::require_device_auto().requires_device());
    assert!(TileOutputPreference::require_device_auto_with_compressed_decode().requires_device());
}

#[test]
fn tile_output_preference_can_disable_adaptive_decode_route() {
    let preference = TileOutputPreference::prefer_device_auto_with_compressed_decode();
    assert!(preference.adaptive_decode_route_enabled());

    let preference = preference.without_adaptive_decode_route();
    assert!(!preference.adaptive_decode_route_enabled());
    assert!(preference.compressed_device_decode_enabled());
}

#[cfg(feature = "cuda")]
#[test]
fn tile_output_preference_cuda_constructors_attach_sessions() {
    let sessions = crate::output::cuda::CudaBackendSessions::new();

    match TileOutputPreference::prefer_device_auto_with_cuda(sessions.clone()) {
        TileOutputPreference::PreferDevice {
            backend: OutputBackendRequest::Auto,
            context,
        } => {
            assert!(context.cuda().is_some());
            assert!(!context.compressed_device_decode());
        }
        other => {
            panic!("prefer_device_auto_with_cuda must produce PreferDevice/Auto, got {other:?}")
        }
    }

    match TileOutputPreference::prefer_device_auto_with_cuda_and_compressed_decode(
        sessions.clone(),
    ) {
        TileOutputPreference::PreferDevice {
            backend: OutputBackendRequest::Auto,
            context,
        } => {
            assert!(context.cuda().is_some());
            assert!(context.compressed_device_decode());
        }
        other => panic!(
            "prefer_device_auto_with_cuda_and_compressed_decode must produce PreferDevice/Auto, got {other:?}"
        ),
    }

    match TileOutputPreference::require_device_auto_with_cuda_and_compressed_decode(sessions) {
        TileOutputPreference::RequireDevice {
            backend: OutputBackendRequest::Auto,
            context,
        } => {
            assert!(context.cuda().is_some());
            assert!(context.compressed_device_decode());
        }
        other => panic!(
            "require_device_auto_with_cuda_and_compressed_decode must produce RequireDevice/Auto, got {other:?}"
        ),
    }
}

#[cfg(feature = "cuda")]
#[test]
fn device_output_context_holds_cuda_sessions() {
    let sessions = crate::output::cuda::CudaBackendSessions::new();
    let context = DeviceOutputContext::with_cuda(sessions);

    assert!(context.cuda().is_some());
    assert!(!context.compressed_device_decode());
    assert!(context.adaptive_decode_route());
}

#[cfg(feature = "cuda")]
#[test]
fn cuda_output_types_are_clone_debug_surfaces() {
    fn assert_clone_debug<T: Clone + std::fmt::Debug>() {}

    assert_clone_debug::<crate::output::cuda::CudaBackendSessions>();
    assert_clone_debug::<crate::output::cuda::CudaDeviceStorage>();
    assert_clone_debug::<crate::output::cuda::CudaDeviceTile>();
}

// --- TileLayout intersection ---

#[test]
fn regular_tiles_for_region_basic() {
    let layout = TileLayout::Regular {
        tile_width: 256,
        tile_height: 256,
        tiles_across: 4,
        tiles_down: 4,
    };
    // 300x300 at (100, 100) → cols 0-1, rows 0-1 → 4 tiles
    let tiles = layout.tiles_for_region(100, 100, 300, 300);
    assert_eq!(tiles.len(), 4);
    let coords: Vec<(i64, i64)> = tiles.iter().map(|t| (t.col, t.row)).collect();
    assert!(coords.contains(&(0, 0)));
    assert!(coords.contains(&(1, 0)));
    assert!(coords.contains(&(0, 1)));
    assert!(coords.contains(&(1, 1)));
}

#[test]
fn regular_tiles_single_tile() {
    let layout = TileLayout::Regular {
        tile_width: 256,
        tile_height: 256,
        tiles_across: 4,
        tiles_down: 4,
    };
    let tiles = layout.tiles_for_region(0, 0, 100, 100);
    assert_eq!(tiles.len(), 1);
    assert_eq!(tiles[0].col, 0);
    assert_eq!(tiles[0].row, 0);
}

#[test]
fn regular_tiles_clipped_at_bounds() {
    let layout = TileLayout::Regular {
        tile_width: 256,
        tile_height: 256,
        tiles_across: 2,
        tiles_down: 2,
    };
    // Region extends beyond grid
    let tiles = layout.tiles_for_region(256, 256, 512, 512);
    assert_eq!(tiles.len(), 1);
    assert_eq!(tiles[0].col, 1);
    assert_eq!(tiles[0].row, 1);
}

#[test]
fn regular_tiles_negative_coords() {
    let layout = TileLayout::Regular {
        tile_width: 256,
        tile_height: 256,
        tiles_across: 4,
        tiles_down: 4,
    };
    // Negative start — only in-bounds tiles returned
    let tiles = layout.tiles_for_region(-100, -100, 200, 200);
    assert_eq!(tiles.len(), 1);
    assert_eq!(tiles[0].col, 0);
    assert_eq!(tiles[0].row, 0);
}

#[test]
fn whole_level_tiles_for_region() {
    let layout = TileLayout::WholeLevel {
        width: 1024,
        height: 768,
        virtual_tile_width: 256,
        virtual_tile_height: 256,
    };
    // Region covering the entire image → ceil(1024/256) * ceil(768/256) = 4*3 = 12 tiles
    let tiles = layout.tiles_for_region(0, 0, 1024, 768);
    assert_eq!(tiles.len(), 12);
}

#[test]
fn whole_level_small_region() {
    let layout = TileLayout::WholeLevel {
        width: 4096,
        height: 4096,
        virtual_tile_width: 512,
        virtual_tile_height: 512,
    };
    // 100x100 at origin → 1 tile
    let tiles = layout.tiles_for_region(0, 0, 100, 100);
    assert_eq!(tiles.len(), 1);
    assert_eq!(tiles[0].col, 0);
    assert_eq!(tiles[0].row, 0);
}

#[test]
fn whole_level_negative_coords_clamp_to_first_tile() {
    let layout = TileLayout::WholeLevel {
        width: 1024,
        height: 1024,
        virtual_tile_width: 256,
        virtual_tile_height: 256,
    };

    let tiles = layout.tiles_for_region(-300, -300, 400, 400);
    assert_eq!(tiles.len(), 1);
    assert_eq!(tiles[0].col, 0);
    assert_eq!(tiles[0].row, 0);
    assert_eq!(tiles[0].dest_x, 300);
    assert_eq!(tiles[0].dest_y, 300);
}

#[test]
fn irregular_tiles_for_region_basic() {
    let mut tiles_map = std::collections::HashMap::new();
    tiles_map.insert(
        (0i64, 0i64),
        TileEntry {
            offset: (0.0, 0.0),
            dimensions: (256, 256),
            tiff_tile_index: None,
        },
    );
    tiles_map.insert(
        (1, 0),
        TileEntry {
            offset: (5.0, 0.0),
            dimensions: (256, 256),
            tiff_tile_index: None,
        },
    );
    tiles_map.insert(
        (0, 1),
        TileEntry {
            offset: (0.0, 3.0),
            dimensions: (256, 256),
            tiff_tile_index: None,
        },
    );

    let layout = TileLayout::Irregular {
        tile_advance: (256.0, 256.0),
        extra_tiles: (1, 0, 1, 0),
        tiles: tiles_map,
    };

    let result = layout.tiles_for_region(0, 0, 512, 512);
    assert_eq!(result.len(), 3);
}

#[test]
fn irregular_tiles_negative_offset() {
    let mut tiles_map = std::collections::HashMap::new();
    tiles_map.insert(
        (0i64, 0i64),
        TileEntry {
            offset: (-10.0, -5.0),
            dimensions: (256, 256),
            tiff_tile_index: None,
        },
    );

    let layout = TileLayout::Irregular {
        tile_advance: (256.0, 256.0),
        extra_tiles: (0, 1, 0, 1),
        tiles: tiles_map,
    };

    // Tile actual position is (-10, -5) to (246, 251)
    // Region (0, 0, 100, 100) should hit it
    let result = layout.tiles_for_region(0, 0, 100, 100);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].dest_x, -10);
    assert_eq!(result[0].dest_y, -5);
}

#[test]
fn irregular_tiles_no_match() {
    let mut tiles_map = std::collections::HashMap::new();
    tiles_map.insert(
        (0i64, 0i64),
        TileEntry {
            offset: (0.0, 0.0),
            dimensions: (256, 256),
            tiff_tile_index: None,
        },
    );

    let layout = TileLayout::Irregular {
        tile_advance: (256.0, 256.0),
        extra_tiles: (0, 0, 0, 0),
        tiles: tiles_map,
    };

    let result = layout.tiles_for_region(10000, 10000, 100, 100);
    assert_eq!(result.len(), 0);
}

#[test]
fn tile_layout_zero_tile_dimensions_return_no_hits_instead_of_panicking() {
    let regular = TileLayout::Regular {
        tile_width: 0,
        tile_height: 256,
        tiles_across: 1,
        tiles_down: 1,
    };
    assert!(regular.tiles_for_region(0, 0, 64, 64).is_empty());

    let regular = TileLayout::Regular {
        tile_width: 256,
        tile_height: 0,
        tiles_across: 1,
        tiles_down: 1,
    };
    assert!(regular.tiles_for_region(0, 0, 64, 64).is_empty());

    let whole_level = TileLayout::WholeLevel {
        width: 1024,
        height: 1024,
        virtual_tile_width: 0,
        virtual_tile_height: 256,
    };
    assert!(whole_level.tiles_for_region(0, 0, 64, 64).is_empty());

    let whole_level = TileLayout::WholeLevel {
        width: 1024,
        height: 1024,
        virtual_tile_width: 256,
        virtual_tile_height: 0,
    };
    assert!(whole_level.tiles_for_region(0, 0, 64, 64).is_empty());
}

#[test]
fn tile_layout_extreme_region_coordinates_return_no_hits_instead_of_panicking() {
    let regular = TileLayout::Regular {
        tile_width: 256,
        tile_height: 256,
        tiles_across: 4,
        tiles_down: 4,
    };
    assert!(regular
        .tiles_for_region(i64::MAX - 8, i64::MAX - 8, 64, 64)
        .is_empty());
    assert!(regular
        .tiles_for_region(i64::MIN + 8, i64::MIN + 8, 64, 64)
        .is_empty());

    let whole_level = TileLayout::WholeLevel {
        width: 1024,
        height: 1024,
        virtual_tile_width: 256,
        virtual_tile_height: 256,
    };
    assert!(whole_level
        .tiles_for_region(i64::MAX - 8, i64::MAX - 8, 64, 64)
        .is_empty());
    assert!(whole_level
        .tiles_for_region(i64::MIN + 8, i64::MIN + 8, 64, 64)
        .is_empty());
}

// --- Compression ---

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

#[test]
fn to_rgba_from_rgb_u8() {
    let buf = CpuTile {
        width: 2,
        height: 1,
        channels: 3,
        color_space: ColorSpace::Rgb,
        layout: CpuTileLayout::Interleaved,
        data: CpuTileData::u8(vec![255, 0, 0, 0, 255, 0]),
    };
    let img = buf.to_rgba().unwrap();
    assert_eq!(img.get_pixel(0, 0).0, [255, 0, 0, 255]);
    assert_eq!(img.get_pixel(1, 0).0, [0, 255, 0, 255]);
}

#[test]
fn to_rgba_from_rgb_u8_planar() {
    let buf = CpuTile {
        width: 2,
        height: 1,
        channels: 3,
        color_space: ColorSpace::Rgb,
        layout: CpuTileLayout::Planar,
        data: CpuTileData::u8(vec![255, 0, 0, 255, 0, 0]),
    };
    let img = buf.to_rgba().unwrap();
    assert_eq!(img.get_pixel(0, 0).0, [255, 0, 0, 255]);
    assert_eq!(img.get_pixel(1, 0).0, [0, 255, 0, 255]);
}

#[test]
fn to_rgba_from_grayscale_u8() {
    let buf = CpuTile {
        width: 1,
        height: 1,
        channels: 1,
        color_space: ColorSpace::Grayscale,
        layout: CpuTileLayout::Interleaved,
        data: CpuTileData::u8(vec![128]),
    };
    let img = buf.to_rgba().unwrap();
    assert_eq!(img.get_pixel(0, 0).0, [128, 128, 128, 255]);
}

#[test]
fn to_rgba_from_palette() {
    let lut = vec![[255, 0, 0], [0, 255, 0]];
    let buf = CpuTile {
        width: 2,
        height: 1,
        channels: 1,
        color_space: ColorSpace::Palette(Arc::new(lut)),
        layout: CpuTileLayout::Interleaved,
        data: CpuTileData::u8(vec![0, 1]),
    };
    let img = buf.to_rgba().unwrap();
    assert_eq!(img.get_pixel(0, 0).0, [255, 0, 0, 255]);
    assert_eq!(img.get_pixel(1, 0).0, [0, 255, 0, 255]);
}

#[test]
fn to_rgba_rejects_non_u8() {
    let buf = CpuTile {
        width: 1,
        height: 1,
        channels: 1,
        color_space: ColorSpace::Grayscale,
        layout: CpuTileLayout::Interleaved,
        data: CpuTileData::u16(vec![1000]),
    };
    assert!(buf.to_rgba().is_err());
}

#[test]
fn to_rgba_windowed_u16() {
    let buf = CpuTile {
        width: 2,
        height: 1,
        channels: 1,
        color_space: ColorSpace::Grayscale,
        layout: CpuTileLayout::Interleaved,
        data: CpuTileData::u16(vec![0, 1000]),
    };
    let window = DisplayWindow::new(0.0, 1000.0).unwrap();
    let img = buf.to_rgba_windowed(&window).unwrap();
    assert_eq!(img.get_pixel(0, 0).0[0], 0); // 0 maps to 0
    assert_eq!(img.get_pixel(1, 0).0[0], 255); // 1000 maps to 255
}

#[test]
fn to_rgba_windowed_u16_planar_rgb() {
    let buf = CpuTile {
        width: 2,
        height: 1,
        channels: 3,
        color_space: ColorSpace::Rgb,
        layout: CpuTileLayout::Planar,
        data: CpuTileData::u16(vec![0, 1000, 0, 1000, 0, 0]),
    };
    let window = DisplayWindow::new(0.0, 1000.0).unwrap();
    let img = buf.to_rgba_windowed(&window).unwrap();
    assert_eq!(img.get_pixel(0, 0).0, [0, 0, 0, 255]);
    assert_eq!(img.get_pixel(1, 0).0, [255, 255, 0, 255]);
}

#[test]
fn display_window_new_accepts_positive_finite_range() {
    let window = DisplayWindow::new(0.0, 1000.0).unwrap();
    assert_eq!(window.min(), 0.0);
    assert_eq!(window.max(), 1000.0);
}

#[test]
fn display_window_new_rejects_invalid_bounds() {
    for (min, max) in [
        (50.0, 50.0),
        (100.0, 50.0),
        (f64::NAN, 100.0),
        (0.0, f64::INFINITY),
    ] {
        let err = DisplayWindow::new(min, max).unwrap_err();
        assert!(matches!(err, WsiError::DisplayConversion(_)));
    }
}

#[test]
fn display_window_new_rejects_zero_range_before_conversion() {
    let err = DisplayWindow::new(50.0, 50.0).unwrap_err();
    assert!(matches!(err, WsiError::DisplayConversion(_)));
}

#[test]
fn to_rgb_from_rgb_u8() {
    let buf = CpuTile {
        width: 1,
        height: 1,
        channels: 3,
        color_space: ColorSpace::Rgb,
        layout: CpuTileLayout::Interleaved,
        data: CpuTileData::u8(vec![100, 150, 200]),
    };
    let img = buf.to_rgb().unwrap();
    assert_eq!(img.get_pixel(0, 0).0, [100, 150, 200]);
}

#[test]
fn into_rgb_reuses_interleaved_rgb_storage() {
    let raw = vec![100, 150, 200];
    let ptr = raw.as_ptr();
    let buf = CpuTile {
        width: 1,
        height: 1,
        channels: 3,
        color_space: ColorSpace::Rgb,
        layout: CpuTileLayout::Interleaved,
        data: CpuTileData::u8(raw),
    };
    let img = buf.into_rgb().unwrap();
    assert_eq!(img.as_raw().as_ptr(), ptr);
    assert_eq!(img.get_pixel(0, 0).0, [100, 150, 200]);
}

#[test]
fn into_rgba_reuses_interleaved_rgba_storage() {
    let raw = vec![100, 150, 200, 255];
    let ptr = raw.as_ptr();
    let buf = CpuTile {
        width: 1,
        height: 1,
        channels: 4,
        color_space: ColorSpace::Rgba,
        layout: CpuTileLayout::Interleaved,
        data: CpuTileData::u8(raw),
    };
    let img = buf.into_rgba().unwrap();
    assert_eq!(img.as_raw().as_ptr(), ptr);
    assert_eq!(img.get_pixel(0, 0).0, [100, 150, 200, 255]);
}

// --- CpuTile::new() ---

#[test]
fn sample_buffer_new_valid() {
    let buf = CpuTile::new(
        2,
        1,
        3,
        ColorSpace::Rgb,
        CpuTileLayout::Interleaved,
        CpuTileData::u8(vec![0; 6]),
    );
    assert!(buf.is_ok());
    assert_eq!(buf.unwrap().width, 2);
}

#[test]
fn cpu_tile_accessors_expose_validated_metadata() {
    let tile = CpuTile::new(
        2,
        1,
        3,
        ColorSpace::Rgb,
        CpuTileLayout::Interleaved,
        CpuTileData::u8(vec![10, 20, 30, 40, 50, 60]),
    )
    .expect("valid tile should build");

    assert_eq!(tile.width(), 2);
    assert_eq!(tile.height(), 1);
    assert_eq!(tile.channels(), 3);
    assert_eq!(tile.color_space(), &ColorSpace::Rgb);
    assert_eq!(tile.layout(), CpuTileLayout::Interleaved);
    assert_eq!(tile.data().as_u8().unwrap(), &[10, 20, 30, 40, 50, 60]);
}

#[test]
fn sample_buffer_new_invalid_length() {
    let buf = CpuTile::new(
        2,
        1,
        3,
        ColorSpace::Rgb,
        CpuTileLayout::Interleaved,
        CpuTileData::u8(vec![0; 5]),
    );
    assert!(buf.is_err());
}

#[test]
#[should_panic(expected = "CpuTile::new_for_test currently stores packed interleaved data")]
fn cpu_tile_new_for_test_rejects_padded_stride() {
    CpuTile::new_for_test(
        Arc::<[u8]>::from(vec![0u8; 32]),
        2,
        2,
        16,
        PixelFormat::Rgba8,
    );
}

#[test]
fn sample_buffer_new_overflow_dimensions() {
    let buf = CpuTile::new(
        u32::MAX,
        u32::MAX,
        3,
        ColorSpace::Rgb,
        CpuTileLayout::Interleaved,
        CpuTileData::u8(vec![]),
    );
    assert!(buf.is_err());
}

// --- Direct to_rgb() paths ---

#[test]
fn to_rgb_direct_path_rgb8() {
    let buf = CpuTile {
        width: 2,
        height: 1,
        channels: 3,
        color_space: ColorSpace::Rgb,
        layout: CpuTileLayout::Interleaved,
        data: CpuTileData::u8(vec![255, 0, 0, 0, 255, 0]),
    };
    let img = buf.to_rgb().unwrap();
    assert_eq!(img.get_pixel(0, 0).0, [255, 0, 0]);
    assert_eq!(img.get_pixel(1, 0).0, [0, 255, 0]);
}

#[test]
fn to_rgb_direct_path_grayscale() {
    let buf = CpuTile {
        width: 1,
        height: 1,
        channels: 1,
        color_space: ColorSpace::Grayscale,
        layout: CpuTileLayout::Interleaved,
        data: CpuTileData::u8(vec![128]),
    };
    let img = buf.to_rgb().unwrap();
    assert_eq!(img.get_pixel(0, 0).0, [128, 128, 128]);
}

#[test]
fn to_rgb_rejects_non_u8() {
    let buf = CpuTile {
        width: 1,
        height: 1,
        channels: 1,
        color_space: ColorSpace::Grayscale,
        layout: CpuTileLayout::Interleaved,
        data: CpuTileData::u16(vec![1000]),
    };
    assert!(buf.to_rgb().is_err());
}

#[test]
fn to_rgb_windowed_u16_direct() {
    let buf = CpuTile {
        width: 2,
        height: 1,
        channels: 1,
        color_space: ColorSpace::Grayscale,
        layout: CpuTileLayout::Interleaved,
        data: CpuTileData::u16(vec![0, 1000]),
    };
    let window = DisplayWindow::new(0.0, 1000.0).unwrap();
    let img = buf.to_rgb_windowed(&window).unwrap();
    assert_eq!(img.get_pixel(0, 0).0, [0, 0, 0]);
    assert_eq!(img.get_pixel(1, 0).0, [255, 255, 255]);
}

#[test]
fn to_rgb_windowed_f32_3ch_direct() {
    let buf = CpuTile {
        width: 1,
        height: 1,
        channels: 3,
        color_space: ColorSpace::Rgb,
        layout: CpuTileLayout::Interleaved,
        data: CpuTileData::f32(vec![0.0, 0.5, 1.0]),
    };
    let window = DisplayWindow::new(0.0, 1.0).unwrap();
    let img = buf.to_rgb_windowed(&window).unwrap();
    assert_eq!(img.get_pixel(0, 0).0, [0, 128, 255]);
}

// --- Arc Palette ---

#[test]
fn palette_clone_is_cheap() {
    let lut = Arc::new(vec![[255, 0, 0]; 256]);
    let cs = ColorSpace::Palette(lut.clone());
    let cs2 = cs.clone();
    drop(cs2);
    assert_eq!(Arc::strong_count(&lut), 2); // original + cs
}
