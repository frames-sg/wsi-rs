use std::{env, ffi::OsString, fs, path::Path};

use super::process::{
    ensure_clean_worktree, run_cargo, run_cargo_with_env, run_program, run_program_capture,
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
    release_fuzz()?;
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
    run_cargo(&["test", "--locked", "--lib", "--tests"])?;
    run_cargo(&[
        "test",
        "--locked",
        "--lib",
        "--tests",
        "--features",
        "parity-openslide",
    ])?;
    if cfg!(target_os = "macos") {
        run_cargo(&[
            "test",
            "--locked",
            "--lib",
            "--tests",
            "--features",
            "parity-metal",
        ])?;
    }
    Ok(())
}

pub(super) fn nextest() -> Result<(), String> {
    run_cargo(&["nextest", "run", "--locked", "--lib", "--tests"])?;
    run_cargo(&[
        "nextest",
        "run",
        "--locked",
        "--lib",
        "--tests",
        "--features",
        "parity-openslide",
    ])?;
    if cfg!(target_os = "macos") {
        run_cargo(&[
            "nextest",
            "run",
            "--locked",
            "--lib",
            "--tests",
            "--features",
            "parity-metal",
        ])?;
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

pub(super) fn parity_corpus_test() -> Result<(), String> {
    run_cargo(&[
        "test",
        "--locked",
        "--test",
        "argos_huron",
        "--",
        "--ignored",
    ])?;
    run_cargo(&[
        "test",
        "--locked",
        "--test",
        "openslide_parity",
        "--features",
        "parity-openslide",
        "preflight",
        "--",
        "--exact",
        "--ignored",
    ])?;
    run_cargo(&[
        "test",
        "--locked",
        "--test",
        "j2k_parity",
        "j2k_cpu_vs_reference_within_tolerance",
        "--",
        "--exact",
        "--ignored",
    ])?;
    run_cargo(&[
        "test",
        "--locked",
        "--test",
        "dicom_parity",
        "dicom_public_corpus_decodes_with_wsi_rs",
        "--",
        "--exact",
        "--ignored",
    ])?;
    run_cargo(&[
        "test",
        "--locked",
        "--test",
        "dicom_parity",
        "--features",
        "parity-openslide",
        "dicom_public_corpus_matches_openslide_within_tolerance",
        "--",
        "--exact",
        "--ignored",
    ])?;
    run_cargo(&[
        "test",
        "--locked",
        "--test",
        "real_wsi_behavior",
        "--",
        "--ignored",
    ])?;
    for (alias, test) in [
        (
            "dicom-jp2k-001",
            "opens_public_dicom_folder_and_member_with_matching_levels_when_available",
        ),
        (
            "olympus-vsi-001",
            "opens_olympus_vsi_when_corpus_is_available",
        ),
    ] {
        if corpus_alias_selected(alias) {
            run_ignored(&["--lib"], test, None, false)?;
        }
    }
    if corpus_alias_selected("olympus-vsi-001") || corpus_alias_selected("zeiss-zvi-001") {
        conformance_test(vec!["vendor".into()])?;
    }
    Ok(())
}

fn corpus_alias_selected(alias: &str) -> bool {
    env::var("WSI_RS_PARITY_ALIASES").map_or(true, |aliases| {
        aliases.split(',').any(|value| value.trim() == alias)
    })
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
        "vendor" => run_ignored(
            &["--test", "vendor_reference"],
            "vendor_native_pixels_",
            None,
            false,
        ),
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
    if features.is_some_and(|features| features.split(',').any(|feature| feature == "metal"))
        && !cfg!(target_os = "macos")
    {
        return Err("this lane requires a macOS Metal runtime".into());
    }
    let mut base = vec!["test", "--locked"];
    base.extend_from_slice(target);
    if let Some(features) = features {
        base.extend(["--features", features]);
    }
    if release {
        base.push("--release");
    }
    let mut list = base.clone();
    list.extend(["--", "--ignored", "--list"]);
    let output = run_program_capture(
        env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo")),
        &list,
        &[],
    )?;
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

pub(super) fn release_fuzz() -> Result<(), String> {
    for target in FUZZ_TARGETS {
        let corpus = format!("fuzz/corpus/{target}");
        fs::create_dir_all(&corpus)
            .map_err(|error| format!("create release fuzz corpus {corpus}: {error}"))?;
        let seed = format!("fuzz/seeds/{target}");
        let mut owned_args = vec![
            "run".to_string(),
            PINNED_NIGHTLY_TOOLCHAIN.to_string(),
            "cargo".to_string(),
            "fuzz".to_string(),
            "run".to_string(),
            "--sanitizer".to_string(),
            "address".to_string(),
            target.to_string(),
            corpus,
        ];
        if Path::new(&seed).is_dir() {
            owned_args.push(seed);
        }
        owned_args.extend([
            "--".to_string(),
            "-max_total_time=300".to_string(),
            "-timeout=10".to_string(),
            "-rss_limit_mb=2048".to_string(),
        ]);
        let args = owned_args.iter().map(String::as_str).collect::<Vec<_>>();
        run_program(OsString::from("rustup"), &args, &[])
            .map_err(|error| format!("release fuzz campaign failed for {target}: {error}"))?;
    }
    Ok(())
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
    coverage_with_corpus(
        std::env::var("WSI_RS_PARITY_ALIASES").is_ok_and(|aliases| !aliases.trim().is_empty()),
    )
}

fn coverage_with_corpus(include_corpus: bool) -> Result<(), String> {
    // Do not narrow this to `--lib --tests`: xtask, the OpenSlide shim, and the
    // performance worker all contain production binary code with unit tests.
    run_cargo(COVERAGE_BASE_ARGS)?;

    if include_corpus {
        let report = [
            "--no-clean",
            "--lcov",
            "--output-path",
            "target/coverage-corpus-step.lcov",
            "--locked",
        ];
        let corpus_runs: &[&[&str]] = &[
            &[
                "--test",
                "openslide_parity",
                "--features",
                "parity-openslide",
                "--",
                "preflight",
                "--exact",
                "--ignored",
            ],
            &[
                "--test",
                "j2k_parity",
                "--",
                "j2k_cpu_vs_reference_within_tolerance",
                "--exact",
                "--ignored",
            ],
            &[
                "--test",
                "dicom_parity",
                "--",
                "dicom_public_corpus_decodes_with_wsi_rs",
                "--exact",
                "--ignored",
            ],
            &[
                "--test",
                "dicom_parity",
                "--features",
                "parity-openslide",
                "--",
                "dicom_public_corpus_matches_openslide_within_tolerance",
                "--exact",
                "--ignored",
            ],
            &["--test", "real_wsi_behavior", "--", "--ignored"],
        ];
        for run in corpus_runs {
            let mut args = vec!["llvm-cov"];
            args.extend(report);
            args.extend(*run);
            run_cargo(&args)?;
        }
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
