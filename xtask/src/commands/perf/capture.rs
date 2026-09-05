use std::collections::BTreeSet;
use std::process::Command;

use serde_json::Value;

use super::checksum::{
    validate_capture_checksums, validate_declared_capture_plan, validate_worker_run_typed,
};
use super::manifest::{load_manifest, resolve_manifest_slides, SlideSpec};
use super::metadata::capture_summary;
use super::process_metrics::annotate_run_resource_usage_typed;
use super::schema::CaptureRun;
use super::worker::{
    cache_bytes, prepare_bench, prepare_pair, result_dir, run_bench, BenchInvocation, BenchLibrary,
};

const DEFAULT_REPEAT_COUNT: u32 = 5;
const SLIDES_ENV: &str = "WSI_RS_PERF_SLIDES";
const REPEATS_ENV: &str = "WSI_RS_PERF_REPEATS";
const WORKERS_ENV: &str = "WSI_RS_PERF_WORKERS";
const ONLY_WORKLOAD_ENV: &str = "WSI_RS_PERF_ONLY";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WorkerMatrix {
    pub(super) counts: Vec<usize>,
    pub(super) physical_core_count: usize,
    pub(super) physical_core_method: String,
}

struct CaptureSettings {
    cache_bytes: usize,
    only_workload: Option<String>,
    planned_workloads: Vec<String>,
}

struct RunSpec<'a> {
    slide: &'a SlideSpec,
    repeat: u32,
    workers: usize,
    engine_order: &'a [BenchLibrary],
    engine_position: usize,
}

pub(in crate::commands) fn capture(args: Vec<String>) -> Result<(), String> {
    capture_single(args, BenchLibrary::WsiRs)
}

pub(in crate::commands) fn capture_openslide(args: Vec<String>) -> Result<(), String> {
    capture_single(args, BenchLibrary::OpenSlide)
}

pub(in crate::commands) fn capture_pair(args: Vec<String>) -> Result<(), String> {
    let (label, selectors) = capture_arguments(&args, "perf-capture-pair")?;
    let manifest = load_manifest()?;
    let slides = resolve_manifest_slides(&manifest, &selectors, true, false)?;
    let repeats = repeat_count()?;
    let worker_matrix = requested_worker_matrix()?;
    let settings = capture_settings()?;
    let (wsi_rs, openslide) = prepare_pair()?;
    let mut wsi_rs_runs = Vec::new();
    let mut openslide_runs = Vec::new();

    for repeat in 0..repeats {
        let engine_order = paired_engine_order(repeat);
        for &workers in &worker_matrix.counts {
            for slide in &slides {
                for (engine_position, library) in engine_order.into_iter().enumerate() {
                    let invocation = match library {
                        BenchLibrary::WsiRs => &wsi_rs,
                        BenchLibrary::OpenSlide => &openslide,
                    };
                    let run = capture_run(
                        library,
                        invocation,
                        &settings,
                        RunSpec {
                            slide,
                            repeat,
                            workers,
                            engine_order: &engine_order,
                            engine_position,
                        },
                    )?;
                    match library {
                        BenchLibrary::WsiRs => wsi_rs_runs.push(run),
                        BenchLibrary::OpenSlide => openslide_runs.push(run),
                    }
                }
            }
        }
    }

    write_capture(
        &format!("{label}-wsi_rs"),
        BenchLibrary::WsiRs,
        repeats,
        &slides,
        &worker_matrix,
        &settings.planned_workloads,
        wsi_rs_runs,
    )?;
    write_capture(
        &format!("{label}-openslide"),
        BenchLibrary::OpenSlide,
        repeats,
        &slides,
        &worker_matrix,
        &settings.planned_workloads,
        openslide_runs,
    )
}

fn capture_single(args: Vec<String>, library: BenchLibrary) -> Result<(), String> {
    let (label, selectors) = capture_arguments(&args, capture_task_name(library))?;
    let slides = resolve_single_engine_slides(&selectors, library)?;
    let repeats = repeat_count()?;
    let worker_matrix = requested_worker_matrix()?;
    let settings = capture_settings()?;
    let invocation = prepare_bench(library)?;
    let engine_order = [library];
    let mut runs = Vec::new();

    for repeat in 0..repeats {
        for &workers in &worker_matrix.counts {
            for slide in &slides {
                runs.push(capture_run(
                    library,
                    &invocation,
                    &settings,
                    RunSpec {
                        slide,
                        repeat,
                        workers,
                        engine_order: &engine_order,
                        engine_position: 0,
                    },
                )?);
            }
        }
    }

    write_capture(
        label,
        library,
        repeats,
        &slides,
        &worker_matrix,
        &settings.planned_workloads,
        runs,
    )
}

fn capture_settings() -> Result<CaptureSettings, String> {
    let only_workload = std::env::var(ONLY_WORKLOAD_ENV).ok();
    let planned_workloads = only_workload.clone().map_or_else(
        || {
            wsi_rs_perf::CAPTURE_WORKLOAD_NAMES
                .iter()
                .map(ToString::to_string)
                .collect()
        },
        |workload| vec![workload],
    );
    Ok(CaptureSettings {
        cache_bytes: cache_bytes()?,
        only_workload,
        planned_workloads,
    })
}

fn capture_run(
    library: BenchLibrary,
    invocation: &BenchInvocation,
    settings: &CaptureSettings,
    run_spec: RunSpec<'_>,
) -> Result<CaptureRun, String> {
    let output = run_bench(
        library,
        invocation,
        &run_spec.slide.path,
        run_spec.repeat,
        settings.cache_bytes,
        run_spec.workers,
        settings.only_workload.as_deref(),
    )?;
    let worker: wsi_rs_perf::WorkerResult = serde_json::from_slice(&output.process.stdout)
        .map_err(|err| {
            format!(
                "invalid {} JSON for {}: {err}",
                library.binary(),
                run_spec.slide.path.display()
            )
        })?;
    let mut run = CaptureRun::from_worker(worker);
    validate_worker_run_typed(&run, library.name(), &run_spec.slide.path, run_spec.repeat)?;
    annotate_run_resource_usage_typed(&mut run, &output.process.stderr);
    annotate_run_context_typed(
        &mut run,
        output.decode_cpu_concurrency,
        run_spec.slide,
        run_spec.workers,
        run_spec.engine_order,
        run_spec.engine_position,
    )?;
    Ok(run)
}

fn annotate_run_context_typed(
    run: &mut CaptureRun,
    decode_cpu_concurrency: Value,
    slide: &SlideSpec,
    workers: usize,
    engine_order: &[BenchLibrary],
    engine_position: usize,
) -> Result<(), String> {
    if run.worker_count != Some(workers as u64) {
        return Err(format!(
            "performance worker count did not match requested workers={workers}"
        ));
    }
    run.alias = Some(slide.alias.clone());
    run.format = Some(slide.format.clone());
    run.benchmark_group = Some(slide.benchmark_group.clone());
    run.engine_position = Some(engine_position);
    run.decode_cpu_concurrency = Some(decode_cpu_concurrency);
    run.engine_order = engine_order
        .iter()
        .map(|library| library.name().to_string())
        .collect();
    run.manifest_sha256.clone_from(&slide.manifest_sha256);
    Ok(())
}

#[cfg(test)]
fn annotate_run_context(
    run: &mut Value,
    decode_cpu_concurrency: Value,
    slide: &SlideSpec,
    workers: usize,
    engine_order: &[BenchLibrary],
    engine_position: usize,
) -> Result<(), String> {
    if !run.is_object() {
        return Err("performance worker JSON must be an object".into());
    }
    let mut typed = serde_json::from_value::<CaptureRun>(run.clone())
        .map_err(|error| format!("invalid test run JSON: {error}"))?;
    annotate_run_context_typed(
        &mut typed,
        decode_cpu_concurrency,
        slide,
        workers,
        engine_order,
        engine_position,
    )?;
    *run = serde_json::to_value(typed).map_err(|error| error.to_string())?;
    Ok(())
}

fn write_capture(
    label: &str,
    library: BenchLibrary,
    repeats: u32,
    slides: &[SlideSpec],
    worker_matrix: &WorkerMatrix,
    planned_workloads: &[String],
    runs: Vec<CaptureRun>,
) -> Result<(), String> {
    let output_path = result_dir().join(format!("{label}.json"));
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("failed to create {}: {err}", parent.display()))?;
    }
    let runs = runs
        .into_iter()
        .map(|run| serde_json::to_value(run).map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    let summary = capture_summary(
        label,
        library,
        repeats,
        slides,
        worker_matrix,
        planned_workloads,
        runs,
    )?;
    validate_declared_capture_plan(&summary)?;
    validate_capture_checksums(&summary)?;
    std::fs::write(
        &output_path,
        serde_json::to_vec_pretty(&summary).map_err(|err| err.to_string())?,
    )
    .map_err(|err| format!("failed to write {}: {err}", output_path.display()))?;
    println!("{}", output_path.display());
    Ok(())
}

fn capture_arguments<'a>(
    args: &'a [String],
    task_name: &str,
) -> Result<(&'a str, Vec<String>), String> {
    let Some(label) = args.first() else {
        return Err(format!(
            "usage: cargo xtask {task_name} <label> [aliases-or-slides...]"
        ));
    };
    let selectors = if args.len() > 1 {
        args[1..].to_vec()
    } else {
        std::env::var_os(SLIDES_ENV)
            .map(|value| {
                std::env::split_paths(&value)
                    .map(|path| path.to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default()
    };
    Ok((label, selectors))
}

fn resolve_single_engine_slides(
    selectors: &[String],
    library: BenchLibrary,
) -> Result<Vec<SlideSpec>, String> {
    resolve_manifest_slides(
        &load_manifest()?,
        selectors,
        matches!(library, BenchLibrary::OpenSlide),
        true,
    )
}

fn capture_task_name(library: BenchLibrary) -> &'static str {
    match library {
        BenchLibrary::WsiRs => "perf-capture",
        BenchLibrary::OpenSlide => "perf-capture-openslide",
    }
}

fn paired_engine_order(repeat: u32) -> [BenchLibrary; 2] {
    if repeat.is_multiple_of(2) {
        [BenchLibrary::WsiRs, BenchLibrary::OpenSlide]
    } else {
        [BenchLibrary::OpenSlide, BenchLibrary::WsiRs]
    }
}

pub(super) fn requested_worker_matrix() -> Result<WorkerMatrix, String> {
    let (physical_core_count, physical_core_method) = physical_core_count();
    let counts = match std::env::var(WORKERS_ENV) {
        Ok(value) => parse_worker_matrix(&value)?,
        Err(std::env::VarError::NotPresent) => default_worker_matrix(physical_core_count),
        Err(err) => return Err(format!("failed to read {WORKERS_ENV}: {err}")),
    };
    Ok(WorkerMatrix {
        counts,
        physical_core_count,
        physical_core_method,
    })
}

pub(super) fn worker_count() -> Result<usize, String> {
    requested_worker_matrix()?
        .counts
        .into_iter()
        .next()
        .ok_or_else(|| "performance worker matrix is empty".into())
}

fn default_worker_matrix(physical_cores: usize) -> Vec<usize> {
    [1, 2, physical_cores.max(1)]
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn parse_worker_matrix(raw: &str) -> Result<Vec<usize>, String> {
    let counts = raw
        .split(|ch: char| ch == ',' || ch == ';' || ch.is_whitespace())
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .parse::<usize>()
                .map_err(|err| format!("invalid {WORKERS_ENV}={raw:?}: {err}"))
                .and_then(|count| {
                    if count == 0 {
                        Err(format!("{WORKERS_ENV} values must be positive"))
                    } else {
                        Ok(count)
                    }
                })
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    if counts.is_empty() {
        return Err(format!("{WORKERS_ENV} must contain at least one count"));
    }
    Ok(counts.into_iter().collect())
}

fn physical_core_count() -> (usize, String) {
    if cfg!(target_os = "macos") {
        if let Ok(output) = Command::new("sysctl")
            .args(["-n", "hw.physicalcpu"])
            .output()
        {
            if output.status.success() {
                if let Some(count) = parse_positive_usize(&String::from_utf8_lossy(&output.stdout))
                {
                    return (count, "macos:sysctl -n hw.physicalcpu".into());
                }
            }
        }
    }
    if cfg!(target_os = "linux") {
        if let Ok(output) = Command::new("lscpu").arg("-p=CORE,SOCKET").output() {
            if output.status.success() {
                if let Some(count) =
                    parse_linux_physical_cores(&String::from_utf8_lossy(&output.stdout))
                {
                    return (count, "linux:lscpu -p=CORE,SOCKET".into());
                }
            }
        }
    }
    let count = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1);
    (
        count,
        "fallback:std::thread::available_parallelism (logical CPUs)".into(),
    )
}

fn parse_positive_usize(raw: &str) -> Option<usize> {
    raw.trim().parse::<usize>().ok().filter(|value| *value > 0)
}

fn parse_linux_physical_cores(raw: &str) -> Option<usize> {
    let pairs = raw
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| {
            let (core, socket) = line.split_once(',')?;
            let core = core.trim();
            let socket = socket.trim();
            (!core.is_empty() && core != "-" && !socket.is_empty() && socket != "-")
                .then(|| (socket.to_string(), core.to_string()))
        })
        .collect::<BTreeSet<_>>();
    (!pairs.is_empty()).then_some(pairs.len())
}

fn repeat_count() -> Result<u32, String> {
    match std::env::var(REPEATS_ENV) {
        Ok(value) => value
            .parse::<u32>()
            .map_err(|err| format!("invalid {REPEATS_ENV}={value:?}: {err}"))
            .and_then(|value| {
                if value >= DEFAULT_REPEAT_COUNT {
                    Ok(value)
                } else {
                    Err(format!(
                        "{REPEATS_ENV} must be at least {DEFAULT_REPEAT_COUNT}"
                    ))
                }
            }),
        Err(std::env::VarError::NotPresent) => Ok(DEFAULT_REPEAT_COUNT),
        Err(err) => Err(format!("failed to read {REPEATS_ENV}: {err}")),
    }
}

#[cfg(test)]
#[path = "tests/capture.rs"]
mod tests;
