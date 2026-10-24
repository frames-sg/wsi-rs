use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};
use wsi_rs_perf::{Engine, WorkerConfig, DEFAULT_CACHE_BYTES, PINNED_OPENSLIDE_VERSION};
use wsi_rs_test_support::openslide::{configured_library, LIBRARY_ENV_VARS};

const CACHE_BYTES_ENV: &str = "WSI_RS_PERF_CACHE_BYTES";
const RESULT_DIR_ENV: &str = "WSI_RS_PERF_RESULTS_DIR";
const WSI_RS_LIBRARY_ENV: &str = "WSI_RS_BENCH_WSI_RS_LIBRARY";
const PREVIOUS_LIBRARY_ENV: &str = "WSI_RS_BENCH_PREVIOUS_LIBRARY";
pub(super) const RAYON_NUM_THREADS_ENV: &str = "RAYON_NUM_THREADS";
/// wsi-rs 0.6 shims read this per-handle JP2K pool width and otherwise use
/// every core, ignoring RAYON_NUM_THREADS. Later shims ignore it.
pub(super) const LEGACY_SHIM_JP2K_THREADS_ENV: &str = "WSI_RS_SHIM_JP2K_CPU_THREADS";

/// The performance worker's package and binary name.
pub(super) const WORKER_BINARY: &str = "wsi-rs-perf";

pub(super) fn performance_gpu_feature() -> Result<Option<String>, String> {
    match std::env::var("WSI_RS_PERF_GPU_FEATURE") {
        Ok(feature) if matches!(feature.as_str(), "metal" | "cuda") => Ok(Some(feature)),
        Ok(feature) => Err(format!(
            "invalid WSI_RS_PERF_GPU_FEATURE={feature:?}; expected metal or cuda"
        )),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(error) => Err(format!("failed to read WSI_RS_PERF_GPU_FEATURE: {error}")),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BenchInvocation {
    pub(super) worker: PathBuf,
    pub(super) library: PathBuf,
}

pub(super) struct BenchOutput {
    pub(super) process: std::process::Output,
    pub(super) decode_cpu_concurrency: Value,
}

/// The OpenSlide version prefix the worker must find for `engine`: the
/// pinned release for OpenSlide, none for the wsi-rs shim.
pub(super) fn required_version_prefix(engine: Engine) -> Option<&'static str> {
    match engine {
        Engine::WsiRs => None,
        Engine::OpenSlide => Some(PINNED_OPENSLIDE_VERSION),
    }
}

pub(super) fn run_bench(
    library: Engine,
    invocation: &BenchInvocation,
    slide: &Path,
    repeat: u32,
    cache_bytes: usize,
    workers: usize,
    only_workload: Option<&str>,
) -> Result<BenchOutput, String> {
    let mut command = if cfg!(target_os = "macos") {
        let mut command = Command::new("/usr/bin/time");
        command.arg("-l").arg(&invocation.worker);
        command
    } else if cfg!(target_os = "linux") && Path::new("/usr/bin/time").is_file() {
        let mut command = Command::new("/usr/bin/time");
        command.arg("-v").arg(&invocation.worker);
        command
    } else {
        Command::new(&invocation.worker)
    };
    let config = worker_config(
        library,
        invocation,
        slide,
        repeat,
        cache_bytes,
        workers,
        only_workload,
    );
    let decode_cpu_concurrency = configure_worker_environment(&mut command, library, workers);
    let output = command
        .args(config.to_args())
        .output()
        .map_err(|err| format!("failed to run {}: {err}", invocation.worker.display()))?;
    if output.status.success() {
        Ok(BenchOutput {
            process: output,
            decode_cpu_concurrency,
        })
    } else {
        Err(format!(
            "{WORKER_BINARY} failed for {} repeat {repeat}: {}",
            slide.display(),
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

pub(super) fn run_pixel_comparison(
    candidate: &BenchInvocation,
    reference: &BenchInvocation,
    slide: &Path,
    cache_bytes: usize,
    only_workload: Option<&str>,
) -> Result<wsi_rs_perf::PixelComparison, String> {
    let config = WorkerConfig {
        comparison_library: Some(reference.library.clone()),
        ..worker_config(
            Engine::WsiRs,
            candidate,
            slide,
            0,
            cache_bytes,
            1,
            only_workload,
        )
    };
    let mut command = Command::new(&candidate.worker);
    command.args(config.to_args());
    configure_worker_environment(&mut command, Engine::WsiRs, 1);
    let output = command
        .output()
        .map_err(|error| format!("pixel comparison: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "pixel comparison for {}: {}",
            slide.display(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("invalid pixel comparison: {error}"))
}

fn configure_worker_environment(command: &mut Command, library: Engine, workers: usize) -> Value {
    match library {
        Engine::WsiRs => {
            command.env(RAYON_NUM_THREADS_ENV, workers.to_string());
            // Keep a previous-release baseline on the same budget: N handles
            // with one JP2K thread each, as the 0.6 harness configured it.
            command.env(LEGACY_SHIM_JP2K_THREADS_ENV, "1");
        }
        Engine::OpenSlide => {
            command.env_remove(RAYON_NUM_THREADS_ENV);
            command.env_remove(LEGACY_SHIM_JP2K_THREADS_ENV);
        }
    }
    decode_cpu_concurrency(library, workers)
}

pub(super) fn decode_cpu_concurrency(library: Engine, workers: usize) -> Value {
    match library {
        Engine::WsiRs => json!({
            "client_handles": workers,
            "rayon_threads_process_wide": workers,
            "active_jp2k_thread_budget": workers,
            "enforced": true,
            "method": "RAYON_NUM_THREADS=N process-wide JP2K pool shared by N client handles",
            "legacy_per_handle_jp2k_threads": 1,
        }),
        Engine::OpenSlide => json!({
            "client_handles": workers,
            "decoder_threads_per_handle": 1,
            "active_decode_thread_budget": workers,
            "enforced": true,
            "method": format!(
                "N independent client handles with one decoder thread per handle in pinned \
                 OpenSlide {PINNED_OPENSLIDE_VERSION}"
            ),
        }),
    }
}

/// The worker run for `library` on `slide`, requiring the pinned OpenSlide
/// version where [`required_version_prefix`] asks for one.
pub(super) fn worker_config(
    library: Engine,
    invocation: &BenchInvocation,
    slide: &Path,
    repeat: u32,
    cache_bytes: usize,
    workers: usize,
    only_workload: Option<&str>,
) -> WorkerConfig {
    WorkerConfig {
        engine: library,
        library_path: invocation.library.clone(),
        slide_path: slide.to_path_buf(),
        repeat_index: repeat,
        cache_bytes,
        workers,
        only: only_workload.map(str::to_string),
        required_version_prefix: required_version_prefix(library).map(str::to_string),
        comparison_library: None,
    }
}

pub(super) fn prepare_bench(library: Engine) -> Result<BenchInvocation, String> {
    build_package(WORKER_BINARY)?;
    if matches!(library, Engine::WsiRs) && std::env::var_os(WSI_RS_LIBRARY_ENV).is_none() {
        build_wsi_rs_shim()?;
    }
    invocation(library, &target_directory()?)
}

pub(super) fn prepare_pair() -> Result<(BenchInvocation, BenchInvocation), String> {
    build_package(WORKER_BINARY)?;
    if std::env::var_os(WSI_RS_LIBRARY_ENV).is_none() {
        build_wsi_rs_shim()?;
    }
    let target_dir = target_directory()?;
    Ok((
        invocation(Engine::WsiRs, &target_dir)?,
        invocation(Engine::OpenSlide, &target_dir)?,
    ))
}

pub(super) fn previous_invocation(
    current: &BenchInvocation,
) -> Result<Option<BenchInvocation>, String> {
    std::env::var_os(PREVIOUS_LIBRARY_ENV)
        .map(|path| {
            Ok(BenchInvocation {
                worker: current.worker.clone(),
                library: exact_library_path(Path::new(&path), "previous wsi-rs OpenSlide shim")?,
            })
        })
        .transpose()
}

fn invocation(library: Engine, target_dir: &Path) -> Result<BenchInvocation, String> {
    let worker = worker_path(target_dir);
    let library_path = match library {
        Engine::WsiRs => {
            let candidate = wsi_rs_library_candidate(
                target_dir,
                std::env::var_os(WSI_RS_LIBRARY_ENV).as_deref(),
            );
            exact_library_path(&candidate, "wsi-rs OpenSlide shim")?
        }
        Engine::OpenSlide => explicit_openslide_library()?,
    };
    if !worker.is_file() {
        return Err(format!(
            "performance worker was not built: {}",
            worker.display()
        ));
    }
    Ok(BenchInvocation {
        worker,
        library: library_path,
    })
}

/// The release worker binary in `target_dir`.
pub(super) fn worker_path(target_dir: &Path) -> PathBuf {
    target_dir
        .join("release")
        .join(format!("{WORKER_BINARY}{}", std::env::consts::EXE_SUFFIX))
}

fn wsi_rs_library_candidate(target_dir: &Path, override_path: Option<&OsStr>) -> PathBuf {
    override_path.map_or_else(
        || target_dir.join("release").join(shim_library_name()),
        PathBuf::from,
    )
}

fn explicit_openslide_library() -> Result<PathBuf, String> {
    explicit_openslide_library_from(configured_library())
}

fn explicit_openslide_library_from(configured: Option<PathBuf>) -> Result<PathBuf, String> {
    let [primary, fallback] = LIBRARY_ENV_VARS;
    let path = configured.ok_or_else(|| {
        format!(
            "OpenSlide capture requires an exact {PINNED_OPENSLIDE_VERSION} library path in \
                 {primary} (or {fallback})"
        )
    })?;
    exact_library_path(&path, "OpenSlide library")
}

fn exact_library_path(path: &Path, description: &str) -> Result<PathBuf, String> {
    if !path.is_file() {
        return Err(format!("{description} is not a file: {}", path.display()));
    }
    path.canonicalize()
        .map_err(|err| format!("failed to canonicalize {}: {err}", path.display()))
}

fn build_package(package: &str) -> Result<(), String> {
    eprintln!("+ cargo build --locked --release -p {package}");
    let status = Command::new(cargo())
        .args(["build", "--locked", "--release", "-p", package])
        .status()
        .map_err(|err| format!("failed to build {package}: {err}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "failed to build {package}: cargo exited with {status}"
        ))
    }
}

fn build_wsi_rs_shim() -> Result<(), String> {
    let feature = performance_gpu_feature()?;
    let mut features = vec!["route-telemetry".to_string()];
    let mut args = vec![
        "build".to_string(),
        "--locked".to_string(),
        "--release".to_string(),
        "-p".to_string(),
        "wsi-rs-openslide-shim".to_string(),
    ];
    if let Some(feature) = feature {
        features.push(format!("wsi-rs/{feature}"));
    }
    args.extend(["--features".to_string(), features.join(",")]);
    eprintln!("+ cargo {}", args.join(" "));
    let status = Command::new(cargo())
        .args(&args)
        .status()
        .map_err(|err| format!("failed to build wsi-rs-openslide-shim: {err}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "cargo build for wsi-rs-openslide-shim exited with {status}"
        ))
    }
}

pub(super) fn target_directory() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("CARGO_TARGET_DIR") {
        let path = PathBuf::from(path);
        return Ok(if path.is_absolute() {
            path
        } else {
            workspace_root().join(path)
        });
    }
    let output = Command::new(cargo())
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .output()
        .map_err(|err| format!("failed to run cargo metadata: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let metadata: Value = serde_json::from_slice(&output.stdout)
        .map_err(|err| format!("invalid cargo metadata JSON: {err}"))?;
    metadata
        .get("target_directory")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| "cargo metadata did not report target_directory".into())
}

pub(super) fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask manifest has a workspace parent")
        .to_path_buf()
}

pub(super) fn shim_library_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "wsi_rs_openslide_shim.dll"
    } else if cfg!(target_os = "macos") {
        "libwsi_rs_openslide_shim.dylib"
    } else {
        "libwsi_rs_openslide_shim.so"
    }
}

pub(super) fn cache_bytes() -> Result<usize, String> {
    match std::env::var(CACHE_BYTES_ENV) {
        Ok(value) => {
            let bytes = value
                .parse::<usize>()
                .map_err(|err| format!("invalid {CACHE_BYTES_ENV}={value:?}: {err}"))?;
            if bytes == 0 {
                return Err(format!("{CACHE_BYTES_ENV} must be positive"));
            }
            Ok(bytes)
        }
        Err(std::env::VarError::NotPresent) => Ok(DEFAULT_CACHE_BYTES),
        Err(err) => Err(format!("failed to read {CACHE_BYTES_ENV}: {err}")),
    }
}

pub(super) fn result_dir() -> PathBuf {
    std::env::var_os(RESULT_DIR_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let host = command_text(Command::new("hostname"))
                .unwrap_or_else(|| format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH));
            let commit =
                git_text(&["rev-parse", "--short=12", "HEAD"]).unwrap_or_else(|| "unknown".into());
            let dirty = git_status_dirty();
            default_result_dir(&host, &commit, dirty)
        })
}

fn default_result_dir(host: &str, commit: &str, dirty: Option<bool>) -> PathBuf {
    let revision = match dirty {
        Some(true) => format!("{}-dirty", path_component(commit)),
        Some(false) => path_component(commit),
        None => format!("{}-state-unknown", path_component(commit)),
    };
    PathBuf::from("bench/results")
        .join(path_component(host))
        .join(revision)
}

fn path_component(raw: &str) -> String {
    let value = raw
        .trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if value.is_empty() {
        "unknown".into()
    } else {
        value
    }
}

fn command_text(mut command: Command) -> Option<String> {
    let output = command.output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn git_text(args: &[&str]) -> Option<String> {
    let mut command = Command::new("git");
    command.current_dir(workspace_root()).args(args);
    command_text(command).filter(|value| !value.is_empty())
}

fn git_status_dirty() -> Option<bool> {
    let mut command = Command::new("git");
    let output = command
        .current_dir(workspace_root())
        .args(["status", "--porcelain", "--untracked-files=normal"])
        .output()
        .ok()?;
    output.status.success().then_some(!output.stdout.is_empty())
}

pub(super) fn default_public_fixture() -> PathBuf {
    workspace_root()
        .join("tests")
        .join("fixtures")
        .join("jp2k")
        .join("rgb_nomct.j2k")
}

fn cargo() -> OsString {
    std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"))
}

#[cfg(test)]
#[path = "tests/worker.rs"]
mod tests;
