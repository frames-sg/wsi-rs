use super::help_text;

#[test]
fn help_lists_supported_tasks_without_obsolete_guidance() {
    let help = help_text();

    for required in [
        "bench-check",
        "perf-capture",
        "perf-capture-openslide",
        "perf-capture-pair",
        "perf-compare",
        "perf-profile",
        "coverage-changed",
        "coverage     generate lcov.info and enforce",
        "api-check    run public API and semver stability checks",
        "doc-test     compile rustdoc examples with doctest",
        "fuzz-check   type-check cargo-fuzz targets",
        "package      package the crate from a clean worktree with verification",
        "rc-preflight run local release-candidate preflight gates",
    ] {
        assert!(help.contains(required), "missing help text: {required}");
    }
    for obsolete in [
        "Criterion",
        "coverage-check",
        "coverage-device",
        "without verification",
    ] {
        assert!(!help.contains(obsolete), "obsolete help text: {obsolete}");
    }
}
