use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::Value;

use super::acceptance::{
    evaluate_openslide_acceptance, validate_comparison_context, OPENSLIDE_HEADLINE_RATIO,
};
use super::checksum::{
    validate_capture_checksums, validate_cross_capture_checksums, validate_declared_capture_plan,
};
use super::process_metrics::PEAK_RSS_METRIC;
use super::schema::{CaptureDocument, CaptureWorkload};

const REQUIRED_REPEAT_COUNT: usize = 5;
const REQUIRED_REGRESSED_REPEATS: usize = 3;
pub(super) const REGRESSION_RATIO: f64 = 1.05;
const THROUGHPUT_FLOOR_RATIO: f64 = 0.95;
const RSS_REGRESSION_RATIO: f64 = 1.10;
pub(super) const P95_MIN_SAMPLE_COUNT: u64 = 20;
pub(super) const P99_MIN_SAMPLE_COUNT: u64 = 100;
const PROCESS_METRICS_WORKLOAD: &str = "__process__";
const WORKLOAD_METRICS: [&str; 5] = [
    "p50_us",
    "p95_us",
    "p99_us",
    "mean_us",
    "throughput_bytes_per_second",
];
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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct MetricKey {
    slide_path: String,
    alias: String,
    format: String,
    benchmark_group: String,
    workload: String,
    worker_count: u64,
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
    alias: String,
    format: String,
    benchmark_group: String,
    workload: String,
    worker_count: u64,
    metric: &'static str,
    comparable_runs: usize,
    regressed_runs: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct MetricSummary {
    pub(super) slide_path: String,
    pub(super) alias: String,
    pub(super) format: String,
    pub(super) benchmark_group: String,
    pub(super) workload: String,
    pub(super) worker_count: u64,
    pub(super) metric: &'static str,
    pub(super) comparable_runs: usize,
    pub(super) regressed_runs: usize,
    pub(super) median_before: u64,
    pub(super) median_after: u64,
    pub(super) ratio: f64,
}

pub(in crate::commands) fn compare(args: Vec<String>) -> Result<(), String> {
    if args.len() != 2 {
        return Err("usage: cargo xtask perf-compare <before.json> <after.json>".into());
    }
    let before = read_json(Path::new(&args[0]))?;
    let after = read_json(Path::new(&args[1]))?;
    validate_capture_checksums(&before)?;
    validate_capture_checksums(&after)?;
    if let Some((openslide, wsi_rs)) = openslide_capture_pair(&before, &after) {
        let report = evaluate_openslide_acceptance(openslide, wsi_rs)?;
        println!(
            "OpenSlide acceptance: viewer_ratio={:.3} cells={} target<={OPENSLIDE_HEADLINE_RATIO:.2}",
            report.headline_ratio, report.headline_cells
        );
        if report.failures.is_empty() {
            if report.headline_ratio <= 0.50 {
                println!("OpenSlide 2x viewer p50 win observed (evidence only)");
            }
            println!("OpenSlide viewer p50, tail-latency, cell, and RSS gates passed");
            return Ok(());
        }
        for failure in &report.failures {
            println!("OpenSlide acceptance failure: {failure}");
        }
        return Err(format!(
            "{} OpenSlide acceptance gate(s) failed",
            report.failures.len()
        ));
    }
    validate_same_engine_inputs(&before, &after)?;
    let summaries = comparison_summaries(&before, &after)?;
    if summaries.is_empty() {
        println!("no comparable benchmark metric groups found");
    } else {
        println!("benchmark comparison summary:");
        for summary in &summaries {
            println!(
                "{} alias={} format={} group={} workers={} {} {} median_before={} median_after={} ratio={:.3} regressed_runs={}/{}",
                summary.slide_path,
                summary.alias,
                summary.format,
                summary.benchmark_group,
                summary.worker_count,
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
            "{} alias={} format={} group={} workers={} {} {} regressed in {}/{} comparable runs",
            regression.slide_path,
            regression.alias,
            regression.format,
            regression.benchmark_group,
            regression.worker_count,
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

fn validate_same_engine_inputs(before: &Value, after: &Value) -> Result<(), String> {
    validate_declared_capture_plan(before)?;
    validate_declared_capture_plan(after)?;
    for (label, capture) in [("baseline", before), ("candidate", after)] {
        let repeats = CaptureDocument::parse(capture)?.repeat_count;
        if repeats != REQUIRED_REPEAT_COUNT as u64 {
            return Err(format!(
                "previous-release comparison {label} requires exactly {REQUIRED_REPEAT_COUNT} repeats, found {repeats}"
            ));
        }
    }
    validate_comparison_context(before, after)?;
    validate_cross_capture_checksums(before, after)
}

fn read_json(path: &Path) -> Result<Value, String> {
    let bytes =
        std::fs::read(path).map_err(|err| format!("failed to read {}: {err}", path.display()))?;
    serde_json::from_slice(&bytes)
        .map_err(|err| format!("failed to parse {}: {err}", path.display()))
}

fn openslide_capture_pair<'a>(
    first: &'a Value,
    second: &'a Value,
) -> Option<(&'a Value, &'a Value)> {
    match (capture_library(first), capture_library(second)) {
        (Some("openslide"), Some("wsi_rs")) => Some((first, second)),
        (Some("wsi_rs"), Some("openslide")) => Some((second, first)),
        _ => None,
    }
}

pub(super) fn capture_library(capture: &Value) -> Option<&str> {
    capture
        .get("metadata")?
        .get("benchmark")?
        .get("library")?
        .as_str()
}

pub(super) fn comparison_summaries(
    before: &Value,
    after: &Value,
) -> Result<Vec<MetricSummary>, String> {
    let include_process_metrics = workload_sets_match(before, after);
    let before_metrics = metric_map(before, include_process_metrics)?;
    let after_metrics = metric_map(after, include_process_metrics)?;
    let mut groups: BTreeMap<MetricKey, Vec<MetricPair>> = BTreeMap::new();

    for (key, before_value) in before_metrics {
        if let Some(after_value) = after_metrics.get(&key) {
            let group_key = MetricKey {
                slide_path: key.slide_path,
                alias: key.alias,
                format: key.format,
                benchmark_group: key.benchmark_group,
                workload: key.workload,
                worker_count: key.worker_count,
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
            if pairs.len() < REQUIRED_REPEAT_COUNT {
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
                alias: key.alias,
                format: key.format,
                benchmark_group: key.benchmark_group,
                workload: key.workload,
                worker_count: key.worker_count,
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
    let ratio = metric_ratio(pair.before, pair.after);
    match metric {
        "throughput_bytes_per_second" => ratio < THROUGHPUT_FLOOR_RATIO,
        PEAK_RSS_METRIC => ratio > RSS_REGRESSION_RATIO,
        _ => ratio > REGRESSION_RATIO,
    }
}

fn regressions_from_summaries(summaries: &[MetricSummary]) -> Vec<Regression> {
    summaries
        .iter()
        .filter(|summary| metric_summary_regressed(summary))
        .map(|summary| Regression {
            slide_path: summary.slide_path.clone(),
            alias: summary.alias.clone(),
            format: summary.format.clone(),
            benchmark_group: summary.benchmark_group.clone(),
            workload: summary.workload.clone(),
            worker_count: summary.worker_count,
            metric: summary.metric,
            comparable_runs: summary.comparable_runs,
            regressed_runs: summary.regressed_runs,
        })
        .collect()
}

fn metric_summary_regressed(summary: &MetricSummary) -> bool {
    is_previous_release_gate_metric(summary.metric)
        && summary.comparable_runs >= REQUIRED_REPEAT_COUNT
        && summary.regressed_runs >= REQUIRED_REGRESSED_REPEATS
}

fn is_previous_release_gate_metric(metric: &str) -> bool {
    matches!(
        metric,
        "p50_us" | "p95_us" | "p99_us" | "throughput_bytes_per_second" | PEAK_RSS_METRIC
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
    let Ok(capture) = CaptureDocument::parse(capture) else {
        return BTreeSet::new();
    };
    let declared = if capture.metadata.benchmark.planned_workloads.is_empty() {
        &capture.metadata.benchmark.workloads
    } else {
        &capture.metadata.benchmark.planned_workloads
    };
    if !declared.is_empty() {
        return declared.iter().cloned().collect();
    }
    capture
        .runs
        .iter()
        .flat_map(|run| run.workloads.iter().map(|workload| workload.name.clone()))
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RunMetricKey {
    slide_path: String,
    alias: String,
    format: String,
    benchmark_group: String,
    workload: String,
    worker_count: u64,
    repeat_index: u32,
    metric: &'static str,
}

fn metric_map(
    capture: &Value,
    include_process_metrics: bool,
) -> Result<BTreeMap<RunMetricKey, u64>, String> {
    if capture.get("runs").and_then(Value::as_array).is_none() {
        return Err("capture JSON missing runs array".into());
    }
    let capture = CaptureDocument::parse(capture)?;
    let mut out = BTreeMap::new();
    for run in &capture.runs {
        if run.slide_path.is_empty() {
            return Err("run missing slide_path".into());
        }
        let slide_path = run.slide_path.clone();
        let alias = run.alias().to_string();
        let format = run.format().to_string();
        let benchmark_group = run.benchmark_group().to_string();
        let worker_count = run.worker_count();
        let repeat_index = run
            .repeat_index
            .ok_or_else(|| "run missing repeat_index".to_string())
            .and_then(|value| {
                u32::try_from(value).map_err(|_| format!("repeat_index {value} exceeds u32"))
            })?;
        for workload in &run.workloads {
            if workload.name.is_empty() {
                continue;
            }
            for metric in WORKLOAD_METRICS {
                if !workload_metric_is_comparable(workload, metric) {
                    continue;
                }
                if let Some(value) = workload.metric(metric) {
                    out.insert(
                        RunMetricKey {
                            slide_path: slide_path.clone(),
                            alias: alias.clone(),
                            format: format.clone(),
                            benchmark_group: benchmark_group.clone(),
                            workload: workload.name.clone(),
                            worker_count,
                            repeat_index,
                            metric,
                        },
                        value,
                    );
                }
            }
            for diagnostic in DIAGNOSTIC_METRICS {
                let Some(value) = workload
                    .diagnostics
                    .as_ref()
                    .and_then(|diagnostics| diagnostics.get(diagnostic.cache_name))
                    .and_then(|cache| cache.get(diagnostic.field_name))
                    .and_then(Value::as_u64)
                else {
                    continue;
                };
                out.insert(
                    RunMetricKey {
                        slide_path: slide_path.clone(),
                        alias: alias.clone(),
                        format: format.clone(),
                        benchmark_group: benchmark_group.clone(),
                        workload: workload.name.clone(),
                        worker_count,
                        repeat_index,
                        metric: diagnostic.metric,
                    },
                    value,
                );
            }
        }
        if include_process_metrics {
            if let Some(value) = run.peak_rss_bytes {
                out.insert(
                    RunMetricKey {
                        slide_path,
                        alias,
                        format,
                        benchmark_group,
                        workload: PROCESS_METRICS_WORKLOAD.to_string(),
                        worker_count,
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

fn workload_metric_is_comparable(workload: &CaptureWorkload, metric: &str) -> bool {
    let Some(min_samples) = tail_metric_min_samples(metric) else {
        return true;
    };
    workload
        .sample_count()
        .is_none_or(|sample_count| sample_count >= min_samples)
}

fn tail_metric_min_samples(metric: &str) -> Option<u64> {
    match metric {
        "p95_us" => Some(P95_MIN_SAMPLE_COUNT),
        "p99_us" => Some(P99_MIN_SAMPLE_COUNT),
        _ => None,
    }
}

fn median_u64(values: &mut [u64]) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    Some(values[values.len() / 2])
}

#[cfg(test)]
#[path = "tests/comparison.rs"]
mod tests;
