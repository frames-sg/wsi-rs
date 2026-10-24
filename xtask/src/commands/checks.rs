use std::{env, ffi::OsString, fs, path::Path};

use wsi_rs_test_support::corpus;

use super::process::{
    cargo, ensure_clean_worktree, run_cargo, run_cargo_with_env, run_program, run_program_capture,
};
use super::{artifact, coverage};

const PUBLIC_API_SNAPSHOT_PATH: &str = "api/wsi-rs-public-api.txt";
const PUBLIC_API_CUDA_SNAPSHOT_PATH: &str = "api/wsi-rs-public-api-cuda.txt";
const PUBLIC_API_METAL_SNAPSHOT_PATH: &str = "api/wsi-rs-public-api-metal.txt";
const PINNED_NIGHTLY_TOOLCHAIN: &str = "nightly-2026-08-13";
const FUZZ_TARGETS: [&str; 9] = [
    "open_wsi_bytes",
    "open_jp2k_codestream_bytes",
    "open_svcache_bytes",
    "parse_xml_bytes",
    "open_dicom_bytes",
    "open_zvi_bytes",
    "open_mirax_bundle_bytes",
    "open_vms_bundle_bytes",
    "open_vsi_bundle_bytes",
];

pub(super) fn ci() -> Result<(), String> {
    validate()?;
    package()
}

pub(super) fn rc_preflight() -> Result<(), String> {
    artifact::smoke_configured()?;
    api_check()?;
    deps()?;
    fuzz_check()?;
    run_fuzz_campaigns(RELEASE_FUZZ_BUDGET)?;
    feature_check()?;
    validate()?;
    release_test()?;
    parity_corpus_test()?;
    coverage_with_corpus(true)?;
    performance_preflight()?;
    package()
}

pub(super) fn validate() -> Result<(), String> {
    fmt()?;
    clippy()?;
    bench_check()?;
    nextest()?;
    doc_test()?;
    doc()
}

pub(super) fn fmt() -> Result<(), String> {
    run_cargo(&["fmt", "--all", "--", "--check"])
}

pub(super) fn clippy() -> Result<(), String> {
    run_cargo(&[
        "clippy",
        "--locked",
        "--all-targets",
        "--",
        "-D",
        "warnings",
    ])
}

pub(super) fn test() -> Result<(), String> {
    run_test_matrix(&["test"])
}

pub(super) fn nextest() -> Result<(), String> {
    run_test_matrix(&["nextest", "run"])
}

fn run_test_matrix(runner: &[&str]) -> Result<(), String> {
    for feature in [
        None,
        Some("parity-openslide"),
        #[cfg(target_os = "macos")]
        Some("parity-metal"),
    ] {
        let mut args = runner.to_vec();
        args.extend(["--locked", "--lib", "--tests", "-p", "wsi-rs"]);
        if let Some(feature) = feature {
            args.extend(["--features", feature]);
        } else {
            // Shared corpus and OpenSlide tests belong to their owning crate.
            // They have no reader feature variants and only need to run once.
            args.extend(["-p", "wsi-rs-test-support"]);
        }
        run_cargo(&args)?;
    }
    Ok(())
}

pub(super) fn bench_check() -> Result<(), String> {
    run_cargo(&[
        "build",
        "--locked",
        "--release",
        "-p",
        "wsi-rs-perf",
        "-p",
        "wsi-rs-openslide-shim",
    ])
}

pub(super) fn feature_check() -> Result<(), String> {
    run_cargo(&[
        "hack",
        "check",
        "--locked",
        "--workspace",
        "--all-targets",
        "--feature-powerset",
        "--exclude-features",
        "metal,parity-metal",
    ])
}

/// Runner prefix for corpus runs under `parity-corpus-test`.
const CARGO_TEST: &[&str] = &["test", "--locked"];
/// Runner prefix for one instrumented corpus run under `coverage`.
const COVERAGE_STEP: &[&str] = &[
    "llvm-cov",
    "--no-clean",
    "--lcov",
    "--output-path",
    "target/coverage-corpus-step.lcov",
    "--locked",
];

/// One corpus-backed ignored-test run. `parity-corpus-test` and the corpus
/// part of `coverage` both run every selected entry of [`CORPUS_RUNS`].
struct CorpusRun {
    target: &'static [&'static str],
    features: Option<&'static str>,
    tests: CorpusTests,
    /// The run is skipped unless one of these aliases is selected. Empty
    /// means it runs whenever the corpus does.
    aliases: &'static [&'static str],
}

enum CorpusTests {
    /// The ignored test with this exact name.
    Exact(&'static str),
    /// Every ignored test in the target.
    AllIgnored,
    /// Each ignored test whose name starts with this prefix, one at a time.
    Prefix(&'static str),
}

impl CorpusRun {
    fn is_selected(&self) -> bool {
        self.aliases.is_empty()
            || self
                .aliases
                .iter()
                .any(|alias| corpus::alias_selected(alias))
    }
}

const VENDOR_REFERENCE_RUN: CorpusRun = CorpusRun {
    target: &["--test", "vendor_reference"],
    features: None,
    tests: CorpusTests::Prefix("vendor_native_pixels_"),
    aliases: &["olympus-vsi-001", "zeiss-zvi-001"],
};

const CORPUS_RUNS: &[CorpusRun] = &[
    CorpusRun {
        target: &["--test", "argos_huron"],
        features: None,
        tests: CorpusTests::Exact(
            "public_argos_slides_expose_sparse_pyramids_z_planes_and_associated_images",
        ),
        aliases: &["argos-001", "argos-stacked-001"],
    },
    CorpusRun {
        target: &["--test", "argos_huron"],
        features: None,
        tests: CorpusTests::Exact(
            "public_huron_jpeg_and_uncompressed_slides_decode_with_associated_images",
        ),
        aliases: &["huron-jpeg-001", "huron-uncompressed-001"],
    },
    CorpusRun {
        target: &["--test", "openslide_parity"],
        features: Some("parity-openslide"),
        tests: CorpusTests::Exact("preflight"),
        aliases: &[],
    },
    CorpusRun {
        target: &["--test", "j2k_parity"],
        features: None,
        tests: CorpusTests::Exact("j2k_cpu_vs_reference_within_tolerance"),
        aliases: &[],
    },
    CorpusRun {
        target: &["--test", "dicom_parity"],
        features: None,
        tests: CorpusTests::Exact("dicom_public_corpus_decodes_with_wsi_rs"),
        aliases: &[],
    },
    CorpusRun {
        target: &["--test", "dicom_parity"],
        features: Some("parity-openslide"),
        tests: CorpusTests::Exact("dicom_public_corpus_matches_openslide_within_tolerance"),
        aliases: &[],
    },
    CorpusRun {
        target: &["--test", "real_wsi_behavior"],
        features: None,
        tests: CorpusTests::AllIgnored,
        aliases: &[],
    },
    CorpusRun {
        target: &["--lib"],
        features: None,
        tests: CorpusTests::Prefix(
            "opens_public_dicom_folder_and_member_with_matching_levels_when_available",
        ),
        aliases: &["dicom-jp2k-001"],
    },
    CorpusRun {
        target: &["--lib"],
        features: None,
        tests: CorpusTests::Prefix("opens_olympus_vsi_when_corpus_is_available"),
        aliases: &["olympus-vsi-001"],
    },
    VENDOR_REFERENCE_RUN,
];

pub(super) fn parity_corpus_test() -> Result<(), String> {
    run_selected_corpus(CARGO_TEST)
}

fn run_selected_corpus(runner: &[&str]) -> Result<(), String> {
    for run in CORPUS_RUNS.iter().filter(|run| run.is_selected()) {
        run_corpus(run, runner)?;
    }
    Ok(())
}

fn run_corpus(run: &CorpusRun, runner: &[&str]) -> Result<(), String> {
    let mut args = test_command(runner, run.target, run.features, false);
    match run.tests {
        CorpusTests::Prefix(prefix) => {
            return run_ignored_with(runner, run.target, prefix, run.features, false)
        }
        CorpusTests::Exact(name) => args.extend(["--", name, "--exact", "--ignored"]),
        CorpusTests::AllIgnored => args.extend(["--", "--ignored"]),
    }
    run_cargo(&args)
}

/// Explicit lanes for fixtures and hardware excluded from quick correctness runs.
pub(super) fn conformance_test(arguments: Vec<String>) -> Result<(), String> {
    let [suite] = arguments.as_slice() else {
        return Err("usage: cargo xtask conformance-test <czi|vendor|zvi-variants|vmu|openslide|metal-color|metal-edges|local-htj2k|legacy-slides|ome-tiff>".into());
    };
    match suite.as_str() {
        "czi" => {
            run_ignored(&["--lib"], "uncompressed_sentinel_", None, false)?;
            run_ignored(&["--test", "zeiss_czi"], "real_jpegxr_czi_", None, false)
        }
        "vendor" => run_corpus(&VENDOR_REFERENCE_RUN, CARGO_TEST),
        "zvi-variants" => run_ignored(
            &["--test", "zeiss_zvi"],
            "builtin_registry_opens_zeiss_zvi_",
            None,
            false,
        ),
        "vmu" => run_ignored(
            &["--test", "hamamatsu_vmu"],
            "vmu_",
            Some("parity-openslide"),
            false,
        ),
        "openslide" => {
            run_ignored(
                &["--test", "openslide_compare"],
                "compare_against_openslide_",
                None,
                false,
            )?;
            run_ignored(
                &["--test", "openslide_compare"],
                "aperio_jpeg_rgb_pyramid_",
                None,
                false,
            )
        }
        "metal-color" => run_ignored(&["--lib"], "all_ycbcr8_values_", Some("metal"), false),
        "metal-edges" => run_ignored(
            &["--test", "metal_edge_conformance"],
            "configured_wsi_edges_",
            Some("metal"),
            false,
        ),
        "local-htj2k" => run_ignored(
            &["--lib"],
            "local_htj2k_device_pixels_match_cpu",
            Some("metal"),
            false,
        ),
        "legacy-slides" => {
            for name in [
                "opens_wrapped_first_ifd_ndpi_when_corpus_is_available",
                "opens_legacy_wrapped_offset_ndpi_when_corpus_is_available",
                "opens_dissimilar_leica_scenes_when_corpus_is_available",
                "opens_fluorescence_leica_channels_when_corpus_is_available",
                "opens_3dhistech_split_sparse_level_when_corpus_is_available",
            ] {
                run_ignored(&["--lib"], name, None, false)?;
            }
            Ok(())
        }
        "ome-tiff" => run_ignored(
            &["--lib"],
            "opens_ome_tiff_sub_ifd_pyramid_when_corpus_is_available",
            None,
            false,
        ),
        _ => Err(format!("unknown conformance suite `{suite}`")),
    }
}

/// Preserve the specialized experiments as maintained release benchmark recipes.
pub(super) fn perf_experiment(arguments: Vec<String>) -> Result<(), String> {
    let [name] = arguments.as_slice() else {
        return Err("usage: cargo xtask perf-experiment <test-name>; see README.md".into());
    };
    let (target, features): (&[&str], &str) = match name.as_str() {
        "metal_window_performance"
        | "metal_cooperative_performance"
        | "ycbcr_selected_u32_stays_within_five_percent_of_reference"
        | "local_htj2k_dicom_level_preparation_meets_interactive_budget" => {
            (&["--lib"], "metal,route-telemetry")
        }
        "prepared_cpu_performance"
        | "pooled_cpu_scheduling_performance"
        | "composition_performance" => (&["--lib"], "route-telemetry"),
        "native_decode_matrix" | "native_decode_revisits" | "native_decode_fresh_readers" => {
            (&["--test", "decode_performance"], "metal,route-telemetry")
        }
        "panning_matrix" | "warm_region_revisits" => {
            (&["--test", "panning_performance"], "metal,route-telemetry")
        }
        "czi_reader_workloads" => (&["--test", "czi_performance"], "route-telemetry"),
        _ => {
            return Err(format!(
                "unknown performance experiment `{name}`; see README.md"
            ))
        }
    };
    // Host-output experiments can run CPU routes on non-Metal hosts.
    let features = if cfg!(target_os = "macos") || target == ["--lib"] {
        features
    } else {
        "route-telemetry"
    };
    run_ignored(target, name, Some(features), true)
}

fn run_ignored(
    target: &[&str],
    filter: &str,
    features: Option<&str>,
    release: bool,
) -> Result<(), String> {
    run_ignored_with(CARGO_TEST, target, filter, features, release)
}

/// `runner`, a cargo subcommand with its options, followed by the test
/// target selection.
fn test_command<'a>(
    runner: &[&'a str],
    target: &[&'a str],
    features: Option<&'a str>,
    release: bool,
) -> Vec<&'a str> {
    let mut command = runner.to_vec();
    command.extend_from_slice(target);
    if let Some(features) = features {
        command.extend(["--features", features]);
    }
    if release {
        command.push("--release");
    }
    command
}

fn run_ignored_with(
    runner: &[&str],
    target: &[&str],
    filter: &str,
    features: Option<&str>,
    release: bool,
) -> Result<(), String> {
    if features.is_some_and(|features| features.split(',').any(|feature| feature == "metal"))
        && !cfg!(target_os = "macos")
    {
        return Err("this lane requires a macOS Metal runtime".into());
    }
    let base = test_command(runner, target, features, release);
    let mut list = base.clone();
    list.extend(["--", "--ignored", "--list"]);
    let output = run_program_capture(cargo(), &list, &[])?;
    let names: Vec<_> = output
        .lines()
        .filter_map(|line| line.strip_suffix(": test"))
        .filter(|name| name.rsplit("::").next().unwrap_or(name).starts_with(filter))
        .collect();
    if names.is_empty() {
        return Err(format!(
            "ignored test selection `{filter}` matched no tests"
        ));
    }
    for name in names {
        let mut args = base.clone();
        args.extend(["--", name, "--exact", "--ignored", "--test-threads=1"]);
        run_cargo(&args)?;
    }
    Ok(())
}

pub(super) fn doc() -> Result<(), String> {
    run_cargo_with_env(
        &["doc", "--locked", "--no-deps"],
        &[("RUSTDOCFLAGS", "-D warnings")],
    )
}

pub(super) fn doc_test() -> Result<(), String> {
    run_cargo(&["test", "--locked", "--doc"])
}

pub(super) fn typos() -> Result<(), String> {
    run_program(OsString::from("typos"), &[], &[])
}

pub(super) fn deny() -> Result<(), String> {
    run_cargo(&[
        "deny",
        "--locked",
        "check",
        "advisories",
        "bans",
        "licenses",
        "sources",
    ])
}

pub(super) fn unused_deps() -> Result<(), String> {
    run_program(OsString::from("cargo-machete"), &["."], &[])
}

pub(super) fn deps() -> Result<(), String> {
    deny()?;
    unused_deps()?;
    run_cargo(&["vet", "--locked"])
}

pub(super) fn api_check() -> Result<(), String> {
    check_public_api_snapshot_for(
        PUBLIC_API_SNAPSHOT_PATH,
        &["public-api", "-p", "wsi-rs", "-sss", "--color", "never"],
    )?;
    check_public_api_snapshot_for(
        PUBLIC_API_CUDA_SNAPSHOT_PATH,
        &[
            "public-api",
            "-p",
            "wsi-rs",
            "--features",
            "cuda",
            "-sss",
            "--color",
            "never",
        ],
    )?;
    if cfg!(target_os = "macos") {
        check_public_api_snapshot_for(
            PUBLIC_API_METAL_SNAPSHOT_PATH,
            &[
                "public-api",
                "-p",
                "wsi-rs",
                "--features",
                "metal",
                "-sss",
                "--color",
                "never",
            ],
        )?;
    }
    run_semver_check()
}

pub(super) fn fuzz_check() -> Result<(), String> {
    let root_lock = fs::read("Cargo.lock").map_err(|err| format!("read Cargo.lock: {err}"))?;
    let fuzz_lock =
        fs::read("fuzz/Cargo.lock").map_err(|err| format!("read fuzz/Cargo.lock: {err}"))?;
    for target in FUZZ_TARGETS {
        run_program(
            OsString::from("rustup"),
            &[
                "run",
                PINNED_NIGHTLY_TOOLCHAIN,
                "cargo",
                "fuzz",
                "check",
                target,
            ],
            &[],
        )
        .map_err(|err| {
            format!(
                "{err}\n`cargo xtask fuzz-check` requires nightly Rust and cargo-fuzz; install cargo-fuzz with `cargo install cargo-fuzz` if the command is unavailable"
            )
        })?;
    }
    if fs::read("Cargo.lock").ok().as_deref() != Some(root_lock.as_slice())
        || fs::read("fuzz/Cargo.lock").ok().as_deref() != Some(fuzz_lock.as_slice())
    {
        return Err("cargo-fuzz changed a tracked lockfile; update and review lockfiles before rerunning the gate".into());
    }
    Ok(())
}

/// How long each fuzz target runs, and how long one input may take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FuzzBudget {
    seconds: u64,
    timeout_seconds: u64,
}

/// The release preflight's campaign, and `release-fuzz`'s default.
const RELEASE_FUZZ_BUDGET: FuzzBudget = FuzzBudget {
    seconds: 300,
    timeout_seconds: 10,
};

const RELEASE_FUZZ_USAGE: &str = "usage: cargo xtask release-fuzz [--seconds N] [--timeout N]";

pub(super) fn release_fuzz(arguments: Vec<String>) -> Result<(), String> {
    run_fuzz_campaigns(parse_fuzz_budget(&arguments)?)
}

fn parse_fuzz_budget(arguments: &[String]) -> Result<FuzzBudget, String> {
    let mut budget = RELEASE_FUZZ_BUDGET;
    let mut arguments = arguments.iter();
    while let Some(flag) = arguments.next() {
        let field = match flag.as_str() {
            "--seconds" => &mut budget.seconds,
            "--timeout" => &mut budget.timeout_seconds,
            _ => return Err(format!("{RELEASE_FUZZ_USAGE}; unknown argument {flag:?}")),
        };
        *field = arguments
            .next()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
            .ok_or_else(|| {
                format!("{RELEASE_FUZZ_USAGE}; {flag} takes a positive number of seconds")
            })?;
    }
    Ok(budget)
}

fn run_fuzz_campaigns(budget: FuzzBudget) -> Result<(), String> {
    for target in FUZZ_TARGETS {
        let corpus = format!("fuzz/corpus/{target}");
        fs::create_dir_all(&corpus)
            .map_err(|error| format!("create release fuzz corpus {corpus}: {error}"))?;
        let seed = format!("fuzz/seeds/{target}");
        let seed = Path::new(&seed).is_dir().then_some(seed);
        let owned_args = fuzz_run_args(target, corpus, seed, budget);
        let args = owned_args.iter().map(String::as_str).collect::<Vec<_>>();
        run_program(OsString::from("rustup"), &args, &[])
            .map_err(|error| format!("release fuzz campaign failed for {target}: {error}"))?;
    }
    Ok(())
}

fn fuzz_run_args(
    target: &str,
    corpus: String,
    seed: Option<String>,
    budget: FuzzBudget,
) -> Vec<String> {
    let mut args = [
        "run",
        PINNED_NIGHTLY_TOOLCHAIN,
        "cargo",
        "fuzz",
        "run",
        "--sanitizer",
        "address",
        target,
    ]
    .map(String::from)
    .to_vec();
    args.push(corpus);
    args.extend(seed);
    args.extend([
        "--".to_string(),
        format!("-max_total_time={}", budget.seconds),
        format!("-timeout={}", budget.timeout_seconds),
        "-rss_limit_mb=2048".to_string(),
    ]);
    args
}

fn performance_preflight() -> Result<(), String> {
    let capture = |name: &str| {
        env::var(name).map_err(|_| {
            format!("release performance gate requires {name} to name a reviewed capture JSON")
        })
    };
    let openslide = capture("WSI_RS_RC_OPENSLIDE_CAPTURE")?;
    let previous = capture("WSI_RS_RC_PREVIOUS_CAPTURE")?;
    let current = capture("WSI_RS_RC_CURRENT_CAPTURE")?;
    super::perf::compare(vec![openslide, current.clone()])?;
    super::perf::compare(vec![previous, current])
}

fn check_public_api_snapshot_for(snapshot_path: &str, args: &[&str]) -> Result<(), String> {
    let rustup_args = pinned_nightly_cargo_args(args);
    let actual = run_program_capture(OsString::from("rustup"), &rustup_args, &[]).map_err(|err| {
        format!(
            "{err}\n`cargo xtask api-check` requires cargo-public-api; install it with `cargo install cargo-public-api` if the command is unavailable"
        )
    })?;
    check_public_api_snapshot(&actual, snapshot_path)
}

fn pinned_nightly_cargo_args<'a>(args: &'a [&'a str]) -> Vec<&'a str> {
    let mut rustup_args = vec!["run", PINNED_NIGHTLY_TOOLCHAIN, "cargo"];
    rustup_args.extend_from_slice(args);
    rustup_args
}

fn run_semver_check() -> Result<(), String> {
    run_program(OsString::from("scripts/check-semver.sh"), &[], &[]).map_err(|err| {
        format!("{err}\n`cargo xtask api-check` requires nightly Rust and cargo-semver-checks")
    })
}

fn check_public_api_snapshot(actual: &str, snapshot_path: &str) -> Result<(), String> {
    check_public_api_snapshot_with_update(
        actual,
        Path::new(snapshot_path),
        env::var("WSI_RS_UPDATE_PUBLIC_API").as_deref() == Ok("1"),
    )
}

fn check_public_api_snapshot_with_update(
    actual: &str,
    snapshot_path: &Path,
    update: bool,
) -> Result<(), String> {
    let normalized_actual = normalize_snapshot(actual);
    if update {
        if let Some(parent) = snapshot_path.parent() {
            fs::create_dir_all(parent).map_err(|err| {
                format!(
                    "failed to create public API snapshot directory {}: {err}",
                    parent.display()
                )
            })?;
        }
        fs::write(snapshot_path, format!("{normalized_actual}\n")).map_err(|err| {
            format!(
                "failed to write public API snapshot {}: {err}",
                snapshot_path.display()
            )
        })?;
        return Ok(());
    }

    let expected = fs::read_to_string(snapshot_path).map_err(|err| {
        format!(
            "failed to read public API snapshot {}: {err}\nrun `WSI_RS_UPDATE_PUBLIC_API=1 cargo xtask api-check` to create or refresh it",
            snapshot_path.display()
        )
    })?;
    let normalized_expected = normalize_snapshot(&expected);
    if normalized_actual == normalized_expected {
        Ok(())
    } else {
        Err(format!(
            "public API snapshot is stale: {}\nrun `WSI_RS_UPDATE_PUBLIC_API=1 cargo xtask api-check` and review the snapshot diff",
            snapshot_path.display()
        ))
    }
}

fn normalize_snapshot(snapshot: &str) -> String {
    snapshot.trim_end().replace("\r\n", "\n")
}

pub(super) fn release_test() -> Result<(), String> {
    run_cargo(&["test", "--locked", "--lib", "--tests", "--release"])
}

const COVERAGE_BASE_ARGS: &[&str] = &[
    "llvm-cov",
    "--locked",
    "--workspace",
    "--lcov",
    "--output-path",
    "lcov.info",
];
const COVERAGE_REPORT_ARGS: &[&str] = &[
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
    "lcov.info",
];

pub(super) fn coverage() -> Result<(), String> {
    coverage_with_corpus(corpus::selected_aliases().is_some())
}

fn coverage_with_corpus(include_corpus: bool) -> Result<(), String> {
    // Do not narrow this to `--lib --tests`: xtask, the OpenSlide shim, and the
    // performance worker all contain production binary code with unit tests.
    run_cargo(COVERAGE_BASE_ARGS)?;

    if include_corpus {
        run_selected_corpus(COVERAGE_STEP)?;
        run_cargo(COVERAGE_REPORT_ARGS)?;
    }

    coverage::enforce_workspace(Path::new("lcov.info"))
}

pub(super) fn package() -> Result<(), String> {
    ensure_clean_worktree()?;
    run_cargo(&["package", "--locked"])?;
    run_cargo(&["publish", "--dry-run", "--locked"])
}

#[cfg(test)]
mod tests;
