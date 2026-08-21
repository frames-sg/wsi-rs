use super::*;
use crate::commands::perf::PERF_CAPTURE_SCHEMA_VERSION;
use serde_json::{json, Value};

fn compare_captures(before: &Value, after: &Value) -> Result<Vec<Regression>, String> {
    Ok(regressions_from_summaries(&comparison_summaries(
        before, after,
    )?))
}

fn capture_json(values: &[(u32, u64, u64)]) -> Value {
    json!({
        "runs": values.iter().map(|(repeat, p50, p95)| json!({
            "slide_path": "fixture.svs",
            "repeat_index": repeat,
            "workloads": [{
                "name": "single_tile_l0",
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

fn engine_capture(library: &str, p50: u64, p95: u64, p99: u64, rss: u64) -> Value {
    json!({
        "schema_version": PERF_CAPTURE_SCHEMA_VERSION,
        "metadata": {
            "host": {"os": "test", "arch": "test", "cpu": "test-cpu"},
            "build": {
                "profile": "release",
                "features": [],
                "rustflags": null,
                "native_cpu_tuned": false,
            },
            "benchmark": {
                "library": library,
                "cache_bytes": 268_435_456,
                "workloads": ["pan_trace_l0"],
            },
        },
        "runs": (0..3).map(|repeat| json!({
            "slide_path": "fixture.svs",
            "slide_sha256": "slide-hash",
            "repeat_index": repeat,
            "peak_rss_bytes": rss,
            "level0_bounds": {"x": 0, "y": 0, "width": 1000, "height": 800},
            "levels": [{"width": 1000, "height": 800, "downsample": 1.0}],
            "workloads": [{
                "name": "pan_trace_l0",
                "n": 128,
                "p50_us": p50,
                "p95_us": p95,
                "p99_us": p99,
                "mean_us": p50,
                "checksum_sha256": "pixels",
            }],
        })).collect::<Vec<_>>(),
    })
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
fn two_wsi_rs_captures_use_the_five_percent_regression_gate() {
    let before = engine_capture("wsi_rs", 10_000, 20_000, 30_000, 1_000);
    let after = engine_capture("wsi_rs", 11_000, 22_000, 33_000, 1_000);

    assert!(openslide_capture_pair(&before, &after).is_none());
    let regressions = compare_captures(&before, &after).expect("same-engine comparison");

    assert!(regressions.iter().any(|regression| {
        regression.workload == "pan_trace_l0" && regression.metric == "p50_us"
    }));
}

#[test]
fn same_engine_input_validation_rejects_mismatched_output_cells() {
    let capture = |checksum: &str| {
        let mut capture = engine_capture("wsi_rs", 10_000, 20_000, 30_000, 1_000);
        capture["repeat_count"] = json!(3);
        capture["slide_manifest"] = json!([{
            "path": "fixture.svs",
            "alias": "fixture",
            "format": "aperio",
            "benchmark_group": "aperio/jpeg",
        }]);
        capture["metadata"]["benchmark"]["planned_workloads"] = json!(["pan_trace_l0"]);
        capture["metadata"]["benchmark"]["client_worker_matrix"] = json!([1]);
        for run in capture["runs"].as_array_mut().expect("runs") {
            run["alias"] = json!("fixture");
            run["format"] = json!("aperio");
            run["benchmark_group"] = json!("aperio/jpeg");
            run["worker_count"] = json!(1);
            run["workloads"][0]["checksum_sha256"] = json!(checksum);
        }
        capture
    };

    let error = validate_same_engine_inputs(&capture("before"), &capture("after"))
        .expect_err("same-engine output changes must invalidate timing comparison");

    assert!(error.contains("output checksum mismatch"), "{error}");
}

#[test]
fn same_engine_input_validation_rejects_cross_host_results() {
    let capture = || {
        let mut capture = engine_capture("wsi_rs", 10_000, 20_000, 30_000, 1_000);
        capture["repeat_count"] = json!(3);
        capture["slide_manifest"] = json!([{
            "path": "fixture.svs",
            "alias": "fixture",
            "format": "aperio",
            "benchmark_group": "aperio/jpeg",
        }]);
        capture["metadata"]["benchmark"]["planned_workloads"] = json!(["pan_trace_l0"]);
        capture["metadata"]["benchmark"]["client_worker_matrix"] = json!([1]);
        for run in capture["runs"].as_array_mut().expect("runs") {
            run["alias"] = json!("fixture");
            run["format"] = json!("aperio");
            run["benchmark_group"] = json!("aperio/jpeg");
            run["worker_count"] = json!(1);
        }
        capture
    };
    let before = capture();
    let mut after = capture();
    after["metadata"]["host"]["cpu"] = json!("different-cpu");

    let error = validate_same_engine_inputs(&before, &after)
        .expect_err("same-engine baselines from different hosts must not be compared");

    assert!(error.contains("host metadata"), "{error}");
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
fn same_engine_gate_does_not_waive_small_cells_with_large_ratios() {
    let before = capture_json(&[(0, 10, 90), (1, 10, 90), (2, 10, 90)]);
    let after = capture_json(&[(0, 20, 150), (1, 20, 150), (2, 10, 90)]);

    let regressions = compare_captures(&before, &after).expect("compare captures");

    assert!(regressions.iter().any(|regression| {
        regression.workload == "single_tile_l0" && regression.metric == "p50_us"
    }));
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
