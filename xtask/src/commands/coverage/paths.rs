#[cfg(test)]
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

pub(super) fn git_repo_root() -> Result<PathBuf, String> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|err| format!("failed to start `git rev-parse --show-toplevel`: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "`git rev-parse --show-toplevel` exited with {}",
            output.status
        ));
    }
    Ok(PathBuf::from(
        String::from_utf8_lossy(&output.stdout).trim(),
    ))
}

pub(super) fn path_matches_root(path: &Path, root: &str) -> bool {
    path.starts_with(root) || path == Path::new(root).with_extension("rs")
}

pub(super) fn is_production_coverage_path(path: &Path) -> bool {
    const PORTABLE_EXCLUSIONS: &[&str] = &[
        "src/decode/jp2k/cuda",
        "src/decode/jp2k/device",
        "src/decode/jp2k/metal",
        "src/formats/dicom/reader/device",
        "src/output/cuda",
        "src/output/metal",
    ];

    is_workspace_coverage_path(path)
        && !PORTABLE_EXCLUSIONS
            .iter()
            .any(|root| path_matches_root(path, root))
}

pub(super) fn is_workspace_coverage_path(path: &Path) -> bool {
    const WORKSPACE_SOURCE_ROOTS: &[&str] = &[
        "src",
        "wsi-rs-openslide-shim/src",
        "xtask/src",
        "perf-runner/src",
    ];

    is_coverage_candidate(path)
        && WORKSPACE_SOURCE_ROOTS
            .iter()
            .any(|root| path.starts_with(root))
}

pub(super) fn is_coverage_candidate(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "rs")
        && path
            .file_name()
            .is_none_or(|name| name != "tests.rs" && name != "test_support.rs")
        && !path
            .components()
            .any(|component| component.as_os_str() == "tests")
        && !path.starts_with("tests")
        && !path.starts_with("benches")
}

#[cfg(test)]
pub(super) fn collect_git_paths(
    paths: &mut BTreeSet<PathBuf>,
    args: &[&str],
) -> Result<(), String> {
    let output = Command::new("git")
        .args(args)
        .output()
        .map_err(|err| format!("failed to start `git {}`: {err}", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!(
            "`git {}` exited with {}",
            args.join(" "),
            output.status
        ));
    }
    paths.extend(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(PathBuf::from),
    );
    Ok(())
}
