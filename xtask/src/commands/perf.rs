use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

const PERF_CAPTURE_SCHEMA_VERSION: u32 = 2;
const DEFAULT_REPEAT_COUNT: u32 = 3;
const REGRESSION_RATIO: f64 = 1.05;
const LATENCY_ABSOLUTE_REGRESSION_FLOOR_US: u64 = 500;
const P95_MIN_SAMPLE_COUNT: u64 = 20;
const P99_MIN_SAMPLE_COUNT: u64 = 100;
const RESULT_DIR_ENV: &str = "STATUMEN_PERF_RESULTS_DIR";
const SLIDES_ENV: &str = "STATUMEN_PERF_SLIDES";
const REPEATS_ENV: &str = "STATUMEN_PERF_REPEATS";
const PROFILE_DIR_ENV: &str = "STATUMEN_PERF_PROFILE_DIR";
const PROCESS_METRICS_WORKLOAD: &str = "__process__";
const WORKLOAD_METRICS: [&str; 4] = ["p50_us", "p95_us", "p99_us", "mean_us"];
const PEAK_RSS_METRIC: &str = "peak_rss_bytes";
const DIAGNOSTIC_METRICS: [DiagnosticMetric; 8] = [
    DiagnosticMetric::cache("shared_cache_misses", "shared_cache", "misses"),
    DiagnosticMetric::cache("shared_cache_puts", "shared_cache", "puts"),
    DiagnosticMetric::cache("shared_cache_evictions", "shared_cache", "evictions"),
    DiagnosticMetric::cache(
        "shared_cache_rejected_oversize",
        "shared_cache",
        "rejected_oversize",
    ),
    DiagnosticMetric::cache("display_cache_misses", "display_cache", "misses"),
    DiagnosticMetric::cache("display_cache_puts", "display_cache", "puts"),
    DiagnosticMetric::cache("display_cache_evictions", "display_cache", "evictions"),
    DiagnosticMetric::cache(
        "display_cache_rejected_oversize",
        "display_cache",
        "rejected_oversize",
    ),
];
const MACOS_RSS_METHOD: &str = "macos:/usr/bin/time -l";
const TRACKED_ENV_VARS: [&str; 15] = [
    "RUSTFLAGS",
    "RAYON_NUM_THREADS",
    "STATUMEN_PERF_RESULTS_DIR",
    "STATUMEN_PERF_SLIDES",
    "STATUMEN_PERF_REPEATS",
    "STATUMEN_TILE_CACHE_BYTES",
    "STATUMEN_DISPLAY_TILE_CACHE_BYTES",
    "STATUMEN_FULL_DECODE_CACHE_BYTES",
    "STATUMEN_NDPI_STRIP_CACHE_BYTES",
    "STATUMEN_SYNTHETIC_LEVEL_CACHE_BYTES",
    "STATUMEN_JPEG_DEVICE_DECODE",
    "STATUMEN_JP2K_DEVICE_DECODE",
    "STATUMEN_JP2K_DEVICE_BATCH",
    "STATUMEN_JP2K_CPU_THREADS",
    "WSI_BENCH_ONLY",
];

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct MetricKey {
    slide_path: String,
    workload: String,
    metric: &'static str,
}

#[derive(Debug, Clone, Copy)]
struct MetricPair {
    before: u64,
    after: u64,
}

#[derive(Debug, Clone, Copy)]
struct DiagnosticMetric {
    metric: &'static str,
    cache_name: &'static str,
    field_name: &'static str,
}

impl DiagnosticMetric {
    const fn cache(
        metric: &'static str,
        cache_name: &'static str,
        field_name: &'static str,
    ) -> Self {
        Self {
            metric,
            cache_name,
            field_name,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Regression {
    slide_path: String,
    workload: String,
    metric: &'static str,
    comparable_runs: usize,
    regressed_runs: usize,
}

#[derive(Debug, Clone, PartialEq)]
struct MetricSummary {
    slide_path: String,
    workload: String,
    metric: &'static str,
    comparable_runs: usize,
    regressed_runs: usize,
    median_before: u64,
    median_after: u64,
    ratio: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProfileRecipes {
    cpu_samply: Vec<String>,
    cpu_time_profiler: Vec<String>,
    metal_system_trace: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BenchLibrary {
    Statumen,
    OpenSlide,
}

impl BenchLibrary {
    fn name(self) -> &'static str {
        match self {
            Self::Statumen => "statumen",
            Self::OpenSlide => "openslide",
        }
    }

    fn binary(self) -> &'static str {
        match self {
            Self::Statumen => "wsi_bench",
            Self::OpenSlide => "openslide_bench",
        }
    }

    fn features(self) -> &'static [&'static str] {
        match self {
            Self::Statumen => &["bench"],
            Self::OpenSlide => &["bench", "openslide-bench"],
        }
    }

    fn allow_default_fixture(self) -> bool {
        matches!(self, Self::Statumen)
    }
}

pub(super) fn capture(args: Vec<String>) -> Result<(), String> {
    capture_with_library(args, BenchLibrary::Statumen)
}

pub(super) fn capture_openslide(args: Vec<String>) -> Result<(), String> {
    capture_with_library(args, BenchLibrary::OpenSlide)
}

fn capture_with_library(args: Vec<String>, library: BenchLibrary) -> Result<(), String> {
    let Some(label) = args.first() else {
        return Err(format!(
            "usage: cargo xtask {} <label> [slides...]",
            capture_task_name(library)
        ));
    };
    let slides = resolve_slides(&args[1..], library.allow_default_fixture())?;
    let repeats = repeat_count()?;
    let output_path = result_dir().join(format!("{label}.json"));
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("failed to create {}: {err}", parent.display()))?;
    }

    let mut runs = Vec::new();
    for slide in &slides {
        for repeat in 0..repeats {
            let output = run_bench(library, slide, repeat)?;
            let mut run_json: Value = serde_json::from_slice(&output.stdout).map_err(|err| {
                format!(
                    "invalid {} JSON for {}: {err}",
                    library.binary(),
                    slide.display()
                )
            })?;
            annotate_run_resource_usage(&mut run_json, &output.stderr);
            runs.push(run_json);
        }
    }

    let summary = capture_summary(label, library, repeats, &slides, runs)?;
    std::fs::write(
        &output_path,
        serde_json::to_vec_pretty(&summary).map_err(|err| err.to_string())?,
    )
    .map_err(|err| format!("failed to write {}: {err}", output_path.display()))?;
    println!("{}", output_path.display());
    Ok(())
}

fn capture_task_name(library: BenchLibrary) -> &'static str {
    match library {
        BenchLibrary::Statumen => "perf-capture",
        BenchLibrary::OpenSlide => "perf-capture-openslide",
    }
}

pub(super) fn profile(args: Vec<String>) -> Result<(), String> {
    let Some(slide_arg) = args.first() else {
        return Err("usage: cargo xtask perf-profile <slide-path> [workload-name]".into());
    };
    if args.len() > 2 {
        return Err("usage: cargo xtask perf-profile <slide-path> [workload-name]".into());
    }
    let slide = PathBuf::from(slide_arg);
    if !slide.is_file() {
        return Err(format!("profile slide is not a file: {}", slide.display()));
    }
    let workload = args.get(1).map(String::as_str);
    let label = profile_label(&slide, workload);
    let recipes = profile_recipes(&slide, workload, &label);

    println!(
        "Build first:\n  cargo build --release --features bench --bin wsi_bench\n\n\
         CPU samply:\n  {}\n\n\
         CPU xctrace Time Profiler:\n  {}\n\n\
         Metal xctrace Metal System Trace:\n  {}",
        shell_join(&recipes.cpu_samply),
        shell_join(&recipes.cpu_time_profiler),
        shell_join(&recipes.metal_system_trace)
    );
    Ok(())
}

pub(super) fn compare(args: Vec<String>) -> Result<(), String> {
    if args.len() != 2 {
        return Err("usage: cargo xtask perf-compare <before.json> <after.json>".into());
    }
    let before = read_json(Path::new(&args[0]))?;
    let after = read_json(Path::new(&args[1]))?;
    let summaries = comparison_summaries(&before, &after)?;
    if summaries.is_empty() {
        println!("no comparable benchmark metric groups found");
    } else {
        println!("benchmark comparison summary:");
        for summary in &summaries {
            println!(
                "{} {} {} median_before={} median_after={} ratio={:.3} regressed_runs={}/{}",
                summary.slide_path,
                summary.workload,
                summary.metric,
                summary.median_before,
                summary.median_after,
                summary.ratio,
                summary.regressed_runs,
                summary.comparable_runs
            );
        }
    }
    let regressions = regressions_from_summaries(&summaries);
    if regressions.is_empty() {
        println!(
            "no benchmark regressions above {:.0}% noise guard",
            (REGRESSION_RATIO - 1.0) * 100.0
        );
        return Ok(());
    }

    println!("benchmark regressions:");
    for regression in &regressions {
        println!(
            "{} {} {} regressed in {}/{} comparable runs",
            regression.slide_path,
            regression.workload,
            regression.metric,
            regression.regressed_runs,
            regression.comparable_runs
        );
    }
    Err(format!(
        "{} benchmark regression group(s) exceeded guard",
        regressions.len()
    ))
}

fn run_bench(
    library: BenchLibrary,
    slide: &Path,
    repeat: u32,
) -> Result<std::process::Output, String> {
    let mut command = if cfg!(target_os = "macos") {
        let mut command = Command::new("/usr/bin/time");
        command.arg("-l").arg(cargo());
        command
    } else {
        Command::new(cargo())
    };
    let features = library.features().join(" ");
    let output = command
        .args([
            "run",
            "--release",
            "--features",
            features.as_str(),
            "--bin",
            library.binary(),
            "--",
        ])
        .arg(slide)
        .env("WSI_BENCH_REPEAT_INDEX", repeat.to_string())
        .output()
        .map_err(|err| format!("failed to run wsi_bench: {err}"))?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(format!(
            "{} failed for {} repeat {repeat}: {}",
            library.binary(),
            slide.display(),
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

fn annotate_run_resource_usage(run_json: &mut Value, stderr: &[u8]) {
    if let Some(rss) = parse_peak_rss_bytes(&String::from_utf8_lossy(stderr)) {
        if let Some(object) = run_json.as_object_mut() {
            object.insert(PEAK_RSS_METRIC.to_string(), json!(rss));
            object.insert("rss_method".to_string(), json!(MACOS_RSS_METHOD));
        }
    }
}

fn parse_peak_rss_bytes(stderr: &str) -> Option<u64> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    stderr.lines().find_map(|line| {
        if line.contains("maximum resident set size") {
            line.split_whitespace().next()?.parse::<u64>().ok()
        } else {
            None
        }
    })
}

fn read_json(path: &Path) -> Result<Value, String> {
    let bytes =
        std::fs::read(path).map_err(|err| format!("failed to read {}: {err}", path.display()))?;
    serde_json::from_slice(&bytes)
        .map_err(|err| format!("failed to parse {}: {err}", path.display()))
}

fn capture_summary(
    label: &str,
    library: BenchLibrary,
    repeats: u32,
    slides: &[PathBuf],
    runs: Vec<Value>,
) -> Result<Value, String> {
    let workloads = workload_names(&runs);
    Ok(json!({
        "schema_version": PERF_CAPTURE_SCHEMA_VERSION,
        "kind": "statumen-perf-capture",
        "label": label,
        "repeat_count": repeats,
        "slides": slides.iter().map(|path| path.display().to_string()).collect::<Vec<_>>(),
        "metadata": capture_metadata(library, repeats, slides, &workloads),
        "runs": runs,
    }))
}

fn capture_metadata(
    library: BenchLibrary,
    repeats: u32,
    slides: &[PathBuf],
    workloads: &[String],
) -> Value {
    json!({
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
            "features": library.features(),
            "rustflags": std::env::var("RUSTFLAGS").ok(),
            "native_cpu_tuned": std::env::var("RUSTFLAGS")
                .is_ok_and(|value| value.contains("target-cpu=native")),
        },
        "benchmark": {
            "library": library.name(),
            "binary": library.binary(),
            "corpus_tier": corpus_tier(slides),
            "repeat_count": repeats,
            "workloads": workloads,
            "result_dir": result_dir().display().to_string(),
            "regression_ratio": REGRESSION_RATIO,
            "latency_absolute_regression_floor_us": LATENCY_ABSOLUTE_REGRESSION_FLOOR_US,
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
            "macos_metal_trace": "xcrun xctrace record --template 'Metal System Trace'",
            "flamegraph": "optional diagnostic artifact; not a benchmark gate",
        }
    })
}

fn workload_names(runs: &[Value]) -> Vec<String> {
    let mut names = BTreeSet::new();
    for run in runs {
        let Some(workloads) = run.get("workloads").and_then(Value::as_array) else {
            continue;
        };
        for workload in workloads {
            if let Some(name) = workload.get("name").and_then(Value::as_str) {
                names.insert(name.to_string());
            }
        }
    }
    names.into_iter().collect()
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

fn corpus_tier(slides: &[PathBuf]) -> &'static str {
    if slides.iter().all(|path| {
        let path = path.to_string_lossy();
        path.contains("tests/fixtures/")
    }) {
        return "public-fixture";
    }
    if slides.iter().any(|path| {
        let path = path.to_string_lossy();
        path.contains(".cache/slideviewer/parity-corpus")
    }) {
        return "local-parity";
    }
    "custom"
}

fn default_public_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("tests")
        .join("fixtures")
        .join("jp2k")
        .join("rgb_nomct.j2k")
}

fn profile_recipes(slide: &Path, workload: Option<&str>, label: &str) -> ProfileRecipes {
    let profile_dir = std::env::var_os(PROFILE_DIR_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("bench/results/profiles"));
    let bench_invocation = vec![
        "target/release/wsi_bench".to_string(),
        slide.display().to_string(),
    ];

    let mut env_bench_invocation = Vec::new();
    env_bench_invocation.push("env".to_string());
    if let Some(workload) = workload {
        env_bench_invocation.push(format!("WSI_BENCH_ONLY={workload}"));
    }
    env_bench_invocation.extend(bench_invocation.clone());

    let mut cpu_samply = Vec::new();
    if let Some(workload) = workload {
        cpu_samply.push(format!("WSI_BENCH_ONLY={workload}"));
    }
    cpu_samply.extend([
        "samply".to_string(),
        "record".to_string(),
        "--save-only".to_string(),
        "--output".to_string(),
        profile_dir
            .join(format!("{label}-samply.json.gz"))
            .display()
            .to_string(),
        "--profile-name".to_string(),
        label.to_string(),
    ]);
    cpu_samply.extend(bench_invocation);

    let mut cpu_time_profiler = vec![
        "xcrun".to_string(),
        "xctrace".to_string(),
        "record".to_string(),
        "--template".to_string(),
        "Time Profiler".to_string(),
        "--output".to_string(),
        profile_dir
            .join(format!("{label}-time-profiler.trace"))
            .display()
            .to_string(),
        "--launch".to_string(),
        "--".to_string(),
    ];
    cpu_time_profiler.extend(env_bench_invocation.clone());

    let mut metal_system_trace = vec![
        "xcrun".to_string(),
        "xctrace".to_string(),
        "record".to_string(),
        "--template".to_string(),
        "Metal System Trace".to_string(),
        "--output".to_string(),
        profile_dir
            .join(format!("{label}-metal.trace"))
            .display()
            .to_string(),
        "--launch".to_string(),
        "--".to_string(),
    ];
    metal_system_trace.extend(env_bench_invocation);

    ProfileRecipes {
        cpu_samply,
        cpu_time_profiler,
        metal_system_trace,
    }
}

fn profile_label(slide: &Path, workload: Option<&str>) -> String {
    let stem = slide
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("slide");
    let workload = workload.unwrap_or("full-suite");
    sanitize_label(&format!("{stem}-{workload}"))
}

fn sanitize_label(raw: &str) -> String {
    raw.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

fn shell_join(args: &[String]) -> String {
    args.iter()
        .map(|arg| shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_quote(arg: &str) -> String {
    if arg
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '-' | '_' | '=' | ':'))
    {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
fn compare_captures(before: &Value, after: &Value) -> Result<Vec<Regression>, String> {
    Ok(regressions_from_summaries(&comparison_summaries(
        before, after,
    )?))
}

fn comparison_summaries(before: &Value, after: &Value) -> Result<Vec<MetricSummary>, String> {
    let include_process_metrics = workload_sets_match(before, after);
    let before_metrics = metric_map(before, include_process_metrics)?;
    let after_metrics = metric_map(after, include_process_metrics)?;
    let mut groups: BTreeMap<MetricKey, Vec<MetricPair>> = BTreeMap::new();

    for (key, before_value) in before_metrics {
        if let Some(after_value) = after_metrics.get(&key) {
            let group_key = MetricKey {
                slide_path: key.slide_path,
                workload: key.workload,
                metric: key.metric,
            };
            groups.entry(group_key).or_default().push(MetricPair {
                before: before_value,
                after: *after_value,
            });
        }
    }

    Ok(groups
        .into_iter()
        .filter_map(|(key, pairs)| {
            if pairs.len() < DEFAULT_REPEAT_COUNT as usize {
                return None;
            }
            let regressed = pairs
                .iter()
                .filter(|pair| metric_pair_regressed(key.metric, pair))
                .count();
            let mut before_values = pairs.iter().map(|pair| pair.before).collect::<Vec<_>>();
            let mut after_values = pairs.iter().map(|pair| pair.after).collect::<Vec<_>>();
            let median_before = median_u64(&mut before_values)?;
            let median_after = median_u64(&mut after_values)?;
            let ratio = metric_ratio(median_before, median_after);
            Some(MetricSummary {
                slide_path: key.slide_path,
                workload: key.workload,
                metric: key.metric,
                comparable_runs: pairs.len(),
                regressed_runs: regressed,
                median_before,
                median_after,
                ratio,
            })
        })
        .collect())
}

fn metric_pair_regressed(metric: &str, pair: &MetricPair) -> bool {
    if pair.before == 0 || pair.after as f64 / pair.before as f64 <= REGRESSION_RATIO {
        return false;
    }
    if WORKLOAD_METRICS.contains(&metric) {
        return pair.after.saturating_sub(pair.before) >= LATENCY_ABSOLUTE_REGRESSION_FLOOR_US;
    }
    true
}

fn regressions_from_summaries(summaries: &[MetricSummary]) -> Vec<Regression> {
    summaries
        .iter()
        .filter(|summary| summary.regressed_runs >= 2 && metric_summary_regressed(summary))
        .map(|summary| Regression {
            slide_path: summary.slide_path.clone(),
            workload: summary.workload.clone(),
            metric: summary.metric,
            comparable_runs: summary.comparable_runs,
            regressed_runs: summary.regressed_runs,
        })
        .collect()
}

fn metric_summary_regressed(summary: &MetricSummary) -> bool {
    metric_pair_regressed(
        summary.metric,
        &MetricPair {
            before: summary.median_before,
            after: summary.median_after,
        },
    )
}

fn metric_ratio(before: u64, after: u64) -> f64 {
    match (before, after) {
        (0, 0) => 1.0,
        (0, _) => f64::INFINITY,
        _ => after as f64 / before as f64,
    }
}

fn workload_sets_match(before: &Value, after: &Value) -> bool {
    capture_workload_set(before) == capture_workload_set(after)
}

fn capture_workload_set(capture: &Value) -> BTreeSet<String> {
    let metadata_workloads = capture
        .get("metadata")
        .and_then(|metadata| metadata.get("benchmark"))
        .and_then(|benchmark| benchmark.get("workloads"))
        .and_then(Value::as_array)
        .map(|workloads| {
            workloads
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    if !metadata_workloads.is_empty() {
        return metadata_workloads;
    }
    let runs = capture
        .get("runs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    workload_names(&runs).into_iter().collect()
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RunMetricKey {
    slide_path: String,
    workload: String,
    repeat_index: u32,
    metric: &'static str,
}

fn metric_map(
    capture: &Value,
    include_process_metrics: bool,
) -> Result<BTreeMap<RunMetricKey, u64>, String> {
    let runs = capture
        .get("runs")
        .and_then(Value::as_array)
        .ok_or_else(|| "capture JSON missing runs array".to_string())?;
    let mut out = BTreeMap::new();
    for run in runs {
        let slide_path = run
            .get("slide_path")
            .and_then(Value::as_str)
            .ok_or_else(|| "run missing slide_path".to_string())?
            .to_string();
        let repeat_index = run
            .get("repeat_index")
            .and_then(Value::as_u64)
            .ok_or_else(|| "run missing repeat_index".to_string())
            .and_then(|value| {
                u32::try_from(value).map_err(|_| format!("repeat_index {value} exceeds u32"))
            })?;
        let workloads = run
            .get("workloads")
            .and_then(Value::as_array)
            .ok_or_else(|| "run missing workloads".to_string())?;
        for workload in workloads {
            let Some(name) = workload.get("name").and_then(Value::as_str) else {
                continue;
            };
            for metric in WORKLOAD_METRICS {
                if !workload_metric_is_comparable(workload, metric) {
                    continue;
                }
                if let Some(value) = workload.get(metric).and_then(Value::as_u64) {
                    out.insert(
                        RunMetricKey {
                            slide_path: slide_path.clone(),
                            workload: name.to_string(),
                            repeat_index,
                            metric,
                        },
                        value,
                    );
                }
            }
            for diagnostic in DIAGNOSTIC_METRICS {
                let Some(value) = workload
                    .get("diagnostics")
                    .and_then(|diagnostics| diagnostics.get(diagnostic.cache_name))
                    .and_then(|cache| cache.get(diagnostic.field_name))
                    .and_then(Value::as_u64)
                else {
                    continue;
                };
                out.insert(
                    RunMetricKey {
                        slide_path: slide_path.clone(),
                        workload: name.to_string(),
                        repeat_index,
                        metric: diagnostic.metric,
                    },
                    value,
                );
            }
        }
        if include_process_metrics {
            if let Some(value) = run.get(PEAK_RSS_METRIC).and_then(Value::as_u64) {
                out.insert(
                    RunMetricKey {
                        slide_path,
                        workload: PROCESS_METRICS_WORKLOAD.to_string(),
                        repeat_index,
                        metric: PEAK_RSS_METRIC,
                    },
                    value,
                );
            }
        }
    }
    Ok(out)
}

fn workload_metric_is_comparable(workload: &Value, metric: &str) -> bool {
    let Some(min_samples) = tail_metric_min_samples(metric) else {
        return true;
    };
    workload_sample_count(workload).is_none_or(|sample_count| sample_count >= min_samples)
}

fn tail_metric_min_samples(metric: &str) -> Option<u64> {
    match metric {
        "p95_us" => Some(P95_MIN_SAMPLE_COUNT),
        "p99_us" => Some(P99_MIN_SAMPLE_COUNT),
        _ => None,
    }
}

fn workload_sample_count(workload: &Value) -> Option<u64> {
    workload.get("n").and_then(Value::as_u64).or_else(|| {
        workload
            .get("samples_us")?
            .as_array()
            .map(|samples| samples.len() as u64)
    })
}

fn median_u64(values: &mut [u64]) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    Some(values[values.len() / 2])
}

fn resolve_slides(args: &[String], allow_default_fixture: bool) -> Result<Vec<PathBuf>, String> {
    let mut slides = if args.is_empty() {
        std::env::var_os(SLIDES_ENV)
            .map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
            .unwrap_or_default()
    } else {
        args.iter().map(PathBuf::from).collect()
    };
    if slides.is_empty() {
        if allow_default_fixture {
            slides.push(default_public_fixture());
        } else {
            return Err(format!(
                "OpenSlide perf capture requires explicit WSI slide paths or {SLIDES_ENV}; \
                 the default JP2K codestream fixture is not an OpenSlide slide"
            ));
        }
    }
    for slide in &slides {
        if !slide.is_file() {
            return Err(format!(
                "benchmark slide is not a file: {}",
                slide.display()
            ));
        }
    }
    Ok(slides)
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

fn result_dir() -> PathBuf {
    std::env::var_os(RESULT_DIR_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("bench/results/local-regression"))
}

fn cargo() -> OsString {
    std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capture_json(values: &[(u32, u64, u64)]) -> Value {
        json!({
            "runs": values.iter().map(|(repeat, p50, p95)| json!({
                "slide_path": "fixture.svs",
                "repeat_index": repeat,
                "workloads": [{
                    "name": "single_tile_l0",
                    "target_n": 20,
                    "n": 20,
                    "p50_us": p50,
                    "p95_us": p95,
                }]
            })).collect::<Vec<_>>()
        })
    }

    fn full_capture_json(values: &[(u32, u64, u64, u64, u64, u64)]) -> Value {
        json!({
            "runs": values.iter().map(|(repeat, p50, p95, p99, mean, rss)| json!({
                "slide_path": "fixture.svs",
                "repeat_index": repeat,
                "peak_rss_bytes": rss,
                "workloads": [{
                    "name": "single_tile_l0",
                    "target_n": 100,
                    "n": 100,
                    "p50_us": p50,
                    "p95_us": p95,
                    "p99_us": p99,
                    "mean_us": mean,
                    "samples_us": [p50, p95, p99],
                }]
            })).collect::<Vec<_>>()
        })
    }

    #[test]
    fn capture_summary_records_environment_metadata_and_raw_samples() {
        let run = json!({
            "slide_path": "tests/fixtures/jp2k/rgb_nomct.j2k",
            "repeat_index": 0,
            "workloads": [{
                "name": "single_tile_l0",
                "p50_us": 10,
                "p95_us": 20,
                "p99_us": 30,
                "mean_us": 15,
                "samples_us": [10, 20, 30],
            }]
        });

        let summary = capture_summary(
            "baseline-public",
            BenchLibrary::Statumen,
            3,
            &[PathBuf::from("tests/fixtures/jp2k/rgb_nomct.j2k")],
            vec![run],
        )
        .expect("capture summary");

        assert_eq!(summary["schema_version"], PERF_CAPTURE_SCHEMA_VERSION);
        assert_eq!(summary["kind"], "statumen-perf-capture");
        assert_eq!(summary["metadata"]["benchmark"]["library"], "statumen");
        assert_eq!(summary["metadata"]["benchmark"]["binary"], "wsi_bench");
        assert_eq!(summary["metadata"]["build"]["features"][0], "bench");
        assert_eq!(
            summary["metadata"]["benchmark"]["corpus_tier"],
            "public-fixture"
        );
        assert_eq!(summary["metadata"]["benchmark"]["repeat_count"], 3);
        assert_eq!(
            summary["metadata"]["benchmark"]["workloads"][0],
            "single_tile_l0"
        );
        assert!(summary["metadata"]["git"]["branch"].is_string());
        assert!(summary["metadata"]["toolchain"]["rustc"].is_string());
        assert!(summary["metadata"]["host"]["cpu"].is_string());
        assert_eq!(summary["runs"][0]["workloads"][0]["samples_us"][2], 30);
    }

    #[test]
    fn openslide_capture_summary_records_competitor_library() {
        let run = json!({
            "library": "openslide",
            "slide_path": "fixture.svs",
            "repeat_index": 0,
            "workloads": [{
                "name": "region_2k",
                "p50_us": 100,
                "p95_us": 120,
                "p99_us": 140,
                "mean_us": 110,
                "samples_us": [100, 120, 140],
            }]
        });

        let summary = capture_summary(
            "openslide-baseline",
            BenchLibrary::OpenSlide,
            3,
            &[PathBuf::from("fixture.svs")],
            vec![run],
        )
        .expect("OpenSlide capture summary");

        assert_eq!(summary["metadata"]["benchmark"]["library"], "openslide");
        assert_eq!(
            summary["metadata"]["benchmark"]["binary"],
            "openslide_bench"
        );
        assert_eq!(
            summary["metadata"]["build"]["features"],
            json!(["bench", "openslide-bench"])
        );
    }

    #[test]
    fn compare_flags_regression_when_two_of_three_runs_exceed_guard() {
        let before = capture_json(&[
            (0, 10_000, 20_000),
            (1, 10_000, 20_000),
            (2, 10_000, 20_000),
        ]);
        let after = capture_json(&[
            (0, 10_700, 20_000),
            (1, 10_800, 21_200),
            (2, 10_000, 21_300),
        ]);

        let regressions = compare_captures(&before, &after).expect("compare captures");

        assert!(regressions.iter().any(|regression| {
            regression.workload == "single_tile_l0"
                && regression.metric == "p50_us"
                && regression.regressed_runs == 2
        }));
        assert!(regressions.iter().any(|regression| {
            regression.workload == "single_tile_l0"
                && regression.metric == "p95_us"
                && regression.regressed_runs == 2
        }));
    }

    #[test]
    fn compare_ignores_single_noisy_regression() {
        let before = capture_json(&[
            (0, 10_000, 20_000),
            (1, 10_000, 20_000),
            (2, 10_000, 20_000),
        ]);
        let after = capture_json(&[
            (0, 13_000, 26_000),
            (1, 10_000, 20_000),
            (2, 10_000, 20_000),
        ]);

        let regressions = compare_captures(&before, &after).expect("compare captures");

        assert!(regressions.is_empty());
    }

    #[test]
    fn compare_does_not_gate_tail_metrics_when_sample_count_is_too_low() {
        let capture = |values: &[(u32, u64, u64, u64, u64)]| {
            json!({
                "runs": values.iter().map(|(repeat, p50, p95, p99, mean)| json!({
                    "slide_path": "fixture.svs",
                    "repeat_index": repeat,
                    "workloads": [{
                        "name": "cold_open",
                        "target_n": 10,
                        "n": 10,
                        "p50_us": p50,
                        "p95_us": p95,
                        "p99_us": p99,
                        "mean_us": mean,
                    }]
                })).collect::<Vec<_>>()
            })
        };
        let before = capture(&[
            (0, 400, 1_100, 1_100, 500),
            (1, 400, 1_100, 1_100, 500),
            (2, 400, 1_100, 1_100, 500),
        ]);
        let after = capture(&[
            (0, 400, 1_650, 1_650, 500),
            (1, 400, 1_600, 1_600, 500),
            (2, 400, 1_580, 1_580, 500),
        ]);

        let regressions = compare_captures(&before, &after).expect("compare captures");

        assert!(regressions.is_empty());
    }

    #[test]
    fn compare_ignores_latency_ratios_below_absolute_noise_floor() {
        let before = capture_json(&[(0, 10, 90), (1, 10, 90), (2, 10, 90)]);
        let after = capture_json(&[(0, 20, 150), (1, 20, 150), (2, 10, 90)]);

        let regressions = compare_captures(&before, &after).expect("compare captures");

        assert!(regressions.is_empty());
    }

    #[test]
    fn metric_ratio_reports_zero_to_zero_as_unchanged() {
        assert_eq!(metric_ratio(0, 0), 1.0);
        assert_eq!(metric_ratio(0, 1), f64::INFINITY);
        assert_eq!(metric_ratio(100, 110), 1.1);
    }

    #[test]
    fn compare_checks_p99_mean_and_peak_rss_regressions() {
        let before = full_capture_json(&[
            (0, 1_000, 2_000, 3_000, 1_500, 1_000),
            (1, 1_000, 2_000, 3_000, 1_500, 1_000),
            (2, 1_000, 2_000, 3_000, 1_500, 1_000),
        ]);
        let after = full_capture_json(&[
            (0, 1_000, 2_000, 3_600, 2_100, 1_100),
            (1, 1_000, 2_000, 3_610, 2_110, 1_110),
            (2, 1_000, 2_000, 3_000, 1_500, 1_000),
        ]);

        let regressions = compare_captures(&before, &after).expect("compare captures");

        assert!(regressions.iter().any(|regression| {
            regression.workload == "single_tile_l0"
                && regression.metric == "p99_us"
                && regression.regressed_runs == 2
        }));
        assert!(regressions.iter().any(|regression| {
            regression.workload == "single_tile_l0"
                && regression.metric == "mean_us"
                && regression.regressed_runs == 2
        }));
        assert!(regressions.iter().any(|regression| {
            regression.workload == PROCESS_METRICS_WORKLOAD
                && regression.metric == "peak_rss_bytes"
                && regression.regressed_runs == 2
        }));
    }

    #[test]
    fn compare_skips_peak_rss_when_workload_sets_differ() {
        let before = json!({
            "metadata": {
                "benchmark": {
                    "workloads": ["single_tile_l0"]
                }
            },
            "runs": (0..3).map(|repeat| json!({
                "slide_path": "fixture.svs",
                "repeat_index": repeat,
                "peak_rss_bytes": 1_000,
                "workloads": [{
                    "name": "single_tile_l0",
                    "p50_us": 1_000,
                }]
            })).collect::<Vec<_>>()
        });
        let after = json!({
            "metadata": {
                "benchmark": {
                    "workloads": ["raw_tile_l0", "single_tile_l0"]
                }
            },
            "runs": (0..3).map(|repeat| json!({
                "slide_path": "fixture.svs",
                "repeat_index": repeat,
                "peak_rss_bytes": 2_000,
                "workloads": [{
                    "name": "single_tile_l0",
                    "p50_us": 1_000,
                }, {
                    "name": "raw_tile_l0",
                    "p50_us": 1_000,
                }]
            })).collect::<Vec<_>>()
        });

        let regressions = compare_captures(&before, &after).expect("compare captures");

        assert!(!regressions
            .iter()
            .any(|regression| regression.workload == PROCESS_METRICS_WORKLOAD));
    }

    #[test]
    fn compare_checks_higher_is_worse_cache_diagnostics() {
        let capture = |misses: u64| {
            json!({
                "runs": (0..3).map(|repeat| json!({
                    "slide_path": "fixture.svs",
                    "repeat_index": repeat,
                    "workloads": [{
                        "name": "region_2k",
                        "target_n": 30,
                        "n": 30,
                        "p50_us": 1_000,
                        "diagnostics": {
                            "shared_cache": {
                                "hits": 10,
                                "misses": misses,
                                "puts": misses,
                                "evictions": 0,
                                "rejected_oversize": 0
                            },
                            "display_cache": {
                                "hits": 0,
                                "misses": 0,
                                "puts": 0,
                                "evictions": 0,
                                "rejected_oversize": 0
                            },
                            "decode_route_cache_entries": 0
                        }
                    }]
                })).collect::<Vec<_>>()
            })
        };

        let regressions = compare_captures(&capture(2), &capture(4)).expect("compare captures");

        assert!(regressions.iter().any(|regression| {
            regression.workload == "region_2k" && regression.metric == "shared_cache_misses"
        }));
        assert!(!regressions.iter().any(|regression| {
            regression.workload == "region_2k" && regression.metric == "shared_cache_hits"
        }));
    }

    #[test]
    fn profile_recipes_include_cpu_and_metal_commands() {
        let recipes = profile_recipes(
            Path::new("/tmp/fixture.svs"),
            Some("single_tile_l0"),
            "single-tile-profile",
        );

        assert!(recipes.cpu_samply.join(" ").contains("samply record"));
        assert!(recipes.cpu_samply.iter().any(|arg| arg == "--save-only"));
        assert!(!recipes.cpu_samply.iter().any(|arg| arg == "env"));
        assert!(recipes
            .cpu_time_profiler
            .join(" ")
            .contains("Time Profiler"));
        assert!(recipes
            .metal_system_trace
            .join(" ")
            .contains("Metal System Trace"));
        assert!(recipes
            .cpu_samply
            .iter()
            .any(|arg| arg == "WSI_BENCH_ONLY=single_tile_l0"));
    }
}
