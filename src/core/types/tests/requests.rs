use super::super::*;

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
