#[cfg(test)]
use crate::core::registry::ConservativeManagedReader;
use crate::core::registry::{ManagedSlideReader, SlideReader};
use crate::core::types::{CpuTile, Dataset, TileCodecKind, TileRequest};
#[cfg(any(test, feature = "metal", feature = "cuda"))]
use crate::core::types::{Level, TileLayout};
use crate::error::WsiError;
#[cfg(any(test, feature = "metal", feature = "cuda"))]
use lru::LruCache;
use rayon::ThreadPool;
#[cfg(any(test, feature = "metal", feature = "cuda"))]
use std::num::NonZeroUsize;
#[cfg(feature = "route-telemetry")]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(any(test, feature = "metal", feature = "cuda"))]
use std::sync::Mutex;
use std::sync::{Arc, OnceLock};
#[cfg(any(test, feature = "metal", feature = "cuda"))]
use std::time::Duration;
#[cfg(any(feature = "metal", feature = "cuda"))]
use std::time::Instant;

#[cfg(any(test, feature = "metal", feature = "cuda"))]
const ROUTE_SAMPLE_SIZE: usize = 8;
#[cfg(any(test, feature = "metal", feature = "cuda"))]
const DEVICE_WIN_RATIO: f64 = 0.85;
#[cfg(any(test, feature = "metal", feature = "cuda"))]
const ROUTE_CACHE_MAX_ENTRIES: usize = 1024;

#[cfg(feature = "route-telemetry")]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct RouteBackendTelemetry {
    device_attempt_tiles: u64,
    device_tiles: u64,
    adaptive_cpu_tiles: u64,
    fallback_tiles: u64,
    device_failure_fallback_tiles: u64,
    unavailable_fallback_tiles: u64,
}

#[cfg(feature = "route-telemetry")]
struct DecodeRouteTelemetryCounters {
    device_attempt_tiles: AtomicU64,
    device_tiles: AtomicU64,
    adaptive_cpu_tiles: AtomicU64,
    fallback_tiles: AtomicU64,
    device_failure_fallback_tiles: AtomicU64,
    unavailable_fallback_tiles: AtomicU64,
}

#[cfg(feature = "route-telemetry")]
impl DecodeRouteTelemetryCounters {
    const fn new() -> Self {
        Self {
            device_attempt_tiles: AtomicU64::new(0),
            device_tiles: AtomicU64::new(0),
            adaptive_cpu_tiles: AtomicU64::new(0),
            fallback_tiles: AtomicU64::new(0),
            device_failure_fallback_tiles: AtomicU64::new(0),
            unavailable_fallback_tiles: AtomicU64::new(0),
        }
    }

    fn snapshot(&self) -> RouteBackendTelemetry {
        RouteBackendTelemetry {
            device_attempt_tiles: self.device_attempt_tiles.load(Ordering::Relaxed),
            device_tiles: self.device_tiles.load(Ordering::Relaxed),
            adaptive_cpu_tiles: self.adaptive_cpu_tiles.load(Ordering::Relaxed),
            fallback_tiles: self.fallback_tiles.load(Ordering::Relaxed),
            device_failure_fallback_tiles: self
                .device_failure_fallback_tiles
                .load(Ordering::Relaxed),
            unavailable_fallback_tiles: self.unavailable_fallback_tiles.load(Ordering::Relaxed),
        }
    }
}

#[cfg(feature = "route-telemetry")]
static METAL_ROUTE_TELEMETRY: DecodeRouteTelemetryCounters = DecodeRouteTelemetryCounters::new();
#[cfg(feature = "route-telemetry")]
static CUDA_ROUTE_TELEMETRY: DecodeRouteTelemetryCounters = DecodeRouteTelemetryCounters::new();

/// Serialize cumulative process-wide adaptive JP2K route counters.
///
/// This benchmark-only surface is intentionally JSON rather than public route
/// types so normal consumers do not acquire another compatibility API.
#[cfg(feature = "route-telemetry")]
#[doc(hidden)]
pub fn decode_route_telemetry_json() -> String {
    let metal = METAL_ROUTE_TELEMETRY.snapshot();
    let cuda = CUDA_ROUTE_TELEMETRY.snapshot();
    serde_json::json!({
        "metal": {
            "device_attempt_tiles": metal.device_attempt_tiles,
            "device_tiles": metal.device_tiles,
            "adaptive_cpu_tiles": metal.adaptive_cpu_tiles,
            "fallback_tiles": metal.fallback_tiles,
            "device_failure_fallback_tiles": metal.device_failure_fallback_tiles,
            "unavailable_fallback_tiles": metal.unavailable_fallback_tiles,
        },
        "cuda": {
            "device_attempt_tiles": cuda.device_attempt_tiles,
            "device_tiles": cuda.device_tiles,
            "adaptive_cpu_tiles": cuda.adaptive_cpu_tiles,
            "fallback_tiles": cuda.fallback_tiles,
            "device_failure_fallback_tiles": cuda.device_failure_fallback_tiles,
            "unavailable_fallback_tiles": cuda.unavailable_fallback_tiles,
        }
    })
    .to_string()
}

/// Controls whether CPU-returning reads may use an available JP2K device path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DecodeAcceleration {
    /// Measure CPU against an available Metal or CUDA path, including readback.
    Auto,
    /// Decode entirely on the CPU.
    CpuOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct DecodeExecutionOptions {
    acceleration: DecodeAcceleration,
}

impl DecodeExecutionOptions {
    pub fn with_acceleration(mut self, acceleration: DecodeAcceleration) -> Self {
        self.acceleration = acceleration;
        self
    }

    pub fn acceleration(&self) -> DecodeAcceleration {
        self.acceleration
    }
}

impl Default for DecodeExecutionOptions {
    fn default() -> Self {
        Self {
            acceleration: DecodeAcceleration::Auto,
        }
    }
}

#[cfg(any(test, feature = "metal", feature = "cuda"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DecodeRoute {
    Cpu,
    Device,
}

#[cfg(any(test, feature = "metal", feature = "cuda"))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct DecodeRouteDecision {
    winner: DecodeRoute,
    cpu_elapsed: Duration,
    device_elapsed: Duration,
    device_failure: bool,
}

#[cfg(any(test, feature = "metal", feature = "cuda"))]
impl DecodeRouteDecision {
    fn measured(cpu_elapsed: Duration, device_elapsed: Duration) -> Self {
        let cpu_seconds = cpu_elapsed.as_secs_f64();
        let device_seconds = device_elapsed.as_secs_f64();
        let winner = if cpu_seconds > 0.0 && device_seconds <= cpu_seconds * DEVICE_WIN_RATIO {
            DecodeRoute::Device
        } else {
            DecodeRoute::Cpu
        };
        Self {
            winner,
            cpu_elapsed,
            device_elapsed,
            device_failure: false,
        }
    }

    fn device_failure() -> Self {
        Self {
            winner: DecodeRoute::Cpu,
            cpu_elapsed: Duration::ZERO,
            device_elapsed: Duration::MAX,
            device_failure: true,
        }
    }
}

#[derive(Debug)]
pub(crate) struct DecodeRuntime {
    options: DecodeExecutionOptions,
    #[cfg(any(test, feature = "metal", feature = "cuda"))]
    route_cache: Mutex<DecodeRouteCache>,
    #[cfg(feature = "metal")]
    metal_sessions: OnceLock<Result<crate::output::metal::MetalBackendSessions, String>>,
    #[cfg(feature = "cuda")]
    cuda_sessions: OnceLock<Result<crate::output::cuda::CudaBackendSessions, String>>,
}

impl DecodeRuntime {
    #[cfg(test)]
    pub(crate) fn new(options: DecodeExecutionOptions) -> Result<Self, WsiError> {
        Ok(Self::build(options))
    }

    pub(crate) fn arc_for_options(options: DecodeExecutionOptions) -> Result<Arc<Self>, WsiError> {
        static AUTO_RUNTIME: OnceLock<Arc<DecodeRuntime>> = OnceLock::new();
        static CPU_ONLY_RUNTIME: OnceLock<Arc<DecodeRuntime>> = OnceLock::new();
        let runtime = match options.acceleration {
            DecodeAcceleration::Auto => AUTO_RUNTIME.get_or_init(|| Arc::new(Self::build(options))),
            DecodeAcceleration::CpuOnly => {
                CPU_ONLY_RUNTIME.get_or_init(|| Arc::new(Self::build(options)))
            }
        };
        Ok(Arc::clone(runtime))
    }

    fn build(options: DecodeExecutionOptions) -> Self {
        Self {
            options,
            #[cfg(any(test, feature = "metal", feature = "cuda"))]
            route_cache: Mutex::new(new_decode_route_cache()),
            #[cfg(feature = "metal")]
            metal_sessions: OnceLock::new(),
            #[cfg(feature = "cuda")]
            cuda_sessions: OnceLock::new(),
        }
    }

    pub(crate) fn default_arc() -> Arc<Self> {
        Self::arc_for_options(DecodeExecutionOptions::default())
            .expect("constructing the default decode runtime is infallible")
    }

    #[cfg(test)]
    fn inline(options: DecodeExecutionOptions) -> Self {
        Self::build(options)
    }

    pub(crate) fn install_jp2k_cpu<R: Send>(&self, operation: impl FnOnce() -> R + Send) -> R {
        // Reuse an invoking Rayon worker instead of entering another registry.
        // Calls from ordinary threads share the one process-wide WSI pool.
        if rayon::current_thread_index().is_some() {
            operation()
        } else if let Some(pool) = process_jp2k_cpu_pool() {
            pool.install(operation)
        } else {
            operation()
        }
    }

    pub(crate) fn options(&self) -> DecodeExecutionOptions {
        self.options
    }

    #[cfg(any(test, feature = "metal", feature = "cuda"))]
    fn cached_route(&self, key: &DecodeRouteKey) -> Option<DecodeRouteDecision> {
        self.route_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .peek(key)
            .cloned()
    }

    #[cfg(any(test, feature = "metal", feature = "cuda"))]
    fn store_route(
        &self,
        key: DecodeRouteKey,
        decision: DecodeRouteDecision,
        control: Option<&crate::ReadControl>,
    ) -> Result<(), WsiError> {
        let mut cache = self
            .route_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(control) = control {
            control.publish_if_active(|| insert_decode_route(&mut cache, key, decision))
        } else {
            insert_decode_route(&mut cache, key, decision);
            Ok(())
        }
    }

    #[cfg(feature = "metal")]
    fn metal_sessions(&self) -> Result<&crate::output::metal::MetalBackendSessions, WsiError> {
        self.metal_sessions
            .get_or_init(|| {
                crate::output::metal::MetalBackendSessions::system_default()
                    .map_err(|error| error.to_string())
            })
            .as_ref()
            .map_err(|reason| WsiError::Unsupported {
                reason: format!("Metal JP2K acceleration unavailable: {reason}"),
            })
    }

    #[cfg(feature = "cuda")]
    fn cuda_sessions(&self) -> Result<&crate::output::cuda::CudaBackendSessions, WsiError> {
        self.cuda_sessions
            .get_or_init(|| {
                crate::output::cuda::CudaBackendSessions::system_default()
                    .map_err(|error| error.to_string())
            })
            .as_ref()
            .map_err(|reason| WsiError::Unsupported {
                reason: reason.clone(),
            })
    }
}

#[cfg(any(test, feature = "metal", feature = "cuda"))]
type DecodeRouteCache = LruCache<DecodeRouteKey, DecodeRouteDecision>;

#[cfg(any(test, feature = "metal", feature = "cuda"))]
fn new_decode_route_cache() -> DecodeRouteCache {
    LruCache::new(
        NonZeroUsize::new(ROUTE_CACHE_MAX_ENTRIES).expect("route cache capacity is nonzero"),
    )
}

#[cfg(any(test, feature = "metal", feature = "cuda"))]
fn insert_decode_route(
    cache: &mut DecodeRouteCache,
    key: DecodeRouteKey,
    decision: DecodeRouteDecision,
) {
    // Peeks keep reads and replacements on the existing FIFO eviction order.
    if let Some(existing) = cache.peek_mut(&key) {
        *existing = decision;
    } else {
        cache.put(key, decision);
    }
}

fn process_jp2k_cpu_pool() -> Option<&'static ThreadPool> {
    static POOL: OnceLock<Option<ThreadPool>> = OnceLock::new();
    POOL.get_or_init(|| {
        // Rayon's default sizing honors RAYON_NUM_THREADS; leaving it unset
        // also avoids a second wsi-rs-specific concurrency control.
        rayon::ThreadPoolBuilder::new()
            .thread_name(|index| format!("wsi-rs-jp2k-cpu-{index}"))
            .build()
            .map(Some)
            .unwrap_or_else(|error| {
                tracing::error!(
                    %error,
                    "failed to initialize process-wide JP2K CPU pool; decoding inline"
                );
                None
            })
    })
    .as_ref()
}

#[cfg(any(feature = "metal", feature = "cuda"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum DeviceKind {
    #[cfg(feature = "metal")]
    Metal,
    #[cfg(feature = "cuda")]
    Cuda,
}

#[cfg(all(feature = "route-telemetry", any(feature = "metal", feature = "cuda")))]
fn telemetry_counters(device: DeviceKind) -> &'static DecodeRouteTelemetryCounters {
    match device {
        #[cfg(feature = "metal")]
        DeviceKind::Metal => &METAL_ROUTE_TELEMETRY,
        #[cfg(feature = "cuda")]
        DeviceKind::Cuda => &CUDA_ROUTE_TELEMETRY,
    }
}

#[cfg(all(feature = "route-telemetry", any(feature = "metal", feature = "cuda")))]
fn telemetry_add(counter: &AtomicU64, tiles: usize) {
    let tiles = u64::try_from(tiles).unwrap_or(u64::MAX);
    counter.fetch_add(tiles, Ordering::Relaxed);
}

#[cfg(all(feature = "route-telemetry", any(feature = "metal", feature = "cuda")))]
fn record_device_attempt(device: DeviceKind, tiles: usize) {
    telemetry_add(&telemetry_counters(device).device_attempt_tiles, tiles);
}

#[cfg(all(feature = "route-telemetry", any(feature = "metal", feature = "cuda")))]
fn record_device_route(device: DeviceKind, tiles: usize) {
    telemetry_add(&telemetry_counters(device).device_tiles, tiles);
}

#[cfg(all(feature = "route-telemetry", any(feature = "metal", feature = "cuda")))]
fn record_adaptive_cpu_route(device: DeviceKind, tiles: usize) {
    telemetry_add(&telemetry_counters(device).adaptive_cpu_tiles, tiles);
}

#[cfg(all(feature = "route-telemetry", any(feature = "metal", feature = "cuda")))]
fn record_device_failure_fallback(device: DeviceKind, tiles: usize) {
    let counters = telemetry_counters(device);
    telemetry_add(&counters.fallback_tiles, tiles);
    telemetry_add(&counters.device_failure_fallback_tiles, tiles);
}

#[cfg(all(feature = "route-telemetry", any(feature = "metal", feature = "cuda")))]
fn record_unavailable_fallback(device: DeviceKind, tiles: usize) {
    let counters = telemetry_counters(device);
    telemetry_add(&counters.fallback_tiles, tiles);
    telemetry_add(&counters.unavailable_fallback_tiles, tiles);
}

#[cfg(all(
    not(feature = "route-telemetry"),
    any(feature = "metal", feature = "cuda")
))]
fn record_device_attempt(_device: DeviceKind, _tiles: usize) {}
#[cfg(all(
    not(feature = "route-telemetry"),
    any(feature = "metal", feature = "cuda")
))]
fn record_device_route(_device: DeviceKind, _tiles: usize) {}
#[cfg(all(
    not(feature = "route-telemetry"),
    any(feature = "metal", feature = "cuda")
))]
fn record_adaptive_cpu_route(_device: DeviceKind, _tiles: usize) {}
#[cfg(all(
    not(feature = "route-telemetry"),
    any(feature = "metal", feature = "cuda")
))]
fn record_device_failure_fallback(_device: DeviceKind, _tiles: usize) {}
#[cfg(all(
    not(feature = "route-telemetry"),
    any(feature = "metal", feature = "cuda")
))]
fn record_unavailable_fallback(_device: DeviceKind, _tiles: usize) {}

#[cfg(any(test, feature = "metal", feature = "cuda"))]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct DecodeRouteKey {
    dataset_id: u128,
    scene: usize,
    series: usize,
    level: u32,
    sample_geometry: RouteSampleGeometry,
    codec_kind: TileCodecKind,
    device_identity: String,
    sample_tile_count: usize,
}

#[cfg(any(test, feature = "metal", feature = "cuda"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct RouteTileGeometry {
    width: u32,
    height: u32,
}

#[cfg(any(test, feature = "metal", feature = "cuda"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct RouteSampleGeometry {
    tiles: [RouteTileGeometry; ROUTE_SAMPLE_SIZE],
    len: u8,
}

#[cfg(any(test, feature = "metal", feature = "cuda"))]
impl RouteSampleGeometry {
    #[cfg(test)]
    fn from_dimensions<const N: usize>(dimensions: [(u32, u32); N]) -> Self {
        assert!(
            N <= ROUTE_SAMPLE_SIZE,
            "route sample geometry exceeds its fixed capacity"
        );
        let mut tiles = [RouteTileGeometry {
            width: 0,
            height: 0,
        }; ROUTE_SAMPLE_SIZE];
        for (slot, (width, height)) in tiles.iter_mut().zip(dimensions) {
            *slot = RouteTileGeometry { width, height };
        }
        tiles[..N].sort_unstable_by_key(|tile| (tile.width, tile.height));
        Self {
            tiles,
            len: N as u8,
        }
    }
}

pub(crate) struct AdaptiveDecodeReader {
    inner: Box<dyn ManagedSlideReader>,
    runtime: Arc<DecodeRuntime>,
}

impl AdaptiveDecodeReader {
    #[cfg(test)]
    pub(crate) fn new(inner: Box<dyn SlideReader>, runtime: Arc<DecodeRuntime>) -> Self {
        Self::new_managed(
            Box::new(ConservativeManagedReader::new(
                inner,
                crate::SlideLimits::default().encoded_unit_bytes(),
            )),
            runtime,
        )
    }

    pub(crate) fn new_managed(
        inner: Box<dyn ManagedSlideReader>,
        runtime: Arc<DecodeRuntime>,
    ) -> Self {
        Self { inner, runtime }
    }

    fn read_tiles_adaptive(
        &self,
        reqs: &[TileRequest],
        control: Option<&crate::ReadControl>,
    ) -> Result<Vec<CpuTile>, WsiError> {
        Self::check_control(control)?;
        if reqs.is_empty() || self.runtime.options.acceleration == DecodeAcceleration::CpuOnly {
            return self.read_inner_cpu(reqs, control);
        }
        #[cfg(any(feature = "metal", feature = "cuda"))]
        {
            self.read_tiles_adaptive_device(reqs, control)
        }
        #[cfg(not(any(feature = "metal", feature = "cuda")))]
        {
            self.read_inner_cpu(reqs, control)
        }
    }

    #[cfg(any(feature = "metal", feature = "cuda"))]
    fn read_tiles_adaptive_device(
        &self,
        reqs: &[TileRequest],
        control: Option<&crate::ReadControl>,
    ) -> Result<Vec<CpuTile>, WsiError> {
        let Some(device) = self.preferred_device() else {
            let tiles = self.read_inner_cpu(reqs, control)?;
            if let Some(device) = self.configured_device() {
                record_unavailable_fallback(device, jp2k_tile_count(self.inner.as_ref(), reqs));
            }
            return Ok(tiles);
        };
        let device_identity = self.device_identity(device)?;
        let Some(key) = route_key_for_batch(self.inner.as_ref(), reqs, &device_identity) else {
            return self.read_inner_cpu(reqs, control);
        };

        if let Some(decision) = self.runtime.cached_route(&key) {
            tracing::debug!(
                route = ?decision.winner,
                cpu_elapsed_ms = decision.cpu_elapsed.as_secs_f64() * 1000.0,
                device_elapsed_ms = decision.device_elapsed.as_secs_f64() * 1000.0,
                route_cache_hit = true,
                "wsi adaptive JP2K route"
            );
            return match decision.winner {
                DecodeRoute::Cpu => {
                    let tiles = self.read_inner_cpu(reqs, control)?;
                    if decision.device_failure {
                        record_device_failure_fallback(device, tiles.len());
                    } else {
                        record_adaptive_cpu_route(device, tiles.len());
                    }
                    Ok(tiles)
                }
                DecodeRoute::Device => match self.read_device_host(reqs, device, control) {
                    Ok(tiles) => {
                        record_device_route(device, reqs.len());
                        Ok(tiles)
                    }
                    Err(error) => {
                        tracing::debug!(error = %error, "cached JP2K device route fell back to CPU");
                        self.runtime.store_route(
                            key,
                            DecodeRouteDecision::device_failure(),
                            control,
                        )?;
                        let tiles = self.read_inner_cpu(reqs, control)?;
                        record_device_failure_fallback(device, tiles.len());
                        Ok(tiles)
                    }
                },
            };
        }

        let sample_len = reqs.len().min(ROUTE_SAMPLE_SIZE);
        let sample = &reqs[..sample_len];
        let device_started = Instant::now();
        let device_sample = match self.read_device_host(sample, device, control) {
            Ok(tiles) => tiles,
            Err(error) => {
                tracing::debug!(error = %error, "JP2K device route unavailable; using CPU");
                let cpu_tiles = self.read_inner_cpu(reqs, control)?;
                self.runtime
                    .store_route(key, DecodeRouteDecision::device_failure(), control)?;
                record_device_failure_fallback(device, cpu_tiles.len());
                return Ok(cpu_tiles);
            }
        };
        let device_elapsed = device_started.elapsed();

        let cpu_started = Instant::now();
        let cpu_sample = self.read_inner_cpu(sample, control)?;
        let cpu_elapsed = cpu_started.elapsed();
        let decision = DecodeRouteDecision::measured(cpu_elapsed, device_elapsed);
        let winner = decision.winner;
        let mut measured_cpu_sample = Some(cpu_sample);
        let mut tiles = match winner {
            DecodeRoute::Cpu => measured_cpu_sample
                .take()
                .expect("the measured CPU sample is present"),
            DecodeRoute::Device => device_sample,
        };
        if sample_len < reqs.len() {
            let remainder = match winner {
                DecodeRoute::Cpu => self.read_inner_cpu(&reqs[sample_len..], control),
                DecodeRoute::Device => self.read_device_host(&reqs[sample_len..], device, control),
            };
            match remainder {
                Ok(mut remainder) => tiles.append(&mut remainder),
                Err(error) if winner == DecodeRoute::Device => {
                    tracing::debug!(
                        error = %error,
                        "selected JP2K device route failed; completing the batch on CPU"
                    );
                    self.runtime.store_route(
                        key,
                        DecodeRouteDecision::device_failure(),
                        control,
                    )?;
                    let mut cpu_tiles = measured_cpu_sample
                        .take()
                        .expect("the measured CPU sample remains available for device fallback");
                    cpu_tiles.extend(self.read_inner_cpu(&reqs[sample_len..], control)?);
                    record_device_failure_fallback(device, cpu_tiles.len());
                    return Ok(cpu_tiles);
                }
                Err(error) => return Err(error),
            }
        }
        tracing::debug!(
            route = ?winner,
            sample_tile_count = sample_len,
            cpu_elapsed_ms = cpu_elapsed.as_secs_f64() * 1000.0,
            device_elapsed_ms = device_elapsed.as_secs_f64() * 1000.0,
            route_cache_hit = false,
            "wsi adaptive JP2K route"
        );
        self.runtime.store_route(key, decision, control)?;
        match winner {
            DecodeRoute::Cpu => record_adaptive_cpu_route(device, reqs.len()),
            DecodeRoute::Device => record_device_route(device, reqs.len()),
        }
        Ok(tiles)
    }

    fn check_control(control: Option<&crate::ReadControl>) -> Result<(), WsiError> {
        control.map_or(Ok(()), crate::ReadControl::check_cancelled)
    }

    fn read_inner_cpu(
        &self,
        reqs: &[TileRequest],
        control: Option<&crate::ReadControl>,
    ) -> Result<Vec<CpuTile>, WsiError> {
        Self::check_control(control)?;
        let operation = || match control {
            Some(control) => self.inner.read_tiles_cpu_controlled(reqs, control),
            None => self.inner.read_tiles_cpu(reqs),
        };
        let result = if batch_uses_jp2k(self.inner.as_ref(), reqs) {
            self.runtime.install_jp2k_cpu(operation)
        } else {
            operation()
        };
        Self::check_control(control)?;
        result.and_then(|tiles| {
            crate::core::batch::expect_exact_count(tiles, reqs.len(), "adaptive CPU tile batch")
        })
    }

    #[cfg(any(feature = "metal", feature = "cuda"))]
    fn preferred_device(&self) -> Option<DeviceKind> {
        #[cfg(all(feature = "metal", target_os = "macos"))]
        if self.runtime.metal_sessions().is_ok() {
            return Some(DeviceKind::Metal);
        }
        #[cfg(feature = "cuda")]
        {
            if self.runtime.cuda_sessions().is_ok() {
                return Some(DeviceKind::Cuda);
            }
        }
        #[allow(unreachable_code)]
        None
    }

    #[cfg(any(feature = "metal", feature = "cuda"))]
    fn configured_device(&self) -> Option<DeviceKind> {
        #[cfg(all(feature = "metal", target_os = "macos"))]
        {
            Some(DeviceKind::Metal)
        }
        #[cfg(all(feature = "cuda", not(all(feature = "metal", target_os = "macos"))))]
        {
            Some(DeviceKind::Cuda)
        }
        #[cfg(not(any(all(feature = "metal", target_os = "macos"), feature = "cuda")))]
        {
            None
        }
    }

    #[cfg(any(feature = "metal", feature = "cuda"))]
    fn device_identity(&self, device: DeviceKind) -> Result<String, WsiError> {
        match device {
            #[cfg(feature = "metal")]
            DeviceKind::Metal => Ok(self.runtime.metal_sessions()?.device_identity()),
            #[cfg(feature = "cuda")]
            DeviceKind::Cuda => Ok(self.runtime.cuda_sessions()?.device_identity().to_owned()),
        }
    }

    #[cfg(any(feature = "metal", feature = "cuda"))]
    fn read_device_host(
        &self,
        reqs: &[TileRequest],
        device: DeviceKind,
        control: Option<&crate::ReadControl>,
    ) -> Result<Vec<CpuTile>, WsiError> {
        Self::check_control(control)?;
        record_device_attempt(device, reqs.len());
        let tiles = match device {
            #[cfg(feature = "metal")]
            DeviceKind::Metal => self
                .inner
                .read_tiles_metal(reqs, self.runtime.metal_sessions()?)?
                .iter()
                .map(crate::output::metal::MetalDeviceTile::download_cpu)
                .collect::<Result<Vec<_>, _>>()?,
            #[cfg(feature = "cuda")]
            DeviceKind::Cuda => self
                .inner
                .read_tiles_cuda(reqs, self.runtime.cuda_sessions()?)?
                .iter()
                .map(crate::output::cuda::CudaDeviceTile::download_cpu)
                .collect::<Result<Vec<_>, _>>()?,
        };
        Self::check_control(control)?;
        crate::core::batch::expect_exact_count(tiles, reqs.len(), "adaptive device tile batch")
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

    fn prepare_level_controlled(
        &self,
        scene: crate::core::types::SceneId,
        series: crate::core::types::SeriesId,
        level: crate::core::types::LevelIdx,
        control: &crate::ReadControl,
    ) -> Result<(), WsiError> {
        self.inner
            .prepare_level_controlled(scene, series, level, control)
    }

    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        crate::core::batch::exactly_one(
            self.read_tiles_adaptive(std::slice::from_ref(req), None)?,
            "adaptive single tile read",
        )
    }

    fn read_tiles_cpu(&self, reqs: &[TileRequest]) -> Result<Vec<CpuTile>, WsiError> {
        self.read_tiles_adaptive(reqs, None)
    }

    fn read_tiles_cpu_controlled(
        &self,
        reqs: &[TileRequest],
        control: &crate::ReadControl,
    ) -> Result<Vec<CpuTile>, WsiError> {
        self.read_tiles_adaptive(reqs, Some(control))
    }

    #[cfg(feature = "metal")]
    fn read_tiles_metal(
        &self,
        reqs: &[TileRequest],
        session: &crate::output::metal::MetalBackendSessions,
    ) -> Result<Vec<crate::output::metal::MetalDeviceTile>, WsiError> {
        self.inner.read_tiles_metal(reqs, session)
    }

    #[cfg(feature = "cuda")]
    fn read_tiles_cuda(
        &self,
        reqs: &[TileRequest],
        session: &crate::output::cuda::CudaBackendSessions,
    ) -> Result<Vec<crate::output::cuda::CudaDeviceTile>, WsiError> {
        self.inner.read_tiles_cuda(reqs, session)
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

    fn use_display_tile_cache(&self, req: &crate::core::types::TileViewRequest) -> bool {
        self.inner.use_display_tile_cache(req)
    }

    fn read_region_fastpath(
        &self,
        ctx: &mut crate::core::registry::SlideReadContext<'_>,
        req: &crate::core::types::RegionRequest,
    ) -> Option<Result<CpuTile, WsiError>> {
        self.runtime
            .install_jp2k_cpu(|| self.inner.read_region_fastpath(ctx, req))
    }

    fn read_region(&self, req: &crate::core::types::RegionRequest) -> Result<CpuTile, WsiError> {
        self.runtime
            .install_jp2k_cpu(|| self.inner.read_region(req))
    }

    fn read_display_tile(
        &self,
        req: &crate::core::types::TileViewRequest,
    ) -> Result<CpuTile, WsiError> {
        self.runtime
            .install_jp2k_cpu(|| self.inner.read_display_tile(req))
    }

    fn read_associated(&self, name: &str) -> Result<CpuTile, WsiError> {
        self.inner.read_associated(name)
    }
}

impl ManagedSlideReader for AdaptiveDecodeReader {
    fn tile_encoded_upper_bound(&self, req: &TileRequest) -> Result<u64, WsiError> {
        self.inner.tile_encoded_upper_bound(req)
    }

    fn tile_batch_encoded_upper_bound(&self, reqs: &[TileRequest]) -> Result<u64, WsiError> {
        self.inner.tile_batch_encoded_upper_bound(reqs)
    }

    fn display_tile_encoded_upper_bound(
        &self,
        req: &crate::core::types::TileViewRequest,
    ) -> Result<u64, WsiError> {
        self.inner.display_tile_encoded_upper_bound(req)
    }

    fn associated_encoded_upper_bound(&self, name: &str) -> Result<u64, WsiError> {
        self.inner.associated_encoded_upper_bound(name)
    }

    fn region_fastpath_encoded_upper_bound(
        &self,
        req: &crate::core::types::RegionRequest,
    ) -> Result<u64, WsiError> {
        self.inner.region_fastpath_encoded_upper_bound(req)
    }
}

fn batch_uses_jp2k(reader: &dyn SlideReader, reqs: &[TileRequest]) -> bool {
    jp2k_tile_count(reader, reqs) != 0
}

fn jp2k_tile_count(reader: &dyn SlideReader, reqs: &[TileRequest]) -> usize {
    reqs.iter()
        .filter(|request| {
            matches!(
                reader.tile_codec_kind(request),
                TileCodecKind::Jp2k | TileCodecKind::Htj2k
            )
        })
        .count()
}

#[cfg(any(test, feature = "metal", feature = "cuda"))]
fn route_key_for_batch(
    reader: &dyn SlideReader,
    reqs: &[TileRequest],
    device_identity: &str,
) -> Option<DecodeRouteKey> {
    let first = reqs.first()?;
    if !reqs.iter().all(|request| {
        request.scene == first.scene
            && request.series == first.series
            && request.level == first.level
    }) {
        return None;
    }
    let codec_kind = reader.tile_codec_kind(first);
    if !matches!(codec_kind, TileCodecKind::Jp2k | TileCodecKind::Htj2k)
        || !reqs
            .iter()
            .all(|request| reader.tile_codec_kind(request) == codec_kind)
    {
        return None;
    }
    let level = dataset_level(
        reader.dataset(),
        first.scene.get(),
        first.series.get(),
        first.level.get(),
    )?;
    let sample_len = reqs.len().min(ROUTE_SAMPLE_SIZE);
    let mut dimensions = [(0, 0); ROUTE_SAMPLE_SIZE];
    for (slot, request) in dimensions.iter_mut().zip(&reqs[..sample_len]) {
        *slot = logical_tile_dimensions(level, request)?;
    }
    dimensions[..sample_len].sort_unstable();
    let sample_geometry = RouteSampleGeometry {
        tiles: dimensions.map(|(width, height)| RouteTileGeometry { width, height }),
        len: sample_len as u8,
    };
    Some(DecodeRouteKey {
        dataset_id: reader.dataset().id.0,
        scene: first.scene.get(),
        series: first.series.get(),
        level: first.level.get(),
        sample_geometry,
        codec_kind,
        device_identity: device_identity.to_owned(),
        sample_tile_count: reqs.len().min(ROUTE_SAMPLE_SIZE),
    })
}

#[cfg(any(test, feature = "metal", feature = "cuda"))]
fn dataset_level(dataset: &Dataset, scene: usize, series: usize, level: u32) -> Option<&Level> {
    dataset
        .scenes
        .get(scene)?
        .series
        .get(series)?
        .levels
        .get(level as usize)
}

#[cfg(any(test, feature = "metal", feature = "cuda"))]
fn logical_tile_dimensions(level: &Level, request: &TileRequest) -> Option<(u32, u32)> {
    match &level.tile_layout {
        TileLayout::Regular {
            tile_width,
            tile_height,
            tiles_across,
            tiles_down,
        } => {
            let col = u64::try_from(request.col).ok()?;
            let row = u64::try_from(request.row).ok()?;
            if col >= *tiles_across || row >= *tiles_down {
                return None;
            }
            let x = col.checked_mul(u64::from(*tile_width))?;
            let y = row.checked_mul(u64::from(*tile_height))?;
            let width = level
                .dimensions
                .0
                .checked_sub(x)?
                .min(u64::from(*tile_width));
            let height = level
                .dimensions
                .1
                .checked_sub(y)?
                .min(u64::from(*tile_height));
            Some((u32::try_from(width).ok()?, u32::try_from(height).ok()?))
        }
        _ => None,
    }
}

#[cfg(test)]
#[path = "decode_runtime/tests.rs"]
mod tests;
