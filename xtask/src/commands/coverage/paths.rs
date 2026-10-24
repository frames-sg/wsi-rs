#[cfg(test)]
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::commands::process::git_in;

/// The repository containing the current directory, which coverage gates
/// inspect.
pub(super) fn git_repo_root() -> Result<PathBuf, String> {
    Ok(PathBuf::from(
        git_in(Path::new("."), &["rev-parse", "--show-toplevel"])?.trim(),
    ))
}

pub(super) fn path_matches_root(path: &Path, root: &str) -> bool {
    path.starts_with(root) || path == Path::new(root).with_extension("rs")
}

pub(super) fn is_production_coverage_path(path: &Path) -> bool {
    const PORTABLE_EXCLUSIONS: &[&str] = &[
        // These split modules, like the existing device modules below, are
        // compiled only with Metal/CUDA and cannot appear in CPU-only LCOV.
        "src/core/decode_runtime/adaptive",
        "src/decode/jp2k/prepared_batch",
        "src/decode/jp2k/metal_batch",
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
        && !path.starts_with("fuzz")
}

#[cfg(test)]
pub(super) fn collect_git_paths(
    paths: &mut BTreeSet<PathBuf>,
    args: &[&str],
) -> Result<(), String> {
    paths.extend(git_in(Path::new("."), args)?.lines().map(PathBuf::from));
    Ok(())
}
