use std::{env, ffi::OsString, fs, path::Path};

use super::process::{
    ensure_clean_worktree, run_cargo, run_cargo_capture, run_cargo_with_env, run_program,
};

const PUBLIC_API_SNAPSHOT_PATH: &str = "api/wsi-rs-public-api.txt";
const PUBLIC_API_CUDA_SNAPSHOT_PATH: &str = "api/wsi-rs-public-api-cuda.txt";
const PUBLIC_API_METAL_SNAPSHOT_PATH: &str = "api/wsi-rs-public-api-metal.txt";
const SEMVER_BASELINE_ROOT_ENV: &str = "WSI_RS_SEMVER_BASELINE_ROOT";

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
    run_cargo(&["clippy", "--all-targets", "--", "-D", "warnings"])
}

pub(super) fn test() -> Result<(), String> {
    run_cargo(&["test", "--lib", "--tests"])?;
    run_cargo(&["test", "--lib", "--tests", "--features", "parity-openslide"])?;
    if cfg!(target_os = "macos") {
        run_cargo(&[
            "test",
            "--lib",
            "--tests",
            "--features",
            "metal",
            "--no-run",
        ])?;
    }
    Ok(())
}

pub(super) fn nextest() -> Result<(), String> {
    run_cargo(&["nextest", "run", "--lib", "--tests"])?;
    run_cargo(&[
        "nextest",
        "run",
        "--lib",
        "--tests",
        "--features",
        "parity-openslide",
    ])?;
    if cfg!(target_os = "macos") {
        run_cargo(&[
            "test",
            "--lib",
            "--tests",
            "--features",
            "metal",
            "--no-run",
        ])?;
    }
    Ok(())
}

pub(super) fn bench_check() -> Result<(), String> {
    run_cargo(&["bench", "--benches", "--no-run"])?;
    run_cargo(&[
        "bench",
        "--benches",
        "--features",
        "parity-openslide",
        "--no-run",
    ])
}

pub(super) fn bench() -> Result<(), String> {
    run_cargo(&[
        "bench",
        "--bench",
        "read_paths",
        "--",
        "synthetic_read_paths",
        "--sample-size",
        "10",
        "--warm-up-time",
        "1",
        "--measurement-time",
        "1",
    ])
}

pub(super) fn feature_check() -> Result<(), String> {
    run_cargo(&[
        "hack",
        "check",
        "--workspace",
        "--all-targets",
        "--feature-powerset",
        "--exclude-features",
        "openslide-bench,metal,parity-metal",
    ])
}

pub(super) fn parity_corpus_test() -> Result<(), String> {
    run_cargo(&[
        "test",
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
        "--test",
        "j2k_parity",
        "j2k_cpu_vs_reference_within_tolerance",
        "--",
        "--exact",
        "--ignored",
    ])?;
    run_cargo(&[
        "test",
        "--test",
        "dicom_parity",
        "dicom_public_corpus_decodes_with_wsi_rs",
        "--",
        "--exact",
        "--ignored",
    ])?;
    run_cargo(&[
        "test",
        "--test",
        "dicom_parity",
        "--features",
        "parity-openslide",
        "dicom_public_corpus_matches_openslide_within_tolerance",
        "--",
        "--exact",
        "--ignored",
    ])?;
    run_cargo(&["test", "--test", "real_wsi_behavior", "--", "--ignored"])
}

pub(super) fn doc() -> Result<(), String> {
    run_cargo_with_env(&["doc", "--no-deps"], &[("RUSTDOCFLAGS", "-D warnings")])
}

pub(super) fn doc_test() -> Result<(), String> {
    run_cargo(&["test", "--doc"])
}

pub(super) fn typos() -> Result<(), String> {
    run_program(OsString::from("typos"), &[], &[])
}

pub(super) fn deny() -> Result<(), String> {
    run_cargo(&["deny", "check", "advisories", "bans", "licenses", "sources"])
}

pub(super) fn unused_deps() -> Result<(), String> {
    run_program(OsString::from("cargo-machete"), &["."], &[])
}

pub(super) fn deps() -> Result<(), String> {
    deny()?;
    unused_deps()
}

pub(super) fn api_check() -> Result<(), String> {
    check_public_api_snapshot_for(
        PUBLIC_API_SNAPSHOT_PATH,
        &["public-api", "-p", "wsi_rs", "-sss", "--color", "never"],
    )?;
    check_public_api_snapshot_for(
        PUBLIC_API_CUDA_SNAPSHOT_PATH,
        &[
            "public-api",
            "-p",
            "wsi_rs",
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
                "wsi_rs",
                "--features",
                "metal",
                "-sss",
                "--color",
                "never",
            ],
        )?;
    }
    run_semver_check(&[])?;
    run_semver_check(&["--features", "cuda"])?;
    if cfg!(target_os = "macos") {
        run_semver_check(&["--features", "metal"])?;
    }
    Ok(())
}

pub(super) fn fuzz_check() -> Result<(), String> {
    for target in [
        "open_wsi_bytes",
        "open_jp2k_codestream_bytes",
        "open_svcache_bytes",
    ] {
        run_program(
            OsString::from("rustup"),
            &["run", "nightly", "cargo", "fuzz", "check", target],
            &[],
        )
        .map_err(|err| {
            format!(
                "{err}\n`cargo xtask fuzz-check` requires nightly Rust and cargo-fuzz; install cargo-fuzz with `cargo install cargo-fuzz` if the command is unavailable"
            )
        })?;
    }
    Ok(())
}

fn check_public_api_snapshot_for(snapshot_path: &str, args: &[&str]) -> Result<(), String> {
    let actual = run_cargo_capture(args).map_err(|err| {
        format!(
            "{err}\n`cargo xtask api-check` requires cargo-public-api; install it with `cargo install cargo-public-api` if the command is unavailable"
        )
    })?;
    check_public_api_snapshot(&actual, snapshot_path)
}

fn run_semver_check(extra_args: &[&str]) -> Result<(), String> {
    let baseline_root = env::var_os(SEMVER_BASELINE_ROOT_ENV);
    let args = semver_check_args(extra_args, baseline_root.as_deref().map(Path::new));
    let args = args.iter().map(String::as_str).collect::<Vec<_>>();
    run_cargo(&args).map_err(|err| {
        format!(
            "{err}\n`cargo xtask api-check` requires cargo-semver-checks; install it with `cargo install cargo-semver-checks` if the command is unavailable"
        )
    })
}

fn semver_check_args(extra_args: &[&str], baseline_root: Option<&Path>) -> Vec<String> {
    let mut args = vec![
        "semver-checks".to_string(),
        "check-release".to_string(),
        "-p".to_string(),
        "wsi_rs".to_string(),
    ];
    args.extend(extra_args.iter().map(|arg| (*arg).to_string()));
    if let Some(baseline_root) = baseline_root {
        args.push("--baseline-root".to_string());
        args.push(baseline_root.display().to_string());
    }
    args
}

fn check_public_api_snapshot(actual: &str, snapshot_path: &str) -> Result<(), String> {
    let snapshot_path = Path::new(snapshot_path);
    let normalized_actual = normalize_snapshot(actual);
    if env::var("WSI_RS_UPDATE_PUBLIC_API").as_deref() == Ok("1") {
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
    run_cargo(&["test", "--lib", "--tests", "--release"])
}

pub(super) fn coverage() -> Result<(), String> {
    run_cargo(&[
        "llvm-cov",
        "--workspace",
        "--lib",
        "--tests",
        "--lcov",
        "--output-path",
        "lcov.info",
    ])
}

pub(super) fn package() -> Result<(), String> {
    ensure_clean_worktree()?;
    run_cargo(&["package", "--locked"])?;
    run_cargo(&["publish", "--dry-run", "--locked"])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn semver_check_args_accept_local_baseline_root() {
        let args = semver_check_args(&["--features", "cuda"], Some(Path::new("target/baseline")));

        assert_eq!(
            args,
            vec![
                "semver-checks",
                "check-release",
                "-p",
                "wsi_rs",
                "--features",
                "cuda",
                "--baseline-root",
                "target/baseline",
            ]
        );
    }
}
