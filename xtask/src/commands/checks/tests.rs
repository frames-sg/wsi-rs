use super::*;
use std::path::Path;

#[test]
fn nightly_tools_use_the_ci_pinned_toolchain() {
    assert_eq!(PINNED_NIGHTLY_TOOLCHAIN, "nightly-2026-04-17");
    assert_eq!(
        pinned_nightly_cargo_args(&["public-api", "-p", "wsi-rs"]),
        [
            "run",
            "nightly-2026-04-17",
            "cargo",
            "public-api",
            "-p",
            "wsi-rs"
        ]
    );
}

#[test]
fn coverage_instruments_workspace_binary_and_library_tests() {
    assert!(COVERAGE_BASE_ARGS.contains(&"--workspace"));
    assert!(!COVERAGE_BASE_ARGS.contains(&"--lib"));
    assert!(!COVERAGE_BASE_ARGS.contains(&"--tests"));
}

#[test]
fn corpus_coverage_report_keeps_every_workspace_package() {
    assert_eq!(
        COVERAGE_REPORT_ARGS,
        [
            "llvm-cov",
            "report",
            "-p",
            "wsi-rs",
            "-p",
            "wsi-rs-openslide-shim",
            "-p",
            "xtask",
            "-p",
            "wsi-rs-perf",
            "--lcov",
            "--output-path",
            "lcov.info"
        ]
    );
}

#[test]
fn semver_check_uses_checksum_pinned_published_baseline() {
    let script = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../scripts/check-semver.sh"),
    )
    .expect("read semver script");
    assert!(script.contains("BASELINE_VERSION=\"0.5.2\""));
    assert!(script.contains(
        "BASELINE_SHA256=\"0118b54cd6fe19b48d9170c1a54a089599e61442f076eff6a6da05d0f3891a98\""
    ));
    assert!(script.contains("USER_AGENT=\"wsi-rs-semver-check/0.6.0"));
    assert!(script.contains("--baseline-rustdoc"));
    assert!(script.contains("cargo +nightly-2026-04-17 rustdoc"));
    assert!(!script.contains("cargo +nightly rustdoc"));
    assert!(!script.contains("skipping cargo-semver-checks"));
}

#[test]
fn semver_check_covers_default_and_device_profiles_using_versioned_compatibility() {
    let script = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../scripts/check-semver.sh"),
    )
    .expect("read semver script");
    assert!(script.contains("profiles=(default cuda)"));
    assert!(script.contains("profiles+=(metal)"));
    assert!(script.contains("if [[ \"$(uname -s)\" == \"Darwin\" ]]"));
    assert!(script.contains("for profile in \"${profiles[@]}\""));
    assert!(!script.contains("for profile in default cuda metal"));
    assert!(!script.contains("--release-type"));
}

#[test]
fn dependency_policy_allows_path_only_dev_crates_without_allowing_registry_wildcards() {
    let config = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../deny.toml"))
        .expect("read cargo-deny config");
    assert!(config.contains("wildcards = \"deny\""));
    assert!(config.contains("allow-wildcard-paths = true"));
}

#[test]
fn public_api_snapshot_comparison_normalizes_newlines_and_reports_drift() {
    let directory = tempfile::tempdir().unwrap();
    let snapshot = directory.path().join("api.txt");
    fs::write(&snapshot, "pub struct Stable;\n").unwrap();

    assert!(
        check_public_api_snapshot_with_update("pub struct Stable;\r\n", &snapshot, false).is_ok()
    );
    let error =
        check_public_api_snapshot_with_update("pub struct Changed;", &snapshot, false).unwrap_err();
    assert!(error.contains("public API snapshot is stale"));
    assert!(error.contains(&snapshot.display().to_string()));

    let missing = directory.path().join("missing.txt");
    assert!(
        check_public_api_snapshot_with_update("anything", &missing, false)
            .unwrap_err()
            .contains("failed to read public API snapshot")
    );
}

#[test]
fn public_api_snapshot_update_creates_parent_and_trailing_newline() {
    let directory = tempfile::tempdir().unwrap();
    let snapshot = directory.path().join("nested/api.txt");

    check_public_api_snapshot_with_update("pub struct Updated;\r\n", &snapshot, true).unwrap();

    assert_eq!(
        fs::read_to_string(snapshot).unwrap(),
        "pub struct Updated;\n"
    );
}

#[test]
fn fuzz_gate_covers_every_declared_fuzz_binary() {
    let manifest =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../fuzz/Cargo.toml"))
            .expect("read fuzz manifest");

    for target in FUZZ_TARGETS {
        assert!(
            manifest.contains(&format!("name = \"{target}\"")),
            "fuzz target {target} is not declared"
        );
    }
    assert_eq!(manifest.matches("[[bin]]").count(), FUZZ_TARGETS.len());
}
