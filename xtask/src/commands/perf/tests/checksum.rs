use super::*;
use serde_json::json;

#[test]
fn capture_checksum_validation_fails_on_nondeterministic_repeat() {
    let capture = json!({
        "schema_version": PERF_CAPTURE_SCHEMA_VERSION,
        "runs": [
            {
                "slide_path": "fixture.svs",
                "repeat_index": 0,
                "slide_sha256": "slide-hash",
                "workloads": [{"name": "pan_trace_l0", "checksum_sha256": "aaa"}],
            },
            {
                "slide_path": "fixture.svs",
                "repeat_index": 1,
                "slide_sha256": "slide-hash",
                "workloads": [{"name": "pan_trace_l0", "checksum_sha256": "bbb"}],
            },
        ],
    });

    let error = validate_capture_checksums(&capture).expect_err("checksum mismatch must fail");

    assert!(error.contains("pan_trace_l0"));
    assert!(error.contains("nondeterministic"));
}

#[test]
fn cross_capture_checksum_validation_rejects_unequal_output() {
    let capture = |checksum: &str| {
        json!({
            "schema_version": PERF_CAPTURE_SCHEMA_VERSION,
            "runs": [{
                "slide_path": "fixture.svs",
                "repeat_index": 0,
                "slide_sha256": "slide-hash",
                "level0_bounds": {"x": 0, "y": 0, "width": 1000, "height": 800},
                "levels": [{"width": 1000, "height": 800, "downsample": 1.0}],
                "workloads": [{"name": "viewport_region_l2", "checksum_sha256": checksum}],
            }],
        })
    };

    let error = validate_cross_capture_checksums(&capture("aaa"), &capture("bbb"))
        .expect_err("different pixels must invalidate comparison");

    assert!(error.contains("viewport_region_l2"));
    assert!(error.contains("output checksum mismatch"));
}

#[test]
fn cross_capture_checksum_validation_rejects_unequal_geometry() {
    let capture = |bounds_x: i64| {
        json!({
            "schema_version": PERF_CAPTURE_SCHEMA_VERSION,
            "runs": [{
                "slide_path": "fixture.svs",
                "repeat_index": 0,
                "slide_sha256": "slide-hash",
                "level0_bounds": {"x": bounds_x, "y": 0, "width": 1000, "height": 800},
                "levels": [{"width": 1000, "height": 800, "downsample": 1.0}],
                "workloads": [{"name": "pan_trace_l0", "checksum_sha256": "same-pixels"}],
            }],
        })
    };

    let error = validate_cross_capture_checksums(&capture(0), &capture(1))
        .expect_err("different geometry must invalidate comparison");

    assert!(error.contains("geometry"), "{error}");
}

#[test]
fn cross_capture_checksum_validation_fails_closed_without_current_schema() {
    let legacy = json!({"runs": []});

    let error = validate_cross_capture_checksums(&legacy, &legacy)
        .expect_err("legacy captures cannot establish output parity");

    assert!(error.contains("requires schema_version"));
}

fn declared_capture() -> Value {
    json!({
        "schema_version": PERF_CAPTURE_SCHEMA_VERSION,
        "repeat_count": 1,
        "slide_manifest": [{"alias": "fixture"}],
        "metadata": {"benchmark": {
            "planned_workloads": ["pan_trace_l0"],
            "client_worker_matrix": [1],
        }},
        "runs": [{
            "slide_path": "fixture.svs",
            "slide_sha256": "slide-hash",
            "alias": "fixture",
            "format": "aperio",
            "benchmark_group": "aperio/jpeg",
            "repeat_index": 0,
            "worker_count": 1,
            "level0_bounds": {"x": 0, "y": 0, "width": 1000, "height": 800},
            "levels": [{"width": 1000, "height": 800, "downsample": 1.0}],
            "workloads": [{"name": "pan_trace_l0", "checksum_sha256": "pixels"}],
        }],
    })
}

#[test]
fn declared_plan_validation_rejects_malformed_declarations() {
    let invalid = [
        ("missing manifest", json!({})),
        ("empty manifest", {
            let mut capture = declared_capture();
            capture["slide_manifest"] = json!([]);
            capture
        }),
        ("duplicate alias", {
            let mut capture = declared_capture();
            capture["slide_manifest"] = json!([{"alias": "fixture"}, {"alias": "fixture"}]);
            capture
        }),
        ("empty workload", {
            let mut capture = declared_capture();
            capture["metadata"]["benchmark"]["planned_workloads"] = json!([""]);
            capture
        }),
        ("duplicate workload", {
            let mut capture = declared_capture();
            capture["metadata"]["benchmark"]["planned_workloads"] =
                json!(["pan_trace_l0", "pan_trace_l0"]);
            capture
        }),
        ("zero worker", {
            let mut capture = declared_capture();
            capture["metadata"]["benchmark"]["client_worker_matrix"] = json!([0]);
            capture
        }),
        ("duplicate worker", {
            let mut capture = declared_capture();
            capture["metadata"]["benchmark"]["client_worker_matrix"] = json!([1, 1]);
            capture
        }),
        ("zero repeats", {
            let mut capture = declared_capture();
            capture["repeat_count"] = json!(0);
            capture
        }),
    ];

    for (case, capture) in invalid {
        assert!(validate_declared_capture_plan(&capture).is_err(), "{case}");
    }
}

#[test]
fn declared_plan_validation_rejects_malformed_or_extra_run_cells() {
    for field in ["alias", "worker_count", "repeat_index", "workloads"] {
        let mut capture = declared_capture();
        capture["runs"][0]
            .as_object_mut()
            .expect("run")
            .remove(field);
        assert!(validate_declared_capture_plan(&capture).is_err(), "{field}");
    }

    let mut duplicate = declared_capture();
    duplicate["runs"][0]["workloads"] = json!([
        {"name": "pan_trace_l0", "checksum_sha256": "pixels"},
        {"name": "pan_trace_l0", "checksum_sha256": "pixels"},
    ]);
    assert!(validate_declared_capture_plan(&duplicate)
        .unwrap_err()
        .contains("duplicate declared"));

    let mut extra = declared_capture();
    extra["runs"][0]["workloads"][0]["name"] = json!("zoom_trace");
    assert!(validate_declared_capture_plan(&extra)
        .unwrap_err()
        .contains("missing declared"));
}

#[test]
fn worker_run_validation_checks_kind_engine_repeat_and_canonical_slide() {
    let slide = crate::commands::perf::worker::workspace_root().join("Cargo.toml");
    let canonical = slide.canonicalize().expect("canonical slide");
    let valid = || {
        json!({
            "schema_version": wsi_rs_perf::WORKER_SCHEMA_VERSION,
            "kind": "wsi-rs-perf-worker",
            "engine": "wsi_rs",
            "repeat_index": 2,
            "slide_path": canonical.display().to_string(),
            "slide_sha256": "b".repeat(64),
            "library_sha256": "a".repeat(64),
            "level0_bounds": {"x": 0, "y": 0, "width": 1000, "height": 800},
            "levels": [{"width": 1000, "height": 800, "downsample": 1.0}],
        })
    };
    assert!(validate_worker_run(&valid(), "wsi_rs", &slide, 2).is_ok());

    for field in ["slide_sha256", "library_sha256", "level0_bounds", "levels"] {
        let mut missing = valid();
        missing
            .as_object_mut()
            .expect("worker result")
            .remove(field);
        assert!(validate_worker_run(&missing, "wsi_rs", &slide, 2)
            .unwrap_err()
            .contains(field));
    }

    let mut stale_schema = valid();
    stale_schema["schema_version"] = json!(0);
    assert!(validate_worker_run(&stale_schema, "wsi_rs", &slide, 2)
        .unwrap_err()
        .contains("schema"));

    for (field, value) in [
        ("kind", json!("wrong")),
        ("engine", json!("openslide")),
        ("repeat_index", json!(3)),
        ("slide_path", json!("wrong")),
    ] {
        let mut run = valid();
        run[field] = value;
        assert!(
            validate_worker_run(&run, "wsi_rs", &slide, 2).is_err(),
            "{field}"
        );
    }
}

#[test]
fn checksum_maps_reject_missing_duplicate_and_changed_cells() {
    assert!(
        validate_capture_checksums(&json!({"schema_version": PERF_CAPTURE_SCHEMA_VERSION}))
            .is_err()
    );
    assert!(validate_capture_checksums(&json!({"schema_version": 1})).is_ok());

    let mut changed_slide = declared_capture();
    let mut repeat = changed_slide["runs"][0].clone();
    repeat["repeat_index"] = json!(1);
    repeat["slide_sha256"] = json!("other-hash");
    changed_slide["runs"]
        .as_array_mut()
        .expect("runs")
        .push(repeat);
    assert!(validate_capture_checksums(&changed_slide)
        .unwrap_err()
        .contains("slide contents changed"));

    let mut duplicate = declared_capture();
    let duplicate_run = duplicate["runs"][0].clone();
    duplicate["runs"]
        .as_array_mut()
        .expect("runs")
        .push(duplicate_run);
    assert!(
        validate_cross_capture_checksums(&duplicate, &declared_capture())
            .unwrap_err()
            .contains("duplicate required")
    );

    let missing = json!({"schema_version": PERF_CAPTURE_SCHEMA_VERSION, "runs": []});
    assert!(
        validate_cross_capture_checksums(&declared_capture(), &missing)
            .unwrap_err()
            .contains("second capture")
    );
    assert!(
        validate_cross_capture_checksums(&missing, &declared_capture())
            .unwrap_err()
            .contains("first capture")
    );
}
