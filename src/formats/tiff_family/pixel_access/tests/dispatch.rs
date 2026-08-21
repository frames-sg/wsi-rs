use super::*;

#[test]
fn read_tiles_classifies_distinct_jpeg_tiled_ifd_requests_as_batchable() {
    let tiles = [
        encode_solid_rgb_jpeg(8, 8, [200, 10, 10]),
        encode_solid_rgb_jpeg(8, 8, [10, 200, 10]),
        encode_solid_rgb_jpeg(8, 8, [10, 10, 200]),
        encode_solid_rgb_jpeg(8, 8, [220, 220, 20]),
    ];
    let reader = build_tiled_jpeg_reader(16, 16, 8, 8, &tiles);
    let reqs = [
        TileRequest {
            scene: 0usize.into(),
            series: 0usize.into(),
            level: 0u32.into(),
            plane: PlaneSelection::default().into(),
            col: 0,
            row: 0,
        },
        TileRequest {
            scene: 0usize.into(),
            series: 0usize.into(),
            level: 0u32.into(),
            plane: PlaneSelection::default().into(),
            col: 1,
            row: 0,
        },
        TileRequest {
            scene: 0usize.into(),
            series: 0usize.into(),
            level: 0u32.into(),
            plane: PlaneSelection::default().into(),
            col: 0,
            row: 1,
        },
        TileRequest {
            scene: 0usize.into(),
            series: 0usize.into(),
            level: 0u32.into(),
            plane: PlaneSelection::default().into(),
            col: 1,
            row: 1,
        },
    ];

    assert_eq!(
        reader.tiled_ifd_batch_compression(&reqs).unwrap(),
        Some(Compression::Jpeg)
    );

    let batched = reader.read_tiles_cpu(&reqs).unwrap();
    let controlled = reader
        .read_tiles_controlled(
            &reqs,
            TileOutputPreference::cpu(),
            &crate::ReadControl::default(),
        )
        .expect("controlled TIFF batch");
    let sequential = reqs
        .iter()
        .map(|req| reader.read_tile_cpu(req))
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(batched.len(), sequential.len());
    assert_eq!(controlled.len(), reqs.len());
    for ((batched, controlled), sequential) in
        batched.iter().zip(controlled.iter()).zip(sequential.iter())
    {
        assert_eq!((batched.width, batched.height), (8, 8));
        assert_eq!(batched.data.as_u8(), sequential.data.as_u8());
        #[allow(unreachable_patterns)]
        match controlled {
            TilePixels::Cpu(controlled) => {
                assert_eq!(controlled.data.as_u8(), sequential.data.as_u8());
            }
            TilePixels::Device(_) => panic!("CPU preference returned a device tile"),
        }
    }
}

#[test]
fn read_tiles_single_jpeg_request_matches_direct_tile_read() {
    let tiles = [encode_solid_rgb_jpeg(8, 8, [200, 10, 10])];
    let reader = build_tiled_jpeg_reader(8, 8, 8, 8, &tiles);
    let req = TileRequest {
        scene: 0usize.into(),
        series: 0usize.into(),
        level: 0u32.into(),
        plane: PlaneSelection::default().into(),
        col: 0,
        row: 0,
    };

    let batched = reader.read_tiles_cpu(std::slice::from_ref(&req)).unwrap();
    let direct = reader.read_tile_cpu(&req).unwrap();

    assert_eq!(batched.len(), 1);
    assert_eq!((batched[0].width, batched[0].height), (8, 8));
    assert_eq!(batched[0].data.as_u8(), direct.data.as_u8());
}

#[test]
fn dispatch_fallbacks_and_associated_metadata_errors_are_contextual() {
    let tiles = [encode_solid_rgb_jpeg(8, 8, [200, 10, 10])];
    let mut reader = build_tiled_jpeg_reader(8, 8, 8, 8, &tiles);
    let ifd_id = reader.container.top_ifds()[0];
    let key = tile_source_key(0);
    let request = TileRequest::new(0usize, 0usize, 0u32, 0, 0);
    let view = TileViewRequest::new(0usize, 0usize, 0u32, 0, 0, 8, 8);

    assert!(reader.use_display_tile_cache(&view));
    reader.layout.tile_sources.remove(&key);
    let error = reader.tile_source_for(&request).unwrap_err();
    assert!(error.to_string().contains("no tile source"));
    assert!(reader.use_display_tile_cache(&view));

    reader.layout.tile_sources.insert(
        key.clone(),
        TileSource::Stripped {
            ifd_id,
            jpeg_tables: None,
            compression: Compression::Jpeg,
            strip_offsets: vec![],
            strip_byte_counts: vec![],
        },
    );
    let error = reader
        .read_tiles_cpu_with_backend(std::slice::from_ref(&request), BackendRequest::Auto)
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("Associated stripped images cannot be read via read_tile"));

    reader.layout.tile_sources.insert(
        key.clone(),
        TileSource::TiledIfd {
            ifd_id,
            jpeg_tables: None,
            compression: Compression::None,
        },
    );
    let decoded = reader
        .read_tiles_cpu_with_backend(std::slice::from_ref(&request), BackendRequest::Auto)
        .unwrap();
    assert_eq!((decoded[0].width, decoded[0].height), (8, 8));
    assert!(reader.read_raw_compressed_display_tile(&view).is_err());

    reader.layout.tile_sources.insert(
        key.clone(),
        TileSource::SyntheticDownsample {
            base_level: 0,
            factor: 2,
        },
    );
    assert!(!reader.use_display_tile_cache(&view));
    assert!(reader.read_raw_compressed_display_tile(&view).is_err());

    reader.layout.tile_sources.insert(
        key,
        TileSource::NdpiFullDecode {
            ifd_id,
            jpeg_header: vec![],
            strip_offset: 0,
            strip_byte_count: 0,
        },
    );
    assert!(!reader.use_display_tile_cache(&view));

    reader.layout.associated_sources.insert(
        "missing-stripped".into(),
        TileSource::Stripped {
            ifd_id,
            jpeg_tables: None,
            compression: Compression::None,
            strip_offsets: vec![],
            strip_byte_counts: vec![],
        },
    );
    assert!(reader
        .read_associated("missing-stripped")
        .unwrap_err()
        .to_string()
        .contains("missing-stripped"));

    reader.layout.associated_sources.insert(
        "missing-whole".into(),
        TileSource::NdpiFullDecode {
            ifd_id,
            jpeg_header: vec![],
            strip_offset: 0,
            strip_byte_count: 0,
        },
    );
    assert!(reader
        .read_associated("missing-whole")
        .unwrap_err()
        .to_string()
        .contains("missing-whole"));

    reader.layout.associated_sources.insert(
        "missing-tiled".into(),
        TileSource::TiledIfd {
            ifd_id,
            jpeg_tables: None,
            compression: Compression::Jpeg,
        },
    );
    assert!(reader
        .read_associated("missing-tiled")
        .unwrap_err()
        .to_string()
        .contains("missing-tiled"));

    reader.layout.associated_sources.insert(
        "missing-external".into(),
        TileSource::ExternalJpeg {
            path: reader.container.path().with_extension("missing.jpg"),
        },
    );
    assert!(reader
        .read_associated("missing-external")
        .unwrap_err()
        .to_string()
        .contains("failed to read external JPEG"));
}

#[test]
fn malformed_jp2k_batch_preserves_tile_request_context() {
    let reader = build_tiled_encoded_reader(
        8,
        8,
        8,
        8,
        &[vec![1, 2, 3, 4]],
        Compression::Jp2kRgb,
        33004,
        3,
        2,
    );
    let request = TileRequest::new(0usize, 0usize, 0u32, 0, 0);
    let error = reader
        .read_tiles_cpu_with_backend(&[request], BackendRequest::Auto)
        .unwrap_err();
    assert!(matches!(error, WsiError::TileRead { col: 0, row: 0, .. }));
}

#[test]
fn tiled_ifd_raw_passthrough_reports_layout_and_table_errors() {
    let jpeg_tiles = [encode_solid_rgb_jpeg(8, 8, [200, 10, 10])];
    let mut jpeg_reader = build_tiled_jpeg_reader(8, 8, 8, 8, &jpeg_tiles);
    let ifd_id = *jpeg_reader.container.top_ifds().first().unwrap();

    let err = jpeg_reader
        .read_tiled_ifd_raw_jpeg_tile(&TileRequest::new(0usize, 0usize, 0u32, 1, 0), ifd_id, None)
        .unwrap_err();
    assert!(err.to_string().contains("tile (1,0) out of range"));

    jpeg_reader.layout.dataset.scenes[0].series[0].levels[0].tile_layout = TileLayout::WholeLevel {
        width: 8,
        height: 8,
        virtual_tile_width: 8,
        virtual_tile_height: 8,
    };
    let err = jpeg_reader
        .read_tiled_ifd_raw_jpeg_tile(&TileRequest::new(0usize, 0usize, 0u32, 0, 0), ifd_id, None)
        .unwrap_err();
    assert!(err.to_string().contains("does not use WholeLevel layout"));

    let mut short_jpeg_reader = build_tiled_jpeg_reader(8, 8, 8, 8, &jpeg_tiles);
    let ifd_id = *short_jpeg_reader.container.top_ifds().first().unwrap();
    short_jpeg_reader.layout.dataset.scenes[0].series[0].levels[0].dimensions = (16, 8);
    short_jpeg_reader.layout.dataset.scenes[0].series[0].levels[0].tile_layout =
        TileLayout::Regular {
            tile_width: 8,
            tile_height: 8,
            tiles_across: 2,
            tiles_down: 1,
        };
    let err = short_jpeg_reader
        .read_tiled_ifd_raw_jpeg_tile(&TileRequest::new(0usize, 0usize, 0u32, 1, 0), ifd_id, None)
        .unwrap_err();
    assert!(err.to_string().contains("tile index 1 out of range"));

    let empty_jpeg_reader = build_tiled_jpeg_reader(8, 8, 8, 8, &[Vec::new()]);
    let ifd_id = *empty_jpeg_reader.container.top_ifds().first().unwrap();
    let err = empty_jpeg_reader
        .read_tiled_ifd_raw_jpeg_tile(&TileRequest::new(0usize, 0usize, 0u32, 0, 0), ifd_id, None)
        .unwrap_err();
    assert!(err.to_string().contains("empty TIFF tiles"));

    let codestream = include_bytes!("../../../../../tests/fixtures/jp2k/rgb_nomct.j2k").to_vec();
    let mut short_jp2k_reader =
        build_tiled_encoded_reader(8, 8, 8, 8, &[codestream], Compression::Jp2kRgb, 33004, 3, 2);
    let ifd_id = *short_jp2k_reader.container.top_ifds().first().unwrap();
    short_jp2k_reader.layout.dataset.scenes[0].series[0].levels[0].dimensions = (16, 8);
    short_jp2k_reader.layout.dataset.scenes[0].series[0].levels[0].tile_layout =
        TileLayout::Regular {
            tile_width: 8,
            tile_height: 8,
            tiles_across: 2,
            tiles_down: 1,
        };
    let err = short_jp2k_reader
        .read_tiled_ifd_raw_jp2k_tile(
            &TileRequest::new(0usize, 0usize, 0u32, 1, 0),
            ifd_id,
            Compression::Jp2kRgb,
        )
        .unwrap_err();
    assert!(err.to_string().contains("tile index 1 out of range"));

    let empty_jp2k_reader =
        build_tiled_encoded_reader(8, 8, 8, 8, &[Vec::new()], Compression::Jp2kRgb, 33004, 3, 2);
    let ifd_id = *empty_jp2k_reader.container.top_ifds().first().unwrap();
    let err = empty_jp2k_reader
        .read_tiled_ifd_raw_jp2k_tile(
            &TileRequest::new(0usize, 0usize, 0u32, 0, 0),
            ifd_id,
            Compression::Jp2kRgb,
        )
        .unwrap_err();
    assert!(err.to_string().contains("empty TIFF tiles"));

    let err = empty_jp2k_reader
        .decode_tiled_ifd_jpeg_batch(
            &[TileRequest::new(0usize, 0usize, 0u32, 0, 0)],
            BackendRequest::Auto,
        )
        .unwrap_err();
    assert!(err.to_string().contains("non-JPEG tile source"));
}

#[test]
fn tiled_ifd_irregular_layout_uses_tiff_grid_metadata_for_missing_tile_index() {
    let jpeg_tiles = [encode_solid_rgb_jpeg(8, 8, [200, 10, 10])];
    let mut reader = build_tiled_jpeg_reader(8, 8, 8, 8, &jpeg_tiles);
    let ifd_id = *reader.container.top_ifds().first().unwrap();
    reader.layout.dataset.scenes[0].series[0].levels[0].tile_layout = TileLayout::Irregular {
        tile_advance: (8.0, 8.0),
        extra_tiles: (0, 0, 0, 0),
        tiles: HashMap::from([
            ((0, 0), TileEntry::new((0.0, 0.0), (8, 8))),
            ((-1, 0), TileEntry::new((-8.0, 0.0), (8, 8))),
        ]),
    };

    let raw = reader
        .read_tiled_ifd_raw_jpeg_tile(&TileRequest::new(0usize, 0usize, 0u32, 0, 0), ifd_id, None)
        .expect("irregular tile should resolve through TIFF grid metadata");
    assert_eq!((raw.width(), raw.height()), (8, 8));

    let err = reader
        .read_tiled_ifd_raw_jpeg_tile(&TileRequest::new(0usize, 0usize, 0u32, 1, 0), ifd_id, None)
        .unwrap_err();
    assert!(err.to_string().contains("no irregular tile at (1,0)"));

    let err = reader
        .read_tiled_ifd_raw_jpeg_tile(&TileRequest::new(0usize, 0usize, 0u32, -1, 0), ifd_id, None)
        .unwrap_err();
    assert!(err
        .to_string()
        .contains("irregular tile row/col out of range for TIFF tile grid"));
}
