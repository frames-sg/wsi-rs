use super::*;

fn required_args() -> Vec<String> {
    [
        "--engine",
        "wsi_rs",
        "--library",
        "/tmp/libshim.dylib",
        "--slide",
        "/tmp/fixture.svs",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

#[test]
fn parses_required_and_optional_worker_arguments() {
    let mut args = required_args();
    args.extend(
        [
            "--repeat-index",
            "2",
            "--cache-bytes",
            "4096",
            "--only",
            "zoom_trace",
            "--workers",
            "2",
            "--require-version-prefix",
            "4.0.1",
        ]
        .into_iter()
        .map(str::to_string),
    );

    let config = WorkerConfig::parse(args).expect("valid arguments");

    assert_eq!(config.engine, Engine::WsiRs);
    assert_eq!(config.repeat_index, 2);
    assert_eq!(config.cache_bytes, 4_096);
    assert_eq!(config.workers, 2);
    assert_eq!(config.only.as_deref(), Some("zoom_trace"));
    assert_eq!(config.required_version_prefix.as_deref(), Some("4.0.1"));
}

#[test]
fn uses_reproducible_defaults() {
    let config = WorkerConfig::parse(required_args()).expect("valid arguments");

    assert_eq!(config.repeat_index, 0);
    assert_eq!(config.cache_bytes, 256 * 1024 * 1024);
    assert_eq!(config.workers, 1);
    assert_eq!(config.only, None);
    assert_eq!(config.required_version_prefix, None);
}

#[test]
fn rejects_unknown_duplicate_missing_and_invalid_arguments() {
    assert!(WorkerConfig::parse(["--wat".into()]).is_err());
    assert!(WorkerConfig::parse([
        "--engine".into(),
        "invalid".into(),
        "--library".into(),
        "x".into(),
        "--slide".into(),
        "y".into(),
    ])
    .is_err());

    let mut duplicate = required_args();
    duplicate.extend(["--engine".into(), "openslide".into()]);
    assert!(WorkerConfig::parse(duplicate).is_err());

    let mut zero_cache = required_args();
    zero_cache.extend(["--cache-bytes".into(), "0".into()]);
    assert!(WorkerConfig::parse(zero_cache).is_err());

    let mut zero_workers = required_args();
    zero_workers.extend(["--workers".into(), "0".into()]);
    assert!(WorkerConfig::parse(zero_workers).is_err());

    assert!(WorkerConfig::parse(Vec::<String>::new()).is_err());
}

#[test]
fn rejects_every_malformed_value_and_missing_required_field() {
    for invalid in [
        vec!["--engine"],
        vec!["--engine", "wsi_rs"],
        vec!["--engine", "wsi_rs", "--library", "lib"],
    ] {
        assert!(WorkerConfig::parse(invalid.into_iter().map(str::to_string)).is_err());
    }

    for (flag, value) in [
        ("--repeat-index", "nope"),
        ("--cache-bytes", "nope"),
        ("--workers", "nope"),
        ("--only", ""),
        ("--require-version-prefix", ""),
    ] {
        let mut args = required_args();
        args.extend([flag.to_string(), value.to_string()]);
        assert!(WorkerConfig::parse(args).is_err(), "{flag}={value:?}");
    }
}

#[test]
fn engine_names_match_worker_cli_values() {
    assert_eq!(Engine::WsiRs.name(), "wsi_rs");
    assert_eq!(Engine::OpenSlide.name(), "openslide");
}
