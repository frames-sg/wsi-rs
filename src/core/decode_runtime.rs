use crate::core::registry::SlideReader;
use crate::core::types::{
    CpuTile, Dataset, Level, OutputBackendRequest, TileCodecKind, TileLayout, TileOutputPreference,
    TilePixels, TileRequest,
};
use crate::error::WsiError;
use rayon::ThreadPool;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const DEFAULT_ROUTE_SAMPLE_SIZE: usize = 32;
const DIRECT_DEVICE_BATCH_THRESHOLD: usize = 8;
const DEVICE_WIN_RATIO: f64 = 0.85;
const ROUTE_CACHE_MAX_ENTRIES: usize = 1024;

thread_local! {
    static CURRENT_DECODE_RUNTIME: RefCell<Option<Arc<DecodeRuntime>>> = const { RefCell::new(None) };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct DecodeExecutionOptions {
    jp2k_cpu_threads: Option<NonZeroUsize>,
    route_sample_size: usize,
}

impl DecodeExecutionOptions {
    pub fn with_jp2k_cpu_threads(mut self, threads: NonZeroUsize) -> Self {
        self.jp2k_cpu_threads = Some(threads);
        self
    }

    pub fn with_route_sample_size(mut self, sample_size: usize) -> Self {
        self.route_sample_size = sample_size.max(1);
        self
    }

    pub fn jp2k_cpu_threads(&self) -> Option<NonZeroUsize> {
        self.jp2k_cpu_threads
    }

    pub fn route_sample_size(&self) -> usize {
        self.route_sample_size
    }
}

impl Default for DecodeExecutionOptions {
    fn default() -> Self {
        Self {
            jp2k_cpu_threads: None,
            route_sample_size: DEFAULT_ROUTE_SAMPLE_SIZE,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DecodeRoute {
    Cpu,
    Device,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct DecodeRouteDecision {
    pub winner: DecodeRoute,
    pub sample_tile_count: usize,
    pub cpu_elapsed: Duration,
    pub device_elapsed: Duration,
    pub device_tile_count: usize,
}

impl DecodeRouteDecision {
    pub fn measured(
        sample_tile_count: usize,
        cpu_elapsed: Duration,
        device_elapsed: Duration,
        device_tile_count: usize,
    ) -> Self {
        Self {
            winner: Self::winner_for_measurement(cpu_elapsed, device_elapsed, device_tile_count),
            sample_tile_count,
            cpu_elapsed,
            device_elapsed,
            device_tile_count,
        }
    }

    pub fn winner_for_measurement(
        cpu_elapsed: Duration,
        device_elapsed: Duration,
        device_tile_count: usize,
    ) -> DecodeRoute {
        let cpu_ms = cpu_elapsed.as_secs_f64() * 1000.0;
        let device_ms = device_elapsed.as_secs_f64() * 1000.0;
        if device_tile_count > 0 && cpu_ms > 0.0 && device_ms <= cpu_ms * DEVICE_WIN_RATIO {
            DecodeRoute::Device
        } else {
            DecodeRoute::Cpu
        }
    }
}

struct MeasuredDecodeRoute {
    decision: DecodeRouteDecision,
    sample_tiles: Vec<TilePixels>,
}

#[derive(Debug)]
pub(crate) struct DecodeRuntime {
    options: DecodeExecutionOptions,
    jp2k_cpu_pool: Option<ThreadPool>,
    route_cache: Mutex<DecodeRouteCache>,
}

impl DecodeRuntime {
    pub(crate) fn new(options: DecodeExecutionOptions) -> Result<Self, WsiError> {
        Self::build(options, true)
    }

    pub(crate) fn arc_for_options(options: DecodeExecutionOptions) -> Result<Arc<Self>, WsiError> {
        if options == DecodeExecutionOptions::default() {
            Ok(Self::default_arc())
        } else {
            Ok(Arc::new(Self::new(options)?))
        }
    }

    fn build(options: DecodeExecutionOptions, fail_on_pool_error: bool) -> Result<Self, WsiError> {
        let threads = options
            .jp2k_cpu_threads
            .map_or_else(default_jp2k_cpu_threads, NonZeroUsize::get);
        let jp2k_cpu_pool = match rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|index| format!("wsi_rs-jp2k-cpu-{index}"))
            .build()
        {
            Ok(pool) => Some(pool),
            Err(err) if fail_on_pool_error => {
                return Err(WsiError::Unsupported {
                    reason: format!("failed to initialize JP2K CPU decode pool: {err}"),
                });
            }
            Err(err) => {
                tracing::error!(
                    error = %err,
                    "failed to initialize default JP2K CPU decode pool; falling back to inline decode"
                );
                None
            }
        };
        Ok(Self {
            options,
            jp2k_cpu_pool,
            route_cache: Mutex::new(DecodeRouteCache::new()),
        })
    }

    pub(crate) fn default_arc() -> Arc<Self> {
        static DEFAULT_RUNTIME: OnceLock<Arc<DecodeRuntime>> = OnceLock::new();
        DEFAULT_RUNTIME
            .get_or_init(|| {
                Arc::new(match Self::build(DecodeExecutionOptions::default(), false) {
                    Ok(runtime) => runtime,
                    Err(err) => {
                        tracing::error!(
                            error = %err,
                            "failed to initialize default decode runtime; falling back to inline decode"
                        );
                        Self::inline(DecodeExecutionOptions::default())
                    }
                })
            })
            .clone()
    }

    fn inline(options: DecodeExecutionOptions) -> Self {
        Self {
            options,
            jp2k_cpu_pool: None,
            route_cache: Mutex::new(DecodeRouteCache::new()),
        }
    }

    pub(crate) fn install_jp2k_cpu<R: Send>(&self, op: impl FnOnce() -> R + Send) -> R {
        if let Some(pool) = &self.jp2k_cpu_pool {
            pool.install(op)
        } else {
            op()
        }
    }

    pub(crate) fn has_jp2k_cpu_pool(&self) -> bool {
        self.jp2k_cpu_pool.is_some()
    }

    pub(crate) fn options(&self) -> DecodeExecutionOptions {
        self.options
    }

    pub(crate) fn with_current<T>(self: &Arc<Self>, f: impl FnOnce() -> T) -> T {
        struct Restore(Option<Arc<DecodeRuntime>>);
        impl Drop for Restore {
            fn drop(&mut self) {
                let previous = self.0.take();
                CURRENT_DECODE_RUNTIME.with(|slot| {
                    *slot.borrow_mut() = previous;
                });
            }
        }

        let previous = CURRENT_DECODE_RUNTIME.with(|slot| slot.replace(Some(self.clone())));
        let _restore = Restore(previous);
        f()
    }

    fn cached_route(&self, key: &DecodeRouteKey) -> Option<DecodeRouteDecision> {
        self.route_cache
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .get(key)
    }

    fn store_route(&self, key: DecodeRouteKey, decision: DecodeRouteDecision) {
        self.route_cache
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .insert(key, decision);
    }
}

#[derive(Debug)]
struct DecodeRouteCache {
    entries: HashMap<DecodeRouteKey, DecodeRouteDecision>,
    insertion_order: VecDeque<DecodeRouteKey>,
}

impl DecodeRouteCache {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            insertion_order: VecDeque::new(),
        }
    }

    fn get(&self, key: &DecodeRouteKey) -> Option<DecodeRouteDecision> {
        self.entries.get(key).cloned()
    }

    fn insert(&mut self, key: DecodeRouteKey, decision: DecodeRouteDecision) {
        if !self.entries.contains_key(&key) {
            while self.entries.len() >= ROUTE_CACHE_MAX_ENTRIES {
                let Some(evicted) = self.insertion_order.pop_front() else {
                    break;
                };
                self.entries.remove(&evicted);
            }
            self.insertion_order.push_back(key.clone());
        }
        self.entries.insert(key, decision);
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

pub(crate) fn current_decode_runtime() -> Option<Arc<DecodeRuntime>> {
    CURRENT_DECODE_RUNTIME.with(|slot| slot.borrow().clone())
}

fn default_jp2k_cpu_threads() -> usize {
    std::thread::available_parallelism()
        .map_or(1, NonZeroUsize::get)
        .max(1)
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct DecodeRouteKey {
    dataset_id: u128,
    scene: usize,
    series: usize,
    level: u32,
    tile_grid: RouteTileGrid,
    codec_kind: TileCodecKind,
    output_backend: OutputBackendRequest,
    device_backend_identity: String,
    sample_tile_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct RouteTileGrid {
    tile_width: u32,
    tile_height: u32,
    tiles_across: u64,
    tiles_down: u64,
}

pub(crate) struct AdaptiveDecodeReader {
    inner: Box<dyn SlideReader>,
    runtime: Arc<DecodeRuntime>,
}

impl AdaptiveDecodeReader {
    pub(crate) fn new(inner: Box<dyn SlideReader>, runtime: Arc<DecodeRuntime>) -> Self {
        Self { inner, runtime }
    }

    fn read_tiles_adaptive(
        &self,
        reqs: &[TileRequest],
        output: TileOutputPreference,
    ) -> Result<Vec<TilePixels>, WsiError> {
        if !should_adapt_output(&output) {
            tracing::debug!(
                requested_tiles = reqs.len(),
                adaptive_decode = false,
                "wsi tile batch routed without adaptive decode"
            );
            return self
                .runtime
                .with_current(|| self.inner.read_tiles(reqs, output));
        }
        let route_sample_size = self.runtime.options.route_sample_size();
        let Some(key) = route_key_for_batch(self.inner.as_ref(), reqs, &output, route_sample_size)
        else {
            tracing::debug!(
                requested_tiles = reqs.len(),
                route_sample_size,
                adaptive_decode = true,
                route_key_available = false,
                "wsi adaptive decode fell back to requested output"
            );
            return self
                .runtime
                .with_current(|| self.inner.read_tiles(reqs, output));
        };
        if reqs.len() >= DIRECT_DEVICE_BATCH_THRESHOLD {
            tracing::debug!(
                requested_tiles = reqs.len(),
                route_sample_size,
                direct_device_batch_threshold = DIRECT_DEVICE_BATCH_THRESHOLD,
                adaptive_decode = true,
                route_key_available = true,
                "wsi adaptive decode sent large batch through requested output"
            );
            return self
                .runtime
                .with_current(|| self.inner.read_tiles(reqs, output));
        }
        let (route, measured) = match self.runtime.cached_route(&key) {
            Some(decision) => {
                tracing::debug!(
                    requested_tiles = reqs.len(),
                    route_sample_size,
                    route_cache_hit = true,
                    route = ?decision.winner,
                    sample_tile_count = decision.sample_tile_count,
                    cpu_elapsed_ms = decision.cpu_elapsed.as_secs_f64() * 1000.0,
                    device_elapsed_ms = decision.device_elapsed.as_secs_f64() * 1000.0,
                    device_tile_count = decision.device_tile_count,
                    "wsi adaptive decode reused cached route"
                );
                (decision.winner, None)
            }
            None => {
                let measured = self.measure_route(reqs, output.clone())?;
                let winner = measured.decision.winner;
                tracing::debug!(
                    requested_tiles = reqs.len(),
                    route_sample_size,
                    route_cache_hit = false,
                    route = ?winner,
                    sample_tile_count = measured.decision.sample_tile_count,
                    cpu_elapsed_ms = measured.decision.cpu_elapsed.as_secs_f64() * 1000.0,
                    device_elapsed_ms = measured.decision.device_elapsed.as_secs_f64() * 1000.0,
                    device_tile_count = measured.decision.device_tile_count,
                    "wsi adaptive decode measured route"
                );
                self.runtime.store_route(key, measured.decision.clone());
                (winner, Some(measured.sample_tiles))
            }
        };
        let routed_output = match route {
            DecodeRoute::Cpu => TileOutputPreference::cpu(),
            DecodeRoute::Device => output,
        };
        if let Some(mut measured) = measured {
            let sample_len = reqs.len().min(self.runtime.options.route_sample_size());
            if measured.len() == sample_len {
                if sample_len == reqs.len() {
                    return Ok(measured);
                }
                let mut rest = self
                    .runtime
                    .with_current(|| self.inner.read_tiles(&reqs[sample_len..], routed_output))?;
                measured.append(&mut rest);
                return Ok(measured);
            }
        }
        self.runtime
            .with_current(|| self.inner.read_tiles(reqs, routed_output))
    }

    fn measure_route(
        &self,
        reqs: &[TileRequest],
        device_output: TileOutputPreference,
    ) -> Result<MeasuredDecodeRoute, WsiError> {
        let sample_len = reqs.len().min(self.runtime.options.route_sample_size());
        let sample = &reqs[..sample_len];

        let device_started = Instant::now();
        let device_result = self
            .runtime
            .with_current(|| self.inner.read_tiles(sample, device_output));
        let device_elapsed = device_started.elapsed();
        let device_tile_count = device_result
            .as_ref()
            .map(|tiles| {
                tiles
                    .iter()
                    .filter(|tile| matches!(tile, TilePixels::Device(_)))
                    .count()
            })
            .unwrap_or(0);
        let device_result = match device_result {
            Ok(device_tiles) if device_tile_count == 0 => {
                return Ok(MeasuredDecodeRoute {
                    decision: DecodeRouteDecision::measured(
                        device_tiles.len(),
                        device_elapsed,
                        device_elapsed,
                        device_tile_count,
                    ),
                    sample_tiles: device_tiles,
                });
            }
            other => other,
        };

        let cpu_started = Instant::now();
        let cpu_tiles = self
            .runtime
            .with_current(|| self.inner.read_tiles(sample, TileOutputPreference::cpu()))?;
        let cpu_elapsed = cpu_started.elapsed();

        let decision = DecodeRouteDecision::measured(
            cpu_tiles.len(),
            cpu_elapsed,
            device_elapsed,
            device_tile_count,
        );
        let sample_tiles = match decision.winner {
            DecodeRoute::Cpu => cpu_tiles,
            DecodeRoute::Device => device_result?,
        };

        Ok(MeasuredDecodeRoute {
            decision,
            sample_tiles,
        })
    }
}

impl SlideReader for AdaptiveDecodeReader {
    fn dataset(&self) -> &Dataset {
        self.inner.dataset()
    }

    fn tile_codec_kind(&self, req: &TileRequest) -> TileCodecKind {
        self.inner.tile_codec_kind(req)
    }

    fn level_source_kind(
        &self,
        scene: crate::core::types::SceneId,
        series: crate::core::types::SeriesId,
        level: crate::core::types::LevelIdx,
    ) -> Result<crate::core::types::LevelSourceKind, WsiError> {
        self.inner.level_source_kind(scene, series, level)
    }

    fn read_tiles(
        &self,
        reqs: &[TileRequest],
        output: TileOutputPreference,
    ) -> Result<Vec<TilePixels>, WsiError> {
        self.read_tiles_adaptive(reqs, output)
    }

    fn read_tiles_controlled(
        &self,
        reqs: &[TileRequest],
        output: TileOutputPreference,
        control: &crate::ReadControl,
    ) -> Result<Vec<TilePixels>, WsiError> {
        control.check_cancelled()?;
        let tiles = self
            .runtime
            .with_current(|| self.inner.read_tiles_controlled(reqs, output, control))?;
        control.check_cancelled()?;
        Ok(tiles)
    }

    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        self.runtime.with_current(|| self.inner.read_tile_cpu(req))
    }

    fn read_raw_compressed_tile(
        &self,
        req: &TileRequest,
    ) -> Result<crate::core::types::RawCompressedTile, WsiError> {
        self.inner.read_raw_compressed_tile(req)
    }

    fn read_raw_compressed_display_tile(
        &self,
        req: &crate::core::types::TileViewRequest,
    ) -> Result<crate::core::types::RawCompressedTile, WsiError> {
        self.inner.read_raw_compressed_display_tile(req)
    }

    fn read_tiles_cpu(&self, reqs: &[TileRequest]) -> Result<Vec<CpuTile>, WsiError> {
        self.runtime
            .with_current(|| self.inner.read_tiles_cpu(reqs))
    }

    fn use_display_tile_cache(&self, req: &crate::core::types::TileViewRequest) -> bool {
        self.inner.use_display_tile_cache(req)
    }

    fn read_region_fastpath(
        &self,
        ctx: &mut crate::core::registry::SlideReadContext<'_>,
        req: &crate::core::types::RegionRequest,
    ) -> Option<Result<CpuTile, WsiError>> {
        self.runtime
            .with_current(|| self.inner.read_region_fastpath(ctx, req))
    }

    fn read_region(
        &self,
        req: &crate::core::types::RegionRequest,
        output: TileOutputPreference,
    ) -> Result<TilePixels, WsiError> {
        self.runtime
            .with_current(|| self.inner.read_region(req, output))
    }

    fn read_display_tile(
        &self,
        req: &crate::core::types::TileViewRequest,
    ) -> Result<CpuTile, WsiError> {
        self.runtime
            .with_current(|| self.inner.read_display_tile(req))
    }

    fn read_associated(&self, name: &str) -> Result<CpuTile, WsiError> {
        self.inner.read_associated(name)
    }

    fn recommended_shared_cache_bytes(&self) -> Option<u64> {
        self.inner.recommended_shared_cache_bytes()
    }
}

fn should_adapt_output(output: &TileOutputPreference) -> bool {
    matches!(output, TileOutputPreference::PreferDevice { .. })
        && output.compressed_device_decode_enabled()
        && output.adaptive_decode_route_enabled()
}

fn route_key_for_batch(
    reader: &dyn SlideReader,
    reqs: &[TileRequest],
    output: &TileOutputPreference,
    route_sample_size: usize,
) -> Option<DecodeRouteKey> {
    let first = reqs.first()?;
    if !reqs.iter().all(|req| {
        req.scene == first.scene && req.series == first.series && req.level == first.level
    }) {
        return None;
    }
    let codec_kind = reader.tile_codec_kind(first);
    if !matches!(codec_kind, TileCodecKind::Jp2k | TileCodecKind::Htj2k) {
        return None;
    }
    if !reqs
        .iter()
        .all(|req| reader.tile_codec_kind(req) == codec_kind)
    {
        return None;
    }
    let level = dataset_level(
        reader.dataset(),
        first.scene.get(),
        first.series.get(),
        first.level.get(),
    )?;
    let tile_grid = route_tile_grid(level)?;
    Some(DecodeRouteKey {
        dataset_id: reader.dataset().id.0,
        scene: first.scene.get(),
        series: first.series.get(),
        level: first.level.get(),
        tile_grid,
        codec_kind,
        output_backend: output.backend(),
        device_backend_identity: device_backend_identity(output),
        sample_tile_count: reqs.len().min(route_sample_size.max(1)),
    })
}

fn dataset_level(dataset: &Dataset, scene: usize, series: usize, level: u32) -> Option<&Level> {
    dataset
        .scenes
        .get(scene)?
        .series
        .get(series)?
        .levels
        .get(level as usize)
}

fn route_tile_grid(level: &Level) -> Option<RouteTileGrid> {
    match &level.tile_layout {
        TileLayout::Regular {
            tile_width,
            tile_height,
            tiles_across,
            tiles_down,
        } => Some(RouteTileGrid {
            tile_width: *tile_width,
            tile_height: *tile_height,
            tiles_across: *tiles_across,
            tiles_down: *tiles_down,
        }),
        _ => None,
    }
}

fn device_backend_identity(output: &TileOutputPreference) -> String {
    #[cfg(feature = "metal")]
    if let Some(metal) = output.metal_sessions() {
        return format!("{:?}:{}", output.backend(), metal.device_identity());
    }
    #[cfg(feature = "cuda")]
    if let Some(cuda) = output.cuda_sessions() {
        return format!("{:?}:{}", output.backend(), cuda.device_identity());
    }
    format!("{:?}", output.backend())
}

#[cfg(test)]
#[path = "decode_runtime/tests.rs"]
mod tests;
