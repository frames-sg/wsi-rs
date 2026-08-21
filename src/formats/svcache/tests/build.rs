use super::*;

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
    let source = raw_jp2k_source();
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
    let source = raw_jp2k_source();

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
    let source = raw_jp2k_source();
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

#[test]
fn svcache_complete_and_partial_builders_cover_merge_and_invalid_inputs() {
    let source = raw_jp2k_source();
    let out_dir = tempfile::tempdir().unwrap();
    let complete_path = out_dir.path().join("complete.svcache");
    build_svcache(source.path(), &complete_path).unwrap();
    let (_, _, complete) = read_svcache(&complete_path).unwrap();
    assert!(complete.complete);
    assert_eq!(complete.scenes.len(), 1);

    let selection =
        SvcacheTileSelection::new(SceneId::new(0), SeriesId::new(0), LevelIdx::new(0), 0, 0);
    let first = CpuTile::from_u8_interleaved(1, 1, 3, ColorSpace::Rgb, vec![1, 2, 3]).unwrap();
    let second = CpuTile::from_u8_interleaved(1, 1, 3, ColorSpace::Rgb, vec![4, 5, 6]).unwrap();
    let merge_path = out_dir.path().join("merge-provided.svcache");
    assert_eq!(
        build_svcache_tile_payloads_merge(source.path(), &merge_path, &[(selection, first)])
            .unwrap(),
        1
    );
    assert_eq!(
        build_svcache_tile_payloads_merge(source.path(), &merge_path, &[(selection, second)])
            .unwrap(),
        0
    );

    for (name, invalid) in [
        (
            "negative",
            SvcacheTileSelection::new(SceneId::new(0), SeriesId::new(0), LevelIdx::new(0), -1, 0),
        ),
        (
            "coordinate",
            SvcacheTileSelection::new(SceneId::new(0), SeriesId::new(0), LevelIdx::new(0), 1, 0),
        ),
        (
            "level",
            SvcacheTileSelection::new(SceneId::new(0), SeriesId::new(0), LevelIdx::new(1), 0, 0),
        ),
    ] {
        let path = out_dir.path().join(format!("invalid-{name}.svcache"));
        assert!(build_svcache_tiles(source.path(), &path, &[invalid]).is_err());
    }

    let u16_tile = CpuTile::new(
        1,
        1,
        1,
        ColorSpace::Grayscale,
        CpuTileLayout::Interleaved,
        CpuTileData::u16(vec![1]),
    )
    .unwrap();
    assert!(matches!(
        build_svcache_tile_payloads_replace(
            source.path(),
            &out_dir.path().join("u16.svcache"),
            &[(selection, u16_tile)]
        ),
        Err(WsiError::UnsupportedFormat(_))
    ));

    let unsupported_color =
        CpuTile::from_u8_interleaved(1, 1, 3, ColorSpace::YCbCr, vec![1, 2, 3]).unwrap();
    assert!(matches!(
        build_svcache_tile_payloads_replace(
            source.path(),
            &out_dir.path().join("ycbcr.svcache"),
            &[(selection, unsupported_color)]
        ),
        Err(WsiError::UnsupportedFormat(_))
    ));
}
