use super::*;
use crate::core::types::*;
use crate::test_support::{regular_rgb_dataset_for_test, RegularLevelForTest};
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
