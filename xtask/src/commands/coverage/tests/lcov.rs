use super::*;

#[test]
fn parse_lcov_counts_hit_and_found_lines() {
    let root = Path::new("/repo");
    let lcov = "\
SF:/repo/src/lib.rs
FN:1,_Rfirst
FN:2,_Rsecond
FNDA:2,_Rfirst
FNDA:0,_Rsecond
DA:1,1
DA:2,0
end_of_record
SF:/repo/src/main.rs
FN:10,_Rmain
FNDA:3,_Rmain
DA:10,3
end_of_record
";

    let parsed = parse_lcov(lcov, root).unwrap();

    assert_eq!(
        parsed.get(Path::new("src/lib.rs")),
        Some(&FileCoverage {
            lines: BTreeMap::from([(1, 1), (2, 0)]),
            functions: BTreeMap::from([(1, true), (2, false)]),
        })
    );
    assert_eq!(
        parsed.get(Path::new("src/main.rs")),
        Some(&FileCoverage {
            lines: BTreeMap::from([(10, 3)]),
            functions: BTreeMap::from([(10, true)]),
        })
    );
}

#[test]
fn lcov_parser_rejects_malformed_function_and_line_records() {
    for (lcov, expected) in [
        (
            "SF:src/lib.rs\nFN:missing-comma\n",
            "invalid LCOV FN record",
        ),
        (
            "SF:src/lib.rs\nFNDA:missing-comma\n",
            "invalid LCOV FNDA record",
        ),
        (
            "SF:src/lib.rs\nFNDA:not-a-count,function\n",
            "invalid LCOV function hit count",
        ),
        (
            "SF:src/lib.rs\nFNDA:1,missing-definition\nend_of_record\n",
            "missing FN source line",
        ),
        (
            "SF:src/lib.rs\nDA:missing-comma\n",
            "invalid LCOV DA record",
        ),
        (
            "SF:src/lib.rs\nDA:not-a-line,1\n",
            "invalid LCOV line number",
        ),
        (
            "SF:src/lib.rs\nDA:1,not-a-count\n",
            "invalid LCOV hit count",
        ),
    ] {
        assert!(parse_lcov(lcov, Path::new("/repo"))
            .unwrap_err()
            .contains(expected));
    }
}

#[test]
fn lcov_parser_merges_repeated_records_without_losing_hits() {
    let parsed = parse_lcov(
        "SF:src/lib.rs\n\
         FN:1,function_hash_a\nFNDA:1,function_hash_a\n\
         FN:1,function_hash_b\nFNDA:0,function_hash_b\n\
         DA:1,1\nend_of_record\n\
         SF:src/lib.rs\nFN:1,function_hash_c\nFNDA:0,function_hash_c\nDA:1,0\nend_of_record\n",
        Path::new("/repo"),
    )
    .unwrap();
    assert_eq!(parsed[Path::new("src/lib.rs")].lines[&1], 1);
    assert!(parsed[Path::new("src/lib.rs")].functions[&1]);
    assert_eq!(
        normalize_lcov_path(Path::new("/outside/src/lib.rs"), Path::new("/repo")),
        PathBuf::from("/outside/src/lib.rs")
    );
}
