use super::*;
use crate::commands::perf::PERF_CAPTURE_SCHEMA_VERSION;
use serde_json::{json, Value};

fn engine_capture(library: &str, p50: u64, p95: u64, p99: u64, rss: u64) -> Value {
    const SLIDES: [(&str, &str, &str); 7] = [
        ("svs-001", "aperio", "aperio/jpeg"),
        ("svs-jp2k-001", "aperio", "aperio/j2k"),
        ("ndpi-001", "ndpi", "ndpi/jpeg"),
        ("vms-001", "hamamatsu_vms", "hamamatsu_vms/jpeg"),
        ("leica-001", "leica", "leica/jpeg"),
        ("ventana-001", "ventana", "ventana/jpeg"),
        ("mirax-001", "mirax", "mirax/jpeg"),
    ];
    const WORKERS: [u64; 3] = [1, 2, 4];
    let mut runs = Vec::new();
    for (alias, format, benchmark_group) in SLIDES {
        for worker_count in WORKERS {
            for repeat_index in 0..3 {
                let workloads = viewer_workloads()
                    .iter()
                    .copied()
                    .map(|name| {
                        json!({
                            "name": name,
                            "n": 128,
                            "p50_us": p50,
                            "p95_us": p95,
                            "p99_us": p99,
                            "mean_us": p50,
                            "checksum_sha256": format!("pixels-{alias}-{name}"),
                        })
                    })
                    .collect::<Vec<_>>();
                runs.push(json!({
                    "slide_path": format!("{alias}.svs"),
                    "slide_sha256": format!("hash-{alias}"),
                    "alias": alias,
                    "format": format,
                    "benchmark_group": benchmark_group,
                    "repeat_index": repeat_index,
                    "worker_count": worker_count,
                    "peak_rss_bytes": rss,
                    "level0_bounds": {"x": 0, "y": 0, "width": 1000, "height": 800},
                    "levels": [{"width": 1000, "height": 800, "downsample": 1.0}],
                    "workloads": workloads,
                }));
            }
        }
    }
    json!({
        "schema_version": PERF_CAPTURE_SCHEMA_VERSION,
        "repeat_count": 3,
        "slide_manifest": SLIDES.into_iter().map(|(alias, format, benchmark_group)| json!({
            "path": format!("{alias}.svs"),
            "alias": alias,
            "format": format,
            "benchmark_group": benchmark_group,
        })).collect::<Vec<_>>(),
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
                "workloads": viewer_workloads(),
                "planned_workloads": viewer_workloads(),
                "client_worker_matrix": WORKERS,
                "physical_core_count": 4,
                "internal_codec_thread_budget": {"enforced_by_harness": true},
            },
        },
        "runs": runs,
    })
}

#[test]
fn openslide_acceptance_rejects_a_complete_but_partial_headline_matrix() {
    let partial = |library| {
        let mut capture = engine_capture(library, 10_000, 20_000, 30_000, 1_000);
        capture["slide_manifest"]
            .as_array_mut()
            .expect("slide manifest")
            .retain(|slide| slide["alias"] == "svs-001");
        capture["runs"]
            .as_array_mut()
            .expect("runs")
            .retain(|run| run["alias"] == "svs-001");
        capture
    };
    let openslide = partial("openslide");
    let wsi_rs = partial("wsi_rs");

    let error = evaluate_openslide_acceptance(&openslide, &wsi_rs)
        .expect_err("a declared subset must not certify the final acceptance gate");

    assert!(error.contains("mandatory OpenSlide aliases"), "{error}");
    assert!(error.contains("svs-jp2k-001"), "{error}");
}

#[test]
fn openslide_acceptance_rejects_relabelled_required_benchmark_groups() {
    let relabel = |library| {
        let mut capture = engine_capture(library, 10_000, 20_000, 30_000, 1_000);
        for slide in capture["slide_manifest"]
            .as_array_mut()
            .expect("slide manifest")
        {
            if slide["alias"] == "svs-jp2k-001" {
                slide["benchmark_group"] = json!("aperio/jpeg");
            }
        }
        for run in capture["runs"].as_array_mut().expect("runs") {
            if run["alias"] == "svs-jp2k-001" {
                run["benchmark_group"] = json!("aperio/jpeg");
            }
        }
        capture
    };

    let error = evaluate_openslide_acceptance(&relabel("openslide"), &relabel("wsi_rs"))
        .expect_err("required format groups must not be relabelled for headline weighting");

    assert!(error.contains("svs-jp2k-001"), "{error}");
    assert!(error.contains("aperio/j2k"), "{error}");
}

#[test]
fn openslide_acceptance_rejects_cross_host_or_unequal_cache_comparisons() {
    let openslide = engine_capture("openslide", 10_000, 20_000, 30_000, 1_000);
    let mut wsi_rs = engine_capture("wsi_rs", 5_000, 10_000, 15_000, 1_000);
    wsi_rs["metadata"]["host"]["cpu"] = json!("different-cpu");

    let error = evaluate_openslide_acceptance(&openslide, &wsi_rs)
        .expect_err("cross-host results must not be interchangeable");
    assert!(error.contains("host metadata"), "{error}");

    let mut wsi_rs = engine_capture("wsi_rs", 5_000, 10_000, 15_000, 1_000);
    wsi_rs["metadata"]["benchmark"]["cache_bytes"] = json!(1);
    let error = evaluate_openslide_acceptance(&openslide, &wsi_rs)
        .expect_err("unequal decoded-cache budgets must not be compared");
    assert!(error.contains("cache_bytes"), "{error}");
}

#[test]
fn mandatory_matrix_requires_all_viewer_workloads_and_host_worker_counts() {
    let mut missing_workload = engine_capture("openslide", 10_000, 20_000, 30_000, 1_000);
    missing_workload["metadata"]["benchmark"]["planned_workloads"]
        .as_array_mut()
        .expect("planned workloads")
        .retain(|workload| workload != "cache_pressure_l0");
    for run in missing_workload["runs"].as_array_mut().expect("runs") {
        run["workloads"]
            .as_array_mut()
            .expect("workloads")
            .retain(|workload| workload["name"] != "cache_pressure_l0");
    }
    validate_declared_capture_plan(&missing_workload).expect("internally complete subset");
    assert!(validate_mandatory_acceptance_matrix(&missing_workload)
        .unwrap_err()
        .contains("cache_pressure_l0"));

    let mut missing_worker = engine_capture("openslide", 10_000, 20_000, 30_000, 1_000);
    missing_worker["metadata"]["benchmark"]["client_worker_matrix"] = json!([1, 2]);
    missing_worker["runs"]
        .as_array_mut()
        .expect("runs")
        .retain(|run| run["worker_count"] != 4);
    validate_declared_capture_plan(&missing_worker).expect("internally complete worker subset");
    assert!(validate_mandatory_acceptance_matrix(&missing_worker)
        .unwrap_err()
        .contains("[4]"));
}

#[test]
fn openslide_acceptance_requires_two_x_and_tail_memory_guards() {
    let openslide = engine_capture("openslide", 10_000, 20_000, 30_000, 1_000);
    let twice_as_fast = engine_capture("wsi_rs", 5_000, 10_000, 15_000, 1_200);

    let report =
        evaluate_openslide_acceptance(&openslide, &twice_as_fast).expect("comparable captures");

    assert_eq!(report.headline_ratio, 0.5);
    assert!(report.failures.is_empty());

    let too_slow = engine_capture("wsi_rs", 6_000, 10_000, 15_000, 1_200);
    let report = evaluate_openslide_acceptance(&openslide, &too_slow).expect("comparable captures");
    assert!(report
        .failures
        .iter()
        .any(|failure| failure.contains("2x headline")));

    let too_much_memory = engine_capture("wsi_rs", 5_000, 10_000, 15_000, 1_201);
    let report =
        evaluate_openslide_acceptance(&openslide, &too_much_memory).expect("comparable captures");
    assert!(report
        .failures
        .iter()
        .any(|failure| failure.contains("peak RSS")));
}

#[test]
fn openslide_acceptance_fails_closed_without_peak_rss_measurements() {
    let openslide = engine_capture("openslide", 10_000, 20_000, 30_000, 1_000);
    let mut wsi_rs = engine_capture("wsi_rs", 5_000, 10_000, 15_000, 1_000);
    for run in wsi_rs["runs"].as_array_mut().expect("runs") {
        run.as_object_mut()
            .expect("run object")
            .remove("peak_rss_bytes");
    }

    let report =
        evaluate_openslide_acceptance(&openslide, &wsi_rs).expect("otherwise comparable captures");

    assert!(report
        .failures
        .iter()
        .any(|failure| failure.contains("peak RSS measurements are missing")));
}

#[test]
fn openslide_acceptance_rejects_missing_mandatory_tail_metrics() {
    let mut openslide = engine_capture("openslide", 10_000, 20_000, 30_000, 1_000);
    let mut wsi_rs = engine_capture("wsi_rs", 5_000, 10_000, 15_000, 1_000);
    for capture in [&mut openslide, &mut wsi_rs] {
        for run in capture["runs"].as_array_mut().expect("runs") {
            let workload = run["workloads"]
                .as_array_mut()
                .expect("workloads")
                .iter_mut()
                .find(|workload| workload["name"] == "viewport_region_l2")
                .expect("viewport workload");
            workload
                .as_object_mut()
                .expect("workload object")
                .remove("p99_us");
        }
    }

    let error = evaluate_openslide_acceptance(&openslide, &wsi_rs)
        .expect_err("missing p99 must not be silently omitted");

    assert!(error.contains("viewport_region_l2"), "{error}");
    assert!(error.contains("p99_us"), "{error}");
}

#[test]
fn openslide_acceptance_fails_when_codec_threads_are_observational_only() {
    let mut openslide = engine_capture("openslide", 10_000, 20_000, 30_000, 1_000);
    openslide["metadata"]["benchmark"]["internal_codec_thread_budget"] = json!({
        "enforced_by_harness": false,
    });
    let wsi_rs = engine_capture("wsi_rs", 5_000, 10_000, 15_000, 1_000);

    let report = evaluate_openslide_acceptance(&openslide, &wsi_rs).expect("comparable captures");

    assert!(report
        .failures
        .iter()
        .any(|failure| failure.contains("not equalized")));
}

#[test]
fn openslide_acceptance_fails_closed_when_codec_controls_are_missing() {
    let mut openslide = engine_capture("openslide", 10_000, 20_000, 30_000, 1_000);
    openslide["metadata"]["benchmark"]
        .as_object_mut()
        .expect("benchmark metadata")
        .remove("internal_codec_thread_budget");
    let wsi_rs = engine_capture("wsi_rs", 5_000, 10_000, 15_000, 1_000);

    let report = evaluate_openslide_acceptance(&openslide, &wsi_rs).expect("comparable captures");

    assert!(report
        .failures
        .iter()
        .any(|failure| failure.contains("not equalized")));
}

#[test]
fn openslide_acceptance_rejects_a_workload_missing_from_both_captures() {
    let mut openslide = engine_capture("openslide", 10_000, 20_000, 30_000, 1_000);
    let mut wsi_rs = engine_capture("wsi_rs", 5_000, 10_000, 15_000, 1_000);
    for capture in [&mut openslide, &mut wsi_rs] {
        for run in capture["runs"].as_array_mut().expect("runs") {
            run["workloads"]
                .as_array_mut()
                .expect("workloads")
                .retain(|workload| workload["name"] != "zoom_trace");
        }
    }

    let error = evaluate_openslide_acceptance(&openslide, &wsi_rs)
        .expect_err("symmetric workload omission must fail closed");

    assert!(error.contains("missing declared cell"), "{error}");
    assert!(error.contains("workload=zoom_trace"), "{error}");
}

#[test]
fn openslide_acceptance_rejects_a_worker_count_missing_from_both_captures() {
    let mut openslide = engine_capture("openslide", 10_000, 20_000, 30_000, 1_000);
    let mut wsi_rs = engine_capture("wsi_rs", 5_000, 10_000, 15_000, 1_000);
    for capture in [&mut openslide, &mut wsi_rs] {
        capture["runs"]
            .as_array_mut()
            .expect("runs")
            .retain(|run| run["worker_count"] != 2);
    }

    let error = evaluate_openslide_acceptance(&openslide, &wsi_rs)
        .expect_err("symmetric worker omission must fail closed");

    assert!(error.contains("missing declared cell"), "{error}");
    assert!(error.contains("workers=2"), "{error}");
}

#[test]
fn openslide_acceptance_rejects_a_repeat_missing_from_both_captures() {
    let mut openslide = engine_capture("openslide", 10_000, 20_000, 30_000, 1_000);
    let mut wsi_rs = engine_capture("wsi_rs", 5_000, 10_000, 15_000, 1_000);
    for capture in [&mut openslide, &mut wsi_rs] {
        capture["repeat_count"] = json!(4);
    }

    let error = evaluate_openslide_acceptance(&openslide, &wsi_rs)
        .expect_err("symmetric repeat omission must fail closed");

    assert!(error.contains("missing declared cell"), "{error}");
    assert!(error.contains("repeat=3"), "{error}");
}

#[test]
fn cache_pressure_is_a_headline_and_critical_viewer_cell() {
    let openslide = engine_capture("openslide", 10_000, 20_000, 30_000, 1_000);
    let mut wsi_rs = engine_capture("wsi_rs", 5_000, 10_000, 15_000, 1_000);
    for run in wsi_rs["runs"].as_array_mut().expect("runs") {
        for workload in run["workloads"].as_array_mut().expect("workloads") {
            if workload["name"] == "cache_pressure_l0" {
                workload["p50_us"] = json!(10_600);
                workload["p95_us"] = json!(21_200);
                workload["p99_us"] = json!(31_800);
            }
        }
    }

    let report = evaluate_openslide_acceptance(&openslide, &wsi_rs).expect("comparable captures");

    assert_eq!(report.headline_cells, 147);
    assert!(report
        .failures
        .iter()
        .any(|failure| failure.contains("cache_pressure_l0")));
}

#[test]
fn openslide_headline_weights_formats_before_slides() {
    let summaries = [
        headline_summary("aperio/jpeg", 0.25),
        headline_summary("aperio/jpeg", 0.25),
        headline_summary("ndpi/jpeg", 1.0),
    ];
    let (ratio, _) = headline_viewer_ratio(&summaries).expect("headline ratio");

    assert!((ratio - 0.5).abs() < f64::EPSILON);
}

#[test]
fn openslide_headline_weights_aperio_codecs_as_separate_groups() {
    let summaries = [
        headline_summary("aperio/jpeg", 0.25),
        headline_summary("aperio/jpeg", 0.25),
        headline_summary("aperio/j2k", 1.0),
    ];
    let (ratio, _) = headline_viewer_ratio(&summaries).expect("headline ratio");

    assert!((ratio - 0.5).abs() < f64::EPSILON);
}

#[test]
fn openslide_comparison_rejects_a_missing_worker_cell() {
    let openslide = engine_capture("openslide", 10_000, 20_000, 30_000, 1_000);
    let mut wsi_rs = engine_capture("wsi_rs", 5_000, 10_000, 15_000, 1_000);
    wsi_rs["runs"]
        .as_array_mut()
        .expect("runs")
        .retain(|run| !(run["alias"] == "svs-001" && run["worker_count"] == 2));

    let error = evaluate_openslide_acceptance(&openslide, &wsi_rs)
        .expect_err("missing required worker cell must fail closed");

    assert!(error.contains("missing declared"), "{error}");
    assert!(error.contains("workers=2"), "{error}");
}

fn headline_summary(benchmark_group: &str, ratio: f64) -> super::super::comparison::MetricSummary {
    super::super::comparison::MetricSummary {
        slide_path: "fixture.svs".into(),
        alias: "fixture".into(),
        format: "fixture".into(),
        benchmark_group: benchmark_group.into(),
        workload: "pan_trace_l0".into(),
        worker_count: 1,
        metric: "p50_us",
        comparable_runs: 3,
        regressed_runs: 0,
        median_before: 10_000,
        median_after: (10_000.0 * ratio) as u64,
        ratio,
    }
}
