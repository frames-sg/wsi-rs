use std::collections::HashMap;
use std::path::PathBuf;

use super::lcov::{CoverageTotals, FileCoverage};
use super::paths::{is_production_coverage_path, path_matches_root};

pub(super) const WORKSPACE_COVERAGE_THRESHOLD: f64 = 80.0;
pub(super) const COMPONENT_COVERAGE_THRESHOLD: f64 = 70.0;

#[derive(Clone, Copy, Debug)]
pub(super) struct CoverageComponent {
    pub(super) name: &'static str,
    pub(super) root: &'static str,
}

pub(super) const REQUIRED_COMPONENTS: &[CoverageComponent] = &[
    CoverageComponent {
        name: "core",
        root: "src/core",
    },
    CoverageComponent {
        name: "decode",
        root: "src/decode",
    },
    CoverageComponent {
        name: "DICOM",
        root: "src/formats/dicom",
    },
    CoverageComponent {
        name: "Hamamatsu VMS",
        root: "src/formats/hamamatsu_vms",
    },
    CoverageComponent {
        name: "MIRAX",
        root: "src/formats/mirax",
    },
    CoverageComponent {
        name: "Olympus VSI",
        root: "src/formats/olympus_vsi",
    },
    CoverageComponent {
        name: "raw JP2K",
        root: "src/formats/raw_jp2k",
    },
    CoverageComponent {
        name: "svcache",
        root: "src/formats/svcache",
    },
    CoverageComponent {
        name: "TIFF family",
        root: "src/formats/tiff_family",
    },
    CoverageComponent {
        name: "generic TIFF",
        root: "src/formats/tiff_family/layout/generic",
    },
    CoverageComponent {
        name: "Aperio",
        root: "src/formats/tiff_family/layout/aperio",
    },
    CoverageComponent {
        name: "ARGOS",
        root: "src/formats/tiff_family/layout/argos",
    },
    CoverageComponent {
        name: "Huron",
        root: "src/formats/tiff_family/layout/huron",
    },
    CoverageComponent {
        name: "NDPI",
        root: "src/formats/tiff_family/layout/ndpi",
    },
    CoverageComponent {
        name: "Leica",
        root: "src/formats/tiff_family/layout/leica",
    },
    CoverageComponent {
        name: "Philips",
        root: "src/formats/tiff_family/layout/philips",
    },
    CoverageComponent {
        name: "Trestle",
        root: "src/formats/tiff_family/layout/trestle",
    },
    CoverageComponent {
        name: "Ventana",
        root: "src/formats/tiff_family/layout/ventana",
    },
    CoverageComponent {
        name: "Zeiss CZI",
        root: "src/formats/zeiss",
    },
    CoverageComponent {
        name: "Zeiss ZVI",
        root: "src/formats/zeiss_zvi",
    },
    CoverageComponent {
        name: "OpenSlide shim",
        root: "wsi-rs-openslide-shim/src",
    },
    CoverageComponent {
        name: "xtask",
        root: "xtask/src",
    },
    CoverageComponent {
        name: "performance runner",
        root: "perf-runner/src",
    },
];

pub(super) fn validate_workspace_coverage(
    coverage: &HashMap<PathBuf, FileCoverage>,
    workspace_threshold: f64,
    component_threshold: f64,
) -> Result<(), String> {
    let mut failures = Vec::new();
    validate_coverage_totals(
        "workspace",
        workspace_coverage(coverage),
        workspace_threshold,
        &mut failures,
    );
    for component in REQUIRED_COMPONENTS {
        validate_coverage_totals(
            component.name,
            component_coverage(coverage, *component),
            component_threshold,
            &mut failures,
        );
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "coverage gate failed:\n  {}",
            failures.join("\n  ")
        ))
    }
}

pub(super) fn validate_coverage_totals(
    name: &str,
    totals: CoverageTotals,
    threshold: f64,
    failures: &mut Vec<String>,
) {
    if totals.lines_found == 0 {
        failures.push(format!("{name} has no instrumented production lines"));
    } else if totals.line_percent() + f64::EPSILON < threshold {
        failures.push(format!(
            "{name} line coverage {:.2}% is below required {threshold:.2}%",
            totals.line_percent()
        ));
    }

    if totals.functions_found == 0 {
        failures.push(format!("{name} has no instrumented production functions"));
    } else if totals.function_percent() + f64::EPSILON < threshold {
        failures.push(format!(
            "{name} function coverage {:.2}% is below required {threshold:.2}%",
            totals.function_percent()
        ));
    }
}

pub(super) fn workspace_coverage(coverage: &HashMap<PathBuf, FileCoverage>) -> CoverageTotals {
    coverage
        .iter()
        .filter(|(path, _)| is_production_coverage_path(path))
        .fold(CoverageTotals::default(), |totals, (_, file)| {
            add_file_coverage(totals, file)
        })
}

pub(super) fn component_coverage(
    coverage: &HashMap<PathBuf, FileCoverage>,
    component: CoverageComponent,
) -> CoverageTotals {
    coverage
        .iter()
        .filter(|(path, _)| {
            is_production_coverage_path(path) && path_matches_root(path, component.root)
        })
        .fold(CoverageTotals::default(), |totals, (_, file)| {
            add_file_coverage(totals, file)
        })
}

fn add_file_coverage(mut totals: CoverageTotals, file: &FileCoverage) -> CoverageTotals {
    totals.lines_found += file.lines.len() as u64;
    totals.lines_hit += file.lines.values().filter(|count| **count > 0).count() as u64;
    totals.functions_found += file.functions.len() as u64;
    totals.functions_hit += file.functions.values().filter(|hit| **hit).count() as u64;
    totals
}

pub(super) fn print_coverage_summary(name: &str, totals: CoverageTotals) {
    println!(
        "  {name}: lines {:.2}% ({}/{}), functions {:.2}% ({}/{})",
        totals.line_percent(),
        totals.lines_hit,
        totals.lines_found,
        totals.function_percent(),
        totals.functions_hit,
        totals.functions_found
    );
}
