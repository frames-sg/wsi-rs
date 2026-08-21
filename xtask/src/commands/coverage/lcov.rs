use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct FileCoverage {
    pub(super) lines: BTreeMap<u32, u64>,
    pub(super) functions: BTreeMap<u32, bool>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct CoverageTotals {
    pub(super) lines_found: u64,
    pub(super) lines_hit: u64,
    pub(super) functions_found: u64,
    pub(super) functions_hit: u64,
}

impl CoverageTotals {
    pub(super) fn line_percent(self) -> f64 {
        percent(self.lines_hit, self.lines_found)
    }

    pub(super) fn function_percent(self) -> f64 {
        percent(self.functions_hit, self.functions_found)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct LcovRecord {
    lines: BTreeMap<u32, u64>,
    function_hits: BTreeMap<String, u64>,
    function_lines: BTreeMap<String, u32>,
}

pub(super) fn read_lcov(
    path: &Path,
    repo_root: &Path,
) -> Result<HashMap<PathBuf, FileCoverage>, String> {
    let contents = std::fs::read_to_string(path)
        .map_err(|err| format!("failed to read LCOV file {}: {err}", path.display()))?;
    parse_lcov(&contents, repo_root)
}

pub(super) fn parse_lcov(
    contents: &str,
    repo_root: &Path,
) -> Result<HashMap<PathBuf, FileCoverage>, String> {
    let mut files = HashMap::new();
    let mut current_path: Option<PathBuf> = None;
    let mut current = LcovRecord::default();

    for line in contents.lines() {
        if let Some(path) = line.strip_prefix("SF:") {
            flush_lcov_record(&mut files, &mut current_path, &mut current)?;
            current_path = Some(normalize_lcov_path(Path::new(path), repo_root));
        } else if let Some(data) = line.strip_prefix("FN:") {
            let Some((source_line, name)) = data.split_once(',') else {
                return Err(format!("invalid LCOV FN record `{line}`"));
            };
            let source_line = source_line
                .parse::<u32>()
                .map_err(|err| format!("invalid LCOV function line in `{line}`: {err}"))?;
            current.function_hits.entry(name.to_string()).or_default();
            current.function_lines.insert(name.to_string(), source_line);
        } else if let Some(data) = line.strip_prefix("FNDA:") {
            let Some((count, name)) = data.split_once(',') else {
                return Err(format!("invalid LCOV FNDA record `{line}`"));
            };
            let count = count
                .parse::<u64>()
                .map_err(|err| format!("invalid LCOV function hit count in `{line}`: {err}"))?;
            let entry = current.function_hits.entry(name.to_string()).or_default();
            *entry = entry.saturating_add(count);
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
            flush_lcov_record(&mut files, &mut current_path, &mut current)?;
        }
    }
    flush_lcov_record(&mut files, &mut current_path, &mut current)?;
    Ok(files)
}

fn flush_lcov_record(
    files: &mut HashMap<PathBuf, FileCoverage>,
    current_path: &mut Option<PathBuf>,
    current: &mut LcovRecord,
) -> Result<(), String> {
    if let Some(path) = current_path.take() {
        let functions = record_function_coverage(current, &path)?;
        files
            .entry(path)
            .and_modify(|existing| {
                for (line, count) in &current.lines {
                    let entry = existing.lines.entry(*line).or_default();
                    *entry = entry.saturating_add(*count);
                }
                for (line, hit) in &functions {
                    let entry = existing.functions.entry(*line).or_default();
                    *entry |= *hit;
                }
            })
            .or_insert_with(|| FileCoverage {
                lines: current.lines.clone(),
                functions,
            });
    }
    *current = LcovRecord::default();
    Ok(())
}

fn record_function_coverage(
    record: &LcovRecord,
    path: &Path,
) -> Result<BTreeMap<u32, bool>, String> {
    let mut functions = BTreeMap::<u32, bool>::new();
    for (name, count) in &record.function_hits {
        let line = record.function_lines.get(name).ok_or_else(|| {
            format!(
                "LCOV function `{name}` in {} is missing FN source line",
                path.display()
            )
        })?;
        let hit = functions.entry(*line).or_default();
        // LLVM emits separate symbols for the same source function when a workspace
        // compiles it in multiple crates, test harnesses, or monomorphizations. The
        // symbol hashes are unstable, but the defining source line is stable.
        *hit |= *count > 0;
    }
    Ok(functions)
}

pub(super) fn normalize_lcov_path(path: &Path, repo_root: &Path) -> PathBuf {
    if path.is_absolute() {
        path.strip_prefix(repo_root).unwrap_or(path).to_path_buf()
    } else {
        path.to_path_buf()
    }
}

pub(super) fn percent(hit: u64, found: u64) -> f64 {
    if found == 0 {
        0.0
    } else {
        hit as f64 * 100.0 / found as f64
    }
}
