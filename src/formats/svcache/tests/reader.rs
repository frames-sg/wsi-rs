use super::*;

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
fn svcache_reader_covers_batch_associated_bounds_and_payload_failures() {
    let mut payload = tempfile::tempfile().unwrap();
    let tile = CpuTile::from_u8_interleaved(1, 1, 3, ColorSpace::Rgb, vec![10, 20, 30]).unwrap();
    let tile_meta = write_tile_payload(&mut payload, &tile).unwrap();
    let associated_meta = write_tile_payload(&mut payload, &tile).unwrap();
    let source = tempfile::NamedTempFile::new().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let out_path = out_dir.path().join("reader.svcache");
    let mut metadata =
        single_level_svcache_metadata(source.path(), true, 1, 1, vec![Some(tile_meta)]);
    metadata.associated.push(AssociatedMeta {
        name: "label".into(),
        dimensions: (1, 1),
        tile: associated_meta,
    });
    write_svcache_file(&out_path, &metadata, payload).unwrap();

    let reader = SvcacheBackend.open(&out_path).unwrap();
    assert_eq!(reader.dataset().scenes.len(), 1);
    let request = TileRequest {
        scene: 0usize.into(),
        series: 0usize.into(),
        level: 0u32.into(),
        plane: PlaneSelection::default().into(),
        col: 0,
        row: 0,
    };
    let batch = reader
        .read_tiles(std::slice::from_ref(&request), TileOutputPreference::cpu())
        .unwrap();
    assert!(matches!(&batch[..], [TilePixels::Cpu(_)]));
    assert_eq!(
        reader
            .read_associated("label")
            .unwrap()
            .data
            .as_u8()
            .unwrap(),
        &[10, 20, 30]
    );
    assert!(matches!(
        reader.read_associated("missing"),
        Err(WsiError::AssociatedImageNotFound(_))
    ));
    assert!(matches!(
        reader.read_tiles(
            std::slice::from_ref(&request),
            TileOutputPreference::require_device_auto()
        ),
        Err(WsiError::Unsupported { .. })
    ));

    for invalid in [
        TileRequest {
            scene: 1usize.into(),
            ..request.clone()
        },
        TileRequest {
            col: -1,
            ..request.clone()
        },
        TileRequest {
            col: 1,
            ..request.clone()
        },
    ] {
        assert!(reader.read_tile_cpu(&invalid).is_err());
    }

    let corrupt_path = out_dir.path().join("checksum.svcache");
    let mut corrupt_payload = tempfile::tempfile().unwrap();
    let mut corrupt_meta = write_tile_payload(&mut corrupt_payload, &tile).unwrap();
    corrupt_meta.sha256 = "0".repeat(64);
    let corrupt_metadata =
        single_level_svcache_metadata(source.path(), true, 1, 1, vec![Some(corrupt_meta)]);
    write_svcache_file(&corrupt_path, &corrupt_metadata, corrupt_payload).unwrap();
    let corrupt_reader = SvcacheBackend.open(&corrupt_path).unwrap();
    assert!(corrupt_reader
        .read_tile_cpu(&request)
        .unwrap_err()
        .to_string()
        .contains("checksum mismatch"));

    let codec_path = out_dir.path().join("codec.svcache");
    let mut codec_payload = tempfile::tempfile().unwrap();
    codec_payload.write_all(&[0]).unwrap();
    let codec_meta = TileMeta {
        payload_offset: 0,
        payload_len: 1,
        decoded_len: 3,
        width: 1,
        height: 1,
        channels: 3,
        color_space: ColorSpaceMeta::Rgb,
        codec: PayloadCodec::Zstd,
        sha256: super::super::storage::hex_encode(&Sha256::digest([0])),
    };
    let codec_metadata =
        single_level_svcache_metadata(source.path(), true, 1, 1, vec![Some(codec_meta)]);
    write_svcache_file(&codec_path, &codec_metadata, codec_payload).unwrap();
    let codec_reader = SvcacheBackend.open(&codec_path).unwrap();
    assert!(matches!(
        codec_reader.read_tile_cpu(&request),
        Err(WsiError::Codec {
            codec: "svcache-zstd",
            ..
        })
    ));
}

#[test]
fn reader_recovers_from_a_poisoned_file_lock() {
    let mut payload = tempfile::tempfile().unwrap();
    let tile = CpuTile::from_u8_interleaved(1, 1, 3, ColorSpace::Rgb, vec![7, 8, 9]).unwrap();
    let tile_meta = write_tile_payload(&mut payload, &tile).unwrap();
    let source = tempfile::NamedTempFile::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("poison.svcache");
    let metadata = single_level_svcache_metadata(source.path(), true, 1, 1, vec![Some(tile_meta)]);
    write_svcache_file(&path, &metadata, payload).unwrap();

    let (file, payload_start, metadata) = read_svcache(&path).unwrap();
    let reader = std::sync::Arc::new(SvcacheReader {
        file: Mutex::new(file),
        payload_start,
        dataset: super::super::storage::dataset_from_metadata(&path, &metadata),
        metadata,
        associated_index: HashMap::new(),
    });
    let poisoner = std::sync::Arc::clone(&reader);
    assert!(std::thread::spawn(move || {
        let _guard = poisoner.file.lock().unwrap();
        panic!("poison the test mutex");
    })
    .join()
    .is_err());

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
    assert_eq!(decoded.data.as_u8().unwrap(), &[7, 8, 9]);
}
