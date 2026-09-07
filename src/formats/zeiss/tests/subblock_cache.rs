use super::fixtures::*;
use super::*;

#[test]
fn czi_large_source_groups_fit_decoded_staging() {
    let blocks: Vec<_> = [0, 1024]
        .into_iter()
        .map(|x| {
            let mut block = SubblockSpec::bgr24(
                x,
                0,
                1024,
                1024,
                jpeg_rgb(1024, 1024, &vec![100; 1024 * 1024 * 3]),
            );
            block.compression = 1;
            block
        })
        .collect();
    let fixture = write_fixture(&blocks, &[], &metadata_xml(2048, 1024));
    let reader = ZeissReader {
        slide: Arc::new(
            ZeissSlide::parse_with_cache_config(
                fixture.path(),
                CacheConfig::default().with_shared_tile_bytes(0),
            )
            .unwrap(),
        ),
    };
    let reqs: Vec<_> = [0, 4, 0, 4]
        .into_iter()
        .map(|col| TileRequest::new(0, 0, 0, col, 0))
        .collect();
    let actual = reader.read_tiles_cpu(&reqs).unwrap();
    assert_eq!(actual.len(), 4);
    assert!(actual
        .iter()
        .all(|tile| (tile.width(), tile.height()) == (256, 256)));
    assert_eq!(
        reader
            .slide
            .prepared_source_peak_bytes
            .load(Ordering::Relaxed),
        1024 * 1024 * 3,
        "the logical batch is smaller than a source block, so stage only one source at a time"
    );
}

#[test]
fn concurrent_czi_batches_share_one_source_miss() {
    let fixture = jpeg_fixture();
    let mut slide = ZeissSlide::parse(fixture.path()).unwrap();
    if crate::core::decode_runtime::DecodeRuntime::default_arc().cpu_worker_count() > 1 {
        slide.source_miss_barrier = Some(Arc::new(std::sync::Barrier::new(2)));
    }
    let reader = ZeissReader {
        slide: Arc::new(slide),
    };
    let reqs = [
        TileRequest::new(0, 0, 0, 0, 0),
        TileRequest::new(0, 0, 0, 1, 0),
    ];
    std::thread::scope(|scope| {
        let a = scope.spawn(|| reader.read_tiles_cpu(&reqs).unwrap());
        let b = scope.spawn(|| reader.read_tiles_cpu(&reqs).unwrap());
        for (a, b) in a.join().unwrap().into_iter().zip(b.join().unwrap()) {
            assert_eq!(a.as_u8(), b.as_u8());
        }
    });
    assert_eq!(reader.slide.subblock_decodes.load(Ordering::Relaxed), 1);
}

#[test]
fn concurrent_czi_neighbors_share_one_source_miss() {
    let fixture = jpeg_fixture();
    let reference = ZeissReader {
        slide: Arc::new(ZeissSlide::parse(fixture.path()).unwrap()),
    };
    let reqs = [
        TileRequest::new(0, 0, 0, 0, 0),
        TileRequest::new(0, 0, 0, 1, 0),
    ];
    let expected: Vec<_> = reqs
        .iter()
        .map(|req| reference.read_tile_cpu(req).unwrap())
        .collect();
    let mut slide = ZeissSlide::parse(fixture.path()).unwrap();
    slide.source_miss_barrier = Some(Arc::new(std::sync::Barrier::new(2)));
    let reader = ZeissReader {
        slide: Arc::new(slide),
    };
    std::thread::scope(|scope| {
        let handles: Vec<_> = reqs
            .iter()
            .map(|req| scope.spawn(|| reader.read_tile_cpu(req).unwrap()))
            .collect();
        for (handle, expected) in handles.into_iter().zip(&expected) {
            assert_eq!(handle.join().unwrap().as_u8(), expected.as_u8());
        }
    });
    assert_eq!(reader.slide.subblock_decodes.load(Ordering::Relaxed), 1);
}

#[test]
fn czi_batch_decodes_each_source_once_without_persistent_cache() {
    let fixture = jpeg_fixture();
    let reader = ZeissReader {
        slide: Arc::new(
            ZeissSlide::parse_with_cache_config(
                fixture.path(),
                CacheConfig::default().with_shared_tile_bytes(0),
            )
            .unwrap(),
        ),
    };
    let reqs: Vec<_> = [1, 0, 1]
        .into_iter()
        .map(|col| TileRequest::new(0, 0, 0, col, 0))
        .collect();
    let expected: Vec<_> = reqs
        .iter()
        .map(|req| reader.read_tile_cpu(req).unwrap())
        .collect();
    let before = reader.slide.subblock_decodes.load(Ordering::Relaxed);
    let actual = reader.read_tiles_cpu(&reqs).unwrap();
    assert_eq!(
        reader.slide.subblock_decodes.load(Ordering::Relaxed) - before,
        1
    );
    assert_eq!(
        reader.slide.subblock_cache.lock().unwrap().current_bytes(),
        0
    );
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.iter().zip(&expected) {
        assert_eq!(actual.as_u8(), expected.as_u8());
    }
}

fn jpeg_fixture() -> tempfile::NamedTempFile {
    let rgb: Vec<_> = (0..512 * 8)
        .flat_map(|i| [(i % 251) as u8, 70, 150])
        .collect();
    let mut block = SubblockSpec::bgr24(0, 0, 512, 8, jpeg_rgb(512, 8, &rgb));
    block.compression = 1;
    write_fixture(&[block], &[], &metadata_xml(512, 8))
}

#[test]
#[ignore = "exports a synthetic corpus using existing CZI fixture facilities for native performance comparisons"]
fn export_czi_batch_performance_fixture() {
    let output = std::env::var_os("WSI_RS_CZI_PERF_FIXTURE").expect("fixture output path");
    let blocks: Vec<_> = (0..4)
        .map(|index| {
            let rgb: Vec<_> = (0..1024 * 1024)
                .flat_map(|pixel| {
                    let x = pixel % 1024;
                    let y = pixel / 1024;
                    [
                        ((x / 7 + y / 11 + index * 41) % 251) as u8,
                        ((x / 13 + y / 3 + 63) % 251) as u8,
                        ((x / 5 + y / 17 + 113) % 251) as u8,
                    ]
                })
                .collect();
            let mut block = SubblockSpec::bgr24(
                index % 2 * 1024,
                index / 2 * 1024,
                1024,
                1024,
                jpeg_rgb(1024, 1024, &rgb),
            );
            block.compression = 1;
            block
        })
        .collect();
    let fixture = write_fixture(&blocks, &[], &metadata_xml(2048, 2048));
    let reader = ZeissReader {
        slide: Arc::new(ZeissSlide::parse(fixture.path()).unwrap()),
    };
    let reqs: Vec<_> = (0..8)
        .flat_map(|row| (0..8).map(move |col| TileRequest::new(0, 0, 0, col, row)))
        .collect();
    let actual = reader.read_tiles_cpu(&reqs).unwrap();
    assert_eq!(actual.len(), 64);
    assert!(actual
        .iter()
        .all(|tile| (tile.width(), tile.height()) == (256, 256)));
    std::fs::copy(fixture.path(), output).unwrap();
}

#[test]
fn neighboring_tiles_decode_a_shared_compressed_subblock_once() {
    let fixture = jpeg_fixture();
    let slide = ZeissSlide::parse(fixture.path()).unwrap();
    let reference = ZeissSlide::parse_with_cache_config(
        fixture.path(),
        CacheConfig::default().with_shared_tile_bytes(0),
    )
    .unwrap();
    for col in [0, 1] {
        let actual = slide
            .read_tile(0, 0, 0, col, 0, BackendRequest::Cpu)
            .unwrap();
        let expected = reference
            .read_tile(0, 0, 0, col, 0, BackendRequest::Cpu)
            .unwrap();
        assert_eq!(actual.data.as_u8(), expected.data.as_u8());
        assert_eq!((actual.width, actual.height), (256, 8));
    }
    assert_eq!(reference.subblock_decodes.load(Ordering::Relaxed), 2);
    assert_eq!(slide.subblock_decodes.load(Ordering::Relaxed), 1);
}

#[test]
fn disabled_and_undersized_caches_keep_decoding_without_retention() {
    let fixture = jpeg_fixture();
    for budget in [0, 1024] {
        let slide = ZeissSlide::parse_with_cache_config(
            fixture.path(),
            CacheConfig::default().with_shared_tile_bytes(budget),
        )
        .unwrap();
        for col in [0, 1, 0] {
            let tile = slide
                .read_tile(0, 0, 0, col, 0, BackendRequest::Cpu)
                .unwrap();
            assert_eq!((tile.width, tile.height), (256, 8));
        }
        assert_eq!(slide.subblock_decodes.load(Ordering::Relaxed), 3);
        assert_eq!(slide.subblock_cache.lock().unwrap().current_bytes(), 0);
    }
}

#[test]
fn subblock_eviction_respects_the_aggregate_private_budget() {
    let blocks: Vec<_> = [0, 512]
        .into_iter()
        .map(|x| {
            let mut block =
                SubblockSpec::bgr24(x, 0, 512, 8, jpeg_rgb(512, 8, &vec![120; 512 * 8 * 3]));
            block.compression = 1;
            block
        })
        .collect();
    let fixture = write_fixture(&blocks, &[], &metadata_xml(1024, 8));
    // Half the private budget fits one decoded 12,288-byte source block.
    let config = CacheConfig::default().with_shared_tile_bytes(52_224);
    let slide = ZeissSlide::parse_with_cache_config(fixture.path(), config).unwrap();
    for col in [0, 2, 1] {
        slide
            .read_tile(0, 0, 0, col, 0, BackendRequest::Cpu)
            .unwrap();
        let cache = slide.subblock_cache.lock().unwrap();
        assert_eq!(cache.len(), 1);
        assert!(cache.current_bytes() <= cache.capacity_bytes());
    }
    assert_eq!(slide.subblock_decodes.load(Ordering::Relaxed), 3);
    let capacity = slide.subblock_cache.lock().unwrap().capacity_bytes()
        + slide.tile_cache.lock().unwrap().capacity_bytes()
        + slide.level_cache.lock().unwrap().capacity_bytes()
        + slide.associated_cache.lock().unwrap().capacity_bytes();
    assert_eq!(capacity, config.private_cache_budget_bytes());
}

#[test]
fn default_subblock_budget_fits_a_full_resolution_zeiss_source_block() {
    let fixture = jpeg_fixture();
    let slide = ZeissSlide::parse(fixture.path()).unwrap();
    let capacity = slide.subblock_cache.lock().unwrap().capacity_bytes();
    // Public Zeiss-5-JXR source blocks are 2056 x 2464 RGB pixels (~14.5 MiB).
    assert!(capacity >= 2056 * 2464 * 3 + 256);
    assert_eq!(
        capacity,
        CacheConfig::default().private_cache_budget_bytes() / 2
    );
}

#[test]
fn jpegxr_neighbors_and_mixed_overlap_match_reference_pixels() {
    let expected =
        image::load_from_memory(include_bytes!("../../../../tests/fixtures/jxr/rgb.ppm"))
            .unwrap()
            .into_rgb8();
    let mut xr = SubblockSpec::bgr24(
        250,
        0,
        16,
        16,
        include_bytes!("../../../../tests/fixtures/jxr/rgb.jxr").to_vec(),
    );
    xr.compression = 4;
    xr.m_index = Some(1);
    let mut overlay = SubblockSpec::bgr24(255, 2, 2, 3, [30, 20, 10].repeat(6));
    overlay.m_index = Some(2);
    let anchor = SubblockSpec::bgr24(0, 0, 1, 1, vec![3, 2, 1]);
    // Deliberately scramble file order; mosaic order must survive cache hits.
    let fixture = write_fixture(&[overlay, xr, anchor], &[], &metadata_xml(266, 16));
    let slide = ZeissSlide::parse(fixture.path()).unwrap();
    let mut reference = vec![0; 266 * 16 * 3];
    reference[..3].copy_from_slice(&[1, 2, 3]);
    for y in 0..16 {
        for x in 0..16 {
            let start = (y * 266 + 250 + x) * 3;
            reference[start..start + 3]
                .copy_from_slice(&expected.as_raw()[(y * 16 + x) * 3..(y * 16 + x + 1) * 3]);
        }
    }
    for y in 2..5 {
        for x in 255..257 {
            reference[(y * 266 + x) * 3..(y * 266 + x + 1) * 3].copy_from_slice(&[10, 20, 30]);
        }
    }
    let reader = ZeissReader {
        slide: Arc::new(slide),
    };
    let reqs: Vec<_> = [1, 0, 1]
        .into_iter()
        .map(|col| TileRequest::new(0usize, 0usize, 0, col, 0))
        .collect();
    let tiles = reader.read_tiles_cpu(&reqs).unwrap();
    for (req, tile) in reqs.iter().zip(tiles) {
        let x = req.col as usize * 256;
        let width = 256.min(266 - x);
        let expected_tile: Vec<_> = (0..16)
            .flat_map(|y| {
                reference[(y * 266 + x) * 3..(y * 266 + x + width) * 3]
                    .iter()
                    .copied()
            })
            .collect();
        assert_eq!((tile.width, tile.height), (width as u32, 16));
        assert_eq!(tile.data.as_u8(), Some(expected_tile.as_slice()));
    }
    assert_eq!(
        reader.slide.scene_level_image(0, 0).unwrap().data.as_u8(),
        Some(reference.as_slice())
    );
    assert_eq!(reader.slide.subblock_decodes.load(Ordering::Relaxed), 1);

    let public = crate::Slide::open(fixture.path()).unwrap();
    let region = RegionRequest::builder(0usize, 0usize, 0)
        .origin_px((252, 1))
        .size_px((12, 5))
        .build()
        .unwrap();
    let rgba = public.read_region_rgba(&region).unwrap();
    let expected_rgba: Vec<_> = (1..6)
        .flat_map(|y| {
            reference[(y * 266 + 252) * 3..(y * 266 + 264) * 3]
                .chunks_exact(3)
                .flat_map(|p| [p[0], p[1], p[2], 255])
        })
        .collect();
    assert_eq!(rgba.as_raw(), &expected_rgba);
}

#[cfg(unix)]
#[test]
fn cached_subblocks_do_not_hide_source_replacement() {
    let fixture = jpeg_fixture();
    let slide = ZeissSlide::parse(fixture.path()).unwrap();
    slide.read_tile(0, 0, 0, 0, 0, BackendRequest::Cpu).unwrap();
    let directory = tempfile::tempdir().unwrap();
    std::fs::rename(fixture.path(), directory.path().join("original.czi")).unwrap();
    std::fs::copy(directory.path().join("original.czi"), fixture.path()).unwrap();
    let error = slide
        .read_tile(0, 0, 0, 1, 0, BackendRequest::Cpu)
        .unwrap_err();
    assert!(error.to_string().contains("source identity"));
    assert_eq!(slide.subblock_decodes.load(Ordering::Relaxed), 1);
}
