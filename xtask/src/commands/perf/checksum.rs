use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::Value;

use super::schema::{CaptureDocument, CaptureRun};
#[cfg(test)]
use super::PERF_CAPTURE_SCHEMA_VERSION;

const CHECKSUM_CAPTURE_SCHEMA_VERSION: u32 = 5;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ChecksumKey {
    slide_sha256: String,
    alias: String,
    format: String,
    benchmark_group: String,
    workload: String,
    worker_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RequiredCell {
    checksum: ChecksumKey,
    repeat_index: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct DeclaredCell {
    alias: String,
    workload: String,
    worker_count: u64,
    repeat_index: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct GeometryKey {
    slide_sha256: String,
    alias: String,
    worker_count: u64,
    repeat_index: u64,
}

#[cfg(test)]
pub(super) fn validate_worker_run(
    run: &Value,
    expected_engine: &str,
    slide: &Path,
    repeat: u32,
) -> Result<(), String> {
    let run = serde_json::from_value::<CaptureRun>(run.clone())
        .map_err(|error| format!("invalid performance worker JSON: {error}"))?;
    validate_worker_run_typed(&run, expected_engine, slide, repeat)
}

pub(super) fn validate_worker_run_typed(
    run: &CaptureRun,
    expected_engine: &str,
    slide: &Path,
    repeat: u32,
) -> Result<(), String> {
    if run.schema_version != wsi_rs_perf::WORKER_SCHEMA_VERSION {
        return Err(format!(
            "performance worker schema did not match {}",
            wsi_rs_perf::WORKER_SCHEMA_VERSION
        ));
    }
    if run.kind != "wsi-rs-perf-worker" {
        return Err("performance worker returned an unexpected JSON kind".into());
    }
    if run.engine != expected_engine {
        return Err(format!(
            "performance worker engine did not match {expected_engine}"
        ));
    }
    if run.repeat_index != Some(u64::from(repeat)) {
        return Err(format!(
            "performance worker repeat index did not match {repeat}"
        ));
    }
    let expected_slide = slide
        .canonicalize()
        .map_err(|error| format!("failed to canonicalize {}: {error}", slide.display()))?;
    if run.slide_path != expected_slide.to_string_lossy() {
        return Err(format!(
            "performance worker slide path did not match {}",
            expected_slide.display()
        ));
    }
    validate_sha256(run.library_sha256.as_deref(), "library_sha256")?;
    validate_sha256(run.slide_sha256.as_deref(), "slide_sha256")?;
    run_geometry(run)?;
    Ok(())
}

pub(super) fn validate_capture_checksums(capture: &Value) -> Result<(), String> {
    let has_runs = capture.get("runs").and_then(Value::as_array).is_some();
    let capture = CaptureDocument::parse(capture)?;
    if capture.schema_version < CHECKSUM_CAPTURE_SCHEMA_VERSION {
        return Ok(());
    }
    if !has_runs {
        return Err("capture JSON missing runs array".into());
    }
    capture_checksum_map(&capture)?;
    capture_geometry_map(&capture)?;
    Ok(())
}

pub(super) fn validate_cross_capture_checksums(
    before: &Value,
    after: &Value,
) -> Result<(), String> {
    let before = CaptureDocument::parse(before)?;
    let after = CaptureDocument::parse(after)?;
    if [before.schema_version, after.schema_version]
        .into_iter()
        .any(|version| version < CHECKSUM_CAPTURE_SCHEMA_VERSION)
    {
        return Err(format!(
            "cross-engine comparison requires schema_version >= {CHECKSUM_CAPTURE_SCHEMA_VERSION} with checksums"
        ));
    }
    compare_maps(
        required_cell_map(&before)?,
        required_cell_map(&after)?,
        "required cell",
        describe_cell,
        |cell, first, second| {
            format!(
                "output checksum mismatch for {}: {first} != {second}",
                describe_cell(cell)
            )
        },
    )?;
    compare_maps(
        capture_geometry_map(&before)?,
        capture_geometry_map(&after)?,
        "geometry",
        describe_geometry_key,
        |key, _, _| format!("slide geometry mismatch for {}", describe_geometry_key(key)),
    )
}

pub(super) fn validate_declared_capture_plan(capture: &Value) -> Result<(), String> {
    let capture = CaptureDocument::parse(capture)?;
    if capture.slide_manifest.is_empty() {
        return Err("capture JSON missing declared slide_manifest".into());
    }
    let aliases = unique_nonempty(
        capture
            .slide_manifest
            .iter()
            .map(|slide| slide.alias.as_str()),
        "slide_manifest aliases",
    )?;
    let workloads = unique_nonempty(
        capture
            .metadata
            .benchmark
            .planned_workloads
            .iter()
            .map(String::as_str),
        "planned_workloads",
    )?;
    let workers = unique_positive(
        &capture.metadata.benchmark.client_worker_matrix,
        "client_worker_matrix",
    )?;
    if capture.repeat_count == 0 {
        return Err("capture JSON missing positive repeat_count".into());
    }

    let mut expected = BTreeSet::new();
    for alias in aliases {
        for workload in &workloads {
            for &worker_count in &workers {
                for repeat_index in 0..capture.repeat_count {
                    expected.insert(DeclaredCell {
                        alias: alias.clone(),
                        workload: workload.clone(),
                        worker_count,
                        repeat_index,
                    });
                }
            }
        }
    }
    let actual = capture_declared_cells(&capture)?;
    if let Some(cell) = expected.difference(&actual).next() {
        return Err(format!("missing declared {}", describe_declared_cell(cell)));
    }
    if let Some(cell) = actual.difference(&expected).next() {
        return Err(format!("found undeclared {}", describe_declared_cell(cell)));
    }
    Ok(())
}

fn unique_nonempty<'a>(
    values: impl Iterator<Item = &'a str>,
    field: &str,
) -> Result<BTreeSet<String>, String> {
    let values = values.collect::<Vec<_>>();
    let unique = values
        .iter()
        .filter(|value| !value.is_empty())
        .map(|value| (*value).to_string())
        .collect::<BTreeSet<_>>();
    if unique.is_empty() || unique.len() != values.len() {
        return Err(format!(
            "declared {field} entries must be unique and nonempty"
        ));
    }
    Ok(unique)
}

fn unique_positive(values: &[u64], field: &str) -> Result<BTreeSet<u64>, String> {
    let unique = values
        .iter()
        .copied()
        .filter(|value| *value > 0)
        .collect::<BTreeSet<_>>();
    if unique.is_empty() || unique.len() != values.len() {
        return Err(format!(
            "declared {field} entries must be unique positive integers"
        ));
    }
    Ok(unique)
}

fn capture_declared_cells(capture: &CaptureDocument) -> Result<BTreeSet<DeclaredCell>, String> {
    let mut cells = BTreeSet::new();
    for run in &capture.runs {
        let alias = run
            .alias
            .clone()
            .filter(|alias| !alias.is_empty())
            .ok_or_else(|| "run missing alias".to_string())?;
        let worker_count = run
            .worker_count
            .filter(|count| *count > 0)
            .ok_or_else(|| format!("run for alias={alias} missing positive worker_count"))?;
        let repeat_index = run
            .repeat_index
            .ok_or_else(|| format!("run for alias={alias} missing repeat_index"))?;
        if run.workloads.is_empty() {
            return Err(format!("run for alias={alias} missing workloads"));
        }
        for workload in &run.workloads {
            if workload.name.is_empty() {
                return Err("workload missing name".into());
            }
            let cell = DeclaredCell {
                alias: alias.clone(),
                workload: workload.name.clone(),
                worker_count,
                repeat_index,
            };
            if !cells.insert(cell.clone()) {
                return Err(format!(
                    "duplicate declared {}",
                    describe_declared_cell(&cell)
                ));
            }
        }
    }
    Ok(cells)
}

fn capture_checksum_map(
    capture: &CaptureDocument,
) -> Result<BTreeMap<ChecksumKey, String>, String> {
    let mut checksums = BTreeMap::new();
    let mut slide_hashes = BTreeMap::new();
    for run in &capture.runs {
        let slide_sha256 = required(run.slide_sha256.as_deref(), "run missing slide_sha256")?;
        if run.slide_path.is_empty() {
            return Err("run missing slide_path".into());
        }
        if let Some(existing) = slide_hashes.insert(run.slide_path.clone(), slide_sha256.clone()) {
            if existing != slide_sha256 {
                return Err(format!(
                    "slide contents changed between repeats for {}: {existing} != {slide_sha256}",
                    run.slide_path
                ));
            }
        }
        for workload in &run.workloads {
            let checksum = required(
                workload.checksum_sha256.as_deref(),
                "workload missing checksum_sha256",
            )?;
            let key = ChecksumKey {
                slide_sha256: slide_sha256.clone(),
                alias: run.alias().to_string(),
                format: run.format().to_string(),
                benchmark_group: run.benchmark_group().to_string(),
                workload: required(Some(&workload.name), "workload missing name")?,
                worker_count: run.worker_count(),
            };
            if let Some(existing) = checksums.insert(key, checksum.clone()) {
                if existing != checksum {
                    return Err(format!(
                        "nondeterministic output for {} workload {}: {existing} != {checksum}",
                        run.slide_path, workload.name
                    ));
                }
            }
        }
    }
    Ok(checksums)
}

fn required_cell_map(capture: &CaptureDocument) -> Result<BTreeMap<RequiredCell, String>, String> {
    let mut cells = BTreeMap::new();
    for run in &capture.runs {
        let slide_sha256 = required(run.slide_sha256.as_deref(), "run missing slide_sha256")?;
        let repeat_index = run
            .repeat_index
            .ok_or_else(|| format!("run for {} missing repeat_index", run.slide_path))?;
        for workload in &run.workloads {
            let checksum = required(
                workload.checksum_sha256.as_deref(),
                "workload missing checksum_sha256",
            )?;
            let cell = RequiredCell {
                checksum: ChecksumKey {
                    slide_sha256: slide_sha256.clone(),
                    alias: run.alias().to_string(),
                    format: run.format().to_string(),
                    benchmark_group: run.benchmark_group().to_string(),
                    workload: required(Some(&workload.name), "workload missing name")?,
                    worker_count: run.worker_count(),
                },
                repeat_index,
            };
            if cells.insert(cell.clone(), checksum).is_some() {
                return Err(format!("duplicate required {}", describe_cell(&cell)));
            }
        }
    }
    Ok(cells)
}

fn capture_geometry_map(
    capture: &CaptureDocument,
) -> Result<BTreeMap<GeometryKey, String>, String> {
    let mut geometry = BTreeMap::new();
    for run in &capture.runs {
        let key = GeometryKey {
            slide_sha256: required(run.slide_sha256.as_deref(), "run missing slide_sha256")?,
            alias: run.alias().to_string(),
            worker_count: run.worker_count(),
            repeat_index: run
                .repeat_index
                .ok_or_else(|| "run missing repeat_index".to_string())?,
        };
        let signature = run_geometry(run)?;
        if geometry.insert(key.clone(), signature).is_some() {
            return Err(format!(
                "duplicate geometry for {}",
                describe_geometry_key(&key)
            ));
        }
    }
    Ok(geometry)
}

fn run_geometry(run: &CaptureRun) -> Result<String, String> {
    let bounds = run
        .level0_bounds
        .ok_or_else(|| "performance worker result missing level0_bounds".to_string())?;
    if bounds.width == 0 || bounds.height == 0 {
        return Err("performance worker level0_bounds missing positive dimensions".into());
    }
    if run.levels.is_empty() {
        return Err("performance worker result missing nonempty levels".into());
    }
    let mut signature = format!(
        "{}:{}:{}:{}",
        bounds.x, bounds.y, bounds.width, bounds.height
    );
    for (index, level) in run.levels.iter().enumerate() {
        if level.width == 0 || level.height == 0 {
            return Err(format!(
                "performance worker level {index} missing positive dimensions"
            ));
        }
        if !level.downsample.is_finite() || level.downsample <= 0.0 {
            return Err(format!(
                "performance worker level {index} has invalid downsample"
            ));
        }
        signature.push_str(&format!(
            "|{}:{}:{}",
            level.width,
            level.height,
            level.downsample.to_bits()
        ));
    }
    Ok(signature)
}

fn compare_maps<K: Ord, V: PartialEq>(
    first: BTreeMap<K, V>,
    second: BTreeMap<K, V>,
    label: &str,
    describe: impl Fn(&K) -> String,
    mismatch: impl Fn(&K, &V, &V) -> String,
) -> Result<(), String> {
    for (key, first_value) in &first {
        let Some(second_value) = second.get(key) else {
            return Err(format!(
                "missing {label} {} in second capture",
                describe(key)
            ));
        };
        if first_value != second_value {
            return Err(mismatch(key, first_value, second_value));
        }
    }
    if let Some(key) = second.keys().find(|key| !first.contains_key(*key)) {
        return Err(format!(
            "missing {label} {} in first capture",
            describe(key)
        ));
    }
    Ok(())
}

fn required(value: Option<&str>, message: &str) -> Result<String, String> {
    value
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| message.to_string())
}

fn validate_sha256(digest: Option<&str>, field: &str) -> Result<(), String> {
    let digest = digest.ok_or_else(|| format!("performance worker result missing {field}"))?;
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("performance worker result has invalid {field}"));
    }
    Ok(())
}

fn describe_declared_cell(cell: &DeclaredCell) -> String {
    format!(
        "cell alias={} workload={} workers={} repeat={}",
        cell.alias, cell.workload, cell.worker_count, cell.repeat_index
    )
}

fn describe_geometry_key(key: &GeometryKey) -> String {
    format!(
        "alias={} workers={} repeat={}",
        key.alias, key.worker_count, key.repeat_index
    )
}

fn describe_cell(cell: &RequiredCell) -> String {
    format!(
        "cell alias={} format={} group={} workload={} workers={} repeat={}",
        cell.checksum.alias,
        cell.checksum.format,
        cell.checksum.benchmark_group,
        cell.checksum.workload,
        cell.checksum.worker_count,
        cell.repeat_index
    )
}

#[cfg(test)]
#[path = "tests/checksum.rs"]
mod tests;
