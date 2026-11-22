use super::*;
use crate::core::registry::ConservativeManagedReader;
use crate::core::types::*;
use crate::test_support::{regular_rgb_dataset_for_test, RegularLevelForTest};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

impl DecodeRuntime {
    pub(crate) fn new(options: DecodeExecutionOptions) -> Result<Self, WsiError> {
        Ok(Self::build(options))
    }

    pub(crate) fn inline(options: DecodeExecutionOptions) -> Self {
        Self::build(options)
    }
}

impl DecodeRuntimeReader {
    pub(crate) fn new(inner: Box<dyn SlideReader>, runtime: Arc<DecodeRuntime>) -> Self {
        Self::new_managed(
            Box::new(ConservativeManagedReader::new(
                inner,
                crate::SlideLimits::default().encoded_unit_bytes(),
            )),
            runtime,
        )
    }
}

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
        .install_codec_cpu(rayon::current_thread_index)
        .is_some());
}

#[test]
fn admitted_codec_work_on_a_foreign_worker_moves_to_the_wsi_pool_without_stealing() {
    use crate::core::limits::SlideAdmission;
    use std::sync::atomic::AtomicBool;

    let admission = SlideAdmission::new(1024);
    // One worker: a job spawned on this pool can only run on the caller.
    let foreign = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    let queued_job_ran = Arc::new(AtomicBool::new(false));
    let (on_wsi_pool, ran_while_waiting) = foreign.install(|| {
        let reservation = admission.reserve(512, None).unwrap();
        let ran = Arc::clone(&queued_job_ran);
        rayon::spawn(move || ran.store(true, Ordering::SeqCst));
        let observed = crate::core::batch::install_codec_cpu(|| {
            // Give a stealing wait time to pick up the queued job.
            std::thread::sleep(std::time::Duration::from_millis(50));
            (
                crate::core::batch::process_codec_cpu_pool()
                    .is_some_and(|pool| pool.current_thread_index().is_some()),
                queued_job_ran.load(Ordering::SeqCst),
            )
        });
        drop(reservation);
        observed
    });

    assert!(
        on_wsi_pool,
        "admitted codec work must leave the caller's pool"
    );
    assert!(
        !ran_while_waiting,
        "the admitted caller must not run its pool's jobs while it waits"
    );
}

#[test]
fn codec_work_on_a_worker_without_admission_stays_on_that_worker() {
    let foreign = rayon::ThreadPoolBuilder::new()
        .num_threads(2)
        .build()
        .unwrap();
    let invoking = foreign.install(|| {
        let caller = std::thread::current().id();
        crate::core::batch::install_codec_cpu(|| std::thread::current().id() == caller)
    });
    assert!(invoking);
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
    let reader = DecodeRuntimeReader::new(Box::new(source), runtime);
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
    let reader = DecodeRuntimeReader::new(Box::new(source), runtime);
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
fn runtime_reader_preserves_non_tile_boundaries() {
    let reader = DecodeRuntimeReader::new(
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

/// GPU decodes made on this thread: Metal batch submissions, Metal single
/// decodes and CUDA jobs, for the features compiled in.
fn device_decodes() -> u64 {
    #[allow(unused_mut)]
    let mut total = 0;
    #[cfg(feature = "metal")]
    {
        use crate::core::execution_telemetry::{test_count, Event};
        total += test_count(Event::MetalBatchSubmissions) + test_count(Event::MetalSingleDecodes);
    }
    #[cfg(feature = "cuda")]
    {
        use crate::core::execution_telemetry::{test_count, Event};
        total += test_count(Event::CudaDecodeJobs);
    }
    total
}

#[test]
fn ordinary_jpeg_2000_reads_decode_on_the_cpu_under_every_policy() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rgb.j2k");
    std::fs::write(
        &path,
        include_bytes!("../../../tests/fixtures/jp2k/rgb_nomct.j2k"),
    )
    .unwrap();
    let open = |acceleration| {
        crate::Slide::open_with_options_and_runtime(
            &path,
            crate::SlideOpenOptions::default()
                .with_cache_config(crate::CacheConfig::default().with_shared_tile_bytes(0)),
            Arc::new(DecodeRuntime::inline(
                DecodeExecutionOptions::default().with_acceleration(acceleration),
            )),
        )
        .unwrap()
    };
    let tile = TileRequest::new(0usize, 0usize, 0u32, 0, 0);
    let region = RegionRequest::new(0usize, 0usize, 0u32, (1, 1), (8, 6));
    let read = |slide: &crate::Slide| {
        let mut tiles = slide.read_tiles(&[tile.clone(), tile.clone()]).unwrap();
        tiles.push(slide.read_tile(&tile).unwrap());
        tiles.push(slide.read_region(&region).unwrap());
        tiles
    };
    let oracle = read(&open(DecodeAcceleration::CpuOnly));
    let slide = open(DecodeAcceleration::Auto);
    for call in 0..2 {
        let before = device_decodes();
        let actual = read(&slide);
        assert_eq!(device_decodes(), before, "call={call}");
        assert_eq!(actual.len(), oracle.len());
        for (actual, oracle) in actual.iter().zip(&oracle) {
            assert_eq!(actual.as_u8(), oracle.as_u8(), "call={call}");
        }
    }
}

#[cfg(feature = "route-telemetry")]
#[test]
fn route_telemetry_exports_execution_counters_including_device_decodes() {
    let telemetry: serde_json::Value =
        serde_json::from_str(&super::decode_route_telemetry_json()).unwrap();
    for key in [
        "cpu_jp2k_tiles",
        "metal_batch_submissions",
        "metal_single_decodes",
        "cuda_decode_jobs",
    ] {
        assert!(telemetry["execution"][key].is_u64(), "execution.{key}");
    }
    assert_eq!(telemetry.as_object().unwrap().len(), 1);
}
