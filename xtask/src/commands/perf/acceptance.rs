use std::collections::BTreeMap;

use serde_json::Value;

use super::checksum::{validate_cross_capture_checksums, validate_declared_capture_plan};
use super::comparison::{capture_library, comparison_summaries, P99_MIN_SAMPLE_COUNT};
use super::process_metrics::PEAK_RSS_METRIC;
use super::schema::CaptureDocument;

pub(super) const OPENSLIDE_HEADLINE_RATIO: f64 = 1.00;
const OPENSLIDE_CELL_REGRESSION_RATIO: f64 = 1.05;
const OPENSLIDE_RSS_RATIO: f64 = 1.20;
const ACCEPTANCE_REPEATS: u64 = 5;
const REQUIRED_OPENSLIDE_GROUPS: [(&str, &str); 7] = [
    ("svs-001", "aperio/jpeg"),
    ("svs-jp2k-001", "aperio/j2k"),
    ("ndpi-001", "ndpi/jpeg"),
    ("vms-001", "hamamatsu_vms/jpeg"),
    ("leica-001", "leica/jpeg"),
    ("ventana-001", "ventana/jpeg"),
    ("mirax-001", "mirax/jpeg"),
];
#[derive(Debug, Clone, PartialEq)]
pub(super) struct OpenSlideAcceptance {
    pub(super) headline_ratio: f64,
    pub(super) headline_cells: usize,
    pub(super) failures: Vec<String>,
}

pub(super) fn evaluate_openslide_acceptance(
    openslide: &Value,
    wsi_rs: &Value,
) -> Result<OpenSlideAcceptance, String> {
    if capture_library(openslide) != Some("openslide") || capture_library(wsi_rs) != Some("wsi_rs")
    {
        return Err("OpenSlide acceptance requires openslide and wsi_rs captures".into());
    }
    validate_declared_capture_plan(openslide)?;
    validate_declared_capture_plan(wsi_rs)?;
    validate_mandatory_acceptance_matrix(openslide)?;
    validate_mandatory_acceptance_matrix(wsi_rs)?;
    validate_mandatory_viewer_metrics(openslide)?;
    validate_mandatory_viewer_metrics(wsi_rs)?;
    validate_comparison_context(openslide, wsi_rs)?;
    validate_paired_run_order(openslide)?;
    validate_paired_run_order(wsi_rs)?;
    validate_gpu_route_evidence(wsi_rs)?;
    validate_cross_capture_checksums(openslide, wsi_rs)?;
    let summaries = comparison_summaries(openslide, wsi_rs)?;
    let mut failures = Vec::new();
    if [openslide, wsi_rs]
        .iter()
        .any(|capture| codec_thread_budget_enforced(capture) != Some(true))
    {
        failures.push(
            "internal codec thread budget controls are missing or not equalized; OpenSlide acceptance cannot pass"
                .into(),
        );
    }
    if [openslide, wsi_rs]
        .iter()
        .any(|capture| !has_complete_positive_run_metric(capture, PEAK_RSS_METRIC))
    {
        failures.push(
            "peak RSS measurements are missing or invalid; OpenSlide memory acceptance cannot pass"
                .into(),
        );
    }

    for summary in &summaries {
        if viewer_workloads().contains(&summary.workload.as_str())
            && matches!(summary.metric, "p50_us" | "p95_us" | "p99_us")
            && summary.ratio > OPENSLIDE_CELL_REGRESSION_RATIO
        {
            failures.push(format!(
                "{} alias={} format={} group={} workers={} {} {} ratio {:.3} exceeds {:.2}",
                summary.slide_path,
                summary.alias,
                summary.format,
                summary.benchmark_group,
                summary.worker_count,
                summary.workload,
                summary.metric,
                summary.ratio,
                OPENSLIDE_CELL_REGRESSION_RATIO
            ));
        }
        if summary.metric == PEAK_RSS_METRIC && summary.ratio > OPENSLIDE_RSS_RATIO {
            failures.push(format!(
                "{} alias={} format={} group={} workers={} peak RSS ratio {:.3} exceeds {:.2}",
                summary.slide_path,
                summary.alias,
                summary.format,
                summary.benchmark_group,
                summary.worker_count,
                summary.ratio,
                OPENSLIDE_RSS_RATIO
            ));
        }
    }

    let (headline_ratio, headline_cells) = headline_viewer_ratio(&summaries)?;
    if headline_ratio > OPENSLIDE_HEADLINE_RATIO {
        failures.push(format!(
            "viewer p50 geometric-mean ratio {headline_ratio:.3} exceeds {OPENSLIDE_HEADLINE_RATIO:.2}"
        ));
    }

    Ok(OpenSlideAcceptance {
        headline_ratio,
        headline_cells,
        failures,
    })
}

fn headline_viewer_ratio(
    summaries: &[super::comparison::MetricSummary],
) -> Result<(f64, usize), String> {
    let mut group_workload_ratios: BTreeMap<&str, BTreeMap<&str, Vec<f64>>> = BTreeMap::new();
    for summary in summaries {
        if summary.metric == "p50_us" && viewer_workloads().contains(&summary.workload.as_str()) {
            group_workload_ratios
                .entry(&summary.benchmark_group)
                .or_default()
                .entry(&summary.workload)
                .or_default()
                .push(summary.ratio);
        }
    }
    if group_workload_ratios.is_empty() {
        return Err("captures have no comparable viewer p50 cells".into());
    }
    let headline_cells = group_workload_ratios
        .values()
        .flat_map(BTreeMap::values)
        .map(Vec::len)
        .sum();
    let per_group = group_workload_ratios
        .into_values()
        .map(|workloads| {
            let per_workload = workloads
                .into_values()
                .map(|ratios| geometric_mean(&ratios))
                .collect::<Result<Vec<_>, _>>()?;
            geometric_mean(&per_workload)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((geometric_mean(&per_group)?, headline_cells))
}

fn validate_mandatory_acceptance_matrix(capture: &Value) -> Result<(), String> {
    let capture = CaptureDocument::parse(capture)?;
    let manifest = &capture.slide_manifest;
    if manifest.is_empty() {
        return Err("capture JSON missing slide_manifest".into());
    }
    let missing_aliases = REQUIRED_OPENSLIDE_GROUPS
        .iter()
        .map(|(alias, _)| *alias)
        .filter(|alias| !manifest.iter().any(|slide| slide.alias == *alias))
        .collect::<Vec<_>>();
    if !missing_aliases.is_empty() {
        return Err(format!(
            "capture is missing mandatory OpenSlide aliases: {}",
            missing_aliases.join(", ")
        ));
    }
    for (alias, required_group) in REQUIRED_OPENSLIDE_GROUPS {
        let slide = manifest
            .iter()
            .find(|slide| slide.alias == alias)
            .expect("required alias presence checked above");
        let actual_group = slide.benchmark_group.as_str();
        if actual_group != required_group {
            return Err(format!(
                "mandatory alias {alias} must use benchmark_group={required_group}, found {actual_group}"
            ));
        }
    }

    let benchmark = &capture.metadata.benchmark;
    let workloads = benchmark
        .planned_workloads
        .iter()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    let missing_workloads = viewer_workloads()
        .iter()
        .copied()
        .filter(|workload| !workloads.contains(workload))
        .collect::<Vec<_>>();
    if !missing_workloads.is_empty() {
        return Err(format!(
            "capture is missing mandatory viewer workloads: {}",
            missing_workloads.join(", ")
        ));
    }

    let workers = benchmark
        .client_worker_matrix
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    let physical_cores = (benchmark.physical_core_count > 0)
        .then_some(benchmark.physical_core_count)
        .ok_or_else(|| "capture JSON missing positive physical_core_count".to_string())?;
    let required_workers = [1, 2, physical_cores]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    let missing_workers = required_workers
        .difference(&workers)
        .copied()
        .collect::<Vec<_>>();
    if !missing_workers.is_empty() {
        return Err(format!(
            "capture is missing mandatory worker counts: {missing_workers:?}"
        ));
    }

    let repeats = capture.repeat_count;
    if repeats != ACCEPTANCE_REPEATS {
        return Err(format!(
            "OpenSlide acceptance requires exactly {ACCEPTANCE_REPEATS} alternating process repeats, found {repeats}"
        ));
    }
    Ok(())
}

fn validate_paired_run_order(capture: &Value) -> Result<(), String> {
    let capture = CaptureDocument::parse(capture)?;
    let library = capture.metadata.benchmark.library.as_str();
    for run in &capture.runs {
        let repeat = run
            .repeat_index
            .ok_or_else(|| "paired performance run missing repeat_index".to_string())?;
        let expected = if repeat.is_multiple_of(2) {
            ["wsi_rs", "openslide"]
        } else {
            ["openslide", "wsi_rs"]
        };
        if run.engine_order != expected {
            return Err(format!(
                "paired performance run repeat={repeat} has engine_order={:?}, expected {expected:?}",
                run.engine_order
            ));
        }
        let expected_position = expected
            .iter()
            .position(|engine| *engine == library)
            .ok_or_else(|| format!("unknown paired performance library {library:?}"))?;
        if run.engine_position != Some(expected_position) {
            return Err(format!(
                "paired performance run repeat={repeat} library={library} has engine_position={:?}, expected {expected_position}",
                run.engine_position
            ));
        }
    }
    Ok(())
}

fn validate_gpu_route_evidence(capture: &Value) -> Result<(), String> {
    let capture = CaptureDocument::parse(capture)?;
    let features = capture
        .metadata
        .build
        .get("features")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            "wsi-rs GPU acceptance capture missing build feature metadata".to_string()
        })?;
    let declared_gpu_features = ["metal", "cuda"]
        .into_iter()
        .filter(|candidate| {
            features
                .iter()
                .any(|feature| feature.as_str() == Some(candidate))
        })
        .collect::<Vec<_>>();
    if declared_gpu_features.len() != 1 {
        return Err(format!(
            "wsi-rs GPU acceptance capture must declare exactly one compiled metal or cuda feature, found {declared_gpu_features:?}"
        ));
    }
    let feature = declared_gpu_features.first().copied().ok_or_else(|| {
        "wsi-rs GPU acceptance capture must declare compiled metal or cuda support".to_string()
    })?;

    let host = &capture.metadata.host;
    let expected_platform = match feature {
        "metal" => ("macos", "aarch64"),
        "cuda" => ("linux", "x86_64"),
        _ => unreachable!("feature selected from fixed allowlist"),
    };
    if host.get("os").and_then(Value::as_str) != Some(expected_platform.0)
        || host.get("arch").and_then(Value::as_str) != Some(expected_platform.1)
    {
        return Err(format!(
            "{feature} performance acceptance requires pinned {}-{} host metadata",
            expected_platform.0, expected_platform.1
        ));
    }
    if host
        .get("pinned_host_id")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        return Err("GPU performance acceptance requires nonempty pinned_host_id metadata".into());
    }
    if host
        .get("gpu")
        .and_then(Value::as_str)
        .is_none_or(|gpu| gpu.is_empty() || gpu.starts_with("unavailable:"))
    {
        return Err("GPU performance acceptance requires available GPU identity metadata".into());
    }

    for run in capture
        .runs
        .iter()
        .filter(|run| run.benchmark_group() == "aperio/j2k")
    {
        for workload in &run.workloads {
            if !viewer_workloads().contains(&workload.name.as_str()) {
                continue;
            }
            let route = workload
                .diagnostics
                .as_ref()
                .and_then(|diagnostics| diagnostics.get("decode_route"));
            let device_tiles = route
                .and_then(|route| route.get("device_tiles"))
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let fallback_tiles = route
                .and_then(|route| route.get("fallback_tiles"))
                .and_then(Value::as_u64);
            let reported_feature = route
                .and_then(|route| route.get("feature"))
                .and_then(Value::as_str);
            if device_tiles == 0 || fallback_tiles != Some(0) || reported_feature != Some(feature) {
                return Err(format!(
                    "GPU cell alias={} workload={} lacks actual {feature} route evidence or reported fallback",
                    run.alias(), workload.name
                ));
            }
        }
    }
    Ok(())
}

fn validate_mandatory_viewer_metrics(capture: &Value) -> Result<(), String> {
    if capture.get("runs").and_then(Value::as_array).is_none() {
        return Err("capture JSON missing runs array".into());
    }
    let capture = CaptureDocument::parse(capture)?;
    for run in &capture.runs {
        let alias = run.alias.as_deref().unwrap_or("unknown");
        for workload in &run.workloads {
            if workload.name.is_empty() {
                continue;
            }
            let name = workload.name.as_str();
            if !viewer_workloads().contains(&name) {
                continue;
            }
            let samples = workload.sample_count().unwrap_or(0);
            if samples < P99_MIN_SAMPLE_COUNT {
                return Err(format!(
                    "mandatory viewer cell alias={alias} workload={name} needs at least {P99_MIN_SAMPLE_COUNT} samples for p99_us, found {samples}"
                ));
            }
            for metric in ["p50_us", "p95_us", "p99_us"] {
                if workload.metric(metric).is_none() {
                    return Err(format!(
                        "mandatory viewer cell alias={alias} workload={name} missing {metric}"
                    ));
                }
            }
        }
    }
    Ok(())
}

pub(super) fn validate_comparison_context(first: &Value, second: &Value) -> Result<(), String> {
    let first = CaptureDocument::parse(first)?;
    let second = CaptureDocument::parse(second)?;
    let first_host = &first.metadata.host;
    let second_host = &second.metadata.host;
    if first_host.is_null() || second_host.is_null() {
        return Err("capture missing host metadata".into());
    }
    if first_host != second_host {
        return Err("capture host metadata differ".into());
    }
    if first_host
        .get("pinned_host_id")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        return Err("capture missing nonempty pinned_host_id metadata".into());
    }

    let first_build = &first.metadata.build;
    let second_build = &second.metadata.build;
    if first_build.is_null() || second_build.is_null() {
        return Err("capture missing build metadata".into());
    }
    if first_build != second_build {
        return Err("capture build metadata differ".into());
    }
    if first_build.get("profile").and_then(Value::as_str) != Some("release")
        || first_build.get("native_cpu_tuned").and_then(Value::as_bool) != Some(false)
    {
        return Err("headline acceptance requires a portable release build".into());
    }

    let first_cache = (first.metadata.benchmark.cache_bytes > 0)
        .then_some(first.metadata.benchmark.cache_bytes)
        .ok_or_else(|| "first capture missing positive cache_bytes".to_string())?;
    let second_cache = (second.metadata.benchmark.cache_bytes > 0)
        .then_some(second.metadata.benchmark.cache_bytes)
        .ok_or_else(|| "second capture missing positive cache_bytes".to_string())?;
    if first_cache != second_cache {
        return Err(format!(
            "decoded cache_bytes differ: first={first_cache}, second={second_cache}"
        ));
    }
    Ok(())
}

fn viewer_workloads() -> &'static [&'static str] {
    &wsi_rs_perf::CAPTURE_WORKLOAD_NAMES[1..8]
}

fn codec_thread_budget_enforced(capture: &Value) -> Option<bool> {
    CaptureDocument::parse(capture)
        .ok()?
        .metadata
        .benchmark
        .internal_codec_thread_budget
        .get("enforced_by_harness")?
        .as_bool()
}

fn has_complete_positive_run_metric(capture: &Value, field: &str) -> bool {
    let Ok(capture) = CaptureDocument::parse(capture) else {
        return false;
    };
    field == PEAK_RSS_METRIC
        && !capture.runs.is_empty()
        && capture
            .runs
            .iter()
            .all(|run| run.peak_rss_bytes.is_some_and(|value| value > 0))
}

fn geometric_mean(values: &[f64]) -> Result<f64, String> {
    if values.is_empty()
        || values
            .iter()
            .any(|value| !value.is_finite() || *value <= 0.0)
    {
        return Err("geometric mean requires positive finite ratios".into());
    }
    Ok((values.iter().map(|value| value.ln()).sum::<f64>() / values.len() as f64).exp())
}

#[cfg(test)]
#[path = "tests/acceptance.rs"]
mod tests;
