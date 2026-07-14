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
        BenchLibrary::WsiRs,
        3,
        &[PathBuf::from("tests/fixtures/jp2k/rgb_nomct.j2k")],
        vec![run],
    )
    .expect("capture summary");

    assert_eq!(summary["schema_version"], PERF_CAPTURE_SCHEMA_VERSION);
    assert_eq!(summary["kind"], "wsi_rs-perf-capture");
    assert_eq!(summary["metadata"]["benchmark"]["library"], "wsi_rs");
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
