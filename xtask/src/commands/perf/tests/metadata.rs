use super::*;
use std::path::PathBuf;

fn slide(path: &str, alias: &str, format: &str) -> SlideSpec {
    SlideSpec {
        path: PathBuf::from(path),
        alias: alias.into(),
        format: format.into(),
        benchmark_group: format.into(),
        manifest_sha256: None,
    }
}

fn worker_matrix() -> WorkerMatrix {
    WorkerMatrix {
        counts: vec![1, 2, 8],
        physical_core_count: 8,
        physical_core_method: "test".into(),
    }
}

#[test]
fn rust_codec_dependencies_are_sorted_and_fail_closed() {
    let directory = tempfile::tempdir().unwrap();
    let lock_path = directory.path().join("Cargo.lock");
    std::fs::write(
        &lock_path,
        r#"
version = 4

[[package]]
name = "zstd"
version = "0.13.3"

[[package]]
name = "j2k"
version = "0.6.0"

[[package]]
name = "serde"
version = "1.0.0"

[[package]]
name = "j2k"
version = "0.5.0"

[[package]]
name = "dicom-transfer-syntax-registry"
version = "0.8.2"
"#,
    )
    .unwrap();

    let dependencies = rust_codec_dependencies(&lock_path).expect("codec dependencies");
    assert_eq!(
        dependencies.keys().map(String::as_str).collect::<Vec<_>>(),
        vec!["dicom-transfer-syntax-registry", "j2k", "zstd"]
    );
    assert_eq!(dependencies["j2k"], ["0.5.0", "0.6.0"]);
    assert!(!dependencies.contains_key("serde"));

    std::fs::write(&lock_path, "not valid TOML = [").unwrap();
    assert!(rust_codec_dependencies(&lock_path)
        .unwrap_err()
        .contains("failed to parse"));
    std::fs::remove_file(&lock_path).unwrap();
    assert!(rust_codec_dependencies(&lock_path)
        .unwrap_err()
        .contains("failed to read"));
}

#[test]
fn capture_summary_records_environment_metadata_and_raw_samples() {
    let run = json!({
        "library_sha256": "a".repeat(64),
        "slide_path": "tests/fixtures/jp2k/rgb_nomct.j2k",
        "repeat_index": 0,
        "worker_count": 2,
        "decode_cpu_concurrency": {
            "client_handles": 2,
            "rayon_threads_process_wide": 2,
            "jp2k_threads_per_handle": 1,
            "active_jp2k_thread_budget": 2,
            "enforced": true,
        },
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
        &[slide(
            "tests/fixtures/jp2k/rgb_nomct.j2k",
            "fixture-jp2k",
            "raw_jp2k",
        )],
        &worker_matrix(),
        &["single_tile_l0".into()],
        vec![run],
    )
    .expect("capture summary");

    assert_eq!(summary["schema_version"], PERF_CAPTURE_SCHEMA_VERSION);
    assert_eq!(summary["kind"], "wsi_rs-perf-capture");
    assert_eq!(summary["metadata"]["benchmark"]["library"], "wsi_rs");
    assert_eq!(summary["metadata"]["benchmark"]["binary"], "wsi-rs-perf");
    assert!(summary.get("slides").is_none());
    assert!(summary["metadata"]["benchmark"]
        .get("library_sha256s")
        .is_none());
    let codec_versions = summary["metadata"]["benchmark"]["rust_codec_dependencies"]
        .as_object()
        .expect("Rust codec dependency versions");
    assert!(codec_versions.contains_key("j2k"));
    assert!(codec_versions.contains_key("jpeg-decoder"));
    assert!(codec_versions.contains_key("image"));
    assert!(codec_versions.contains_key("png"));
    assert!(codec_versions.contains_key("dicom-transfer-syntax-registry"));
    assert_eq!(summary["metadata"]["build"]["features"], json!([]));
    assert_eq!(
        summary["metadata"]["benchmark"]["corpus_tier"],
        "public-fixture"
    );
    assert!(summary["metadata"]["benchmark"]
        .get("repeat_count")
        .is_none());
    assert_eq!(
        summary["metadata"]["benchmark"]["client_worker_matrix"],
        json!([1, 2, 8])
    );
    assert_eq!(
        summary["metadata"]["benchmark"]["client_process_concurrency"]["enforced_by_worker"],
        true
    );
    assert_eq!(
        summary["metadata"]["benchmark"]["internal_codec_thread_budget"]["enforced_by_harness"],
        true
    );
    assert_eq!(summary["slide_manifest"][0]["alias"], "fixture-jp2k");
    assert_eq!(summary["slide_manifest"][0]["format"], "raw_jp2k");
    assert_eq!(
        summary["metadata"]["benchmark"]["planned_workloads"],
        json!(["single_tile_l0"])
    );
    assert!(summary["metadata"]["git"]["branch"].is_string());
    assert!(summary["metadata"]["toolchain"]["rustc"].is_string());
    assert!(summary["metadata"]["host"]["cpu"].is_string());
    assert_eq!(summary["runs"][0]["workloads"][0]["samples_us"][2], 30);
}

#[test]
fn capture_summary_marks_missing_decode_controls_as_not_equalized() {
    let summary = capture_summary(
        "incomplete",
        BenchLibrary::WsiRs,
        3,
        &[slide("fixture.svs", "svs-001", "aperio")],
        &worker_matrix(),
        &["single_tile_l0".into()],
        vec![json!({"workloads": []})],
    )
    .expect("capture summary");

    assert_eq!(
        summary["metadata"]["benchmark"]["internal_codec_thread_budget"]["enforced_by_harness"],
        false
    );
    assert_eq!(
        summary["metadata"]["benchmark"]["internal_codec_thread_budget"]["comparison_status"],
        "not_equalized"
    );
}

#[test]
fn openslide_capture_summary_records_competitor_library() {
    let run = json!({
        "library": "openslide",
        "slide_path": "fixture.svs",
        "repeat_index": 0,
        "worker_count": 2,
        "decode_cpu_concurrency": {
            "client_handles": 2,
            "decoder_threads_per_handle": 1,
            "active_decode_thread_budget": 2,
            "enforced": true,
        },
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
        &[slide("fixture.svs", "svs-001", "aperio")],
        &worker_matrix(),
        &["region_2k".into()],
        vec![run],
    )
    .expect("OpenSlide capture summary");

    assert_eq!(summary["metadata"]["benchmark"]["library"], "openslide");
    assert_eq!(summary["metadata"]["benchmark"]["binary"], "wsi-rs-perf");
    assert_eq!(summary["metadata"]["build"]["features"], json!([]));
    assert_eq!(
        summary["metadata"]["benchmark"]["internal_codec_thread_budget"]["enforced_by_harness"],
        true
    );
}
