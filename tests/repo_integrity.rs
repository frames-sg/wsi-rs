use std::{
    fs,
    path::{Path, PathBuf},
};

fn crate_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn read_repo_text(relative: &str) -> String {
    let path = crate_root().join(relative);
    if path.is_dir() {
        let mut files = Vec::new();
        collect_text_files(&path, &mut files);
        files.retain(|path| path.extension().and_then(|value| value.to_str()) == Some("rs"));
        files.sort();
        return files
            .into_iter()
            .map(|path| {
                fs::read_to_string(&path).unwrap_or_else(|err| {
                    panic!("read {}: {err}", path.display());
                })
            })
            .collect::<Vec<_>>()
            .join("\n");
    }
    fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

fn markdown_link_targets(markdown: &str) -> Vec<&str> {
    markdown
        .split("](")
        .skip(1)
        .filter_map(|tail| tail.split_once(')').map(|(target, _)| target))
        .collect()
}

fn path_matches_package_exclude(path: &str, exclude: &str) -> bool {
    if let Some(prefix) = exclude.strip_suffix("/**") {
        path.starts_with(&format!("{prefix}/"))
    } else {
        path == exclude
    }
}

#[test]
fn package_metadata_uses_wsi_rs_identity() {
    let manifest = fs::read_to_string(crate_root().join("Cargo.toml")).expect("read manifest");

    assert!(
        manifest.contains("name = \"wsi-rs\""),
        "crate package name must be wsi-rs"
    );
    assert!(
        manifest.contains("description = \"wsi-rs whole-slide image reader\""),
        "crate description must use the wsi-rs name"
    );
    assert!(
        manifest.contains("repository = \"https://github.com/frames-sg/wsi-rs\""),
        "crate repository metadata must point at the renamed wsi-rs repo"
    );
}

#[test]
fn changelog_records_release_hardening() {
    let changelog = fs::read_to_string(crate_root().join("CHANGELOG.md")).expect("read changelog");

    for required in [
        "## [Unreleased]",
        "Removed internal release/stability/architecture Markdown files",
        "## [0.4.0] - 2026-05-27",
        "cargo xtask rc-preflight",
        "API snapshot",
        "fuzz",
        "supply chain",
    ] {
        assert!(
            changelog.contains(required),
            "CHANGELOG must keep concise release marker `{required}`"
        );
    }
}

#[test]
fn docs_rs_metadata_keeps_public_docs_build_portable() {
    let manifest = fs::read_to_string(crate_root().join("Cargo.toml")).expect("read manifest");
    let manifest = manifest
        .parse::<toml::Value>()
        .expect("Cargo.toml must parse as TOML");
    let docs_rs = manifest
        .get("package")
        .and_then(toml::Value::as_table)
        .and_then(|package| package.get("metadata"))
        .and_then(toml::Value::as_table)
        .and_then(|metadata| metadata.get("docs"))
        .and_then(toml::Value::as_table)
        .and_then(|docs| docs.get("rs"))
        .and_then(toml::Value::as_table)
        .expect("Cargo.toml must define [package.metadata.docs.rs]");

    assert_eq!(
        docs_rs
            .get("no-default-features")
            .and_then(toml::Value::as_bool),
        Some(true),
        "docs.rs builds must be explicit about default features"
    );
    assert_ne!(
        docs_rs.get("all-features").and_then(toml::Value::as_bool),
        Some(true),
        "docs.rs must not enable every feature because some features are platform or system dependent"
    );

    let features = docs_rs
        .get("features")
        .and_then(toml::Value::as_array)
        .expect("docs.rs metadata must define the feature list explicitly");
    let features = features
        .iter()
        .map(|feature| feature.as_str().expect("docs.rs features must be strings"))
        .collect::<Vec<_>>();
    for excluded in ["metal", "openslide-bench", "parity-metal", "cuda"] {
        assert!(
            !features.contains(&excluded),
            "docs.rs metadata must not enable platform/system-dependent feature `{excluded}`"
        );
    }
}

#[test]
fn crate_docs_expose_compile_checked_quickstart() {
    let lib = fs::read_to_string(crate_root().join("src/lib.rs")).expect("read lib");

    for required in [
        "//! # wsi-rs",
        "//! ## Quick Start",
        "//! ## Tile Reads",
        "//! ```rust,no_run",
        "RegionRequest::builder",
        "TileRequest::builder",
        "read_region_rgba",
        "TileOutputPreference::cpu",
    ] {
        assert!(
            lib.contains(required),
            "crate-level docs must expose compile-checked public docs.rs quickstart content; missing `{required}`"
        );
    }
    assert!(
        lib.contains("TilePixels::Device(_) => unreachable!(\"CPU output was requested\"),\n//!         _ => unreachable!(\"CPU output was requested\"),"),
        "crate docs must handle #[non_exhaustive] TilePixels with a wildcard arm"
    );
}

#[test]
fn repository_hygiene_files_match_public_release_policy() {
    assert!(
        crate_root().join("rust-toolchain.toml").is_file(),
        "wsi_rs should pin the contributor/MSRV toolchain like the sibling repos"
    );

    let gitignore = fs::read_to_string(crate_root().join(".gitignore")).expect("read gitignore");
    assert!(
        gitignore.lines().any(|line| line.trim() == "lcov.info"),
        "coverage output lcov.info must stay ignored"
    );

    for removed in [
        "architecture.md",
        "CODE_OF_CONDUCT.md",
        "CONTRIBUTING.md",
        "docs/architecture.md",
        "docs/rc-evidence-template.md",
        "docs/release-gates.md",
        "docs/stability.md",
    ] {
        assert!(
            !crate_root().join(removed).exists(),
            "internal release/architecture markdown should not be tracked as public repo surface: {removed}"
        );
    }
}

#[test]
fn public_docs_use_wsi_rs_entrypoint() {
    let readme = fs::read_to_string(crate_root().join("README.md")).expect("read README");

    assert!(
        readme.contains("# wsi-rs"),
        "README must title the crate wsi-rs"
    );
    assert!(
        readme.contains("use wsi_rs::"),
        "README quick start must import wsi_rs"
    );
    for required in [
        "cargo add wsi-rs",
        "read_region_rgba",
        "SlideOpenOptions",
        "OpenSlide Compatibility Shim",
        "cargo xtask validate",
    ] {
        assert!(
            readme.contains(required),
            "README must keep public usability docs current; missing `{required}`"
        );
    }
    for required in [
        "RegionRequest::builder",
        "TileRequest::builder",
        "`.j2k`, `.j2c`",
    ] {
        assert!(
            readme.contains(required),
            "README must document current public behavior; missing `{required}`"
        );
    }
    for stale in [
        "wsi_rs = \"0.1\"",
        "version = \"0.2\"",
        "TileOutputPreference::metal()",
        "Phase 7a",
        "sv-slide",
        "`.jp2`, `.jpc`",
        "TBD: replace",
        "sibling j2k",
        "let region = RegionRequest {",
        "let req = TileRequest {",
        "adapter crates they name",
        "j2k-jpeg-metal",
        "j2k-metal",
        "j2k-jpeg-metal = \"0.4\"",
        "j2k-metal = \"0.4\"",
        "MetalBackendSession::new",
    ] {
        assert!(
            !readme.contains(stale),
            "README must not retain stale public docs text `{stale}`"
        );
    }
    for stale in [
        "docs/stability.md",
        "docs/release-gates.md",
        "docs/architecture.md",
        "CONTRIBUTING.md",
        "CODE_OF_CONDUCT.md",
        "core/types.rs",
        "core/registry.rs",
        "formats/dicom.rs",
        "pixel_access.rs",
        "TileRequest { scene",
        "RegionRequest { scene",
        "via j2k_metal",
        "MetalDeviceTile + j2k Metal sessions",
    ] {
        assert!(
            !readme.contains(stale),
            "README must not retain stale docs/module text `{stale}`"
        );
    }
}

#[test]
fn readme_does_not_advertise_removed_benchmark_tooling() {
    let readme = fs::read_to_string(crate_root().join("README.md")).expect("read README");

    for removed in [
        "`bench`",
        "`openslide-bench`",
        "cargo xtask bench",
        "cargo xtask perf-capture",
        "cargo xtask perf-capture-openslide",
        "cargo xtask perf-compare",
        "cargo xtask perf-profile",
        "wsi_bench",
        "openslide_bench",
        "bench_driver",
        "benches/",
    ] {
        assert!(
            !readme.contains(removed),
            "README must not advertise removed benchmark tooling `{removed}`"
        );
    }
}

#[test]
fn published_readme_links_are_package_safe() {
    let readme = fs::read_to_string(crate_root().join("README.md")).expect("read README");
    let manifest = fs::read_to_string(crate_root().join("Cargo.toml")).expect("read manifest");
    let manifest = manifest
        .parse::<toml::Value>()
        .expect("Cargo.toml must parse as TOML");
    let workspace_members = manifest
        .get("workspace")
        .and_then(toml::Value::as_table)
        .and_then(|workspace| workspace.get("members"))
        .and_then(toml::Value::as_array)
        .expect("workspace.members must be present");
    let workspace_member_dirs = workspace_members
        .iter()
        .filter_map(toml::Value::as_str)
        .filter(|member| *member != ".")
        .collect::<Vec<_>>();
    let excludes = manifest
        .get("package")
        .and_then(toml::Value::as_table)
        .and_then(|package| package.get("exclude"))
        .and_then(toml::Value::as_array)
        .expect("package.exclude must be present");

    for target in markdown_link_targets(&readme) {
        if target.starts_with("http://")
            || target.starts_with("https://")
            || target.starts_with('#')
            || target.starts_with("mailto:")
        {
            continue;
        }
        let target = target.split_once('#').map_or(target, |(path, _)| path);
        if target.is_empty() {
            continue;
        }

        assert!(
            workspace_member_dirs
                .iter()
                .all(|member| !target.starts_with(&format!("{member}/"))),
            "README link `{target}` points into a workspace member that is not packaged with the published crate"
        );
        for exclude in excludes.iter().filter_map(toml::Value::as_str) {
            assert!(
                !path_matches_package_exclude(target, exclude),
                "README link `{target}` points at excluded package path `{exclude}`"
            );
        }
    }
}

#[test]
fn published_external_targets_use_request_constructors() {
    for relative in [
        "README.md",
        "examples/extract_jpeg_tiles.rs",
        "examples/fw01_trace_pattern.rs",
        "src/bin/svcache.rs",
        "wsi-rs-openslide-shim/src/lib.rs",
    ] {
        let text = fs::read_to_string(crate_root().join(relative))
            .unwrap_or_else(|err| panic!("read {relative}: {err}"));
        for (line_index, line) in text.lines().enumerate() {
            if line.contains("->") {
                continue;
            }
            for stale in ["RegionRequest {", "TileRequest {", "TileViewRequest {"] {
                assert!(
                    !line.contains(stale),
                    "{relative}:{} should show request constructor APIs instead of direct request struct literals",
                    line_index + 1
                );
            }
        }
    }
}

#[test]
fn manifest_targets_are_not_excluded_from_package() {
    let manifest = fs::read_to_string(crate_root().join("Cargo.toml")).expect("read manifest");
    let manifest = manifest
        .parse::<toml::Value>()
        .expect("Cargo.toml must parse as TOML");
    let excludes = manifest
        .get("package")
        .and_then(toml::Value::as_table)
        .and_then(|package| package.get("exclude"))
        .and_then(toml::Value::as_array)
        .expect("package.exclude must be present")
        .iter()
        .filter_map(toml::Value::as_str)
        .collect::<Vec<_>>();

    for bench in manifest
        .get("bench")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
    {
        let bench = bench.as_table().expect("bench target must be a table");
        let name = bench
            .get("name")
            .and_then(toml::Value::as_str)
            .expect("bench target must have a name");
        let path = bench
            .get("path")
            .and_then(toml::Value::as_str)
            .map_or_else(|| format!("benches/{name}.rs"), ToOwned::to_owned);
        for exclude in &excludes {
            assert!(
                !path_matches_package_exclude(&path, exclude),
                "Cargo.toml declares benchmark target `{name}` at `{path}` but excludes it from the published package via `{exclude}`"
            );
        }
    }
}

#[test]
fn readme_and_security_cover_public_support_surface() {
    let readme = fs::read_to_string(crate_root().join("README.md")).expect("read README");
    let security = fs::read_to_string(crate_root().join("SECURITY.md")).expect("read security");
    let manifest = fs::read_to_string(crate_root().join("Cargo.toml")).expect("read manifest");

    for input_family in [
        "TIFF-family WSI",
        "DICOM VL WSI",
        "Zeiss",
        "MIRAX",
        "Hamamatsu VMS/VMU",
        "Olympus VSI",
        "Raw JPEG 2000 codestream",
        ".svcache",
    ] {
        assert!(
            readme.contains(input_family),
            "README must document input family `{input_family}`"
        );
    }

    for feature in ["`metal`", "`cuda`", "`parity-openslide`", "`parity-metal`"] {
        assert!(
            readme.contains(feature),
            "README must document Cargo feature {feature}"
        );
    }

    for required in [
        "Supported Inputs",
        "Features",
        "Unsupported or incomplete sources return `WsiError`",
        "RegionRequest::builder",
        "TileRequest::builder",
        "cargo xtask rc-preflight",
        "cargo xtask fuzz-check",
    ] {
        assert!(readme.contains(required), "README must state `{required}`");
    }

    assert!(
        security.contains("latest published release, the 0.4 stabilization line, and main"),
        "SECURITY.md must not retain stale supported-version policy"
    );
    assert!(
        !manifest.contains("\"docs/**\""),
        "Cargo.toml must not carry stale docs package rules"
    );

    for removed in [
        "CODE_OF_CONDUCT.md",
        "CONTRIBUTING.md",
        "docs/stability.md",
        "docs/release-gates.md",
        "docs/rc-evidence-template.md",
    ] {
        assert!(
            !readme.contains(removed),
            "README must not link removed internal markdown `{removed}`"
        );
    }
}

#[test]
fn supply_chain_policy_documents_duplicate_allowances() {
    let deny = fs::read_to_string(crate_root().join("deny.toml")).expect("read deny config");
    let deny = deny
        .parse::<toml::Value>()
        .expect("deny.toml must parse as TOML");
    let bans = deny
        .get("bans")
        .and_then(toml::Value::as_table)
        .expect("deny.toml must define [bans]");
    let skip = bans
        .get("skip")
        .and_then(toml::Value::as_array)
        .expect("duplicate dependency skip list must be explicit");

    for crate_spec in [
        "getrandom@0.3.4",
        "r-efi@5.3.0",
        "thiserror@1.0.69",
        "thiserror-impl@1.0.69",
    ] {
        let reason = skip
            .iter()
            .filter_map(toml::Value::as_table)
            .find(|entry| entry.get("crate").and_then(toml::Value::as_str) == Some(crate_spec))
            .and_then(|entry| entry.get("reason"))
            .and_then(toml::Value::as_str)
            .unwrap_or("");
        assert!(
            reason.len() >= 32,
            "cargo-deny duplicate skip `{crate_spec}` must include a concrete rationale"
        );
    }
}

#[test]
fn release_candidate_parity_aliases_are_wired_in_ci() {
    let ci = fs::read_to_string(crate_root().join(".github/workflows/ci.yml")).expect("read CI");
    let public_manifest =
        fs::read_to_string(crate_root().join("tests/fixtures/parity_corpus.public.toml"))
            .expect("read public parity manifest");
    let manifest = public_manifest
        .parse::<toml::Value>()
        .expect("public parity manifest must parse as TOML");
    let aliases = manifest
        .get("slide")
        .and_then(toml::Value::as_array)
        .expect("public parity manifest must define slide entries")
        .iter()
        .filter_map(toml::Value::as_table)
        .filter(|entry| {
            entry.get("redistributable").and_then(toml::Value::as_bool) == Some(true)
                && entry
                    .get("must_decode")
                    .and_then(toml::Value::as_array)
                    .is_some_and(|levels| !levels.is_empty())
        })
        .map(|entry| {
            entry
                .get("alias")
                .and_then(toml::Value::as_str)
                .expect("public parity entry must define an alias")
        })
        .collect::<Vec<_>>();
    assert!(
        aliases.len() >= 8,
        "release-candidate parity gate should cover the available redistributable public vendor corpus"
    );

    for alias in aliases {
        assert!(
            public_manifest.contains(&format!("alias            = \"{alias}\"")),
            "public parity manifest must define release-candidate alias `{alias}`"
        );
        assert!(
            ci.contains(alias),
            "CI parity corpus job must fetch and run release-candidate alias `{alias}`"
        );
    }
}

#[test]
fn release_candidate_parity_gate_runs_openslide_oracle() {
    let ci = fs::read_to_string(crate_root().join(".github/workflows/ci.yml")).expect("read CI");
    let xtask_checks = fs::read_to_string(crate_root().join("xtask/src/commands/checks.rs"))
        .expect("read xtask checks");
    let parity_gate_start = xtask_checks
        .find("pub(super) fn parity_corpus_test()")
        .expect("xtask checks must define parity_corpus_test");
    let parity_gate = &xtask_checks[parity_gate_start..];
    let parity_gate_end = parity_gate
        .find("\npub(super) fn doc")
        .expect("parity_corpus_test must appear before doc task");
    let parity_gate = &parity_gate[..parity_gate_end];

    for required in [
        "\"openslide_parity\"",
        "\"--features\"",
        "\"parity-openslide\"",
        "\"dicom_public_corpus_matches_openslide_within_tolerance\"",
    ] {
        assert!(
            parity_gate.contains(required),
            "cargo xtask parity-corpus-test must include OpenSlide oracle gate `{required}`"
        );
    }

    for required in ["apt-get update", "libopenslide0"] {
        assert!(
            ci.contains(required),
            "CI parity corpus job must install OpenSlide runtime dependency `{required}`"
        );
    }
}

#[test]
fn public_parity_expected_failures_are_reviewable_release_exceptions() {
    let public_manifest =
        fs::read_to_string(crate_root().join("tests/fixtures/parity_corpus.public.toml"))
            .expect("read public parity manifest");
    let manifest = public_manifest
        .parse::<toml::Value>()
        .expect("public parity manifest must parse as TOML");
    let slides = manifest
        .get("slide")
        .and_then(toml::Value::as_array)
        .expect("public parity manifest must define slide entries");

    let mut expected_failure_count = 0usize;
    for slide in slides.iter().filter_map(toml::Value::as_table) {
        let alias = slide
            .get("alias")
            .and_then(toml::Value::as_str)
            .expect("public parity entry must define an alias");
        let expected_failures = slide
            .get("expected_failures")
            .and_then(toml::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(toml::Value::as_str)
            .collect::<Vec<_>>();
        if expected_failures.is_empty() {
            continue;
        }
        expected_failure_count += expected_failures.len();
        let rationale = slide
            .get("expected_failure_rationale")
            .and_then(toml::Value::as_table)
            .unwrap_or_else(|| {
                panic!(
                    "public parity expected failures for `{alias}` must have expected_failure_rationale entries"
                )
            });
        for expected_failure in expected_failures {
            let reason = rationale
                .get(expected_failure)
                .and_then(toml::Value::as_str)
                .unwrap_or("");
            assert!(
                reason.len() >= 96,
                "expected parity failure `{alias}:{expected_failure}` must have a concrete rationale"
            );
        }
    }

    assert!(
        expected_failure_count > 0,
        "integrity test should cover the public parity expected-failure policy"
    );
    assert!(
        public_manifest.contains("expected_failure_rationale"),
        "public parity manifest must keep expected failures reviewable"
    );
}

#[test]
fn openslide_shim_has_public_usage_docs() {
    let readme = fs::read_to_string(crate_root().join("wsi-rs-openslide-shim/README.md"))
        .expect("read OpenSlide shim README");
    for required in [
        "cargo build -p wsi-rs-openslide-shim --release",
        "libopenslide.1.dylib",
        "libopenslide.so.1",
        "private prefix",
        "Implemented ABI surface",
        "read_region",
    ] {
        assert!(
            readme.contains(required),
            "OpenSlide shim README must document `{required}`"
        );
    }
}

#[test]
fn environment_knobs_use_wsi_rs_prefix() {
    for relative in [
        "src/core/cache.rs",
        "src/decode/jp2k.rs",
        "src/formats/tiff_family/pixel_access",
        "tests/dicom_parity.rs",
        "tests/fixtures/parity_corpus.public.toml",
        "tests/openslide_compare.rs",
        "tests/openslide_parity.rs",
        "tests/openslide_test_support.rs",
        "tests/real_wsi_behavior.rs",
        "tests/j2k_parity.rs",
        "tests/support/corpus.rs",
        "tests/support/openslide_shim.rs",
    ] {
        let source = read_repo_text(relative);
        let retired_prefix = ["ZIG", "GURAT_"].concat();
        assert!(
            !source.contains(&retired_prefix),
            "{relative} must use WSI_RS_ environment variable names"
        );
    }
}

#[test]
fn registry_does_not_use_type_erased_region_cache_tokens() {
    let registry = read_repo_text("src/core/registry");

    assert!(
        !registry.contains("RegionCacheToken"),
        "region cache plumbing must use SlideReadContext, not RegionCacheToken"
    );
    assert!(
        !registry.contains("std::any::Any"),
        "SlideReader cache plumbing must not use Any/downcast"
    );
}

#[test]
fn format_registry_does_not_silently_rewrite_svcache_paths() {
    let registry = read_repo_text("src/core/registry");

    assert!(
        !registry.contains("resolve_svcache"),
        "FormatRegistry must not silently resolve .svcache paths"
    );
    assert!(
        !registry.contains("resolve_open_path("),
        "implicit .svcache resolution belongs behind SlideOpenOptions"
    );
}

#[test]
fn public_wsi_api_does_not_reexport_j2k_backend_request() {
    let lib = fs::read_to_string(crate_root().join("src/lib.rs")).expect("read lib");

    assert!(
        !lib.contains("pub use j2k_core::BackendRequest"),
        "wsi_rs public output policy must use OutputBackendRequest"
    );
}

#[test]
fn api_stability_tooling_is_wired() {
    let ci = fs::read_to_string(crate_root().join(".github/workflows/ci.yml")).expect("read CI");
    let xtask_mod = fs::read_to_string(crate_root().join("xtask/src/commands/mod.rs"))
        .expect("read xtask command router");
    let xtask_checks = fs::read_to_string(crate_root().join("xtask/src/commands/checks.rs"))
        .expect("read xtask checks");

    for required in [
        "cargo-public-api",
        "cargo-semver-checks",
        "cargo xtask api-check",
    ] {
        assert!(
            ci.contains(required),
            "CI must install and run API stability tooling; missing `{required}`"
        );
    }
    assert!(
        ci.contains("api-stability:\n    strategy:")
            && ci.contains("os: [ubuntu-latest, macos-latest]"),
        "CI API stability job must run on macOS as well as Linux so feature-gated Metal API snapshots are checked"
    );

    assert!(
        xtask_mod.contains("\"api-check\" => checks::api_check()"),
        "cargo xtask must route the api-check task"
    );
    assert!(
        xtask_mod.contains("api-check    run public API and semver stability checks"),
        "cargo xtask help must advertise the api-check task"
    );
    for required in [
        "public-api",
        "semver-checks",
        "check-release",
        "WSI_RS_SEMVER_BASELINE_ROOT",
    ] {
        assert!(
            xtask_checks.contains(required),
            "xtask api-check must invoke `{required}`"
        );
    }
    assert!(
        xtask_checks.contains("api/wsi-rs-public-api.txt"),
        "xtask api-check must compare against the checked-in public API snapshot"
    );
    for required in [
        "api/wsi-rs-public-api-metal.txt",
        "api/wsi-rs-public-api-cuda.txt",
        "\"--features\"",
        "\"metal\"",
        "\"cuda\"",
        "cfg!(target_os = \"macos\")",
        "run_semver_check(&[])?;",
        "run_semver_check(&[\"--features\", \"cuda\"])",
        "run_semver_check(&[\"--features\", \"metal\"])",
    ] {
        assert!(
            xtask_checks.contains(required),
            "xtask api-check must cover optional public API snapshots; missing `{required}`"
        );
    }
    assert!(
        crate_root().join("api/wsi-rs-public-api.txt").is_file(),
        "public API snapshot must be checked in for reviewable API diffs"
    );
    assert!(
        crate_root()
            .join("api/wsi-rs-public-api-metal.txt")
            .is_file(),
        "Metal feature public API snapshot must be checked in for reviewable optional-surface API diffs"
    );
    assert!(
        crate_root()
            .join("api/wsi-rs-public-api-cuda.txt")
            .is_file(),
        "CUDA feature public API snapshot must be checked in for reviewable optional-surface API diffs"
    );
    let snapshot = fs::read_to_string(crate_root().join("api/wsi-rs-public-api.txt"))
        .expect("read public API snapshot");
    let cuda_snapshot = fs::read_to_string(crate_root().join("api/wsi-rs-public-api-cuda.txt"))
        .expect("read CUDA public API snapshot");
    let metal_snapshot = fs::read_to_string(crate_root().join("api/wsi-rs-public-api-metal.txt"))
        .expect("read Metal public API snapshot");
    assert!(
        !snapshot.contains("impl core::marker::"),
        "public API snapshot should omit auto-trait noise"
    );
    assert!(
        !snapshot.contains("impl core::clone::Clone"),
        "public API snapshot should omit auto-derived impl noise"
    );
    assert!(
        metal_snapshot.contains("MetalDeviceTile")
            && metal_snapshot.contains("MetalDeviceStorage")
            && metal_snapshot
                .contains("#[non_exhaustive] pub struct wsi_rs::output::metal::MetalDeviceTile")
            && metal_snapshot
                .contains("#[non_exhaustive] pub enum wsi_rs::output::metal::MetalDeviceStorage"),
        "Metal public API snapshot must capture future-extensible Metal output types"
    );
    assert!(
        cuda_snapshot.contains("CudaDeviceTile")
            && cuda_snapshot.contains("CudaDeviceStorage")
            && cuda_snapshot
                .contains("#[non_exhaustive] pub struct wsi_rs::output::cuda::CudaDeviceTile")
            && cuda_snapshot
                .contains("#[non_exhaustive] pub enum wsi_rs::output::cuda::CudaDeviceStorage")
            && cuda_snapshot.contains("pub wsi_rs::OutputBackendRequest::Cuda")
            && !cuda_snapshot.contains("pub wsi_rs::OutputBackendRequest::Metal"),
        "CUDA public API snapshot must capture future-extensible CUDA output types without mixing in Metal"
    );
}

#[test]
fn release_validation_runs_doctests() {
    let xtask_mod = fs::read_to_string(crate_root().join("xtask/src/commands/mod.rs"))
        .expect("read xtask command router");
    let xtask_checks = fs::read_to_string(crate_root().join("xtask/src/commands/checks.rs"))
        .expect("read xtask checks");

    assert!(
        xtask_mod.contains("\"doc-test\" => checks::doc_test()"),
        "cargo xtask must route the doc-test task"
    );
    assert!(
        xtask_mod.contains("doc-test     compile rustdoc examples with doctest"),
        "cargo xtask help must advertise the doc-test task"
    );
    assert!(
        xtask_checks.contains("pub(super) fn doc_test()"),
        "xtask checks must expose a doctest task"
    );
    assert!(
        xtask_checks.contains("\"test\", \"--doc\""),
        "doc-test must invoke cargo test --doc"
    );
    assert!(
        xtask_checks.contains("doc_test()?;"),
        "cargo xtask validate must include doctests before release claims"
    );
}

#[test]
fn package_gate_runs_publish_dry_run() {
    let xtask_checks = fs::read_to_string(crate_root().join("xtask/src/commands/checks.rs"))
        .expect("read xtask checks");

    assert!(
        xtask_checks.contains("ensure_clean_worktree()?;"),
        "cargo xtask package must refuse dirty release packaging"
    );
    assert!(
        xtask_checks.contains("\"package\""),
        "cargo xtask package must run cargo package"
    );
    assert!(
        xtask_checks.contains("\"package\", \"--locked\""),
        "cargo xtask package must verify against the checked-in Cargo.lock"
    );
    assert!(
        xtask_checks.contains("\"publish\", \"--dry-run\", \"--locked\""),
        "cargo xtask package must run cargo publish --dry-run before release"
    );
}

#[test]
fn release_candidate_preflight_is_wired() {
    let xtask_mod = fs::read_to_string(crate_root().join("xtask/src/commands/mod.rs"))
        .expect("read xtask command router");
    let xtask_checks = fs::read_to_string(crate_root().join("xtask/src/commands/checks.rs"))
        .expect("read xtask checks");
    let readme = fs::read_to_string(crate_root().join("README.md")).expect("read README");

    assert!(
        xtask_mod.contains("\"rc-preflight\" => checks::rc_preflight()"),
        "cargo xtask must route the rc-preflight task"
    );
    assert!(
        xtask_mod.contains("rc-preflight run local release-candidate preflight gates"),
        "cargo xtask help must advertise the rc-preflight task"
    );
    assert!(
        xtask_checks.contains("pub(super) fn rc_preflight()"),
        "xtask checks must expose rc_preflight"
    );
    for required in [
        "api_check()?;",
        "deps()?;",
        "fuzz_check()?;",
        "feature_check()?;",
        "validate()?;",
        "package()",
    ] {
        assert!(
            xtask_checks.contains(required),
            "rc_preflight must include `{required}`"
        );
    }
    assert!(
        readme.contains("cargo xtask rc-preflight"),
        "README development docs must advertise the local RC preflight command"
    );
    assert!(
        readme.contains("feature-combination checks"),
        "README must state that rc-preflight includes feature-combination checks"
    );
    assert!(
        !xtask_checks.contains("openslide-bench,metal,parity-metal,cuda"),
        "feature-check must compile the public CUDA feature until CUDA support is removed or moved out of the public feature surface"
    );
    assert!(
        readme.contains("`cargo xtask validate` runs the default local gate."),
        "README development docs must describe the validate gate without stale benchmark detail"
    );
}

#[test]
fn release_candidate_preflight_workflow_runs_exact_gate() {
    let workflow_path = crate_root().join(".github/workflows/rc-preflight.yml");
    assert!(
        workflow_path.is_file(),
        "repository must expose an on-demand RC preflight workflow"
    );
    let workflow = fs::read_to_string(workflow_path).expect("read RC preflight workflow");

    for required in [
        "workflow_dispatch:",
        "fetch-depth: 0",
        "dtolnay/rust-toolchain@nightly",
        "dtolnay/rust-toolchain@stable",
        "components: rustfmt,clippy",
        "taiki-e/install-action@nextest",
        "cargo-hack,cargo-public-api,cargo-semver-checks,cargo-fuzz,cargo-deny,cargo-machete",
        "os: [ubuntu-latest, macos-latest]",
        "cargo xtask rc-preflight",
    ] {
        assert!(
            workflow.contains(required),
            "RC preflight workflow must contain `{required}`"
        );
    }
}

#[test]
fn internal_release_markdown_is_not_tracked() {
    let readme = fs::read_to_string(crate_root().join("README.md")).expect("read README");
    let lib = fs::read_to_string(crate_root().join("src/lib.rs")).expect("read lib docs");

    for removed in [
        "architecture.md",
        "CODE_OF_CONDUCT.md",
        "CONTRIBUTING.md",
        "docs/architecture.md",
        "docs/rc-evidence-template.md",
        "docs/release-gates.md",
        "docs/stability.md",
    ] {
        assert!(
            !crate_root().join(removed).exists(),
            "internal markdown bloat should stay removed: {removed}"
        );
        assert!(
            !readme.contains(removed) && !lib.contains(removed),
            "public docs must not link removed internal markdown `{removed}`"
        );
    }
}

#[test]
fn fuzzing_tooling_is_wired() {
    let manifest = fs::read_to_string(crate_root().join("Cargo.toml")).expect("read manifest");
    let ci = fs::read_to_string(crate_root().join(".github/workflows/ci.yml")).expect("read CI");
    let xtask_mod = fs::read_to_string(crate_root().join("xtask/src/commands/mod.rs"))
        .expect("read xtask command router");
    let wsi_fuzz = fs::read_to_string(crate_root().join("fuzz/fuzz_targets/open_wsi_bytes.rs"))
        .expect("read WSI fuzz target");

    assert!(
        manifest.contains("\"fuzz/**\""),
        "fuzz harness should stay out of the published library crate"
    );
    assert!(
        crate_root().join("fuzz/Cargo.toml").is_file(),
        "cargo-fuzz manifest must exist"
    );
    let fuzz_gitignore =
        fs::read_to_string(crate_root().join("fuzz/.gitignore")).expect("read fuzz .gitignore");
    for generated_path in ["artifacts/", "corpus/", "coverage/"] {
        assert!(
            fuzz_gitignore.contains(generated_path),
            "fuzz .gitignore must ignore cargo-fuzz generated `{generated_path}`"
        );
    }

    for target in [
        "open_wsi_bytes",
        "open_jp2k_codestream_bytes",
        "open_svcache_bytes",
    ] {
        assert!(
            crate_root()
                .join(format!("fuzz/fuzz_targets/{target}.rs"))
                .is_file(),
            "fuzz target `{target}` must exist"
        );
    }

    assert!(
        xtask_mod.contains("\"fuzz-check\" => checks::fuzz_check()"),
        "cargo xtask must route the fuzz-check task"
    );
    assert!(
        xtask_mod.contains("fuzz-check   type-check cargo-fuzz targets"),
        "cargo xtask help must advertise the fuzz-check task"
    );
    assert!(
        ci.contains("cargo xtask fuzz-check"),
        "CI must type-check fuzz targets"
    );

    for extension in [
        "svs", "ndpi", "scn", "tif", "tiff", "bif", "mrxs", "vms", "vmu", "vsi", "dcm", "czi",
        "zvi",
    ] {
        assert!(
            wsi_fuzz.contains(&format!("\"{extension}\"")),
            "open_wsi_bytes fuzz target must exercise `{extension}` inputs"
        );
    }
    assert!(
        wsi_fuzz.contains("split_first"),
        "open_wsi_bytes fuzz target must use fuzz input to select a vendor extension, not hard-code one path"
    );
}

#[test]
fn public_api_snapshot_uses_stable_user_facing_paths() {
    for snapshot_path in [
        "api/wsi-rs-public-api.txt",
        "api/wsi-rs-public-api-cuda.txt",
        "api/wsi-rs-public-api-metal.txt",
    ] {
        let snapshot = fs::read_to_string(crate_root().join(snapshot_path))
            .unwrap_or_else(|err| panic!("read {snapshot_path}: {err}"));

        for forbidden in [
            "wsi_rs::core::",
            "wsi_rs::formats::",
            "CpuTile::new_for_test",
            "CpuTile::solid_red",
            "SlideReadContext<'a>::new",
        ] {
            assert!(
                !snapshot.contains(forbidden),
                "{snapshot_path} must not expose internal/testing detail `{forbidden}`"
            );
        }
    }

    let default_snapshot = fs::read_to_string(crate_root().join("api/wsi-rs-public-api.txt"))
        .expect("read default public API snapshot");
    assert!(
        !default_snapshot.contains("j2k_core::"),
        "default public API snapshot must not expose J2k output-policy internals"
    );
    assert!(
        default_snapshot.contains(
            "pub fn wsi_rs::RawCompressedTile::builder(wsi_rs::Compression) -> wsi_rs::RawCompressedTileBuilder"
        ),
        "Raw compressed tile payloads must expose a named-field builder before the release-candidate API freeze"
    );
    assert!(
        default_snapshot.contains(
            "pub fn wsi_rs::RegionRequest::builder(impl core::convert::Into<wsi_rs::SceneId>, impl core::convert::Into<wsi_rs::SeriesId>, impl core::convert::Into<wsi_rs::LevelIdx>) -> wsi_rs::RegionRequestBuilder"
        ),
        "RegionRequest builders must accept the same typed or plain public indices as tile requests in the public API snapshot"
    );
    assert!(
        !default_snapshot.contains(
            "pub fn wsi_rs::RegionRequest::builder(wsi_rs::SceneId, wsi_rs::SeriesId, wsi_rs::LevelIdx)"
        ),
        "RegionRequest builders must not force manual index newtype construction in the public API snapshot"
    );
    assert!(
        default_snapshot.contains(
            "pub fn wsi_rs::SvcacheTileSelection::new(impl core::convert::Into<wsi_rs::SceneId>, impl core::convert::Into<wsi_rs::SeriesId>, impl core::convert::Into<wsi_rs::LevelIdx>, i64, i64) -> Self"
        ),
        "SvcacheTileSelection must accept the same typed or plain public indices as read requests in the public API snapshot"
    );
    assert!(
        !default_snapshot.contains(
            "pub fn wsi_rs::SvcacheTileSelection::new(wsi_rs::SceneId, wsi_rs::SeriesId, wsi_rs::LevelIdx"
        ),
        "SvcacheTileSelection must not force manual index newtype construction in the public API snapshot"
    );
    assert!(
        default_snapshot.contains(
            "pub fn wsi_rs::Slide::level_source_kind(&self, impl core::convert::Into<wsi_rs::SceneId>, impl core::convert::Into<wsi_rs::SeriesId>, impl core::convert::Into<wsi_rs::LevelIdx>) -> core::result::Result<wsi_rs::LevelSourceKind, wsi_rs::error::WsiError>"
        ),
        "Slide::level_source_kind must accept typed or plain public indices in the public API snapshot"
    );
    assert!(
        default_snapshot.contains(
            "pub fn wsi_rs::SlideReader::level_source_kind(&self, wsi_rs::SceneId, wsi_rs::SeriesId, wsi_rs::LevelIdx)"
        ),
        "SlideReader::level_source_kind must keep object-safe typed index parameters for backend implementations"
    );
    assert!(
        !default_snapshot.contains(
            "pub fn wsi_rs::Slide::level_source_kind(&self, wsi_rs::SceneId, wsi_rs::SeriesId, wsi_rs::LevelIdx)"
        ),
        "Slide::level_source_kind must not force manual index newtype construction in the public API snapshot"
    );
    assert!(
        !default_snapshot.contains("pub fn wsi_rs::RawCompressedTile::new("),
        "Raw compressed tile payloads must not advertise a long positional public constructor before the release-candidate API freeze"
    );

    let metal_snapshot = fs::read_to_string(crate_root().join("api/wsi-rs-public-api-metal.txt"))
        .expect("read Metal public API snapshot");
    assert!(
        metal_snapshot
            .contains("pub wsi_rs::output::metal::MetalDeviceTile::format: wsi_rs::PixelFormat"),
        "Metal device tiles must expose a wsi-rs-owned pixel format type"
    );
    assert!(
        !metal_snapshot.contains(
            "pub wsi_rs::output::metal::MetalDeviceTile::format: j2k_core::"
        ),
        "Metal device tile payloads must not expose J2k pixel format types as the public field contract"
    );
    assert!(
        metal_snapshot.contains(
            "pub fn wsi_rs::output::metal::MetalBackendSessions::new(metal::device::Device) -> Self"
        ),
        "Metal backend setup should accept a Metal device directly instead of requiring callers to construct codec adapter sessions"
    );
    for forbidden in [
        "j2k_jpeg_metal::MetalBackendSession",
        "j2k_metal::MetalBackendSession",
        "MetalBackendSessions::with_private_jpeg_decode",
    ] {
        assert!(
            !metal_snapshot.contains(forbidden),
            "Metal backend setup must not expose internal codec/session tuning `{forbidden}` in the public API"
        );
    }
}

#[test]
fn default_output_api_keeps_metal_constructors_feature_gated() {
    let snapshot = fs::read_to_string(crate_root().join("api/wsi-rs-public-api.txt"))
        .expect("read public API snapshot");
    let metal_snapshot = fs::read_to_string(crate_root().join("api/wsi-rs-public-api-metal.txt"))
        .expect("read Metal public API snapshot");
    let output = read_repo_text("src/core/types/output.rs");

    assert!(
        snapshot.contains("TileOutputPreference::require_device_auto() -> Self"),
        "default output API must expose a generic require-device constructor"
    );
    assert!(
        !snapshot.contains("TileOutputPreference::require_metal"),
        "default public API must not advertise Metal-specific constructors"
    );
    for forbidden in [
        "pub wsi_rs::OutputBackendRequest::Metal",
        "pub wsi_rs::OutputBackendRequest::Cuda",
    ] {
        assert!(
            !snapshot.contains(forbidden),
            "default public API must not expose feature-specific output backend variant `{forbidden}`"
        );
    }
    assert!(
        metal_snapshot.contains("TileOutputPreference::require_metal() -> Self"),
        "Metal feature snapshot must retain the Metal-specific require constructor"
    );
    assert!(
        metal_snapshot.contains("pub wsi_rs::OutputBackendRequest::Metal"),
        "Metal feature snapshot must expose the Metal backend request variant"
    );
    assert!(
        !metal_snapshot.contains("pub wsi_rs::OutputBackendRequest::Cuda"),
        "Metal feature snapshot must not expose the experimental CUDA backend variant"
    );

    let require_metal = output
        .find("pub fn require_metal() -> Self")
        .expect("TileOutputPreference::require_metal must exist");
    let previous_function = output[..require_metal].rfind("pub fn ").unwrap_or_default();
    let metal_cfg = output[..require_metal]
        .rfind("#[cfg(feature = \"metal\")]")
        .expect("require_metal must be feature-gated");
    assert!(
        metal_cfg > previous_function,
        "require_metal must have its own #[cfg(feature = \"metal\")] gate"
    );

    for (variant, feature) in [("Metal", "metal"), ("Cuda", "cuda")] {
        let variant = output
            .find(&format!("{variant},"))
            .unwrap_or_else(|| panic!("OutputBackendRequest::{variant} must exist"));
        let previous_variant = output[..variant].rfind(',').unwrap_or_default();
        let feature_cfg = output[..variant]
            .rfind(&format!("#[cfg(feature = \"{feature}\")]"))
            .unwrap_or_else(|| panic!("OutputBackendRequest::{variant} must be feature-gated"));
        assert!(
            feature_cfg > previous_variant,
            "OutputBackendRequest::{variant} must have its own #[cfg(feature = \"{feature}\")] gate"
        );
    }
}

#[test]
fn public_api_extensible_enums_are_non_exhaustive() {
    for (relative, enum_name) in [
        ("src/error.rs", "WsiError"),
        ("src/core/registry/traits.rs", "ProbeConfidence"),
        ("src/core/decode_runtime.rs", "DecodeRoute"),
        ("src/core/types/geometry.rs", "TileLayout"),
        ("src/core/types/model.rs", "LevelSourceKind"),
        ("src/core/types/model.rs", "Compression"),
        ("src/core/types/model.rs", "TileCodecKind"),
        (
            "src/core/types/model.rs",
            "EncodedTilePhotometricInterpretation",
        ),
        ("src/core/types/output.rs", "OutputBackendRequest"),
        ("src/core/types/output.rs", "TileOutputPreference"),
        ("src/core/types/output.rs", "TilePixels"),
        ("src/core/types/output.rs", "DeviceTile"),
        ("src/core/types/pixels.rs", "SampleType"),
        ("src/core/types/pixels.rs", "PixelFormat"),
        ("src/core/types/pixels.rs", "CpuTileData"),
        ("src/core/types/pixels.rs", "ColorSpace"),
        ("src/core/types/pixels.rs", "CpuTileLayout"),
        ("src/formats/svcache.rs", "SvcachePolicy"),
        ("src/core/types/requests.rs", "RequestBuildError"),
    ] {
        assert_non_exhaustive_enum(relative, enum_name);
    }
}

#[test]
fn public_request_structs_are_non_exhaustive() {
    for (relative, struct_name) in [
        ("src/core/types/requests.rs", "RegionRequest"),
        ("src/core/types/requests.rs", "TileRequest"),
        ("src/core/types/requests.rs", "TileViewRequest"),
    ] {
        assert_non_exhaustive_struct(relative, struct_name);
    }
}

#[test]
fn public_tile_request_indices_use_stable_newtypes() {
    let requests = read_repo_text("src/core/types/requests.rs");
    let model = read_repo_text("src/core/types/model.rs");

    for required in [
        "pub scene: SceneId",
        "pub series: SeriesId",
        "pub level: LevelIdx",
        "pub plane: PlaneIdx",
        "pub fn new(\n        scene: impl Into<SceneId>,",
        "pub fn builder(\n        scene: impl Into<SceneId>,",
        "pub fn with_plane(mut self, plane: impl Into<PlaneIdx>) -> Self",
    ] {
        assert!(
            requests.contains(required),
            "tile and display tile requests must use stable public index newtypes; missing `{required}`"
        );
    }

    for forbidden in [
        "pub scene: usize",
        "pub series: usize",
        "pub level: u32",
        "pub plane: PlaneSelection",
        "pub fn new(scene: usize, series: usize, level: u32",
        "pub fn builder(scene: usize, series: usize, level: u32",
        "impl From<u8> for SceneId",
        "impl From<u8> for SeriesId",
        "impl From<u8> for LevelIdx",
    ] {
        assert!(
            !requests.contains(forbidden) && !model.contains(forbidden),
            "tile and display tile requests must not expose primitive index field/API `{forbidden}`"
        );
    }
}

#[test]
fn public_level_source_kind_uses_stable_newtypes() {
    let slide = read_repo_text("src/core/registry/slide.rs");
    let traits = read_repo_text("src/core/registry/traits.rs");

    for required in [
        "pub fn level_source_kind(",
        "scene: impl Into<SceneId>",
        "series: impl Into<SeriesId>",
        "level: impl Into<LevelIdx>",
        ".level_source_kind(scene.into(), series.into(), level.into())",
    ] {
        assert!(
            slide.contains(required),
            "Slide::level_source_kind must accept the same typed or plain public indices as read requests; missing `{required}`"
        );
    }

    for required in [
        "fn level_source_kind(",
        "scene: SceneId",
        "series: SeriesId",
        "level: LevelIdx",
    ] {
        assert!(
            traits.contains(required),
            "SlideReader::level_source_kind must keep concrete typed newtypes for object-safe backend implementations; missing `{required}`"
        );
    }

    for source in [&slide, &traits] {
        assert!(
            !source.contains("scene: usize,\n        series: usize,\n        level: u32"),
            "public level_source_kind APIs must not expose primitive scene/series/level indices"
        );
    }
}

#[test]
fn public_region_request_indices_match_tile_request_ergonomics() {
    let requests = read_repo_text("src/core/types/requests.rs");
    let region_impl = requests
        .split("impl RegionRequest {")
        .nth(1)
        .and_then(|tail| tail.split("/// Builder for [`RegionRequest`].").next())
        .expect("RegionRequest impl block must be present");

    for required in [
        "pub fn new(\n        scene: impl Into<SceneId>,",
        "series: impl Into<SeriesId>,",
        "level: impl Into<LevelIdx>,",
        "pub fn builder(\n        scene: impl Into<SceneId>,",
    ] {
        assert!(
            region_impl.contains(required),
            "RegionRequest constructors must accept the same typed or plain public indices as tile requests; missing `{required}`"
        );
    }
}

#[test]
fn public_request_plane_setters_accept_plain_plane_selection() {
    let requests = read_repo_text("src/core/types/requests.rs");
    let model = read_repo_text("src/core/types/model.rs");

    assert!(
        model.contains("impl From<PlaneSelection> for PlaneIdx"),
        "PlaneSelection must convert into PlaneIdx for ergonomic public request APIs"
    );

    for required in [
        "pub fn with_plane(mut self, plane: impl Into<PlaneIdx>) -> Self",
        "pub fn plane(mut self, plane: impl Into<PlaneIdx>) -> Self",
    ] {
        assert!(
            requests.contains(required),
            "request builders must accept PlaneSelection through `{required}`"
        );
    }
}

#[test]
fn public_index_newtypes_have_constructor_api_before_layout_freeze() {
    for (relative, struct_name) in [
        ("src/core/types/model.rs", "DatasetId"),
        ("src/core/types/model.rs", "SceneId"),
        ("src/core/types/model.rs", "SeriesId"),
        ("src/core/types/model.rs", "LevelIdx"),
        ("src/core/types/model.rs", "PlaneIdx"),
    ] {
        assert_non_exhaustive_struct(relative, struct_name);
    }

    let model = read_repo_text("src/core/types/model.rs");
    for required in [
        "pub struct DatasetId(pub(crate) u128)",
        "impl DatasetId",
        "pub const fn new(value: u128) -> Self",
        "pub const fn get(self) -> u128",
        "pub struct SceneId(pub(crate) usize)",
        "impl SceneId",
        "pub const fn new(index: usize) -> Self",
        "pub const fn get(self) -> usize",
        "pub struct SeriesId(pub(crate) usize)",
        "impl SeriesId",
        "pub struct LevelIdx(pub(crate) u32)",
        "impl LevelIdx",
        "pub const fn new(index: u32) -> Self",
        "pub const fn get(self) -> u32",
        "pub struct PlaneIdx(pub(crate) PlaneSelection)",
        "impl PlaneIdx",
        "pub const fn new(plane: PlaneSelection) -> Self",
        "pub const fn get(self) -> PlaneSelection",
    ] {
        assert!(
            model.contains(required),
            "public index newtypes must expose constructor/accessor API `{required}` before hiding tuple construction"
        );
    }
}

#[test]
fn public_probe_result_has_future_extensible_constructor_api() {
    assert_non_exhaustive_struct("src/core/registry/traits.rs", "ProbeResult");

    let traits = read_repo_text("src/core/registry/traits.rs");
    for required in [
        "impl ProbeResult",
        "pub fn detected(",
        "pub fn not_detected(",
    ] {
        assert!(
            traits.contains(required),
            "ProbeResult must expose `{required}` before hiding literal construction"
        );
    }
}

#[test]
fn public_configuration_and_diagnostic_structs_are_non_exhaustive() {
    for (relative, struct_name) in [
        ("src/core/cache.rs", "CacheConfig"),
        ("src/core/decode_runtime.rs", "DecodeExecutionOptions"),
        ("src/core/decode_runtime.rs", "DecodeRouteDecision"),
        ("src/core/registry/open_options.rs", "SlideOpenOptions"),
        ("src/core/types/output.rs", "DeviceOutputContext"),
        ("src/core/types/pixels.rs", "DisplayWindow"),
    ] {
        assert_non_exhaustive_struct(relative, struct_name);
    }
}

#[test]
fn public_display_window_has_constructor_api_before_non_exhaustive_freeze() {
    assert_non_exhaustive_struct("src/core/types/pixels.rs", "DisplayWindow");

    let pixels = read_repo_text("src/core/types/pixels.rs");
    assert!(
        pixels.contains("impl DisplayWindow")
            && pixels.contains("pub fn new(")
            && pixels.contains("window range must be positive"),
        "DisplayWindow must expose a validating constructor before hiding literal construction"
    );
    for forbidden in ["pub min: f64", "pub max: f64"] {
        assert!(
            !pixels.contains(forbidden),
            "DisplayWindow bounds must stay private so downstream code cannot bypass constructor validation with `{forbidden}`"
        );
    }

    for snapshot_path in [
        "api/wsi-rs-public-api.txt",
        "api/wsi-rs-public-api-cuda.txt",
        "api/wsi-rs-public-api-metal.txt",
    ] {
        let snapshot = fs::read_to_string(crate_root().join(snapshot_path))
            .unwrap_or_else(|err| panic!("read {snapshot_path}: {err}"));
        for required in [
            "pub fn wsi_rs::DisplayWindow::min(&self) -> f64",
            "pub fn wsi_rs::DisplayWindow::max(&self) -> f64",
        ] {
            assert!(
                snapshot.contains(required),
                "{snapshot_path} must expose read accessors for private DisplayWindow bounds; missing `{required}`"
            );
        }
        for forbidden in [
            "pub wsi_rs::DisplayWindow::min: f64",
            "pub wsi_rs::DisplayWindow::max: f64",
        ] {
            assert!(
                !snapshot.contains(forbidden),
                "{snapshot_path} must not expose public mutable DisplayWindow bounds; found `{forbidden}`"
            );
        }
    }
}

#[test]
fn public_metadata_and_pixel_structs_are_non_exhaustive() {
    for (relative, struct_name) in [
        ("src/core/types/model.rs", "Dataset"),
        ("src/core/types/model.rs", "IccProfileKey"),
        ("src/core/types/model.rs", "Scene"),
        ("src/core/types/model.rs", "Series"),
        ("src/core/types/model.rs", "AxesShape"),
        ("src/core/types/model.rs", "Level"),
        ("src/core/types/model.rs", "ChannelInfo"),
        ("src/core/types/model.rs", "AssociatedImage"),
        ("src/core/types/model.rs", "RawCompressedTile"),
        ("src/core/types/pixels.rs", "CpuTile"),
        ("src/properties.rs", "Properties"),
    ] {
        assert_non_exhaustive_struct(relative, struct_name);
    }
}

#[test]
fn public_cpu_tile_has_validated_read_only_api_before_non_exhaustive_freeze() {
    assert_non_exhaustive_struct("src/core/types/pixels.rs", "CpuTile");

    let pixels = read_repo_text("src/core/types/pixels.rs");
    for required in [
        "pub fn new(",
        "CpuTile invariant violated",
        "pub fn width(&self) -> u32",
        "pub fn height(&self) -> u32",
        "pub fn channels(&self) -> u16",
        "pub fn color_space(&self) -> &ColorSpace",
        "pub fn layout(&self) -> CpuTileLayout",
        "pub fn data(&self) -> &CpuTileData",
    ] {
        assert!(
            pixels.contains(required),
            "CpuTile must expose a validating constructor and read-only accessors before hiding fields; missing `{required}`"
        );
    }
    for forbidden in [
        "pub width: u32",
        "pub height: u32",
        "pub channels: u16",
        "pub color_space: ColorSpace",
        "pub layout: CpuTileLayout",
        "pub data: CpuTileData",
    ] {
        assert!(
            !pixels.contains(forbidden),
            "CpuTile fields must stay private to preserve constructor validation; found `{forbidden}`"
        );
    }

    for snapshot_path in [
        "api/wsi-rs-public-api.txt",
        "api/wsi-rs-public-api-cuda.txt",
        "api/wsi-rs-public-api-metal.txt",
    ] {
        let snapshot = fs::read_to_string(crate_root().join(snapshot_path))
            .unwrap_or_else(|err| panic!("read {snapshot_path}: {err}"));
        for required in [
            "pub fn wsi_rs::CpuTile::channels(&self) -> u16",
            "pub fn wsi_rs::CpuTile::color_space(&self) -> &wsi_rs::ColorSpace",
            "pub fn wsi_rs::CpuTile::data(&self) -> &wsi_rs::CpuTileData",
            "pub fn wsi_rs::CpuTile::layout(&self) -> wsi_rs::CpuTileLayout",
        ] {
            assert!(
                snapshot.contains(required),
                "{snapshot_path} must expose read-only CpuTile accessors; missing `{required}`"
            );
        }
        for forbidden in [
            "pub wsi_rs::CpuTile::channels:",
            "pub wsi_rs::CpuTile::color_space:",
            "pub wsi_rs::CpuTile::data:",
            "pub wsi_rs::CpuTile::height:",
            "pub wsi_rs::CpuTile::layout:",
            "pub wsi_rs::CpuTile::width:",
        ] {
            assert!(
                !snapshot.contains(forbidden),
                "{snapshot_path} must not expose mutable CpuTile fields; found `{forbidden}`"
            );
        }
    }
}

#[test]
fn public_raw_compressed_tile_has_validated_read_only_api_before_non_exhaustive_freeze() {
    assert_non_exhaustive_struct("src/core/types/model.rs", "RawCompressedTile");

    let model = read_repo_text("src/core/types/model.rs");
    for required in [
        "pub fn builder(compression: Compression) -> RawCompressedTileBuilder",
        "pub fn compression(&self) -> Compression",
        "pub fn width(&self) -> u32",
        "pub fn height(&self) -> u32",
        "pub fn bits_allocated(&self) -> u16",
        "pub fn samples_per_pixel(&self) -> u16",
        "pub fn photometric_interpretation(&self) -> EncodedTilePhotometricInterpretation",
        "pub fn data(&self) -> &[u8]",
        "pub fn into_data(self) -> Vec<u8>",
        "RawCompressedTileBuildError::InvalidDimensions",
        "RawCompressedTileBuildError::InvalidBitsAllocated",
        "RawCompressedTileBuildError::InvalidSamplesPerPixel",
        "RawCompressedTileBuildError::EmptyData",
    ] {
        assert!(
            model.contains(required),
            "RawCompressedTile must expose builder validation and read-only accessors before hiding fields; missing `{required}`"
        );
    }
    for forbidden in [
        "pub(crate) compression: Compression",
        "pub compression: Compression",
        "pub(crate) width: u32",
        "pub width: u32",
        "pub(crate) height: u32",
        "pub height: u32",
        "pub(crate) bits_allocated: u16",
        "pub bits_allocated: u16",
        "pub(crate) samples_per_pixel: u16",
        "pub samples_per_pixel: u16",
        "pub(crate) photometric_interpretation: EncodedTilePhotometricInterpretation",
        "pub photometric_interpretation: EncodedTilePhotometricInterpretation",
        "pub(crate) data: Vec<u8>",
        "pub data: Vec<u8>",
    ] {
        assert!(
            !model.contains(forbidden),
            "RawCompressedTile fields must stay private to preserve builder validation; found `{forbidden}`"
        );
    }

    for snapshot_path in [
        "api/wsi-rs-public-api.txt",
        "api/wsi-rs-public-api-cuda.txt",
        "api/wsi-rs-public-api-metal.txt",
    ] {
        let snapshot = fs::read_to_string(crate_root().join(snapshot_path))
            .unwrap_or_else(|err| panic!("read {snapshot_path}: {err}"));
        for required in [
            "pub fn wsi_rs::RawCompressedTile::bits_allocated(&self) -> u16",
            "pub fn wsi_rs::RawCompressedTile::compression(&self) -> wsi_rs::Compression",
            "pub fn wsi_rs::RawCompressedTile::data(&self) -> &[u8]",
            "pub fn wsi_rs::RawCompressedTile::height(&self) -> u32",
            "pub fn wsi_rs::RawCompressedTile::into_data(self) -> alloc::vec::Vec<u8>",
            "pub fn wsi_rs::RawCompressedTile::photometric_interpretation(&self) -> wsi_rs::EncodedTilePhotometricInterpretation",
            "pub fn wsi_rs::RawCompressedTile::samples_per_pixel(&self) -> u16",
            "pub fn wsi_rs::RawCompressedTile::width(&self) -> u32",
        ] {
            assert!(
                snapshot.contains(required),
                "{snapshot_path} must expose read-only RawCompressedTile accessors; missing `{required}`"
            );
        }
        for forbidden in [
            "pub wsi_rs::RawCompressedTile::bits_allocated:",
            "pub wsi_rs::RawCompressedTile::compression:",
            "pub wsi_rs::RawCompressedTile::data:",
            "pub wsi_rs::RawCompressedTile::height:",
            "pub wsi_rs::RawCompressedTile::photometric_interpretation:",
            "pub wsi_rs::RawCompressedTile::samples_per_pixel:",
            "pub wsi_rs::RawCompressedTile::width:",
        ] {
            assert!(
                !snapshot.contains(forbidden),
                "{snapshot_path} must not expose mutable RawCompressedTile fields; found `{forbidden}`"
            );
        }
    }
}

#[test]
fn raw_compressed_tile_construction_is_centralized_through_builder() {
    for relative in [
        "src/formats",
        "src/core/registry",
        "src/core/decode_runtime.rs",
        "src/bin",
        "examples",
    ] {
        let source = read_repo_text(relative);
        assert!(
            !source.contains("Ok(RawCompressedTile {"),
            "{relative} must construct raw compressed tile payloads through RawCompressedTile::builder so validation is centralized"
        );
    }
}

#[test]
fn public_metadata_structs_have_constructor_api_before_non_exhaustive_freeze() {
    let model = read_repo_text("src/core/types/model.rs");
    for required in [
        "impl Dataset",
        "pub fn new(id: DatasetId, scenes: Vec<Scene>) -> Self",
        "pub fn with_associated_images(",
        "pub fn with_properties(",
        "pub fn with_icc_profiles(",
        "impl IccProfileKey",
        "pub const fn new(scene: SceneId, series: SeriesId) -> Self",
        "impl Scene",
        "pub fn new(id: impl Into<String>, series: Vec<Series>) -> Self",
        "pub fn with_name(",
        "impl Series",
        "pub fn new(",
        "impl AxesShape",
        "pub const fn new(z: u32, c: u32, t: u32) -> Self",
        "impl Level",
        "pub fn new(dimensions: (u64, u64), downsample: f64, tile_layout: TileLayout) -> Self",
        "impl ChannelInfo",
        "pub fn new() -> Self",
        "pub fn with_color(",
        "impl AssociatedImage",
        "pub const fn new(dimensions: (u32, u32), sample_type: SampleType, channels: u16) -> Self",
        "impl RawCompressedTile",
        "pub fn builder(compression: Compression) -> RawCompressedTileBuilder",
        "pub struct RawCompressedTileBuilder",
        "pub enum RawCompressedTileBuildError",
    ] {
        assert!(
            model.contains(required),
            "metadata model must expose named construction API `{required}` before hiding literal construction"
        );
    }
}

#[test]
fn public_icc_profile_metadata_uses_stable_key_type() {
    let model = read_repo_text("src/core/types/model.rs");
    for required in [
        "pub struct IccProfileKey",
        "pub scene: SceneId",
        "pub series: SeriesId",
        "pub icc_profiles: HashMap<IccProfileKey, Vec<u8>>",
        "pub const fn new(scene: SceneId, series: SeriesId) -> Self",
        "pub fn with_icc_profiles(mut self, icc_profiles: HashMap<IccProfileKey, Vec<u8>>) -> Self",
    ] {
        assert!(
            model.contains(required),
            "ICC profile metadata must use a named typed key before the release-candidate API freeze; missing `{required}`"
        );
    }

    for forbidden in [
        "pub icc_profiles: HashMap<(usize, usize), Vec<u8>>",
        "with_icc_profiles(mut self, icc_profiles: HashMap<(usize, usize), Vec<u8>>)",
    ] {
        assert!(
            !model.contains(forbidden),
            "ICC profile metadata must not expose primitive tuple key `{forbidden}`"
        );
    }
}

#[test]
fn public_selection_and_geometry_structs_are_non_exhaustive() {
    for (relative, struct_name) in [
        ("src/core/types/requests.rs", "PlaneSelection"),
        ("src/core/types/geometry.rs", "TileEntry"),
        ("src/core/types/geometry.rs", "TileHit"),
        ("src/formats/svcache.rs", "SvcacheTileSelection"),
    ] {
        assert_non_exhaustive_struct(relative, struct_name);
    }
}

#[test]
fn public_selection_and_geometry_structs_have_constructor_api() {
    let requests = read_repo_text("src/core/types/requests.rs");
    assert!(
        requests.contains("impl PlaneSelection")
            && requests.contains("pub const fn new(z: u32, c: u32, t: u32) -> Self"),
        "PlaneSelection must expose a constructor before hiding literal construction"
    );

    let geometry = read_repo_text("src/core/types/geometry.rs");
    for required in [
        "impl TileEntry",
        "pub fn new(offset: (f64, f64), dimensions: (u32, u32)) -> Self",
        "pub fn with_tiff_tile_index(",
    ] {
        assert!(
            geometry.contains(required),
            "TileEntry must expose constructor API `{required}` before hiding literal construction"
        );
    }

    let svcache = read_repo_text("src/formats/svcache.rs");
    for required in [
        "impl SvcacheTileSelection",
        "pub fn new(",
        "scene: impl Into<SceneId>",
        "series: impl Into<SeriesId>",
        "level: impl Into<LevelIdx>",
        "pub fn with_plane(",
    ] {
        assert!(
            svcache.contains(required),
            "SvcacheTileSelection must expose constructor API `{required}` before hiding literal construction"
        );
    }
}

#[test]
fn public_svcache_tile_selection_uses_stable_newtypes() {
    let svcache = read_repo_text("src/formats/svcache.rs");
    for required in [
        "pub scene: SceneId",
        "pub series: SeriesId",
        "pub level: LevelIdx",
        "pub plane: PlaneIdx",
        "pub fn new(",
        "scene: impl Into<SceneId>",
        "series: impl Into<SeriesId>",
        "level: impl Into<LevelIdx>",
        "plane: impl Into<PlaneIdx>",
    ] {
        assert!(
            svcache.contains(required),
            "SvcacheTileSelection must use typed public indices before the release-candidate API freeze; missing `{required}`"
        );
    }

    for forbidden in [
        "pub scene: usize",
        "pub series: usize",
        "pub level: u32",
        "pub plane: PlaneSelection",
        "pub fn new(scene: usize, series: usize, level: u32",
    ] {
        assert!(
            !svcache.contains(forbidden),
            "SvcacheTileSelection must not expose primitive public indices `{forbidden}`"
        );
    }
}

#[test]
fn optional_metal_public_surface_is_future_extensible() {
    assert_non_exhaustive_struct("src/output/metal.rs", "MetalDeviceTile");
    assert_non_exhaustive_enum("src/output/metal.rs", "MetalDeviceStorage");
    assert_non_exhaustive_struct("src/output/cuda.rs", "CudaDeviceTile");
    assert_non_exhaustive_enum("src/output/cuda.rs", "CudaDeviceStorage");
}

#[test]
fn optional_cuda_public_surface_matches_device_tile_contract() {
    let cuda = read_repo_text("src/output/cuda.rs");
    for required in [
        "pub width: u32",
        "pub height: u32",
        "pub pitch_bytes: usize",
        "pub format: PixelFormat",
        "pub storage: CudaDeviceStorage",
        "j2k_jpeg_cuda::Surface",
        "j2k_cuda::Surface",
        "cuda_surface()",
    ] {
        assert!(
            cuda.contains(required),
            "CUDA device tile output must expose resident surface contract; missing `{required}`"
        );
    }

    let output = read_repo_text("src/core/types/output.rs");
    assert!(
        !output.contains("_phase5_placeholder"),
        "CudaDeviceTile must not remain a placeholder"
    );
}

#[test]
fn default_manifest_uses_cpu_jp2k_facade_and_optional_metal_adapter() {
    let manifest = fs::read_to_string(crate_root().join("Cargo.toml")).expect("read manifest");
    let manifest = manifest
        .parse::<toml::Value>()
        .expect("Cargo.toml must parse as TOML");

    let dependencies = manifest
        .get("dependencies")
        .and_then(toml::Value::as_table)
        .expect("Cargo.toml must define [dependencies]");
    assert!(
        dependencies.contains_key("j2k"),
        "wsi_rs default JP2K decode must depend on j2k facade"
    );

    let j2k_metal = dependencies
        .get("j2k-metal")
        .and_then(toml::Value::as_table)
        .expect("j2k-metal dependency must use table syntax");
    assert!(
        j2k_metal.get("optional").and_then(toml::Value::as_bool) == Some(true),
        "j2k-metal must be optional"
    );

    let features = manifest
        .get("features")
        .and_then(toml::Value::as_table)
        .expect("Cargo.toml must define [features]");
    let metal_feature = features
        .get("metal")
        .and_then(toml::Value::as_array)
        .expect("metal feature must be an array");
    assert!(
        metal_feature
            .iter()
            .any(|value| value.as_str() == Some("dep:j2k-metal")),
        "metal feature must be the only feature that enables j2k-metal"
    );

    let enabling_features = features
        .iter()
        .filter_map(|(name, value)| {
            value.as_array().and_then(|items| {
                items
                    .iter()
                    .any(|item| item.as_str() == Some("dep:j2k-metal"))
                    .then_some(name.as_str())
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(
        enabling_features,
        vec!["metal"],
        "only the metal feature may enable j2k-metal"
    );
}

#[test]
fn public_manifest_does_not_depend_on_local_path_patches() {
    let manifest = fs::read_to_string(crate_root().join("Cargo.toml")).expect("read manifest");
    let manifest = manifest
        .parse::<toml::Value>()
        .expect("Cargo.toml must parse as TOML");

    let Some(patches) = manifest.get("patch").and_then(toml::Value::as_table) else {
        return;
    };
    let Some(crates_io) = patches.get("crates-io").and_then(toml::Value::as_table) else {
        return;
    };

    let local_path_patches = crates_io
        .iter()
        .filter_map(|(name, value)| {
            value
                .as_table()
                .and_then(|table| table.get("path"))
                .and_then(toml::Value::as_str)
                .map(|path| format!("{name} -> {path}"))
        })
        .collect::<Vec<_>>();

    assert!(
        local_path_patches.is_empty(),
        "public manifest must verify against registry dependencies, not local path patches:\n{}",
        local_path_patches.join("\n")
    );
}

#[test]
fn tracked_text_files_do_not_contain_local_user_paths() {
    let mut offenders = Vec::new();
    let local_user_path = ["/Users", "user", ""].join("/");
    for path in tracked_text_files(crate_root()) {
        let source = fs::read_to_string(&path).unwrap_or_else(|err| {
            panic!("read {}: {err}", path.display());
        });
        if source.contains(&local_user_path) {
            offenders.push(relative_path(&path));
        }
    }

    assert!(
        offenders.is_empty(),
        "tracked text files must not contain local /Users/user paths:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn tracked_text_files_do_not_reference_agent_private_artifacts() {
    let private_docs_name = ["super", "powers"].concat();
    let private_dir = ["docs", private_docs_name.as_str()].join("/");
    let migration_doc = ["MIGRATION", ".md"].concat();
    let migration_doc_lower = migration_doc.to_ascii_lowercase();
    let mut offenders = Vec::new();

    for path in tracked_text_files(crate_root()) {
        let relative = relative_path(&path);
        let file_name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if relative.starts_with(&private_dir) || file_name == migration_doc_lower {
            offenders.push(relative);
        }
    }

    assert!(
        offenders.is_empty(),
        "tracked text files must not include agent-private planning docs or migration scratch files:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn public_docs_use_production_library_language() {
    let public_docs = [
        "README.md",
        "CHANGELOG.md",
        "SECURITY.md",
        "wsi-rs-openslide-shim/README.md",
        "tests/fixtures/jp2k/README.md",
        "src/lib.rs",
    ];
    let forbidden = [
        ["L", "L", "M"].concat(),
        ["A", "I", "-assisted"].concat(),
        ["agent", " or engineer"].concat(),
    ];
    let mut offenders = Vec::new();

    for relative in public_docs {
        let text = fs::read_to_string(crate_root().join(relative))
            .unwrap_or_else(|err| panic!("read {relative}: {err}"));
        for term in &forbidden {
            if text.contains(term) {
                offenders.push(format!("{relative}: {term}"));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "public docs should read like production library documentation, not assistant-facing prompt material:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn referenced_parity_corpus_fetch_script_exists() {
    let script = crate_root().join("scripts/parity-corpus-fetch.sh");
    assert!(
        script.is_file(),
        "tests reference scripts/parity-corpus-fetch.sh, so the script must exist"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&script)
            .expect("stat fetch script")
            .permissions()
            .mode();
        assert!(
            mode & 0o111 != 0,
            "scripts/parity-corpus-fetch.sh must be executable"
        );
    }
}

#[test]
fn public_docs_do_not_advertise_unregistered_zeiss_support() {
    let formats_mod =
        fs::read_to_string(crate_root().join("src/formats/mod.rs")).expect("read formats mod");
    let registry = read_repo_text("src/core/registry");
    let zeiss_registered = formats_mod.contains("mod zeiss") && registry.contains("ZeissBackend");

    if !zeiss_registered {
        let docs = [(
            "README.md",
            fs::read_to_string(crate_root().join("README.md")).expect("read README"),
        )];
        let offenders = docs
            .iter()
            .filter_map(|(path, text)| text.contains("Zeiss").then_some(*path))
            .collect::<Vec<_>>();
        assert!(
            offenders.is_empty(),
            "public docs must not advertise Zeiss until the backend is registered: {}",
            offenders.join(", ")
        );
    }
}

#[test]
fn unregistered_zeiss_backend_is_not_left_as_packaged_source() {
    let zeiss_source = crate_root().join("src/formats/zeiss.rs");
    if !zeiss_source.exists() {
        return;
    }

    let formats_mod =
        fs::read_to_string(crate_root().join("src/formats/mod.rs")).expect("read formats mod");
    let registry = read_repo_text("src/core/registry");
    assert!(
        formats_mod.contains("mod zeiss") && registry.contains("ZeissBackend"),
        "src/formats/zeiss.rs exists but the Zeiss backend is not registered"
    );
}

fn tracked_text_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_text_files(root, &mut files);
    files
}

fn collect_text_files(path: &Path, files: &mut Vec<PathBuf>) {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    if matches!(name, ".git" | "target") {
        return;
    }

    let metadata = fs::metadata(path).unwrap_or_else(|err| {
        panic!("stat {}: {err}", path.display());
    });
    if metadata.is_dir() {
        let mut entries = fs::read_dir(path)
            .unwrap_or_else(|err| panic!("read dir {}: {err}", path.display()))
            .collect::<Result<Vec<_>, _>>()
            .unwrap_or_else(|err| panic!("read dir entry under {}: {err}", path.display()));
        entries.sort_by_key(|entry| entry.path());
        for entry in entries {
            collect_text_files(&entry.path(), files);
        }
        return;
    }

    if is_text_file(path) {
        files.push(path.to_path_buf());
    }
}

fn is_text_file(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    if matches!(name, ".gitignore" | "LICENSE") {
        return true;
    }
    matches!(
        path.extension().and_then(|value| value.to_str()),
        Some("rs" | "md" | "toml" | "yml" | "yaml" | "sh" | "py" | "txt" | "lock" | "example")
    )
}

fn assert_non_exhaustive_enum(relative: &str, enum_name: &str) {
    let source = read_repo_text(relative);
    let needle = format!("pub enum {enum_name}");
    let Some(enum_start) = source.find(&needle) else {
        panic!("{relative} must define public enum `{enum_name}`");
    };
    let preceding = &source[..enum_start];
    let has_attribute = preceding
        .lines()
        .rev()
        .take_while(|line| {
            let trimmed = line.trim();
            trimmed.is_empty()
                || trimmed.starts_with("#[")
                || trimmed.starts_with("///")
                || trimmed.starts_with("//")
        })
        .any(|line| line.trim() == "#[non_exhaustive]");
    assert!(
        has_attribute,
        "{relative} public enum `{enum_name}` must be #[non_exhaustive] before 1.0"
    );
}

fn assert_non_exhaustive_struct(relative: &str, struct_name: &str) {
    let source = read_repo_text(relative);
    let needle = format!("pub struct {struct_name}");
    let Some(struct_start) = source.find(&needle) else {
        panic!("{relative} must define public struct `{struct_name}`");
    };
    let preceding = &source[..struct_start];
    let has_attribute = preceding
        .lines()
        .rev()
        .take_while(|line| {
            let trimmed = line.trim();
            trimmed.is_empty()
                || trimmed.starts_with("#[")
                || trimmed.starts_with("///")
                || trimmed.starts_with("//")
        })
        .any(|line| line.trim() == "#[non_exhaustive]");
    assert!(
        has_attribute,
        "{relative} public struct `{struct_name}` must be #[non_exhaustive] before 1.0"
    );
}

fn relative_path(path: &Path) -> String {
    path.strip_prefix(crate_root())
        .unwrap_or(path)
        .display()
        .to_string()
}
