use super::*;
use crate::core::types::*;
use crate::test_support::{regular_rgb_dataset_for_test, RegularLevelForTest};
use std::sync::atomic::{AtomicUsize, Ordering};

fn dataset(id: u128) -> Dataset {
    regular_rgb_dataset_for_test(
        DatasetId::new(id),
        "scene",
        "series",
        RegularLevelForTest {
            dimensions: (1_536, 1_024),
            tile_width: 256,
            tile_height: 256,
            tiles_across: 6,
            tiles_down: 4,
        },
    )
}

fn marker_tile(request: &TileRequest) -> CpuTile {
    let marker = u8::try_from(request.col).unwrap_or(u8::MAX);
    CpuTile::from_u8_interleaved(1, 1, 3, ColorSpace::Rgb, vec![marker; 3]).unwrap()
}

struct CountingSource {
    dataset: Dataset,
    batch_reads: Arc<AtomicUsize>,
    requested_tiles: Arc<AtomicUsize>,
    batch_sizes: Arc<Mutex<Vec<usize>>>,
    codec: TileCodecKind,
}

impl CountingSource {
    fn new(id: u128, codec: TileCodecKind) -> (Self, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let batch_reads = Arc::new(AtomicUsize::new(0));
        let requested_tiles = Arc::new(AtomicUsize::new(0));
        (
            Self {
                dataset: dataset(id),
                batch_reads: Arc::clone(&batch_reads),
                requested_tiles: Arc::clone(&requested_tiles),
                batch_sizes: Arc::new(Mutex::new(Vec::new())),
                codec,
            },
            batch_reads,
            requested_tiles,
        )
    }
}

impl SlideReader for CountingSource {
    fn dataset(&self) -> &Dataset {
        &self.dataset
    }

    fn tile_codec_kind(&self, _req: &TileRequest) -> TileCodecKind {
        self.codec
    }

    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        Ok(marker_tile(req))
    }

    fn read_tiles_cpu(&self, reqs: &[TileRequest]) -> Result<Vec<CpuTile>, WsiError> {
        self.batch_reads.fetch_add(1, Ordering::SeqCst);
        self.batch_sizes.lock().unwrap().push(reqs.len());
        self.requested_tiles.fetch_add(reqs.len(), Ordering::SeqCst);
        reqs.iter()
            .map(|request| Ok(marker_tile(request)))
            .collect()
    }
}

#[test]
fn decode_options_default_to_automatic_acceleration_and_hide_sampling_policy() {
    let options = DecodeExecutionOptions::default();

    assert_eq!(options.acceleration(), DecodeAcceleration::Auto);
    assert_eq!(
        options
            .with_acceleration(DecodeAcceleration::CpuOnly)
            .acceleration(),
        DecodeAcceleration::CpuOnly
    );
}

#[test]
fn default_decode_options_reuse_the_process_runtime() {
    let first = DecodeRuntime::arc_for_options(DecodeExecutionOptions::default()).unwrap();
    let second = DecodeRuntime::arc_for_options(DecodeExecutionOptions::default()).unwrap();

    assert!(Arc::ptr_eq(&first, &second));
}

#[test]
fn cpu_only_options_reuse_one_runtime_and_process_pool() {
    let options = DecodeExecutionOptions::default().with_acceleration(DecodeAcceleration::CpuOnly);
    let first = DecodeRuntime::arc_for_options(options).unwrap();
    let second = DecodeRuntime::arc_for_options(options).unwrap();

    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(first.options().acceleration(), DecodeAcceleration::CpuOnly);
    assert!(first
        .install_jp2k_cpu(rayon::current_thread_index)
        .is_some());
}

#[test]
fn cpu_only_batches_preserve_order_and_cardinality() {
    let (source, batch_reads, requested_tiles) = CountingSource::new(42, TileCodecKind::Jp2k);
    let runtime = Arc::new(
        DecodeRuntime::new(
            DecodeExecutionOptions::default().with_acceleration(DecodeAcceleration::CpuOnly),
        )
        .unwrap(),
    );
    let reader = AdaptiveDecodeReader::new(Box::new(source), runtime);
    let requests = [7, 2, 9]
        .into_iter()
        .map(|col| TileRequest::new(0usize, 0usize, 0u32, col, 0))
        .collect::<Vec<_>>();

    let tiles = reader.read_tiles_cpu(&requests).unwrap();

    assert_eq!(tiles.len(), requests.len());
    assert_eq!(batch_reads.load(Ordering::SeqCst), 1);
    assert_eq!(requested_tiles.load(Ordering::SeqCst), requests.len());
    assert_eq!(
        tiles
            .iter()
            .map(|tile| tile.as_u8().unwrap()[0])
            .collect::<Vec<_>>(),
        vec![7, 2, 9]
    );
}

#[test]
fn route_threshold_requires_a_fifteen_percent_device_win() {
    assert_eq!(
        DecodeRouteDecision::measured(Duration::from_millis(100), Duration::from_millis(85)).winner,
        DecodeRoute::Device
    );
    assert_eq!(
        DecodeRouteDecision::measured(Duration::from_millis(100), Duration::from_millis(86)).winner,
        DecodeRoute::Cpu
    );
    assert_eq!(
        DecodeRouteDecision::measured(Duration::ZERO, Duration::ZERO).winner,
        DecodeRoute::Cpu
    );
    let failure = DecodeRouteDecision::device_failure();
    assert_eq!(failure.winner, DecodeRoute::Cpu);
    assert!(failure.device_failure);
}

fn route_key(sequence: usize) -> DecodeRouteKey {
    DecodeRouteKey {
        dataset_id: sequence as u128,
        scene: 0,
        series: 0,
        level: 0,
        sample_geometry: RouteSampleGeometry::from_dimensions([(256, 256)]),
        codec_kind: TileCodecKind::Jp2k,
        device_identity: "unavailable".into(),
        sample_tile_count: 1,
        cpu_workers: 8,
    }
}

#[test]
fn route_cache_recovers_after_poisoning_and_remains_bounded() {
    let runtime = Arc::new(DecodeRuntime::inline(DecodeExecutionOptions::default()));
    let poisoned = Arc::clone(&runtime);
    let _ = std::thread::spawn(move || {
        let _guard = poisoned.route_cache.lock().unwrap();
        panic!("poison route cache");
    })
    .join();

    for sequence in 0..ROUTE_CACHE_MAX_ENTRIES + 5 {
        runtime
            .store_route(
                route_key(sequence),
                DecodeRouteDecision::measured(
                    Duration::from_millis(100),
                    Duration::from_millis(80),
                ),
                None,
            )
            .unwrap();
    }

    let cache = runtime
        .route_cache
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(cache.len(), ROUTE_CACHE_MAX_ENTRIES);
    drop(cache);
    assert!(runtime.cached_route(&route_key(0)).is_none());
    assert!(runtime
        .cached_route(&route_key(ROUTE_CACHE_MAX_ENTRIES + 4))
        .is_some());
}

#[test]
fn route_cache_reads_and_replacements_preserve_fifo_eviction_order() {
    let runtime = DecodeRuntime::inline(DecodeExecutionOptions::default());
    let decision =
        DecodeRouteDecision::measured(Duration::from_millis(100), Duration::from_millis(80));
    for sequence in 0..ROUTE_CACHE_MAX_ENTRIES {
        runtime
            .store_route(route_key(sequence), decision.clone(), None)
            .unwrap();
    }

    assert!(runtime.cached_route(&route_key(0)).is_some());
    runtime
        .store_route(route_key(0), decision.clone(), None)
        .unwrap();
    runtime
        .store_route(route_key(ROUTE_CACHE_MAX_ENTRIES), decision, None)
        .unwrap();

    assert!(runtime.cached_route(&route_key(0)).is_none());
    assert!(runtime.cached_route(&route_key(1)).is_some());
}

#[test]
fn cancelled_route_publication_does_not_mutate_the_cache() {
    let runtime = DecodeRuntime::inline(DecodeExecutionOptions::default());
    let token = crate::ReadCancellationToken::new();
    token.cancel();
    let control = crate::ReadControl::new(token);
    let key = route_key(91);

    let error = runtime
        .store_route(
            key.clone(),
            DecodeRouteDecision::measured(Duration::from_millis(100), Duration::from_millis(80)),
            Some(&control),
        )
        .unwrap_err();

    assert!(matches!(error, WsiError::Cancelled));
    assert!(runtime.cached_route(&key).is_none());
}

#[test]
fn route_keys_include_full_geometry_codec_batch_count_and_workers() {
    let (source, _, _) = CountingSource::new(73, TileCodecKind::Htj2k);
    let requests = (0..12)
        .map(|index| TileRequest::new(0usize, 0usize, 0u32, index % 6, index / 6))
        .collect::<Vec<_>>();

    let key = route_key_for_batch(&source, &requests, "test-device").unwrap();

    assert_eq!(key.codec_kind, TileCodecKind::Htj2k);
    assert_eq!(key.sample_tile_count, requests.len());
    assert_ne!(
        key,
        route_key_for_batch(&source, &requests[..8], "test-device").unwrap()
    );
    assert_eq!(
        key.sample_geometry,
        RouteSampleGeometry::from_dimensions([(256, 256); 12])
    );
}

#[test]
fn route_keys_sort_sample_geometry_and_distinguish_logical_edge_tiles() {
    let source = CountingSource {
        dataset: regular_rgb_dataset_for_test(
            DatasetId::new(78),
            "scene",
            "series",
            RegularLevelForTest {
                dimensions: (513, 257),
                tile_width: 256,
                tile_height: 256,
                tiles_across: 3,
                tiles_down: 2,
            },
        ),
        batch_reads: Arc::new(AtomicUsize::new(0)),
        requested_tiles: Arc::new(AtomicUsize::new(0)),
        batch_sizes: Arc::new(std::sync::Mutex::new(Vec::new())),
        codec: TileCodecKind::Jp2k,
    };
    let interior_then_edges = [
        TileRequest::new(0usize, 0usize, 0u32, 0, 0),
        TileRequest::new(0usize, 0usize, 0u32, 2, 0),
        TileRequest::new(0usize, 0usize, 0u32, 0, 1),
        TileRequest::new(0usize, 0usize, 0u32, 2, 1),
    ];
    let edges_then_interior = [
        interior_then_edges[3].clone(),
        interior_then_edges[2].clone(),
        interior_then_edges[1].clone(),
        interior_then_edges[0].clone(),
    ];

    let first = route_key_for_batch(&source, &interior_then_edges, "cuda:0").unwrap();
    let reordered = route_key_for_batch(&source, &edges_then_interior, "cuda:0").unwrap();
    let other_device = route_key_for_batch(&source, &interior_then_edges, "cuda:1").unwrap();
    let interior = route_key_for_batch(&source, &interior_then_edges[..1], "cuda:0").unwrap();
    let corner = route_key_for_batch(&source, &interior_then_edges[3..], "cuda:0").unwrap();

    assert_eq!(first, reordered);
    assert_eq!(
        first.sample_geometry,
        RouteSampleGeometry::from_dimensions([(1, 1), (1, 256), (256, 1), (256, 256)])
    );
    assert_ne!(interior.sample_geometry, corner.sample_geometry);
    assert_ne!(first, other_device);
}

#[test]
fn route_keys_reject_mixed_levels_and_non_jp2k_codecs() {
    let (jp2k, _, _) = CountingSource::new(74, TileCodecKind::Jp2k);
    let mixed_levels = [
        TileRequest::new(0usize, 0usize, 0u32, 0, 0),
        TileRequest::new(0usize, 0usize, 1u32, 0, 0),
    ];
    assert!(route_key_for_batch(&jp2k, &mixed_levels, "test-device").is_none());

    let (jpeg, _, _) = CountingSource::new(75, TileCodecKind::Jpeg);
    let request = TileRequest::new(0usize, 0usize, 0u32, 0, 0);
    assert!(route_key_for_batch(&jpeg, std::slice::from_ref(&request), "test-device").is_none());
}

struct CancellingSource {
    dataset: Dataset,
    token: crate::ReadCancellationToken,
    calls: Arc<AtomicUsize>,
}

impl SlideReader for CancellingSource {
    fn dataset(&self) -> &Dataset {
        &self.dataset
    }

    fn tile_codec_kind(&self, _req: &TileRequest) -> TileCodecKind {
        TileCodecKind::Jp2k
    }

    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        Ok(marker_tile(req))
    }

    fn read_tiles_cpu_controlled(
        &self,
        reqs: &[TileRequest],
        _control: &crate::ReadControl,
    ) -> Result<Vec<CpuTile>, WsiError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.token.cancel();
        let request = &reqs[0];
        Err(WsiError::TileRead {
            col: request.col,
            row: request.row,
            level: request.level.get(),
            reason: "decode failed while cancellation was requested".into(),
        })
    }
}

#[test]
fn terminal_cancellation_wins_over_a_simultaneous_source_error() {
    let token = crate::ReadCancellationToken::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let source = CancellingSource {
        dataset: dataset(76),
        token: token.clone(),
        calls: Arc::clone(&calls),
    };
    let runtime = Arc::new(
        DecodeRuntime::new(
            DecodeExecutionOptions::default().with_acceleration(DecodeAcceleration::CpuOnly),
        )
        .unwrap(),
    );
    let reader = AdaptiveDecodeReader::new(Box::new(source), runtime);
    let request = TileRequest::new(0usize, 0usize, 0u32, 0, 0);

    let error = reader
        .read_tiles_cpu_controlled(
            std::slice::from_ref(&request),
            &crate::ReadControl::new(token),
        )
        .unwrap_err();

    assert!(matches!(error, WsiError::Cancelled));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

struct DelegatingSource {
    dataset: Dataset,
}

impl SlideReader for DelegatingSource {
    fn dataset(&self) -> &Dataset {
        &self.dataset
    }

    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        Ok(marker_tile(req))
    }

    fn read_region(&self, _req: &RegionRequest) -> Result<CpuTile, WsiError> {
        Ok(marker_tile(&TileRequest::new(0usize, 0usize, 0u32, 8, 0)))
    }

    fn read_display_tile(&self, _req: &TileViewRequest) -> Result<CpuTile, WsiError> {
        Ok(marker_tile(&TileRequest::new(0usize, 0usize, 0u32, 9, 0)))
    }
}

#[test]
fn adaptive_reader_preserves_non_tile_boundaries() {
    let reader = AdaptiveDecodeReader::new(
        Box::new(DelegatingSource {
            dataset: dataset(77),
        }),
        Arc::new(DecodeRuntime::inline(DecodeExecutionOptions::default())),
    );
    let region = RegionRequest::new(0usize, 0usize, 0u32, (0, 0), (1, 1));
    let view = TileViewRequest::new(0usize, 0usize, 0u32, 0, 0, 1, 1);

    assert_eq!(reader.read_region(&region).unwrap().as_u8().unwrap()[0], 8);
    assert_eq!(
        reader.read_display_tile(&view).unwrap().as_u8().unwrap()[0],
        9
    );
}

#[cfg(feature = "metal")]
#[test]
fn first_automatic_read_returns_cpu_without_initializing_metal() {
    let (source, calls, tiles) = CountingSource::new(904, TileCodecKind::Jp2k);
    let runtime = Arc::new(DecodeRuntime::inline(DecodeExecutionOptions::default()));
    let reader = AdaptiveDecodeReader::new(Box::new(source), runtime.clone());
    let request = TileRequest::new(0usize, 0usize, 0u32, 0, 0);
    assert_eq!(
        reader.read_tiles_cpu(&[request]).unwrap()[0]
            .as_u8()
            .unwrap(),
        &[0; 3]
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(tiles.load(Ordering::SeqCst), 1);
    assert!(
        runtime.metal_sessions.get().is_none(),
        "first read initialized Metal calibration"
    );
}

#[test]
fn calibration_is_deferred_exclusive_and_uses_three_median_ratios() {
    let runtime = DecodeRuntime::inline(DecodeExecutionOptions::default());
    let key = route_key(909);
    assert!(matches!(
        runtime.claim_route(key.clone()),
        RouteClaim::FirstCpu { .. }
    ));
    let RouteClaim::Calibrate(warmup) = runtime.claim_route(key.clone()) else {
        panic!("warmup pending")
    };
    assert_eq!(warmup.step, CalibrationStep::Warmup);
    assert!(matches!(runtime.claim_route(key.clone()), RouteClaim::Cpu));
    warmup.complete(None, None).unwrap();
    for (index, device_ms) in [80, 200, 70].into_iter().enumerate() {
        let RouteClaim::Calibrate(sample) = runtime.claim_route(key.clone()) else {
            panic!("sample pending")
        };
        assert_eq!(
            sample.step,
            CalibrationStep::Sample {
                cpu_first: index % 2 == 0
            }
        );
        sample
            .complete(
                Some((Duration::from_millis(100), Duration::from_millis(device_ms))),
                None,
            )
            .unwrap();
        if index < 2 {
            assert!(runtime.cached_route(&key).is_none());
        }
    }
    assert_eq!(
        runtime.cached_route(&key).unwrap().winner,
        DecodeRoute::Device
    );
    let RouteClaim::Ready(decision) = runtime.claim_route(key) else {
        panic!("completed decision missing")
    };
    assert_eq!(decision.winner, DecodeRoute::Device);
}

#[test]
fn abandoned_and_cancelled_calibration_release_ownership_without_publishing() {
    let runtime = DecodeRuntime::inline(DecodeExecutionOptions::default());
    let key = route_key(910);
    assert!(matches!(
        runtime.claim_route(key.clone()),
        RouteClaim::FirstCpu { .. }
    ));
    let RouteClaim::Calibrate(lease) = runtime.claim_route(key.clone()) else {
        panic!("warmup pending")
    };
    drop(lease);
    let RouteClaim::Calibrate(lease) = runtime.claim_route(key.clone()) else {
        panic!("ownership leaked")
    };
    let token = crate::ReadCancellationToken::new();
    token.cancel();
    assert!(matches!(
        lease.complete(None, Some(&crate::ReadControl::new(token))),
        Err(WsiError::Cancelled)
    ));
    let RouteClaim::Calibrate(lease) = runtime.claim_route(key.clone()) else {
        panic!("ownership leaked")
    };
    assert_eq!(lease.step, CalibrationStep::Warmup);
    drop(lease);
    assert!(runtime.cached_route(&key).is_none());
}

#[test]
fn busy_routes_cannot_be_evicted_into_duplicate_calibration() {
    let runtime = DecodeRuntime::inline(DecodeExecutionOptions::default());
    let mut leases = Vec::new();
    for n in 0..ROUTE_CACHE_MAX_ENTRIES {
        assert!(matches!(
            runtime.claim_route(route_key(n)),
            RouteClaim::FirstCpu { .. }
        ));
        let RouteClaim::Calibrate(lease) = runtime.claim_route(route_key(n)) else {
            panic!("warmup pending")
        };
        leases.push(lease);
    }
    assert!(matches!(
        runtime.claim_route(route_key(9000)),
        RouteClaim::Cpu
    ));
    assert_eq!(
        runtime.route_cache.lock().unwrap().len(),
        ROUTE_CACHE_MAX_ENTRIES
    );
    assert!(matches!(runtime.claim_route(route_key(0)), RouteClaim::Cpu));
    drop(leases);
    let RouteClaim::Calibrate(_) = runtime.claim_route(route_key(0)) else {
        panic!("owner not released")
    };
}

#[cfg(feature = "metal")]
#[test]
fn admitted_auto_reads_warm_once_then_collect_three_complete_comparisons() {
    use crate::core::execution_telemetry::{test_count, Event};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("prepared.j2k");
    std::fs::write(
        &path,
        include_bytes!("../../../tests/fixtures/jp2k/rgb_nomct.j2k"),
    )
    .unwrap();
    let slide = crate::Slide::open(&path).unwrap();
    let request = TileRequest::new(0usize, 0usize, 0u32, 0, 0);
    let before = test_count(Event::MetalBatchSubmissions);
    let oracle = slide.read_tile(&request).unwrap();
    assert_eq!(test_count(Event::MetalBatchSubmissions), before);
    // Warmup and each comparison are on distinct reads. A selected CPU route
    // does no more optional device work after the third complete comparison.
    for index in 1..=4 {
        let actual = slide.read_tile(&request).unwrap();
        assert_eq!(actual.as_u8(), oracle.as_u8());
        assert_eq!(test_count(Event::MetalBatchSubmissions), before + index);
    }
    let runtime = DecodeRuntime::default_arc();
    let identity = runtime.metal_sessions().unwrap().device_identity();
    let key = route_key_for_batch(slide.source(), &[request], &identity).unwrap();
    assert!(runtime.cached_route(&key).is_some());
}

#[cfg(feature = "metal")]
#[test]
fn constrained_admission_skips_calibration_and_strict_native_copying() {
    use crate::core::execution_telemetry::{test_count, Event};
    let bytes = include_bytes!("../../../tests/fixtures/jp2k/rgb_nomct.j2k");
    let info = crate::decode::jp2k_codestream::parse_codestream_header(bytes).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bounded.j2k");
    std::fs::write(&path, bytes).unwrap();
    let ordinary =
        bytes.len() as u64 + u64::from(info.image_width) * u64::from(info.image_height) * 4 * 2 * 2;
    let limits = crate::SlideLimits::default()
        .with_operation_transient_bytes(ordinary)
        .unwrap();
    let slide = crate::Slide::open_with_options(
        &path,
        crate::SlideOpenOptions::default().with_limits(limits),
    )
    .unwrap();
    let request = TileRequest::new(0usize, 0usize, 0u32, 0, 0);
    let requests = [request.clone(), request];
    let before = test_count(Event::MetalBatchSubmissions);
    let oracle = slide.read_tiles(&requests).unwrap();
    assert_eq!(
        slide.read_tiles(&requests).unwrap()[0].as_u8(),
        oracle[0].as_u8()
    );
    assert_eq!(test_count(Event::MetalBatchSubmissions), before);
    let sessions = crate::output::metal::MetalBackendSessions::system_default().unwrap();
    let actual = slide.read_tiles_metal(&requests, &sessions).unwrap();
    assert_eq!(test_count(Event::MetalBatchSubmissions), before);
    for (tile, expected) in actual.iter().zip(&oracle) {
        assert_eq!(tile.download_cpu().unwrap().as_u8(), expected.as_u8());
    }
}

#[test]
fn calibration_binds_a_device_lazily_and_failure_publishes_cpu() {
    let runtime = DecodeRuntime::inline(DecodeExecutionOptions::default());
    let mut key = route_key(911);
    key.device_identity.clear();
    assert!(matches!(
        runtime.claim_route(key.clone()),
        RouteClaim::FirstCpu { .. }
    ));
    let RouteClaim::Calibrate(mut lease) = runtime.claim_route(key.clone()) else {
        panic!("pending warmup")
    };
    let pending = key.clone();
    key.device_identity = "metal:1234:test".into();
    assert!(matches!(runtime.claim_route(key.clone()), RouteClaim::Cpu));
    lease.bind_device(key.device_identity.clone());
    assert!(!runtime.route_cache.lock().unwrap().contains(&pending));
    lease.fail(None).unwrap();
    let decision = runtime.cached_route(&key).unwrap();
    assert_eq!(decision.winner, DecodeRoute::Cpu);
    assert!(decision.device_failure);
}

#[test]
fn concurrent_route_claims_do_not_wait_for_the_calibrating_caller() {
    let runtime = DecodeRuntime::inline(DecodeExecutionOptions::default());
    let key = route_key(912);
    assert!(matches!(
        runtime.claim_route(key.clone()),
        RouteClaim::FirstCpu { .. }
    ));
    std::thread::scope(|scope| {
        let (owned_tx, owned_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let runtime = &runtime;
        let key = &key;
        scope.spawn(move || {
            let RouteClaim::Calibrate(lease) = runtime.claim_route(key.clone()) else {
                panic!("warmup pending")
            };
            owned_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            drop(lease);
        });
        owned_rx.recv().unwrap();
        assert!(matches!(runtime.claim_route(key.clone()), RouteClaim::Cpu));
        release_tx.send(()).unwrap();
    });
    assert!(matches!(runtime.claim_route(key), RouteClaim::Calibrate(_)));
}

#[cfg(feature = "metal")]
struct CancellingPreparation {
    ready: bool,
    source: CountingSource,
    cancellation: crate::ReadCancellationToken,
}

#[cfg(feature = "metal")]
impl SlideReader for CancellingPreparation {
    fn dataset(&self) -> &Dataset {
        self.source.dataset()
    }
    fn tile_codec_kind(&self, req: &TileRequest) -> TileCodecKind {
        self.source.tile_codec_kind(req)
    }
    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        self.source.read_tile_cpu(req)
    }
    fn read_tiles_cpu(&self, reqs: &[TileRequest]) -> Result<Vec<CpuTile>, WsiError> {
        self.source.read_tiles_cpu(reqs)
    }
}

#[cfg(feature = "metal")]
impl ManagedSlideReader for CancellingPreparation {
    fn read_tiles_cpu_fastpath(
        &self,
        reqs: &[TileRequest],
        _: Option<&crate::ReadControl>,
    ) -> Option<Result<Vec<CpuTile>, WsiError>> {
        self.ready
            .then(|| Ok(reqs.iter().map(marker_tile).collect()))
    }
    fn prepare_adaptive_jp2k(
        &self,
        _: &[TileRequest],
        _: usize,
        control: Option<&crate::ReadControl>,
    ) -> Option<Result<crate::decode::jp2k::PreparedJp2kBatch, WsiError>> {
        assert!(
            control.is_some(),
            "optional source preparation must receive cancellation controls"
        );
        self.cancellation.cancel();
        Some(Err(WsiError::Jp2k(
            "source failed during cancellation".into(),
        )))
    }
    fn tile_encoded_upper_bound(&self, _: &TileRequest) -> Result<u64, WsiError> {
        Ok(0)
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

#[cfg(feature = "metal")]
#[test]
fn cancelled_optional_preparation_releases_route_and_memory_ownership() {
    use crate::core::limits::{ReadExecutionContext, SlideAdmission};
    let (source, calls, _) = CountingSource::new(914, TileCodecKind::Jp2k);
    let token = crate::ReadCancellationToken::new();
    let runtime = Arc::new(DecodeRuntime::inline(DecodeExecutionOptions::default()));
    let reader = AdaptiveDecodeReader::new_managed(
        Box::new(CancellingPreparation {
            ready: false,
            source,
            cancellation: token.clone(),
        }),
        runtime.clone(),
    );
    let admission = SlideAdmission::new(2 * 1024 * 1024);
    let ordinary = admission.reserve(512 * 1024, None).unwrap();
    let control = crate::ReadControl::new(token);
    let calibration = std::sync::atomic::AtomicBool::new(false);
    let context =
        ReadExecutionContext::new(&ordinary, 2 * 1024 * 1024, Some(&control), &calibration);
    let reqs = [TileRequest::new(0usize, 0usize, 0u32, 0, 0)];
    reader.read_tiles_with_context(&reqs, &context).unwrap();
    // A fresh public read permits the pending preparation to begin.
    let calibration = std::sync::atomic::AtomicBool::new(false);
    let context =
        ReadExecutionContext::new(&ordinary, 2 * 1024 * 1024, Some(&control), &calibration);
    assert!(matches!(
        reader.read_tiles_with_context(&reqs, &context),
        Err(WsiError::Cancelled)
    ));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "no CPU fallback after cancellation"
    );
    assert!(runtime.metal_sessions.get().is_none());
    let key = route_key_for_batch(&reader, &reqs, "").unwrap();
    let RouteClaim::Calibrate(lease) = runtime.claim_route(key) else {
        panic!("calibration ownership leaked")
    };
    assert_eq!(lease.step, CalibrationStep::Warmup);
    let active = ReadExecutionContext::new(&ordinary, 2 * 1024 * 1024, None, &calibration);
    assert!(
        active.try_extra(1536 * 1024).unwrap().is_some(),
        "optional memory reservation leaked"
    );
}

#[test]
fn the_initial_cpu_read_owns_the_pending_route_until_completion() {
    let runtime = DecodeRuntime::inline(DecodeExecutionOptions::default());
    let key = route_key(9901);
    let initial = runtime.claim_route(key.clone());
    assert!(
        matches!(runtime.claim_route(key.clone()), RouteClaim::Cpu),
        "warmup must not race the route's first CPU read"
    );
    drop(initial);
    let RouteClaim::Calibrate(warmup) = runtime.claim_route(key) else {
        panic!("a later read must be able to warm the device");
    };
    assert_eq!(warmup.step, CalibrationStep::Warmup);
}

#[cfg(any(feature = "metal", feature = "cuda"))]
#[test]
fn automatic_native_reads_execute_the_bounded_batches_they_calibrate() {
    for (codec, side, bounded_count) in [
        (TileCodecKind::Jp2k, 256, 16),
        (TileCodecKind::Jp2k, 512, 4),
        (TileCodecKind::Jpeg, 256, 64),
    ] {
        for acceleration in [DecodeAcceleration::CpuOnly, DecodeAcceleration::Auto] {
            let (mut source, _, _) = CountingSource::new(9902, codec);
            source.dataset = regular_rgb_dataset_for_test(
                DatasetId::new(9902),
                "scene",
                "series",
                RegularLevelForTest {
                    dimensions: (u64::from(side) * 6, u64::from(side) * 4),
                    tile_width: side,
                    tile_height: side,
                    tiles_across: 6,
                    tiles_down: 4,
                },
            );
            let sizes = source.batch_sizes.clone();
            let slide = crate::Slide::from_source_with_config_and_runtime(
                Box::new(source),
                crate::CacheConfig::default(),
                crate::SlideLimits::default()
                    .with_encoded_unit_bytes(1)
                    .unwrap(),
                Arc::new(DecodeRuntime::inline(
                    DecodeExecutionOptions::default().with_acceleration(acceleration),
                )),
            );
            let requests = (0..64)
                .map(|i| TileRequest::new(0, 0, 0, i % 6, (i / 6) % 4))
                .collect::<Vec<_>>();
            let tiles = slide.read_tiles(&requests).unwrap();
            assert_eq!(tiles.len(), requests.len());
            for (tile, request) in tiles.iter().zip(&requests) {
                assert_eq!(tile.as_u8(), marker_tile(request).as_u8());
            }
            assert_eq!(
                *sizes.lock().unwrap(),
                if acceleration == DecodeAcceleration::Auto {
                    vec![bounded_count; 64 / bounded_count]
                } else {
                    vec![64]
                }
            );
        }
    }
}

#[cfg(feature = "metal")]
#[test]
fn ready_device_routes_reuse_cached_pixels_but_calibration_bypasses_them() {
    use crate::core::limits::{ReadExecutionContext, SlideAdmission};
    let (source, calls, _) = CountingSource::new(9903, TileCodecKind::Jp2k);
    let reqs = [TileRequest::new(0, 0, 0, 0, 0)];
    let key = route_key_for_batch(&source, &reqs, "").unwrap();
    let runtime = Arc::new(DecodeRuntime::inline(DecodeExecutionOptions::default()));
    runtime
        .store_route(
            key.clone(),
            DecodeRouteDecision::measured(Duration::from_millis(100), Duration::from_millis(1)),
            None,
        )
        .unwrap();
    let token = crate::ReadCancellationToken::new();
    let reader = AdaptiveDecodeReader::new_managed(
        Box::new(CancellingPreparation {
            ready: true,
            source,
            cancellation: token.clone(),
        }),
        runtime.clone(),
    );
    let admission = SlideAdmission::new(2 * 1024 * 1024);
    let ordinary = admission.reserve(512 * 1024, None).unwrap();
    let control = crate::ReadControl::new(token);
    let calibration = std::sync::atomic::AtomicBool::new(false);
    let context =
        ReadExecutionContext::new(&ordinary, 2 * 1024 * 1024, Some(&control), &calibration);
    let cached = reader
        .read_tiles_with_context(&reqs, &context)
        .expect("ready pixels must precede device preparation");
    assert_eq!(cached[0].as_u8(), marker_tile(&reqs[0]).as_u8());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(runtime.metal_sessions.get().is_none());
    control.check_cancelled().unwrap();

    runtime.route_cache.lock().unwrap().pop(&key);
    reader.read_tiles_with_context(&reqs, &context).unwrap();
    let calibration = std::sync::atomic::AtomicBool::new(false);
    let context =
        ReadExecutionContext::new(&ordinary, 2 * 1024 * 1024, Some(&control), &calibration);
    assert!(
        matches!(
            reader.read_tiles_with_context(&reqs, &context),
            Err(WsiError::Cancelled)
        ),
        "pending calibration must still prepare the route despite cached output"
    );
}
