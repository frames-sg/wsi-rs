use std::{env, ffi::OsString, fs, path::Path};

use super::coverage;
use super::process::{
    ensure_clean_worktree, run_cargo, run_cargo_with_env, run_program, run_program_capture,
};

const PUBLIC_API_SNAPSHOT_PATH: &str = "api/wsi-rs-public-api.txt";
const PUBLIC_API_CUDA_SNAPSHOT_PATH: &str = "api/wsi-rs-public-api-cuda.txt";
const PUBLIC_API_METAL_SNAPSHOT_PATH: &str = "api/wsi-rs-public-api-metal.txt";
const PINNED_NIGHTLY_TOOLCHAIN: &str = "nightly-2026-04-17";

pub(super) fn ci() -> Result<(), String> {
    validate()?;
    package()
}

pub(super) fn rc_preflight() -> Result<(), String> {
    api_check()?;
    deps()?;
    fuzz_check()?;
    feature_check()?;
    validate()?;
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
    ])
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
    for target in [
        "open_wsi_bytes",
        "open_jp2k_codestream_bytes",
        "open_svcache_bytes",
        "parse_xml_bytes",
        "open_dicom_bytes",
        "open_zvi_bytes",
        "open_mirax_bundle_bytes",
    ] {
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
    // Do not narrow this to `--lib --tests`: xtask, the OpenSlide shim, and the
    // performance worker all contain production binary code with unit tests.
    run_cargo(COVERAGE_BASE_ARGS)?;

    if std::env::var("WSI_RS_PARITY_ALIASES").is_ok_and(|aliases| !aliases.trim().is_empty()) {
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
