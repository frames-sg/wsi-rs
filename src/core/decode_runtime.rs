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
        "execution": crate::core::execution_telemetry::snapshot(),
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
    /// Start on CPU, then calibrate on later eligible reads against an available
    /// Metal or CUDA path, including readback. Optional work never waits for memory.
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
            crate::core::execution_telemetry::record(
                crate::core::execution_telemetry::Event::CpuPoolDispatches,
                1,
            );
            pool.install(operation)
        } else {
            operation()
        }
    }

    pub(crate) fn cpu_worker_count(&self) -> usize {
        if rayon::current_thread_index().is_some() {
            rayon::current_num_threads()
        } else {
            process_jp2k_cpu_pool().map_or(1, ThreadPool::current_num_threads)
        }
    }

    pub(crate) fn options(&self) -> DecodeExecutionOptions {
        self.options
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
mod calibration;
#[cfg(any(test, feature = "metal", feature = "cuda"))]
use calibration::*;

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

/// Where the adaptive router sent a JP2K tile. Every tile a routed batch
/// returns is recorded exactly once as `Device`, `AdaptiveCpu` or a fallback.
#[cfg(any(feature = "metal", feature = "cuda"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RouteTileOutcome {
    DeviceAttempt,
    Device,
    AdaptiveCpu,
    DeviceFailureFallback,
    UnavailableFallback,
}

#[cfg(all(test, any(feature = "metal", feature = "cuda")))]
thread_local! {
    static TEST_ROUTE_TILES: std::cell::Cell<[u64; 5]> = const { std::cell::Cell::new([0; 5]) };
}

// Only the Metal adaptive-routing tests read these counters.
#[cfg(all(test, feature = "metal"))]
fn test_route_tiles(outcome: RouteTileOutcome) -> u64 {
    TEST_ROUTE_TILES.with(|cell| cell.get()[outcome as usize])
}

#[cfg(any(feature = "metal", feature = "cuda"))]
fn record_route(device: DeviceKind, outcome: RouteTileOutcome, tiles: usize) {
    #[cfg(feature = "route-telemetry")]
    {
        let counters = telemetry_counters(device);
        let tiles = u64::try_from(tiles).unwrap_or(u64::MAX);
        let add = |counter: &AtomicU64| {
            counter.fetch_add(tiles, Ordering::Relaxed);
        };
        match outcome {
            RouteTileOutcome::DeviceAttempt => add(&counters.device_attempt_tiles),
            RouteTileOutcome::Device => add(&counters.device_tiles),
            RouteTileOutcome::AdaptiveCpu => add(&counters.adaptive_cpu_tiles),
            RouteTileOutcome::DeviceFailureFallback => {
                add(&counters.fallback_tiles);
                add(&counters.device_failure_fallback_tiles);
            }
            RouteTileOutcome::UnavailableFallback => {
                add(&counters.fallback_tiles);
                add(&counters.unavailable_fallback_tiles);
            }
        }
    }
    #[cfg(test)]
    TEST_ROUTE_TILES.with(|cell| {
        let mut counts = cell.get();
        counts[outcome as usize] += tiles as u64;
        cell.set(counts);
    });
    let _ = (device, outcome, tiles);
}

#[cfg(any(feature = "metal", feature = "cuda"))]
fn record_device_attempt(device: DeviceKind, tiles: usize) {
    record_route(device, RouteTileOutcome::DeviceAttempt, tiles);
}

#[cfg(any(feature = "metal", feature = "cuda"))]
fn record_device_route(device: DeviceKind, tiles: usize) {
    record_route(device, RouteTileOutcome::Device, tiles);
}

#[cfg(any(feature = "metal", feature = "cuda"))]
fn record_adaptive_cpu_route(device: DeviceKind, tiles: usize) {
    record_route(device, RouteTileOutcome::AdaptiveCpu, tiles);
}

#[cfg(any(feature = "metal", feature = "cuda"))]
fn record_device_failure_fallback(device: DeviceKind, tiles: usize) {
    record_route(device, RouteTileOutcome::DeviceFailureFallback, tiles);
}

#[cfg(any(feature = "metal", feature = "cuda"))]
fn record_unavailable_fallback(device: DeviceKind, tiles: usize) {
    record_route(device, RouteTileOutcome::UnavailableFallback, tiles);
}

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
    cpu_workers: usize,
}

#[cfg(any(test, feature = "metal", feature = "cuda"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct RouteTileGeometry {
    width: u32,
    height: u32,
}

#[cfg(any(test, feature = "metal", feature = "cuda"))]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RouteSampleGeometry(Vec<(RouteTileGeometry, usize)>);

#[cfg(any(test, feature = "metal", feature = "cuda"))]
impl RouteSampleGeometry {
    fn from_dimensions(dimensions: impl IntoIterator<Item = (u32, u32)>) -> Self {
        let mut counts = std::collections::BTreeMap::new();
        for dimensions in dimensions {
            *counts.entry(dimensions).or_insert(0) += 1;
        }
        Self(
            counts
                .into_iter()
                .map(|((width, height), count)| (RouteTileGeometry { width, height }, count))
                .collect(),
        )
    }
}

pub(crate) struct AdaptiveDecodeReader {
    inner: Box<dyn ManagedSlideReader>,
    runtime: Arc<DecodeRuntime>,
}

#[cfg(any(feature = "metal", feature = "cuda"))]
mod adaptive;
mod reader;
#[cfg(test)]
use reader::*;

#[cfg(test)]
#[path = "decode_runtime/tests.rs"]
mod tests;
