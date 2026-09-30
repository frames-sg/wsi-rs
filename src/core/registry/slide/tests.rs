type RecordedTileBatches = Arc<std::sync::Mutex<Vec<Vec<(i64, i64)>>>>;
use super::*;
use crate::test_support::{regular_rgb_dataset_for_test, RegularLevelForTest};

struct TinySource {
    dataset: Dataset,
}

impl TinySource {
    fn new() -> Self {
        Self {
            dataset: regular_rgb_dataset_for_test(
                DatasetId::new(77),
                "scene",
                "series",
                RegularLevelForTest {
                    dimensions: (1, 1),
                    tile_width: 1,
                    tile_height: 1,
                    tiles_across: 1,
                    tiles_down: 1,
                },
            ),
        }
    }
}

impl SlideReader for TinySource {
    fn dataset(&self) -> &Dataset {
        &self.dataset
    }

    fn read_tile_cpu(&self, _req: &TileRequest) -> Result<CpuTile, WsiError> {
        CpuTile::from_u8_interleaved(1, 1, 3, ColorSpace::Rgb, vec![10, 20, 30])
    }
}

#[test]
fn slide_cache_poison_recovery_keeps_public_inspection_and_conversion_working() {
    let slide = Arc::new(Slide::from_source(
        Box::new(TinySource::new()),
        Arc::new(TileCache::new(4096)),
    ));
    assert!(format!("{slide:?}").contains("dataset_id"));
    assert_eq!(
        slide.decode_execution_options(),
        DecodeExecutionOptions::default()
    );

    let poisoned = Arc::clone(&slide);
    let _ = std::thread::spawn(move || {
        let _guard = poisoned.cache.write().unwrap();
        panic!("poison slide cache owner");
    })
    .join();

    let replacement = Arc::new(TileCache::new(8192));
    let detached = slide.replace_shared_tile_cache(Arc::clone(&replacement));
    assert!(!Arc::ptr_eq(&detached, &replacement));

    let tile = TileRequest::new(0usize, 0usize, 0u32, 0, 0);
    assert!(!slide.cached_tile_present(&tile));
    let view = TileViewRequest::new(0usize, 0usize, 0u32, 0, 0, 1, 1);
    assert!(matches!(
        slide.read_raw_compressed_display_tile(&view),
        Err(WsiError::Unsupported { .. })
    ));

    let region = RegionRequest::new(0usize, 0usize, 0u32, (0, 0), (1, 1));
    let rgba = slide
        .read_region_rgba_windowed(&region, &DisplayWindow::new(0.0, 255.0).unwrap())
        .expect("windowed convenience conversion");
    assert_eq!(rgba.get_pixel(0, 0).0, [10, 20, 30, 255]);
}

#[test]
fn subpixel_region_rejects_offsets_outside_one_pixel() {
    let slide = Slide::from_source(Box::new(TinySource::new()), Arc::new(TileCache::new(4096)));
    let req = RegionRequest::new(0usize, 0usize, 0u32, (0, 0), (1, 1));

    for offset in [(-0.1, 0.0), (1.0, 0.0), (0.0, f64::NAN)] {
        let err = slide
            .read_region_subpixel(&req, offset)
            .expect_err("invalid subpixel offset must fail");
        assert!(
            err.to_string()
                .contains("subpixel offset must be finite and in [0, 1)"),
            "unexpected error for {offset:?}: {err}"
        );
    }
}

#[test]
fn zero_subpixel_offset_preserves_integral_region_path() {
    let slide = Slide::from_source(Box::new(TinySource::new()), Arc::new(TileCache::new(4096)));
    let req = RegionRequest::new(0usize, 0usize, 0u32, (0, 0), (1, 1));

    let integral = slide.read_region(&req).expect("integral region");
    let subpixel = slide
        .read_region_subpixel(&req, (0.0, 0.0))
        .expect("zero-offset region");

    assert_eq!(subpixel.data.as_u8(), integral.data.as_u8());
}

#[test]
fn fractional_region_preserves_filter_coverage_as_rgba() {
    let slide = Slide::from_source(Box::new(TinySource::new()), Arc::new(TileCache::new(4096)));
    let req = RegionRequest::new(0usize, 0usize, 0u32, (0, 0), (1, 1));

    let tile = slide
        .read_region_subpixel(&req, (0.5, 0.5))
        .expect("fractional region");
    let pixel = tile.as_u8().expect("RGBA8 fractional region");

    assert_eq!(tile.color_space(), &ColorSpace::Rgba);
    assert_eq!(tile.channels(), 4);
    assert_eq!(pixel, &[8, 20, 28, 64]);
}

#[test]
fn tiny_nonzero_subpixel_offset_still_uses_fractional_composition() {
    let slide = Slide::from_source(Box::new(TinySource::new()), Arc::new(TileCache::new(4096)));
    let req = RegionRequest::new(0usize, 0usize, 0u32, (0, 0), (1, 1));

    let tile = slide
        .read_region_subpixel(&req, (f64::EPSILON, f64::EPSILON))
        .expect("tiny fractional region");

    assert_eq!(tile.color_space(), &ColorSpace::Rgba);
}

struct RegionBatchSource {
    dataset: Dataset,
    calls: RecordedTileBatches,
}

impl RegionBatchSource {
    fn pixel_tile(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        let TileLayout::Regular {
            tile_width,
            tile_height,
            ..
        } = self.dataset.scenes[0].series[0].levels[0].tile_layout
        else {
            panic!("regular fixture")
        };
        CpuTile::from_u8_interleaved(
            tile_width,
            tile_height,
            3,
            ColorSpace::Rgb,
            [req.col as u8 * 40, req.row as u8 * 40, 123]
                .repeat(tile_width as usize * tile_height as usize),
        )
    }
}

impl SlideReader for RegionBatchSource {
    fn dataset(&self) -> &Dataset {
        &self.dataset
    }
    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        self.calls.lock().unwrap().push(vec![(req.col, req.row)]);
        self.pixel_tile(req)
    }
    fn read_tiles_cpu(&self, reqs: &[TileRequest]) -> Result<Vec<CpuTile>, WsiError> {
        self.calls
            .lock()
            .unwrap()
            .push(reqs.iter().map(|r| (r.col, r.row)).collect());
        reqs.iter().map(|req| self.pixel_tile(req)).collect()
    }
}

#[test]
fn region_batches_preserve_pixels_and_fit_staging_and_encoded_bounds() {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(4)
        .build()
        .unwrap();
    for encoded_bound in [0, 1] {
        for offset in [(0.0, 0.0), (0.25, 0.5)] {
            let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
            let source = RegionBatchSource {
                dataset: regular_rgb_dataset_for_test(
                    DatasetId::new(90),
                    "scene",
                    "series",
                    RegularLevelForTest {
                        dimensions: (16, 16),
                        tile_width: 4,
                        tile_height: 4,
                        tiles_across: 4,
                        tiles_down: 4,
                    },
                ),
                calls: calls.clone(),
            };
            let req = RegionRequest::new(0, 0, 0, (1, 1), (12, 12));
            let expected = if offset == (0.0, 0.0) {
                composite_region_from_source(&source, None, &req, 1024)
            } else {
                composite_fractional_region_from_source(
                    &source,
                    None,
                    &req,
                    (1.0 + offset.0, 1.0 + offset.1),
                    1024,
                )
            }
            .unwrap();
            let expected_order: Vec<_> = calls.lock().unwrap().iter().flatten().copied().collect();
            calls.lock().unwrap().clear();
            let slide = Slide::from_managed_source_with_config_and_runtime(
                Box::new(ConservativeManagedReader::new(
                    Box::new(source),
                    encoded_bound,
                )),
                CacheConfig::deterministic().with_shared_tile_bytes(0),
                SlideLimits::default(),
                Arc::new(
                    DecodeRuntime::new(
                        DecodeExecutionOptions::default()
                            .with_acceleration(crate::DecodeAcceleration::CpuOnly),
                    )
                    .unwrap(),
                ),
            );
            // No idle cores: streamed batches keep their bounded structure.
            let actual = pool
                .install(|| {
                    crate::core::batch::with_idle_cores(0, || {
                        slide.read_region_subpixel(&req, offset)
                    })
                })
                .unwrap();
            assert_eq!(actual.to_rgba().unwrap(), expected.to_rgba().unwrap());
            let calls = calls.lock().unwrap();
            let lengths: Vec<_> = calls.iter().map(Vec::len).collect();
            if encoded_bound == 0 {
                assert_eq!(lengths, [2; 8], "two tiles plus codec work fit staging");
            } else {
                assert_eq!(
                    lengths, [1; 16],
                    "unknown concurrent inputs require singleton reads"
                );
            }
            assert_eq!(
                calls.iter().flatten().copied().collect::<Vec<_>>(),
                expected_order
            );
        }
    }
}

#[test]
fn a_complete_region_fitting_admission_keeps_one_batch_with_one_worker() {
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let source = RegionBatchSource {
        dataset: regular_rgb_dataset_for_test(
            DatasetId::new(93),
            "scene",
            "series",
            RegularLevelForTest {
                dimensions: (8, 8),
                tile_width: 4,
                tile_height: 4,
                tiles_across: 2,
                tiles_down: 2,
            },
        ),
        calls: calls.clone(),
    };
    let req = RegionRequest::new(0, 0, 0, (0, 0), (8, 8));
    let expected = composite_region_from_source(&source, None, &req, 1024).unwrap();
    calls.lock().unwrap().clear();
    let slide = Slide::from_managed_source_with_config_and_runtime(
        Box::new(ConservativeManagedReader::new(Box::new(source), 0)),
        CacheConfig::deterministic().with_shared_tile_bytes(0),
        SlideLimits::default(),
        Arc::new(
            DecodeRuntime::new(
                DecodeExecutionOptions::default()
                    .with_acceleration(crate::DecodeAcceleration::CpuOnly),
            )
            .unwrap(),
        ),
    );
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    let actual = pool.install(|| slide.read_region(&req)).unwrap();
    assert_eq!(actual.as_u8(), expected.as_u8());
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .map(Vec::len)
            .collect::<Vec<_>>(),
        [4]
    );
}

impl ManagedSlideReader for RegionBatchSource {
    fn tile_encoded_upper_bound(&self, _: &TileRequest) -> Result<u64, WsiError> {
        Ok(0)
    }
    fn tile_batch_encoded_upper_bound(&self, _: &[TileRequest]) -> Result<u64, WsiError> {
        Ok(0)
    }
    fn region_fastpath_encoded_upper_bound(&self, _: &RegionRequest) -> Result<u64, WsiError> {
        Ok(1024)
    }
    fn display_tile_encoded_upper_bound(&self, _: &TileViewRequest) -> Result<u64, WsiError> {
        Ok(1024)
    }
    fn associated_encoded_upper_bound(&self, _: &str) -> Result<u64, WsiError> {
        Ok(0)
    }
}

#[test]
fn streamed_batches_do_not_repurpose_encoded_allowance_as_decoded_staging() {
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let source = RegionBatchSource {
        dataset: regular_rgb_dataset_for_test(
            DatasetId::new(94),
            "scene",
            "series",
            RegularLevelForTest {
                dimensions: (12, 12),
                tile_width: 4,
                tile_height: 4,
                tiles_across: 3,
                tiles_down: 3,
            },
        ),
        calls: calls.clone(),
    };
    let slide = Slide::from_managed_source_with_config_and_runtime(
        Box::new(source),
        CacheConfig::deterministic().with_shared_tile_bytes(0),
        SlideLimits::default(),
        Arc::new(
            DecodeRuntime::new(
                DecodeExecutionOptions::default()
                    .with_acceleration(crate::DecodeAcceleration::CpuOnly),
            )
            .unwrap(),
        ),
    );
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(8)
        .build()
        .unwrap();
    let tile = pool
        .install(|| {
            crate::core::batch::with_idle_cores(0, || {
                slide.read_region(&RegionRequest::new(0, 0, 0, (1, 1), (8, 8)))
            })
        })
        .unwrap();
    assert_eq!((tile.width(), tile.height()), (8, 8));
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .map(Vec::len)
            .collect::<Vec<_>>(),
        [1; 9]
    );
}

#[test]
fn streamed_regions_bound_codec_staging_without_changing_pixels() {
    for size in [512, 1024] {
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let source = RegionBatchSource {
            dataset: regular_rgb_dataset_for_test(
                DatasetId::new(95),
                "scene",
                "series",
                RegularLevelForTest {
                    dimensions: (1536, 1536),
                    tile_width: 256,
                    tile_height: 256,
                    tiles_across: 6,
                    tiles_down: 6,
                },
            ),
            calls: calls.clone(),
        };
        let req = RegionRequest::new(0, 0, 0, (1, 1), (size, size));
        let expected = composite_region_from_source(&source, None, &req, 2 * 1024 * 1024).unwrap();
        calls.lock().unwrap().clear();
        let slide = Slide::from_managed_source_with_config_and_runtime(
            Box::new(source),
            CacheConfig::default().with_shared_tile_bytes(8 * 1024 * 1024),
            SlideLimits::default(),
            Arc::new(
                DecodeRuntime::new(
                    DecodeExecutionOptions::default()
                        .with_acceleration(crate::DecodeAcceleration::CpuOnly),
                )
                .unwrap(),
            ),
        );
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(8)
            .build()
            .unwrap();
        let actual = pool
            .install(|| crate::core::batch::with_idle_cores(0, || slide.read_region(&req)))
            .unwrap();
        assert_eq!(actual.as_u8(), expected.as_u8());
        let lengths = calls
            .lock()
            .unwrap()
            .iter()
            .map(Vec::len)
            .collect::<Vec<_>>();
        let expected_batches = if size == 512 {
            vec![1; 9]
        } else {
            [vec![2; 12], vec![1]].concat()
        };
        assert_eq!(lengths, expected_batches, "region size {size}");
        calls.lock().unwrap().clear();
        let warm = pool.install(|| slide.read_region(&req)).unwrap();
        assert_eq!(warm.as_u8(), actual.as_u8(), "cached region size {size}");
        assert!(
            calls.lock().unwrap().is_empty(),
            "cached regions must not decode again"
        );
    }
}

#[test]
fn streamed_batches_decode_several_tiles_on_idle_cores_when_admission_allows() {
    let read = |idle, limits: SlideLimits| {
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let source = RegionBatchSource {
            dataset: regular_rgb_dataset_for_test(
                DatasetId::new(96),
                "scene",
                "series",
                RegularLevelForTest {
                    dimensions: (12, 12),
                    tile_width: 4,
                    tile_height: 4,
                    tiles_across: 3,
                    tiles_down: 3,
                },
            ),
            calls: calls.clone(),
        };
        let req = RegionRequest::new(0, 0, 0, (1, 1), (8, 8));
        let expected = composite_region_from_source(&source, None, &req, 1024).unwrap();
        calls.lock().unwrap().clear();
        let slide = Slide::from_managed_source_with_config_and_runtime(
            Box::new(source),
            CacheConfig::deterministic().with_shared_tile_bytes(0),
            limits,
            Arc::new(
                DecodeRuntime::new(
                    DecodeExecutionOptions::default()
                        .with_acceleration(crate::DecodeAcceleration::CpuOnly),
                )
                .unwrap(),
            ),
        );
        let tile = crate::core::batch::with_idle_cores(idle, || slide.read_region(&req)).unwrap();
        assert_eq!(tile.as_u8(), expected.as_u8());
        let lengths = calls
            .lock()
            .unwrap()
            .iter()
            .map(Vec::len)
            .collect::<Vec<_>>();
        lengths
    };
    // Three idle cores let each streamed batch hold four tiles.
    assert_eq!(read(3, SlideLimits::default()), [4, 4, 1]);
    // Without optional headroom beyond the read's own work, tiles stream alone.
    let ordinary = SlideLimits::default()
        .with_operation_transient_bytes(1536)
        .unwrap();
    assert_eq!(read(3, ordinary), [1; 9]);
}

#[test]
fn streamed_tiles_larger_than_the_region_share_a_batch_on_idle_cores() {
    // 64-pixel tiles under a 32-pixel read: one decoded tile exceeds the
    // region's own staging, as a 512-pixel tile does under a 256-pixel read.
    let read = |idle| {
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let source = RegionBatchSource {
            dataset: regular_rgb_dataset_for_test(
                DatasetId::new(97),
                "scene",
                "series",
                RegularLevelForTest {
                    dimensions: (128, 128),
                    tile_width: 64,
                    tile_height: 64,
                    tiles_across: 2,
                    tiles_down: 2,
                },
            ),
            calls: calls.clone(),
        };
        let req = RegionRequest::new(0, 0, 0, (48, 48), (32, 32));
        let expected = composite_region_from_source(&source, None, &req, 1 << 20).unwrap();
        calls.lock().unwrap().clear();
        let slide = Slide::from_managed_source_with_config_and_runtime(
            Box::new(source),
            CacheConfig::deterministic().with_shared_tile_bytes(0),
            SlideLimits::default(),
            Arc::new(
                DecodeRuntime::new(
                    DecodeExecutionOptions::default()
                        .with_acceleration(crate::DecodeAcceleration::CpuOnly),
                )
                .unwrap(),
            ),
        );
        let tile = crate::core::batch::with_idle_cores(idle, || slide.read_region(&req)).unwrap();
        assert_eq!(tile.as_u8(), expected.as_u8());
        let lengths = calls
            .lock()
            .unwrap()
            .iter()
            .map(Vec::len)
            .collect::<Vec<_>>();
        lengths
    };
    assert_eq!(read(0), [1; 4]);
    assert_eq!(read(3), [4]);
}

struct RegionFastpathOnly(TinySource);

impl SlideReader for RegionFastpathOnly {
    fn dataset(&self) -> &Dataset {
        self.0.dataset()
    }
    fn read_tile_cpu(&self, _: &TileRequest) -> Result<CpuTile, WsiError> {
        Err(WsiError::Unsupported {
            reason: "use the region path".into(),
        })
    }
    fn read_region_fastpath(
        &self,
        _: &mut SlideReadContext<'_>,
        _: &RegionRequest,
    ) -> Option<Result<CpuTile, WsiError>> {
        Some(self.0.read_tile_cpu(&TileRequest::new(0, 0, 0, 0, 0)))
    }
}

impl ManagedSlideReader for RegionFastpathOnly {
    fn tile_encoded_upper_bound(&self, _: &TileRequest) -> Result<u64, WsiError> {
        Err(WsiError::Unsupported {
            reason: "generic tiles are unavailable".into(),
        })
    }
    fn tile_batch_encoded_upper_bound(&self, _: &[TileRequest]) -> Result<u64, WsiError> {
        Ok(0)
    }
    fn region_fastpath_encoded_upper_bound(&self, _: &RegionRequest) -> Result<u64, WsiError> {
        Ok(0)
    }
    fn display_tile_encoded_upper_bound(&self, _: &TileViewRequest) -> Result<u64, WsiError> {
        Ok(0)
    }
    fn associated_encoded_upper_bound(&self, _: &str) -> Result<u64, WsiError> {
        Ok(0)
    }
}

#[test]
fn a_region_fastpath_does_not_require_unused_generic_tile_bounds() {
    let source = RegionFastpathOnly(TinySource::new());
    let slide = Slide::from_managed_source_with_config_and_runtime(
        Box::new(source),
        CacheConfig::default(),
        SlideLimits::default(),
        Arc::new(
            DecodeRuntime::new(
                DecodeExecutionOptions::default()
                    .with_acceleration(crate::DecodeAcceleration::CpuOnly),
            )
            .unwrap(),
        ),
    );
    assert_eq!(
        slide
            .read_region(&RegionRequest::new(0, 0, 0, (0, 0), (1, 1)))
            .unwrap()
            .as_u8()
            .unwrap(),
        &[10, 20, 30]
    );
}

/// 4×4 grid of 32-pixel tiles at whole-pixel positions, missing tile (3, 3).
struct IrregularGridSource {
    dataset: Dataset,
    color_space: ColorSpace,
}

impl IrregularGridSource {
    fn new(color_space: ColorSpace, irregular: bool) -> Self {
        let layout = if irregular {
            TileLayout::Irregular {
                tile_advance: (32.0, 32.0),
                extra_tiles: (0, 0, 0, 0),
                tiles: (0..4)
                    .flat_map(|row| (0..4).map(move |col| (col, row)))
                    .filter(|position| *position != (3, 3))
                    .map(|position| (position, TileEntry::new((0.0, 0.0), (32, 32))))
                    .collect(),
            }
        } else {
            TileLayout::Regular {
                tile_width: 32,
                tile_height: 32,
                tiles_across: 4,
                tiles_down: 4,
            }
        };
        let series = Series::new(
            "grid",
            AxesShape::default(),
            vec![Level::new((128, 128), 1.0, layout)],
            SampleType::Uint8,
            vec![],
        );
        Self {
            dataset: Dataset::new(DatasetId::new(78), vec![Scene::new("grid", vec![series])]),
            color_space,
        }
    }
}

impl SlideReader for IrregularGridSource {
    fn dataset(&self) -> &Dataset {
        &self.dataset
    }

    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        let channels = if self.color_space == ColorSpace::Rgba {
            4
        } else {
            3
        };
        let mut data = Vec::with_capacity(32 * 32 * channels);
        for y in 0..32 {
            for x in 0..32 {
                let pixel = [(req.col * 32 + x) as u8, (req.row * 32 + y) as u8, 7, 255];
                data.extend_from_slice(&pixel[..channels]);
            }
        }
        CpuTile::from_u8_interleaved(32, 32, channels as u16, self.color_space.clone(), data)
    }
}

#[test]
fn cached_dense_irregular_regions_write_argb32_like_the_composed_path() {
    let slide = Slide::from_source(
        Box::new(IrregularGridSource::new(ColorSpace::Rgb, true)),
        Arc::new(TileCache::new(1 << 20)),
    );
    let dense = RegionRequest::new(0usize, 0usize, 0u32, (5, 9), (70, 50));
    let mut pixels = vec![u32::MAX; 70 * 50];

    // Uncached tiles need the ordinary path, which leaves pixels untouched.
    assert!(!slide
        .read_cached_region_argb32_into(&dense, (0.0, 0.0), &mut pixels)
        .unwrap());
    assert!(pixels.iter().all(|&pixel| pixel == u32::MAX));

    // Uncached composition keeps coverage as RGBA; dense coverage is opaque.
    let composed = slide.read_region_subpixel(&dense, (0.0, 0.0)).unwrap();
    let channels = usize::from(composed.channels());
    let expected: Vec<u32> = composed
        .as_u8()
        .unwrap()
        .chunks_exact(channels)
        .map(|pixel| {
            assert!(channels == 3 || pixel[3] == 255);
            0xff00_0000
                | (u32::from(pixel[0]) << 16)
                | (u32::from(pixel[1]) << 8)
                | u32::from(pixel[2])
        })
        .collect();
    assert!(slide
        .read_cached_region_argb32_into(&dense, (0.0, 0.0), &mut pixels)
        .unwrap());
    assert_eq!(pixels, expected);

    let mut short = vec![0; 70 * 50 - 1];
    assert!(slide
        .read_cached_region_argb32_into(&dense, (0.0, 0.0), &mut short)
        .is_err());

    // A gap or a fractional placement keeps the coverage-preserving path.
    let gap = RegionRequest::new(0usize, 0usize, 0u32, (80, 80), (40, 40));
    for (request, offset) in [(&gap, (0.0, 0.0)), (&dense, (0.25, 0.0))] {
        slide.read_region_subpixel(request, offset).unwrap();
        let mut untouched = vec![u32::MAX; request.size_px.0 as usize * request.size_px.1 as usize];
        assert!(!slide
            .read_cached_region_argb32_into(request, offset, &mut untouched)
            .unwrap());
        assert!(untouched.iter().all(|&pixel| pixel == u32::MAX));
    }
}

#[test]
fn cached_argb32_regions_decline_regular_levels_and_alpha_tiles() {
    for (color_space, irregular) in [(ColorSpace::Rgb, false), (ColorSpace::Rgba, true)] {
        let slide = Slide::from_source(
            Box::new(IrregularGridSource::new(color_space, irregular)),
            Arc::new(TileCache::new(1 << 20)),
        );
        let request = RegionRequest::new(0usize, 0usize, 0u32, (5, 9), (70, 50));
        slide.read_region_subpixel(&request, (0.0, 0.0)).unwrap();
        let mut pixels = vec![u32::MAX; 70 * 50];
        assert!(!slide
            .read_cached_region_argb32_into(&request, (0.0, 0.0), &mut pixels)
            .unwrap());
        assert!(pixels.iter().all(|&pixel| pixel == u32::MAX));
    }
}
