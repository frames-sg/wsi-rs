use std::fs::File;
use std::hint::black_box;
use std::io::{BufReader, Read};
use std::path::Path;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{
    summarize_samples, Level0Bounds, LevelInfo, ReadSpec, WorkerConfig, Workload, WorkloadPlan,
    CAPTURE_WORKLOAD_NAMES,
};
use wsi_rs_test_support::openslide::{
    OpenSlide, OpenSlideApi, OpenSlideBounds, OpenSlideCache, OpenSlideLevel,
};

pub const WORKER_SCHEMA_VERSION: u32 = 4;
const OPEN_SAMPLE_COUNT: usize = 10;
const ROUTE_TELEMETRY_PROPERTY: &str = "wsi-rs.internal.decode-route-telemetry";

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WorkerResult {
    pub schema_version: u32,
    pub kind: String,
    pub engine: String,
    pub library_path: String,
    pub library_sha256: String,
    pub library_version: String,
    pub slide_path: String,
    pub slide_sha256: String,
    pub repeat_index: u32,
    pub cache_bytes: usize,
    pub worker_count: usize,
    pub level0_bounds: Level0Bounds,
    pub levels: Vec<LevelResult>,
    pub workloads: Vec<WorkloadResult>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LevelResult {
    pub width: u64,
    pub height: u64,
    pub downsample: f64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WorkloadResult {
    pub name: String,
    pub n: usize,
    pub samples_us: Vec<u64>,
    pub p50_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
    pub mean_us: u64,
    pub bytes_read: u64,
    pub workers: usize,
    pub effective_elapsed_us: u64,
    pub throughput_bytes_per_second: u64,
    pub checksum_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<Value>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
struct RouteBackendSnapshot {
    device_attempt_tiles: u64,
    device_tiles: u64,
    adaptive_cpu_tiles: u64,
    fallback_tiles: u64,
    device_failure_fallback_tiles: u64,
    unavailable_fallback_tiles: u64,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
struct RouteTelemetrySnapshot {
    metal: RouteBackendSnapshot,
    cuda: RouteBackendSnapshot,
}

pub fn run(config: &WorkerConfig) -> Result<WorkerResult, String> {
    let library_path = canonical_file(&config.library_path, "OpenSlide library")?;
    let slide_path = canonical_file(&config.slide_path, "slide")?;
    let library_sha256 = sha256_file(&library_path)?;
    let slide_sha256 = sha256_file(&slide_path)?;
    let api = OpenSlideApi::load(&library_path)?;
    let library_version = api.version()?;
    validate_version(&library_version, config.required_version_prefix.as_deref())?;
    let cache = api.create_cache(config.cache_bytes)?;
    let slide = api.open_with_cache(&slide_path, &cache)?;
    let levels = slide
        .levels()?
        .into_iter()
        .map(LevelInfo::from)
        .collect::<Vec<_>>();
    let level0_bounds = slide
        .level0_bounds()?
        .map(Level0Bounds::from)
        .unwrap_or_else(|| full_level0_bounds(&levels));
    let plan = WorkloadPlan::with_level0_bounds(levels.clone(), level0_bounds)?;
    let mut workloads = Vec::new();

    if config
        .only
        .as_deref()
        .is_none_or(|name| name == CAPTURE_WORKLOAD_NAMES[0])
    {
        workloads.push(run_open_latency(
            &api,
            &cache,
            &slide_path,
            &levels,
            level0_bounds,
        )?);
    }
    for workload in plan.viewer_workloads() {
        if config
            .only
            .as_deref()
            .is_none_or(|name| name == workload.name)
        {
            workloads.push(run_read_workload(
                &library_path,
                &slide_path,
                config.cache_bytes,
                config.workers,
                workload,
                Some(&slide),
            )?);
        }
    }
    if workloads.is_empty() {
        return Err(format!(
            "unknown workload {:?}; expected open_latency or a viewer workload",
            config.only
        ));
    }

    Ok(WorkerResult {
        schema_version: WORKER_SCHEMA_VERSION,
        kind: "wsi-rs-perf-worker".into(),
        engine: config.engine.name().into(),
        library_path: library_path.display().to_string(),
        library_sha256,
        library_version,
        slide_path: slide_path.display().to_string(),
        slide_sha256,
        repeat_index: config.repeat_index,
        cache_bytes: config.cache_bytes,
        worker_count: config.workers,
        level0_bounds,
        levels: levels.into_iter().map(LevelResult::from).collect(),
        workloads,
    })
}

fn canonical_file(path: &Path, description: &str) -> Result<std::path::PathBuf, String> {
    if !path.is_file() {
        return Err(format!("{description} is not a file: {}", path.display()));
    }
    path.canonicalize()
        .map_err(|err| format!("failed to canonicalize {}: {err}", path.display()))
}

fn validate_version(actual: &str, required_prefix: Option<&str>) -> Result<(), String> {
    if let Some(required) = required_prefix {
        if !actual.starts_with(required) {
            return Err(format!(
                "OpenSlide version mismatch: required prefix {required:?}, loaded {actual:?}"
            ));
        }
    }
    Ok(())
}

fn run_open_latency(
    api: &OpenSlideApi,
    cache: &OpenSlideCache,
    slide_path: &Path,
    expected_levels: &[LevelInfo],
    expected_bounds: Level0Bounds,
) -> Result<WorkloadResult, String> {
    let mut samples = Vec::with_capacity(OPEN_SAMPLE_COUNT);
    let mut checksum = Sha256::new();
    for _ in 0..OPEN_SAMPLE_COUNT {
        let started = Instant::now();
        let reopened = api.open_with_cache(slide_path, cache)?;
        let levels = reopened
            .levels()?
            .into_iter()
            .map(LevelInfo::from)
            .collect::<Vec<_>>();
        let bounds = reopened
            .level0_bounds()?
            .map(Level0Bounds::from)
            .unwrap_or_else(|| full_level0_bounds(&levels));
        samples.push(elapsed_micros(started));
        if levels != expected_levels || bounds != expected_bounds {
            return Err("slide metadata changed between opens".into());
        }
        hash_levels(&mut checksum, &levels);
        hash_bounds(&mut checksum, bounds);
        black_box(&reopened);
    }
    let effective_elapsed_us = samples.iter().copied().sum();
    finish_workload(
        CAPTURE_WORKLOAD_NAMES[0],
        samples,
        0,
        1,
        effective_elapsed_us,
        checksum,
        None,
    )
}

#[derive(Debug)]
struct ReadSample {
    index: usize,
    elapsed_us: u64,
    bytes_read: u64,
    digest: [u8; 32],
}

#[derive(Debug)]
struct WorkerReads {
    samples: Vec<ReadSample>,
    elapsed_us: u64,
}

fn run_read_workload(
    library_path: &Path,
    slide_path: &Path,
    cache_bytes: usize,
    requested_workers: usize,
    workload: Workload,
    diagnostics_slide: Option<&OpenSlide>,
) -> Result<WorkloadResult, String> {
    let route_before = diagnostics_slide
        .map(read_route_telemetry)
        .transpose()?
        .flatten();
    let worker_count = requested_workers.min(workload.reads.len()).max(1);
    let barrier = Arc::new(Barrier::new(worker_count));
    let worker_results = thread::scope(|scope| {
        let mut handles = Vec::with_capacity(worker_count);
        for worker_index in 0..worker_count {
            let reads = workload
                .reads
                .iter()
                .copied()
                .enumerate()
                .filter(|(index, _)| index % worker_count == worker_index)
                .collect::<Vec<_>>();
            let barrier = Arc::clone(&barrier);
            let worker_cache_bytes = cache_share(cache_bytes, worker_count, worker_index);
            handles.push(scope.spawn(move || {
                run_read_worker(
                    library_path,
                    slide_path,
                    worker_cache_bytes,
                    workload.warmup,
                    reads,
                    &barrier,
                )
            }));
        }
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .map_err(|_| "performance read worker panicked".to_string())?
            })
            .collect::<Result<Vec<_>, _>>()
    })?;

    let effective_elapsed_us = worker_results
        .iter()
        .map(|worker| worker.elapsed_us)
        .max()
        .unwrap_or(0);
    let mut read_samples = worker_results
        .into_iter()
        .flat_map(|worker| worker.samples)
        .collect::<Vec<_>>();
    read_samples.sort_unstable_by_key(|sample| sample.index);

    let samples = read_samples
        .iter()
        .map(|sample| sample.elapsed_us)
        .collect::<Vec<_>>();
    let mut checksum = Sha256::new();
    let mut bytes_read = 0u64;
    for sample in read_samples {
        checksum.update(sample.digest);
        bytes_read = bytes_read.saturating_add(sample.bytes_read);
    }
    let route_after = diagnostics_slide
        .map(read_route_telemetry)
        .transpose()?
        .flatten();
    let diagnostics = route_diagnostics(route_before, route_after)?;
    finish_workload(
        workload.name,
        samples,
        bytes_read,
        worker_count,
        effective_elapsed_us,
        checksum,
        diagnostics,
    )
}

fn run_read_worker(
    library_path: &Path,
    slide_path: &Path,
    cache_bytes: usize,
    warmup: bool,
    reads: Vec<(usize, ReadSpec)>,
    barrier: &Barrier,
) -> Result<WorkerReads, String> {
    macro_rules! setup_or_release {
        ($result:expr) => {
            match $result {
                Ok(value) => value,
                Err(error) => {
                    barrier.wait();
                    return Err(error);
                }
            }
        };
    }

    let api = setup_or_release!(OpenSlideApi::load(library_path));
    let cache = setup_or_release!(api.create_cache(cache_bytes));
    let slide = setup_or_release!(api.open_with_cache(slide_path, &cache));
    let mut buffer = Vec::new();
    if warmup {
        for (_, spec) in reads.iter().take(4) {
            setup_or_release!(slide.read_region_argb_into(
                spec.x,
                spec.y,
                spec.level,
                spec.width,
                spec.height,
                &mut buffer,
            ));
            black_box(buffer.as_ptr());
        }
    }

    barrier.wait();
    let worker_started = Instant::now();
    let mut samples = Vec::with_capacity(reads.len());
    for (index, spec) in reads {
        prepare_buffer(spec, &mut buffer)?;
        let started = Instant::now();
        slide.read_region_argb_into(
            spec.x,
            spec.y,
            spec.level,
            spec.width,
            spec.height,
            &mut buffer,
        )?;
        let elapsed_us = elapsed_micros(started);
        let bytes_read = (buffer.len() as u64).saturating_mul(4);
        samples.push(ReadSample {
            index,
            elapsed_us,
            bytes_read,
            digest: read_digest(spec, &buffer),
        });
        black_box(buffer.as_ptr());
    }
    Ok(WorkerReads {
        samples,
        elapsed_us: elapsed_micros(worker_started),
    })
}

fn cache_share(total: usize, workers: usize, worker_index: usize) -> usize {
    total / workers + usize::from(worker_index < total % workers)
}

fn prepare_buffer(spec: ReadSpec, buffer: &mut Vec<u32>) -> Result<(), String> {
    let len = usize::try_from(spec.width)
        .ok()
        .and_then(|width| {
            usize::try_from(spec.height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .ok_or_else(|| format!("region dimensions overflow: {}x{}", spec.width, spec.height))?;
    buffer.resize(len, 0);
    Ok(())
}

fn finish_workload(
    name: &'static str,
    samples_us: Vec<u64>,
    bytes_read: u64,
    workers: usize,
    effective_elapsed_us: u64,
    checksum: Sha256,
    diagnostics: Option<Value>,
) -> Result<WorkloadResult, String> {
    let summary = summarize_samples(&samples_us)
        .ok_or_else(|| format!("workload {name} produced no samples"))?;
    let throughput = if effective_elapsed_us == 0 {
        0
    } else {
        u128::from(bytes_read)
            .saturating_mul(1_000_000)
            .checked_div(u128::from(effective_elapsed_us))
            .and_then(|value| u64::try_from(value).ok())
            .unwrap_or(u64::MAX)
    };
    Ok(WorkloadResult {
        name: name.into(),
        n: samples_us.len(),
        samples_us,
        p50_us: summary.p50_us,
        p95_us: summary.p95_us,
        p99_us: summary.p99_us,
        mean_us: summary.mean_us,
        bytes_read,
        workers,
        effective_elapsed_us,
        throughput_bytes_per_second: throughput,
        checksum_sha256: format!("{:x}", checksum.finalize()),
        diagnostics,
    })
}

fn read_route_telemetry(
    slide: &wsi_rs_test_support::openslide::OpenSlide,
) -> Result<Option<RouteTelemetrySnapshot>, String> {
    let Some(raw) = slide.property(ROUTE_TELEMETRY_PROPERTY) else {
        return Ok(None);
    };
    serde_json::from_str(&raw)
        .map(Some)
        .map_err(|error| format!("invalid wsi-rs route telemetry property: {error}"))
}

fn route_diagnostics(
    before: Option<RouteTelemetrySnapshot>,
    after: Option<RouteTelemetrySnapshot>,
) -> Result<Option<Value>, String> {
    let (Some(before), Some(after)) = (before, after) else {
        return Ok(None);
    };
    let metal = route_delta(after.metal, before.metal);
    let cuda = route_delta(after.cuda, before.cuda);
    let mut active = [("metal", metal), ("cuda", cuda)]
        .into_iter()
        .filter(|(_, counters)| route_activity(*counters));
    let Some((feature, counters)) = active.next() else {
        return Ok(None);
    };
    if active.next().is_some() {
        return Err("workload reported route activity from both Metal and CUDA".into());
    }
    Ok(Some(json!({
        "decode_route": {
            "feature": feature,
            "device_attempt_tiles": counters.device_attempt_tiles,
            "device_tiles": counters.device_tiles,
            "adaptive_cpu_tiles": counters.adaptive_cpu_tiles,
            "fallback_tiles": counters.fallback_tiles,
            "device_failure_fallback_tiles": counters.device_failure_fallback_tiles,
            "unavailable_fallback_tiles": counters.unavailable_fallback_tiles,
        }
    })))
}

fn route_activity(counters: RouteBackendSnapshot) -> bool {
    counters.device_attempt_tiles != 0
        || counters.device_tiles != 0
        || counters.adaptive_cpu_tiles != 0
        || counters.fallback_tiles != 0
}

fn route_delta(after: RouteBackendSnapshot, before: RouteBackendSnapshot) -> RouteBackendSnapshot {
    RouteBackendSnapshot {
        device_attempt_tiles: after
            .device_attempt_tiles
            .saturating_sub(before.device_attempt_tiles),
        device_tiles: after.device_tiles.saturating_sub(before.device_tiles),
        adaptive_cpu_tiles: after
            .adaptive_cpu_tiles
            .saturating_sub(before.adaptive_cpu_tiles),
        fallback_tiles: after.fallback_tiles.saturating_sub(before.fallback_tiles),
        device_failure_fallback_tiles: after
            .device_failure_fallback_tiles
            .saturating_sub(before.device_failure_fallback_tiles),
        unavailable_fallback_tiles: after
            .unavailable_fallback_tiles
            .saturating_sub(before.unavailable_fallback_tiles),
    }
}

fn elapsed_micros(started: Instant) -> u64 {
    let micros = started.elapsed().as_nanos().div_ceil(1_000);
    u64::try_from(micros).unwrap_or(u64::MAX)
}

fn hash_levels(checksum: &mut Sha256, levels: &[LevelInfo]) {
    for level in levels {
        checksum.update(level.width.to_le_bytes());
        checksum.update(level.height.to_le_bytes());
        checksum.update(level.downsample.to_bits().to_le_bytes());
    }
}

fn hash_bounds(checksum: &mut Sha256, bounds: Level0Bounds) {
    checksum.update(bounds.x.to_le_bytes());
    checksum.update(bounds.y.to_le_bytes());
    checksum.update(bounds.width.to_le_bytes());
    checksum.update(bounds.height.to_le_bytes());
}

fn full_level0_bounds(levels: &[LevelInfo]) -> Level0Bounds {
    let level0 = levels
        .first()
        .expect("validated OpenSlide metadata always has level zero");
    Level0Bounds {
        x: 0,
        y: 0,
        width: level0.width,
        height: level0.height,
    }
}

fn hash_read(checksum: &mut Sha256, spec: ReadSpec, pixels: &[u32]) {
    checksum.update(spec.x.to_le_bytes());
    checksum.update(spec.y.to_le_bytes());
    checksum.update(spec.level.to_le_bytes());
    checksum.update(spec.width.to_le_bytes());
    checksum.update(spec.height.to_le_bytes());
    for pixel in pixels {
        checksum.update(pixel.to_le_bytes());
    }
}

fn read_digest(spec: ReadSpec, pixels: &[u32]) -> [u8; 32] {
    let mut checksum = Sha256::new();
    hash_read(&mut checksum, spec, pixels);
    checksum.finalize().into()
}

pub fn sha256_file(path: &Path) -> Result<String, String> {
    let file = File::open(path)
        .map_err(|err| format!("failed to open {} for hashing: {err}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut checksum = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|err| format!("failed to hash {}: {err}", path.display()))?;
        if read == 0 {
            break;
        }
        checksum.update(&buffer[..read]);
    }
    Ok(format!("{:x}", checksum.finalize()))
}

impl From<LevelInfo> for LevelResult {
    fn from(level: LevelInfo) -> Self {
        Self {
            width: level.width,
            height: level.height,
            downsample: level.downsample,
        }
    }
}

impl From<OpenSlideLevel> for LevelInfo {
    fn from(level: OpenSlideLevel) -> Self {
        Self {
            width: level.width,
            height: level.height,
            downsample: level.downsample,
        }
    }
}

impl From<OpenSlideBounds> for Level0Bounds {
    fn from(bounds: OpenSlideBounds) -> Self {
        Self {
            x: bounds.x,
            y: bounds.y,
            width: bounds.width,
            height: bounds.height,
        }
    }
}

#[cfg(test)]
#[path = "tests/runner.rs"]
mod tests;
