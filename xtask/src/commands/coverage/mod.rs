mod changed;
mod lcov;
mod paths;
mod workspace;

use std::path::Path;

use changed::{
    changed_rust_lines, summarize_changed_coverage, validate_changed_coverage,
    ChangedCoverageOptions,
};
use lcov::read_lcov;
use paths::git_repo_root;
use workspace::{
    component_coverage, print_coverage_summary, validate_workspace_coverage, workspace_coverage,
    COMPONENT_COVERAGE_THRESHOLD, REQUIRED_COMPONENTS, WORKSPACE_COVERAGE_THRESHOLD,
};

pub(super) fn enforce_workspace(lcov_path: &Path) -> Result<(), String> {
    let repo_root = git_repo_root()?;
    let coverage = read_lcov(lcov_path, &repo_root)?;

    print_coverage_summary("workspace", workspace_coverage(&coverage));
    for component in REQUIRED_COMPONENTS {
        print_coverage_summary(component.name, component_coverage(&coverage, *component));
    }

    validate_workspace_coverage(
        &coverage,
        WORKSPACE_COVERAGE_THRESHOLD,
        COMPONENT_COVERAGE_THRESHOLD,
    )
}

pub(super) fn changed(args: Vec<String>) -> Result<(), String> {
    let options = ChangedCoverageOptions::parse(args)?;
    let repo_root = git_repo_root()?;
    let changed_lines = changed_rust_lines(&options.base)?;
    if changed_lines.is_empty() {
        println!("no changed Rust source lines found for coverage gate");
        return Ok(());
    }

    let coverage = read_lcov(&options.lcov_path, &repo_root)?;
    let summary = summarize_changed_coverage(&coverage, &changed_lines);
    for (path, (hit, found)) in &summary.files {
        let file_percent = *hit as f64 * 100.0 / *found as f64;
        println!("  {:6.2}% ({hit}/{found}) {}", file_percent, path.display());
    }
    let percent = summary.percent();
    println!(
        "changed-path coverage: {:.2}% ({}/{} lines) across {} file(s)",
        percent,
        summary.hit,
        summary.found,
        changed_lines.len()
    );
    validate_changed_coverage(&summary, options.threshold)
}

#[cfg(test)]
use std::collections::{BTreeMap, BTreeSet, HashMap};
#[cfg(test)]
use std::path::PathBuf;

#[cfg(test)]
use changed::{
    add_file_lines, add_repo_file_lines, changed_lines_are_declaration_only,
    collect_git_diff_lines, parse_diff_added_lines, source_has_function_definition,
    untracked_rust_paths, ChangedCoverageSummary,
};
#[cfg(test)]
use lcov::{normalize_lcov_path, parse_lcov, percent, FileCoverage};
#[cfg(test)]
use paths::{collect_git_paths, is_coverage_candidate};

#[cfg(test)]
mod tests;
