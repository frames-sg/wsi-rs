use super::build::{
    cache_grid_for_level, copy_existing_svcache_tiles, copy_existing_svcache_tiles_with_policy,
    metadata_shell, write_svcache_file, write_tile_payload, ExistingTilePolicy,
};
use super::storage::{fingerprint_source, is_fresh_svcache, read_svcache};
use super::*;
use crate::core::types::CpuTile;
use std::fs::FileTimes;

fn single_level_svcache_metadata(
    source_path: &std::path::Path,
    complete: bool,
    tiles_across: u64,
    tiles_down: u64,
    tiles: Vec<Option<TileMeta>>,
) -> SvcacheMetadata {
    SvcacheMetadata {
        schema_version: SCHEMA_VERSION,
        complete,
        source: fingerprint_source(source_path).unwrap(),
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
                    dimensions: (tiles_across, tiles_down),
                    downsample: 1.0,
                    tile_width: 1,
                    tile_height: 1,
                    tiles_across,
                    tiles_down,
                    tiles,
                    sparse_tiles: Vec::new(),
                }],
            }],
        }],
        associated: Vec::new(),
    }
}

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
fn source_fingerprint_detects_same_size_same_mtime_replacement() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.j2c");
    std::fs::write(&source, b"first-content").unwrap();
    let original_modified = std::fs::metadata(&source).unwrap().modified().unwrap();
    let first = fingerprint_source(&source).unwrap();

    std::fs::write(&source, b"other-content").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&source)
        .unwrap()
        .set_times(FileTimes::new().set_modified(original_modified))
        .unwrap();
    let second = fingerprint_source(&source).unwrap();

    assert_eq!(first.len, second.len);
    assert_eq!(first.modified_unix_nanos, second.modified_unix_nanos);
    assert_ne!(first.sample_sha256, second.sample_sha256);
    assert_ne!(first, second);
}

#[test]
fn prefer_fresh_surfaces_corrupt_cache_instead_of_falling_back() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.j2c");
    std::fs::write(&source, b"source").unwrap();
    let cache = default_svcache_path(&source);
    std::fs::write(&cache, b"corrupt").unwrap();

    let err = resolve_open_path_with_policy(&source, SvcachePolicy::PreferFresh).unwrap_err();
    assert!(
        err.to_string().contains("I/O error") || err.to_string().contains("svcache"),
        "unexpected corrupt-cache error: {err}"
    );
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
    let metadata = single_level_svcache_metadata(source.path(), true, 1, 1, vec![Some(tile_meta)]);
    write_svcache_file(&out_path, &metadata, payload).unwrap();

    let backend = SvcacheBackend;
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
fn svcache_rejects_incoherent_tile_metadata_before_reading_payload() {
    fn assert_rejected(source: &std::path::Path, tile: TileMeta, expected: &str) {
        let mut payload = tempfile::tempfile().unwrap();
        payload.write_all(&[0]).unwrap();
        let out_dir = tempfile::tempdir().unwrap();
        let out_path = out_dir.path().join("invalid.svcache");
        let metadata = single_level_svcache_metadata(source, true, 1, 1, vec![Some(tile)]);
        write_svcache_file(&out_path, &metadata, payload).unwrap();

        let err = read_svcache(&out_path).unwrap_err();
        assert!(
            err.to_string().contains(expected),
            "expected '{expected}', got: {err}"
        );
    }

    let source = tempfile::NamedTempFile::new().unwrap();
    let base = TileMeta {
        payload_offset: 0,
        payload_len: 1,
        decoded_len: 3,
        width: 1,
        height: 1,
        channels: 3,
        color_space: ColorSpaceMeta::Rgb,
        codec: PayloadCodec::Zstd,
        sha256: "0".repeat(64),
    };

    let mut invalid = base.clone();
    invalid.decoded_len = usize::MAX;
    assert_rejected(source.path(), invalid, "decoded tile length");

    let mut invalid = base.clone();
    invalid.channels = 4;
    assert_rejected(source.path(), invalid, "channel count");

    let mut invalid = base.clone();
    invalid.payload_offset = u64::MAX;
    assert_rejected(source.path(), invalid, "payload range overflow");

    let mut invalid = base;
    invalid.sha256 = "not-a-checksum".into();
    assert_rejected(source.path(), invalid, "checksum");
}

#[test]
fn svcache_rejects_incoherent_container_metadata_before_payload_reads() {
    fn valid_tile() -> TileMeta {
        TileMeta {
            payload_offset: 0,
            payload_len: 1,
            decoded_len: 3,
            width: 1,
            height: 1,
            channels: 3,
            color_space: ColorSpaceMeta::Rgb,
            codec: PayloadCodec::Zstd,
            sha256: "0".repeat(64),
        }
    }

    fn assert_rejected(metadata: SvcacheMetadata, payload: &[u8], expected: &str) {
        let mut payload_file = tempfile::tempfile().unwrap();
        payload_file.write_all(payload).unwrap();
        let out_dir = tempfile::tempdir().unwrap();
        let out_path = out_dir.path().join("invalid-container.svcache");
        write_svcache_file(&out_path, &metadata, payload_file).unwrap();

        let error = read_svcache(&out_path).unwrap_err();
        assert!(
            error.to_string().contains(expected),
            "expected '{expected}', got: {error}"
        );
    }

    let source = tempfile::NamedTempFile::new().unwrap();
    let base =
        || single_level_svcache_metadata(source.path(), true, 1, 1, vec![Some(valid_tile())]);

    let mut invalid = base();
    invalid.properties = vec![
        ("duplicate".into(), "one".into()),
        ("duplicate".into(), "two".into()),
    ];
    assert_rejected(invalid, &[0], "duplicate svcache property");

    let mut invalid = base();
    invalid.scenes.push(invalid.scenes[0].clone());
    assert_rejected(invalid, &[0], "duplicate svcache scene id");

    let mut invalid = base();
    let duplicate_series = invalid.scenes[0].series[0].clone();
    invalid.scenes[0].series.push(duplicate_series);
    assert_rejected(invalid, &[0], "duplicate svcache series id");

    let mut invalid = base();
    invalid.scenes[0].series[0].axes.z = 0;
    assert_rejected(invalid, &[0], "axis extents must be positive");

    let mut invalid = base();
    invalid.scenes[0].series[0].levels[0].dimensions.0 = 0;
    assert_rejected(invalid, &[0], "level geometry is invalid");

    let mut invalid = base();
    invalid.scenes[0].series[0].levels[0].tiles_across = 2;
    assert_rejected(invalid, &[0], "tile grid does not match dimensions");

    let mut invalid = base();
    invalid.scenes[0].series[0].levels[0].sparse_tiles = vec![SparseTileMeta {
        index: 0,
        tile: valid_tile(),
    }];
    assert_rejected(invalid, &[0], "mixes dense and sparse");

    let mut invalid = base();
    invalid.scenes[0].series[0].levels[0].tiles.clear();
    assert_rejected(invalid, &[0], "does not contain every tile");

    let mut invalid = base();
    invalid.complete = false;
    invalid.scenes[0].series[0].levels[0].tiles = vec![Some(valid_tile()), None];
    assert_rejected(invalid, &[0], "dense tile index has incorrect length");

    let mut invalid = base();
    invalid.scenes[0].series[0].levels[0].tiles[0] = None;
    assert_rejected(invalid, &[0], "empty dense tile slot");

    let mut invalid = base();
    invalid.associated = vec![AssociatedMeta {
        name: "label".into(),
        dimensions: (2, 1),
        tile: valid_tile(),
    }];
    assert_rejected(invalid, &[0], "dimensions do not match its tile");

    let mut invalid = base();
    invalid.associated = vec![AssociatedMeta {
        name: "label".into(),
        dimensions: (1, 1),
        tile: valid_tile(),
    }];
    assert_rejected(invalid, &[0], "payload ranges overlap");

    let mut invalid = base();
    invalid.scenes[0].series[0].levels[0].tiles[0]
        .as_mut()
        .unwrap()
        .payload_len = 0;
    assert_rejected(invalid, &[0], "encoded tile length is invalid");

    let mut invalid = base();
    invalid.scenes[0].series[0].levels[0].tiles[0]
        .as_mut()
        .unwrap()
        .payload_offset = 1;
    assert_rejected(invalid, &[0], "payload extends past EOF");

    let mut invalid = base();
    invalid.scenes[0].series[0].levels[0].tiles[0]
        .as_mut()
        .unwrap()
        .width = 0;
    assert_rejected(invalid, &[0], "tile dimensions must be positive");
}

#[test]
fn svcache_rejects_duplicate_sparse_indexes() {
    let mut payload = tempfile::tempfile().unwrap();
    let tile =
        CpuTile::from_u8_interleaved(1, 1, 3, ColorSpace::Rgb, vec![10_u8, 20_u8, 30_u8]).unwrap();
    let tile_meta = write_tile_payload(&mut payload, &tile).unwrap();
    let source = tempfile::NamedTempFile::new().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let out_path = out_dir.path().join("duplicate-sparse.svcache");
    let mut metadata = single_level_svcache_metadata(source.path(), false, 2, 1, Vec::new());
    metadata.scenes[0].series[0].levels[0].sparse_tiles = vec![
        SparseTileMeta {
            index: 0,
            tile: tile_meta.clone(),
        },
        SparseTileMeta {
            index: 0,
            tile: tile_meta,
        },
    ];
    write_svcache_file(&out_path, &metadata, payload).unwrap();

    let err = read_svcache(&out_path).unwrap_err();
    assert!(err.to_string().contains("sparse tile indexes"));
}

#[test]
fn svcache_sparse_level_reports_missing_tile() {
    let payload = tempfile::tempfile().unwrap();
    let source = tempfile::NamedTempFile::new().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let out_path = out_dir.path().join("sparse.svcache");
    let metadata = single_level_svcache_metadata(source.path(), false, 2, 1, vec![None, None]);
    write_svcache_file(&out_path, &metadata, payload).unwrap();

    let backend = SvcacheBackend;
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
    let metadata =
        single_level_svcache_metadata(source.path(), false, 2, 1, vec![Some(existing_tile), None]);
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
    let metadata =
        single_level_svcache_metadata(source.path(), false, 2, 1, vec![Some(existing_tile), None]);
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

    let backend = SvcacheBackend;
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

    let backend = SvcacheBackend;
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
