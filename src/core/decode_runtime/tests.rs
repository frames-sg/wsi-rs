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
        sample_tile_count: ROUTE_SAMPLE_SIZE,
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
fn route_keys_include_geometry_codec_and_internal_batch_bucket() {
    let (source, _, _) = CountingSource::new(73, TileCodecKind::Htj2k);
    let requests = (0..12)
        .map(|index| TileRequest::new(0usize, 0usize, 0u32, index % 6, index / 6))
        .collect::<Vec<_>>();

    let key = route_key_for_batch(&source, &requests, "test-device").unwrap();

    assert_eq!(key.codec_kind, TileCodecKind::Htj2k);
    assert_eq!(key.sample_tile_count, ROUTE_SAMPLE_SIZE);
    assert_eq!(
        key.sample_geometry,
        RouteSampleGeometry::from_dimensions([(256, 256); ROUTE_SAMPLE_SIZE])
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
