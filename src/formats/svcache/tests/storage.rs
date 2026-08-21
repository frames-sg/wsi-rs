use super::*;

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
fn storage_rejects_header_schema_and_arithmetic_boundaries() {
    let dir = tempfile::tempdir().unwrap();
    let oversized = dir.path().join("oversized.svcache");
    let mut bytes = MAGIC.to_vec();
    bytes.extend_from_slice(&u64::MAX.to_le_bytes());
    std::fs::write(&oversized, bytes).unwrap();
    assert!(read_svcache(&oversized)
        .unwrap_err()
        .to_string()
        .contains("metadata is too large"));

    let source = tempfile::NamedTempFile::new().unwrap();
    let schema = dir.path().join("schema.svcache");
    let mut metadata = SvcacheMetadata {
        schema_version: SCHEMA_VERSION - 1,
        complete: true,
        source: fingerprint_source(source.path()).unwrap(),
        properties: Vec::new(),
        scenes: Vec::new(),
        associated: Vec::new(),
    };
    write_svcache_file(&schema, &metadata, tempfile::tempfile().unwrap()).unwrap();
    assert!(read_svcache(&schema)
        .unwrap_err()
        .to_string()
        .contains("unsupported svcache schema"));

    let tile_count = dir.path().join("tile-count.svcache");
    metadata.schema_version = SCHEMA_VERSION;
    metadata.complete = false;
    metadata.scenes = single_level_svcache_metadata(source.path(), false, 1, 1, Vec::new()).scenes;
    let level = &mut metadata.scenes[0].series[0].levels[0];
    level.dimensions = (u64::MAX, 2);
    level.tile_width = 1;
    level.tile_height = 1;
    level.tiles_across = u64::MAX;
    level.tiles_down = 2;
    write_svcache_file(&tile_count, &metadata, tempfile::tempfile().unwrap()).unwrap();
    assert!(read_svcache(&tile_count)
        .unwrap_err()
        .to_string()
        .contains("tile count overflow"));

    let decoded = dir.path().join("decoded-overflow.svcache");
    let overflowing_tile = TileMeta {
        payload_offset: 0,
        payload_len: 1,
        decoded_len: 1,
        width: u32::MAX,
        height: u32::MAX,
        channels: 4,
        color_space: ColorSpaceMeta::Rgba,
        codec: PayloadCodec::Zstd,
        sha256: "0".repeat(64),
    };
    metadata.scenes =
        single_level_svcache_metadata(source.path(), true, 1, 1, vec![Some(overflowing_tile)])
            .scenes;
    {
        let level = &mut metadata.scenes[0].series[0].levels[0];
        level.dimensions = (u64::from(u32::MAX), u64::from(u32::MAX));
        level.tile_width = u32::MAX;
        level.tile_height = u32::MAX;
    }
    let mut payload = tempfile::tempfile().unwrap();
    payload.write_all(&[0]).unwrap();
    write_svcache_file(&decoded, &metadata, payload).unwrap();
    assert!(read_svcache(&decoded)
        .unwrap_err()
        .to_string()
        .contains("decoded tile length overflow"));

    assert!(fingerprint_source(dir.path())
        .unwrap_err()
        .to_string()
        .contains("regular file"));
    assert!(matches!(
        fingerprint_source(&dir.path().join("missing")),
        Err(WsiError::IoWithPath { .. })
    ));
}
