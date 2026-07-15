use super::support::*;
use std::fs;

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
    for required in ["public-api", "scripts/check-semver.sh"] {
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
        "run_semver_check()",
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
        xtask_checks.contains("\"test\", \"--locked\", \"--doc\""),
        "doc-test must invoke locked cargo test --doc"
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
        "dtolnay/rust-toolchain@4be7066ada62dd38de10e7b70166bc74ed198c30",
        "toolchain: nightly-2026-04-17",
        "toolchain: \"1.96.0\"",
        "components: rustfmt,clippy",
        "taiki-e/install-action@2ca9b94c269419b7b0c711c09d0b21c4e1d51145",
        "cargo-nextest@0.9.136,cargo-hack@0.6.44,cargo-public-api@0.52.0,cargo-semver-checks@0.48.0,cargo-fuzz@0.13.1,cargo-deny@0.19.4,cargo-machete@0.9.2,cargo-vet@0.10.2",
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
fn cuda_release_validation_is_fail_closed_on_the_hardware_runner() {
    let workflow_path = crate_root().join(".github/workflows/cuda-validation.yml");
    assert!(
        workflow_path.is_file(),
        "CUDA releases require an on-demand hardware validation workflow"
    );
    let workflow = fs::read_to_string(workflow_path).expect("read CUDA validation workflow");

    for required in [
        "workflow_dispatch:",
        "runs-on: [self-hosted, Linux, X64, cuda]",
        "J2K_REQUIRE_CUDA_RUNTIME: \"1\"",
        "J2K_REQUIRE_CUDA_OXIDE_BUILD: \"1\"",
        "actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd",
        "dtolnay/rust-toolchain@4be7066ada62dd38de10e7b70166bc74ed198c30",
        "cargo test --locked --lib --features cuda",
    ] {
        assert!(
            workflow.contains(required),
            "CUDA validation workflow must contain `{required}`"
        );
    }
    assert!(
        workflow.contains("libclang not found; CUDA Oxide bindgen cannot run"),
        "CUDA validation must fail instead of skipping when bindgen cannot run"
    );
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
