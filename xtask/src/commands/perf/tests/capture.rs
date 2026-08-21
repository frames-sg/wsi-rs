use super::*;
use serde_json::json;

#[test]
fn paired_engine_order_alternates_across_repeats() {
    assert_eq!(
        paired_engine_order(0),
        [BenchLibrary::WsiRs, BenchLibrary::OpenSlide]
    );
    assert_eq!(
        paired_engine_order(1),
        [BenchLibrary::OpenSlide, BenchLibrary::WsiRs]
    );
    assert_eq!(
        paired_engine_order(2),
        [BenchLibrary::WsiRs, BenchLibrary::OpenSlide]
    );
}

#[test]
fn default_worker_matrix_deduplicates_small_physical_core_counts() {
    assert_eq!(default_worker_matrix(8), vec![1, 2, 8]);
    assert_eq!(default_worker_matrix(2), vec![1, 2]);
    assert_eq!(default_worker_matrix(1), vec![1, 2]);
}

#[test]
fn linux_physical_core_parser_counts_unique_socket_core_pairs() {
    let lscpu = "# Core,Socket\n0,0\n0,0\n1,0\n1,0\n0,1\n0,1\n";

    assert_eq!(parse_linux_physical_cores(lscpu), Some(3));
    assert_eq!(parse_linux_physical_cores("# comments only\n"), None);
    assert_eq!(parse_linux_physical_cores("-,0\n0,-\ninvalid\n"), None);
}

#[test]
fn capture_cli_and_worker_matrix_parsers_reject_invalid_inputs() {
    assert!(capture(vec![]).unwrap_err().contains("usage:"));
    assert!(capture_openslide(vec![]).unwrap_err().contains("usage:"));
    assert!(capture_pair(vec![]).unwrap_err().contains("usage:"));

    let args = vec!["label".into(), "a".into(), "b".into()];
    let (label, selectors) = capture_arguments(&args, "perf-capture").expect("arguments");
    assert_eq!(label, "label");
    assert_eq!(selectors, ["a", "b"]);
    assert_eq!(capture_task_name(BenchLibrary::WsiRs), "perf-capture");
    assert_eq!(
        capture_task_name(BenchLibrary::OpenSlide),
        "perf-capture-openslide"
    );

    assert_eq!(parse_worker_matrix("8, 2;1 2").expect("matrix"), [1, 2, 8]);
    assert!(parse_worker_matrix("")
        .unwrap_err()
        .contains("at least one"));
    assert!(parse_worker_matrix("0").unwrap_err().contains("positive"));
    assert!(parse_worker_matrix("wat").unwrap_err().contains("invalid"));
    assert_eq!(parse_positive_usize(" 4 "), Some(4));
    assert_eq!(parse_positive_usize("0"), None);
    assert_eq!(parse_positive_usize("wat"), None);
}

#[test]
fn host_worker_defaults_and_capture_settings_are_nonempty() {
    let (cores, method) = physical_core_count();
    assert!(cores > 0);
    assert!(!method.is_empty());
    let matrix = requested_worker_matrix().expect("worker matrix");
    assert!(!matrix.counts.is_empty());
    assert!(worker_count().expect("worker count") > 0);
    assert!(repeat_count().expect("repeat count") >= DEFAULT_REPEAT_COUNT);
    let settings = capture_settings().expect("capture settings");
    assert!(settings.cache_bytes > 0);
    assert!(!settings.planned_workloads.is_empty());
}

#[test]
fn run_context_rejects_non_objects_and_worker_mismatches() {
    let slide = SlideSpec {
        path: "fixture.svs".into(),
        alias: "fixture".into(),
        format: "aperio".into(),
        benchmark_group: "aperio/jpeg".into(),
        manifest_sha256: None,
    };
    let order = [BenchLibrary::WsiRs];
    assert!(annotate_run_context(
        &mut json!(null),
        json!({"enforced": true}),
        &slide,
        1,
        &order,
        0,
    )
    .unwrap_err()
    .contains("must be an object"));
    assert!(annotate_run_context(
        &mut json!({"worker_count": 2}),
        json!({"enforced": true}),
        &slide,
        1,
        &order,
        0,
    )
    .unwrap_err()
    .contains("did not match"));
}

#[test]
fn custom_single_engine_slide_resolution_preserves_path_identity() {
    let path = crate::commands::perf::worker::workspace_root().join("Cargo.toml");
    let slides = resolve_single_engine_slides(&[path.display().to_string()], BenchLibrary::WsiRs)
        .expect("custom slide");

    assert_eq!(slides.len(), 1);
    assert_eq!(slides[0].format, "custom");
    assert_eq!(slides[0].path, path);
}

#[test]
fn run_context_records_manifest_identity_worker_and_engine_order() {
    let slide = SlideSpec {
        path: "fixture.svs".into(),
        alias: "svs-001".into(),
        format: "aperio".into(),
        benchmark_group: "aperio/jpeg".into(),
        manifest_sha256: Some("manifest-hash".into()),
    };
    let mut run = json!({"worker_count": 2});
    let order = paired_engine_order(1);

    annotate_run_context(
        &mut run,
        crate::commands::perf::worker::decode_cpu_concurrency(BenchLibrary::WsiRs, 2),
        &slide,
        2,
        &order,
        0,
    )
    .expect("run context");

    assert_eq!(run["alias"], "svs-001");
    assert_eq!(run["format"], "aperio");
    assert_eq!(run["benchmark_group"], "aperio/jpeg");
    assert_eq!(run["engine_order"], json!(["openslide", "wsi_rs"]));
    assert_eq!(run["engine_position"], 0);
    assert_eq!(run["decode_cpu_concurrency"]["client_handles"], 2);
    assert_eq!(
        run["decode_cpu_concurrency"]["rayon_threads_process_wide"],
        2
    );
    assert_eq!(run["decode_cpu_concurrency"]["jp2k_threads_per_handle"], 1);
    assert_eq!(
        run["decode_cpu_concurrency"]["active_jp2k_thread_budget"],
        2
    );
    assert_eq!(run["decode_cpu_concurrency"]["enforced"], true);
}

#[test]
fn openslide_run_context_records_per_handle_decode_budget() {
    let slide = SlideSpec {
        path: "fixture.svs".into(),
        alias: "svs-001".into(),
        format: "aperio".into(),
        benchmark_group: "aperio/jpeg".into(),
        manifest_sha256: None,
    };
    let mut run = json!({"worker_count": 4});
    let order = paired_engine_order(0);

    annotate_run_context(
        &mut run,
        crate::commands::perf::worker::decode_cpu_concurrency(BenchLibrary::OpenSlide, 4),
        &slide,
        4,
        &order,
        1,
    )
    .expect("run context");

    assert_eq!(run["decode_cpu_concurrency"]["client_handles"], 4);
    assert_eq!(
        run["decode_cpu_concurrency"]["decoder_threads_per_handle"],
        1
    );
    assert_eq!(
        run["decode_cpu_concurrency"]["active_decode_thread_budget"],
        4
    );
    assert_eq!(run["decode_cpu_concurrency"]["enforced"], true);
}
