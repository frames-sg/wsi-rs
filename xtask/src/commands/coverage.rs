use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;

const DEFAULT_CHANGED_PATH_COVERAGE_THRESHOLD: f64 = 80.0;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct FileCoverage {
    lines: BTreeMap<u32, u64>,
}

pub(super) fn changed(args: Vec<String>) -> Result<(), String> {
    let options = ChangedCoverageOptions::parse(args)?;
    let repo_root = git_repo_root()?;
    let base_fingerprints = base_rust_line_fingerprints(&options.base)?;
    let changed_lines = changed_rust_lines(&options.base, &base_fingerprints)?;
    if changed_lines.is_empty() {
        println!("no changed Rust source lines found for coverage gate");
        return Ok(());
    }

    let lcov = std::fs::read_to_string(&options.lcov_path).map_err(|err| {
        format!(
            "failed to read LCOV file {}: {err}",
            options.lcov_path.display()
        )
    })?;
    let coverage = parse_lcov(&lcov, &repo_root)?;
    let summary = summarize_changed_coverage(&coverage, &changed_lines);
    if !summary.missing_files.is_empty() {
        eprintln!(
            "skipping changed Rust path(s) absent from LCOV, likely no instrumented lines: {}",
            summary
                .missing_files
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if summary.found == 0 {
        println!("changed Rust paths had no instrumented lines");
        return Ok(());
    }

    let percent = summary.percent();
    println!(
        "changed-path coverage: {:.2}% ({}/{} lines) across {} file(s)",
        percent,
        summary.hit,
        summary.found,
        changed_lines.len()
    );
    if percent + f64::EPSILON < options.threshold {
        return Err(format!(
            "changed-path coverage {:.2}% is below required {:.2}%",
            percent, options.threshold
        ));
    }
    Ok(())
}

#[derive(Debug, PartialEq)]
struct ChangedCoverageOptions {
    base: String,
    lcov_path: PathBuf,
    threshold: f64,
}

impl ChangedCoverageOptions {
    fn parse(args: Vec<String>) -> Result<Self, String> {
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
struct ChangedCoverageSummary {
    found: u64,
    hit: u64,
    missing_files: Vec<PathBuf>,
}

impl ChangedCoverageSummary {
    fn percent(&self) -> f64 {
        if self.found == 0 {
            100.0
        } else {
            self.hit as f64 * 100.0 / self.found as f64
        }
    }
}

fn summarize_changed_coverage(
    coverage: &HashMap<PathBuf, FileCoverage>,
    changed_lines: &HashMap<PathBuf, BTreeSet<u32>>,
) -> ChangedCoverageSummary {
    let mut summary = ChangedCoverageSummary::default();
    for (path, lines) in changed_lines {
        match coverage.get(path) {
            Some(file) => {
                for line in lines {
                    if let Some(count) = file.lines.get(line) {
                        summary.found += 1;
                        if *count > 0 {
                            summary.hit += 1;
                        }
                    }
                }
            }
            None => summary.missing_files.push(path.clone()),
        }
    }
    summary
}

fn parse_lcov(contents: &str, repo_root: &Path) -> Result<HashMap<PathBuf, FileCoverage>, String> {
    let mut files = HashMap::new();
    let mut current_path: Option<PathBuf> = None;
    let mut current = FileCoverage::default();

    for line in contents.lines() {
        if let Some(path) = line.strip_prefix("SF:") {
            flush_lcov_record(&mut files, &mut current_path, &mut current);
            current_path = Some(normalize_lcov_path(Path::new(path), repo_root));
        } else if let Some(data) = line.strip_prefix("DA:") {
            let Some((line_no, count)) = data.split_once(',') else {
                return Err(format!("invalid LCOV DA record `{line}`"));
            };
            let line_no = line_no
                .parse::<u32>()
                .map_err(|err| format!("invalid LCOV line number in `{line}`: {err}"))?;
            let count = count
                .split(',')
                .next()
                .ok_or_else(|| format!("invalid LCOV DA count `{line}`"))?
                .parse::<u64>()
                .map_err(|err| format!("invalid LCOV hit count in `{line}`: {err}"))?;
            current.lines.insert(line_no, count);
        } else if line == "end_of_record" {
            flush_lcov_record(&mut files, &mut current_path, &mut current);
        }
    }
    flush_lcov_record(&mut files, &mut current_path, &mut current);
    Ok(files)
}

fn flush_lcov_record(
    files: &mut HashMap<PathBuf, FileCoverage>,
    current_path: &mut Option<PathBuf>,
    current: &mut FileCoverage,
) {
    if let Some(path) = current_path.take() {
        files
            .entry(path)
            .and_modify(|existing| {
                for (line, count) in &current.lines {
                    existing.lines.insert(*line, *count);
                }
            })
            .or_insert_with(|| current.clone());
    }
    *current = FileCoverage::default();
}

fn normalize_lcov_path(path: &Path, repo_root: &Path) -> PathBuf {
    if path.is_absolute() {
        path.strip_prefix(repo_root).unwrap_or(path).to_path_buf()
    } else {
        path.to_path_buf()
    }
}

fn git_repo_root() -> Result<PathBuf, String> {
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

fn changed_rust_lines(
    base: &str,
    base_fingerprints: &BTreeSet<String>,
) -> Result<HashMap<PathBuf, BTreeSet<u32>>, String> {
    let range = format!("{base}...HEAD");
    let mut lines = HashMap::new();
    collect_git_diff_lines(
        &mut lines,
        base_fingerprints,
        &["diff", "--name-only", "--diff-filter=ACMR", &range],
    )?;
    collect_git_diff_lines(
        &mut lines,
        base_fingerprints,
        &["diff", "--cached", "--name-only", "--diff-filter=ACMR"],
    )?;
    collect_git_diff_lines(
        &mut lines,
        base_fingerprints,
        &["diff", "--name-only", "--diff-filter=ACMR"],
    )?;
    for path in untracked_rust_paths()? {
        add_file_lines(&mut lines, &path, base_fingerprints)?;
    }
    Ok(lines)
}

fn is_coverage_candidate(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "rs")
        && path.file_name().is_none_or(|name| name != "tests.rs")
        && !path.starts_with("tests")
        && !path.starts_with("benches")
        && !path.starts_with("xtask")
}

fn collect_git_diff_lines(
    lines: &mut HashMap<PathBuf, BTreeSet<u32>>,
    base_fingerprints: &BTreeSet<String>,
    name_args: &[&str],
) -> Result<(), String> {
    let mut args = name_args.to_vec();
    if let Some(position) = args.iter().position(|arg| *arg == "--name-only") {
        args[position] = "--unified=0";
    }
    let output = Command::new("git")
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
    parse_diff_added_lines(
        lines,
        &String::from_utf8_lossy(&output.stdout),
        base_fingerprints,
    );
    Ok(())
}

fn parse_diff_added_lines(
    lines: &mut HashMap<PathBuf, BTreeSet<u32>>,
    diff: &str,
    base_fingerprints: &BTreeSet<String>,
) {
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
            let added = &line[1..];
            if !is_base_line(added, base_fingerprints) {
                lines.entry(path.clone()).or_default().insert(*line_no);
            }
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

fn untracked_rust_paths() -> Result<Vec<PathBuf>, String> {
    let mut paths = BTreeSet::new();
    collect_git_paths(&mut paths, &["ls-files", "--others", "--exclude-standard"])?;
    Ok(paths
        .into_iter()
        .filter(|path| is_coverage_candidate(path))
        .collect())
}

fn add_file_lines(
    lines: &mut HashMap<PathBuf, BTreeSet<u32>>,
    path: &Path,
    base_fingerprints: &BTreeSet<String>,
) -> Result<(), String> {
    let contents = std::fs::read_to_string(path)
        .map_err(|err| format!("failed to read changed file {}: {err}", path.display()))?;
    for (index, line) in contents.lines().enumerate() {
        if !is_base_line(line, base_fingerprints) {
            lines
                .entry(path.to_path_buf())
                .or_default()
                .insert(index as u32 + 1);
        }
    }
    Ok(())
}

fn is_base_line(line: &str, base_fingerprints: &BTreeSet<String>) -> bool {
    let normalized = normalize_source_line(line);
    normalized.is_empty() || base_fingerprints.contains(&normalized)
}

fn normalize_source_line(line: &str) -> String {
    line.trim().to_string()
}

fn base_rust_line_fingerprints(base: &str) -> Result<BTreeSet<String>, String> {
    let output = Command::new("git")
        .args(["ls-tree", "-r", "--name-only", base])
        .output()
        .map_err(|err| format!("failed to start `git ls-tree` for {base}: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "`git ls-tree -r --name-only {base}` exited with {}",
            output.status
        ));
    }

    let mut fingerprints = BTreeSet::new();
    for path in String::from_utf8_lossy(&output.stdout).lines() {
        let path = Path::new(path);
        if !is_coverage_candidate(path) {
            continue;
        }
        let spec = format!("{base}:{}", path.display());
        let output = Command::new("git")
            .args(["show", &spec])
            .output()
            .map_err(|err| format!("failed to start `git show {spec}`: {err}"))?;
        if !output.status.success() {
            continue;
        }
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let normalized = normalize_source_line(line);
            if !normalized.is_empty() {
                fingerprints.insert(normalized);
            }
        }
    }
    Ok(fingerprints)
}

fn collect_git_paths(paths: &mut BTreeSet<PathBuf>, args: &[&str]) -> Result<(), String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_lcov_counts_hit_and_found_lines() {
        let root = Path::new("/repo");
        let lcov = "\
SF:/repo/src/lib.rs
DA:1,1
DA:2,0
end_of_record
SF:/repo/src/main.rs
DA:10,3
end_of_record
";

        let parsed = parse_lcov(lcov, root).unwrap();

        assert_eq!(
            parsed.get(Path::new("src/lib.rs")),
            Some(&FileCoverage {
                lines: BTreeMap::from([(1, 1), (2, 0)])
            })
        );
        assert_eq!(
            parsed.get(Path::new("src/main.rs")),
            Some(&FileCoverage {
                lines: BTreeMap::from([(10, 3)])
            })
        );
    }

    #[test]
    fn summarize_changed_coverage_reports_missing_files() {
        let coverage = HashMap::from([(
            PathBuf::from("src/lib.rs"),
            FileCoverage {
                lines: BTreeMap::from([(1, 1), (2, 1), (3, 0)]),
            },
        )]);
        let summary = summarize_changed_coverage(
            &coverage,
            &HashMap::from([
                (PathBuf::from("src/lib.rs"), BTreeSet::from([1, 3])),
                (PathBuf::from("src/missing.rs"), BTreeSet::from([1])),
            ]),
        );

        assert_eq!(summary.found, 2);
        assert_eq!(summary.hit, 1);
        assert_eq!(summary.percent(), 50.0);
        assert_eq!(summary.missing_files, vec![PathBuf::from("src/missing.rs")]);
    }

    #[test]
    fn parse_diff_added_lines_skips_lines_seen_in_base() {
        let diff = "\
diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -10,0 +11,3 @@
+let moved = true;
+let new_line = true;
+}
";
        let mut lines = HashMap::new();
        parse_diff_added_lines(
            &mut lines,
            diff,
            &BTreeSet::from(["let moved = true;".to_string(), "}".to_string()]),
        );

        assert_eq!(
            lines,
            HashMap::from([(PathBuf::from("src/lib.rs"), BTreeSet::from([12]))])
        );
    }

    #[test]
    fn coverage_candidates_skip_test_harness_paths() {
        assert!(is_coverage_candidate(Path::new("src/lib.rs")));
        assert!(is_coverage_candidate(Path::new(
            "wsi-rs-openslide-shim/src/lib.rs"
        )));
        assert!(!is_coverage_candidate(Path::new(
            "src/formats/foo/tests.rs"
        )));
        assert!(!is_coverage_candidate(Path::new("tests/integration.rs")));
        assert!(!is_coverage_candidate(Path::new("benches/read_paths.rs")));
        assert!(!is_coverage_candidate(Path::new(
            "xtask/src/commands/perf.rs"
        )));
    }

    #[test]
    fn options_parse_overrides_defaults() {
        let options = ChangedCoverageOptions::parse(vec![
            "--base".into(),
            "origin/dev".into(),
            "--lcov".into(),
            "coverage/lcov.info".into(),
            "--threshold".into(),
            "85.5".into(),
        ])
        .unwrap();

        assert_eq!(
            options,
            ChangedCoverageOptions {
                base: "origin/dev".into(),
                lcov_path: PathBuf::from("coverage/lcov.info"),
                threshold: 85.5,
            }
        );
    }
}
