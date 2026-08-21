use super::*;
use crate::core::types::*;
use crate::test_support::{regular_rgb_dataset_for_test, RegularLevelForTest};
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};

struct CountingAdaptiveSource {
    dataset: Dataset,
    batch_reads: Arc<AtomicUsize>,
    requested_tiles: Arc<AtomicUsize>,
}

impl CountingAdaptiveSource {
    fn new(batch_reads: Arc<AtomicUsize>, requested_tiles: Arc<AtomicUsize>) -> Self {
        Self {
            dataset: regular_rgb_dataset_for_test(
                DatasetId::new(42),
                "scene",
                "series",
                RegularLevelForTest {
                    dimensions: (128, 128),
                    tile_width: 128,
                    tile_height: 128,
                    tiles_across: 1,
                    tiles_down: 1,
                },
            ),
            batch_reads,
            requested_tiles,
        }
    }
}

impl SlideReader for CountingAdaptiveSource {
    fn dataset(&self) -> &Dataset {
        &self.dataset
    }

    fn tile_codec_kind(&self, _req: &TileRequest) -> TileCodecKind {
        TileCodecKind::Jp2k
    }

    fn read_tiles(
        &self,
        reqs: &[TileRequest],
        _output: TileOutputPreference,
    ) -> Result<Vec<TilePixels>, WsiError> {
        self.batch_reads.fetch_add(1, Ordering::SeqCst);
        self.requested_tiles.fetch_add(reqs.len(), Ordering::SeqCst);
        reqs.iter()
            .map(|req| self.read_tile_cpu(req).map(TilePixels::Cpu))
            .collect()
    }

    fn read_tile_cpu(&self, _req: &TileRequest) -> Result<CpuTile, WsiError> {
        Ok(CpuTile {
            width: 128,
            height: 128,
            channels: 3,
            color_space: ColorSpace::Rgb,
            layout: CpuTileLayout::Interleaved,
            data: CpuTileData::u8(vec![7; 128 * 128 * 3]),
        })
    }

    fn read_associated(&self, name: &str) -> Result<CpuTile, WsiError> {
        Err(WsiError::AssociatedImageNotFound(name.into()))
    }
}

#[test]
fn default_decode_options_reuse_shared_runtime() {
    let first = DecodeRuntime::arc_for_options(DecodeExecutionOptions::default()).expect("runtime");
    let second =
        DecodeRuntime::arc_for_options(DecodeExecutionOptions::default()).expect("runtime");

    assert!(Arc::ptr_eq(&first, &second));
}

#[test]
fn explicit_cpu_pool_options_and_inline_runtime_preserve_their_contracts() {
    let threads = NonZeroUsize::new(2).unwrap();
    let options = DecodeExecutionOptions::default().with_jp2k_cpu_threads(threads);
    assert_eq!(options.jp2k_cpu_threads(), Some(threads));

    let runtime = DecodeRuntime::inline(options);
    assert!(!runtime.has_jp2k_cpu_pool());
    assert_eq!(runtime.install_jp2k_cpu(|| 23), 23);
    assert_eq!(runtime.options().jp2k_cpu_threads(), Some(threads));
}

struct PoolRecordingSource {
    dataset: Dataset,
    observed_threads: Arc<AtomicUsize>,
}

impl PoolRecordingSource {
    fn new(dataset_id: u128, observed_threads: Arc<AtomicUsize>) -> Self {
        Self {
            dataset: regular_rgb_dataset_for_test(
                DatasetId::new(dataset_id),
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
            observed_threads,
        }
    }
}

impl SlideReader for PoolRecordingSource {
    fn dataset(&self) -> &Dataset {
        &self.dataset
    }

    fn tile_codec_kind(&self, _req: &TileRequest) -> TileCodecKind {
        TileCodecKind::Jp2k
    }

    fn read_tiles(
        &self,
        reqs: &[TileRequest],
        _output: TileOutputPreference,
    ) -> Result<Vec<TilePixels>, WsiError> {
        self.observed_threads
            .store(rayon::current_num_threads(), Ordering::SeqCst);
        Ok(reqs
            .iter()
            .map(|_| {
                TilePixels::Cpu(
                    CpuTile::from_u8_interleaved(1, 1, 3, ColorSpace::Rgb, vec![1, 2, 3]).unwrap(),
                )
            })
            .collect())
    }

    fn read_tile_cpu(&self, _req: &TileRequest) -> Result<CpuTile, WsiError> {
        unreachable!("the test exercises the batch boundary")
    }
}

#[test]
fn adaptive_jp2k_reads_use_their_explicit_runtime_without_cross_read_leakage() {
    let request = TileRequest::new(0usize, 0usize, 0u32, 0, 0);
    for (dataset_id, threads) in [(101, 1), (102, 2), (103, 1)] {
        let observed = Arc::new(AtomicUsize::new(0));
        let options = DecodeExecutionOptions::default()
            .with_jp2k_cpu_threads(NonZeroUsize::new(threads).unwrap());
        let runtime = Arc::new(DecodeRuntime::new(options).unwrap());
        let reader = AdaptiveDecodeReader::new(
            Box::new(PoolRecordingSource::new(dataset_id, Arc::clone(&observed))),
            runtime,
        );

        reader
            .read_tiles(std::slice::from_ref(&request), TileOutputPreference::cpu())
            .unwrap();

        assert_eq!(observed.load(Ordering::SeqCst), threads);
    }
}

#[test]
fn matching_rayon_width_avoids_reentering_an_equivalent_decode_pool() {
    let threads = NonZeroUsize::new(rayon::current_num_threads()).unwrap();
    let runtime =
        DecodeRuntime::new(DecodeExecutionOptions::default().with_jp2k_cpu_threads(threads))
            .unwrap();
    let caller_thread = std::thread::current().id();

    let operation_thread = runtime.install_jp2k_cpu(|| std::thread::current().id());

    assert_eq!(operation_thread, caller_thread);
}

#[test]
fn route_cache_operations_recover_after_mutex_poisoning() {
    let runtime = Arc::new(DecodeRuntime::inline(DecodeExecutionOptions::default()));
    let poisoned = Arc::clone(&runtime);
    let _ = std::thread::spawn(move || {
        let _guard = poisoned.route_cache.lock().unwrap();
        panic!("poison route cache");
    })
    .join();

    let first_key = route_key_for_test(20_001);
    let first =
        DecodeRouteDecision::measured(1, Duration::from_millis(2), Duration::from_millis(1), 1);
    runtime.store_route(first_key.clone(), first.clone());
    assert_eq!(runtime.cached_route(&first_key), Some(first));

    let second_key = route_key_for_test(20_002);
    runtime
        .store_route_controlled(
            second_key.clone(),
            DecodeRouteDecision::measured(1, Duration::from_millis(3), Duration::from_millis(1), 1),
            &crate::ReadControl::default(),
        )
        .unwrap();
    assert!(runtime.cached_route(&second_key).is_some());
}

struct DelegatingSource {
    dataset: Dataset,
}

impl DelegatingSource {
    fn new() -> Self {
        Self {
            dataset: regular_rgb_dataset_for_test(
                DatasetId::new(99),
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

    fn tile() -> CpuTile {
        CpuTile::from_u8_interleaved(1, 1, 3, ColorSpace::Rgb, vec![7, 8, 9]).unwrap()
    }
}

impl SlideReader for DelegatingSource {
    fn dataset(&self) -> &Dataset {
        &self.dataset
    }

    fn read_tile_cpu(&self, _req: &TileRequest) -> Result<CpuTile, WsiError> {
        Ok(Self::tile())
    }

    fn read_raw_compressed_display_tile(
        &self,
        _req: &TileViewRequest,
    ) -> Result<RawCompressedTile, WsiError> {
        RawCompressedTile::builder(Compression::Jpeg)
            .dimensions(1, 1)
            .bits_allocated(8)
            .samples_per_pixel(3)
            .photometric_interpretation(EncodedTilePhotometricInterpretation::Rgb)
            .data(vec![0xff, 0xd8, 0xff, 0xd9])
            .build()
            .map_err(|error| WsiError::Jpeg(error.to_string()))
    }

    fn read_region(
        &self,
        _req: &RegionRequest,
        _output: TileOutputPreference,
    ) -> Result<TilePixels, WsiError> {
        Ok(TilePixels::Cpu(Self::tile()))
    }

    fn read_display_tile(&self, _req: &TileViewRequest) -> Result<CpuTile, WsiError> {
        Ok(Self::tile())
    }

    fn recommended_shared_cache_bytes(&self) -> Option<u64> {
        Some(4096)
    }
}

#[test]
fn adaptive_reader_preserves_nonadaptive_reader_boundaries() {
    let runtime = Arc::new(DecodeRuntime::inline(DecodeExecutionOptions::default()));
    let reader = AdaptiveDecodeReader::new(Box::new(DelegatingSource::new()), runtime);
    let view = TileViewRequest::new(0usize, 0usize, 0u32, 0, 0, 1, 1);
    let region = RegionRequest::new(0usize, 0usize, 0u32, (0, 0), (1, 1));

    let raw = reader
        .read_raw_compressed_display_tile(&view)
        .expect("raw display tile delegation");
    assert_eq!(raw.data(), &[0xff, 0xd8, 0xff, 0xd9]);

    let region_pixels = reader
        .read_region(&region, TileOutputPreference::cpu())
        .expect("region delegation");
    #[allow(unreachable_patterns)]
    let region_tile = match region_pixels {
        TilePixels::Cpu(tile) => tile,
        TilePixels::Device(_) => panic!("test reader returns a CPU region"),
    };
    assert_eq!(region_tile.as_u8(), Some(&[7, 8, 9][..]));
    assert_eq!(
        reader.read_display_tile(&view).unwrap().as_u8(),
        Some(&[7, 8, 9][..])
    );
    assert_eq!(reader.recommended_shared_cache_bytes(), Some(4096));
}

#[test]
fn route_cache_is_bounded() {
    let runtime = DecodeRuntime::new(DecodeExecutionOptions::default()).expect("runtime");
    let first_key = route_key_for_test(0);

    for sequence in 0..ROUTE_CACHE_MAX_ENTRIES + 5 {
        runtime.store_route(
            route_key_for_test(sequence),
            DecodeRouteDecision::measured(1, Duration::from_millis(2), Duration::from_millis(1), 1),
        );
    }

    let cache_len = runtime
        .route_cache
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .len();
    assert_eq!(cache_len, ROUTE_CACHE_MAX_ENTRIES);
    assert!(runtime.cached_route(&first_key).is_none());
    assert!(runtime
        .cached_route(&route_key_for_test(ROUTE_CACHE_MAX_ENTRIES + 4))
        .is_some());
}

#[test]
fn controlled_route_publication_rechecks_cancellation_under_the_cache_lock() {
    let runtime = DecodeRuntime::new(DecodeExecutionOptions::default()).expect("runtime");
    let key = route_key_for_test(99);
    let token = crate::ReadCancellationToken::new();
    let control = crate::ReadControl::new(token.clone());

    let error = runtime
        .store_route_controlled_with_hook(
            key.clone(),
            DecodeRouteDecision::measured(1, Duration::from_millis(2), Duration::from_millis(1), 1),
            &control,
            || token.cancel(),
        )
        .expect_err("cancellation at the locked publication boundary must prevent insertion");

    assert!(matches!(error, WsiError::Cancelled));
    assert!(runtime.cached_route(&key).is_none());
}

fn route_key_for_test(sequence: usize) -> DecodeRouteKey {
    DecodeRouteKey {
        dataset_id: sequence as u128,
        scene: 0,
        series: 0,
        level: 0,
        tile_grid: RouteTileGrid {
            tile_width: 128,
            tile_height: 128,
            tiles_across: 1,
            tiles_down: 1,
        },
        codec_kind: TileCodecKind::Jp2k,
        output_backend: OutputBackendRequest::Auto,
        device_backend_identity: format!("test-{sequence}"),
        sample_tile_count: 1,
    }
}

struct AdaptiveRouteFixture {
    batch_reads: Arc<AtomicUsize>,
    requested_tiles: Arc<AtomicUsize>,
    runtime: Arc<DecodeRuntime>,
    reader: AdaptiveDecodeReader,
    req: TileRequest,
}

fn adaptive_route_fixture(route_sample_size: usize) -> AdaptiveRouteFixture {
    let batch_reads = Arc::new(AtomicUsize::new(0));
    let requested_tiles = Arc::new(AtomicUsize::new(0));
    let runtime = Arc::new(
        DecodeRuntime::new(
            DecodeExecutionOptions::default().with_route_sample_size(route_sample_size),
        )
        .expect("decode runtime"),
    );
    let reader = AdaptiveDecodeReader::new(
        Box::new(CountingAdaptiveSource::new(
            batch_reads.clone(),
            requested_tiles.clone(),
        )),
        runtime.clone(),
    );
    let req = TileRequest {
        scene: 0usize.into(),
        series: 0usize.into(),
        level: 0u32.into(),
        plane: PlaneSelection::default().into(),
        col: 0,
        row: 0,
    };

    AdaptiveRouteFixture {
        batch_reads,
        requested_tiles,
        runtime,
        reader,
        req,
    }
}

#[test]
fn adaptive_route_reuses_device_cpu_fallback_sample_for_first_read() {
    let fixture = adaptive_route_fixture(4);

    let tiles = fixture
        .reader
        .read_tiles(
            &[fixture.req],
            TileOutputPreference::prefer_device_auto_with_compressed_decode(),
        )
        .expect("adaptive read");

    assert_eq!(tiles.len(), 1);
    assert_eq!(fixture.batch_reads.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.requested_tiles.load(Ordering::SeqCst), 1);
}

#[test]
fn adaptive_route_keys_subsampled_batches_separately() {
    let fixture = adaptive_route_fixture(4);
    let output = TileOutputPreference::prefer_device_auto_with_compressed_decode();
    let single_key = route_key_for_batch(
        fixture.reader.inner.as_ref(),
        std::slice::from_ref(&fixture.req),
        &output,
        4,
    )
    .expect("route key is available for one-tile JP2K regular batch");
    let full_sample_key = route_key_for_batch(
        fixture.reader.inner.as_ref(),
        &[
            fixture.req.clone(),
            fixture.req.clone(),
            fixture.req.clone(),
            fixture.req.clone(),
        ],
        &output,
        4,
    )
    .expect("route key is available for JP2K regular tile");

    let tiles = fixture
        .reader
        .read_tiles(&[fixture.req], output)
        .expect("adaptive read");

    assert_eq!(tiles.len(), 1);
    assert_eq!(fixture.batch_reads.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.requested_tiles.load(Ordering::SeqCst), 1);
    assert!(
        fixture.runtime.cached_route(&single_key).is_some(),
        "a one-tile read should cache its own route"
    );
    assert!(
        fixture.runtime.cached_route(&full_sample_key).is_none(),
        "a one-tile read must not poison the route for four-plus-tile batches"
    );
}

#[test]
fn default_route_sample_covers_viewer_sized_dicom_device_batches() {
    let batch_reads = Arc::new(AtomicUsize::new(0));
    let requested_tiles = Arc::new(AtomicUsize::new(0));
    let reader = CountingAdaptiveSource::new(batch_reads, requested_tiles);
    let reqs = (0..15)
        .map(|col| TileRequest {
            scene: 0usize.into(),
            series: 0usize.into(),
            level: 0u32.into(),
            plane: PlaneSelection::default().into(),
            col: col as i64,
            row: 0,
        })
        .collect::<Vec<_>>();
    let output = TileOutputPreference::prefer_device_auto_with_compressed_decode();

    let key = route_key_for_batch(
        &reader,
        &reqs,
        &output,
        DecodeExecutionOptions::default().route_sample_size(),
    )
    .expect("route key is available for a viewer-sized JP2K batch");

    assert_eq!(
            key.sample_tile_count, 15,
            "default adaptive sampling must measure a real visible-tile batch instead of undersampling into the CPU path"
        );
}

#[test]
fn adaptive_route_sends_large_jp2k_batches_to_device_preferred_reader_without_sampling() {
    let batch_reads = Arc::new(AtomicUsize::new(0));
    let requested_tiles = Arc::new(AtomicUsize::new(0));
    let runtime = Arc::new(DecodeRuntime::new(DecodeExecutionOptions::default()).unwrap());
    let reader = AdaptiveDecodeReader::new(
        Box::new(CountingAdaptiveSource::new(
            batch_reads.clone(),
            requested_tiles.clone(),
        )),
        runtime.clone(),
    );
    let reqs = (0..15)
        .map(|col| TileRequest {
            scene: 0usize.into(),
            series: 0usize.into(),
            level: 0u32.into(),
            plane: PlaneSelection::default().into(),
            col: col as i64,
            row: 0,
        })
        .collect::<Vec<_>>();
    let output = TileOutputPreference::prefer_device_auto_with_compressed_decode();
    let key = route_key_for_batch(
        reader.inner.as_ref(),
        &reqs,
        &output,
        DecodeExecutionOptions::default().route_sample_size(),
    )
    .expect("route key is available for a viewer-sized JP2K batch");

    let tiles = reader.read_tiles(&reqs, output).expect("adaptive read");

    assert_eq!(tiles.len(), 15);
    assert_eq!(
        batch_reads.load(Ordering::SeqCst),
        1,
        "large JP2K batches should avoid cold adaptive double-decode"
    );
    assert_eq!(requested_tiles.load(Ordering::SeqCst), 15);
    assert!(
        runtime.cached_route(&key).is_none(),
        "direct large-batch routing should not cache a CPU-biased sample"
    );
}

#[test]
fn adaptive_route_samples_uncached_subthreshold_batches_before_routing_remainder() {
    let batch_reads = Arc::new(AtomicUsize::new(0));
    let requested_tiles = Arc::new(AtomicUsize::new(0));
    let runtime = Arc::new(
        DecodeRuntime::new(DecodeExecutionOptions::default().with_route_sample_size(4))
            .expect("decode runtime"),
    );
    let reader = AdaptiveDecodeReader::new(
        Box::new(CountingAdaptiveSource::new(
            batch_reads.clone(),
            requested_tiles.clone(),
        )),
        runtime.clone(),
    );
    let reqs = (0..7)
        .map(|col| TileRequest {
            scene: 0usize.into(),
            series: 0usize.into(),
            level: 0u32.into(),
            plane: PlaneSelection::default().into(),
            col: col as i64,
            row: 0,
        })
        .collect::<Vec<_>>();
    let output = TileOutputPreference::prefer_device_auto_with_compressed_decode();
    let key = route_key_for_batch(reader.inner.as_ref(), &reqs, &output, 4)
        .expect("route key is available for JP2K regular tile");

    let tiles = reader
        .read_tiles(&reqs, output.clone())
        .expect("adaptive read");

    assert_eq!(tiles.len(), 7);
    assert_eq!(
        batch_reads.load(Ordering::SeqCst),
        2,
        "uncached subthreshold batches should sample, cache a route, then route the remainder"
    );
    assert_eq!(requested_tiles.load(Ordering::SeqCst), 7);
    assert!(
        runtime.cached_route(&key).is_some(),
        "subthreshold auto routing should cache a measured route"
    );

    batch_reads.store(0, Ordering::SeqCst);
    requested_tiles.store(0, Ordering::SeqCst);
    let tiles = reader
        .read_tiles(&reqs, output)
        .expect("cached adaptive read");

    assert_eq!(tiles.len(), 7);
    assert_eq!(
        batch_reads.load(Ordering::SeqCst),
        1,
        "cached large-batch routes should not resample"
    );
    assert_eq!(requested_tiles.load(Ordering::SeqCst), 7);
}

#[test]
fn adaptive_controlled_read_uses_the_same_sampling_and_cached_route_as_uncontrolled() {
    let fixture = adaptive_route_fixture(4);
    let reqs = vec![fixture.req.clone(); 7];
    let output = TileOutputPreference::prefer_device_auto_with_compressed_decode();
    let key = route_key_for_batch(fixture.reader.inner.as_ref(), &reqs, &output, 4)
        .expect("route key is available for controlled JP2K batch");

    let tiles = fixture
        .reader
        .read_tiles_controlled(&reqs, output.clone(), &crate::ReadControl::default())
        .expect("controlled adaptive read");

    assert_eq!(tiles.len(), reqs.len());
    assert_eq!(
        fixture.batch_reads.load(Ordering::SeqCst),
        2,
        "controlled reads must sample and route the remainder like uncontrolled reads"
    );
    assert_eq!(fixture.requested_tiles.load(Ordering::SeqCst), reqs.len());
    assert!(fixture.runtime.cached_route(&key).is_some());

    fixture.batch_reads.store(0, Ordering::SeqCst);
    fixture.requested_tiles.store(0, Ordering::SeqCst);
    let cached = fixture
        .reader
        .read_tiles_controlled(&reqs, output, &crate::ReadControl::default())
        .expect("cached controlled adaptive read");

    assert_eq!(cached.len(), reqs.len());
    assert_eq!(fixture.batch_reads.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.requested_tiles.load(Ordering::SeqCst), reqs.len());
}

struct CancellingAdaptiveSource {
    inner: CountingAdaptiveSource,
    token: crate::ReadCancellationToken,
    controlled_reads: Arc<AtomicUsize>,
}

impl SlideReader for CancellingAdaptiveSource {
    fn dataset(&self) -> &Dataset {
        self.inner.dataset()
    }

    fn tile_codec_kind(&self, req: &TileRequest) -> TileCodecKind {
        self.inner.tile_codec_kind(req)
    }

    fn read_tiles_controlled(
        &self,
        _reqs: &[TileRequest],
        _output: TileOutputPreference,
        _control: &crate::ReadControl,
    ) -> Result<Vec<TilePixels>, WsiError> {
        self.controlled_reads.fetch_add(1, Ordering::SeqCst);
        self.token.cancel();
        Err(WsiError::Cancelled)
    }

    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        self.inner.read_tile_cpu(req)
    }
}

#[test]
fn adaptive_controlled_read_does_not_retry_or_cache_after_cancellation() {
    let batch_reads = Arc::new(AtomicUsize::new(0));
    let requested_tiles = Arc::new(AtomicUsize::new(0));
    let controlled_reads = Arc::new(AtomicUsize::new(0));
    let token = crate::ReadCancellationToken::new();
    let runtime = Arc::new(
        DecodeRuntime::new(DecodeExecutionOptions::default().with_route_sample_size(4))
            .expect("decode runtime"),
    );
    let source = CancellingAdaptiveSource {
        inner: CountingAdaptiveSource::new(batch_reads, requested_tiles),
        token: token.clone(),
        controlled_reads: Arc::clone(&controlled_reads),
    };
    let req = TileRequest::new(0usize, 0usize, 0u32, 0, 0);
    let reqs = vec![req; 4];
    let output = TileOutputPreference::prefer_device_auto_with_compressed_decode();
    let key = route_key_for_batch(&source, &reqs, &output, 4)
        .expect("route key is available for cancelling JP2K batch");
    let reader = AdaptiveDecodeReader::new(Box::new(source), Arc::clone(&runtime));

    let error = reader
        .read_tiles_controlled(&reqs, output, &crate::ReadControl::new(token))
        .expect_err("cancellation must stop adaptive routing");

    assert!(matches!(error, WsiError::Cancelled));
    assert_eq!(controlled_reads.load(Ordering::SeqCst), 1);
    assert!(runtime.cached_route(&key).is_none());
}

struct FailingCancellingAdaptiveSource {
    inner: CountingAdaptiveSource,
    token: crate::ReadCancellationToken,
    controlled_reads: Arc<AtomicUsize>,
}

impl SlideReader for FailingCancellingAdaptiveSource {
    fn dataset(&self) -> &Dataset {
        self.inner.dataset()
    }

    fn tile_codec_kind(&self, req: &TileRequest) -> TileCodecKind {
        self.inner.tile_codec_kind(req)
    }

    fn read_tiles_controlled(
        &self,
        reqs: &[TileRequest],
        _output: TileOutputPreference,
        _control: &crate::ReadControl,
    ) -> Result<Vec<TilePixels>, WsiError> {
        self.controlled_reads.fetch_add(1, Ordering::SeqCst);
        self.token.cancel();
        let req = reqs.first().expect("adaptive test submits tiles");
        Err(WsiError::TileRead {
            col: req.col,
            row: req.row,
            level: req.level.get(),
            reason: "decode failed while cancellation was requested".into(),
        })
    }

    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        self.inner.read_tile_cpu(req)
    }
}

#[test]
fn adaptive_controlled_source_error_cannot_mask_terminal_cancellation() {
    let batch_reads = Arc::new(AtomicUsize::new(0));
    let requested_tiles = Arc::new(AtomicUsize::new(0));
    let controlled_reads = Arc::new(AtomicUsize::new(0));
    let token = crate::ReadCancellationToken::new();
    let runtime = Arc::new(
        DecodeRuntime::new(DecodeExecutionOptions::default().with_route_sample_size(4))
            .expect("decode runtime"),
    );
    let source = FailingCancellingAdaptiveSource {
        inner: CountingAdaptiveSource::new(batch_reads, requested_tiles),
        token: token.clone(),
        controlled_reads: Arc::clone(&controlled_reads),
    };
    let reqs = vec![TileRequest::new(0usize, 0usize, 0u32, 0, 0); 4];
    let output = TileOutputPreference::prefer_device_auto_with_compressed_decode();
    let key = route_key_for_batch(&source, &reqs, &output, 4)
        .expect("route key is available for cancelling JP2K batch");
    let reader = AdaptiveDecodeReader::new(Box::new(source), Arc::clone(&runtime));

    let error = reader
        .read_tiles_controlled(&reqs, output, &crate::ReadControl::new(token))
        .expect_err("terminal cancellation must replace a simultaneous source error");

    assert!(matches!(error, WsiError::Cancelled));
    assert_eq!(controlled_reads.load(Ordering::SeqCst), 1);
    assert!(runtime.cached_route(&key).is_none());
}
