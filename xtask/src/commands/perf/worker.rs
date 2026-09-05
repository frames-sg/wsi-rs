use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

const CACHE_BYTES_ENV: &str = "WSI_RS_PERF_CACHE_BYTES";
const OPENSLIDE_LIBRARY_ENV: &str = "WSI_RS_OPENSLIDE_LIBRARY";
const OPENSLIDE_LIBRARY_FALLBACK_ENV: &str = "OPENSLIDE_LIB_PATH";
const RESULT_DIR_ENV: &str = "WSI_RS_PERF_RESULTS_DIR";
const WSI_RS_LIBRARY_ENV: &str = "WSI_RS_BENCH_WSI_RS_LIBRARY";
pub(super) const RAYON_NUM_THREADS_ENV: &str = "RAYON_NUM_THREADS";

pub(super) const DEFAULT_CACHE_BYTES: usize = 256 * 1024 * 1024;
pub(super) const PINNED_OPENSLIDE_VERSION: &str = "4.0.1";

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BenchLibrary {
    WsiRs,
    OpenSlide,
}

impl BenchLibrary {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::WsiRs => "wsi_rs",
            Self::OpenSlide => "openslide",
        }
    }

    pub(super) fn binary(self) -> &'static str {
        "wsi-rs-perf"
    }

    pub(super) fn required_version_prefix(self) -> Option<&'static str> {
        match self {
            Self::WsiRs => None,
            Self::OpenSlide => Some(PINNED_OPENSLIDE_VERSION),
        }
    }
}

pub(super) fn run_bench(
    library: BenchLibrary,
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
    let args = worker_args(
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
        .args(&args)
        .output()
        .map_err(|err| format!("failed to run {}: {err}", invocation.worker.display()))?;
    if output.status.success() {
        Ok(BenchOutput {
            process: output,
            decode_cpu_concurrency,
        })
    } else {
        Err(format!(
            "{} failed for {} repeat {repeat}: {}",
            library.binary(),
            slide.display(),
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

fn configure_worker_environment(
    command: &mut Command,
    library: BenchLibrary,
    workers: usize,
) -> Value {
    match library {
        BenchLibrary::WsiRs => {
            command.env(RAYON_NUM_THREADS_ENV, workers.to_string());
        }
        BenchLibrary::OpenSlide => {
            command.env_remove(RAYON_NUM_THREADS_ENV);
        }
    }
    decode_cpu_concurrency(library, workers)
}

pub(super) fn decode_cpu_concurrency(library: BenchLibrary, workers: usize) -> Value {
    match library {
        BenchLibrary::WsiRs => json!({
            "client_handles": workers,
            "rayon_threads_process_wide": workers,
            "active_jp2k_thread_budget": workers,
            "enforced": true,
            "method": "RAYON_NUM_THREADS=N process-wide JP2K pool shared by N client handles",
        }),
        BenchLibrary::OpenSlide => json!({
            "client_handles": workers,
            "decoder_threads_per_handle": 1,
            "active_decode_thread_budget": workers,
            "enforced": true,
            "method": "N independent client handles with one decoder thread per handle in pinned OpenSlide 4.0.1",
        }),
    }
}

pub(super) fn worker_args(
    library: BenchLibrary,
    invocation: &BenchInvocation,
    slide: &Path,
    repeat: u32,
    cache_bytes: usize,
    workers: usize,
    only_workload: Option<&str>,
) -> Vec<String> {
    let mut args = vec![
        "--engine".to_string(),
        library.name().to_string(),
        "--library".to_string(),
        invocation.library.display().to_string(),
        "--slide".to_string(),
        slide.display().to_string(),
        "--repeat-index".to_string(),
        repeat.to_string(),
        "--cache-bytes".to_string(),
        cache_bytes.to_string(),
        "--workers".to_string(),
        workers.to_string(),
    ];
    if let Some(workload) = only_workload {
        args.extend(["--only".to_string(), workload.to_string()]);
    }
    if let Some(version) = library.required_version_prefix() {
        args.extend(["--require-version-prefix".to_string(), version.to_string()]);
    }
    args
}

pub(super) fn prepare_bench(library: BenchLibrary) -> Result<BenchInvocation, String> {
    build_package("wsi-rs-perf")?;
    if matches!(library, BenchLibrary::WsiRs) && std::env::var_os(WSI_RS_LIBRARY_ENV).is_none() {
        build_wsi_rs_shim()?;
    }
    invocation(library, &target_directory()?)
}

pub(super) fn prepare_pair() -> Result<(BenchInvocation, BenchInvocation), String> {
    build_package("wsi-rs-perf")?;
    if std::env::var_os(WSI_RS_LIBRARY_ENV).is_none() {
        build_wsi_rs_shim()?;
    }
    let target_dir = target_directory()?;
    Ok((
        invocation(BenchLibrary::WsiRs, &target_dir)?,
        invocation(BenchLibrary::OpenSlide, &target_dir)?,
    ))
}

fn invocation(library: BenchLibrary, target_dir: &Path) -> Result<BenchInvocation, String> {
    let worker = target_dir
        .join("release")
        .join(format!("wsi-rs-perf{}", std::env::consts::EXE_SUFFIX));
    let library_path = match library {
        BenchLibrary::WsiRs => {
            let candidate = wsi_rs_library_candidate(
                target_dir,
                std::env::var_os(WSI_RS_LIBRARY_ENV).as_deref(),
            );
            exact_library_path(&candidate, "wsi-rs OpenSlide shim")?
        }
        BenchLibrary::OpenSlide => explicit_openslide_library()?,
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

fn wsi_rs_library_candidate(target_dir: &Path, override_path: Option<&OsStr>) -> PathBuf {
    override_path.map_or_else(
        || target_dir.join("release").join(shim_library_name()),
        PathBuf::from,
    )
}

fn explicit_openslide_library() -> Result<PathBuf, String> {
    explicit_openslide_library_from(
        std::env::var_os(OPENSLIDE_LIBRARY_ENV),
        std::env::var_os(OPENSLIDE_LIBRARY_FALLBACK_ENV),
    )
}

fn explicit_openslide_library_from(
    configured: Option<OsString>,
    fallback: Option<OsString>,
) -> Result<PathBuf, String> {
    let path = configured.or(fallback).map(PathBuf::from).ok_or_else(|| {
        format!(
            "OpenSlide capture requires an exact {PINNED_OPENSLIDE_VERSION} library path in \
                 {OPENSLIDE_LIBRARY_ENV} (or {OPENSLIDE_LIBRARY_FALLBACK_ENV})"
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
