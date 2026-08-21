use super::*;

#[test]
fn prefer_fresh_surfaces_corrupt_cache_instead_of_falling_back() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.j2c");
    std::fs::write(&source, b"source").unwrap();
    let cache = default_svcache_path(&source);
    std::fs::write(&cache, b"corrupt").unwrap();

    let err = resolve_open_path_with_policy(&source, SvcachePolicy::PreferFresh).unwrap_err();
    assert!(
        err.to_string().contains("I/O error") || err.to_string().contains("svcache"),
        "unexpected corrupt-cache error: {err}"
    );
}

#[test]
fn svcache_probe_distinguishes_truncated_foreign_and_valid_files() {
    let dir = tempfile::tempdir().unwrap();
    let truncated = dir.path().join("truncated.svcache");
    std::fs::write(&truncated, b"short").unwrap();
    assert!(!SvcacheBackend.probe(&truncated).unwrap().detected);

    let foreign = dir.path().join("foreign.svcache");
    std::fs::write(&foreign, b"NOTCACHE").unwrap();
    let negative = SvcacheBackend.probe(&foreign).unwrap();
    assert!(!negative.detected);
    assert_eq!(negative.confidence, ProbeConfidence::Definite);

    let source = tempfile::NamedTempFile::new().unwrap();
    let valid = dir.path().join("valid.svcache");
    write_svcache_file(
        &valid,
        &SvcacheMetadata {
            schema_version: SCHEMA_VERSION,
            complete: true,
            source: fingerprint_source(source.path()).unwrap(),
            properties: Vec::new(),
            scenes: Vec::new(),
            associated: Vec::new(),
        },
        tempfile::tempfile().unwrap(),
    )
    .unwrap();
    let positive = SvcacheBackend.probe(&valid).unwrap();
    assert!(positive.detected);
    assert_eq!(positive.confidence, ProbeConfidence::Definite);
}

#[test]
fn require_fresh_reports_the_stale_candidate() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.j2c");
    std::fs::write(&source, b"first").unwrap();
    let cache = default_svcache_path(&source);
    write_svcache_file(
        &cache,
        &SvcacheMetadata {
            schema_version: SCHEMA_VERSION,
            complete: true,
            source: fingerprint_source(&source).unwrap(),
            properties: Vec::new(),
            scenes: Vec::new(),
            associated: Vec::new(),
        },
        tempfile::tempfile().unwrap(),
    )
    .unwrap();
    std::fs::write(&source, b"second-content").unwrap();

    let error = resolve_open_path_with_policy(&source, SvcachePolicy::RequireFresh).unwrap_err();
    let message = error.to_string();
    assert!(message.contains("fresh .svcache required"));
    assert!(message.contains("stale candidate"));
    assert!(message.contains(&cache.display().to_string()));
}
