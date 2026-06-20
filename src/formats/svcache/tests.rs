use super::build::{
    cache_grid_for_level, copy_existing_svcache_tiles, copy_existing_svcache_tiles_with_policy,
    metadata_shell, write_svcache_file, write_tile_payload, ExistingTilePolicy,
};
use super::storage::{fingerprint_source, is_fresh_svcache, read_svcache};
use super::*;
use crate::core::types::CpuTile;

#[test]
fn svcache_tile_selection_constructor_defaults_to_origin_plane() {
    let selection = SvcacheTileSelection::new(1usize, 2usize, 3u32, 4, 5)
        .with_plane(PlaneSelection::new(6, 7, 8));

    assert_eq!(selection.scene.get(), 1);
    assert_eq!(selection.series.get(), 2);
    assert_eq!(selection.level.get(), 3);
    assert_eq!(selection.col, 4);
    assert_eq!(selection.row, 5);
    assert_eq!(selection.plane.get(), PlaneSelection::new(6, 7, 8));
}

#[test]
fn svcache_round_trips_single_tile() {
    let mut payload = tempfile::tempfile().unwrap();
    let tile =
        CpuTile::from_u8_interleaved(1, 1, 3, ColorSpace::Rgb, vec![10_u8, 20_u8, 30_u8]).unwrap();
    let tile_meta = write_tile_payload(&mut payload, &tile).unwrap();
    let source = tempfile::NamedTempFile::new().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let out_path = out_dir.path().join("roundtrip.svcache");
    let metadata = SvcacheMetadata {
        schema_version: SCHEMA_VERSION,
        complete: true,
        source: fingerprint_source(source.path()).unwrap(),
        properties: Vec::new(),
        scenes: vec![SceneMeta {
            id: "scene-0".into(),
            name: None,
            series: vec![SeriesMeta {
                id: "series-0".into(),
                axes: AxesMeta { z: 1, c: 1, t: 1 },
                sample_type: SampleTypeMeta::Uint8,
                channels: Vec::new(),
                levels: vec![LevelMeta {
                    dimensions: (1, 1),
                    downsample: 1.0,
                    tile_width: 1,
                    tile_height: 1,
                    tiles_across: 1,
                    tiles_down: 1,
                    tiles: vec![Some(tile_meta)],
                    sparse_tiles: Vec::new(),
                }],
            }],
        }],
        associated: Vec::new(),
    };
    write_svcache_file(&out_path, &metadata, payload).unwrap();

    let backend = SvcacheBackend::new();
    let reader = backend.open(&out_path).unwrap();
    let decoded = reader
        .read_tile_cpu(&TileRequest {
            scene: 0usize.into(),
            series: 0usize.into(),
            level: 0u32.into(),
            plane: PlaneSelection::default().into(),
            col: 0,
            row: 0,
        })
        .unwrap();

    assert_eq!(decoded.data.as_u8().unwrap(), &[10, 20, 30]);
}

#[test]
fn svcache_sparse_level_reports_missing_tile() {
    let payload = tempfile::tempfile().unwrap();
    let source = tempfile::NamedTempFile::new().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let out_path = out_dir.path().join("sparse.svcache");
    let metadata = SvcacheMetadata {
        schema_version: SCHEMA_VERSION,
        complete: false,
        source: fingerprint_source(source.path()).unwrap(),
        properties: Vec::new(),
        scenes: vec![SceneMeta {
            id: "scene-0".into(),
            name: None,
            series: vec![SeriesMeta {
                id: "series-0".into(),
                axes: AxesMeta { z: 1, c: 1, t: 1 },
                sample_type: SampleTypeMeta::Uint8,
                channels: Vec::new(),
                levels: vec![LevelMeta {
                    dimensions: (2, 1),
                    downsample: 1.0,
                    tile_width: 1,
                    tile_height: 1,
                    tiles_across: 2,
                    tiles_down: 1,
                    tiles: vec![None, None],
                    sparse_tiles: Vec::new(),
                }],
            }],
        }],
        associated: Vec::new(),
    };
    write_svcache_file(&out_path, &metadata, payload).unwrap();

    let backend = SvcacheBackend::new();
    let reader = backend.open(&out_path).unwrap();
    let err = reader
        .read_tile_cpu(&TileRequest {
            scene: 0usize.into(),
            series: 0usize.into(),
            level: 0u32.into(),
            plane: PlaneSelection::default().into(),
            col: 1,
            row: 0,
        })
        .unwrap_err();

    assert!(
        err.to_string().contains(".svcache tile not populated"),
        "unexpected error: {err}"
    );
}

#[test]
fn whole_level_cache_grid_uses_display_tiles() {
    let level = Level {
        dimensions: (3_596, 2_912),
        downsample: 32.0,
        tile_layout: TileLayout::WholeLevel {
            width: 3_596,
            height: 2_912,
            virtual_tile_width: 3_596,
            virtual_tile_height: 2_912,
        },
    };

    assert_eq!(cache_grid_for_level(&level), (256, 256, 15, 12));
}

#[test]
fn partial_svcache_metadata_shell_starts_sparse() {
    let dataset = Dataset {
        id: DatasetId::new(42),
        scenes: vec![Scene {
            id: "scene-0".into(),
            name: None,
            series: vec![Series {
                id: "series-0".into(),
                axes: AxesShape::default(),
                levels: vec![Level {
                    dimensions: (65_536, 65_536),
                    downsample: 1.0,
                    tile_layout: TileLayout::Regular {
                        tile_width: 256,
                        tile_height: 256,
                        tiles_across: 256,
                        tiles_down: 256,
                    },
                }],
                sample_type: SampleType::Uint8,
                channels: Vec::new(),
            }],
        }],
        associated_images: std::collections::HashMap::new(),
        properties: Properties::new(),
        icc_profiles: std::collections::HashMap::new(),
        source_icc_profiles: Vec::new(),
    };

    let scenes = metadata_shell(&dataset).unwrap();
    let level = &scenes[0].series[0].levels[0];

    assert!(level.tiles.is_empty());
    assert!(level.sparse_tiles.is_empty());
    assert_eq!(level.tiles_across, 256);
    assert_eq!(level.tiles_down, 256);
}

#[test]
fn sparse_svcache_is_not_fresh_for_auto_resolution() {
    let payload = tempfile::tempfile().unwrap();
    let source = tempfile::NamedTempFile::new().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let out_path = out_dir.path().join("sparse.svcache");
    let metadata = SvcacheMetadata {
        schema_version: SCHEMA_VERSION,
        complete: false,
        source: fingerprint_source(source.path()).unwrap(),
        properties: Vec::new(),
        scenes: vec![SceneMeta {
            id: "scene-0".into(),
            name: None,
            series: Vec::new(),
        }],
        associated: Vec::new(),
    };
    write_svcache_file(&out_path, &metadata, payload).unwrap();

    assert!(!is_fresh_svcache(&out_path, source.path()).unwrap());
}

#[test]
fn sparse_svcache_can_match_source_for_read_through_overlay() {
    let payload = tempfile::tempfile().unwrap();
    let source = tempfile::NamedTempFile::new().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let out_path = out_dir.path().join("sparse-overlay.svcache");
    let metadata = SvcacheMetadata {
        schema_version: SCHEMA_VERSION,
        complete: false,
        source: fingerprint_source(source.path()).unwrap(),
        properties: Vec::new(),
        scenes: Vec::new(),
        associated: Vec::new(),
    };
    write_svcache_file(&out_path, &metadata, payload).unwrap();

    assert!(svcache_matches_source(&out_path, source.path()).unwrap());
}

#[test]
fn sparse_svcache_merge_preserves_existing_tiles() {
    let mut existing_payload = tempfile::tempfile().unwrap();
    let tile =
        CpuTile::from_u8_interleaved(1, 1, 3, ColorSpace::Rgb, vec![1_u8, 2_u8, 3_u8]).unwrap();
    let existing_tile = write_tile_payload(&mut existing_payload, &tile).unwrap();
    let source = tempfile::NamedTempFile::new().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let out_path = out_dir.path().join("merge.svcache");
    let metadata = SvcacheMetadata {
        schema_version: SCHEMA_VERSION,
        complete: false,
        source: fingerprint_source(source.path()).unwrap(),
        properties: Vec::new(),
        scenes: vec![SceneMeta {
            id: "scene-0".into(),
            name: None,
            series: vec![SeriesMeta {
                id: "series-0".into(),
                axes: AxesMeta { z: 1, c: 1, t: 1 },
                sample_type: SampleTypeMeta::Uint8,
                channels: Vec::new(),
                levels: vec![LevelMeta {
                    dimensions: (2, 1),
                    downsample: 1.0,
                    tile_width: 1,
                    tile_height: 1,
                    tiles_across: 2,
                    tiles_down: 1,
                    tiles: vec![Some(existing_tile), None],
                    sparse_tiles: Vec::new(),
                }],
            }],
        }],
        associated: Vec::new(),
    };
    write_svcache_file(&out_path, &metadata, existing_payload).unwrap();

    let mut merged_payload = tempfile::tempfile().unwrap();
    let mut scenes = metadata.scenes.clone();
    scenes[0].series[0].levels[0].tiles = vec![None, None];

    let copied =
        copy_existing_svcache_tiles(&out_path, source.path(), &mut scenes, &mut merged_payload)
            .unwrap();

    assert_eq!(copied, 1);
    assert!(scenes[0].series[0].levels[0].tiles[0].is_some());
    assert!(scenes[0].series[0].levels[0].tiles[1].is_none());
}

#[test]
fn sparse_svcache_replace_does_not_copy_existing_tiles() {
    let mut existing_payload = tempfile::tempfile().unwrap();
    let tile =
        CpuTile::from_u8_interleaved(1, 1, 3, ColorSpace::Rgb, vec![1_u8, 2_u8, 3_u8]).unwrap();
    let existing_tile = write_tile_payload(&mut existing_payload, &tile).unwrap();
    let source = tempfile::NamedTempFile::new().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let out_path = out_dir.path().join("replace.svcache");
    let metadata = SvcacheMetadata {
        schema_version: SCHEMA_VERSION,
        complete: false,
        source: fingerprint_source(source.path()).unwrap(),
        properties: Vec::new(),
        scenes: vec![SceneMeta {
            id: "scene-0".into(),
            name: None,
            series: vec![SeriesMeta {
                id: "series-0".into(),
                axes: AxesMeta { z: 1, c: 1, t: 1 },
                sample_type: SampleTypeMeta::Uint8,
                channels: Vec::new(),
                levels: vec![LevelMeta {
                    dimensions: (2, 1),
                    downsample: 1.0,
                    tile_width: 1,
                    tile_height: 1,
                    tiles_across: 2,
                    tiles_down: 1,
                    tiles: vec![Some(existing_tile), None],
                    sparse_tiles: Vec::new(),
                }],
            }],
        }],
        associated: Vec::new(),
    };
    write_svcache_file(&out_path, &metadata, existing_payload).unwrap();

    let mut replacement_payload = tempfile::tempfile().unwrap();
    let mut scenes = metadata.scenes.clone();
    scenes[0].series[0].levels[0].tiles = vec![None, None];

    let copied = copy_existing_svcache_tiles_with_policy(
        &out_path,
        source.path(),
        &mut scenes,
        &mut replacement_payload,
        ExistingTilePolicy::Replace,
    )
    .unwrap();

    assert_eq!(copied, 0);
    assert!(scenes[0].series[0].levels[0].tiles[0].is_none());
    assert!(scenes[0].series[0].levels[0].tiles[1].is_none());
}

#[test]
fn build_svcache_tiles_replace_rewrites_selected_tiles_when_cache_exists() {
    let mut source = tempfile::Builder::new().suffix(".j2c").tempfile().unwrap();
    source
        .write_all(include_bytes!("../../../tests/fixtures/jp2k/rgb_nomct.j2k"))
        .unwrap();
    source.flush().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let out_path = out_dir.path().join("viewport.svcache");
    let selections = [SvcacheTileSelection::new(
        SceneId::new(0),
        SeriesId::new(0),
        LevelIdx::new(0),
        0,
        0,
    )];

    let first_written = build_svcache_tiles(source.path(), &out_path, &selections).unwrap();
    let merged_written = build_svcache_tiles(source.path(), &out_path, &selections).unwrap();
    let replaced_written =
        build_svcache_tiles_replace(source.path(), &out_path, &selections).unwrap();

    assert_eq!(first_written, 1);
    assert_eq!(merged_written, 0);
    assert_eq!(
        replaced_written, 1,
        "replace mode must not treat copied existing tiles as already populated"
    );

    let (_, _, metadata) = read_svcache(&out_path).unwrap();
    let level = &metadata.scenes[0].series[0].levels[0];
    assert!(
        level.tiles.is_empty(),
        "viewport caches must not serialize dense empty tile slots"
    );
    assert_eq!(level.sparse_tiles.len(), 1);
    assert_eq!(level.sparse_tiles[0].index, 0);

    let backend = SvcacheBackend::new();
    let reader = backend.open(&out_path).unwrap();
    reader
        .read_tile_cpu(&TileRequest {
            scene: 0usize.into(),
            series: 0usize.into(),
            level: 0u32.into(),
            plane: PlaneSelection::default().into(),
            col: 0,
            row: 0,
        })
        .unwrap();
}

#[test]
fn build_svcache_deduplicates_plane_variants_by_tile_slot_after_sorting() {
    let mut source = tempfile::Builder::new().suffix(".j2c").tempfile().unwrap();
    source
        .write_all(include_bytes!("../../../tests/fixtures/jp2k/rgb_nomct.j2k"))
        .unwrap();
    source.flush().unwrap();

    let z0 = SvcacheTileSelection::new(SceneId::new(0), SeriesId::new(0), LevelIdx::new(0), 0, 0)
        .with_plane(PlaneSelection::new(0, 0, 0));
    let z1 = SvcacheTileSelection::new(SceneId::new(0), SeriesId::new(0), LevelIdx::new(0), 0, 0)
        .with_plane(PlaneSelection::new(1, 0, 0));

    let out_dir = tempfile::tempdir().unwrap();
    let dense_path = out_dir.path().join("dense.svcache");
    let written = build_svcache_tiles(source.path(), &dense_path, &[z1, z0]).unwrap();
    assert_eq!(written, 1);

    let decoded_path = out_dir.path().join("decoded.svcache");
    let first =
        CpuTile::from_u8_interleaved(1, 1, 3, ColorSpace::Rgb, vec![1_u8, 2_u8, 3_u8]).unwrap();
    let second =
        CpuTile::from_u8_interleaved(1, 1, 3, ColorSpace::Rgb, vec![4_u8, 5_u8, 6_u8]).unwrap();
    let written = build_svcache_tile_payloads_replace(
        source.path(),
        &decoded_path,
        &[(z1, second), (z0, first)],
    )
    .unwrap();
    assert_eq!(written, 1);
}

#[test]
fn build_svcache_tile_payloads_replace_writes_sparse_decoded_tiles() {
    let mut source = tempfile::Builder::new().suffix(".j2c").tempfile().unwrap();
    source
        .write_all(include_bytes!("../../../tests/fixtures/jp2k/rgb_nomct.j2k"))
        .unwrap();
    source.flush().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let out_path = out_dir.path().join("decoded-tiles.svcache");
    let selection =
        SvcacheTileSelection::new(SceneId::new(0), SeriesId::new(0), LevelIdx::new(0), 0, 0);
    let tile =
        CpuTile::from_u8_interleaved(1, 1, 4, ColorSpace::Rgba, vec![11_u8, 22_u8, 33_u8, 44_u8])
            .unwrap();

    let written =
        build_svcache_tile_payloads_replace(source.path(), &out_path, &[(selection, tile)])
            .unwrap();

    assert_eq!(written, 1);
    let (_, _, metadata) = read_svcache(&out_path).unwrap();
    let level = &metadata.scenes[0].series[0].levels[0];
    assert!(level.tiles.is_empty());
    assert_eq!(level.sparse_tiles.len(), 1);

    let backend = SvcacheBackend::new();
    let reader = backend.open(&out_path).unwrap();
    let decoded = reader
        .read_tile_cpu(&TileRequest {
            scene: 0usize.into(),
            series: 0usize.into(),
            level: 0u32.into(),
            plane: PlaneSelection::default().into(),
            col: 0,
            row: 0,
        })
        .unwrap();
    assert_eq!(decoded.data.as_u8().unwrap(), &[11, 22, 33, 44]);
}
