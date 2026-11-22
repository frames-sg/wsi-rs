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
/// Names the shim `tests/hamamatsu_vmu.rs` loads through the OpenSlide ABI.
const VMU_SHIM_LIBRARY_ENV: &str = "WSI_RS_VMU_SHIM_LIBRARY";
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
    bench_check()?;
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
    bench_check()?;
    release_test()?;
    parity_corpus_test()?;
    coverage_with_corpus(true)?;
    performance_preflight()?;
    package()
}

pub(super) fn validate() -> Result<(), String> {
    fmt()?;
    clippy()?;
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
    let mut args = runner.to_vec();
    args.extend([
        "--locked",
        "--lib",
        "--tests",
        "-p",
        "wsi-rs",
        "-p",
        "wsi-rs-test-support",
    ]);
    run_cargo(&args)?;

    // OpenSlide only adds oracle tests. Real-slide comparisons belong to
    // parity-corpus-test and conformance-test, where their inputs are required.
    let mut args = runner.to_vec();
    args.extend([
        "--locked",
        "-p",
        "wsi-rs",
        "--test",
        "support_unit",
        "--features",
        "parity-openslide",
        "openslide_oracle_",
    ]);
    run_cargo(&args)?;

    // Metal changes production library paths, so retain the library suite
    // under that configuration, without repeating unrelated integration tests.
    #[cfg(target_os = "macos")]
    {
        let mut args = runner.to_vec();
        args.extend([
            "--locked",
            "-p",
            "wsi-rs",
            "--lib",
            "--test",
            "metal_surface_smoke",
            "--features",
            "parity-metal",
        ]);
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

const LIB: &[&str] = &["--lib"];

/// Which ignored tests of one test target a run selects, by the last segment
/// of each test's path.
#[derive(Clone, Copy, Debug)]
enum IgnoredTests {
    /// The ignored test with this name.
    Exact(&'static str),
    /// Every ignored test whose name starts with this prefix.
    Prefix(&'static str),
    /// Every ignored test in the target.
    All,
}

impl IgnoredTests {
    fn selects(self, path: &str) -> bool {
        let name = path.rsplit("::").next().unwrap_or(path);
        match self {
            Self::Exact(exact) => name == exact,
            Self::Prefix(prefix) => name.starts_with(prefix),
            Self::All => true,
        }
    }
}

/// Ignored tests in one cargo test target, built with `features`.
#[derive(Clone, Copy, Debug)]
struct IgnoredRun {
    target: &'static [&'static str],
    features: Option<&'static str>,
    tests: IgnoredTests,
}

/// One corpus-backed ignored-test run. `parity-corpus-test` and the corpus
/// part of `coverage` both run every selected entry of [`CORPUS_RUNS`].
struct CorpusRun {
    run: IgnoredRun,
    /// The run is skipped unless one of these aliases is selected. Empty
    /// means it runs whenever the corpus does.
    aliases: &'static [&'static str],
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

const VENDOR_REFERENCE_RUN: IgnoredRun = IgnoredRun {
    target: &["--test", "vendor_reference"],
    features: None,
    tests: IgnoredTests::Prefix("vendor_native_pixels_"),
};

const CORPUS_RUNS: &[CorpusRun] = &[
    CorpusRun {
        run: IgnoredRun {
            target: &["--test", "color_regression"],
            features: None,
            tests: IgnoredTests::Exact("public_brightfield_color_sanity_svs_ndpi_dicom"),
        },
        aliases: &["svs-001", "ndpi-001", "dicom-jp2k-001"],
    },
    CorpusRun {
        run: IgnoredRun {
            target: &["--test", "color_regression"],
            features: Some("parity-openslide"),
            tests: IgnoredTests::Exact("svs_and_ndpi_overview_color_matches_openslide"),
        },
        aliases: &["svs-001", "ndpi-001"],
    },
    CorpusRun {
        run: IgnoredRun {
            target: &["--test", "argos_huron"],
            features: None,
            tests: IgnoredTests::Exact(
                "public_argos_slides_expose_sparse_pyramids_z_planes_and_associated_images",
            ),
        },
        aliases: &["argos-001", "argos-stacked-001"],
    },
    CorpusRun {
        run: IgnoredRun {
            target: &["--test", "argos_huron"],
            features: None,
            tests: IgnoredTests::Exact(
                "public_huron_jpeg_and_uncompressed_slides_decode_with_associated_images",
            ),
        },
        aliases: &["huron-jpeg-001", "huron-uncompressed-001"],
    },
    CorpusRun {
        run: IgnoredRun {
            target: &["--test", "openslide_parity"],
            features: Some("parity-openslide"),
            tests: IgnoredTests::Exact("preflight"),
        },
        aliases: &[],
    },
    CorpusRun {
        run: IgnoredRun {
            target: &["--test", "j2k_parity"],
            features: None,
            tests: IgnoredTests::Exact("j2k_cpu_vs_reference_within_tolerance"),
        },
        aliases: &[],
    },
    CorpusRun {
        run: IgnoredRun {
            target: &["--test", "dicom_parity"],
            features: None,
            tests: IgnoredTests::Exact("dicom_public_corpus_decodes_with_wsi_rs"),
        },
        aliases: &[],
    },
    CorpusRun {
        run: IgnoredRun {
            target: &["--test", "dicom_parity"],
            features: Some("parity-openslide"),
            tests: IgnoredTests::Exact("dicom_public_corpus_matches_openslide_within_tolerance"),
        },
        aliases: &[],
    },
    CorpusRun {
        run: IgnoredRun {
            target: &["--test", "real_wsi_behavior"],
            features: None,
            tests: IgnoredTests::All,
        },
        aliases: &[],
    },
    CorpusRun {
        run: IgnoredRun {
            target: LIB,
            features: None,
            tests: IgnoredTests::Exact(
                "opens_public_dicom_folder_and_member_with_matching_levels_when_available",
            ),
        },
        aliases: &["dicom-jp2k-001"],
    },
    CorpusRun {
        run: IgnoredRun {
            target: LIB,
            features: None,
            tests: IgnoredTests::Exact("opens_olympus_vsi_when_corpus_is_available"),
        },
        aliases: &["olympus-vsi-001"],
    },
    CorpusRun {
        run: VENDOR_REFERENCE_RUN,
        aliases: &["olympus-vsi-001", "zeiss-zvi-001"],
    },
];

pub(super) fn parity_corpus_test() -> Result<(), String> {
    run_selected_corpus(CARGO_TEST)
}

fn run_selected_corpus(runner: &[&str]) -> Result<(), String> {
    for corpus_run in CORPUS_RUNS.iter().filter(|run| run.is_selected()) {
        run_ignored_with(runner, &corpus_run.run, false, &[])?;
    }
    Ok(())
}

/// An explicit lane for fixtures and hardware excluded from quick correctness
/// runs.
struct ConformanceSuite {
    name: &'static str,
    runs: &'static [IgnoredRun],
    /// Set to the release OpenSlide shim, which the suite builds first.
    shim_library_env: Option<&'static str>,
}

const fn lib_test(features: Option<&'static str>, tests: IgnoredTests) -> IgnoredRun {
    IgnoredRun {
        target: LIB,
        features,
        tests,
    }
}

const CONFORMANCE_SUITES: &[ConformanceSuite] = &[
    ConformanceSuite {
        name: "czi",
        runs: &[
            lib_test(None, IgnoredTests::Prefix("uncompressed_sentinel_")),
            IgnoredRun {
                target: &["--test", "zeiss_czi"],
                features: None,
                tests: IgnoredTests::Prefix("real_jpegxr_czi_"),
            },
        ],
        shim_library_env: None,
    },
    ConformanceSuite {
        name: "vendor",
        runs: &[VENDOR_REFERENCE_RUN],
        shim_library_env: None,
    },
    ConformanceSuite {
        name: "zvi-variants",
        runs: &[IgnoredRun {
            target: &["--test", "zeiss_zvi"],
            features: None,
            tests: IgnoredTests::All,
        }],
        shim_library_env: None,
    },
    ConformanceSuite {
        name: "vmu",
        runs: &[IgnoredRun {
            target: &["--test", "hamamatsu_vmu"],
            features: Some("parity-openslide"),
            tests: IgnoredTests::Prefix("vmu_"),
        }],
        shim_library_env: Some(VMU_SHIM_LIBRARY_ENV),
    },
    ConformanceSuite {
        name: "openslide",
        runs: &[
            IgnoredRun {
                target: &["--test", "openslide_compare"],
                features: None,
                tests: IgnoredTests::Prefix("compare_against_openslide_"),
            },
            IgnoredRun {
                target: &["--test", "openslide_compare"],
                features: None,
                tests: IgnoredTests::Prefix("aperio_jpeg_rgb_pyramid_"),
            },
        ],
        shim_library_env: None,
    },
    ConformanceSuite {
        name: "metal-color",
        runs: &[lib_test(
            Some("metal"),
            IgnoredTests::Prefix("all_ycbcr8_values_"),
        )],
        shim_library_env: None,
    },
    ConformanceSuite {
        name: "metal-edges",
        runs: &[IgnoredRun {
            target: &["--test", "metal_edge_conformance"],
            features: Some("metal"),
            tests: IgnoredTests::Prefix("configured_wsi_edges_"),
        }],
        shim_library_env: None,
    },
    ConformanceSuite {
        name: "local-htj2k",
        runs: &[lib_test(
            Some("metal"),
            IgnoredTests::Exact("local_htj2k_device_pixels_match_cpu"),
        )],
        shim_library_env: None,
    },
    ConformanceSuite {
        name: "legacy-slides",
        runs: &[
            lib_test(
                None,
                IgnoredTests::Exact("opens_wrapped_first_ifd_ndpi_when_corpus_is_available"),
            ),
            lib_test(
                None,
                IgnoredTests::Exact("opens_legacy_wrapped_offset_ndpi_when_corpus_is_available"),
            ),
            lib_test(
                None,
                IgnoredTests::Exact("opens_dissimilar_leica_scenes_when_corpus_is_available"),
            ),
            lib_test(
                None,
                IgnoredTests::Exact("opens_fluorescence_leica_channels_when_corpus_is_available"),
            ),
            lib_test(
                None,
                IgnoredTests::Exact("opens_3dhistech_split_sparse_level_when_corpus_is_available"),
            ),
        ],
        shim_library_env: None,
    },
    ConformanceSuite {
        name: "ome-tiff",
        runs: &[lib_test(
            None,
            IgnoredTests::Exact("opens_ome_tiff_sub_ifd_pyramid_when_corpus_is_available"),
        )],
        shim_library_env: None,
    },
];

pub(super) fn conformance_test(arguments: Vec<String>) -> Result<(), String> {
    let usage = || {
        let names = CONFORMANCE_SUITES
            .iter()
            .map(|suite| suite.name)
            .collect::<Vec<_>>();
        format!("usage: cargo xtask conformance-test <{}>", names.join("|"))
    };
    let [name] = arguments.as_slice() else {
        return Err(usage());
    };
    let suite = CONFORMANCE_SUITES
        .iter()
        .find(|suite| suite.name == name)
        .ok_or_else(|| format!("unknown conformance suite `{name}`; {}", usage()))?;
    let shim = suite
        .shim_library_env
        .map(|_| release_shim_path())
        .transpose()?;
    let envs = suite
        .shim_library_env
        .zip(shim.as_deref())
        .into_iter()
        .collect::<Vec<_>>();
    for run in suite.runs {
        run_ignored_with(CARGO_TEST, run, false, &envs)?;
    }
    Ok(())
}

/// Builds the release OpenSlide shim and returns its path.
fn release_shim_path() -> Result<String, String> {
    let shim = super::perf::build_release_shim()?;
    shim.to_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("shim path is not UTF-8: {}", shim.display()))
}

/// Release benchmarks in one test target, each run by name.
struct PerfExperiments {
    target: &'static [&'static str],
    features: &'static str,
    names: &'static [&'static str],
}

const PERF_EXPERIMENTS: &[PerfExperiments] = &[
    PerfExperiments {
        target: LIB,
        features: "metal,route-telemetry",
        names: &[
            "metal_window_performance",
            "metal_cooperative_performance",
            "ycbcr_selected_u32_stays_within_five_percent_of_reference",
            "local_htj2k_dicom_level_preparation_meets_interactive_budget",
        ],
    },
    PerfExperiments {
        target: LIB,
        features: "route-telemetry",
        names: &[
            "pooled_cpu_scheduling_performance",
            "composition_performance",
        ],
    },
    PerfExperiments {
        target: &["--test", "decode_performance"],
        features: "metal,route-telemetry",
        names: &[
            "native_decode_matrix",
            "native_decode_revisits",
            "native_decode_fresh_readers",
        ],
    },
    PerfExperiments {
        target: &["--test", "panning_performance"],
        features: "metal,route-telemetry",
        names: &["panning_matrix", "warm_region_revisits"],
    },
    PerfExperiments {
        target: &["--test", "czi_performance"],
        features: "route-telemetry",
        names: &["czi_reader_workloads"],
    },
];

/// Preserve the specialized experiments as maintained release benchmark recipes.
pub(super) fn perf_experiment(arguments: Vec<String>) -> Result<(), String> {
    let [name] = arguments.as_slice() else {
        return Err("usage: cargo xtask perf-experiment <test-name>; see docs/testing.md".into());
    };
    let Some((experiments, name)) = PERF_EXPERIMENTS.iter().find_map(|experiments| {
        experiments
            .names
            .iter()
            .find(|candidate| *candidate == name)
            .map(|name| (experiments, *name))
    }) else {
        return Err(format!(
            "unknown performance experiment `{name}`; see docs/testing.md"
        ));
    };
    // Host-output experiments can run CPU routes on non-Metal hosts.
    let features = if cfg!(target_os = "macos") || experiments.target == LIB {
        experiments.features
    } else {
        "route-telemetry"
    };
    let run = IgnoredRun {
        target: experiments.target,
        features: Some(features),
        tests: IgnoredTests::Exact(name),
    };
    run_ignored_with(CARGO_TEST, &run, true, &[])
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

/// The tests `run` selects among the `--ignored --list` output of its target.
fn selected_ignored_tests<'a>(listing: &'a str, run: &IgnoredRun) -> Vec<&'a str> {
    listing
        .lines()
        .filter_map(|line| line.strip_suffix(": test"))
        .filter(|name| run.tests.selects(name))
        .collect()
}

/// Lists the ignored tests of `run`'s target, fails when its selection matches
/// none, and runs each selected test alone.
fn run_ignored_with(
    runner: &[&str],
    run: &IgnoredRun,
    release: bool,
    envs: &[(&str, &str)],
) -> Result<(), String> {
    if run
        .features
        .is_some_and(|features| features.split(',').any(|feature| feature == "metal"))
        && !cfg!(target_os = "macos")
    {
        return Err("this lane requires a macOS Metal runtime".into());
    }
    let base = test_command(runner, run.target, run.features, release);
    let mut list = base.clone();
    list.extend(["--", "--ignored", "--list"]);
    let output = run_program_capture(cargo(), &list, envs)?;
    let names = selected_ignored_tests(&output, run);
    if names.is_empty() {
        return Err(format!(
            "ignored test selection {:?} matched no tests in `{}`",
            run.tests,
            run.target.join(" ")
        ));
    }
    for name in names {
        let mut args = base.clone();
        args.extend(["--", name, "--exact", "--ignored", "--test-threads=1"]);
        run_cargo_with_env(&args, envs)?;
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
    let manifest =
        fs::read_to_string("Cargo.toml").map_err(|err| format!("read Cargo.toml: {err}"))?;
    let manifest: toml::Value =
        toml::from_str(&manifest).map_err(|err| format!("parse Cargo.toml: {err}"))?;
    if manifest
        .get("package")
        .and_then(|package| package.get("publish"))
        == Some(&toml::Value::Boolean(false))
    {
        eprintln!("GitHub-only release: verify the source build; registry publication is disabled");
        return run_cargo(&["build", "--locked", "--lib", "-p", "wsi-rs"]);
    }
    run_cargo(&["package", "--locked"])?;
    run_cargo(&["publish", "--dry-run", "--locked"])
}

#[cfg(test)]
mod tests;
