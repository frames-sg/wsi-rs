use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;

use super::lcov::FileCoverage;
use super::paths::{git_repo_root, is_coverage_candidate};

pub(super) const DEFAULT_CHANGED_PATH_COVERAGE_THRESHOLD: f64 = 80.0;

#[derive(Debug, PartialEq)]
pub(super) struct ChangedCoverageOptions {
    pub(super) base: String,
    pub(super) lcov_path: PathBuf,
    pub(super) threshold: f64,
}

impl ChangedCoverageOptions {
    pub(super) fn parse(args: Vec<String>) -> Result<Self, String> {
        let mut base = std::env::var("WSI_RS_COVERAGE_BASE")
            .ok()
            .or_else(|| {
                std::env::var("GITHUB_BASE_REF")
                    .ok()
                    .filter(|value| !value.is_empty())
                    .map(|value| format!("origin/{value}"))
            })
            .unwrap_or_else(|| "origin/main".into());
        let mut lcov_path = PathBuf::from(
            std::env::var_os("WSI_RS_COVERAGE_LCOV").unwrap_or_else(|| "lcov.info".into()),
        );
        let mut threshold = std::env::var("WSI_RS_COVERAGE_THRESHOLD")
            .ok()
            .and_then(|value| value.parse::<f64>().ok())
            .unwrap_or(DEFAULT_CHANGED_PATH_COVERAGE_THRESHOLD);

        let mut iter = args.into_iter();
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--base" => {
                    base = iter
                        .next()
                        .ok_or_else(|| "--base requires a revision".to_string())?;
                }
                "--lcov" => {
                    lcov_path = PathBuf::from(
                        iter.next()
                            .ok_or_else(|| "--lcov requires a file path".to_string())?,
                    );
                }
                "--threshold" => {
                    let value = iter
                        .next()
                        .ok_or_else(|| "--threshold requires a percent".to_string())?;
                    threshold = value
                        .parse::<f64>()
                        .map_err(|err| format!("invalid --threshold value {value}: {err}"))?;
                }
                "-h" | "--help" => {
                    return Err(
                        "usage: cargo xtask coverage-changed [--base REV] [--lcov lcov.info] [--threshold 80]".into(),
                    );
                }
                other => return Err(format!("unknown coverage-changed argument `{other}`")),
            }
        }
        if !(0.0..=100.0).contains(&threshold) {
            return Err(format!(
                "coverage threshold must be between 0 and 100, got {threshold}"
            ));
        }
        Ok(Self {
            base,
            lcov_path,
            threshold,
        })
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct ChangedCoverageSummary {
    pub(super) found: u64,
    pub(super) hit: u64,
    pub(super) missing_files: Vec<PathBuf>,
    pub(super) files: BTreeMap<PathBuf, (u64, u64)>,
}

impl ChangedCoverageSummary {
    pub(super) fn percent(&self) -> f64 {
        super::lcov::percent(self.hit, self.found)
    }
}

pub(super) fn validate_changed_coverage(
    summary: &ChangedCoverageSummary,
    threshold: f64,
) -> Result<(), String> {
    if !summary.missing_files.is_empty() {
        return Err(format!(
            "changed Rust source path(s) absent from LCOV: {}",
            summary
                .missing_files
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if summary.found == 0 {
        return Err("changed Rust source had no instrumented lines in LCOV".into());
    }
    let percent = summary.percent();
    if percent + f64::EPSILON < threshold {
        return Err(format!(
            "changed-path coverage {:.2}% is below required {:.2}%",
            percent, threshold
        ));
    }
    Ok(())
}

pub(super) fn summarize_changed_coverage(
    coverage: &HashMap<PathBuf, FileCoverage>,
    changed_lines: &HashMap<PathBuf, BTreeSet<u32>>,
) -> ChangedCoverageSummary {
    let mut summary = ChangedCoverageSummary::default();
    for (path, lines) in changed_lines {
        match coverage.get(path) {
            Some(file) => {
                for line in lines {
                    if let Some(count) = file.lines.get(line) {
                        let file_summary = summary.files.entry(path.clone()).or_default();
                        summary.found += 1;
                        file_summary.1 += 1;
                        if *count > 0 {
                            summary.hit += 1;
                            file_summary.0 += 1;
                        }
                    }
                }
            }
            None => summary.missing_files.push(path.clone()),
        }
    }
    summary
}

pub(super) fn changed_rust_lines(base: &str) -> Result<HashMap<PathBuf, BTreeSet<u32>>, String> {
    let repo_root = git_repo_root()?;
    let range = format!("{base}...HEAD");
    let mut lines = HashMap::new();
    collect_git_diff_lines(
        &mut lines,
        &repo_root,
        &["diff", "--name-only", "--diff-filter=ACMR", &range],
    )?;
    collect_git_diff_lines(
        &mut lines,
        &repo_root,
        &["diff", "--cached", "--name-only", "--diff-filter=ACMR"],
    )?;
    collect_git_diff_lines(
        &mut lines,
        &repo_root,
        &["diff", "--name-only", "--diff-filter=ACMR"],
    )?;
    for path in untracked_rust_paths(&repo_root)? {
        add_repo_file_lines(&mut lines, &repo_root, &path)?;
    }
    let declaration_only = lines
        .iter()
        .map(|(path, changed_lines)| {
            let source_path = if path.is_absolute() {
                path.clone()
            } else {
                repo_root.join(path)
            };
            std::fs::read_to_string(&source_path)
                .map(|source| {
                    (!source_has_function_definition(&source)
                        || changed_lines_are_declaration_only(&source, changed_lines))
                    .then(|| path.clone())
                })
                .map_err(|err| {
                    format!(
                        "failed to read changed file {}: {err}",
                        source_path.display()
                    )
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    for path in declaration_only.into_iter().flatten() {
        lines.remove(&path);
    }
    Ok(lines)
}

pub(super) fn source_has_function_definition(source: &str) -> bool {
    source.lines().any(source_line_has_function_definition)
}

pub(super) fn changed_lines_are_declaration_only(
    source: &str,
    changed_lines: &BTreeSet<u32>,
) -> bool {
    let source_lines = source.lines().collect::<Vec<_>>();
    !changed_lines.is_empty()
        && changed_lines.iter().all(|line_number| {
            let Some(index) = line_number.checked_sub(1).map(|value| value as usize) else {
                return false;
            };
            source_lines
                .get(index)
                .is_some_and(|line| module_wiring_declaration(line))
        })
}

fn source_line_has_function_definition(line: &str) -> bool {
    line.split_once("//")
        .map_or(line, |(code, _)| code)
        .split(|ch: char| !(ch.is_alphanumeric() || ch == '_'))
        .any(|token| token == "fn")
}

fn module_wiring_declaration(line: &str) -> bool {
    let line = line.trim();
    let declaration = line
        .strip_prefix("pub ")
        .or_else(|| line.strip_prefix("pub(crate) "))
        .or_else(|| line.strip_prefix("pub(super) "))
        .unwrap_or(line);
    declaration.starts_with("mod ") || declaration.starts_with("use ")
}

fn collect_git_diff_lines(
    lines: &mut HashMap<PathBuf, BTreeSet<u32>>,
    repo_root: &Path,
    name_args: &[&str],
) -> Result<(), String> {
    let mut args = name_args.to_vec();
    if let Some(position) = args.iter().position(|arg| *arg == "--name-only") {
        args[position] = "--unified=0";
    }
    let output = Command::new("git")
        .current_dir(repo_root)
        .args(&args)
        .output()
        .map_err(|err| format!("failed to start `git {}`: {err}", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!(
            "`git {}` exited with {}",
            args.join(" "),
            output.status
        ));
    }
    parse_diff_added_lines(lines, &String::from_utf8_lossy(&output.stdout));
    Ok(())
}

pub(super) fn parse_diff_added_lines(lines: &mut HashMap<PathBuf, BTreeSet<u32>>, diff: &str) {
    let mut current_path: Option<PathBuf> = None;
    let mut current_line: Option<u32> = None;
    for line in diff.lines() {
        if let Some(path) = line.strip_prefix("+++ b/") {
            let path = PathBuf::from(path);
            current_path = is_coverage_candidate(&path).then_some(path);
            current_line = None;
            continue;
        }
        if line.starts_with("+++ ") {
            current_path = None;
            current_line = None;
            continue;
        }
        if let Some(hunk) = line.strip_prefix("@@ ") {
            current_line = parse_new_hunk_start(hunk);
            continue;
        }
        let Some(path) = current_path.as_ref() else {
            continue;
        };
        let Some(line_no) = current_line.as_mut() else {
            continue;
        };
        if line.starts_with('+') && !line.starts_with("+++") {
            lines.entry(path.clone()).or_default().insert(*line_no);
            *line_no += 1;
        } else if !line.starts_with('-') {
            *line_no += 1;
        }
    }
}

fn parse_new_hunk_start(hunk: &str) -> Option<u32> {
    let plus = hunk.split_whitespace().find(|part| part.starts_with('+'))?;
    plus.trim_start_matches('+')
        .split(',')
        .next()?
        .parse::<u32>()
        .ok()
}

fn untracked_rust_paths(repo_root: &Path) -> Result<Vec<PathBuf>, String> {
    let args = ["ls-files", "--others", "--exclude-standard"];
    let output = Command::new("git")
        .current_dir(repo_root)
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
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(PathBuf::from)
        .filter(|path| is_coverage_candidate(path))
        .collect())
}

#[cfg(test)]
pub(super) fn add_file_lines(
    lines: &mut HashMap<PathBuf, BTreeSet<u32>>,
    path: &Path,
) -> Result<(), String> {
    add_file_lines_from(lines, path, path)
}

pub(super) fn add_repo_file_lines(
    lines: &mut HashMap<PathBuf, BTreeSet<u32>>,
    repo_root: &Path,
    path: &Path,
) -> Result<(), String> {
    let source_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        repo_root.join(path)
    };
    add_file_lines_from(lines, path, &source_path)
}

fn add_file_lines_from(
    lines: &mut HashMap<PathBuf, BTreeSet<u32>>,
    key: &Path,
    source_path: &Path,
) -> Result<(), String> {
    let contents = std::fs::read_to_string(source_path).map_err(|err| {
        format!(
            "failed to read changed file {}: {err}",
            source_path.display()
        )
    })?;
    for (index, _) in contents.lines().enumerate() {
        lines
            .entry(key.to_path_buf())
            .or_default()
            .insert(index as u32 + 1);
    }
    Ok(())
}
