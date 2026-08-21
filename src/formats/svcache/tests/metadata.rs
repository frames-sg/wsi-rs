use super::*;

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
fn svcache_policy_parsing_and_legacy_metadata_default_are_stable() {
    assert_eq!(SvcachePolicy::from_env_value(None), SvcachePolicy::Off);
    assert_eq!(
        SvcachePolicy::from_env_value(Some(" TRUE ")),
        SvcachePolicy::PreferFresh
    );
    assert_eq!(
        SvcachePolicy::from_env_value(Some("required")),
        SvcachePolicy::RequireFresh
    );
    assert_eq!(
        SvcachePolicy::from_env_value(Some("unexpected")),
        SvcachePolicy::Off
    );

    let source = tempfile::NamedTempFile::new().unwrap();
    let metadata = SvcacheMetadata {
        schema_version: SCHEMA_VERSION,
        complete: true,
        source: fingerprint_source(source.path()).unwrap(),
        properties: Vec::new(),
        scenes: Vec::new(),
        associated: Vec::new(),
    };
    let mut value = serde_json::to_value(metadata).unwrap();
    value.as_object_mut().unwrap().remove("complete");
    let legacy: SvcacheMetadata = serde_json::from_value(value).unwrap();
    assert!(legacy.complete);
}
