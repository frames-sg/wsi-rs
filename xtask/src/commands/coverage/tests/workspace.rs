use super::*;

const REQUIRED_TIFF_COMPONENTS: [(&str, &str); 7] = [
    ("generic TIFF", "src/formats/tiff_family/layout/generic"),
    ("Aperio", "src/formats/tiff_family/layout/aperio"),
    ("NDPI", "src/formats/tiff_family/layout/ndpi"),
    ("Leica", "src/formats/tiff_family/layout/leica"),
    ("Philips", "src/formats/tiff_family/layout/philips"),
    ("Trestle", "src/formats/tiff_family/layout/trestle"),
    ("Ventana", "src/formats/tiff_family/layout/ventana"),
];

fn complete_coverage() -> HashMap<PathBuf, FileCoverage> {
    REQUIRED_COMPONENTS
        .iter()
        .map(|component| {
            let root = component.root;
            let path = if root.ends_with("/src") {
                PathBuf::from(root).join("lib.rs")
            } else {
                PathBuf::from(format!("{root}.rs"))
            };
            (
                path,
                FileCoverage {
                    lines: (1..=10).map(|line| (line, 1)).collect(),
                    functions: (1..=10).map(|line| (line, true)).collect(),
                },
            )
        })
        .collect()
}

fn complete_lcov() -> String {
    let mut lcov = String::new();
    for (path, file) in complete_coverage() {
        lcov.push_str(&format!("SF:{}\n", path.display()));
        for (line, hit) in file.functions {
            let name = format!("function_{line}");
            lcov.push_str(&format!("FN:{line},{name}\n"));
            lcov.push_str(&format!("FNDA:{},{name}\n", u8::from(hit)));
        }
        for (line, count) in file.lines {
            lcov.push_str(&format!("DA:{line},{count}\n"));
        }
        lcov.push_str("end_of_record\n");
    }
    lcov
}

#[test]
fn required_components_gate_each_tiff_backend() {
    for (name, root) in REQUIRED_TIFF_COMPONENTS {
        let component = REQUIRED_COMPONENTS
            .iter()
            .find(|component| component.name == name)
            .unwrap_or_else(|| panic!("missing required {name} coverage component"));
        assert_eq!(component.root, root);
    }
}

#[test]
fn workspace_gate_applies_each_tiff_backend_floor_independently() {
    for (name, root) in REQUIRED_TIFF_COMPONENTS {
        let mut coverage = complete_coverage();
        let path = PathBuf::from(format!("{root}.rs"));
        let backend = coverage.get_mut(&path).unwrap();
        for count in backend.lines.values_mut().take(4) {
            *count = 0;
        }
        for hit in backend.functions.values_mut().take(4) {
            *hit = false;
        }

        let error = validate_workspace_coverage(&coverage, 80.0, 70.0).unwrap_err();
        assert!(error.contains(&format!(
            "{name} line coverage 60.00% is below required 70.00%"
        )));
        assert!(error.contains(&format!(
            "{name} function coverage 60.00% is below required 70.00%"
        )));
        assert!(!error.contains("TIFF family line coverage"));
        assert!(!error.contains("TIFF family function coverage"));
    }
}

#[test]
fn workspace_gate_enforces_global_and_component_line_and_function_floors() {
    let mut coverage = complete_coverage();

    assert!(validate_workspace_coverage(&coverage, 80.0, 70.0).is_ok());

    let decode = coverage.get_mut(Path::new("src/decode.rs")).unwrap();
    for count in decode.lines.values_mut().take(4) {
        *count = 0;
    }
    for hit in decode.functions.values_mut().take(4) {
        *hit = false;
    }
    let error = validate_workspace_coverage(&coverage, 80.0, 70.0).unwrap_err();
    assert!(error.contains("decode line coverage 60.00% is below required 70.00%"));
    assert!(error.contains("decode function coverage 60.00% is below required 70.00%"));
}

#[test]
fn workspace_gate_reads_lcov_and_prints_every_required_component() {
    let lcov = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(lcov.path(), complete_lcov()).unwrap();

    assert!(enforce_workspace(lcov.path()).is_ok());
}

#[test]
fn workspace_totals_exclude_tests_external_sources_and_device_only_files() {
    let mut coverage = complete_coverage();
    for path in [
        "tests/integration.rs",
        "/registry/dependency/src/lib.rs",
        "src/output/metal.rs",
        "src/output/cuda.rs",
        "src/formats/tiff_family/test_support.rs",
    ] {
        coverage.insert(
            path.into(),
            FileCoverage {
                lines: BTreeMap::from([(1, 0)]),
                functions: BTreeMap::from([(1, false)]),
            },
        );
    }

    let totals = workspace_coverage(&coverage);
    assert_eq!(totals.lines_found, REQUIRED_COMPONENTS.len() as u64 * 10);
    assert_eq!(totals.lines_hit, totals.lines_found);
    assert_eq!(totals.functions_hit, totals.functions_found);
    assert_eq!(percent(0, 0), 0.0);
}

#[test]
fn workspace_gate_fails_when_a_required_component_is_absent() {
    let coverage = HashMap::from([(
        PathBuf::from("src/core.rs"),
        FileCoverage {
            lines: BTreeMap::from([(1, 1)]),
            functions: BTreeMap::from([(1, true)]),
        },
    )]);

    let error = validate_workspace_coverage(&coverage, 80.0, 70.0).unwrap_err();
    assert!(error.contains("decode has no instrumented production lines"));
    assert!(error.contains("OpenSlide shim has no instrumented production functions"));
    assert!(error.contains("performance runner has no instrumented production lines"));
}
