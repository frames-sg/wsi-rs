use super::support::*;
use std::fs;

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
