use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::process::Command;

use serde::Deserialize;
use serde_json::{json, Value};

use super::capture::WorkerMatrix;
use super::comparison::{P95_MIN_SAMPLE_COUNT, P99_MIN_SAMPLE_COUNT, REGRESSION_RATIO};
use super::manifest::SlideSpec;
use super::worker::{
    cache_bytes, default_public_fixture, result_dir, workspace_root, BenchLibrary,
    DEFAULT_CACHE_BYTES,
};
use super::PERF_CAPTURE_SCHEMA_VERSION;

const TRACKED_ENV_VARS: [&str; 21] = [
    "RUSTFLAGS",
    "RAYON_NUM_THREADS",
    "WSI_RS_PERF_RESULTS_DIR",
    "WSI_RS_PERF_SLIDES",
    "WSI_RS_PERF_REPEATS",
    "WSI_RS_PERF_CACHE_BYTES",
    "WSI_RS_PERF_WORKERS",
    "WSI_RS_PERF_ONLY",
    "WSI_RS_PERF_MANIFEST",
    "WSI_RS_OPENSLIDE_LIBRARY",
    "OPENSLIDE_LIB_PATH",
    "WSI_RS_BENCH_WSI_RS_LIBRARY",
    "WSI_RS_TILE_CACHE_BYTES",
    "WSI_RS_DISPLAY_TILE_CACHE_BYTES",
    "WSI_RS_FULL_DECODE_CACHE_BYTES",
    "WSI_RS_NDPI_STRIP_CACHE_BYTES",
    "WSI_RS_SYNTHETIC_LEVEL_CACHE_BYTES",
    "WSI_RS_JPEG_DEVICE_DECODE",
    "WSI_RS_JP2K_DEVICE_DECODE",
    "WSI_RS_JP2K_DEVICE_BATCH",
    "WSI_RS_SHIM_JP2K_CPU_THREADS",
];

pub(super) fn capture_summary(
    label: &str,
    library: BenchLibrary,
    repeats: u32,
    slides: &[SlideSpec],
    worker_matrix: &WorkerMatrix,
    planned_workloads: &[String],
    runs: Vec<Value>,
) -> Result<Value, String> {
    let metadata = capture_metadata(library, slides, worker_matrix, planned_workloads, &runs)?;
    Ok(json!({
        "schema_version": PERF_CAPTURE_SCHEMA_VERSION,
        "kind": "wsi_rs-perf-capture",
        "label": label,
        "repeat_count": repeats,
        "slide_manifest": slides.iter().map(|slide| json!({
            "path": slide.path.display().to_string(),
            "alias": slide.alias,
            "format": slide.format,
            "benchmark_group": slide.benchmark_group,
            "manifest_sha256": slide.manifest_sha256,
        })).collect::<Vec<_>>(),
        "metadata": metadata,
        "runs": runs,
    }))
}

fn capture_metadata(
    library: BenchLibrary,
    slides: &[SlideSpec],
    worker_matrix: &WorkerMatrix,
    planned_workloads: &[String],
    runs: &[Value],
) -> Result<Value, String> {
    let rust_codec_dependencies = rust_codec_dependencies(&workspace_root().join("Cargo.lock"))?;
    let codec_thread_budget_enforced = runs
        .iter()
        .all(|run| run_decode_concurrency_matches(library, run))
        && !runs.is_empty();
    Ok(json!({
        "git": git_metadata(),
        "toolchain": {
            "rustc": command_stdout("rustc", &["--version"]),
            "cargo": command_stdout("cargo", &["--version"]),
        },
        "host": {
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "uname": command_stdout("uname", &["-a"]),
            "cpu": cpu_identity(),
            "gpu": gpu_identity(),
        },
        "build": {
            "profile": "release",
            "features": Vec::<String>::new(),
            "rustflags": std::env::var("RUSTFLAGS").ok(),
            "native_cpu_tuned": std::env::var("RUSTFLAGS")
                .is_ok_and(|value| value.contains("target-cpu=native")),
        },
        "benchmark": {
            "library": library.name(),
            "binary": library.binary(),
            "worker_package": "wsi-rs-perf",
            "worker_schema_version": wsi_rs_perf::WORKER_SCHEMA_VERSION,
            "rust_codec_dependencies": rust_codec_dependencies,
            "required_openslide_version": library.required_version_prefix(),
            "cache_bytes": cache_bytes().unwrap_or(DEFAULT_CACHE_BYTES),
            "client_worker_matrix": worker_matrix.counts,
            "client_process_concurrency": {
                "unit": "independent OpenSlide handles",
                "enforced_by_worker": true,
            },
            "physical_core_count": worker_matrix.physical_core_count,
            "physical_core_count_method": worker_matrix.physical_core_method,
            "internal_codec_thread_budget": {
                "enforced_by_harness": codec_thread_budget_enforced,
                "comparison_status": if codec_thread_budget_enforced { "equalized" } else { "not_equalized" },
                "per_run_field": "decode_cpu_concurrency",
                "wsi_rs": {
                    "process_wide_rayon_threads": "worker_count via RAYON_NUM_THREADS",
                    "jp2k_threads_per_handle": 1,
                    "control": "WSI_RS_SHIM_JP2K_CPU_THREADS",
                },
                "openslide": {
                    "decoder_threads_per_handle": 1,
                    "active_decode_threads": "worker_count independent handles",
                    "version": "4.0.1",
                },
            },
            "corpus_tier": corpus_tier(slides),
            "planned_workloads": planned_workloads,
            "result_dir": result_dir().display().to_string(),
            "regression_ratio": REGRESSION_RATIO,
            "tail_regression_min_samples": {
                "p95_us": P95_MIN_SAMPLE_COUNT,
                "p99_us": P99_MIN_SAMPLE_COUNT,
            },
            "required_public_fixture": default_public_fixture().display().to_string(),
        },
        "environment": tracked_environment(),
        "profiling": {
            "cpu_default": "samply",
            "macos_cpu_trace": "xcrun xctrace record --template 'Time Profiler'",
            "flamegraph": "optional diagnostic artifact; not a benchmark gate",
        }
    }))
}

#[derive(Deserialize)]
struct CargoLock {
    package: Vec<LockedPackage>,
}

#[derive(Deserialize)]
struct LockedPackage {
    name: String,
    version: String,
}

fn rust_codec_dependencies(path: &Path) -> Result<BTreeMap<String, Vec<String>>, String> {
    let text = fs::read_to_string(path).map_err(|err| {
        format!(
            "failed to read {} for codec metadata: {err}",
            path.display()
        )
    })?;
    let lock: CargoLock = toml::from_str(&text).map_err(|err| {
        format!(
            "failed to parse {} for codec metadata: {err}",
            path.display()
        )
    })?;
    let mut dependencies = BTreeMap::<String, BTreeSet<String>>::new();
    for package in lock.package {
        if is_codec_dependency(&package.name) {
            dependencies
                .entry(package.name)
                .or_default()
                .insert(package.version);
        }
    }
    if dependencies.is_empty() {
        return Err(format!(
            "{} contains no recognized Rust codec dependencies",
            path.display()
        ));
    }
    Ok(dependencies
        .into_iter()
        .map(|(name, versions)| (name, versions.into_iter().collect()))
        .collect())
}

fn is_codec_dependency(name: &str) -> bool {
    name.starts_with("j2k")
        || name.starts_with("jpeg")
        || name.starts_with("zstd")
        || name.starts_with("dicom-")
        || matches!(name, "image" | "png" | "flate2" | "weezl")
}

fn run_decode_concurrency_matches(library: BenchLibrary, run: &Value) -> bool {
    let Some(workers) = run.get("worker_count").and_then(Value::as_u64) else {
        return false;
    };
    let Some(control) = run.get("decode_cpu_concurrency") else {
        return false;
    };
    if control.get("enforced").and_then(Value::as_bool) != Some(true)
        || control.get("client_handles").and_then(Value::as_u64) != Some(workers)
    {
        return false;
    }
    match library {
        BenchLibrary::WsiRs => {
            control
                .get("rayon_threads_process_wide")
                .and_then(Value::as_u64)
                == Some(workers)
                && control
                    .get("jp2k_threads_per_handle")
                    .and_then(Value::as_u64)
                    == Some(1)
                && control
                    .get("active_jp2k_thread_budget")
                    .and_then(Value::as_u64)
                    == Some(workers)
        }
        BenchLibrary::OpenSlide => {
            control
                .get("decoder_threads_per_handle")
                .and_then(Value::as_u64)
                == Some(1)
                && control
                    .get("active_decode_thread_budget")
                    .and_then(Value::as_u64)
                    == Some(workers)
        }
    }
}

fn git_metadata() -> Value {
    json!({
        "branch": command_stdout("git", &["branch", "--show-current"]),
        "commit": command_stdout("git", &["rev-parse", "HEAD"]),
        "dirty": !command_stdout("git", &["status", "--porcelain"]).is_empty(),
        "status_short": command_stdout("git", &["status", "--short"]),
    })
}

fn tracked_environment() -> Value {
    let mut env = serde_json::Map::new();
    for name in TRACKED_ENV_VARS {
        env.insert(
            name.to_string(),
            std::env::var(name)
                .map(Value::String)
                .unwrap_or(Value::Null),
        );
    }
    Value::Object(env)
}

fn command_stdout(program: &str, args: &[&str]) -> String {
    match Command::new(program).args(args).output() {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        }
        Ok(output) => format!(
            "unavailable: {program} exited with {}; {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ),
        Err(err) => format!("unavailable: failed to run {program}: {err}"),
    }
}

fn cpu_identity() -> String {
    if cfg!(target_os = "macos") {
        let brand = command_stdout("sysctl", &["-n", "machdep.cpu.brand_string"]);
        let model = command_stdout("sysctl", &["-n", "hw.model"]);
        let cores = command_stdout("sysctl", &["-n", "hw.ncpu"]);
        return format!("model={model}; cores={cores}; brand={brand}");
    }
    command_stdout(
        "sh",
        &[
            "-c",
            "grep -m1 'model name' /proc/cpuinfo 2>/dev/null || uname -m",
        ],
    )
}

fn gpu_identity() -> String {
    if cfg!(target_os = "macos") {
        let text = command_stdout(
            "system_profiler",
            &["SPDisplaysDataType", "-detailLevel", "mini"],
        );
        let names = text
            .lines()
            .filter_map(|line| line.trim().strip_prefix("Chipset Model:"))
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>();
        if !names.is_empty() {
            return names.join("; ");
        }
        return text;
    }
    "unavailable: GPU metadata is only collected by default on macOS".into()
}

fn corpus_tier(slides: &[SlideSpec]) -> &'static str {
    if slides.iter().all(|slide| {
        let path = slide.path.to_string_lossy();
        path.contains("tests/fixtures/")
    }) {
        return "public-fixture";
    }
    if slides.iter().any(|slide| {
        let path = slide.path.to_string_lossy();
        path.contains(".cache/slideviewer/parity-corpus")
    }) {
        return "local-parity";
    }
    "custom"
}

#[cfg(test)]
#[path = "tests/metadata.rs"]
mod tests;
