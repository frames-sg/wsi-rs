use super::*;
use std::path::Path;

fn repo_file(relative: &str) -> String {
    fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join(relative),
    )
    .unwrap_or_else(|error| panic!("read {relative}: {error}"))
}

/// Every `nightly-YYYY-MM-DD` toolchain named in `text`.
fn dated_nightlies(text: &str) -> Vec<&str> {
    text.match_indices("nightly-")
        .filter_map(|(start, _)| text.get(start..start + "nightly-YYYY-MM-DD".len()))
        .filter(|name| {
            name["nightly-".len()..]
                .bytes()
                .enumerate()
                .all(|(index, byte)| match index {
                    4 | 7 => byte == b'-',
                    _ => byte.is_ascii_digit(),
                })
        })
        .collect()
}

fn workspace_package_version() -> String {
    let manifest: toml::Value = toml::from_str(&repo_file("Cargo.toml")).expect("parse Cargo.toml");
    manifest["package"]["version"]
        .as_str()
        .expect("workspace package version")
        .to_string()
}

fn shell_assignment<'a>(script: &'a str, name: &str) -> &'a str {
    let prefix = format!("readonly {name}=\"");
    let line = script
        .lines()
        .find_map(|line| line.strip_prefix(prefix.as_str()))
        .unwrap_or_else(|| panic!("script assigns {name}"));
    line.strip_suffix('"').expect("quoted shell assignment")
}

#[test]
fn nightly_tools_use_the_ci_pinned_toolchain() {
    for workflow in [
        ".github/workflows/ci.yml",
        ".github/workflows/rc-preflight.yml",
    ] {
        let text = repo_file(workflow);
        let nightlies = dated_nightlies(&text);
        assert!(!nightlies.is_empty(), "{workflow} pins a dated nightly");
        assert!(
            nightlies
                .iter()
                .all(|name| *name == PINNED_NIGHTLY_TOOLCHAIN),
            "{workflow} pins {nightlies:?}, xtask pins {PINNED_NIGHTLY_TOOLCHAIN}"
        );
    }
    assert_eq!(
        pinned_nightly_cargo_args(&["public-api", "-p", "wsi-rs"]),
        [
            "run",
            PINNED_NIGHTLY_TOOLCHAIN,
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
    let script = repo_file("scripts/check-semver.sh");
    let version = workspace_package_version();
    let numeric = |version: &str| {
        version
            .split('.')
            .map(|part| part.parse::<u64>().expect("numeric version component"))
            .collect::<Vec<_>>()
    };
    let baseline = shell_assignment(&script, "BASELINE_VERSION");
    assert!(
        numeric(baseline) < numeric(&version),
        "semver baseline {baseline} must be an earlier release than {version}"
    );
    let checksum = shell_assignment(&script, "BASELINE_SHA256");
    assert!(
        checksum.len() == 64 && checksum.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "baseline archive must be pinned by a SHA-256 digest, got {checksum:?}"
    );
    assert!(shell_assignment(&script, "USER_AGENT")
        .starts_with(&format!("wsi-rs-semver-check/{version} ")));
    assert!(script.contains("--baseline-rustdoc"));
    assert!(script.contains(&format!("cargo +{PINNED_NIGHTLY_TOOLCHAIN} rustdoc")));
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
fn public_api_snapshot_update_reports_directory_and_write_failures() {
    let directory = tempfile::tempdir().unwrap();
    let parent_file = directory.path().join("parent-file");
    fs::write(&parent_file, b"not a directory").unwrap();
    let nested = parent_file.join("api.txt");
    assert!(check_public_api_snapshot_with_update("api", &nested, true)
        .unwrap_err()
        .contains("failed to create"));

    assert!(
        check_public_api_snapshot_with_update("api", directory.path(), true)
            .unwrap_err()
            .contains("failed to write")
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
