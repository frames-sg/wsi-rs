use super::help_text;

#[test]
fn help_lists_benchmark_tasks() {
    let help = help_text();

    assert!(help.contains("bench-check"));
    assert!(!help.contains("Criterion"));
    assert!(help.contains("perf-capture"));
    assert!(help.contains("perf-capture-openslide"));
    assert!(help.contains("perf-capture-pair"));
    assert!(help.contains("perf-compare"));
    assert!(help.contains("perf-profile"));
    assert!(help.contains("coverage-changed"));
    assert!(!help.contains("coverage-check"));
    assert!(!help.contains("coverage-device"));
    assert!(help.contains("coverage     generate lcov.info and enforce"));
}

#[test]
fn help_lists_api_stability_task() {
    let help = help_text();

    assert!(help.contains("api-check    run public API and semver stability checks"));
}

#[test]
fn help_lists_doc_test_task() {
    let help = help_text();

    assert!(help.contains("doc-test     compile rustdoc examples with doctest"));
}

#[test]
fn help_lists_fuzzing_task() {
    let help = help_text();

    assert!(help.contains("fuzz-check   type-check cargo-fuzz targets"));
}

#[test]
fn package_help_advertises_package_verification() {
    let help = help_text();

    assert!(help.contains("package      package the crate from a clean worktree with verification"));
    assert!(!help.contains("without verification"));
}

#[test]
fn help_lists_rc_preflight_task() {
    let help = help_text();

    assert!(help.contains("rc-preflight run local release-candidate preflight gates"));
}
