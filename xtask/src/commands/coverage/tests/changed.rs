use super::*;

#[test]
fn summarize_changed_coverage_reports_missing_files() {
    let coverage = HashMap::from([(
        PathBuf::from("src/lib.rs"),
        FileCoverage {
            lines: BTreeMap::from([(1, 1), (2, 1), (3, 0)]),
            ..FileCoverage::default()
        },
    )]);
    let summary = summarize_changed_coverage(
        &coverage,
        &HashMap::from([
            (PathBuf::from("src/lib.rs"), BTreeSet::from([1, 3])),
            (PathBuf::from("src/missing.rs"), BTreeSet::from([1])),
        ]),
    );

    assert_eq!(summary.found, 2);
    assert_eq!(summary.hit, 1);
    assert_eq!(summary.percent(), 50.0);
    assert_eq!(summary.missing_files, vec![PathBuf::from("src/missing.rs")]);
    assert_eq!(
        summary.files,
        BTreeMap::from([(PathBuf::from("src/lib.rs"), (1, 2))])
    );
}

#[test]
fn changed_coverage_rejects_missing_or_uninstrumented_source() {
    let missing = ChangedCoverageSummary {
        missing_files: vec![PathBuf::from("src/missing.rs")],
        ..ChangedCoverageSummary::default()
    };
    assert!(validate_changed_coverage(&missing, 80.0)
        .unwrap_err()
        .contains("absent from LCOV"));

    let empty = ChangedCoverageSummary::default();
    assert!(validate_changed_coverage(&empty, 80.0)
        .unwrap_err()
        .contains("no instrumented lines"));
}

#[test]
fn changed_coverage_enforces_threshold_after_fail_closed_checks() {
    let below = ChangedCoverageSummary {
        found: 10,
        hit: 7,
        ..ChangedCoverageSummary::default()
    };
    assert!(validate_changed_coverage(&below, 80.0).is_err());

    let passing = ChangedCoverageSummary {
        found: 10,
        hit: 8,
        ..ChangedCoverageSummary::default()
    };
    assert!(validate_changed_coverage(&passing, 80.0).is_ok());
}

#[test]
fn parse_diff_added_lines_keeps_reused_production_lines() {
    let diff = "\
diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -10,0 +11,3 @@
+let moved = true;
+let new_line = true;
+}
";
    let mut lines = HashMap::new();
    parse_diff_added_lines(&mut lines, diff);

    assert_eq!(
        lines,
        HashMap::from([(PathBuf::from("src/lib.rs"), BTreeSet::from([11, 12, 13]))])
    );
}

#[test]
fn declaration_only_sources_have_no_changed_coverage_candidates() {
    assert!(!source_has_function_definition(
        "mod batch;\npub use batch::decode;\npub type Sessions<'a> = Option<&'a ()>;\n"
    ));
    assert!(!source_has_function_definition(
        "#[derive(Debug)]\npub enum Error { Invalid }\n"
    ));
    assert!(source_has_function_definition(
        "pub(crate) fn decode() {}\n"
    ));
    assert!(source_has_function_definition(
        "macro_rules! method { () => { fn generated() {} } }\n"
    ));
}

#[test]
fn module_wiring_changes_are_declaration_only_even_when_the_file_has_functions() {
    let source = "mod candidate;\npub use candidate::is_candidate;\nfn fuzz_only() {}\n";

    assert!(changed_lines_are_declaration_only(
        source,
        &BTreeSet::from([1, 2]),
    ));
    assert!(!changed_lines_are_declaration_only(
        source,
        &BTreeSet::from([3]),
    ));
}

#[test]
fn repository_relative_changed_files_are_read_from_the_git_root() {
    let root = tempfile::tempdir().unwrap();
    let relative = PathBuf::from("src/lib.rs");
    std::fs::create_dir_all(root.path().join("src")).unwrap();
    std::fs::write(root.path().join(&relative), "fn covered() {}\n").unwrap();
    let mut lines = HashMap::new();

    add_repo_file_lines(&mut lines, root.path(), &relative).unwrap();

    assert_eq!(lines[&relative], BTreeSet::from([1]));
}

#[test]
fn coverage_candidates_skip_test_harness_paths() {
    assert!(is_coverage_candidate(Path::new("src/lib.rs")));
    assert!(is_coverage_candidate(Path::new(
        "wsi-rs-openslide-shim/src/lib.rs"
    )));
    assert!(!is_coverage_candidate(Path::new(
        "src/formats/foo/tests.rs"
    )));
    assert!(!is_coverage_candidate(Path::new(
        "src/formats/foo/tests/fixtures.rs"
    )));
    assert!(!is_coverage_candidate(Path::new(
        "src/core/registry/tests/cache.rs"
    )));
    assert!(!is_coverage_candidate(Path::new(
        "src/formats/tiff_family/test_support.rs"
    )));
    assert!(!is_coverage_candidate(Path::new("tests/integration.rs")));
    assert!(!is_coverage_candidate(Path::new("benches/read_paths.rs")));
    assert!(!is_coverage_candidate(Path::new(
        "fuzz/fuzz_targets/open_wsi_bytes.rs"
    )));
    assert!(is_coverage_candidate(Path::new(
        "xtask/src/commands/perf.rs"
    )));
    assert!(is_coverage_candidate(Path::new("perf-runner/src/main.rs")));
}

#[test]
fn options_parse_overrides_defaults() {
    let options = ChangedCoverageOptions::parse(vec![
        "--base".into(),
        "origin/dev".into(),
        "--lcov".into(),
        "coverage/lcov.info".into(),
        "--threshold".into(),
        "85.5".into(),
    ])
    .unwrap();

    assert_eq!(
        options,
        ChangedCoverageOptions {
            base: "origin/dev".into(),
            lcov_path: PathBuf::from("coverage/lcov.info"),
            threshold: 85.5,
        }
    );
}

#[test]
fn options_reject_missing_unknown_and_out_of_range_values() {
    for (arguments, expected) in [
        (vec!["--base"], "--base requires"),
        (vec!["--lcov"], "--lcov requires"),
        (vec!["--threshold"], "--threshold requires"),
        (vec!["--threshold", "invalid"], "invalid --threshold"),
        (vec!["--threshold", "101"], "between 0 and 100"),
        (vec!["--help"], "usage: cargo xtask coverage-changed"),
        (vec!["unknown"], "unknown coverage-changed argument"),
    ] {
        assert!(
            ChangedCoverageOptions::parse(arguments.into_iter().map(str::to_string).collect())
                .unwrap_err()
                .contains(expected)
        );
    }
}

#[test]
fn repository_collection_helpers_report_real_and_invalid_git_inputs() {
    let root = git_repo_root().unwrap();
    assert!(root.join("Cargo.toml").is_file());

    let changed = changed_rust_lines("HEAD").unwrap();
    assert!(changed.keys().all(|path| is_coverage_candidate(path)));
    assert!(changed_rust_lines("definitely-not-a-git-revision").is_err());

    let mut paths = BTreeSet::new();
    collect_git_paths(&mut paths, &["ls-files", "--", "Cargo.toml"]).unwrap();
    assert!(paths.contains(Path::new("Cargo.toml")));
    assert!(collect_git_paths(
        &mut paths,
        &["ls-files", "--error-unmatch", "definitely-missing"]
    )
    .is_err());

    let source = tempfile::Builder::new().suffix(".rs").tempfile().unwrap();
    std::fs::write(source.path(), "one\ntwo\nthree\n").unwrap();
    let mut lines = HashMap::new();
    add_file_lines(&mut lines, source.path()).unwrap();
    assert_eq!(lines[source.path()], BTreeSet::from([1, 2, 3]));

    let missing_root = root.join("definitely-missing-directory");
    let mut changed = HashMap::new();
    assert!(collect_git_diff_lines(&mut changed, &missing_root, &["diff", "--name-only"]).is_err());
    assert!(untracked_rust_paths(&missing_root).is_err());
    assert!(add_repo_file_lines(&mut changed, root.as_path(), Path::new("missing.rs")).is_err());
}
